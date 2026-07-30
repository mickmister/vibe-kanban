pub mod executor_approvals;

use std::{collections::HashSet, sync::Arc, time::Duration as StdDuration};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures::{
    StreamExt,
    future::{BoxFuture, FutureExt, Shared},
};
use json_patch::Patch;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    io::AsyncReadExt,
    process::Command,
    sync::{broadcast, oneshot},
};
use tokio_stream::wrappers::BroadcastStream;
use ts_rs::TS;
use utils::{
    approvals::{
        ApprovalOutcome, ApprovalRequest, ApprovalResponse, HostCommandRequest, HostCommandResult,
    },
    shell::get_shell_command,
};
use uuid::Uuid;

#[derive(Debug)]
struct PendingApproval {
    execution_process_id: Uuid,
    tool_name: String,
    is_question: bool,
    created_at: DateTime<Utc>,
    timeout_at: DateTime<Utc>,
    response_tx: oneshot::Sender<ApprovalOutcome>,
    host_command: Option<HostCommandRequest>,
}

pub(crate) type ApprovalWaiter = Shared<BoxFuture<'static, ApprovalOutcome>>;

#[derive(Debug)]
pub struct ToolContext {
    pub tool_name: String,
    pub execution_process_id: Uuid,
}

/// Info about a currently pending approval, sent to the frontend via WebSocket.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct ApprovalInfo {
    pub approval_id: String,
    pub tool_name: String,
    pub execution_process_id: Uuid,
    pub is_question: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub host_command: Option<HostCommandRequest>,
    pub created_at: DateTime<Utc>,
    pub timeout_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct Approvals {
    pending: Arc<DashMap<String, PendingApproval>>,
    completed: Arc<DashMap<String, ApprovalOutcome>>,
    patches_tx: broadcast::Sender<Patch>,
}

#[derive(Debug, Error)]
pub enum ApprovalError {
    #[error("approval request not found")]
    NotFound,
    #[error("approval request already completed")]
    AlreadyCompleted,
    #[error("no executor session found for session_id: {0}")]
    NoExecutorSession(String),
    #[error("invalid approval status for this tool type")]
    InvalidStatus,
    #[error(transparent)]
    Custom(#[from] anyhow::Error),
}

impl Default for Approvals {
    fn default() -> Self {
        Self::new()
    }
}

impl Approvals {
    pub fn new() -> Self {
        let (patches_tx, _) = broadcast::channel(64);
        Self {
            pending: Arc::new(DashMap::new()),
            completed: Arc::new(DashMap::new()),
            patches_tx,
        }
    }

    pub(crate) async fn create_with_waiter(
        &self,
        request: ApprovalRequest,
        is_question: bool,
    ) -> Result<(ApprovalRequest, ApprovalWaiter), ApprovalError> {
        let (tx, rx) = oneshot::channel();
        let default_timeout = ApprovalOutcome::TimedOut;
        let waiter: ApprovalWaiter = rx
            .map(move |result| result.unwrap_or(default_timeout))
            .boxed()
            .shared();
        let req_id = request.id.clone();

        let info = ApprovalInfo {
            approval_id: req_id.clone(),
            tool_name: request.tool_name.clone(),
            execution_process_id: request.execution_process_id,
            is_question,
            host_command: request.host_command.clone(),
            created_at: request.created_at,
            timeout_at: request.timeout_at,
        };

        let pending_approval = PendingApproval {
            execution_process_id: request.execution_process_id,
            tool_name: request.tool_name.clone(),
            is_question,
            host_command: request.host_command.clone(),
            created_at: request.created_at,
            timeout_at: request.timeout_at,
            response_tx: tx,
        };

        self.pending.insert(req_id.clone(), pending_approval);

        let _ = self
            .patches_tx
            .send(crate::services::events::patches::approvals_patch::created(
                &info,
            ));

        self.spawn_timeout_watcher(req_id.clone(), request.timeout_at, waiter.clone());
        Ok((request, waiter))
    }

    fn validate_approval_response(
        outcome: &ApprovalOutcome,
        is_question: bool,
    ) -> Result<(), ApprovalError> {
        match outcome {
            ApprovalOutcome::Approved | ApprovalOutcome::Denied { .. } if is_question => {
                Err(ApprovalError::InvalidStatus)
            }
            ApprovalOutcome::Answered { .. } if !is_question => Err(ApprovalError::InvalidStatus),
            ApprovalOutcome::HostCommandCompleted { .. } => Err(ApprovalError::InvalidStatus),
            _ => Ok(()),
        }
    }

    #[tracing::instrument(skip(self, id, req))]
    pub async fn respond(
        &self,
        id: &str,
        req: ApprovalResponse,
    ) -> Result<(ApprovalOutcome, ToolContext), ApprovalError> {
        if let Some((_, p)) = self.pending.remove(id) {
            if let Err(e) = Self::validate_approval_response(&req.status, p.is_question) {
                self.pending.insert(id.to_string(), p);
                return Err(e);
            }

            let mut outcome = req.status.clone();
            if let (Some(host_command), ApprovalOutcome::Approved) = (&p.host_command, &req.status)
            {
                tracing::info!(
                    approval_id = %id,
                    execution_process_id = %p.execution_process_id,
                    command = %host_command.command,
                    cwd = %host_command.cwd,
                    reason = %host_command.reason,
                    env_keys = ?host_command.env.keys().collect::<Vec<_>>(),
                    timeout_secs = host_command.timeout_secs,
                    output_limit_bytes = host_command.output_limit_bytes,
                    "approved host command; executing on host"
                );
                let result = run_host_command(host_command.clone()).await;
                tracing::info!(
                    approval_id = %id,
                    execution_process_id = %p.execution_process_id,
                    exit_code = ?result.exit_code,
                    timed_out = result.timed_out,
                    truncated = result.truncated,
                    "host command completed"
                );
                outcome = ApprovalOutcome::HostCommandCompleted { result };
            }
            self.completed.insert(id.to_string(), outcome.clone());
            let _ = p.response_tx.send(outcome.clone());

            let _ =
                self.patches_tx
                    .send(crate::services::events::patches::approvals_patch::resolved(
                        id,
                    ));

            let tool_ctx = ToolContext {
                tool_name: p.tool_name,
                execution_process_id: p.execution_process_id,
            };

            Ok((outcome, tool_ctx))
        } else if self.completed.contains_key(id) {
            Err(ApprovalError::AlreadyCompleted)
        } else {
            Err(ApprovalError::NotFound)
        }
    }

    #[tracing::instrument(skip(self, id, timeout_at, waiter))]
    fn spawn_timeout_watcher(
        &self,
        id: String,
        timeout_at: chrono::DateTime<chrono::Utc>,
        waiter: ApprovalWaiter,
    ) {
        let pending = self.pending.clone();
        let completed = self.completed.clone();
        let patches_tx = self.patches_tx.clone();

        let timeout_outcome = ApprovalOutcome::TimedOut;

        let now = chrono::Utc::now();
        let to_wait = (timeout_at - now)
            .to_std()
            .unwrap_or_else(|_| StdDuration::from_secs(0));
        let deadline = tokio::time::Instant::now() + to_wait;

        tokio::spawn(async move {
            let outcome = tokio::select! {
                biased;

                resolved = waiter.clone() => resolved,
                _ = tokio::time::sleep_until(deadline) => timeout_outcome,
            };

            let is_timeout = matches!(&outcome, ApprovalOutcome::TimedOut);
            completed.insert(id.clone(), outcome.clone());

            if is_timeout && let Some((_, pending_approval)) = pending.remove(&id) {
                let _ = patches_tx.send(
                    crate::services::events::patches::approvals_patch::resolved(&id),
                );
                if pending_approval.response_tx.send(outcome).is_err() {
                    tracing::debug!("approval '{}' timeout notification receiver dropped", id);
                }
            }
        });
    }

    pub(crate) async fn cancel(&self, id: &str) {
        if let Some((_, _pending_approval)) = self.pending.remove(id) {
            let outcome = ApprovalOutcome::Denied {
                reason: Some("Cancelled".to_string()),
            };
            self.completed.insert(id.to_string(), outcome);
            let _ =
                self.patches_tx
                    .send(crate::services::events::patches::approvals_patch::resolved(
                        id,
                    ));
            tracing::debug!("Cancelled approval '{}'", id);
        }
    }

    pub fn patch_stream(&self) -> futures::stream::BoxStream<'static, Patch> {
        let approvals = self.clone();
        let snapshot =
            crate::services::events::patches::approvals_patch::snapshot(&approvals.pending_infos());

        let live = BroadcastStream::new(self.patches_tx.subscribe()).filter_map(move |result| {
            let approvals = approvals.clone();
            async move {
                match result {
                    Ok(patch) => Some(patch),
                    Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(_)) => {
                        Some(crate::services::events::patches::approvals_patch::snapshot(
                            &approvals.pending_infos(),
                        ))
                    }
                }
            }
        });

        futures::stream::iter([snapshot]).chain(live).boxed()
    }

    /// Check which execution processes have pending approvals.
    /// Returns a set of execution_process_ids that have at least one pending approval.
    pub fn get_pending_execution_process_ids(
        &self,
        execution_process_ids: &[Uuid],
    ) -> HashSet<Uuid> {
        let id_set: HashSet<_> = execution_process_ids.iter().collect();
        self.pending
            .iter()
            .filter_map(|entry| {
                let ep_id = entry.value().execution_process_id;
                if id_set.contains(&ep_id) {
                    Some(ep_id)
                } else {
                    None
                }
            })
            .collect()
    }

    fn pending_infos(&self) -> Vec<ApprovalInfo> {
        self.pending
            .iter()
            .map(|entry| {
                let p = entry.value();
                ApprovalInfo {
                    approval_id: entry.key().clone(),
                    tool_name: p.tool_name.clone(),
                    execution_process_id: p.execution_process_id,
                    is_question: p.is_question,
                    host_command: p.host_command.clone(),
                    created_at: p.created_at,
                    timeout_at: p.timeout_at,
                }
            })
            .collect()
    }
}

async fn run_host_command(request: HostCommandRequest) -> HostCommandResult {
    let (shell_cmd, shell_arg) = get_shell_command();
    let mut command = Command::new(shell_cmd);
    command
        .arg(shell_arg)
        .arg(&request.command)
        .current_dir(&request.cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in &request.env {
        command.env(key, value);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return HostCommandResult {
                exit_code: None,
                timed_out: false,
                stdout: String::new(),
                stderr: format!("Failed to spawn host command: {err}"),
                truncated: false,
            };
        }
    };

    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf).await;
        buf
    });
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        buf
    });

    let wait = tokio::time::timeout(
        std::time::Duration::from_secs(request.timeout_secs),
        child.wait(),
    )
    .await;
    let (exit_code, timed_out) = match wait {
        Ok(Ok(status)) => (status.code().map(i64::from), false),
        Ok(Err(err)) => {
            return HostCommandResult {
                exit_code: None,
                timed_out: false,
                stdout: String::new(),
                stderr: format!("Failed to wait for host command: {err}"),
                truncated: false,
            };
        }
        Err(_) => {
            let _ = child.kill().await;
            (None, true)
        }
    };

    let stdout = out_task.await.unwrap_or_default();
    let stderr = err_task.await.unwrap_or_default();
    let (stdout, stderr, truncated) = truncate_output(stdout, stderr, request.output_limit_bytes);
    HostCommandResult {
        exit_code,
        timed_out,
        stdout,
        stderr,
        truncated,
    }
}

fn truncate_output(stdout: Vec<u8>, stderr: Vec<u8>, limit: usize) -> (String, String, bool) {
    let mut truncated = false;
    let mut stdout = stdout;
    let mut stderr = stderr;
    if stdout.len() > limit {
        stdout.truncate(limit);
        truncated = true;
    }
    if stderr.len() > limit {
        stderr.truncate(limit);
        truncated = true;
    }
    (
        String::from_utf8_lossy(&stdout).to_string(),
        String::from_utf8_lossy(&stderr).to_string(),
        truncated,
    )
}

#[cfg(test)]
mod tests {
    use utils::approvals::{ApprovalOutcome, ApprovalResponse, HostCommandRequest};
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn denied_host_command_does_not_run() {
        let approvals = Approvals::new();
        let marker_dir = tempfile::tempdir().unwrap();
        let marker = marker_dir.path().join("marker");
        let mut request = ApprovalRequest::new("host_command".to_string(), Uuid::new_v4());
        request.host_command = Some(HostCommandRequest {
            command: format!("touch {}", marker.display()),
            cwd: std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            reason: "test".to_string(),
            env: Default::default(),
            timeout_secs: 5,
            output_limit_bytes: 1024,
        });
        let id = request.id.clone();
        let (_request, waiter) = approvals.create_with_waiter(request, false).await.unwrap();
        approvals
            .respond(
                &id,
                ApprovalResponse {
                    execution_process_id: Uuid::new_v4(),
                    status: ApprovalOutcome::Denied { reason: None },
                },
            )
            .await
            .unwrap();
        assert!(matches!(waiter.await, ApprovalOutcome::Denied { .. }));
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn approved_host_command_runs_exact_command_cwd_and_caps_output() {
        let approvals = Approvals::new();
        let cwd = tempfile::tempdir().unwrap();
        let mut request = ApprovalRequest::new("host_command".to_string(), Uuid::new_v4());
        request.host_command = Some(HostCommandRequest {
            command: "pwd; printf abcdef".to_string(),
            cwd: cwd.path().to_string_lossy().to_string(),
            reason: "test".to_string(),
            env: Default::default(),
            timeout_secs: 5,
            output_limit_bytes: 1024,
        });
        let id = request.id.clone();
        let (_request, waiter) = approvals.create_with_waiter(request, false).await.unwrap();
        approvals
            .respond(
                &id,
                ApprovalResponse {
                    execution_process_id: Uuid::new_v4(),
                    status: ApprovalOutcome::Approved,
                },
            )
            .await
            .unwrap();
        let ApprovalOutcome::HostCommandCompleted { result } = waiter.await else {
            panic!("expected host command completion");
        };
        assert_eq!(result.exit_code, Some(0));
        assert!(
            result
                .stdout
                .contains(&cwd.path().to_string_lossy().to_string())
        );
        assert!(result.stdout.contains("abcdef"));
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn approved_host_command_timeout_is_enforced() {
        let approvals = Approvals::new();
        let mut request = ApprovalRequest::new("host_command".to_string(), Uuid::new_v4());
        request.host_command = Some(HostCommandRequest {
            command: "sleep 5".to_string(),
            cwd: std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            reason: "test".to_string(),
            env: Default::default(),
            timeout_secs: 0,
            output_limit_bytes: 3,
        });
        let id = request.id.clone();
        let (_request, waiter) = approvals.create_with_waiter(request, false).await.unwrap();
        approvals
            .respond(
                &id,
                ApprovalResponse {
                    execution_process_id: Uuid::new_v4(),
                    status: ApprovalOutcome::Approved,
                },
            )
            .await
            .unwrap();
        let ApprovalOutcome::HostCommandCompleted { result } = waiter.await else {
            panic!("expected host command completion");
        };
        assert!(result.timed_out);
    }

    #[tokio::test]
    async fn approved_host_command_output_cap_is_enforced() {
        let result = run_host_command(HostCommandRequest {
            command: "printf abcdef".to_string(),
            cwd: std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            reason: "test".to_string(),
            env: Default::default(),
            timeout_secs: 5,
            output_limit_bytes: 3,
        })
        .await;
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.stdout, "abc");
        assert!(result.truncated);
    }
}
