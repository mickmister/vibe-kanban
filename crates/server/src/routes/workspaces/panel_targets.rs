use std::collections::HashMap;

use axum::{
    Extension, Router,
    extract::{Path, Request},
    http::StatusCode,
    middleware::{Next, from_fn},
    response::{Json as ResponseJson, Response},
    routing::get,
};
use db::models::{
    preview_slot::PreviewSlot, session::Session, workspace::Workspace,
    workspace_repo::WorkspaceRepo,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use ts_rs::TS;
use utils::response::ApiResponse;

use crate::error::ApiError;

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
    Extension(workspace): Extension<Workspace>,
    Extension(pool): Extension<SqlitePool>,
) -> Result<ResponseJson<ApiResponse<WorkspacePanelTargetAuthoritySnapshot>>, ApiError> {
    let workspace_id = workspace.id;
    let workspace_targets = HashMap::from([
        ("overview".into(), factory("craft-overview", true)),
        ("code".into(), factory("workspace-code", true)),
        ("changes".into(), factory("workspace-changes", true)),
        ("beads".into(), factory("workspace-beads", true)),
        ("forms".into(), factory("workspace-forms", true)),
    ]);
    let sessions = Session::find_by_workspace_id(&pool, workspace_id)
        .await?
        .into_iter()
        .map(|session| SessionPanelTargetDefinition {
            session_id: session.id,
            workspace_id,
            factory: factory("agent-session", true),
        })
        .collect();

    let mut previews = Vec::new();
    for repo in WorkspaceRepo::find_repos_for_workspace(&pool, workspace_id).await? {
        for slot in PreviewSlot::find_by_repo_id(&pool, repo.id).await? {
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

/// Production endpoint owner. The same DB-backed workspace loader is exercised
/// by integration tests; nonexistent and foreign IDs are indistinguishable.
pub fn router<S>(pool: SqlitePool) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/", get(get_panel_target_authority))
        .route_layer(from_fn(load_authorized_workspace))
        .layer(Extension(pool))
}

async fn load_authorized_workspace(
    Path(workspace_id): Path<uuid::Uuid>,
    Extension(pool): Extension<SqlitePool>,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let workspace = Workspace::find_by_id(&pool, workspace_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    request.extensions_mut().insert(workspace);
    Ok(next.run(request).await)
}

fn factory(factory_key: &str, available: bool) -> PanelTargetFactoryReference {
    PanelTargetFactoryReference {
        factory_key: factory_key.into(),
        available,
    }
}

#[cfg(test)]
mod tests {
    use axum::{body::to_bytes, http::Request};
    use sqlx::sqlite::SqlitePoolOptions;
    use tower::ServiceExt;

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

    #[tokio::test]
    async fn real_endpoint_enforces_workspace_loading_and_serializes_only_safe_authority() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("../db/migrations").run(&pool).await.unwrap();
        let workspace_id = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO workspaces (id, branch, name) VALUES (?, 'main', 'owned')")
            .bind(workspace_id)
            .execute(&pool)
            .await
            .unwrap();
        let repo_id = uuid::Uuid::new_v4();
        let session_id = uuid::Uuid::new_v4();
        let run_config_id = uuid::Uuid::new_v4();
        let disabled_preview_id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO repos (id, path, name, display_name) VALUES (?, '/repo', 'repo', 'Repo')",
        )
        .bind(repo_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO workspace_repos (id, workspace_id, repo_id, target_branch) VALUES (?, ?, ?, 'main')")
            .bind(uuid::Uuid::new_v4()).bind(workspace_id).bind(repo_id).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO run_configs (id, repo_id, slug, name, command) VALUES (?, ?, 'web', 'Web', 'true')")
            .bind(run_config_id).bind(repo_id).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO preview_slots (id, repo_id, run_config_id, slot_slug, title, enabled) VALUES (?, ?, ?, 'web', 'Web', 0)")
            .bind(disabled_preview_id).bind(repo_id).bind(run_config_id).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sessions (id, workspace_id) VALUES (?, ?)")
            .bind(session_id)
            .bind(workspace_id)
            .execute(&pool)
            .await
            .unwrap();
        let app = Router::new().nest(
            "/workspaces/{id}/panel-target-authority",
            router(pool.clone()),
        );

        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/workspaces/{workspace_id}/panel-target-authority"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(body.contains("\"terminalsReady\":true"));
        assert!(body.contains("\"terminals\":[]"));
        assert!(body.contains(&format!("\"previewSlotId\":\"{disabled_preview_id}\"")));
        assert!(
            body.contains(&format!("\"sessionId\":\"{session_id}\"")),
            "expected session identity in authority response: {body}"
        );
        assert!(body.contains("\"available\":false"));
        assert!(!body.contains("removed-preview"));
        assert!(!body.contains("removed-session"));
        for forbidden in [
            "workspaceToken",
            "password",
            "bearer",
            "secret",
            "credential",
            "location",
            "delivery",
            "urlParts",
        ] {
            assert!(
                !body.contains(forbidden),
                "leaked forbidden field {forbidden}: {body}"
            );
        }

        for unknown in [uuid::Uuid::new_v4().to_string(), "not-a-uuid".into()] {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("/workspaces/{unknown}/panel-target-authority"))
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST
            ));
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert!(!String::from_utf8_lossy(&body).contains(&workspace_id.to_string()));
        }

        pool.close().await;
        let response = app
            .oneshot(
                Request::get(format!("/workspaces/{workspace_id}/panel-target-authority"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
