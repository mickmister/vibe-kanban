use std::{collections::HashMap, time::Instant};

use axum::{Json, extract::State, response::Json as ResponseJson};
use db::models::{
    coding_agent_turn::CodingAgentTurn,
    execution_process::{ExecutionProcess, ExecutionProcessStatus},
    merge::MergeStatus,
    pull_request::PullRequest,
    workspace::Workspace,
};
use deployment::Deployment;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError};

/// Request for fetching workspace summaries
#[derive(Debug, Deserialize, Serialize, TS)]
pub struct WorkspaceSummaryRequest {
    pub archived: bool,
}

/// Summary info for a single workspace
#[derive(Debug, Serialize, TS)]
pub struct WorkspaceSummary {
    pub workspace_id: Uuid,
    /// Session ID of the latest execution process
    pub latest_session_id: Option<Uuid>,
    /// Is a tool approval currently pending?
    pub has_pending_approval: bool,
    /// Number of files with changes
    pub files_changed: Option<usize>,
    /// Total lines added across all files
    pub lines_added: Option<usize>,
    /// Total lines removed across all files
    pub lines_removed: Option<usize>,
    /// When the latest execution process completed
    #[ts(optional)]
    pub latest_process_completed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Status of the latest execution process
    pub latest_process_status: Option<ExecutionProcessStatus>,
    /// Is a dev server currently running?
    pub has_running_dev_server: bool,
    /// Does this workspace have unseen coding agent turns?
    pub has_unseen_turns: bool,
    /// PR status for this workspace (if any PR exists)
    pub pr_status: Option<MergeStatus>,
    /// PR number for this workspace (if any PR exists)
    pub pr_number: Option<i64>,
    /// PR URL for this workspace (if any PR exists)
    pub pr_url: Option<String>,
}

/// Response containing summaries for requested workspaces
#[derive(Debug, Serialize, TS)]
pub struct WorkspaceSummaryResponse {
    pub summaries: Vec<WorkspaceSummary>,
}

#[derive(Debug, Clone, Default, Serialize, TS)]
pub struct DiffStats {
    pub files_changed: usize,
    pub lines_added: usize,
    pub lines_removed: usize,
}

/// Fetch summary information for workspaces filtered by archived status.
/// This endpoint returns data that cannot be efficiently included in the streaming endpoint.
#[axum::debug_handler]
pub async fn get_workspace_summaries(
    State(deployment): State<DeploymentImpl>,
    Json(request): Json<WorkspaceSummaryRequest>,
) -> Result<ResponseJson<ApiResponse<WorkspaceSummaryResponse>>, ApiError> {
    let started_at = Instant::now();
    let before = utils::process_diag::sample_current_process();
    let pool = &deployment.db().pool;
    let archived = request.archived;

    // 1. Fetch all workspaces with the given archived status
    let workspaces: Vec<Workspace> = Workspace::find_all_with_status(pool, Some(archived), None)
        .await?
        .into_iter()
        .map(|ws| ws.workspace)
        .collect();

    if workspaces.is_empty() {
        return Ok(ResponseJson(ApiResponse::success(
            WorkspaceSummaryResponse { summaries: vec![] },
        )));
    }

    // 2. Fetch latest process info for workspaces with this archived status
    let latest_processes = ExecutionProcess::find_latest_for_workspaces(pool, archived).await?;

    // 3. Check which workspaces have running dev servers
    let dev_server_workspaces =
        ExecutionProcess::find_workspaces_with_running_dev_servers(pool, archived).await?;

    // 4. Check pending approvals for running processes
    let running_ep_ids: Vec<_> = latest_processes
        .values()
        .filter(|info| info.status == ExecutionProcessStatus::Running)
        .map(|info| info.execution_process_id)
        .collect();
    let pending_approval_eps = deployment
        .approvals()
        .get_pending_execution_process_ids(&running_ep_ids);

    // 5. Check which workspaces have unseen coding agent turns
    let unseen_workspaces = CodingAgentTurn::find_workspaces_with_unseen(pool, archived).await?;

    // 6. Get PR status for each workspace
    let pr_statuses = PullRequest::get_latest_for_workspaces(pool, archived).await?;

    // 7. Compute diff stats for each workspace (in parallel)
    let diff_futures: Vec<_> = workspaces
        .iter()
        .map(|ws| {
            let workspace = ws.clone();
            let deployment = deployment.clone();
            async move {
                if workspace.container_ref.is_some() {
                    compute_workspace_diff_report(&deployment, &workspace)
                        .await
                        .map(|report| (workspace.id, report))
                } else {
                    None
                }
            }
        })
        .collect();

    let diff_results: Vec<Option<(Uuid, WorkspaceDiffComputationReport)>> =
        futures_util::future::join_all(diff_futures).await;
    let mut total_repo_count = 0_usize;
    let mut total_base_commit_failures = 0_usize;
    let mut total_diff_fetch_failures = 0_usize;
    let diff_stats: HashMap<Uuid, DiffStats> = diff_results
        .into_iter()
        .flatten()
        .map(|(workspace_id, report)| {
            total_repo_count += report.repo_count;
            total_base_commit_failures += report.base_commit_failures;
            total_diff_fetch_failures += report.diff_fetch_failures;
            (workspace_id, report.stats)
        })
        .collect();

    // 8. Assemble response
    let summaries: Vec<WorkspaceSummary> = workspaces
        .iter()
        .map(|ws| {
            let id = ws.id;
            let latest = latest_processes.get(&id);
            let has_pending = latest
                .map(|p| pending_approval_eps.contains(&p.execution_process_id))
                .unwrap_or(false);
            let stats = diff_stats.get(&id);

            WorkspaceSummary {
                workspace_id: id,
                latest_session_id: latest.map(|p| p.session_id),
                has_pending_approval: has_pending,
                files_changed: stats.map(|s| s.files_changed),
                lines_added: stats.map(|s| s.lines_added),
                lines_removed: stats.map(|s| s.lines_removed),
                latest_process_completed_at: latest.and_then(|p| p.completed_at),
                latest_process_status: latest.map(|p| p.status.clone()),
                has_running_dev_server: dev_server_workspaces.contains(&id),
                has_unseen_turns: unseen_workspaces.contains(&id),
                pr_status: pr_statuses.get(&id).map(|pr| pr.pr_status.clone()),
                pr_number: pr_statuses.get(&id).map(|pr| pr.pr_number),
                pr_url: pr_statuses.get(&id).map(|pr| pr.pr_url.clone()),
            }
        })
        .collect();

    let after = utils::process_diag::sample_current_process();
    if crate::startup::runtime_diagnostics_enabled() {
        tracing::info!(
            archived,
            workspace_count = workspaces.len(),
            workspaces_with_container_ref = workspaces
                .iter()
                .filter(|workspace| workspace.container_ref.is_some())
                .count(),
            diff_stats_workspace_count = diff_stats.len(),
            total_repo_count,
            total_base_commit_failures,
            total_diff_fetch_failures,
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            rss_mb_before = utils::process_diag::bytes_to_mb(before.rss_bytes),
            rss_mb_after = utils::process_diag::bytes_to_mb(after.rss_bytes),
            vm_size_mb_before = utils::process_diag::bytes_to_mb(before.virtual_bytes),
            vm_size_mb_after = utils::process_diag::bytes_to_mb(after.virtual_bytes),
            threads_before = before.thread_count,
            threads_after = after.thread_count,
            fds_before = before.open_fd_count,
            fds_after = after.open_fd_count,
            child_processes_before = before.child_process_count,
            child_processes_after = after.child_process_count,
            "runtime_diag_workspace_summaries"
        );
    }

    Ok(ResponseJson(ApiResponse::success(
        WorkspaceSummaryResponse { summaries },
    )))
}

/// Compute diff stats for a workspace.
pub async fn compute_workspace_diff_stats(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
) -> Option<DiffStats> {
    let report = services::services::diff_stream::compute_diff_stats_with_report(
        &deployment.db().pool,
        deployment.git(),
        workspace,
    )
    .await?;

    let stats = report.stats;

    Some(DiffStats {
        files_changed: stats.files_changed,
        lines_added: stats.lines_added,
        lines_removed: stats.lines_removed,
    })
}

#[derive(Debug, Clone)]
struct WorkspaceDiffComputationReport {
    stats: DiffStats,
    repo_count: usize,
    base_commit_failures: usize,
    diff_fetch_failures: usize,
}

async fn compute_workspace_diff_report(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
) -> Option<WorkspaceDiffComputationReport> {
    let report = services::services::diff_stream::compute_diff_stats_with_report(
        &deployment.db().pool,
        deployment.git(),
        workspace,
    )
    .await?;

    Some(WorkspaceDiffComputationReport {
        stats: DiffStats {
            files_changed: report.stats.files_changed,
            lines_added: report.stats.lines_added,
            lines_removed: report.stats.lines_removed,
        },
        repo_count: report.repo_count,
        base_commit_failures: report.base_commit_failures,
        diff_fetch_failures: report.diff_fetch_failures,
    })
}
