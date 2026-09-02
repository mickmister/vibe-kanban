//! QA Mode: Mock executor for testing
//!
//! This module provides a mock executor that:
//! 1. Preserves the existing random QA smoke behavior by default
//! 2. Supports deterministic scripted outcomes when `VK_QA_SCRIPTED_OUTCOME` or
//!    `VK_QA_SCRIPTED_OUTCOME_FILE` is set
//! 3. Outputs logs in ClaudeJson format for compatibility with existing log normalization

use std::{
    collections::HashMap,
    path::Path,
    process::Stdio,
    sync::{Arc, LazyLock, Mutex},
};

#[cfg(feature = "qa-mode")]
use async_trait::async_trait;
use rand::seq::SliceRandom as _;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use ts_rs::TS;
use workspace_utils::{command_ext::GroupSpawnNoWindowExt, msg_store::MsgStore};

use crate::{
    env::ExecutionEnv,
    executors::{
        ExecutorError, SpawnedChild,
        claude::{
            ClaudeContentItem, ClaudeJson, ClaudeMessage, ClaudeMessageContent, ClaudeToolData,
        },
    },
    logs::utils::EntryIndexProvider,
};
#[cfg(feature = "qa-mode")]
use crate::{
    executors::{BaseCodingAgent, StandardCodingAgentExecutor},
    profile::ExecutorConfig,
};

/// Primary runtime switch for QA-mode agent response mocking.
pub const QA_MODE_ENV_VAR: &str = "VK_QA_MODE";
/// Backwards-compatible shorthand accepted by local QA harnesses.
pub const LEGACY_QA_MODE_ENV_VAR: &str = "QA_MODE";

/// Mock executor for QA testing
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, TS, JsonSchema)]
pub struct QaMockExecutor;

impl QaMockExecutor {
    /// Returns true when QA agent response mocking is enabled at runtime.
    pub fn runtime_enabled() -> bool {
        std::env::var(QA_MODE_ENV_VAR)
            .or_else(|_| std::env::var(LEGACY_QA_MODE_ENV_VAR))
            .is_ok_and(|value| env_value_enabled(&value))
    }

    pub async fn spawn_mock(
        &self,
        current_dir: &Path,
        prompt: &str,
        env: &ExecutionEnv,
    ) -> Result<SpawnedChild, ExecutorError> {
        if let Some(script) = load_scripted_outcome(env, prompt).await? {
            info!(?script.outcome, "QA Mock Executor: spawning scripted execution");
            let logs = generate_scripted_logs(prompt, &script);
            return spawn_log_process(current_dir, logs, script.exit_code(), script.delay_ms())
                .await;
        }

        info!("QA Mock Executor: spawning mock execution");

        // 1. Perform file operations before spawning the log output process
        perform_file_operations(current_dir).await;

        // 2. Generate mock logs and stream them with the historical one-second delay.
        let logs = generate_mock_logs(prompt);
        spawn_log_process(current_dir, logs, 0, 1000).await
    }

    pub async fn spawn_follow_up_mock(
        &self,
        current_dir: &Path,
        prompt: &str,
        _session_id: &str,
        _reset_to_message_id: Option<&str>,
        env: &ExecutionEnv,
    ) -> Result<SpawnedChild, ExecutorError> {
        // QA mode doesn't support real sessions, just spawn fresh
        info!("QA Mock Executor: follow-up request treated as new spawn");
        self.spawn_mock(current_dir, prompt, env).await
    }

    pub fn normalize_mock_logs(
        &self,
        msg_store: Arc<MsgStore>,
        current_dir: &Path,
    ) -> Vec<tokio::task::JoinHandle<()>> {
        // Reuse Claude's log processor since we output ClaudeJson format
        let entry_index_provider = EntryIndexProvider::start_from(&msg_store);
        let h1 = crate::executors::claude::ClaudeLogProcessor::process_logs(
            msg_store,
            current_dir,
            entry_index_provider,
            crate::executors::claude::HistoryStrategy::Default,
        );
        vec![h1]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum QaScriptedOutcomeKind {
    Completed,
    Failed,
    /// Test marker for future scanner work. This currently completes normally
    /// with a final message that explicitly marks the external wait.
    WaitCallback,
    /// Test marker for future scanner work. This currently completes normally
    /// with a final message that explicitly marks the external wait.
    WaitCi,
    /// Emits an explicit human-question marker for future scanner/workflow tests.
    AskHuman,
    /// Premature/stalled turn: emits no final assistant response and exits with failure.
    Stall,
    /// Completes with the structured command serialized as the final assistant message.
    StructuredCommand,
    /// Completes with a deterministic response longer than VK's current response summary cap.
    LongResponse,
    /// Executor abstraction cannot directly mark an execution as killed; it exits failure
    /// with an explicit marker so later scanner tests can distinguish the scenario.
    Killed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QaScriptedOutcome {
    pub outcome: QaScriptedOutcomeKind,
    #[serde(default)]
    pub final_message: Option<String>,
    #[serde(default)]
    pub structured_command: Option<serde_json::Value>,
    #[serde(default)]
    pub wait_ref: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub delay_ms: Option<u64>,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QaScriptedOutcomePlan {
    pub outcomes: Vec<QaScriptedOutcomePlanEntry>,
    #[serde(default)]
    pub fallback: Option<QaScriptedOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QaScriptedOutcomePlanEntry {
    #[serde(default)]
    pub prompt_contains: Option<String>,
    #[serde(flatten)]
    pub outcome: QaScriptedOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
enum QaScriptedOutcomeFile {
    Plan(QaScriptedOutcomePlan),
    Sequence(Vec<QaScriptedOutcomePlanEntry>),
    Single(QaScriptedOutcome),
}

static SCRIPTED_OUTCOME_PLAN_CURSORS: LazyLock<Mutex<HashMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

impl Default for QaScriptedOutcome {
    fn default() -> Self {
        Self {
            outcome: QaScriptedOutcomeKind::Completed,
            final_message: None,
            structured_command: None,
            wait_ref: None,
            session_id: None,
            message_id: None,
            delay_ms: None,
            exit_code: None,
        }
    }
}

impl QaScriptedOutcome {
    fn session_id(&self) -> String {
        self.session_id
            .clone()
            .unwrap_or_else(|| "qa-scripted-session".to_string())
    }

    fn message_id(&self) -> String {
        self.message_id
            .clone()
            .unwrap_or_else(|| "qa-scripted-message".to_string())
    }

    fn delay_ms(&self) -> u64 {
        self.delay_ms.unwrap_or(0)
    }

    fn exit_code(&self) -> i32 {
        match self.outcome {
            QaScriptedOutcomeKind::Completed
            | QaScriptedOutcomeKind::WaitCallback
            | QaScriptedOutcomeKind::WaitCi
            | QaScriptedOutcomeKind::AskHuman
            | QaScriptedOutcomeKind::StructuredCommand
            | QaScriptedOutcomeKind::LongResponse => self.exit_code.unwrap_or(0),
            QaScriptedOutcomeKind::Failed
            | QaScriptedOutcomeKind::Stall
            | QaScriptedOutcomeKind::Killed => self.exit_code.unwrap_or(1),
        }
    }

    fn final_message(&self, prompt: &str) -> Option<String> {
        if matches!(self.outcome, QaScriptedOutcomeKind::Stall) {
            return None;
        }
        if let Some(message) = &self.final_message {
            return Some(message.clone());
        }
        match self.outcome {
            QaScriptedOutcomeKind::Completed => Some(format!(
                "QA scripted execution completed successfully. Original prompt: {prompt}"
            )),
            QaScriptedOutcomeKind::Failed => Some(format!(
                "QA scripted execution failed intentionally. Original prompt: {prompt}"
            )),
            QaScriptedOutcomeKind::Killed => Some(format!(
                "QA scripted execution marked killed scenario; executor exits non-zero because killed status is owned by container stop paths. Original prompt: {prompt}"
            )),
            QaScriptedOutcomeKind::WaitCallback => Some(format!(
                "QA_SCRIPTED_WAIT callback {}. Original prompt: {prompt}",
                self.wait_ref.as_deref().unwrap_or("callback-ref")
            )),
            QaScriptedOutcomeKind::WaitCi => Some(format!(
                "QA_SCRIPTED_WAIT ci {}. Original prompt: {prompt}",
                self.wait_ref.as_deref().unwrap_or("ci-ref")
            )),
            QaScriptedOutcomeKind::AskHuman => Some(format!(
                "QA_SCRIPTED_ASK_HUMAN {}. Original prompt: {prompt}",
                self.wait_ref.as_deref().unwrap_or("human-question-ref")
            )),
            QaScriptedOutcomeKind::StructuredCommand => Some(
                self.structured_command
                    .clone()
                    .unwrap_or_else(|| {
                        serde_json::json!({
                            "type": "qa_scripted_command",
                            "action": "continue",
                            "prompt": prompt,
                        })
                    })
                    .to_string(),
            ),
            QaScriptedOutcomeKind::LongResponse => Some(format!(
                "QA scripted long response start. {} QA scripted long response end. Original prompt: {prompt}",
                "x".repeat(5000)
            )),
            QaScriptedOutcomeKind::Stall => None,
        }
    }
}

#[cfg(feature = "qa-mode")]
#[async_trait]
impl StandardCodingAgentExecutor for QaMockExecutor {
    fn apply_overrides(&mut self, _executor_config: &ExecutorConfig) {}

    async fn spawn(
        &self,
        current_dir: &Path,
        prompt: &str,
        env: &ExecutionEnv,
    ) -> Result<SpawnedChild, ExecutorError> {
        self.spawn_mock(current_dir, prompt, env).await
    }

    async fn spawn_follow_up(
        &self,
        current_dir: &Path,
        prompt: &str,
        session_id: &str,
        reset_to_message_id: Option<&str>,
        env: &ExecutionEnv,
    ) -> Result<SpawnedChild, ExecutorError> {
        self.spawn_follow_up_mock(current_dir, prompt, session_id, reset_to_message_id, env)
            .await
    }

    fn normalize_logs(
        &self,
        msg_store: Arc<MsgStore>,
        current_dir: &Path,
    ) -> Vec<tokio::task::JoinHandle<()>> {
        self.normalize_mock_logs(msg_store, current_dir)
    }

    fn default_mcp_config_path(&self) -> Option<std::path::PathBuf> {
        None // QA mock doesn't need MCP config
    }

    fn get_preset_options(&self) -> ExecutorConfig {
        ExecutorConfig {
            executor: BaseCodingAgent::QaMock,
            variant: None,
            model_id: Some("qa-mock".to_string()),
            agent_id: None,
            reasoning_id: None,
            permission_policy: Some(crate::model_selector::PermissionPolicy::Auto),
        }
    }
}

async fn load_scripted_outcome(
    env: &ExecutionEnv,
    prompt: &str,
) -> Result<Option<QaScriptedOutcome>, ExecutorError> {
    let inline = env
        .get("VK_QA_SCRIPTED_OUTCOME")
        .cloned()
        .or_else(|| std::env::var("VK_QA_SCRIPTED_OUTCOME").ok());
    if let Some(script) = inline.filter(|value| !value.trim().is_empty()) {
        return serde_json::from_str(&script)
            .map(Some)
            .map_err(ExecutorError::Json);
    }

    let file = env
        .get("VK_QA_SCRIPTED_OUTCOME_FILE")
        .cloned()
        .or_else(|| std::env::var("VK_QA_SCRIPTED_OUTCOME_FILE").ok());
    if let Some(file) = file.filter(|value| !value.trim().is_empty()) {
        let content = tokio::fs::read_to_string(&file)
            .await
            .map_err(|e| ExecutorError::Io(std::io::Error::other(e)))?;
        return select_scripted_outcome_from_file(&file, &content, prompt)
            .map(Some)
            .map_err(ExecutorError::Json);
    }

    Ok(None)
}

fn select_scripted_outcome_from_file(
    file: &str,
    content: &str,
    prompt: &str,
) -> Result<QaScriptedOutcome, serde_json::Error> {
    match serde_json::from_str::<QaScriptedOutcomeFile>(content)? {
        QaScriptedOutcomeFile::Single(script) => Ok(script),
        QaScriptedOutcomeFile::Sequence(outcomes) => Ok(select_scripted_outcome_from_plan(
            file, &outcomes, None, prompt,
        )),
        QaScriptedOutcomeFile::Plan(plan) => Ok(select_scripted_outcome_from_plan(
            file,
            &plan.outcomes,
            plan.fallback.as_ref(),
            prompt,
        )),
    }
}

fn select_scripted_outcome_from_plan(
    file: &str,
    outcomes: &[QaScriptedOutcomePlanEntry],
    fallback: Option<&QaScriptedOutcome>,
    prompt: &str,
) -> QaScriptedOutcome {
    if outcomes.is_empty() {
        return fallback.cloned().unwrap_or_default();
    }

    let mut cursors = SCRIPTED_OUTCOME_PLAN_CURSORS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let cursor = *cursors.get(file).unwrap_or(&0);
    let chosen_index = outcomes
        .iter()
        .enumerate()
        .skip(cursor)
        .find(|(_, entry)| {
            entry
                .prompt_contains
                .as_deref()
                .is_none_or(|needle| prompt.contains(needle))
        })
        .map(|(index, _)| index);

    if let Some(index) = chosen_index {
        cursors.insert(file.to_string(), index + 1);
        return outcomes[index].outcome.clone();
    }

    fallback
        .cloned()
        .unwrap_or_else(|| outcomes.last().expect("non-empty outcomes").outcome.clone())
}

async fn spawn_log_process(
    current_dir: &Path,
    logs: Vec<String>,
    exit_code: i32,
    delay_ms: u64,
) -> Result<SpawnedChild, ExecutorError> {
    let temp_dir = std::env::temp_dir();
    let log_file = temp_dir.join(format!("qa_mock_logs_{}.jsonl", uuid::Uuid::new_v4()));

    // Write all logs to file, one per line
    let content = logs.join("\n") + "\n";
    tokio::fs::write(&log_file, &content)
        .await
        .map_err(|e| ExecutorError::Io(std::io::Error::other(e)))?;

    // Use shell variables as literal numeric values generated by Rust, and quote
    // the temp path to preserve spaces. Tests run this only behind qa-mode.
    let script = format!(
        r#"while IFS= read -r line; do echo "$line"; if [ {delay_ms} -gt 0 ]; then sleep {sleep_seconds}; fi; done < "{file}"; rm -f "{file}"; exit {exit_code}"#,
        delay_ms = delay_ms,
        sleep_seconds = format_millis_as_sleep_seconds(delay_ms),
        file = log_file.display(),
        exit_code = exit_code
    );

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(&script)
        .current_dir(current_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = cmd.group_spawn_no_window().map_err(ExecutorError::Io)?;
    Ok(SpawnedChild::from(child))
}

fn format_millis_as_sleep_seconds(delay_ms: u64) -> String {
    if delay_ms == 0 {
        "0".to_string()
    } else {
        format!("{}.{:03}", delay_ms / 1000, delay_ms % 1000)
    }
}

fn env_value_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Perform random file operations in the worktree
async fn perform_file_operations(dir: &Path) {
    info!("QA Mock: performing file operations in {:?}", dir);

    // Create: qa_created_{uuid}.txt
    let uuid = uuid::Uuid::new_v4();
    let new_file = dir.join(format!("qa_created_{}.txt", uuid));
    match tokio::fs::write(&new_file, "QA mode created this file\n").await {
        Ok(_) => info!("QA Mock: created file {:?}", new_file),
        Err(e) => warn!("QA Mock: failed to create file: {}", e),
    }

    // Find files (excluding .git and binary files)
    let files: Vec<_> = walkdir::WalkDir::new(dir)
        .max_depth(3) // Limit depth to avoid long walks
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| !e.path().to_string_lossy().contains(".git"))
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ["rs", "ts", "js", "txt", "md", "json"].contains(&ext))
        })
        .collect();

    if files.len() >= 2 {
        // Pick random indices before any await points (thread_rng is not Send)
        let (remove_idx, modify_idx) = {
            let mut rng = rand::thread_rng();
            let mut indices: Vec<usize> = (0..files.len()).collect();
            indices.shuffle(&mut rng);
            (indices.first().copied(), indices.get(1).copied())
        };

        // Remove a random file (first shuffled index)
        if let Some(idx) = remove_idx {
            let file_to_remove = files[idx].path().to_path_buf();
            // Don't remove the file we just created
            if file_to_remove != new_file {
                match tokio::fs::remove_file(&file_to_remove).await {
                    Ok(_) => info!("QA Mock: removed file {:?}", file_to_remove),
                    Err(e) => warn!("QA Mock: failed to remove file: {}", e),
                }
            }
        }

        // Modify a different random file (second shuffled index)
        if let Some(idx) = modify_idx {
            let file_to_modify = files[idx].path().to_path_buf();
            // Don't modify the file we just created
            if file_to_modify != new_file {
                match tokio::fs::read_to_string(&file_to_modify).await {
                    Ok(content) => {
                        let modified = format!(
                            "{}\n// QA modification at {}\n",
                            content,
                            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
                        );
                        match tokio::fs::write(&file_to_modify, modified).await {
                            Ok(_) => info!("QA Mock: modified file {:?}", file_to_modify),
                            Err(e) => warn!("QA Mock: failed to write modified file: {}", e),
                        }
                    }
                    Err(e) => warn!("QA Mock: failed to read file for modification: {}", e),
                }
            }
        }
    } else {
        info!(
            "QA Mock: not enough files found for remove/modify operations (found {})",
            files.len()
        );
    }
}

fn generate_scripted_logs(prompt: &str, script: &QaScriptedOutcome) -> Vec<String> {
    let session_id = script.session_id();
    let mut logs = vec![system_init(&session_id, "qa-scripted-executor")];

    logs.push(assistant_text(
        &session_id,
        "msg-qa-scripted-thinking",
        "uuid-qa-scripted-thinking",
        "QA scripted executor selected deterministic outcome.",
        None,
    ));

    if let Some(final_message) = script.final_message(prompt) {
        logs.push(assistant_text(
            &session_id,
            &script.message_id(),
            "uuid-qa-scripted-final",
            &final_message,
            Some("end_turn"),
        ));
    } else {
        logs.push(system_status(
            &session_id,
            "QA_SCRIPTED_STALL no final assistant response emitted for scanner tests.",
        ));
    }

    logs.push(result_log(
        &session_id,
        script.exit_code() == 0,
        script.delay_ms(),
    ));

    logs.into_iter()
        .map(|log| serde_json::to_string(&log).expect("ClaudeJson should serialize"))
        .collect()
}

fn system_init(session_id: &str, model: &str) -> ClaudeJson {
    ClaudeJson::System {
        subtype: Some("init".to_string()),
        session_id: Some(session_id.to_string()),
        cwd: None,
        tools: None,
        model: Some(model.to_string()),
        api_key_source: Some("unknown".to_string()),
        status: None,
        slash_commands: vec![],
        plugins: vec![],
        agents: vec![],
        task_id: None,
        tool_use_id: None,
        description: None,
        task_type: None,
        prompt: None,
        summary: None,
        last_tool_name: None,
    }
}

fn system_status(session_id: &str, status: &str) -> ClaudeJson {
    ClaudeJson::System {
        subtype: Some("status".to_string()),
        session_id: Some(session_id.to_string()),
        cwd: None,
        tools: None,
        model: Some("qa-scripted-executor".to_string()),
        api_key_source: Some("unknown".to_string()),
        status: Some(status.to_string()),
        slash_commands: vec![],
        plugins: vec![],
        agents: vec![],
        task_id: None,
        tool_use_id: None,
        description: None,
        task_type: None,
        prompt: None,
        summary: None,
        last_tool_name: None,
    }
}

fn assistant_text(
    session_id: &str,
    message_id: &str,
    uuid: &str,
    text: &str,
    stop_reason: Option<&str>,
) -> ClaudeJson {
    ClaudeJson::Assistant {
        message: ClaudeMessage {
            id: Some(message_id.to_string()),
            message_type: Some("message".to_string()),
            role: "assistant".to_string(),
            model: Some("qa-scripted".to_string()),
            content: ClaudeMessageContent::Array(vec![ClaudeContentItem::Text {
                text: text.to_string(),
            }]),
            stop_reason: stop_reason.map(str::to_string),
        },
        session_id: Some(session_id.to_string()),
        uuid: Some(uuid.to_string()),
    }
}

fn result_log(session_id: &str, success: bool, delay_ms: u64) -> ClaudeJson {
    ClaudeJson::Result {
        subtype: Some(if success { "success" } else { "error" }.to_string()),
        is_error: Some(!success),
        duration_ms: Some(delay_ms),
        result: None,
        error: if success {
            None
        } else {
            Some("QA scripted executor exited with failure".to_string())
        },
        num_turns: Some(1),
        session_id: Some(session_id.to_string()),
        model_usage: None,
        usage: None,
    }
}

/// Generate 10 mock log entries in ClaudeJson format using strongly-typed structs
fn generate_mock_logs(prompt: &str) -> Vec<String> {
    let session_id = uuid::Uuid::new_v4().to_string();

    let logs: Vec<ClaudeJson> = vec![
        // 1. System init
        ClaudeJson::System {
            subtype: Some("init".to_string()),
            session_id: Some(session_id.clone()),
            cwd: None,
            tools: None,
            model: Some("qa-mock-executor".to_string()),
            api_key_source: Some("unknown".to_string()),
            status: None,
            slash_commands: vec![],
            plugins: vec![],
            agents: vec![],
            task_id: None,
            tool_use_id: None,
            description: None,
            task_type: None,
            prompt: None,
            summary: None,
            last_tool_name: None,
        },
        // 2. Assistant thinking
        ClaudeJson::Assistant {
            message: ClaudeMessage {
                id: Some("msg-qa-1".to_string()),
                message_type: Some("message".to_string()),
                role: "assistant".to_string(),
                model: Some("qa-mock".to_string()),
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::Thinking {
                    thinking: "Analyzing the QA task and preparing mock execution...".to_string(),
                }]),
                stop_reason: None,
            },
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-1".to_string()),
        },
        // 3. Read tool use
        ClaudeJson::Assistant {
            message: ClaudeMessage {
                id: Some("msg-qa-2".to_string()),
                message_type: Some("message".to_string()),
                role: "assistant".to_string(),
                model: Some("qa-mock".to_string()),
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::ToolUse {
                    id: "qa-tool-1".to_string(),
                    tool_data: ClaudeToolData::Read {
                        file_path: "README.md".to_string(),
                    },
                }]),
                stop_reason: None,
            },
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-2".to_string()),
        },
        // 4. Read tool result
        ClaudeJson::User {
            message: ClaudeMessage {
                id: Some("msg-qa-3".to_string()),
                message_type: Some("message".to_string()),
                role: "user".to_string(),
                model: None,
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::ToolResult {
                    tool_use_id: "qa-tool-1".to_string(),
                    content: serde_json::json!(
                        "# Project README\\n\\nThis is a QA test repository."
                    ),
                    is_error: Some(false),
                }]),
                stop_reason: None,
            },
            is_synthetic: false,
            is_replay: false,
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-3".to_string()),
        },
        // 5. Write tool use
        ClaudeJson::Assistant {
            message: ClaudeMessage {
                id: Some("msg-qa-4".to_string()),
                message_type: Some("message".to_string()),
                role: "assistant".to_string(),
                model: Some("qa-mock".to_string()),
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::ToolUse {
                    id: "qa-tool-2".to_string(),
                    tool_data: ClaudeToolData::Write {
                        file_path: "qa_output.txt".to_string(),
                        content: "QA generated content".to_string(),
                    },
                }]),
                stop_reason: None,
            },
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-4".to_string()),
        },
        // 6. Write tool result
        ClaudeJson::User {
            message: ClaudeMessage {
                id: Some("msg-qa-5".to_string()),
                message_type: Some("message".to_string()),
                role: "user".to_string(),
                model: None,
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::ToolResult {
                    tool_use_id: "qa-tool-2".to_string(),
                    content: serde_json::json!("File written successfully"),
                    is_error: Some(false),
                }]),
                stop_reason: None,
            },
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-5".to_string()),
            is_synthetic: false,
            is_replay: false,
        },
        // 7. Bash tool use
        ClaudeJson::Assistant {
            message: ClaudeMessage {
                id: Some("msg-qa-6".to_string()),
                message_type: Some("message".to_string()),
                role: "assistant".to_string(),
                model: Some("qa-mock".to_string()),
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::ToolUse {
                    id: "qa-tool-3".to_string(),
                    tool_data: ClaudeToolData::Bash {
                        command: "echo 'QA test complete'".to_string(),
                        description: Some("Run QA test command".to_string()),
                    },
                }]),
                stop_reason: None,
            },
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-6".to_string()),
        },
        // 8. Bash tool result
        ClaudeJson::User {
            message: ClaudeMessage {
                id: Some("msg-qa-7".to_string()),
                message_type: Some("message".to_string()),
                role: "user".to_string(),
                model: None,
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::ToolResult {
                    tool_use_id: "qa-tool-3".to_string(),
                    content: serde_json::json!("QA test complete\\n"),
                    is_error: Some(false),
                }]),
                stop_reason: None,
            },
            is_synthetic: false,
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-7".to_string()),
            is_replay: false,
        },
        // 9. Assistant final message
        ClaudeJson::Assistant {
            message: ClaudeMessage {
                id: Some("msg-qa-8".to_string()),
                message_type: Some("message".to_string()),
                role: "assistant".to_string(),
                model: Some("qa-mock".to_string()),
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::Text {
                    text: format!(
                        "QA mode execution completed successfully.\\n\\nI performed the following operations:\\n1. Read README.md\\n2. Created qa_output.txt\\n3. Ran a test command\\nOriginal prompt: {}",
                        prompt
                    ),
                }]),
                stop_reason: Some("end_turn".to_string()),
            },
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-8".to_string()),
        },
        // 10. Result success
        ClaudeJson::Result {
            subtype: Some("success".to_string()),
            is_error: Some(false),
            duration_ms: Some(10000),
            result: None,
            error: None,
            num_turns: Some(3),
            session_id: Some(session_id),
            model_usage: None,
            usage: None,
        },
    ];

    // Serialize to JSON strings - this ensures proper escaping
    logs.into_iter()
        .map(|log| serde_json::to_string(&log).expect("ClaudeJson should serialize"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_logs_are_deterministic_and_deserialize() {
        let script = QaScriptedOutcome {
            outcome: QaScriptedOutcomeKind::Completed,
            final_message: Some("done deterministically".to_string()),
            session_id: Some("agent-session-1".to_string()),
            message_id: Some("agent-message-1".to_string()),
            ..QaScriptedOutcome::default()
        };

        let first = generate_scripted_logs("same prompt", &script);
        let second = generate_scripted_logs("same prompt", &script);
        assert_eq!(first, second);
        assert_eq!(first.len(), 4);

        for line in &first {
            serde_json::from_str::<ClaudeJson>(line).expect("scripted log should be ClaudeJson");
        }

        let parsed: ClaudeJson = serde_json::from_str(&first[2]).unwrap();
        match parsed {
            ClaudeJson::Assistant {
                message,
                session_id,
                ..
            } => {
                assert_eq!(session_id.as_deref(), Some("agent-session-1"));
                assert_eq!(message.id.as_deref(), Some("agent-message-1"));
                assert_eq!(
                    message.content,
                    ClaudeMessageContent::Array(vec![ClaudeContentItem::Text {
                        text: "done deterministically".to_string()
                    }])
                );
                assert_eq!(message.stop_reason.as_deref(), Some("end_turn"));
            }
            other => panic!("expected assistant final log, got {other:?}"),
        }
    }

    #[test]
    fn scripted_failure_and_wait_markers_are_explicit() {
        let failure = QaScriptedOutcome {
            outcome: QaScriptedOutcomeKind::Failed,
            final_message: None,
            ..QaScriptedOutcome::default()
        };
        assert_eq!(failure.exit_code(), 1);
        assert!(
            failure
                .final_message("fail prompt")
                .unwrap()
                .contains("failed")
        );

        let wait = QaScriptedOutcome {
            outcome: QaScriptedOutcomeKind::WaitCi,
            wait_ref: Some("ci-run-123".to_string()),
            ..QaScriptedOutcome::default()
        };
        assert_eq!(wait.exit_code(), 0);
        assert_eq!(
            wait.final_message("wait prompt").unwrap(),
            "QA_SCRIPTED_WAIT ci ci-run-123. Original prompt: wait prompt"
        );
    }

    #[test]
    fn scripted_structured_and_long_responses_are_final_messages() {
        let structured = QaScriptedOutcome {
            outcome: QaScriptedOutcomeKind::StructuredCommand,
            final_message: None,
            structured_command: Some(
                serde_json::json!({"command":"fan_in","target":"orchestrator"}),
            ),
            wait_ref: None,
            session_id: None,
            message_id: None,
            delay_ms: None,
            exit_code: None,
        };
        assert_eq!(
            structured.final_message("ignored").unwrap(),
            r#"{"command":"fan_in","target":"orchestrator"}"#
        );

        let long = QaScriptedOutcome {
            outcome: QaScriptedOutcomeKind::LongResponse,
            final_message: None,
            ..QaScriptedOutcome::default()
        };
        assert!(long.final_message("long").unwrap().len() > 4096);
    }

    #[test]
    fn scripted_stall_emits_no_final_end_turn_message() {
        let stall = QaScriptedOutcome {
            outcome: QaScriptedOutcomeKind::Stall,
            session_id: Some("stall-session".to_string()),
            ..QaScriptedOutcome::default()
        };
        assert_eq!(stall.exit_code(), 1);
        assert!(stall.final_message("stall").is_none());
        let logs = generate_scripted_logs("stall", &stall);
        let marker: ClaudeJson = serde_json::from_str(&logs[2]).unwrap();
        match marker {
            ClaudeJson::System {
                subtype, status, ..
            } => {
                assert_eq!(subtype.as_deref(), Some("status"));
                assert!(status.unwrap().contains("QA_SCRIPTED_STALL"));
            }
            other => panic!("expected system stall marker, got {other:?}"),
        }
    }

    #[test]
    fn scripted_outcome_file_can_return_sequential_decision_responses() {
        let file = format!("test-sequence-{}", uuid::Uuid::new_v4());
        let content = serde_json::json!({
            "outcomes": [
                {
                    "outcome": "completed",
                    "final_message": "first",
                    "prompt_contains": "dev"
                },
                {
                    "outcome": "completed",
                    "final_message": "<decision action=\"approved\" />"
                }
            ],
            "fallback": {
                "outcome": "completed",
                "final_message": "fallback"
            }
        })
        .to_string();

        let first = select_scripted_outcome_from_file(&file, &content, "hello dev")
            .expect("plan should parse");
        let second = select_scripted_outcome_from_file(&file, &content, "review prompt")
            .expect("plan should parse");
        let third = select_scripted_outcome_from_file(&file, &content, "extra prompt")
            .expect("plan should parse");

        assert_eq!(first.final_message("prompt").as_deref(), Some("first"));
        assert_eq!(
            second.final_message("prompt").as_deref(),
            Some("<decision action=\"approved\" />")
        );
        assert_eq!(third.final_message("prompt").as_deref(), Some("fallback"));
    }

    #[test]
    fn test_generate_mock_logs_count() {
        let logs = generate_mock_logs("test prompt");
        assert_eq!(logs.len(), 10, "Should generate exactly 10 log entries");
    }

    #[test]
    fn test_generate_mock_logs_valid_json() {
        let logs = generate_mock_logs("test prompt");
        for (i, log) in logs.iter().enumerate() {
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(log);
            assert!(
                parsed.is_ok(),
                "Log entry {} should be valid JSON: {}",
                i,
                log
            );
        }
    }

    #[test]
    fn test_generate_mock_logs_deserializes_to_claudejson() {
        let logs = generate_mock_logs("test prompt");
        for (i, log) in logs.iter().enumerate() {
            let parsed: Result<ClaudeJson, _> = serde_json::from_str(log);
            assert!(
                parsed.is_ok(),
                "Log entry {} should deserialize to ClaudeJson: {} - error: {:?}",
                i,
                log,
                parsed.err()
            );
        }
    }

    #[test]
    fn test_escape_special_characters() {
        let logs = generate_mock_logs("test with \"quotes\" and\nnewlines");
        // The final assistant message (index 8) should contain the prompt
        let final_log = &logs[8];
        let parsed: ClaudeJson = serde_json::from_str(final_log).unwrap();

        if let ClaudeJson::Assistant { message, .. } = parsed {
            if let ClaudeMessageContent::Array(items) = &message.content {
                if let Some(ClaudeContentItem::Text { text }) = items.first() {
                    assert!(text.contains("test with \"quotes\" and\nnewlines"));
                } else {
                    panic!("Expected Text content item");
                }
            } else {
                panic!("Expected Array content");
            }
        } else {
            panic!("Expected Assistant variant");
        }
    }
}
