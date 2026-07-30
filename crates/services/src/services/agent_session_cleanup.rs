use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use db::models::{
    execution_process::{ExecutionProcess, ExecutorActionField},
    session::Session,
    workspace::Workspace,
};
use executors::{actions::ExecutorActionType, executors::BaseCodingAgent};
use sqlx::{FromRow, SqlitePool, types::Json};
use tokio::{fs, process::Command, time::timeout};
use uuid::Uuid;

const TRAILING_SESSION_CUSHION: usize = 5;

#[derive(Debug, FromRow)]
struct AgentSessionTurnRow {
    agent_session_id: String,
    executor_action: Json<ExecutorActionField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CleanupCandidate {
    session_id: String,
    cwd: Option<PathBuf>,
}

pub async fn cleanup_obsolete_agent_sessions(
    pool: &SqlitePool,
    execution_process_id: Uuid,
) -> Result<()> {
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
    )
    .await?;

    for candidate in candidates {
        if let Err(error) = cleanup_provider_session(
            base_executor.clone(),
            &candidate.session_id,
            candidate.cwd.as_deref(),
        )
        .await
        {
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
           ORDER BY ep.created_at DESC"#,
    )
    .bind(app_session_id)
    .fetch_all(pool)
    .await
    .context("load agent session ids for cleanup")?;

    Ok(select_obsolete_sessions(
        rows.into_iter().filter_map(|row| {
            action_base_executor(&row.executor_action.0)
                .filter(|executor| executor == &base_executor)
                .map(|_| {
                    let cwd =
                        workspace_root.and_then(|root| action_cwd(&row.executor_action.0, root));
                    CleanupCandidate {
                        session_id: row.agent_session_id,
                        cwd,
                    }
                })
        }),
        TRAILING_SESSION_CUSHION,
    ))
}

fn action_base_executor(action: &ExecutorActionField) -> Option<BaseCodingAgent> {
    match action {
        ExecutorActionField::ExecutorAction(action) => action.base_executor(),
        ExecutorActionField::Other(_) => None,
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
    executor: BaseCodingAgent,
    session_id: &str,
    cwd: Option<&Path>,
) -> Result<()> {
    match executor {
        BaseCodingAgent::Codex => delete_codex_rollout(session_id).await,
        BaseCodingAgent::ClaudeCode => delete_claude_session(session_id, cwd).await,
        BaseCodingAgent::Opencode => delete_opencode_session(session_id).await,
        _ => Ok(()),
    }
}

async fn delete_codex_rollout(session_id: &str) -> Result<()> {
    let Some(codex_home) = executors::executors::codex::codex_home() else {
        return Ok(());
    };
    let sessions_dir = codex_home.join("sessions");
    let mut matches = Vec::new();
    collect_matching_files(
        &sessions_dir,
        &|name| {
            name.starts_with("rollout-") && name.contains(session_id) && name.ends_with(".jsonl")
        },
        &mut matches,
    )
    .await?;

    for path in matches {
        remove_file_if_exists(&path).await?;
    }

    Ok(())
}

async fn delete_claude_session(session_id: &str, cwd: Option<&Path>) -> Result<()> {
    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };

    let projects_dir = home.join(".claude").join("projects");
    if let Some(cwd) = cwd {
        let path = projects_dir
            .join(claude_project_dir_name(cwd))
            .join(format!("{session_id}.jsonl"));
        remove_file_if_exists(&path).await?;
    }

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

async fn delete_opencode_session(session_id: &str) -> Result<()> {
    let status = timeout(
        Duration::from_secs(30),
        Command::new("npx")
            .args(["-y", "opencode-ai@1.4.7", "session", "delete", session_id])
            .env("NPM_CONFIG_LOGLEVEL", "error")
            .status(),
    )
    .await
    .context("timed out deleting OpenCode session")?
    .context("spawn OpenCode session delete")?;

    if !status.success() {
        tracing::debug!(%status, session_id, "OpenCode session delete did not succeed");
    }

    Ok(())
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
    use super::*;

    #[test]
    fn select_obsolete_sessions_keeps_five_newest_unique_sessions() {
        let candidates = (0..7)
            .map(|index| CleanupCandidate {
                session_id: format!("session-{index}"),
                cwd: None,
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
            .map(|session_id| CleanupCandidate {
                session_id: session_id.to_string(),
                cwd: None,
            })
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
    fn claude_project_dir_name_matches_claude_code_convention() {
        assert_eq!(
            claude_project_dir_name(Path::new("/tmp/example/repo")),
            "-tmp-example-repo"
        );
    }
}
