use std::collections::HashMap;

use axum::{Extension, extract::State, response::Json as ResponseJson};
use db::models::{
    preview_identity::{RepoPreviewSlug, WorkspacePreviewToken},
    preview_slot::PreviewSlot,
    session::Session,
    workspace::Workspace,
    workspace_repo::WorkspaceRepo,
};
use deployment::Deployment;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utils::response::ApiResponse;

use super::execution::PreviewSlotUrlParts;
use crate::{DeploymentImpl, error::ApiError};

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PanelTargetDeliveryDefinition {
    pub location: String,
    pub factory_key: String,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionPanelTargetDefinition {
    pub session_id: uuid::Uuid,
    pub workspace_id: uuid::Uuid,
    pub delivery: PanelTargetDeliveryDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TerminalPanelTargetDefinition {
    pub terminal_id: String,
    pub workspace_id: uuid::Uuid,
    pub delivery: PanelTargetDeliveryDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPanelTargetDefinition {
    pub preview_slot_id: uuid::Uuid,
    pub workspace_id: uuid::Uuid,
    pub url_parts: PreviewSlotUrlParts,
    pub customer_slug: String,
    pub factory_key: String,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePanelTargetAuthoritySnapshot {
    pub ready: bool,
    pub workspace_id: uuid::Uuid,
    pub workspace_targets: HashMap<String, PanelTargetDeliveryDefinition>,
    pub sessions: Vec<SessionPanelTargetDefinition>,
    pub terminals_ready: bool,
    pub terminals: Vec<TerminalPanelTargetDefinition>,
    pub previews: Vec<PreviewPanelTargetDefinition>,
}

/// Read-only snapshot owned by VK's workspace/session/terminal/preview services.
/// The terminal service currently has no durable target definitions and therefore
/// deliberately publishes a ready-empty collection rather than omitting readiness.
pub async fn get_panel_target_authority(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<WorkspacePanelTargetAuthoritySnapshot>>, ApiError> {
    let pool = &deployment.db().pool;
    let workspace_id = workspace.id;
    let base = format!("/workspaces/{workspace_id}");
    let workspace_targets = HashMap::from([
        ("overview".into(), delivery(base.clone(), "craft-overview")),
        (
            "code".into(),
            delivery(format!("{base}/vscode"), "workspace-code"),
        ),
        (
            "changes".into(),
            delivery(format!("{base}?view=changes"), "workspace-changes"),
        ),
        (
            "beads".into(),
            delivery(format!("{base}?view=beads"), "workspace-beads"),
        ),
        (
            "forms".into(),
            delivery(format!("{base}?view=forms"), "workspace-forms"),
        ),
    ]);
    let sessions = Session::find_by_workspace_id(pool, workspace_id)
        .await?
        .into_iter()
        .map(|session| SessionPanelTargetDefinition {
            session_id: session.id,
            workspace_id,
            delivery: delivery(format!("{base}?session={}", session.id), "agent-session"),
        })
        .collect();

    let token = WorkspacePreviewToken::ensure(pool, workspace_id)
        .await?
        .token;
    let mut previews = Vec::new();
    for repo in WorkspaceRepo::find_repos_for_workspace(pool, workspace_id).await? {
        let repo_slug = RepoPreviewSlug::ensure(pool, repo.id, &repo.display_name)
            .await?
            .slug;
        for slot in PreviewSlot::find_by_repo_id(pool, repo.id).await? {
            previews.push(PreviewPanelTargetDefinition {
                preview_slot_id: slot.id,
                workspace_id,
                url_parts: PreviewSlotUrlParts {
                    preview_slot_id: slot.id,
                    workspace_token: token.clone(),
                    repo_slug: repo_slug.clone(),
                    slot_slug: slot.slot_slug,
                },
                customer_slug: "preview".into(),
                factory_key: "preview-slot".into(),
                available: slot.enabled,
            });
        }
    }
    previews.sort_by_key(|entry| entry.preview_slot_id);

    Ok(ResponseJson(ApiResponse::success(
        WorkspacePanelTargetAuthoritySnapshot {
            ready: true,
            workspace_id,
            workspace_targets,
            sessions,
            terminals_ready: true,
            terminals: Vec::new(),
            previews,
        },
    )))
}

fn delivery(location: String, factory_key: &str) -> PanelTargetDeliveryDefinition {
    PanelTargetDeliveryDefinition {
        location,
        factory_key: factory_key.into(),
        available: true,
    }
}
