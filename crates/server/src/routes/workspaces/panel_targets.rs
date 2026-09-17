use std::collections::HashMap;

use axum::{Extension, extract::State, response::Json as ResponseJson};
use db::models::{
    preview_slot::PreviewSlot, session::Session, workspace::Workspace,
    workspace_repo::WorkspaceRepo,
};
use deployment::Deployment;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utils::response::ApiResponse;

use crate::{DeploymentImpl, error::ApiError};

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PanelTargetFactoryReference {
    pub factory_key: String,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionPanelTargetDefinition {
    pub session_id: uuid::Uuid,
    pub workspace_id: uuid::Uuid,
    pub factory: PanelTargetFactoryReference,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TerminalPanelTargetDefinition {
    pub terminal_id: String,
    pub workspace_id: uuid::Uuid,
    pub factory: PanelTargetFactoryReference,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPanelTargetDefinition {
    pub preview_slot_id: uuid::Uuid,
    pub workspace_id: uuid::Uuid,
    pub factory_key: String,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePanelTargetAuthoritySnapshot {
    pub ready: bool,
    pub workspace_id: uuid::Uuid,
    pub workspace_targets: HashMap<String, PanelTargetFactoryReference>,
    pub sessions: Vec<SessionPanelTargetDefinition>,
    pub terminals_ready: bool,
    pub terminals: Vec<TerminalPanelTargetDefinition>,
    pub previews: Vec<PreviewPanelTargetDefinition>,
}

/// Read-only snapshot owned by VK's workspace/session/terminal/preview services.
/// The terminal service currently has no durable target definitions and therefore
/// deliberately publishes a ready-empty collection rather than omitting readiness.
pub async fn get_panel_target_authority(
    State(deployment): State<DeploymentImpl>,
    Extension(workspace): Extension<Workspace>,
) -> Result<ResponseJson<ApiResponse<WorkspacePanelTargetAuthoritySnapshot>>, ApiError> {
    let pool = &deployment.db().pool;
    let workspace_id = workspace.id;
    if workspace.archived || workspace.worktree_deleted {
        return Ok(ResponseJson(ApiResponse::success(
            WorkspacePanelTargetAuthoritySnapshot {
                ready: false,
                workspace_id,
                workspace_targets: HashMap::new(),
                sessions: Vec::new(),
                terminals_ready: false,
                terminals: Vec::new(),
                previews: Vec::new(),
            },
        )));
    }
    let workspace_targets = HashMap::from([
        ("overview".into(), factory("craft-overview", true)),
        ("code".into(), factory("workspace-code", true)),
        ("changes".into(), factory("workspace-changes", true)),
        ("beads".into(), factory("workspace-beads", true)),
        ("forms".into(), factory("workspace-forms", true)),
    ]);
    let sessions = Session::find_by_workspace_id(pool, workspace_id)
        .await?
        .into_iter()
        .map(|session| SessionPanelTargetDefinition {
            session_id: session.id,
            workspace_id,
            factory: factory("agent-session", true),
        })
        .collect();

    let mut previews = Vec::new();
    for repo in WorkspaceRepo::find_repos_for_workspace(pool, workspace_id).await? {
        for slot in PreviewSlot::find_by_repo_id(pool, repo.id).await? {
            previews.push(PreviewPanelTargetDefinition {
                preview_slot_id: slot.id,
                workspace_id,
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

fn factory(factory_key: &str, available: bool) -> PanelTargetFactoryReference {
    PanelTargetFactoryReference {
        factory_key: factory_key.into(),
        available,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_authority_contract_omits_secrets_and_locations() {
        let workspace_id = uuid::Uuid::new_v4();
        let preview_slot_id = uuid::Uuid::new_v4();
        let value = serde_json::to_value(WorkspacePanelTargetAuthoritySnapshot {
            ready: true,
            workspace_id,
            workspace_targets: HashMap::from([("code".into(), factory("workspace-code", true))]),
            sessions: vec![SessionPanelTargetDefinition {
                session_id: uuid::Uuid::new_v4(),
                workspace_id,
                factory: factory("agent-session", true),
            }],
            terminals_ready: true,
            terminals: Vec::new(),
            previews: vec![PreviewPanelTargetDefinition {
                preview_slot_id,
                workspace_id,
                factory_key: "preview-slot".into(),
                available: true,
            }],
        })
        .unwrap();
        let serialized = serde_json::to_string(&value).unwrap();
        assert!(!serialized.contains("workspaceToken"));
        assert!(!serialized.contains("urlParts"));
        assert!(!serialized.contains("location"));
        assert_eq!(
            value["previews"][0]["previewSlotId"],
            preview_slot_id.to_string()
        );
        assert_eq!(value["terminalsReady"], true);
        assert_eq!(value["terminals"], serde_json::json!([]));
    }

    #[test]
    fn factory_references_are_stable_and_availability_is_explicit() {
        assert_eq!(
            serde_json::to_value(factory("agent-session", false)).unwrap(),
            serde_json::json!({"factoryKey":"agent-session","available":false})
        );
    }
}
