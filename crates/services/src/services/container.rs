use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use anyhow::{Error as AnyhowError, anyhow};
use async_trait::async_trait;
use db::{
    DBService,
    models::{
        agent_message_queue::{AgentMessageQueueItem, AgentMessageQueueStatus},
        coding_agent_turn::{CodingAgentResumeInfo, CodingAgentTurn, CreateCodingAgentTurn},
        execution_process::{
            CreateExecutionProcess, ExecutionContext, ExecutionProcess, ExecutionProcessError,
            ExecutionProcessRunReason, ExecutionProcessStatus,
        },
        execution_process_repo_state::{
            CreateExecutionProcessRepoState, ExecutionProcessRepoState,
        },
        repo::Repo,
        session::{CreateSession, Session, SessionError},
        workspace::{Workspace, WorkspaceError},
        workspace_repo::WorkspaceRepo,
    },
};
use executors::{
    actions::{
        ExecutorAction, ExecutorActionType,
        coding_agent_follow_up::CodingAgentFollowUpRequest,
        coding_agent_initial::CodingAgentInitialRequest,
        script::{ScriptContext, ScriptRequest, ScriptRequestLanguage},
        session_command::CodingAgentSessionCommandRequest,
    },
    executors::{
        BaseCodingAgent, ExecutorError, StandardCodingAgentExecutor, qa_mock::QaMockExecutor,
    },
    logs::{
        NormalizedEntry, NormalizedEntryError, NormalizedEntryType,
        utils::{
            ConversationPatch,
            patch::{fix_patch_ops, is_add_or_replace, patch_entry_path},
        },
    },
    profile::{ExecutorConfig, ExecutorConfigs, ExecutorProfileId},
};
use futures::{StreamExt, future, stream::BoxStream};
use git::{GitService, GitServiceError};
use json_patch::Patch;
use sqlx::Error as SqlxError;
use thiserror::Error;
use tokio::{sync::RwLock, task::JoinHandle};
use utils::{
    log_msg::LogMsg,
    msg_store::MsgStore,
    text::{git_branch_id, short_uuid},
};
use uuid::Uuid;
use worktree_manager::WorktreeError;

use crate::services::{
    config::Config,
    execution_process,
    notification::NotificationService,
    queued_message::{QueueError, QueuedMessageService},
    webhook_notification::{
        TerminalExecutionStatus, TerminalExecutionWebhookEvent, WebhookNotificationService,
    },
};
pub type ContainerRef = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalProcessReconciliation {
    Absent,
    Terminated,
}

#[derive(Debug, Error)]
pub enum ContainerError {
    #[error("Waiting for team capacity.")]
    AdmissionWaiting,
    #[error("Waiting to reconcile a previously authorized agent process.")]
    ExternalProcessUnresolved,
    #[error(transparent)]
    GitServiceError(#[from] GitServiceError),
    #[error(transparent)]
    Sqlx(#[from] SqlxError),
    #[error(transparent)]
    ExecutorError(#[from] ExecutorError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    ExecutionProcess(#[from] ExecutionProcessError),
    #[error(transparent)]
    Queue(#[from] QueueError),
    #[error("Io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to kill process: {0}")]
    KillFailed(std::io::Error),
    #[error(transparent)]
    Other(#[from] AnyhowError), // Catches any unclassified errors
}

#[async_trait]
pub trait ContainerService {
    fn msg_stores(&self) -> &Arc<RwLock<HashMap<Uuid, Arc<MsgStore>>>>;

    fn db(&self) -> &DBService;

    fn git(&self) -> &GitService;

    fn config(&self) -> &Arc<RwLock<Config>>;

    fn notification_service(&self) -> &NotificationService;

    async fn touch(&self, workspace: &Workspace) -> Result<(), ContainerError>;

    fn workspace_to_current_dir(&self, workspace: &Workspace) -> PathBuf;

    async fn discover_executor_options(
        &self,
        executor_profile_id: ExecutorProfileId,
        session_id: Option<Uuid>,
        workspace_id: Option<Uuid>,
        repo_id: Option<Uuid>,
    ) -> Result<Option<BoxStream<'static, Patch>>, ContainerError> {
        let (workdir, repo_path) = if let Some(session_id) = session_id {
            let session = Session::find_by_id(&self.db().pool, session_id)
                .await?
                .ok_or(SqlxError::RowNotFound)?;

            if let Some(workspace_id) = workspace_id
                && session.workspace_id != workspace_id
            {
                return Err(ContainerError::Other(anyhow!(
                    "Session does not belong to workspace"
                )));
            }

            let workspace = Workspace::find_by_id(&self.db().pool, session.workspace_id)
                .await?
                .ok_or(SqlxError::RowNotFound)?;

            let container_ref = match workspace.container_ref.as_deref() {
                Some(container_ref) if !container_ref.is_empty() => container_ref,
                _ => &self.ensure_container_exists(&workspace).await?,
            };

            if container_ref.is_empty() {
                return Err(ContainerError::Other(anyhow!("Workspace path is empty")));
            }

            let workspace_path = PathBuf::from(container_ref);
            let workdir = match session.agent_working_dir.as_deref() {
                Some(dir) if !dir.is_empty() => Some(workspace_path.join(dir)),
                _ => Some(workspace_path),
            };

            let repos =
                WorkspaceRepo::find_repos_for_workspace(&self.db().pool, session.workspace_id)
                    .await
                    .unwrap_or_default();
            let repo_path = if repos.len() == 1 {
                Some(repos[0].path.clone())
            } else {
                None
            };

            (workdir, repo_path)
        } else if workspace_id.is_some() {
            return Err(ContainerError::Other(anyhow!(
                "session_id is required when workspace_id is provided"
            )));
        } else if let Some(repo_id) = repo_id {
            let repo = Repo::find_by_id(&self.db().pool, repo_id)
                .await
                .ok()
                .flatten()
                .map(|repo| repo.path);
            (None, repo)
        } else {
            (None, None)
        };

        #[cfg(feature = "qa-mode")]
        {
            let _ = executor_profile_id;
            let _ = workdir;
            let _ = repo_path;
            return Ok(None);
        }
        #[cfg(not(feature = "qa-mode"))]
        {
            let executor =
                ExecutorConfigs::get_cached().get_coding_agent_or_default(&executor_profile_id);

            // Spawn background task to refresh global cache for this executor
            let base_agent = executors::executors::BaseCodingAgent::from(&executor);
            executors::executors::utils::spawn_global_cache_refresh_for_agent(base_agent);

            let stream = executor
                .discover_options(workdir.as_deref(), repo_path.as_deref())
                .await?;
            Ok(Some(stream))
        }
    }

    async fn try_start_queued_messages(
        &self,
        queue: &QueuedMessageService,
    ) -> Result<(), ContainerError> {
        let _guard = queue.pump_lock().lock_owned().await;
        let max_concurrent = self.config().read().await.agent_queue_concurrency.max(1);
        let turn_capacity = self.config().read().await.agent_turn_capacity;

        loop {
            let items = queue.lease_next_batch(max_concurrent).await?;
            if items.is_empty() {
                return Ok(());
            }

            let mut started_any = false;
            for item in items {
                let admission = queue.acquire_turn(&item, turn_capacity).await?;
                let db::models::agent_turn_admission::AcquireAgentTurnAdmission::Acquired(token) =
                    admission
                else {
                    // Capacity and workspace serialization are durable admission
                    // concerns. Keep the item pending with product-safe status.
                    let reason = match admission {
                        db::models::agent_turn_admission::AcquireAgentTurnAdmission::CapacityUnavailable => "Waiting for team capacity.",
                        db::models::agent_turn_admission::AcquireAgentTurnAdmission::WorkspaceBusy => "Waiting for the workspace to become available.",
                        db::models::agent_turn_admission::AcquireAgentTurnAdmission::IdentityConflict => "This work request conflicts with an existing admission.",
                        db::models::agent_turn_admission::AcquireAgentTurnAdmission::Acquired(_) => unreachable!(),
                    };
                    queue.requeue_waiting(item.id, reason).await?;
                    continue;
                };

                match self
                    .start_queued_message(queue, &item, Some((token.token_id, token.fence)))
                    .await
                {
                    Ok(Some(_)) => started_any = true,
                    Ok(None) => {
                        queue.release_turn(token.token_id, token.fence).await?;
                    }
                    Err(error) => {
                        if matches!(
                            error,
                            ContainerError::AdmissionWaiting
                                | ContainerError::ExternalProcessUnresolved
                        ) {
                            queue
                                .requeue_waiting(
                                    item.id,
                                    if matches!(error, ContainerError::ExternalProcessUnresolved) {
                                        "Waiting to reconcile the prior agent start."
                                    } else {
                                        "Waiting for team capacity."
                                    },
                                )
                                .await?;
                            continue;
                        }
                        let message = error.to_string();
                        tracing::error!(
                            queue_item_id = %item.id,
                            ?error,
                            "failed to start queued follow-up"
                        );
                        queue.mark_failed(item.id, &message).await?;
                        queue.release_turn(token.token_id, token.fence).await?;
                    }
                }
            }

            if !started_any {
                return Ok(());
            }
        }
    }

    async fn start_queued_message(
        &self,
        queue: &QueuedMessageService,
        item: &AgentMessageQueueItem,
        admission: Option<(Uuid, i64)>,
    ) -> Result<Option<ExecutionProcess>, ContainerError> {
        let session = Session::find_by_id(&self.db().pool, item.session_id)
            .await?
            .ok_or_else(|| ContainerError::Other(anyhow!("Session not found")))?;
        let workspace = Workspace::find_by_id(&self.db().pool, session.workspace_id)
            .await?
            .ok_or_else(|| ContainerError::Other(anyhow!("Workspace not found")))?;
        self.ensure_container_exists(&workspace).await?;

        let executor_config =
            if let Some(config) = Self::queued_executor_config_override(&session, &item.data)? {
                config
            } else {
                self.executor_config_for_session(&session).await?
            };
        let latest_session_info =
            CodingAgentTurn::find_latest_session_info(&self.db().pool, session.id).await?;
        let repos = WorkspaceRepo::find_repos_for_workspace(&self.db().pool, workspace.id).await?;
        let cleanup_action = self.cleanup_actions_for_repos(&repos);
        let working_dir = session
            .agent_working_dir
            .as_ref()
            .filter(|dir| !dir.is_empty())
            .cloned();

        let action_type = Self::build_queued_action_type(
            &item.data,
            executor_config,
            latest_session_info,
            working_dir,
        );

        let cleanup_action = if matches!(
            &action_type,
            ExecutorActionType::CodingAgentSessionCommandRequest(_)
        ) {
            None
        } else {
            cleanup_action
        };
        let action = ExecutorAction::new_with_provenance(
            action_type,
            cleanup_action.map(Box::new),
            item.data.provenance.clone(),
        );
        // The logical queued turn owns a deterministic process identity. A
        // restarted pump therefore resumes the prepared intent instead of
        // inventing a second side effect identity.
        let process_id = item.id;
        if let Some((token_id, fence)) = admission
            && !queue
                .prepare_turn_process(token_id, fence, process_id)
                .await?
        {
            return Ok(None);
        }
        if !queue.mark_starting(item.id, process_id).await? {
            let current = AgentMessageQueueItem::find_by_id(&self.db().pool, item.id).await?;
            if current
                .as_ref()
                .is_some_and(|item| item.status == AgentMessageQueueStatus::Cancelled)
            {
                tracing::debug!(queue_item_id = %item.id, "queued message was cancelled before start");
            } else {
                tracing::warn!(
                    queue_item_id = %item.id,
                    current_status = ?current.map(|item| item.status),
                    "queued message lease was lost before start"
                );
            }
            return Ok(None);
        }

        match self
            .start_execution_with_id(
                &workspace,
                &session,
                &action,
                &ExecutionProcessRunReason::CodingAgent,
                process_id,
            )
            .await
        {
            Ok(process) => {
                queue.mark_running(item.id, process.id).await?;
                if matches!(
                    process.status,
                    ExecutionProcessStatus::Completed
                        | ExecutionProcessStatus::Failed
                        | ExecutionProcessStatus::Killed
                ) {
                    queue
                        .mark_terminal_for_execution_process(process.id, process.status.clone())
                        .await?;
                }
                Ok(Some(process))
            }
            Err(error) => {
                if matches!(error, ContainerError::AdmissionWaiting) {
                    return Err(error);
                }
                queue.mark_failed(item.id, &error.to_string()).await?;
                Err(error)
            }
        }
    }

    fn queued_executor_config_override(
        session: &Session,
        data: &db::models::agent_message_queue::QueuedFollowUpData,
    ) -> Result<Option<ExecutorConfig>, ContainerError> {
        let Some(config) = data.executor_config.clone() else {
            return Ok(None);
        };
        if session.executor.as_deref() != Some(config.executor.to_string().as_str()) {
            return Err(ContainerError::Other(anyhow!(
                "queued executor does not match the selected session"
            )));
        }
        Ok(Some(config))
    }

    fn build_queued_action_type(
        data: &db::models::agent_message_queue::QueuedFollowUpData,
        executor_config: ExecutorConfig,
        latest_session_info: Option<CodingAgentResumeInfo>,
        working_dir: Option<String>,
    ) -> ExecutorActionType {
        if let Some(command) = data.session_command.clone() {
            let latest_session_info = command
                .requires_provider_context(executor_config.executor)
                .then_some(latest_session_info)
                .flatten();
            return ExecutorActionType::CodingAgentSessionCommandRequest(
                CodingAgentSessionCommandRequest {
                    command,
                    prompt: data.message.clone(),
                    session_id: latest_session_info.map(|info| info.session_id),
                    executor_config,
                    working_dir,
                },
            );
        }
        if let Some(info) = latest_session_info {
            return ExecutorActionType::CodingAgentFollowUpRequest(CodingAgentFollowUpRequest {
                prompt: data.message.clone(),
                session_id: info.session_id,
                reset_to_message_id: None,
                executor_config,
                working_dir,
            });
        }
        ExecutorActionType::CodingAgentInitialRequest(CodingAgentInitialRequest {
            prompt: data.message.clone(),
            executor_config,
            working_dir,
        })
    }

    async fn executor_config_for_session(
        &self,
        session: &Session,
    ) -> Result<ExecutorConfig, ContainerError> {
        if let Some(profile) =
            ExecutionProcess::latest_executor_profile_for_session(&self.db().pool, session.id)
                .await?
        {
            return Ok(profile.into());
        }

        let executor = session.executor.as_deref().ok_or_else(|| {
            ContainerError::Other(anyhow!(
                "session has no configured executor; start the session once before queueing follow-ups"
            ))
        })?;
        let base_agent = BaseCodingAgent::from_str(executor).map_err(|_| {
            ContainerError::Other(anyhow!(
                "Unknown executor configured for session: {executor}"
            ))
        })?;
        Ok(ExecutorConfig::new(base_agent))
    }

    async fn store_db_stream_handle(&self, id: Uuid, handle: JoinHandle<()>);

    async fn take_db_stream_handle(&self, id: &Uuid) -> Option<JoinHandle<()>>;

    async fn create(&self, workspace: &Workspace) -> Result<ContainerRef, ContainerError>;

    async fn kill_all_running_processes(&self) -> Result<(), ContainerError>;

    async fn delete(&self, workspace: &Workspace) -> Result<(), ContainerError>;

    /// A context is finalized when
    /// - Always when the execution process has failed or been killed
    /// - Never when the run reason is DevServer
    /// - Never when a setup script has no next_action (parallel mode)
    /// - The next action is None (no follow-up actions)
    fn should_finalize(&self, ctx: &ExecutionContext) -> bool {
        // Never finalize DevServer processes
        if matches!(
            ctx.execution_process.run_reason,
            ExecutionProcessRunReason::DevServer
        ) {
            return false;
        }

        // Never finalize setup scripts without a next_action (parallel mode).
        // In sequential mode, setup scripts have next_action pointing to coding agent,
        // so they won't finalize anyway (handled by next_action.is_none() check below).
        let action = ctx.execution_process.executor_action().unwrap();
        if matches!(
            ctx.execution_process.run_reason,
            ExecutionProcessRunReason::SetupScript
        ) && action.next_action.is_none()
        {
            return false;
        }

        // Always finalize failed or killed executions, regardless of next action
        if matches!(
            ctx.execution_process.status,
            ExecutionProcessStatus::Failed | ExecutionProcessStatus::Killed
        ) {
            return true;
        }

        // Otherwise, finalize only if no next action
        action.next_action.is_none()
    }

    /// Finalize workspace execution by sending notifications
    async fn finalize_task(&self, ctx: &ExecutionContext) {
        // Skip notification if process was intentionally killed by user
        if matches!(ctx.execution_process.status, ExecutionProcessStatus::Killed) {
            return;
        }

        let workspace_name = ctx
            .workspace
            .name
            .as_deref()
            .unwrap_or(&ctx.workspace.branch);
        let title = format!("Workspace Complete: {}", workspace_name);
        let message = match ctx.execution_process.status {
            ExecutionProcessStatus::Completed => format!(
                "✅ '{}' completed successfully\nBranch: {:?}\nExecutor: {:?}",
                workspace_name, ctx.workspace.branch, ctx.session.executor
            ),
            ExecutionProcessStatus::Failed => format!(
                "❌ '{}' execution failed\nBranch: {:?}\nExecutor: {:?}",
                workspace_name, ctx.workspace.branch, ctx.session.executor
            ),
            _ => {
                tracing::warn!(
                    "Tried to notify workspace completion for {} but process is still running!",
                    ctx.workspace.id
                );
                return;
            }
        };
        self.notification_service()
            .notify(&title, &message, Some(ctx.workspace.id))
            .await;
    }

    /// Emits generic refs-only workflow webhooks for terminal coding-agent executions.
    ///
    /// These webhooks are best-effort wakeups for external orchestrators. VK keeps no
    /// durable outbound outbox here; downstream systems must use VK response-read APIs
    /// and their own polling/idempotence as the source of truth.
    async fn emit_terminal_execution_webhook(
        &self,
        ctx: &ExecutionContext,
        queue_item_id: Option<Uuid>,
    ) {
        if !matches!(
            ctx.execution_process.run_reason,
            ExecutionProcessRunReason::CodingAgent
        ) {
            return;
        }
        let status = match ctx.execution_process.status {
            ExecutionProcessStatus::Completed => TerminalExecutionStatus::Completed,
            ExecutionProcessStatus::Failed => TerminalExecutionStatus::Failed,
            ExecutionProcessStatus::Killed => TerminalExecutionStatus::Killed,
            ExecutionProcessStatus::Running => return,
        };
        WebhookNotificationService::new(self.config().clone())
            .emit_terminal_execution_event(TerminalExecutionWebhookEvent {
                workspace_id: ctx.workspace.id,
                session_id: ctx.session.id,
                execution_process_id: ctx.execution_process.id,
                status,
                completed_at: ctx.execution_process.completed_at,
                queue_item_id,
                exit_code: ctx.execution_process.exit_code,
            })
            .await;
    }

    /// Cleanup executions marked as running in the db, call at startup
    async fn cleanup_orphan_executions(&self) -> Result<(), ContainerError> {
        let running_processes = ExecutionProcess::find_running(&self.db().pool).await?;
        for process in running_processes {
            tracing::info!(
                "Found orphaned execution process {} for session {}",
                process.id,
                process.session_id
            );
            let queue_item_id =
                db::models::agent_turn_admission::AgentTurnAdmission::queue_item_for_process(
                    &self.db().pool,
                    process.id,
                )
                .await?;
            let external_state =
                db::models::execution_external_start::ExecutionExternalStart::state(
                    &self.db().pool,
                    process.id,
                )
                .await?;
            if external_state.as_deref() == Some("blocked") {
                if let Some(item_id) = queue_item_id {
                    AgentMessageQueueItem::requeue_waiting(
                        &self.db().pool,
                        item_id,
                        "Agent process status needs operator confirmation before work can continue.",
                    ).await?;
                }
                continue;
            }
            if matches!(external_state.as_deref(), Some("authorized" | "claiming")) {
                if let Some(record) =
                    db::models::execution_external_start::ExecutionExternalStart::record(
                        &self.db().pool,
                        process.id,
                    )
                    .await?
                {
                    if let Err(error) = self.reconcile_external_process(&record).await {
                        if matches!(error, ContainerError::ExternalProcessUnresolved) {
                            db::models::execution_external_start::ExecutionExternalStart::mark_blocked(
                                &self.db().pool, process.id,
                                "External process status requires operator confirmation.",
                            ).await?;
                            if let Some(item_id) = queue_item_id {
                                AgentMessageQueueItem::requeue_waiting(&self.db().pool, item_id,
                                    "Agent process status needs operator confirmation before work can continue.").await?;
                            }
                            continue;
                        }
                        return Err(error);
                    }
                }
                // No external spawn was durably confirmed. The deterministic
                // operation can safely return to waiting after restart.
                ExecutionProcess::rollback_unfinalized_start(&self.db().pool, process.id).await?;
                db::models::agent_turn_admission::AgentTurnAdmission::release_for_process(
                    &self.db().pool,
                    process.id,
                )
                .await?;
                if let Some(item_id) = queue_item_id {
                    AgentMessageQueueItem::requeue_waiting(
                        &self.db().pool,
                        item_id,
                        "Waiting to resume after restart.",
                    )
                    .await?;
                }
                continue;
            }
            if let Some(record) =
                db::models::execution_external_start::ExecutionExternalStart::record(
                    &self.db().pool,
                    process.id,
                )
                .await?
            {
                if let Err(error) = self.reconcile_external_process(&record).await {
                    if matches!(error, ContainerError::ExternalProcessUnresolved) {
                        db::models::execution_external_start::ExecutionExternalStart::mark_blocked(
                            &self.db().pool,
                            process.id,
                            "External process status requires operator confirmation.",
                        )
                        .await?;
                        if let Some(item_id) = queue_item_id {
                            AgentMessageQueueItem::requeue_waiting(&self.db().pool, item_id,
                                "Agent process status needs operator confirmation before work can continue.").await?;
                        }
                        continue;
                    }
                    return Err(error);
                }
            }
            // Only release capacity after the exact prior child is absent or
            // has been terminated by its durable OS identity.
            if let Err(e) = ExecutionProcess::update_completion(
                &self.db().pool,
                process.id,
                ExecutionProcessStatus::Failed,
                None, // No exit code for orphaned processes
            )
            .await
            {
                tracing::error!(
                    "Failed to update orphaned execution process {} status: {}",
                    process.id,
                    e
                );
                continue;
            }
            // Capture after-head commit OID per repository
            if let Ok(ctx) = ExecutionProcess::load_context(&self.db().pool, process.id).await
                && let Some(ref container_ref) = ctx.workspace.container_ref
            {
                let workspace_root = PathBuf::from(container_ref);
                for repo in &ctx.repos {
                    let repo_path = workspace_root.join(&repo.name);
                    if let Ok(head) = self.git().get_head_info(&repo_path)
                        && let Err(err) = ExecutionProcessRepoState::update_after_head_commit(
                            &self.db().pool,
                            process.id,
                            repo.id,
                            &head.oid,
                        )
                        .await
                    {
                        tracing::warn!(
                            "Failed to update after_head_commit for repo {} on process {}: {}",
                            repo.id,
                            process.id,
                            err
                        );
                    }
                }
            }
            // Process marked as failed
            tracing::info!("Marked orphaned execution process {} as failed", process.id);
            db::models::agent_turn_admission::AgentTurnAdmission::release_for_process(
                &self.db().pool,
                process.id,
            )
            .await?;
            if let Some(item_id) = queue_item_id {
                AgentMessageQueueItem::mark_failed(
                    &self.db().pool,
                    item_id,
                    "The agent start was confirmed, but monitoring was interrupted. Review before retrying.",
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Backfill before_head_commit for legacy execution processes.
    /// Rules:
    /// - If a process has after_head_commit and missing before_head_commit,
    ///   then set before_head_commit to the previous process's after_head_commit.
    /// - If there is no previous process, set before_head_commit to the base branch commit.
    async fn backfill_before_head_commits(&self) -> Result<(), ContainerError> {
        let pool = &self.db().pool;
        let rows = ExecutionProcess::list_missing_before_context(pool).await?;
        for row in rows {
            // Skip if no after commit at all (shouldn't happen due to WHERE)
            // Prefer previous process after-commit if present
            let mut before = row.prev_after_head_commit.clone();

            // Fallback to base branch commit OID
            if before.is_none() {
                let repo_path = std::path::Path::new(row.repo_path.as_deref().unwrap_or_default());
                match self
                    .git()
                    .get_branch_oid(repo_path, row.target_branch.as_str())
                {
                    Ok(oid) => before = Some(oid),
                    Err(e) => {
                        tracing::warn!(
                            "Backfill: Failed to resolve base branch OID for workspace {} (branch {}): {}",
                            row.workspace_id,
                            row.target_branch,
                            e
                        );
                    }
                }
            }

            if let Some(before_oid) = before
                && let Err(e) = ExecutionProcessRepoState::update_before_head_commit(
                    pool,
                    row.id,
                    row.repo_id,
                    &before_oid,
                )
                .await
            {
                tracing::warn!(
                    "Backfill: Failed to update before_head_commit for process {}: {}",
                    row.id,
                    e
                );
            }
        }

        Ok(())
    }

    /// Backfill repo names that were migrated with a sentinel placeholder.
    /// Also backfills dev_script_working_dir and agent_working_dir for single-repo projects.
    async fn backfill_repo_names(&self) -> Result<(), ContainerError> {
        let pool = &self.db().pool;
        let repos = Repo::list_needing_name_fix(pool).await?;

        if repos.is_empty() {
            return Ok(());
        }

        tracing::info!("Backfilling {} repo names", repos.len());

        for repo in repos {
            let name = repo
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&repo.id.to_string())
                .to_string();

            Repo::update_name(pool, repo.id, &name, &name).await?;
        }

        Ok(())
    }

    fn cleanup_actions_for_repos(&self, repos: &[Repo]) -> Option<ExecutorAction> {
        let repos_with_cleanup: Vec<_> = repos
            .iter()
            .filter(|r| r.cleanup_script.is_some())
            .collect();

        if repos_with_cleanup.is_empty() {
            return None;
        }

        let mut iter = repos_with_cleanup.iter();
        let first = iter.next()?;
        let mut root_action = ExecutorAction::new(
            ExecutorActionType::ScriptRequest(ScriptRequest {
                script: first.cleanup_script.clone().unwrap(),
                language: ScriptRequestLanguage::Bash,
                context: ScriptContext::CleanupScript,
                working_dir: Some(first.name.clone()),
                env: Default::default(),
            }),
            None,
        );

        for repo in iter {
            root_action = root_action.append_action(ExecutorAction::new(
                ExecutorActionType::ScriptRequest(ScriptRequest {
                    script: repo.cleanup_script.clone().unwrap(),
                    language: ScriptRequestLanguage::Bash,
                    context: ScriptContext::CleanupScript,
                    working_dir: Some(repo.name.clone()),
                    env: Default::default(),
                }),
                None,
            ));
        }

        Some(root_action)
    }

    fn archive_actions_for_repos(&self, repos: &[Repo]) -> Option<ExecutorAction> {
        let repos_with_archive: Vec<_> = repos
            .iter()
            .filter(|r| r.archive_script.is_some())
            .collect();

        if repos_with_archive.is_empty() {
            return None;
        }

        let mut iter = repos_with_archive.iter();
        let first = iter.next()?;
        let mut root_action = ExecutorAction::new(
            ExecutorActionType::ScriptRequest(ScriptRequest {
                script: first.archive_script.clone().unwrap(),
                language: ScriptRequestLanguage::Bash,
                context: ScriptContext::ArchiveScript,
                working_dir: Some(first.name.clone()),
                env: Default::default(),
            }),
            None,
        );

        for repo in iter {
            root_action = root_action.append_action(ExecutorAction::new(
                ExecutorActionType::ScriptRequest(ScriptRequest {
                    script: repo.archive_script.clone().unwrap(),
                    language: ScriptRequestLanguage::Bash,
                    context: ScriptContext::ArchiveScript,
                    working_dir: Some(repo.name.clone()),
                    env: Default::default(),
                }),
                None,
            ));
        }

        Some(root_action)
    }

    /// Attempts to run the archive script for a workspace if configured.
    /// Silently returns Ok if no archive script is configured or if conditions aren't met.
    async fn try_run_archive_script(&self, workspace_id: Uuid) -> Result<(), ContainerError> {
        let pool = &self.db().pool;
        let workspace = Workspace::find_by_id(pool, workspace_id)
            .await?
            .ok_or(ContainerError::Other(anyhow!("Workspace not found")))?;
        if ExecutionProcess::has_running_non_dev_server_processes_for_workspace(pool, workspace.id)
            .await
            .unwrap_or(true)
        {
            return Ok(());
        }
        if self.ensure_container_exists(&workspace).await.is_err() {
            return Ok(());
        }
        let repos = WorkspaceRepo::find_repos_for_workspace(pool, workspace.id).await?;
        let Some(action) = self.archive_actions_for_repos(&repos) else {
            return Ok(());
        };
        let session = match Session::find_latest_by_workspace_id(pool, workspace.id).await? {
            Some(s) => s,
            None => {
                Session::create(
                    pool,
                    &CreateSession {
                        executor: None,
                        name: None,
                    },
                    Uuid::new_v4(),
                    workspace.id,
                )
                .await?
            }
        };
        self.start_execution(
            &workspace,
            &session,
            &action,
            &ExecutionProcessRunReason::ArchiveScript,
        )
        .await?;

        Ok(())
    }

    /// Archive a workspace: set archived flag, stop running dev servers, and run archive script.
    async fn archive_workspace(&self, workspace_id: Uuid) -> Result<(), ContainerError> {
        let pool = &self.db().pool;

        Workspace::set_archived(pool, workspace_id, true).await?;

        // Stop running dev servers
        if let Ok(dev_servers) =
            ExecutionProcess::find_running_dev_servers_by_workspace(pool, workspace_id).await
        {
            for dev_server in dev_servers {
                if let Err(e) = self
                    .stop_execution(&dev_server, ExecutionProcessStatus::Killed)
                    .await
                {
                    tracing::error!(
                        "Failed to stop dev server {} for workspace {}: {}",
                        dev_server.id,
                        workspace_id,
                        e
                    );
                }
            }
        }

        // Run archive script (silently skips if not configured)
        if let Err(e) = self.try_run_archive_script(workspace_id).await {
            tracing::error!(
                "Failed to run archive script for workspace {}: {}",
                workspace_id,
                e
            );
        }

        Ok(())
    }

    fn setup_actions_for_repos(&self, repos: &[Repo]) -> Option<ExecutorAction> {
        let repos_with_setup: Vec<_> = repos.iter().filter(|r| r.setup_script.is_some()).collect();

        if repos_with_setup.is_empty() {
            return None;
        }

        let mut iter = repos_with_setup.iter();
        let first = iter.next()?;
        let mut root_action = ExecutorAction::new(
            ExecutorActionType::ScriptRequest(ScriptRequest {
                script: first.setup_script.clone().unwrap(),
                language: ScriptRequestLanguage::Bash,
                context: ScriptContext::SetupScript,
                working_dir: Some(first.name.clone()),
                env: Default::default(),
            }),
            None,
        );

        for repo in iter {
            root_action = root_action.append_action(ExecutorAction::new(
                ExecutorActionType::ScriptRequest(ScriptRequest {
                    script: repo.setup_script.clone().unwrap(),
                    language: ScriptRequestLanguage::Bash,
                    context: ScriptContext::SetupScript,
                    working_dir: Some(repo.name.clone()),
                    env: Default::default(),
                }),
                None,
            ));
        }

        Some(root_action)
    }

    fn setup_action_for_repo(repo: &Repo) -> Option<ExecutorAction> {
        repo.setup_script.as_ref().map(|script| {
            ExecutorAction::new(
                ExecutorActionType::ScriptRequest(ScriptRequest {
                    script: script.clone(),
                    language: ScriptRequestLanguage::Bash,
                    context: ScriptContext::SetupScript,
                    working_dir: Some(repo.name.clone()),
                    env: Default::default(),
                }),
                None,
            )
        })
    }

    fn build_sequential_setup_chain(
        repos: &[&Repo],
        next_action: ExecutorAction,
    ) -> ExecutorAction {
        let mut chained = next_action;
        for repo in repos.iter().rev() {
            if let Some(script) = &repo.setup_script {
                chained = ExecutorAction::new(
                    ExecutorActionType::ScriptRequest(ScriptRequest {
                        script: script.clone(),
                        language: ScriptRequestLanguage::Bash,
                        context: ScriptContext::SetupScript,
                        working_dir: Some(repo.name.clone()),
                        env: Default::default(),
                    }),
                    Some(Box::new(chained)),
                );
            }
        }
        chained
    }

    async fn stop_running_processes_for_session(
        &self,
        session_id: Uuid,
        include_dev_server: bool,
    ) -> Result<Vec<Uuid>, ContainerError> {
        let processes =
            ExecutionProcess::find_by_session_id(&self.db().pool, session_id, false).await?;
        let mut stopped_processes = Vec::new();

        for process in processes {
            // Skip dev server processes unless explicitly included.
            if !include_dev_server && process.run_reason == ExecutionProcessRunReason::DevServer {
                continue;
            }
            if process.status == ExecutionProcessStatus::Running {
                let process_id = process.id;
                self.stop_execution(&process, ExecutionProcessStatus::Killed)
                    .await
                    .map_err(|e| {
                        tracing::debug!(
                            "Failed to stop execution process {} for session {}: {}",
                            process_id,
                            session_id,
                            e
                        );
                        e
                    })?;
                stopped_processes.push(process_id);
            }
        }

        Ok(stopped_processes)
    }

    async fn running_non_dev_server_processes_for_other_sessions(
        &self,
        workspace_id: Uuid,
        target_session_id: Uuid,
    ) -> Result<Vec<ExecutionProcess>, ContainerError> {
        let sessions = Session::find_by_workspace_id(&self.db().pool, workspace_id).await?;
        let mut running_processes = Vec::new();

        for session in sessions {
            if session.id == target_session_id {
                continue;
            }

            let processes =
                ExecutionProcess::find_by_session_id(&self.db().pool, session.id, false).await?;
            running_processes.extend(processes.into_iter().filter(|process| {
                process.status == ExecutionProcessStatus::Running
                    && process.run_reason != ExecutionProcessRunReason::DevServer
            }));
        }

        Ok(running_processes)
    }

    async fn stop_running_processes_for_other_sessions(
        &self,
        workspace_id: Uuid,
        target_session_id: Uuid,
    ) -> Result<Vec<Uuid>, ContainerError> {
        let sibling_processes = self
            .running_non_dev_server_processes_for_other_sessions(workspace_id, target_session_id)
            .await?;
        let mut stopped_processes = Vec::new();

        for process in sibling_processes {
            let process_id = process.id;
            self.stop_execution(&process, ExecutionProcessStatus::Killed)
                .await
                .map_err(|e| {
                    tracing::debug!(
                        "Failed to stop sibling execution process {} for workspace {}: {}",
                        process_id,
                        workspace_id,
                        e
                    );
                    e
                })?;
            stopped_processes.push(process_id);
        }

        Ok(stopped_processes)
    }

    /// Reset a session to a specific process: stop running processes, restore worktrees, drop later processes.
    async fn reset_session_to_process(
        &self,
        session_id: Uuid,
        target_process_id: Uuid,
        perform_git_reset: bool,
        force_when_dirty: bool,
        stop_other_sessions_for_git_reset: bool,
    ) -> Result<(), ContainerError> {
        let pool = &self.db().pool;

        let process = ExecutionProcess::find_by_id(pool, target_process_id)
            .await?
            .ok_or_else(|| ContainerError::Other(anyhow!("Process not found")))?;
        if process.session_id != session_id {
            return Err(ContainerError::Other(anyhow!(
                "Process does not belong to this session"
            )));
        }

        let session = Session::find_by_id(pool, session_id)
            .await?
            .ok_or_else(|| ContainerError::Other(anyhow!("Session not found")))?;
        let workspace = Workspace::find_by_id(pool, session.workspace_id)
            .await?
            .ok_or_else(|| ContainerError::Other(anyhow!("Workspace not found")))?;

        if perform_git_reset {
            let sibling_running_processes = self
                .running_non_dev_server_processes_for_other_sessions(workspace.id, session_id)
                .await?;

            if !sibling_running_processes.is_empty() {
                if stop_other_sessions_for_git_reset {
                    self.stop_running_processes_for_other_sessions(workspace.id, session_id)
                        .await?;
                } else {
                    return Err(ContainerError::Other(anyhow!(
                        "Cannot reset worktree while another session is running. Retry without worktree reset, or choose the option to stop other running sessions before resetting."
                    )));
                }
            }
        }

        self.stop_running_processes_for_session(session_id, false)
            .await?;

        let repos = WorkspaceRepo::find_repos_for_workspace(pool, workspace.id).await?;
        let repo_states =
            ExecutionProcessRepoState::find_by_execution_process_id(pool, target_process_id)
                .await?;

        let container_ref = self.ensure_container_exists(&workspace).await?;
        let workspace_dir = std::path::PathBuf::from(container_ref);
        let is_dirty = self
            .is_container_clean(&workspace)
            .await
            .map(|is_clean| !is_clean)
            .unwrap_or(false);

        for repo in &repos {
            let repo_state = repo_states.iter().find(|s| s.repo_id == repo.id);
            let target_oid = match repo_state.and_then(|s| s.before_head_commit.clone()) {
                Some(oid) => Some(oid),
                None => {
                    ExecutionProcess::find_prev_after_head_commit(
                        pool,
                        session_id,
                        target_process_id,
                        repo.id,
                    )
                    .await?
                }
            };

            let worktree_path = workspace_dir.join(&repo.name);
            if let Some(oid) = target_oid {
                self.git().reconcile_worktree_to_commit(
                    &worktree_path,
                    &oid,
                    git::WorktreeResetOptions::new(
                        perform_git_reset,
                        force_when_dirty,
                        is_dirty,
                        perform_git_reset,
                    ),
                );
            }
        }

        ExecutionProcess::drop_at_and_after(pool, session_id, target_process_id).await?;
        Session::recompute_context_reset_boundary(pool, session_id).await?;

        Ok(())
    }

    async fn try_stop(&self, workspace: &Workspace, include_dev_server: bool) {
        // stop execution processes for this workspace's sessions
        let sessions = match Session::find_by_workspace_id(&self.db().pool, workspace.id).await {
            Ok(s) => s,
            Err(_) => return,
        };

        for session in sessions {
            let _ = self
                .stop_running_processes_for_session(session.id, include_dev_server)
                .await;
        }
    }

    async fn ensure_container_exists(
        &self,
        workspace: &Workspace,
    ) -> Result<ContainerRef, ContainerError>;

    async fn is_container_clean(&self, workspace: &Workspace) -> Result<bool, ContainerError>;

    async fn start_execution_inner(
        &self,
        workspace: &Workspace,
        execution_process: &ExecutionProcess,
        executor_action: &ExecutorAction,
        external_start_fence: Option<i64>,
    ) -> Result<(), ContainerError>;

    async fn reconcile_external_process(
        &self,
        record: &db::models::execution_external_start::ExecutionExternalStartRecord,
    ) -> Result<ExternalProcessReconciliation, ContainerError>;

    async fn confirm_external_process_stopped(
        &self,
        workspace_id: Uuid,
        process_id: Uuid,
        actor: &str,
        recovery_token: &str,
        recovery_generation: i64,
    ) -> Result<db::models::execution_external_start::ExternalStartRecoveryResult, ContainerError>
    {
        use db::models::execution_external_start::ExternalStartRecoveryResult;
        let Some(record) = db::models::execution_external_start::ExecutionExternalStart::record(
            &self.db().pool,
            process_id,
        )
        .await?
        else {
            return Ok(ExternalStartRecoveryResult::NotBlocked);
        };
        if record.recovery_token.as_deref() != Some(recovery_token)
            || record.recovery_generation != recovery_generation
        {
            return Ok(ExternalStartRecoveryResult::StaleRecovery);
        }
        let process_workspace: Option<Uuid> = sqlx::query_scalar("SELECT s.workspace_id FROM execution_processes ep JOIN sessions s ON s.id=ep.session_id WHERE ep.id=?1")
            .bind(process_id).fetch_optional(&self.db().pool).await?;
        if process_workspace != Some(workspace_id) {
            return Ok(ExternalStartRecoveryResult::WrongWorkspace);
        }
        if record.state == "blocked" {
            // Never turn an operator assertion into released capacity. The platform
            // reconciler must prove the exact prior process group absent or terminate it.
            self.reconcile_external_process(&record).await?;
        }
        let result = db::models::execution_external_start::ExecutionExternalStart::confirm_stopped(
            &self.db().pool,
            process_id,
            workspace_id,
            actor,
            recovery_token,
            recovery_generation,
        )
        .await?;
        if matches!(
            result,
            ExternalStartRecoveryResult::Reconciled
                | ExternalStartRecoveryResult::AlreadyReconciled
        ) {
            ExecutionProcess::update_completion(
                &self.db().pool,
                process_id,
                ExecutionProcessStatus::Failed,
                None,
            )
            .await?;
            db::models::agent_turn_admission::AgentTurnAdmission::release_for_process(
                &self.db().pool,
                process_id,
            )
            .await?;
        }
        Ok(result)
    }

    async fn stop_execution(
        &self,
        execution_process: &ExecutionProcess,
        status: ExecutionProcessStatus,
    ) -> Result<(), ContainerError>;

    async fn try_commit_changes(&self, ctx: &ExecutionContext) -> Result<bool, ContainerError>;

    async fn copy_project_files(
        &self,
        source_dir: &Path,
        target_dir: &Path,
        copy_files: &str,
    ) -> Result<(), ContainerError>;

    /// Stream diff updates as LogMsg for WebSocket endpoints.
    async fn stream_diff(
        &self,
        workspace: &Workspace,
        stats_only: bool,
    ) -> Result<futures::stream::BoxStream<'static, Result<LogMsg, std::io::Error>>, ContainerError>;

    /// Fetch the MsgStore for a given execution ID, panicking if missing.
    async fn get_msg_store_by_id(&self, uuid: &Uuid) -> Option<Arc<MsgStore>> {
        let map = self.msg_stores().read().await;
        map.get(uuid).cloned()
    }

    async fn git_branch_prefix(&self) -> String;

    async fn git_branch_from_workspace(&self, workspace_id: &Uuid, task_title: &str) -> String {
        let task_title_id = git_branch_id(task_title);
        let prefix = self.git_branch_prefix().await;

        if prefix.is_empty() {
            format!("{}-{}", short_uuid(workspace_id), task_title_id)
        } else {
            format!("{}/{}-{}", prefix, short_uuid(workspace_id), task_title_id)
        }
    }

    async fn stream_raw_logs(
        &self,
        id: &Uuid,
    ) -> Option<futures::stream::BoxStream<'static, Result<LogMsg, std::io::Error>>> {
        if let Some(store) = self.get_msg_store_by_id(id).await {
            // First try in-memory store
            return Some(
                store
                    .history_plus_stream()
                    .filter(|msg| {
                        future::ready(matches!(
                            msg,
                            Ok(LogMsg::Stdout(..) | LogMsg::Stderr(..) | LogMsg::Finished)
                        ))
                    })
                    .boxed(),
            );
        } else {
            let messages = execution_process::load_raw_log_messages(&self.db().pool, *id).await?;

            let stream = futures::stream::iter(
                messages
                    .into_iter()
                    .filter(|m| matches!(m, LogMsg::Stdout(_) | LogMsg::Stderr(_)))
                    .chain(std::iter::once(LogMsg::Finished))
                    .map(Ok::<_, std::io::Error>),
            )
            .boxed();

            Some(stream)
        }
    }

    async fn stream_normalized_logs(
        &self,
        id: &Uuid,
    ) -> Option<futures::stream::BoxStream<'static, Result<LogMsg, std::io::Error>>> {
        if let Some(store) = self.get_msg_store_by_id(id).await {
            Some(
                store
                    .history_plus_stream()
                    .take_while(|msg| future::ready(!matches!(msg, Ok(LogMsg::Finished))))
                    .filter(|msg| future::ready(matches!(msg, Ok(LogMsg::JsonPatch(..)))))
                    .chain(futures::stream::once(async {
                        Ok::<_, std::io::Error>(LogMsg::Finished)
                    }))
                    .boxed(),
            )
        } else {
            let raw_messages =
                execution_process::load_raw_log_messages(&self.db().pool, *id).await?;

            // Create temporary store and populate
            // Include JsonPatch messages (already normalized) and Stdout/Stderr (need normalization)
            let temp_store = Arc::new(MsgStore::new());
            for msg in raw_messages {
                if matches!(
                    msg,
                    LogMsg::Stdout(_) | LogMsg::Stderr(_) | LogMsg::JsonPatch(_)
                ) {
                    temp_store.push(msg);
                }
            }
            temp_store.push_finished();

            let process = match ExecutionProcess::find_by_id(&self.db().pool, *id).await {
                Ok(Some(process)) => process,
                Ok(None) => {
                    tracing::error!("No execution process found for ID: {}", id);
                    return None;
                }
                Err(e) => {
                    tracing::error!("Failed to fetch execution process {}: {}", id, e);
                    return None;
                }
            };

            // Get the workspace to determine correct directory
            let (workspace, _session) =
                match process.parent_workspace_and_session(&self.db().pool).await {
                    Ok(Some((workspace, session))) => (workspace, session),
                    Ok(None) => {
                        tracing::error!(
                            "No workspace/session found for session ID: {}",
                            process.session_id
                        );
                        return None;
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to fetch workspace for session {}: {}",
                            process.session_id,
                            e
                        );
                        return None;
                    }
                };

            if let Err(err) = self.ensure_container_exists(&workspace).await {
                tracing::warn!(
                    "Failed to recreate worktree before log normalization for workspace {}: {}",
                    workspace.id,
                    err
                );
            }

            let current_dir = self.workspace_to_current_dir(&workspace);

            let executor_action = if let Ok(executor_action) = process.executor_action() {
                executor_action
            } else {
                tracing::error!(
                    "Failed to parse executor action: {:?}",
                    process.executor_action()
                );
                return None;
            };

            // Spawn normalizer on populated store and collect JoinHandles
            let handles = match executor_action.typ() {
                ExecutorActionType::CodingAgentInitialRequest(request) => {
                    if executor_action.uses_qa_mock_log_normalizer() {
                        let executor = QaMockExecutor;
                        executor.normalize_mock_logs(
                            temp_store.clone(),
                            &request.effective_dir(&current_dir),
                        )
                    } else {
                        let executor = ExecutorConfigs::get_cached()
                            .get_coding_agent_or_default(&request.executor_config.profile_id());
                        executor.normalize_logs(
                            temp_store.clone(),
                            &request.effective_dir(&current_dir),
                        )
                    }
                }
                ExecutorActionType::CodingAgentFollowUpRequest(request) => {
                    if executor_action.uses_qa_mock_log_normalizer() {
                        let executor = QaMockExecutor;
                        executor.normalize_mock_logs(
                            temp_store.clone(),
                            &request.effective_dir(&current_dir),
                        )
                    } else {
                        let executor = ExecutorConfigs::get_cached()
                            .get_coding_agent_or_default(&request.executor_config.profile_id());
                        executor.normalize_logs(
                            temp_store.clone(),
                            &request.effective_dir(&current_dir),
                        )
                    }
                }
                ExecutorActionType::CodingAgentSessionCommandRequest(request) => {
                    if request.static_message().is_some() {
                        executors::actions::session_command::normalize_static_session_command_logs(
                            temp_store.clone(),
                        )
                    } else if executor_action.uses_qa_mock_log_normalizer() {
                        let executor = QaMockExecutor;
                        executor.normalize_mock_logs(
                            temp_store.clone(),
                            &request.effective_dir(&current_dir),
                        )
                    } else {
                        let executor = ExecutorConfigs::get_cached()
                            .get_coding_agent_or_default(&request.executor_config.profile_id());
                        executor.normalize_logs(
                            temp_store.clone(),
                            &request.effective_dir(&current_dir),
                        )
                    }
                }
                ExecutorActionType::ReviewRequest(request) => {
                    if executor_action.uses_qa_mock_log_normalizer() {
                        let executor = QaMockExecutor;
                        executor.normalize_mock_logs(temp_store.clone(), &current_dir)
                    } else {
                        let executor = ExecutorConfigs::get_cached()
                            .get_coding_agent_or_default(&request.executor_config.profile_id());
                        executor.normalize_logs(temp_store.clone(), &current_dir)
                    }
                }
                _ => {
                    tracing::debug!(
                        "Executor action doesn't support log normalization: {:?}",
                        process.executor_action()
                    );
                    return None;
                }
            };

            // Await all normalizer tasks, then push Ready so the dedup
            // stream knows when to flush its buffer and terminate.
            {
                let store = temp_store.clone();
                tokio::spawn(async move {
                    for handle in handles {
                        let _ = handle.await;
                    }
                    store.push(LogMsg::Ready);
                });
            }

            // Stream normalized patches, deduplicating consecutive patches
            // that target the same path (only the final state matters for
            // historical replay). The Ready sentinel flushes the buffer.
            enum PatchOrDone {
                Patch(Patch),
                Done,
            }

            let stream = temp_store
                .history_plus_stream()
                .filter_map(|msg| async move {
                    match msg {
                        Ok(LogMsg::JsonPatch(patch)) => Some(PatchOrDone::Patch(patch)),
                        Ok(LogMsg::Ready) => Some(PatchOrDone::Done),
                        _ => None,
                    }
                });

            let deduped = futures::stream::unfold(
                (stream.boxed(), None::<Patch>, HashSet::<String>::new()),
                |(mut stream, buffered, mut sent_paths)| async move {
                    match stream.next().await {
                        Some(PatchOrDone::Patch(patch)) => {
                            let Some(prev) = buffered else {
                                // First patch — just buffer it
                                return Some((None, (stream, Some(patch), sent_paths)));
                            };
                            if patch_entry_path(&patch) == patch_entry_path(&prev)
                                && is_add_or_replace(&patch)
                                && is_add_or_replace(&prev)
                            {
                                // Same path, both add/replace — replace buffer
                                Some((None, (stream, Some(patch), sent_paths)))
                            } else {
                                // Different — emit prev, buffer new
                                let prev = fix_patch_ops(prev, &mut sent_paths);
                                Some((Some(prev), (stream, Some(patch), sent_paths)))
                            }
                        }
                        Some(PatchOrDone::Done) | None => {
                            // Sentinel or stream end: flush buffer and terminate
                            if let Some(prev) = buffered {
                                let prev = fix_patch_ops(prev, &mut sent_paths);
                                return Some((Some(prev), (stream, None, sent_paths)));
                            }
                            None
                        }
                    }
                },
            )
            .filter_map(|opt| async move { opt })
            .map(|p| Ok::<_, std::io::Error>(LogMsg::JsonPatch(p)))
            .chain(futures::stream::once(async {
                Ok::<_, std::io::Error>(LogMsg::Finished)
            }));

            Some(deduped.boxed())
        }
    }

    async fn start_workspace(
        &self,
        workspace: &Workspace,
        executor_config: ExecutorConfig,
        prompt: String,
    ) -> Result<ExecutionProcess, ContainerError> {
        // Create container
        self.create(workspace).await?;

        let repos = WorkspaceRepo::find_repos_for_workspace(&self.db().pool, workspace.id).await?;

        let workspace = Workspace::find_by_id(&self.db().pool, workspace.id)
            .await?
            .ok_or(SqlxError::RowNotFound)?;

        // Create a session for this workspace
        let session = Session::create(
            &self.db().pool,
            &CreateSession {
                executor: Some(executor_config.executor.to_string()),
                name: None,
            },
            Uuid::new_v4(),
            workspace.id,
        )
        .await?;

        let repos_with_setup: Vec<_> = repos.iter().filter(|r| r.setup_script.is_some()).collect();

        let all_parallel = repos_with_setup.iter().all(|r| r.parallel_setup_script);

        let cleanup_action = self.cleanup_actions_for_repos(&repos);

        let working_dir = session
            .agent_working_dir
            .as_ref()
            .filter(|dir| !dir.is_empty())
            .cloned();

        let coding_action = ExecutorAction::new(
            ExecutorActionType::CodingAgentInitialRequest(CodingAgentInitialRequest {
                prompt,
                executor_config: executor_config.clone(),
                working_dir,
            }),
            cleanup_action.map(Box::new),
        );

        let execution_process = if all_parallel {
            // All parallel: start each setup independently, then start coding agent
            for repo in &repos_with_setup {
                if let Some(action) = Self::setup_action_for_repo(repo)
                    && let Err(e) = self
                        .start_execution(
                            &workspace,
                            &session,
                            &action,
                            &ExecutionProcessRunReason::SetupScript,
                        )
                        .await
                {
                    tracing::warn!(?e, "Failed to start setup script in parallel mode");
                }
            }
            self.start_execution(
                &workspace,
                &session,
                &coding_action,
                &ExecutionProcessRunReason::CodingAgent,
            )
            .await?
        } else {
            // Any sequential: chain ALL setups → coding agent via next_action
            let main_action = Self::build_sequential_setup_chain(&repos_with_setup, coding_action);
            self.start_execution(
                &workspace,
                &session,
                &main_action,
                &ExecutionProcessRunReason::SetupScript,
            )
            .await?
        };

        Ok(execution_process)
    }

    async fn start_execution(
        &self,
        workspace: &Workspace,
        session: &Session,
        executor_action: &ExecutorAction,
        run_reason: &ExecutionProcessRunReason,
    ) -> Result<ExecutionProcess, ContainerError> {
        self.start_execution_with_id(
            workspace,
            session,
            executor_action,
            run_reason,
            Uuid::new_v4(),
        )
        .await
    }

    async fn start_execution_with_id(
        &self,
        workspace: &Workspace,
        session: &Session,
        executor_action: &ExecutorAction,
        run_reason: &ExecutionProcessRunReason,
        process_id: Uuid,
    ) -> Result<ExecutionProcess, ContainerError> {
        // A queued turn uses its queue item as the durable process identity. A
        // concurrent/restarted pump can reach this boundary after the first
        // owner inserted the process but before it updated the queue row. Reuse
        // only the exact live process; attempting a second INSERT would turn a
        // successful external start into a failed queue item.
        if *run_reason == ExecutionProcessRunReason::CodingAgent
            && let Some(existing) =
                ExecutionProcess::find_by_id(&self.db().pool, process_id).await?
        {
            if existing.session_id != session.id || existing.run_reason != *run_reason {
                return Err(ContainerError::Other(anyhow!(
                    "The queued turn process identity conflicts with an existing process"
                )));
            }
            if matches!(
                existing.status,
                ExecutionProcessStatus::Failed | ExecutionProcessStatus::Killed
            ) {
                return Err(ContainerError::Other(anyhow!(
                    "The queued turn process already ended unsuccessfully"
                )));
            }
            return Ok(existing);
        }
        // Create new execution process record
        // Capture current HEAD per repository as the "before" commit for this execution
        let repositories =
            WorkspaceRepo::find_repos_for_workspace(&self.db().pool, workspace.id).await?;
        if repositories.is_empty() {
            return Err(ContainerError::Other(anyhow!(
                "Workspace has no repositories configured"
            )));
        }

        let workspace_root = workspace
            .container_ref
            .as_ref()
            .map(std::path::PathBuf::from)
            .ok_or_else(|| ContainerError::Other(anyhow!("Container ref not found")))?;

        let mut repo_states = Vec::with_capacity(repositories.len());
        for repo in &repositories {
            let repo_path = workspace_root.join(&repo.name);
            let before_head_commit = self.git().get_head_info(&repo_path).ok().map(|h| h.oid);
            repo_states.push(CreateExecutionProcessRepoState {
                repo_id: repo.id,
                before_head_commit,
                after_head_commit: None,
                merge_commit: None,
            });
        }
        let executor_action_for_process = executor_action
            .clone()
            .with_current_runtime_log_normalizer();
        let create_execution_process = CreateExecutionProcess {
            session_id: session.id,
            executor_action: executor_action_for_process.clone(),
            run_reason: run_reason.clone(),
        };

        // All coding-agent starts, including direct/manual routes, cross this
        // database-backed final admission boundary. Dev servers and scripts
        // intentionally use their existing independent capacity policies.
        let admission = if *run_reason == ExecutionProcessRunReason::CodingAgent {
            let capacity = self.config().read().await.agent_turn_capacity;
            let token = match db::models::agent_turn_admission::AgentTurnAdmission::find_by_process(
                &self.db().pool,
                process_id,
            )
            .await?
            {
                Some(token) => token,
                None => match db::models::agent_turn_admission::AgentTurnAdmission::acquire_direct(
                    &self.db().pool,
                    process_id,
                    workspace.id,
                    capacity,
                    chrono::Duration::seconds(120),
                )
                .await?
                {
                    db::models::agent_turn_admission::AcquireAgentTurnAdmission::Acquired(
                        token,
                    ) => token,
                    _ => return Err(ContainerError::Other(anyhow!("Waiting for team capacity."))),
                },
            };
            Some((token, capacity))
        } else {
            None
        };

        let execution_process = match ExecutionProcess::create(
            &self.db().pool,
            &create_execution_process,
            process_id,
            &repo_states,
            admission
                .as_ref()
                .map(|(token, capacity)| (token.token_id, token.fence, *capacity)),
        )
        .await
        {
            Ok(process) => process,
            Err(error) => {
                // Concurrent delayed queue pumps can reach this shared boundary
                // with the same deterministic process identity. The existing
                // row is the durable proof that another owner already crossed
                // the insertion fence; wait for reconciliation instead of
                // converting a uniqueness race into a failed queue turn.
                if ExecutionProcess::find_by_id(&self.db().pool, process_id)
                    .await?
                    .is_some()
                {
                    return Err(ContainerError::AdmissionWaiting);
                }
                if let Some((token, _)) = &admission {
                    let _ = db::models::agent_turn_admission::AgentTurnAdmission::release(
                        &self.db().pool,
                        token.token_id,
                        token.fence,
                    )
                    .await;
                }
                return if admission.is_some() && matches!(error, sqlx::Error::RowNotFound) {
                    Err(ContainerError::AdmissionWaiting)
                } else {
                    Err(error.into())
                };
            }
        };
        if let Some((token, _)) = &admission
            && !db::models::agent_turn_admission::AgentTurnAdmission::mark_started(
                &self.db().pool,
                token.token_id,
                token.fence,
            )
            .await?
        {
            // No external executor has started yet. Remove the durable process
            // row and release this fence so retry remains safe and observable.
            ExecutionProcess::rollback_unfinalized_start(&self.db().pool, process_id).await?;
            let _ = db::models::agent_turn_admission::AgentTurnAdmission::release(
                &self.db().pool,
                token.token_id,
                token.fence,
            )
            .await;
            return Err(ContainerError::AdmissionWaiting);
        }
        if admission.is_some() {
            if let Err(error) =
                db::models::execution_external_start::ExecutionExternalStart::authorize(
                    &self.db().pool,
                    process_id,
                )
                .await
            {
                ExecutionProcess::update_completion(
                    &self.db().pool,
                    process_id,
                    ExecutionProcessStatus::Failed,
                    None,
                )
                .await?;
                if let Some((token, _)) = &admission {
                    let _ = db::models::agent_turn_admission::AgentTurnAdmission::release(
                        &self.db().pool,
                        token.token_id,
                        token.fence,
                    )
                    .await;
                }
                return Err(error.into());
            }
        }
        let execution_process_id = execution_process.id.to_string();
        tracing::Span::current().record("execution_process_id", execution_process_id.as_str());
        self.msg_stores()
            .write()
            .await
            .insert(execution_process.id, Arc::new(MsgStore::new()));
        if *run_reason != ExecutionProcessRunReason::ArchiveScript
            && let Err(e) = Workspace::set_archived(&self.db().pool, workspace.id, false).await
        {
            self.msg_stores()
                .write()
                .await
                .remove(&execution_process.id);
            db::models::execution_external_start::ExecutionExternalStart::fail(
                &self.db().pool,
                process_id,
            )
            .await?;
            ExecutionProcess::update_completion(
                &self.db().pool,
                process_id,
                ExecutionProcessStatus::Failed,
                None,
            )
            .await?;
            if let Some((token, _)) = &admission {
                let _ = db::models::agent_turn_admission::AgentTurnAdmission::release(
                    &self.db().pool,
                    token.token_id,
                    token.fence,
                )
                .await;
            }
            return Err(e.into());
        }

        if let Some(prompt) = match executor_action_for_process.typ() {
            ExecutorActionType::CodingAgentInitialRequest(coding_agent_request) => {
                Some(coding_agent_request.prompt.clone())
            }
            ExecutorActionType::CodingAgentFollowUpRequest(follow_up_request) => {
                Some(follow_up_request.prompt.clone())
            }
            ExecutorActionType::CodingAgentSessionCommandRequest(command_request) => {
                Some(command_request.prompt())
            }
            ExecutorActionType::ReviewRequest(review_request) => {
                Some(review_request.prompt.clone())
            }
            ExecutorActionType::ScriptRequest(_) => None,
        } {
            let create_coding_agent_turn = CreateCodingAgentTurn {
                execution_process_id: execution_process.id,
                prompt: Some(prompt),
            };

            let coding_agent_turn_id = Uuid::new_v4();

            if let Err(e) = CodingAgentTurn::create(
                &self.db().pool,
                &create_coding_agent_turn,
                coding_agent_turn_id,
            )
            .await
            {
                self.msg_stores()
                    .write()
                    .await
                    .remove(&execution_process.id);
                db::models::execution_external_start::ExecutionExternalStart::fail(
                    &self.db().pool,
                    process_id,
                )
                .await?;
                ExecutionProcess::update_completion(
                    &self.db().pool,
                    process_id,
                    ExecutionProcessStatus::Failed,
                    None,
                )
                .await?;
                if let Some((token, _)) = &admission {
                    let _ = db::models::agent_turn_admission::AgentTurnAdmission::release(
                        &self.db().pool,
                        token.token_id,
                        token.fence,
                    )
                    .await;
                }
                return Err(e.into());
            }
        }

        let is_clear_session_command = matches!(
            executor_action_for_process.typ(),
            ExecutorActionType::CodingAgentSessionCommandRequest(
                executors::actions::session_command::CodingAgentSessionCommandRequest {
                    command: executors::actions::session_command::SessionCommand::Clear,
                    ..
                }
            )
        );

        let claim = if admission.is_some() {
            match db::models::execution_external_start::ExecutionExternalStart::claim(
                &self.db().pool,
                process_id,
                chrono::Duration::seconds(120),
            )
            .await?
            {
                Some(claim) => Some(claim),
                None => {
                    ExecutionProcess::update_completion(
                        &self.db().pool,
                        process_id,
                        ExecutionProcessStatus::Failed,
                        None,
                    )
                    .await?;
                    db::models::execution_external_start::ExecutionExternalStart::fail(
                        &self.db().pool,
                        process_id,
                    )
                    .await?;
                    if let Some((token, _)) = &admission {
                        let _ = db::models::agent_turn_admission::AgentTurnAdmission::release(
                            &self.db().pool,
                            token.token_id,
                            token.fence,
                        )
                        .await;
                    }
                    return Err(ContainerError::AdmissionWaiting);
                }
            }
        } else {
            None
        };
        let spawn_request_ready = if let Some(claim) = &claim {
            db::models::execution_external_start::ExecutionExternalStart::mark_spawn_requested(
                &self.db().pool,
                process_id,
                claim.fence,
            )
            .await
            .unwrap_or(false)
        } else {
            true
        };
        if !spawn_request_ready {
            ExecutionProcess::update_completion(
                &self.db().pool,
                process_id,
                ExecutionProcessStatus::Failed,
                None,
            )
            .await?;
            db::models::execution_external_start::ExecutionExternalStart::fail(
                &self.db().pool,
                process_id,
            )
            .await?;
            if let Some((token, _)) = &admission {
                let _ = db::models::agent_turn_admission::AgentTurnAdmission::release(
                    &self.db().pool,
                    token.token_id,
                    token.fence,
                )
                .await;
            }
            return Err(ContainerError::AdmissionWaiting);
        }
        if let Err(start_error) = self
            .start_execution_inner(
                workspace,
                &execution_process,
                &executor_action_for_process,
                claim.as_ref().map(|claim| claim.fence),
            )
            .await
        {
            self.msg_stores()
                .write()
                .await
                .remove(&execution_process.id);
            if matches!(start_error, ContainerError::ExternalProcessUnresolved) {
                // The child may still be alive. Keep the process/admission as
                // capacity until restart reconciliation proves it absent or
                // terminates its exact persisted OS identity.
                return Err(start_error);
            }
            // Mark process as failed
            if let Err(update_error) = ExecutionProcess::update_completion(
                &self.db().pool,
                execution_process.id,
                ExecutionProcessStatus::Failed,
                None,
            )
            .await
            {
                tracing::error!(
                    "Failed to mark execution process {} as failed after start error: {}",
                    execution_process.id,
                    update_error
                );
            }
            let _ = db::models::execution_external_start::ExecutionExternalStart::fail(
                &self.db().pool,
                process_id,
            )
            .await;
            if let Some((token, _)) = &admission {
                let _ = db::models::agent_turn_admission::AgentTurnAdmission::release(
                    &self.db().pool,
                    token.token_id,
                    token.fence,
                )
                .await;
            }
            // Emit stderr error message
            let log_message = LogMsg::Stderr(format!("Failed to start execution: {start_error}"));
            if let Err(e) = execution_process::append_log_message(
                session.id,
                execution_process.id,
                &log_message,
            )
            .await
            {
                tracing::error!(
                    "Failed to write error log for execution {}: {}",
                    execution_process.id,
                    e
                );
            }

            // Emit NextAction with failure context for coding agent requests
            if let ContainerError::ExecutorError(ExecutorError::ExecutableNotFound { program }) =
                &start_error
            {
                let help_text = format!("The required executable `{program}` is not installed.");
                let error_message = NormalizedEntry {
                    timestamp: None,
                    entry_type: NormalizedEntryType::ErrorMessage {
                        error_type: NormalizedEntryError::SetupRequired,
                    },
                    content: help_text,
                    metadata: None,
                };
                let patch = ConversationPatch::add_normalized_entry(2, error_message);
                if let Err(e) = execution_process::append_log_message(
                    session.id,
                    execution_process.id,
                    &LogMsg::JsonPatch(patch),
                )
                .await
                {
                    tracing::error!(
                        "Failed to write setup-required log for execution {}: {}",
                        execution_process.id,
                        e
                    );
                }
            };
            return Err(start_error);
        }
        if admission.is_some()
            && !db::models::execution_external_start::ExecutionExternalStart::is_spawned(
                &self.db().pool,
                process_id,
            )
            .await?
        {
            ExecutionProcess::update_completion(
                &self.db().pool,
                process_id,
                ExecutionProcessStatus::Failed,
                None,
            )
            .await?;
            db::models::execution_external_start::ExecutionExternalStart::fail(
                &self.db().pool,
                process_id,
            )
            .await?;
            if let Some((token, _)) = &admission {
                let _ = db::models::agent_turn_admission::AgentTurnAdmission::release(
                    &self.db().pool,
                    token.token_id,
                    token.fence,
                )
                .await;
            }
            return Err(ContainerError::AdmissionWaiting);
        }

        if is_clear_session_command
            && let Err(e) =
                Session::mark_context_cleared(&self.db().pool, session.id, execution_process.id)
                    .await
        {
            return Err(e.into());
        }

        // Start processing normalised logs for executor requests and follow ups
        let workspace_root = self.workspace_to_current_dir(workspace);
        #[cfg_attr(feature = "qa-mode", allow(unused_variables))]
        if let Some((executor_profile_id, working_dir)) = match executor_action_for_process.typ() {
            ExecutorActionType::CodingAgentInitialRequest(request) => Some((
                request.executor_config.profile_id(),
                request.effective_dir(&workspace_root),
            )),
            ExecutorActionType::CodingAgentFollowUpRequest(request) => Some((
                request.executor_config.profile_id(),
                request.effective_dir(&workspace_root),
            )),
            ExecutorActionType::CodingAgentSessionCommandRequest(request) => {
                if request.static_message().is_some() {
                    executors::actions::session_command::normalize_static_session_command_logs(
                        self.get_msg_store_by_id(&execution_process.id)
                            .await
                            .ok_or_else(|| {
                                ContainerError::Other(anyhow!(
                                    "MsgStore missing for session command execution {}",
                                    execution_process.id
                                ))
                            })?,
                    );
                    None
                } else {
                    Some((
                        request.executor_config.profile_id(),
                        request.effective_dir(&workspace_root),
                    ))
                }
            }
            ExecutorActionType::ReviewRequest(request) => Some((
                request.executor_config.profile_id(),
                request.effective_dir(&workspace_root),
            )),
            _ => None,
        } {
            let msg_store = match self.get_msg_store_by_id(&execution_process.id).await {
                Some(store) => store,
                None => {
                    self.msg_stores()
                        .write()
                        .await
                        .remove(&execution_process.id);
                    return Err(ContainerError::Other(anyhow!(
                        "MsgStore missing for execution {} during normalization setup",
                        execution_process.id
                    )));
                }
            };
            if executor_action_for_process.uses_qa_mock_log_normalizer() {
                let executor = QaMockExecutor;
                let _ = executor.normalize_mock_logs(msg_store, &working_dir);
            } else if let Some(executor) =
                ExecutorConfigs::get_cached().get_coding_agent(&executor_profile_id)
            {
                let _ = executor.normalize_logs(msg_store, &working_dir);
            } else {
                tracing::error!(
                    "Failed to resolve profile '{:?}' for normalization",
                    executor_profile_id
                );
            }
        }

        execution_process::spawn_stream_raw_logs_to_storage(
            self.msg_stores().clone(),
            self.db().clone(),
            execution_process.id,
            session.id,
        );
        Ok(execution_process)
    }

    async fn try_start_next_action(&self, ctx: &ExecutionContext) -> Result<(), ContainerError> {
        let action = ctx.execution_process.executor_action()?;
        let next_action = if let Some(next_action) = action.next_action() {
            next_action
        } else {
            tracing::debug!("No next action configured");
            return Ok(());
        };

        // Determine the run reason of the next action
        let next_run_reason = match (action.typ(), next_action.typ()) {
            (ExecutorActionType::ScriptRequest(_), ExecutorActionType::ScriptRequest(_)) => {
                ExecutionProcessRunReason::SetupScript
            }
            (
                ExecutorActionType::CodingAgentInitialRequest(_)
                | ExecutorActionType::CodingAgentFollowUpRequest(_)
                | ExecutorActionType::CodingAgentSessionCommandRequest(_)
                | ExecutorActionType::ReviewRequest(_),
                ExecutorActionType::ScriptRequest(_),
            ) => ExecutionProcessRunReason::CleanupScript,
            (
                _,
                ExecutorActionType::CodingAgentFollowUpRequest(_)
                | ExecutorActionType::CodingAgentInitialRequest(_)
                | ExecutorActionType::CodingAgentSessionCommandRequest(_)
                | ExecutorActionType::ReviewRequest(_),
            ) => ExecutionProcessRunReason::CodingAgent,
        };

        self.start_execution(&ctx.workspace, &ctx.session, next_action, &next_run_reason)
            .await?;

        tracing::debug!("Started next action: {:?}", next_action);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::{
        ConnectOptions, SqlitePool,
        sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    };
    use tokio::sync::Mutex;

    use super::*;
    use crate::services::config::Config;

    fn queue_test_session(executor: &str) -> Session {
        let now = chrono::Utc::now();
        Session {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            name: Some("workflow-role".into()),
            executor: Some(executor.into()),
            agent_working_dir: None,
            context_reset_execution_process_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn queued_data(
        config: Option<ExecutorConfig>,
    ) -> db::models::agent_message_queue::QueuedFollowUpData {
        db::models::agent_message_queue::QueuedFollowUpData {
            message: "review".into(),
            executor_config: config,
            session_command: None,
            provenance: None,
            operation_key: None,
        }
    }

    #[test]
    fn queued_workflow_config_preserves_model_and_reasoning_exactly() {
        let mut config = ExecutorConfig::new(BaseCodingAgent::Codex);
        config.model_id = Some("gpt-5-codex".into());
        config.reasoning_id = Some("xhigh".into());
        let json = serde_json::to_string(&queued_data(Some(config.clone()))).unwrap();
        let persisted: db::models::agent_message_queue::QueuedFollowUpData =
            serde_json::from_str(&json).unwrap();
        let applied = TestContainerService::queued_executor_config_override(
            &queue_test_session("CODEX"),
            &persisted,
        )
        .unwrap()
        .unwrap();
        assert_eq!(applied.model_id.as_deref(), Some("gpt-5-codex"));
        assert_eq!(applied.reasoning_id.as_deref(), Some("xhigh"));
    }

    #[test]
    fn queued_workflow_config_supports_legacy_entries_and_rejects_executor_mismatch() {
        let legacy: db::models::agent_message_queue::QueuedFollowUpData =
            serde_json::from_str(r#"{"message":"review"}"#).unwrap();
        assert!(
            TestContainerService::queued_executor_config_override(
                &queue_test_session("CODEX"),
                &legacy
            )
            .unwrap()
            .is_none()
        );

        let config = ExecutorConfig::new(BaseCodingAgent::ClaudeCode);
        let error = TestContainerService::queued_executor_config_override(
            &queue_test_session("CODEX"),
            &queued_data(Some(config)),
        )
        .unwrap_err();
        assert!(error.to_string().contains("does not match"));
    }

    fn exact_test_config() -> ExecutorConfig {
        let mut config = ExecutorConfig::new(BaseCodingAgent::Codex);
        config.model_id = Some("gpt-5-codex".into());
        config.reasoning_id = Some("xhigh".into());
        config
    }

    fn assert_exact_config(config: &ExecutorConfig) {
        assert_eq!(config.model_id.as_deref(), Some("gpt-5-codex"));
        assert_eq!(config.reasoning_id.as_deref(), Some("xhigh"));
    }

    #[test]
    fn queued_action_builder_preserves_config_for_initial_and_follow_up() {
        let data = queued_data(Some(exact_test_config()));
        match TestContainerService::build_queued_action_type(&data, exact_test_config(), None, None)
        {
            ExecutorActionType::CodingAgentInitialRequest(request) => {
                assert_exact_config(&request.executor_config)
            }
            other => panic!("expected initial request, got {other:?}"),
        }

        match TestContainerService::build_queued_action_type(
            &data,
            exact_test_config(),
            Some(CodingAgentResumeInfo {
                session_id: "provider-session".into(),
                message_id: None,
            }),
            None,
        ) {
            ExecutorActionType::CodingAgentFollowUpRequest(request) => {
                assert_eq!(request.session_id, "provider-session");
                assert_exact_config(&request.executor_config);
            }
            other => panic!("expected follow-up request, got {other:?}"),
        }
    }

    #[test]
    fn queued_action_builder_preserves_config_for_session_commands() {
        let mut data = queued_data(Some(exact_test_config()));
        data.session_command = Some(
            executors::actions::session_command::SessionCommand::Compact {
                instructions: Some("retain context".into()),
            },
        );
        match TestContainerService::build_queued_action_type(
            &data,
            exact_test_config(),
            Some(CodingAgentResumeInfo {
                session_id: "provider-session".into(),
                message_id: None,
            }),
            None,
        ) {
            ExecutorActionType::CodingAgentSessionCommandRequest(request) => {
                assert_eq!(request.session_id.as_deref(), Some("provider-session"));
                assert_exact_config(&request.executor_config);
            }
            other => panic!("expected session command request, got {other:?}"),
        }
    }

    struct TestContainerService {
        db: DBService,
        git: GitService,
        config: Arc<RwLock<Config>>,
        notifications: NotificationService,
        msg_stores: Arc<RwLock<HashMap<Uuid, Arc<MsgStore>>>>,
        stopped_processes: Arc<Mutex<Vec<Uuid>>>,
        events: Arc<Mutex<Vec<TestContainerEvent>>>,
        external_start_behavior: Arc<Mutex<TestExternalStartBehavior>>,
        container_ref: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum TestContainerEvent {
        EnsureContainerExists,
        IsContainerClean,
        StopExecution(Uuid),
        ExternalStart(Uuid),
    }

    #[derive(Clone, Copy, Debug, Default)]
    enum TestExternalStartBehavior {
        #[default]
        Confirm,
        ErrorBeforeConfirmation,
        ReconcileUnresolved,
    }

    impl TestContainerService {
        fn new(db: DBService, container_ref: String) -> Self {
            let config = Arc::new(RwLock::new(Config::default()));
            Self {
                db,
                git: GitService::new(),
                notifications: NotificationService::new(config.clone()),
                config,
                msg_stores: Arc::new(RwLock::new(HashMap::new())),
                stopped_processes: Arc::new(Mutex::new(Vec::new())),
                events: Arc::new(Mutex::new(Vec::new())),
                external_start_behavior: Arc::new(Mutex::new(TestExternalStartBehavior::Confirm)),
                container_ref,
            }
        }

        async fn stopped_processes(&self) -> Vec<Uuid> {
            self.stopped_processes.lock().await.clone()
        }

        async fn events(&self) -> Vec<TestContainerEvent> {
            self.events.lock().await.clone()
        }

        async fn set_external_start_behavior(&self, behavior: TestExternalStartBehavior) {
            *self.external_start_behavior.lock().await = behavior;
        }
    }

    #[async_trait]
    impl ContainerService for TestContainerService {
        fn msg_stores(&self) -> &Arc<RwLock<HashMap<Uuid, Arc<MsgStore>>>> {
            &self.msg_stores
        }

        fn db(&self) -> &DBService {
            &self.db
        }

        fn git(&self) -> &GitService {
            &self.git
        }

        fn config(&self) -> &Arc<RwLock<Config>> {
            &self.config
        }

        fn notification_service(&self) -> &NotificationService {
            &self.notifications
        }

        async fn touch(&self, _workspace: &Workspace) -> Result<(), ContainerError> {
            Ok(())
        }

        fn workspace_to_current_dir(&self, _workspace: &Workspace) -> PathBuf {
            PathBuf::from(&self.container_ref)
        }

        async fn store_db_stream_handle(&self, _id: Uuid, _handle: JoinHandle<()>) {}

        async fn take_db_stream_handle(&self, _id: &Uuid) -> Option<JoinHandle<()>> {
            None
        }

        async fn create(&self, _workspace: &Workspace) -> Result<ContainerRef, ContainerError> {
            Ok(self.container_ref.clone())
        }

        async fn kill_all_running_processes(&self) -> Result<(), ContainerError> {
            Ok(())
        }

        async fn delete(&self, _workspace: &Workspace) -> Result<(), ContainerError> {
            Ok(())
        }

        async fn ensure_container_exists(
            &self,
            _workspace: &Workspace,
        ) -> Result<ContainerRef, ContainerError> {
            self.events
                .lock()
                .await
                .push(TestContainerEvent::EnsureContainerExists);
            Ok(self.container_ref.clone())
        }

        async fn is_container_clean(&self, _workspace: &Workspace) -> Result<bool, ContainerError> {
            self.events
                .lock()
                .await
                .push(TestContainerEvent::IsContainerClean);
            Ok(true)
        }

        async fn start_execution_inner(
            &self,
            _workspace: &Workspace,
            _execution_process: &ExecutionProcess,
            _executor_action: &ExecutorAction,
            external_start_fence: Option<i64>,
        ) -> Result<(), ContainerError> {
            self.events
                .lock()
                .await
                .push(TestContainerEvent::ExternalStart(_execution_process.id));
            if matches!(
                *self.external_start_behavior.lock().await,
                TestExternalStartBehavior::ErrorBeforeConfirmation
            ) {
                return Err(ContainerError::Other(anyhow!(
                    "injected external start failure"
                )));
            }
            if let Some(fence) = external_start_fence {
                db::models::execution_external_start::ExecutionExternalStart::confirm_spawned(
                    &self.db.pool,
                    _execution_process.id,
                    fence,
                    Some("fake"),
                    Some("fake-start"),
                )
                .await?;
            }
            Ok(())
        }

        async fn reconcile_external_process(
            &self,
            _record: &db::models::execution_external_start::ExecutionExternalStartRecord,
        ) -> Result<ExternalProcessReconciliation, ContainerError> {
            if matches!(
                *self.external_start_behavior.lock().await,
                TestExternalStartBehavior::ReconcileUnresolved
            ) {
                Err(ContainerError::ExternalProcessUnresolved)
            } else {
                Ok(ExternalProcessReconciliation::Terminated)
            }
        }

        async fn stop_execution(
            &self,
            execution_process: &ExecutionProcess,
            status: ExecutionProcessStatus,
        ) -> Result<(), ContainerError> {
            self.events
                .lock()
                .await
                .push(TestContainerEvent::StopExecution(execution_process.id));
            self.stopped_processes
                .lock()
                .await
                .push(execution_process.id);
            ExecutionProcess::update_completion(&self.db.pool, execution_process.id, status, None)
                .await?;
            Ok(())
        }

        async fn try_commit_changes(
            &self,
            _ctx: &ExecutionContext,
        ) -> Result<bool, ContainerError> {
            Ok(false)
        }

        async fn copy_project_files(
            &self,
            _source_dir: &Path,
            _target_dir: &Path,
            _copy_files: &str,
        ) -> Result<(), ContainerError> {
            Ok(())
        }

        async fn stream_diff(
            &self,
            _workspace: &Workspace,
            _stats_only: bool,
        ) -> Result<BoxStream<'static, Result<LogMsg, std::io::Error>>, ContainerError> {
            Ok(futures::stream::empty().boxed())
        }

        async fn git_branch_prefix(&self) -> String {
            String::new()
        }
    }

    async fn test_pool() -> Result<(tempfile::TempDir, SqlitePool), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let db_path = temp_dir.path().join("test.sqlite");
        let database_url = format!("sqlite://{}", db_path.to_string_lossy());
        let options = SqliteConnectOptions::from_str(&database_url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Delete)
            .disable_statement_logging();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::migrate!("../db/migrations").run(&pool).await?;
        Ok((temp_dir, pool))
    }

    async fn insert_workspace(pool: &SqlitePool, container_ref: &str) -> Result<Uuid, sqlx::Error> {
        let workspace_id = Uuid::new_v4();
        sqlx::query("INSERT INTO workspaces (id, branch, container_ref) VALUES (?1, ?2, ?3)")
            .bind(workspace_id)
            .bind("test-branch")
            .bind(container_ref)
            .execute(pool)
            .await?;
        Ok(workspace_id)
    }

    async fn start_fixture() -> Result<
        (
            tempfile::TempDir,
            SqlitePool,
            TestContainerService,
            Workspace,
            Session,
        ),
        Box<dyn std::error::Error>,
    > {
        let (temp_dir, pool) = test_pool().await?;
        let workspace_id =
            insert_workspace(&pool, temp_dir.path().to_string_lossy().as_ref()).await?;
        let repo_dir = temp_dir.path().join("repo");
        std::fs::create_dir_all(&repo_dir)?;
        let repo = Repo::find_or_create(&pool, &repo_dir, "repo").await?;
        sqlx::query("INSERT INTO workspace_repos(id,workspace_id,repo_id,target_branch) VALUES(?1,?2,?3,'main')")
            .bind(Uuid::new_v4()).bind(workspace_id).bind(repo.id).execute(&pool).await?;
        let session = Session::create(
            &pool,
            &CreateSession {
                executor: Some("CODEX".into()),
                name: Some("agent".into()),
            },
            Uuid::new_v4(),
            workspace_id,
        )
        .await?;
        let workspace = Workspace::find_by_id(&pool, workspace_id).await?.unwrap();
        let service = TestContainerService::new(
            DBService { pool: pool.clone() },
            temp_dir.path().to_string_lossy().to_string(),
        );
        Ok((temp_dir, pool, service, workspace, session))
    }

    #[tokio::test]
    async fn shared_direct_coding_start_confirms_external_spawn_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, pool, service, workspace, session) = start_fixture().await?;
        let action = ExecutorAction::new(
            ExecutorActionType::CodingAgentInitialRequest(CodingAgentInitialRequest {
                prompt: "test".into(),
                executor_config: ExecutorConfig::new(BaseCodingAgent::Codex),
                working_dir: None,
            }),
            None,
        );
        let process = service
            .start_execution(
                &workspace,
                &session,
                &action,
                &ExecutionProcessRunReason::CodingAgent,
            )
            .await?;
        assert_eq!(service.events().await.iter().filter(|event| matches!(event, TestContainerEvent::ExternalStart(id) if *id == process.id)).count(), 1);
        assert_eq!(
            db::models::execution_external_start::ExecutionExternalStart::state(&pool, process.id)
                .await?
                .as_deref(),
            Some("spawned")
        );
        let admission: String = sqlx::query_scalar(
            "SELECT status FROM agent_turn_admissions WHERE intended_process_id=?1",
        )
        .bind(process.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(admission, "started");
        Ok(())
    }

    #[tokio::test]
    async fn shared_queued_coding_start_confirms_external_spawn_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, pool, service, _workspace, session) = start_fixture().await?;
        let queue = QueuedMessageService::new(DBService { pool: pool.clone() });
        let item = queue
            .queue_message(
                &session,
                "test".into(),
                None,
                db::models::agent_message_queue::AgentMessageSource::FromUser,
                None,
                None,
                None,
                None,
            )
            .await?;
        service.try_start_queued_messages(&queue).await?;
        assert_eq!(
            service
                .events()
                .await
                .iter()
                .filter(
                    |event| matches!(event, TestContainerEvent::ExternalStart(id) if *id == item.id)
                )
                .count(),
            1
        );
        let current = AgentMessageQueueItem::find_by_id(&pool, item.id)
            .await?
            .unwrap();
        assert_eq!(current.status, AgentMessageQueueStatus::Running);
        assert_eq!(
            db::models::execution_external_start::ExecutionExternalStart::state(&pool, item.id)
                .await?
                .as_deref(),
            Some("spawned")
        );
        Ok(())
    }

    #[tokio::test]
    async fn queued_capacity_rejection_remains_waiting_without_external_start()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, pool, service, _workspace, session) = start_fixture().await?;
        service.config.write().await.agent_turn_capacity = 0;
        let queue = QueuedMessageService::new(DBService { pool: pool.clone() });
        let item = queue
            .queue_message(
                &session,
                "test".into(),
                None,
                db::models::agent_message_queue::AgentMessageSource::FromUser,
                None,
                None,
                None,
                None,
            )
            .await?;
        service.try_start_queued_messages(&queue).await?;
        let current = AgentMessageQueueItem::find_by_id(&pool, item.id)
            .await?
            .unwrap();
        assert_eq!(current.status, AgentMessageQueueStatus::Queued);
        assert_eq!(
            current.last_error.as_deref(),
            Some("Waiting for team capacity.")
        );
        assert!(
            !service
                .events()
                .await
                .iter()
                .any(|event| matches!(event, TestContainerEvent::ExternalStart(_)))
        );
        Ok(())
    }

    #[tokio::test]
    async fn script_start_uses_separate_external_start_policy()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, pool, service, workspace, session) = start_fixture().await?;
        let action = ExecutorAction::new(
            ExecutorActionType::ScriptRequest(ScriptRequest {
                script: "true".into(),
                language: ScriptRequestLanguage::Bash,
                context: ScriptContext::SetupScript,
                working_dir: None,
                env: Default::default(),
            }),
            None,
        );
        let process = service
            .start_execution(
                &workspace,
                &session,
                &action,
                &ExecutionProcessRunReason::SetupScript,
            )
            .await?;
        assert!(
            db::models::execution_external_start::ExecutionExternalStart::state(&pool, process.id)
                .await?
                .is_none()
        );
        Ok(())
    }

    async fn assert_pre_spawn_failure_is_terminal(
        pool: &SqlitePool,
        service: &TestContainerService,
    ) -> Result<(), Box<dyn std::error::Error>> {
        assert!(
            !service
                .events()
                .await
                .iter()
                .any(|event| matches!(event, TestContainerEvent::ExternalStart(_)))
        );
        let (process_id, process_status): (Uuid, String) = sqlx::query_as(
            "SELECT id,status FROM execution_processes ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(process_status, "failed");
        let admission_status: String = sqlx::query_scalar(
            "SELECT status FROM agent_turn_admissions WHERE intended_process_id=?1",
        )
        .bind(process_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(admission_status, "released");
        assert_eq!(
            db::models::execution_external_start::ExecutionExternalStart::state(pool, process_id)
                .await?
                .as_deref(),
            Some("failed")
        );
        Ok(())
    }

    #[tokio::test]
    async fn workspace_update_failure_never_reaches_external_start()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, pool, service, workspace, session) = start_fixture().await?;
        sqlx::query("CREATE TRIGGER fail_workspace_update BEFORE UPDATE ON workspaces BEGIN SELECT RAISE(FAIL,'test workspace failure'); END")
            .execute(&pool).await?;
        let action = ExecutorAction::new(
            ExecutorActionType::CodingAgentInitialRequest(CodingAgentInitialRequest {
                prompt: "test".into(),
                executor_config: ExecutorConfig::new(BaseCodingAgent::Codex),
                working_dir: None,
            }),
            None,
        );
        assert!(
            service
                .start_execution(
                    &workspace,
                    &session,
                    &action,
                    &ExecutionProcessRunReason::CodingAgent
                )
                .await
                .is_err()
        );
        assert_pre_spawn_failure_is_terminal(&pool, &service).await
    }

    #[tokio::test]
    async fn coding_turn_creation_failure_never_reaches_external_start()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, pool, service, workspace, session) = start_fixture().await?;
        sqlx::query("CREATE TRIGGER fail_turn_insert BEFORE INSERT ON coding_agent_turns BEGIN SELECT RAISE(FAIL,'test turn failure'); END")
            .execute(&pool).await?;
        let action = ExecutorAction::new(
            ExecutorActionType::CodingAgentInitialRequest(CodingAgentInitialRequest {
                prompt: "test".into(),
                executor_config: ExecutorConfig::new(BaseCodingAgent::Codex),
                working_dir: None,
            }),
            None,
        );
        assert!(
            service
                .start_execution(
                    &workspace,
                    &session,
                    &action,
                    &ExecutionProcessRunReason::CodingAgent
                )
                .await
                .is_err()
        );
        assert_pre_spawn_failure_is_terminal(&pool, &service).await
    }

    #[tokio::test]
    async fn external_start_error_terminalizes_process_and_releases_admission()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, pool, service, workspace, session) = start_fixture().await?;
        service
            .set_external_start_behavior(TestExternalStartBehavior::ErrorBeforeConfirmation)
            .await;
        let action = ExecutorAction::new(
            ExecutorActionType::CodingAgentInitialRequest(CodingAgentInitialRequest {
                prompt: "test".into(),
                executor_config: ExecutorConfig::new(BaseCodingAgent::Codex),
                working_dir: None,
            }),
            None,
        );
        assert!(
            service
                .start_execution(
                    &workspace,
                    &session,
                    &action,
                    &ExecutionProcessRunReason::CodingAgent
                )
                .await
                .is_err()
        );
        assert_eq!(
            service
                .events()
                .await
                .iter()
                .filter(|event| matches!(event, TestContainerEvent::ExternalStart(_)))
                .count(),
            1
        );
        let (process_id, status): (Uuid, String) = sqlx::query_as(
            "SELECT id,status FROM execution_processes ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(status, "failed");
        let admission: String = sqlx::query_scalar(
            "SELECT status FROM agent_turn_admissions WHERE intended_process_id=?1",
        )
        .bind(process_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(admission, "released");
        assert_eq!(
            db::models::execution_external_start::ExecutionExternalStart::state(&pool, process_id)
                .await?
                .as_deref(),
            Some("failed")
        );
        Ok(())
    }

    #[tokio::test]
    async fn restart_reconciles_unconfirmed_external_start_without_orphan_capacity()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, pool, service, workspace, session) = start_fixture().await?;
        let action = ExecutorAction::new(
            ExecutorActionType::CodingAgentInitialRequest(CodingAgentInitialRequest {
                prompt: "test".into(),
                executor_config: ExecutorConfig::new(BaseCodingAgent::Codex),
                working_dir: None,
            }),
            None,
        );
        let process = service
            .start_execution(
                &workspace,
                &session,
                &action,
                &ExecutionProcessRunReason::CodingAgent,
            )
            .await?;
        // Model the durable crash window immediately before the external hook
        // confirmed a spawn. Startup must reclaim the logical operation rather
        // than treating the authorized process row as live forever.
        sqlx::query("UPDATE execution_external_starts SET state='claiming',external_process_id=NULL,claim_expires_at=?2 WHERE execution_process_id=?1")
            .bind(process.id).bind(chrono::Utc::now()-chrono::Duration::seconds(1)).execute(&pool).await?;
        service.cleanup_orphan_executions().await?;
        assert!(
            ExecutionProcess::find_by_id(&pool, process.id)
                .await?
                .is_none()
        );
        let admission: String = sqlx::query_scalar(
            "SELECT status FROM agent_turn_admissions WHERE intended_process_id=?1",
        )
        .bind(process.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(admission, "released");
        Ok(())
    }

    #[tokio::test]
    async fn uncertain_restart_blocks_until_scoped_idempotent_operator_confirmation()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, pool, service, workspace, session) = start_fixture().await?;
        let action = ExecutorAction::new(
            ExecutorActionType::CodingAgentInitialRequest(CodingAgentInitialRequest {
                prompt: "test".into(),
                executor_config: ExecutorConfig::new(BaseCodingAgent::Codex),
                working_dir: None,
            }),
            None,
        );
        let process = service
            .start_execution(
                &workspace,
                &session,
                &action,
                &ExecutionProcessRunReason::CodingAgent,
            )
            .await?;
        sqlx::query("UPDATE execution_external_starts SET state='claiming',external_process_id=NULL,external_process_started_at=NULL WHERE execution_process_id=?1")
            .bind(process.id).execute(&pool).await?;
        service
            .set_external_start_behavior(TestExternalStartBehavior::ReconcileUnresolved)
            .await;
        service.cleanup_orphan_executions().await?;
        assert_eq!(
            db::models::execution_external_start::ExecutionExternalStart::state(&pool, process.id)
                .await?
                .as_deref(),
            Some("blocked")
        );
        let admission: String = sqlx::query_scalar(
            "SELECT status FROM agent_turn_admissions WHERE intended_process_id=?1",
        )
        .bind(process.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(admission, "started");
        // A repeated startup does not fail the cleanup loop or release capacity.
        service.cleanup_orphan_executions().await?;
        assert_eq!(
            db::models::execution_external_start::ExecutionExternalStart::state(&pool, process.id)
                .await?
                .as_deref(),
            Some("blocked")
        );
        let blocked =
            db::models::execution_external_start::ExecutionExternalStart::record(&pool, process.id)
                .await?
                .unwrap();
        let token = blocked.recovery_token.clone().unwrap();
        let generation = blocked.recovery_generation;
        assert_eq!(
            service
                .confirm_external_process_stopped(
                    Uuid::new_v4(),
                    process.id,
                    "operator",
                    &token,
                    generation,
                )
                .await?,
            db::models::execution_external_start::ExternalStartRecoveryResult::WrongWorkspace
        );
        assert_eq!(
            service
                .confirm_external_process_stopped(
                    workspace.id,
                    process.id,
                    "operator",
                    "stale-capability",
                    generation,
                )
                .await?,
            db::models::execution_external_start::ExternalStartRecoveryResult::StaleRecovery
        );
        // An unresolved exact process retains both the blocked state and capacity.
        assert!(
            service
                .confirm_external_process_stopped(
                    workspace.id,
                    process.id,
                    "operator",
                    &token,
                    generation,
                )
                .await
                .is_err()
        );
        service
            .set_external_start_behavior(TestExternalStartBehavior::Confirm)
            .await;
        assert_eq!(
            service
                .confirm_external_process_stopped(
                    workspace.id,
                    process.id,
                    "operator",
                    &token,
                    generation,
                )
                .await?,
            db::models::execution_external_start::ExternalStartRecoveryResult::Reconciled
        );
        assert_eq!(
            service
                .confirm_external_process_stopped(
                    workspace.id,
                    process.id,
                    "operator",
                    &token,
                    generation,
                )
                .await?,
            db::models::execution_external_start::ExternalStartRecoveryResult::AlreadyReconciled
        );
        let admission: String = sqlx::query_scalar(
            "SELECT status FROM agent_turn_admissions WHERE intended_process_id=?1",
        )
        .bind(process.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(admission, "released");
        let actor: String = sqlx::query_scalar(
            "SELECT actor FROM execution_external_start_recoveries WHERE execution_process_id=?1",
        )
        .bind(process.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(actor, "operator");
        Ok(())
    }

    async fn insert_process(
        pool: &SqlitePool,
        session_id: Uuid,
        run_reason: ExecutionProcessRunReason,
        status: ExecutionProcessStatus,
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Uuid, sqlx::Error> {
        let process_id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO execution_processes
               (id, session_id, run_reason, executor_action, status, dropped,
                started_at, created_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, FALSE, ?6, ?7, ?8)"#,
        )
        .bind(process_id)
        .bind(session_id)
        .bind(run_reason)
        .bind("{}")
        .bind(status)
        .bind(created_at)
        .bind(created_at)
        .bind(created_at)
        .execute(pool)
        .await?;
        Ok(process_id)
    }

    struct TwoSessionResetFixture {
        _temp_dir: tempfile::TempDir,
        pool: SqlitePool,
        service: TestContainerService,
        session_a_id: Uuid,
        target_process_id: Uuid,
        session_a_running_process_id: Uuid,
        session_b_running_process_id: Uuid,
    }

    async fn two_session_reset_fixture()
    -> Result<TwoSessionResetFixture, Box<dyn std::error::Error>> {
        let (temp_dir, pool) = test_pool().await?;
        let workspace_id =
            insert_workspace(&pool, temp_dir.path().to_string_lossy().as_ref()).await?;
        let session_a = Session::create(
            &pool,
            &CreateSession {
                executor: Some("codex".to_string()),
                name: Some("session a".to_string()),
            },
            Uuid::new_v4(),
            workspace_id,
        )
        .await?;
        let session_b = Session::create(
            &pool,
            &CreateSession {
                executor: Some("codex".to_string()),
                name: Some("session b".to_string()),
            },
            Uuid::new_v4(),
            workspace_id,
        )
        .await?;

        let now = chrono::Utc::now();
        let target_process_id = insert_process(
            &pool,
            session_a.id,
            ExecutionProcessRunReason::CodingAgent,
            ExecutionProcessStatus::Completed,
            now,
        )
        .await?;
        let session_a_running_process_id = insert_process(
            &pool,
            session_a.id,
            ExecutionProcessRunReason::CodingAgent,
            ExecutionProcessStatus::Running,
            now + chrono::Duration::milliseconds(1),
        )
        .await?;
        let session_b_running_process_id = insert_process(
            &pool,
            session_b.id,
            ExecutionProcessRunReason::CodingAgent,
            ExecutionProcessStatus::Running,
            now + chrono::Duration::milliseconds(2),
        )
        .await?;

        let service = TestContainerService::new(
            DBService { pool: pool.clone() },
            temp_dir.path().to_string_lossy().to_string(),
        );

        Ok(TwoSessionResetFixture {
            _temp_dir: temp_dir,
            pool,
            service,
            session_a_id: session_a.id,
            target_process_id,
            session_a_running_process_id,
            session_b_running_process_id,
        })
    }

    #[tokio::test]
    async fn reset_session_to_process_stops_only_processes_in_target_session()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = two_session_reset_fixture().await?;

        fixture
            .service
            .reset_session_to_process(
                fixture.session_a_id,
                fixture.target_process_id,
                false,
                false,
                false,
            )
            .await?;

        assert_eq!(
            fixture.service.stopped_processes().await,
            vec![fixture.session_a_running_process_id]
        );

        let session_a_process =
            ExecutionProcess::find_by_id(&fixture.pool, fixture.session_a_running_process_id)
                .await?
                .expect("session A running process should still exist");
        assert_eq!(session_a_process.status, ExecutionProcessStatus::Killed);
        assert!(session_a_process.dropped);

        let session_b_process =
            ExecutionProcess::find_by_id(&fixture.pool, fixture.session_b_running_process_id)
                .await?
                .expect("session B running process should still exist");
        assert_eq!(session_b_process.status, ExecutionProcessStatus::Running);
        assert!(!session_b_process.dropped);

        Ok(())
    }

    #[tokio::test]
    async fn reset_session_to_process_rejects_git_reset_while_other_session_runs_without_override()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = two_session_reset_fixture().await?;

        let err = fixture
            .service
            .reset_session_to_process(
                fixture.session_a_id,
                fixture.target_process_id,
                true,
                false,
                false,
            )
            .await
            .expect_err("git reset should be rejected while another session is running");

        assert!(
            err.to_string()
                .contains("Cannot reset worktree while another session is running")
        );
        assert_eq!(
            fixture.service.stopped_processes().await,
            Vec::<Uuid>::new()
        );

        let session_a_process =
            ExecutionProcess::find_by_id(&fixture.pool, fixture.session_a_running_process_id)
                .await?
                .expect("session A running process should still exist");
        assert_eq!(session_a_process.status, ExecutionProcessStatus::Running);
        assert!(!session_a_process.dropped);

        let session_b_process =
            ExecutionProcess::find_by_id(&fixture.pool, fixture.session_b_running_process_id)
                .await?
                .expect("session B running process should still exist");
        assert_eq!(session_b_process.status, ExecutionProcessStatus::Running);
        assert!(!session_b_process.dropped);

        Ok(())
    }

    #[tokio::test]
    async fn reset_session_to_process_stops_other_session_when_git_reset_override_is_enabled()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = two_session_reset_fixture().await?;

        fixture
            .service
            .reset_session_to_process(
                fixture.session_a_id,
                fixture.target_process_id,
                true,
                false,
                true,
            )
            .await?;

        assert_eq!(
            fixture.service.stopped_processes().await,
            vec![
                fixture.session_b_running_process_id,
                fixture.session_a_running_process_id,
            ]
        );
        assert_eq!(
            fixture.service.events().await,
            vec![
                TestContainerEvent::StopExecution(fixture.session_b_running_process_id),
                TestContainerEvent::StopExecution(fixture.session_a_running_process_id),
                TestContainerEvent::EnsureContainerExists,
                TestContainerEvent::IsContainerClean,
            ]
        );

        let session_a_process =
            ExecutionProcess::find_by_id(&fixture.pool, fixture.session_a_running_process_id)
                .await?
                .expect("session A running process should still exist");
        assert_eq!(session_a_process.status, ExecutionProcessStatus::Killed);
        assert!(session_a_process.dropped);

        let session_b_process =
            ExecutionProcess::find_by_id(&fixture.pool, fixture.session_b_running_process_id)
                .await?
                .expect("session B running process should still exist");
        assert_eq!(session_b_process.status, ExecutionProcessStatus::Killed);
        assert!(!session_b_process.dropped);

        Ok(())
    }
}
