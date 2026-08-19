use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use db::models::{
    execution_process::{ExecutionProcess, ExecutorActionField},
    session::Session,
    workspace::Workspace,
};
use executors::{
    actions::{ExecutorAction, ExecutorActionType},
    command::CommandParts,
    env::{ExecutionEnv, RepoContext},
    executors::{BaseCodingAgent, CodingAgent, StandardCodingAgentExecutor},
    profile::{ExecutorConfig, ExecutorConfigs},
};
use once_cell::sync::Lazy;
use sqlx::{FromRow, SqlitePool, types::Json};
use tokio::{
    fs,
    process::Command,
    sync::Semaphore,
    time::{sleep, timeout},
};
use uuid::Uuid;

use crate::services::config::{
    MAX_SESSION_CLEANUP_RETENTION_COUNT, SessionCleanupConfig, load_config_from_file,
};

const CLEANUP_DEBOUNCE: Duration = Duration::from_millis(250);
const DELETE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONCURRENT_CLEANUPS: usize = 1;

static CLEANUP_SEMAPHORE: Lazy<Arc<Semaphore>> =
    Lazy::new(|| Arc::new(Semaphore::new(MAX_CONCURRENT_CLEANUPS)));
static SCHEDULED_CLEANUPS: Lazy<Mutex<HashSet<CleanupKey>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

#[derive(Debug, FromRow)]
struct AgentSessionTurnRow {
    agent_session_id: String,
    executor_action: Json<ExecutorActionField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CleanupKey {
    app_session_id: Uuid,
    executor: BaseCodingAgent,
}

#[derive(Debug, Clone, PartialEq)]
struct CleanupCandidate {
    session_id: String,
    cwd: Option<PathBuf>,
    executor_config: ExecutorConfig,
}

#[derive(Debug)]
struct NativeDeleteCommand {
    label: &'static str,
    command_parts: CommandParts,
    env: HashMap<String, String>,
    cwd: Option<PathBuf>,
}

struct InFlightCleanupGuard {
    key: CleanupKey,
}

impl Drop for InFlightCleanupGuard {
    fn drop(&mut self) {
        if let Ok(mut scheduled) = SCHEDULED_CLEANUPS.lock() {
            scheduled.remove(&self.key);
        }
    }
}

#[async_trait]
trait DeleteCommandRunner: Sync {
    async fn run_delete_command(&self, command: NativeDeleteCommand) -> Result<ExitStatus>;
}

struct TokioDeleteCommandRunner;

#[async_trait]
impl DeleteCommandRunner for TokioDeleteCommandRunner {
    async fn run_delete_command(&self, command: NativeDeleteCommand) -> Result<ExitStatus> {
        let (program_path, args) = command
            .command_parts
            .into_resolved()
            .await
            .with_context(|| format!("resolve {} delete command", command.label))?;

        let mut process = Command::new(program_path);
        process.args(&args).envs(&command.env);
        if let Some(cwd) = command.cwd.filter(|path| path.exists()) {
            process.current_dir(cwd);
        }

        timeout(DELETE_TIMEOUT, process.status())
            .await
            .with_context(|| format!("timed out deleting {} session", command.label))?
            .with_context(|| format!("spawn {} session delete", command.label))
    }
}

pub fn spawn_cleanup_obsolete_agent_sessions(pool: SqlitePool, execution_process_id: Uuid) {
    tokio::spawn(async move {
        if let Err(error) =
            debounced_cleanup_obsolete_agent_sessions(pool, execution_process_id).await
        {
            tracing::debug!(
                execution_process_id = %execution_process_id,
                ?error,
                "failed to clean up obsolete agent sessions"
            );
        }
    });
}

async fn debounced_cleanup_obsolete_agent_sessions(
    pool: SqlitePool,
    execution_process_id: Uuid,
) -> Result<()> {
    let Some(key) = cleanup_key_for_process(&pool, execution_process_id).await? else {
        return Ok(());
    };

    let _guard = {
        let mut scheduled = SCHEDULED_CLEANUPS
            .lock()
            .expect("agent session cleanup schedule lock poisoned");
        if !scheduled.insert(key) {
            return Ok(());
        }
        InFlightCleanupGuard { key }
    };

    sleep(CLEANUP_DEBOUNCE).await;
    let _permit = CLEANUP_SEMAPHORE
        .acquire()
        .await
        .context("agent session cleanup semaphore closed")?;

    cleanup_obsolete_agent_sessions_with_runner(
        &pool,
        execution_process_id,
        &TokioDeleteCommandRunner,
        load_session_cleanup_config().await,
    )
    .await
}

pub async fn cleanup_obsolete_agent_sessions(
    pool: &SqlitePool,
    execution_process_id: Uuid,
) -> Result<()> {
    cleanup_obsolete_agent_sessions_with_runner(
        pool,
        execution_process_id,
        &TokioDeleteCommandRunner,
        load_session_cleanup_config().await,
    )
    .await
}

async fn cleanup_obsolete_agent_sessions_with_runner(
    pool: &SqlitePool,
    execution_process_id: Uuid,
    runner: &dyn DeleteCommandRunner,
    settings: SessionCleanupConfig,
) -> Result<()> {
    if !settings.enabled {
        tracing::debug!(
            execution_process_id = %execution_process_id,
            "agent session cleanup skipped because it is disabled in settings"
        );
        return Ok(());
    }

    let Some(process) = ExecutionProcess::find_by_id(pool, execution_process_id)
        .await
        .context("load execution process for agent session cleanup")?
    else {
        return Ok(());
    };

    let Json(ExecutorActionField::ExecutorAction(current_action)) = &process.executor_action else {
        return Ok(());
    };

    if !is_forking_action(current_action.typ()) {
        return Ok(());
    }

    let Some(base_executor) = current_action.base_executor() else {
        return Ok(());
    };

    if !is_supported_executor(&base_executor) {
        return Ok(());
    }

    let workspace_root = workspace_root_for_process(pool, process.session_id).await?;
    let candidates = obsolete_session_candidates(
        pool,
        process.session_id,
        base_executor.clone(),
        workspace_root.as_deref(),
        cleanup_retention_count(&settings),
    )
    .await?;

    for candidate in candidates {
        if let Err(error) = cleanup_provider_session(&candidate, runner).await {
            tracing::debug!(
                session_id = candidate.session_id,
                executor = ?base_executor,
                ?error,
                "failed to clean up obsolete agent session"
            );
        }
    }

    Ok(())
}

async fn load_session_cleanup_config() -> SessionCleanupConfig {
    load_config_from_file(&utils::assets::config_path())
        .await
        .session_cleanup
}

fn cleanup_retention_count(settings: &SessionCleanupConfig) -> usize {
    usize::from(
        settings
            .retention_count
            .max(1)
            .min(MAX_SESSION_CLEANUP_RETENTION_COUNT),
    )
}

async fn cleanup_key_for_process(
    pool: &SqlitePool,
    execution_process_id: Uuid,
) -> Result<Option<CleanupKey>> {
    let Some(process) = ExecutionProcess::find_by_id(pool, execution_process_id)
        .await
        .context("load execution process for agent session cleanup scheduling")?
    else {
        return Ok(None);
    };

    let Json(ExecutorActionField::ExecutorAction(current_action)) = &process.executor_action else {
        return Ok(None);
    };

    if !is_forking_action(current_action.typ()) {
        return Ok(None);
    }

    let Some(executor) = current_action.base_executor() else {
        return Ok(None);
    };

    if !is_supported_executor(&executor) {
        return Ok(None);
    }

    Ok(Some(CleanupKey {
        app_session_id: process.session_id,
        executor,
    }))
}

fn is_forking_action(action: &ExecutorActionType) -> bool {
    matches!(
        action,
        ExecutorActionType::CodingAgentFollowUpRequest(_)
            | ExecutorActionType::CodingAgentSessionCommandRequest(_)
            | ExecutorActionType::ReviewRequest(_)
    )
}

fn is_supported_executor(executor: &BaseCodingAgent) -> bool {
    matches!(
        executor,
        BaseCodingAgent::Codex | BaseCodingAgent::ClaudeCode | BaseCodingAgent::Opencode
    )
}

async fn workspace_root_for_process(
    pool: &SqlitePool,
    session_id: Uuid,
) -> Result<Option<PathBuf>> {
    let Some(session) = Session::find_by_id(pool, session_id)
        .await
        .context("load session for agent session cleanup")?
    else {
        return Ok(None);
    };

    let Some(workspace) = Workspace::find_by_id(pool, session.workspace_id)
        .await
        .context("load workspace for agent session cleanup")?
    else {
        return Ok(None);
    };

    Ok(workspace.container_ref.map(PathBuf::from))
}

async fn obsolete_session_candidates(
    pool: &SqlitePool,
    app_session_id: Uuid,
    base_executor: BaseCodingAgent,
    workspace_root: Option<&Path>,
    keep_count: usize,
) -> Result<Vec<CleanupCandidate>> {
    let rows = sqlx::query_as::<_, AgentSessionTurnRow>(
        r#"SELECT
            cat.agent_session_id as agent_session_id,
            ep.executor_action as executor_action
           FROM execution_processes ep
           JOIN coding_agent_turns cat ON cat.execution_process_id = ep.id
           JOIN sessions s ON s.id = ep.session_id
           LEFT JOIN execution_processes reset_ep
             ON reset_ep.id = s.context_reset_execution_process_id
           WHERE ep.session_id = $1
             AND ep.run_reason = 'codingagent'
             AND ep.dropped = FALSE
             AND cat.agent_session_id IS NOT NULL
             AND (
                 s.context_reset_execution_process_id IS NULL
                 OR reset_ep.id IS NULL
                 OR ep.rowid > reset_ep.rowid
             )
           ORDER BY ep.created_at DESC, ep.rowid DESC"#,
    )
    .bind(app_session_id)
    .fetch_all(pool)
    .await
    .context("load agent session ids for cleanup")?;

    Ok(select_obsolete_sessions(
        rows.into_iter().filter_map(|row| {
            action_executor_config(&row.executor_action.0)
                .filter(|config| config.executor == base_executor)
                .map(|executor_config| {
                    let cwd =
                        workspace_root.and_then(|root| action_cwd(&row.executor_action.0, root));
                    CleanupCandidate {
                        session_id: row.agent_session_id,
                        cwd,
                        executor_config,
                    }
                })
        }),
        keep_count,
    ))
}

fn action_executor_config(action: &ExecutorActionField) -> Option<ExecutorConfig> {
    match action {
        ExecutorActionField::ExecutorAction(action) => executor_config(action),
        ExecutorActionField::Other(_) => None,
    }
}

fn executor_config(action: &ExecutorAction) -> Option<ExecutorConfig> {
    match action.typ() {
        ExecutorActionType::CodingAgentInitialRequest(request) => {
            Some(request.executor_config.clone())
        }
        ExecutorActionType::CodingAgentFollowUpRequest(request) => {
            Some(request.executor_config.clone())
        }
        ExecutorActionType::CodingAgentSessionCommandRequest(request) => {
            Some(request.executor_config.clone())
        }
        ExecutorActionType::ReviewRequest(request) => Some(request.executor_config.clone()),
        ExecutorActionType::ScriptRequest(_) => None,
    }
}

fn action_cwd(action: &ExecutorActionField, workspace_root: &Path) -> Option<PathBuf> {
    match action {
        ExecutorActionField::ExecutorAction(action) => match action.typ() {
            ExecutorActionType::CodingAgentInitialRequest(request) => {
                Some(request.effective_dir(workspace_root))
            }
            ExecutorActionType::CodingAgentFollowUpRequest(request) => {
                Some(request.effective_dir(workspace_root))
            }
            ExecutorActionType::CodingAgentSessionCommandRequest(request) => {
                Some(request.effective_dir(workspace_root))
            }
            ExecutorActionType::ReviewRequest(request) => {
                Some(request.effective_dir(workspace_root))
            }
            ExecutorActionType::ScriptRequest(_) => Some(workspace_root.to_path_buf()),
        },
        ExecutorActionField::Other(_) => None,
    }
}

fn select_obsolete_sessions(
    newest_first: impl IntoIterator<Item = CleanupCandidate>,
    keep_count: usize,
) -> Vec<CleanupCandidate> {
    let mut kept_count = 0;
    let mut seen = HashSet::new();
    let mut obsolete = Vec::new();

    for candidate in newest_first {
        if !seen.insert(candidate.session_id.clone()) {
            continue;
        }

        if kept_count < keep_count {
            kept_count += 1;
        } else {
            obsolete.push(candidate);
        }
    }

    obsolete
}

async fn cleanup_provider_session(
    candidate: &CleanupCandidate,
    runner: &dyn DeleteCommandRunner,
) -> Result<()> {
    match candidate.executor_config.executor {
        BaseCodingAgent::Codex => {
            delete_codex_session(&candidate.executor_config, &candidate.session_id, runner).await
        }
        BaseCodingAgent::ClaudeCode => {
            delete_claude_session(&candidate.session_id, candidate.cwd.as_deref()).await
        }
        BaseCodingAgent::Opencode => {
            delete_opencode_session(
                &candidate.executor_config,
                &candidate.session_id,
                candidate.cwd.as_deref(),
                runner,
            )
            .await
        }
        _ => Ok(()),
    }
}

async fn delete_codex_session(
    executor_config: &ExecutorConfig,
    session_id: &str,
    runner: &dyn DeleteCommandRunner,
) -> Result<()> {
    let command = build_codex_delete_command(executor_config, session_id)?;
    let status = runner.run_delete_command(command).await?;

    if !status.success() {
        tracing::debug!(%status, session_id, "Codex session delete did not succeed");
    }

    Ok(())
}

async fn delete_claude_session(session_id: &str, cwd: Option<&Path>) -> Result<()> {
    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };

    let projects_dir = home.join(".claude").join("projects");
    delete_claude_session_from_projects_dir(&projects_dir, session_id, cwd).await
}

async fn delete_claude_session_from_projects_dir(
    projects_dir: &Path,
    session_id: &str,
    cwd: Option<&Path>,
) -> Result<()> {
    if let Some(cwd) = cwd {
        let path = claude_project_session_path(projects_dir, cwd, session_id);
        remove_file_if_exists(&path).await?;
    }

    // Claude Code currently exposes `claude project purge`, but that removes
    // every transcript/task/history item for a project. That is too broad for
    // Vibe Kanban's five-message undo cushion, so Claude cleanup intentionally
    // remains a narrow per-session JSONL removal until a safe native
    // per-session delete command exists.
    let mut matches = Vec::new();
    collect_matching_files(
        &projects_dir,
        &|name| name == format!("{session_id}.jsonl"),
        &mut matches,
    )
    .await?;

    for path in matches {
        remove_file_if_exists(&path).await?;
    }

    Ok(())
}

fn claude_project_session_path(projects_dir: &Path, cwd: &Path, session_id: &str) -> PathBuf {
    projects_dir
        .join(claude_project_dir_name(cwd))
        .join(format!("{session_id}.jsonl"))
}

async fn delete_opencode_session(
    executor_config: &ExecutorConfig,
    session_id: &str,
    cwd: Option<&Path>,
    runner: &dyn DeleteCommandRunner,
) -> Result<()> {
    let command = build_opencode_delete_command(executor_config, session_id, cwd)?;
    let status = runner.run_delete_command(command).await?;

    if !status.success() {
        tracing::debug!(%status, session_id, "OpenCode session delete did not succeed");
    }

    Ok(())
}

fn build_codex_delete_command(
    executor_config: &ExecutorConfig,
    session_id: &str,
) -> Result<NativeDeleteCommand> {
    let agent = configured_agent(executor_config)?;
    let CodingAgent::Codex(codex) = agent else {
        bail!("configured executor profile did not resolve to Codex");
    };

    Ok(NativeDeleteCommand {
        label: "Codex",
        command_parts: codex.build_delete_session_command(session_id)?,
        env: cleanup_env(&codex.cmd),
        cwd: None,
    })
}

fn build_opencode_delete_command(
    executor_config: &ExecutorConfig,
    session_id: &str,
    cwd: Option<&Path>,
) -> Result<NativeDeleteCommand> {
    let agent = configured_agent(executor_config)?;
    let CodingAgent::Opencode(opencode) = agent else {
        bail!("configured executor profile did not resolve to OpenCode");
    };

    Ok(NativeDeleteCommand {
        label: "OpenCode",
        command_parts: opencode.build_delete_session_command(session_id)?,
        env: cleanup_env(&opencode.cmd),
        cwd: cwd.map(Path::to_path_buf),
    })
}

fn configured_agent(executor_config: &ExecutorConfig) -> Result<CodingAgent> {
    let profile_id = executor_config.profile_id();
    let mut agent = ExecutorConfigs::get_cached()
        .get_coding_agent(&profile_id)
        .with_context(|| format!("load executor profile {profile_id} for session cleanup"))?;

    if executor_config.has_overrides() {
        agent.apply_overrides(executor_config);
    }

    Ok(agent)
}

fn cleanup_env(cmd: &executors::command::CmdOverrides) -> HashMap<String, String> {
    let mut env = ExecutionEnv::new(RepoContext::default(), false, String::new());
    env.insert("NPM_CONFIG_LOGLEVEL", "error");
    env.insert("NODE_NO_WARNINGS", "1");
    env.insert("NO_COLOR", "1");
    env.with_profile(cmd).vars
}

fn claude_project_dir_name(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|ch| if ch == '/' || ch == '\\' { '-' } else { ch })
        .collect()
}

async fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path).await {
        Ok(()) => {
            tracing::debug!(path = %path.display(), "deleted obsolete agent session file");
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("delete {}", path.display())),
    }
}

async fn collect_matching_files(
    dir: &Path,
    is_match: &(dyn Fn(&str) -> bool + Sync),
    matches: &mut Vec<PathBuf>,
) -> Result<()> {
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current_dir) = stack.pop() {
        let mut entries = match fs::read_dir(&current_dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", current_dir.display()));
            }
        };

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file()
                && let Some(name) = path.file_name().and_then(|name| name.to_str())
                && is_match(name)
            {
                matches.push(path);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use anyhow::anyhow;
    use executors::{
        actions::{
            ExecutorAction,
            coding_agent_follow_up::CodingAgentFollowUpRequest,
            coding_agent_initial::CodingAgentInitialRequest,
            session_command::{CodingAgentSessionCommandRequest, SessionCommand},
        },
        profile::ExecutorConfig,
    };
    use sqlx::{Executor, SqlitePool, sqlite::SqlitePoolOptions};

    use super::*;

    fn candidate(session_id: &str) -> CleanupCandidate {
        CleanupCandidate {
            session_id: session_id.to_string(),
            cwd: None,
            executor_config: ExecutorConfig::new(BaseCodingAgent::Codex),
        }
    }

    fn cleanup_settings(enabled: bool, retention_count: u16) -> SessionCleanupConfig {
        SessionCleanupConfig {
            enabled,
            retention_count,
        }
    }

    #[test]
    fn select_obsolete_sessions_keeps_five_newest_unique_sessions() {
        let candidates = (0..7)
            .map(|index| CleanupCandidate {
                session_id: format!("session-{index}"),
                cwd: None,
                executor_config: ExecutorConfig::new(BaseCodingAgent::Codex),
            })
            .collect::<Vec<_>>();

        let obsolete = select_obsolete_sessions(candidates, 5);

        assert_eq!(
            obsolete
                .into_iter()
                .map(|candidate| candidate.session_id)
                .collect::<Vec<_>>(),
            vec!["session-5", "session-6"]
        );
    }

    #[test]
    fn select_obsolete_sessions_ignores_duplicate_recent_ids() {
        let candidates = ["new", "new", "a", "b", "c", "d", "old"]
            .into_iter()
            .map(candidate)
            .collect::<Vec<_>>();

        let obsolete = select_obsolete_sessions(candidates, 5);

        assert_eq!(
            obsolete
                .into_iter()
                .map(|candidate| candidate.session_id)
                .collect::<Vec<_>>(),
            vec!["old"]
        );
    }

    #[test]
    fn cleanup_retention_count_clamps_unsafe_values() {
        assert_eq!(cleanup_retention_count(&cleanup_settings(true, 0)), 1);
        assert_eq!(cleanup_retention_count(&cleanup_settings(true, 2)), 2);
        assert_eq!(cleanup_retention_count(&cleanup_settings(true, 250)), 100);
    }

    #[test]
    fn claude_project_dir_name_matches_claude_code_convention() {
        assert_eq!(
            claude_project_dir_name(Path::new("/tmp/example/repo")),
            "-tmp-example-repo"
        );
    }

    #[tokio::test]
    async fn claude_cleanup_targets_only_exact_obsolete_session_files() {
        let temp_dir = tempfile::tempdir().unwrap();
        let projects_dir = temp_dir.path().join("projects");
        let project_dir =
            projects_dir.join(claude_project_dir_name(Path::new("/tmp/example/repo")));
        let other_project_dir = projects_dir.join("-tmp-other-repo");
        fs::create_dir_all(&project_dir).await.unwrap();
        fs::create_dir_all(&other_project_dir).await.unwrap();

        let obsolete = "obsolete-session";
        let exact_in_cwd =
            claude_project_session_path(&projects_dir, Path::new("/tmp/example/repo"), obsolete);
        let exact_in_other_project = other_project_dir.join(format!("{obsolete}.jsonl"));
        let similar = project_dir.join(format!("{obsolete}-suffix.jsonl"));
        let unrelated = project_dir.join("unrelated-session.jsonl");

        fs::write(&exact_in_cwd, "delete me").await.unwrap();
        fs::write(&exact_in_other_project, "delete me too")
            .await
            .unwrap();
        fs::write(&similar, "keep me").await.unwrap();
        fs::write(&unrelated, "keep me too").await.unwrap();

        delete_claude_session_from_projects_dir(
            &projects_dir,
            obsolete,
            Some(Path::new("/tmp/example/repo")),
        )
        .await
        .unwrap();

        assert!(!exact_in_cwd.exists());
        assert!(!exact_in_other_project.exists());
        assert!(similar.exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn cleanup_env_applies_profile_overrides_after_defaults() {
        let mut profile_env = HashMap::new();
        profile_env.insert(
            "XDG_DATA_HOME".to_string(),
            "/tmp/opencode-data".to_string(),
        );
        profile_env.insert("NO_COLOR".to_string(), "0".to_string());
        let overrides = executors::command::CmdOverrides {
            env: Some(profile_env),
            ..Default::default()
        };

        let env = cleanup_env(&overrides);

        assert_eq!(env.get("NPM_CONFIG_LOGLEVEL").unwrap(), "error");
        assert_eq!(env.get("NODE_NO_WARNINGS").unwrap(), "1");
        assert_eq!(env.get("NO_COLOR").unwrap(), "0");
        assert_eq!(env.get("XDG_DATA_HOME").unwrap(), "/tmp/opencode-data");
    }

    struct FailingRunner {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl DeleteCommandRunner for FailingRunner {
        async fn run_delete_command(&self, _command: NativeDeleteCommand) -> Result<ExitStatus> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(anyhow!("delete failed"))
        }
    }

    #[tokio::test]
    async fn provider_delete_failure_is_candidate_local() {
        let runner = FailingRunner {
            calls: AtomicUsize::new(0),
        };
        let candidate = CleanupCandidate {
            session_id: "obsolete-codex-session".to_string(),
            cwd: None,
            executor_config: ExecutorConfig::new(BaseCodingAgent::Codex),
        };

        let result = cleanup_provider_session(&candidate, &runner).await;

        assert!(result.is_err());
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cleanup_disabled_skips_candidate_selection_and_deletes() {
        let pool = test_pool().await;
        let app_session_id = Uuid::new_v4();
        let current_process_id = Uuid::new_v4();
        insert_session(&pool, app_session_id).await;
        insert_turn(
            &pool,
            app_session_id,
            current_process_id,
            "2026-01-01T00:00:10Z",
            follow_up_action(BaseCodingAgent::Codex, "current-session", None),
            Some("current-session"),
            false,
        )
        .await;

        let runner = FailingRunner {
            calls: AtomicUsize::new(0),
        };

        cleanup_obsolete_agent_sessions_with_runner(
            &pool,
            current_process_id,
            &runner,
            cleanup_settings(false, 1),
        )
        .await
        .unwrap();

        assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
    }

    #[cfg(unix)]
    struct NonZeroRunner;

    #[cfg(unix)]
    #[async_trait]
    impl DeleteCommandRunner for NonZeroRunner {
        async fn run_delete_command(&self, _command: NativeDeleteCommand) -> Result<ExitStatus> {
            use std::os::unix::process::ExitStatusExt;

            Ok(ExitStatus::from_raw(1 << 8))
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_delete_nonzero_status_is_best_effort() {
        let candidate = CleanupCandidate {
            session_id: "obsolete-codex-session".to_string(),
            cwd: None,
            executor_config: ExecutorConfig::new(BaseCodingAgent::Codex),
        };

        cleanup_provider_session(&candidate, &NonZeroRunner)
            .await
            .unwrap();
    }

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        pool.execute(
            r#"CREATE TABLE sessions (
                id BLOB PRIMARY KEY,
                context_reset_execution_process_id BLOB NULL
            )"#,
        )
        .await
        .unwrap();
        pool.execute(
            r#"CREATE TABLE execution_processes (
                id BLOB PRIMARY KEY,
                session_id BLOB NOT NULL,
                run_reason TEXT NOT NULL,
                executor_action TEXT NOT NULL,
                dropped BOOLEAN NOT NULL DEFAULT FALSE,
                created_at TEXT NOT NULL
            )"#,
        )
        .await
        .unwrap();
        pool.execute(
            r#"CREATE TABLE coding_agent_turns (
                execution_process_id BLOB NOT NULL,
                agent_session_id TEXT NULL
            )"#,
        )
        .await
        .unwrap();

        pool
    }

    fn follow_up_action(
        executor: BaseCodingAgent,
        provider_session_id: &str,
        working_dir: Option<&str>,
    ) -> ExecutorAction {
        ExecutorAction::new(
            ExecutorActionType::CodingAgentFollowUpRequest(CodingAgentFollowUpRequest {
                prompt: "follow up".to_string(),
                session_id: provider_session_id.to_string(),
                reset_to_message_id: None,
                executor_config: ExecutorConfig::new(executor),
                working_dir: working_dir.map(str::to_string),
            }),
            None,
        )
    }

    fn initial_action(executor: BaseCodingAgent) -> ExecutorAction {
        ExecutorAction::new(
            ExecutorActionType::CodingAgentInitialRequest(CodingAgentInitialRequest {
                prompt: "initial".to_string(),
                executor_config: ExecutorConfig::new(executor),
                working_dir: None,
            }),
            None,
        )
    }

    fn clear_action(executor: BaseCodingAgent) -> ExecutorAction {
        ExecutorAction::new(
            ExecutorActionType::CodingAgentSessionCommandRequest(
                CodingAgentSessionCommandRequest {
                    command: SessionCommand::Clear,
                    prompt: "/clear".to_string(),
                    session_id: None,
                    executor_config: ExecutorConfig::new(executor),
                    working_dir: None,
                },
            ),
            None,
        )
    }

    async fn insert_session(pool: &SqlitePool, session_id: Uuid) {
        sqlx::query("INSERT INTO sessions (id) VALUES (?)")
            .bind(session_id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn mark_clear_boundary(pool: &SqlitePool, session_id: Uuid, process_id: Uuid) {
        sqlx::query("UPDATE sessions SET context_reset_execution_process_id = ? WHERE id = ?")
            .bind(process_id)
            .bind(session_id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn insert_turn(
        pool: &SqlitePool,
        app_session_id: Uuid,
        process_id: Uuid,
        created_at: &str,
        action: ExecutorAction,
        agent_session_id: Option<&str>,
        dropped: bool,
    ) {
        let action_json = serde_json::to_string(&action).unwrap();
        sqlx::query(
            r#"INSERT INTO execution_processes
               (id, session_id, run_reason, executor_action, dropped, created_at)
               VALUES (?, ?, 'codingagent', ?, ?, ?)"#,
        )
        .bind(process_id)
        .bind(app_session_id)
        .bind(action_json)
        .bind(dropped)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO coding_agent_turns (execution_process_id, agent_session_id) VALUES (?, ?)",
        )
        .bind(process_id)
        .bind(agent_session_id)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn obsolete_candidates_keep_newest_unique_current_session() {
        let pool = test_pool().await;
        let app_session_id = Uuid::new_v4();
        insert_session(&pool, app_session_id).await;

        for index in 0..7 {
            let provider_session_id = format!("provider-{index}");
            let created_at = format!("2026-01-01T00:00:0{}Z", 6 - index);
            insert_turn(
                &pool,
                app_session_id,
                Uuid::new_v4(),
                &created_at,
                follow_up_action(BaseCodingAgent::Codex, &provider_session_id, None),
                Some(&provider_session_id),
                false,
            )
            .await;
        }

        let candidates =
            obsolete_session_candidates(&pool, app_session_id, BaseCodingAgent::Codex, None, 5)
                .await
                .unwrap();

        assert_eq!(
            candidates
                .into_iter()
                .map(|candidate| candidate.session_id)
                .collect::<Vec<_>>(),
            vec!["provider-5", "provider-6"]
        );
    }

    #[tokio::test]
    async fn obsolete_candidates_ignore_mixed_executors_and_dropped_processes() {
        let pool = test_pool().await;
        let app_session_id = Uuid::new_v4();
        insert_session(&pool, app_session_id).await;

        for index in 0..6 {
            let provider_session_id = format!("codex-{index}");
            let created_at = format!("2026-01-01T00:00:0{}Z", 9 - index);
            insert_turn(
                &pool,
                app_session_id,
                Uuid::new_v4(),
                &created_at,
                follow_up_action(BaseCodingAgent::Codex, &provider_session_id, None),
                Some(&provider_session_id),
                index == 5,
            )
            .await;
        }
        insert_turn(
            &pool,
            app_session_id,
            Uuid::new_v4(),
            "2026-01-01T00:00:10Z",
            follow_up_action(BaseCodingAgent::Opencode, "opencode-new", None),
            Some("opencode-new"),
            false,
        )
        .await;

        let candidates =
            obsolete_session_candidates(&pool, app_session_id, BaseCodingAgent::Codex, None, 5)
                .await
                .unwrap();

        assert!(candidates.is_empty());
    }

    #[tokio::test]
    async fn obsolete_candidates_respect_clear_boundary() {
        let pool = test_pool().await;
        let app_session_id = Uuid::new_v4();
        let clear_process_id = Uuid::new_v4();
        insert_session(&pool, app_session_id).await;

        insert_turn(
            &pool,
            app_session_id,
            Uuid::new_v4(),
            "2026-01-01T00:00:00Z",
            initial_action(BaseCodingAgent::Codex),
            Some("before-clear"),
            false,
        )
        .await;
        insert_turn(
            &pool,
            app_session_id,
            clear_process_id,
            "2026-01-01T00:00:01Z",
            clear_action(BaseCodingAgent::Codex),
            None,
            false,
        )
        .await;
        mark_clear_boundary(&pool, app_session_id, clear_process_id).await;

        for index in 0..6 {
            let provider_session_id = format!("after-clear-{index}");
            let created_at = format!("2026-01-01T00:00:0{}Z", index + 2);
            insert_turn(
                &pool,
                app_session_id,
                Uuid::new_v4(),
                &created_at,
                follow_up_action(BaseCodingAgent::Codex, &provider_session_id, None),
                Some(&provider_session_id),
                false,
            )
            .await;
        }

        let candidates =
            obsolete_session_candidates(&pool, app_session_id, BaseCodingAgent::Codex, None, 5)
                .await
                .unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].session_id, "after-clear-0");
        assert_ne!(candidates[0].session_id, "before-clear");
    }

    #[tokio::test]
    async fn obsolete_candidates_include_effective_cwd() {
        let pool = test_pool().await;
        let app_session_id = Uuid::new_v4();
        let workspace_root = Path::new("/tmp/workspace");
        insert_session(&pool, app_session_id).await;

        for index in 0..6 {
            let provider_session_id = format!("claude-{index}");
            let created_at = format!("2026-01-01T00:00:0{}Z", 6 - index);
            insert_turn(
                &pool,
                app_session_id,
                Uuid::new_v4(),
                &created_at,
                follow_up_action(
                    BaseCodingAgent::ClaudeCode,
                    &provider_session_id,
                    Some("repo"),
                ),
                Some(&provider_session_id),
                false,
            )
            .await;
        }

        let candidates = obsolete_session_candidates(
            &pool,
            app_session_id,
            BaseCodingAgent::ClaudeCode,
            Some(workspace_root),
            5,
        )
        .await
        .unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].cwd.as_deref(),
            Some(Path::new("/tmp/workspace/repo"))
        );
    }

    #[tokio::test]
    async fn obsolete_candidates_use_configured_retention_count() {
        let pool = test_pool().await;
        let app_session_id = Uuid::new_v4();
        insert_session(&pool, app_session_id).await;

        for index in 0..4 {
            let provider_session_id = format!("provider-{index}");
            let created_at = format!("2026-01-01T00:00:0{}Z", 4 - index);
            insert_turn(
                &pool,
                app_session_id,
                Uuid::new_v4(),
                &created_at,
                follow_up_action(BaseCodingAgent::Codex, &provider_session_id, None),
                Some(&provider_session_id),
                false,
            )
            .await;
        }

        let candidates =
            obsolete_session_candidates(&pool, app_session_id, BaseCodingAgent::Codex, None, 2)
                .await
                .unwrap();

        assert_eq!(
            candidates
                .into_iter()
                .map(|candidate| candidate.session_id)
                .collect::<Vec<_>>(),
            vec!["provider-2", "provider-3"]
        );
    }
}
