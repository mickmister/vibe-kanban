use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use ts_rs::TS;
use workspace_utils::{log_msg::LogMsg, msg_store::MsgStore};

#[cfg(not(feature = "qa-mode"))]
use crate::profile::ExecutorConfigs;
use crate::{
    actions::Executable,
    approvals::ExecutorApprovalService,
    env::ExecutionEnv,
    executors::{BaseCodingAgent, ExecutorError, SpawnedChild, StandardCodingAgentExecutor},
    logs::{
        NormalizedEntry, NormalizedEntryType,
        utils::{ConversationPatch, EntryIndexProvider},
    },
    profile::ExecutorConfig,
    stdout_dup::spawn_local_output_process,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionCommand {
    Clear,
    Compact { instructions: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
pub struct CodingAgentSessionCommandRequest {
    pub command: SessionCommand,
    /// Agent session/thread id to resume when the command needs provider context.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Last known agent message id. Reserved for providers that can fork/resume at a message.
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(alias = "executor_profile_id", alias = "profile_variant_label")]
    pub executor_config: ExecutorConfig,
    #[serde(default)]
    pub working_dir: Option<String>,
}

impl SessionCommand {
    pub fn prompt(&self) -> String {
        match self {
            Self::Clear => "/clear".to_string(),
            Self::Compact { instructions } => instructions
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(|s| format!("/compact {}", s.trim()))
                .unwrap_or_else(|| "/compact".to_string()),
        }
    }
}

impl CodingAgentSessionCommandRequest {
    pub fn effective_dir(&self, current_dir: &Path) -> std::path::PathBuf {
        match &self.working_dir {
            Some(rel_path) => current_dir.join(rel_path),
            None => current_dir.to_path_buf(),
        }
    }

    pub fn base_executor(&self) -> BaseCodingAgent {
        self.executor_config.executor
    }

    pub fn prompt(&self) -> String {
        self.command.prompt()
    }

    fn static_message(&self) -> Option<String> {
        match (&self.command, self.session_id.as_deref()) {
            (SessionCommand::Clear, _) => Some("Context cleared".to_string()),
            (SessionCommand::Compact { .. }, None) => {
                Some("No active context to compact.".to_string())
            }
            (SessionCommand::Compact { .. }, Some(_)) => None,
        }
    }
}

#[async_trait]
impl Executable for CodingAgentSessionCommandRequest {
    #[cfg_attr(feature = "qa-mode", allow(unused_variables))]
    async fn spawn(
        &self,
        current_dir: &Path,
        approvals: Arc<dyn ExecutorApprovalService>,
        env: &ExecutionEnv,
    ) -> Result<SpawnedChild, ExecutorError> {
        if let Some(message) = self.static_message() {
            return spawn_static_session_command_reply(message).await;
        }

        let effective_dir = self.effective_dir(current_dir);
        let session_id = self.session_id.as_deref().ok_or_else(|| {
            ExecutorError::Io(std::io::Error::other(
                "No active session for session command",
            ))
        })?;
        let prompt = self.prompt();

        #[cfg(feature = "qa-mode")]
        {
            tracing::info!("QA mode: using mock executor for session command");
            let executor = crate::executors::qa_mock::QaMockExecutor;
            return executor
                .spawn_follow_up(&effective_dir, &prompt, session_id, None, env)
                .await;
        }

        #[cfg(not(feature = "qa-mode"))]
        {
            let profile_id = self.executor_config.profile_id();
            let mut agent = ExecutorConfigs::get_cached()
                .get_coding_agent(&profile_id)
                .ok_or(ExecutorError::UnknownExecutorType(profile_id.to_string()))?;

            if self.executor_config.has_overrides() {
                agent.apply_overrides(&self.executor_config);
            }
            agent.use_approvals(approvals);

            agent
                .spawn_follow_up(&effective_dir, &prompt, session_id, None, env)
                .await
        }
    }
}

async fn spawn_static_session_command_reply(
    message: String,
) -> Result<SpawnedChild, ExecutorError> {
    let (mut spawned, mut writer) = spawn_local_output_process()?;
    let (exit_signal_tx, exit_signal_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let result = async {
            writer.write_all(message.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await
        }
        .await;

        let _ = exit_signal_tx.send(if result.is_ok() {
            crate::executors::ExecutorExitResult::Success
        } else {
            crate::executors::ExecutorExitResult::Failure
        });
    });

    spawned.exit_signal = Some(exit_signal_rx);
    Ok(spawned)
}

pub fn normalize_static_session_command_logs(
    msg_store: Arc<MsgStore>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let entry_index_provider = EntryIndexProvider::start_from(&msg_store);
    vec![tokio::spawn(async move {
        let mut stream = msg_store.history_plus_stream();
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                LogMsg::Stdout(line) => {
                    let content = line.trim();
                    if content.is_empty() {
                        continue;
                    }
                    let entry = NormalizedEntry {
                        timestamp: None,
                        entry_type: NormalizedEntryType::SystemMessage,
                        content: content.to_string(),
                        metadata: None,
                    };
                    let patch =
                        ConversationPatch::add_normalized_entry(entry_index_provider.next(), entry);
                    msg_store.push_patch(patch);
                }
                LogMsg::Finished => break,
                _ => {}
            }
        }
    })]
}
