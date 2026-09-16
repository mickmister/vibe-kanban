use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    middleware::from_fn_with_state,
    response::Json as ResponseJson,
    routing::get,
};
use db::models::{
    agent_message_queue::{AgentMessageQueueItem, AgentMessageSource},
    session::Session,
};
use deployment::Deployment;
use executors::{
    actions::{ExecutorActionProvenance, ExecutorActionProvenanceKind},
    profile::ExecutorConfig,
};
use serde::{Deserialize, Serialize};
use services::services::{container::ContainerService, queued_message::QueueStatus};
use ts_rs::TS;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError, middleware::load_session_middleware};

#[derive(Debug, Deserialize, TS)]
pub struct QueueMessageRequest {
    pub message: String,
    #[serde(default)]
    pub executor_config: Option<ExecutorConfig>,
    #[serde(default)]
    pub source: Option<AgentMessageSource>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub provenance: Option<ExecutorActionProvenance>,
    #[serde(default)]
    pub operation_key: Option<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct QueueMessageResponse {
    pub queued_item: AgentMessageQueueItem,
    pub status: QueueStatus,
}

/// Queue-aware guarded follow-up path. The legacy `/follow-up` endpoint remains
/// immediate-start for compatibility; MCP/VD callers should migrate here when
/// they want VK scheduler concurrency and workspace-exclusivity guardrails.
async fn queue_message(
    Extension(session): Extension<Session>,
    State(deployment): State<DeploymentImpl>,
    Json(payload): Json<QueueMessageRequest>,
) -> Result<ResponseJson<ApiResponse<QueueMessageResponse>>, ApiError> {
    if let Some(message) = super::invalid_session_command_message(&payload.message) {
        return Err(ApiError::BadRequest(message));
    }
    let session_command = super::parse_session_command(&payload.message);
    let source = payload.source.unwrap_or(AgentMessageSource::FromUser);
    if payload.operation_key.as_deref().is_some_and(|key| {
        key.len() > 160
            || key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._:-".contains(c))
    }) {
        return Err(ApiError::BadRequest(
            "invalid queue operation identity".to_string(),
        ));
    }
    let queued_item = deployment
        .queued_message_service()
        .queue_message(
            &session,
            payload.message,
            session_command,
            source,
            payload.priority,
            payload
                .provenance
                .or_else(|| default_provenance_for_source(source)),
            payload.executor_config,
            payload.operation_key,
        )
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    deployment
        .track_if_analytics_allowed(
            "follow_up_queued",
            serde_json::json!({
                "session_id": session.id.to_string(),
                "workspace_id": session.workspace_id.to_string(),
            }),
        )
        .await;

    if let Err(e) = deployment
        .container()
        .try_start_queued_messages(deployment.queued_message_service())
        .await
    {
        tracing::warn!("Failed to pump queued messages after enqueue: {}", e);
    }

    let delayed_deployment = deployment.clone();
    tokio::spawn(async move {
        for delay_ms in [500_u64, 1_500, 3_000] {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            if let Err(e) = delayed_deployment
                .container()
                .try_start_queued_messages(delayed_deployment.queued_message_service())
                .await
            {
                tracing::warn!("Failed delayed queue pump after enqueue: {}", e);
            }
        }
    });

    let status = deployment
        .queued_message_service()
        .get_status(session.id)
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    Ok(ResponseJson(ApiResponse::success(QueueMessageResponse {
        queued_item,
        status,
    })))
}

fn default_provenance_for_source(source: AgentMessageSource) -> Option<ExecutorActionProvenance> {
    match source {
        AgentMessageSource::FromUser => None,
        AgentMessageSource::Workflow => Some(ExecutorActionProvenance {
            kind: ExecutorActionProvenanceKind::Workflow,
            label: "Workflow automation".to_string(),
            workflow_run_id: None,
            workflow_role_id: None,
            workflow_name: None,
            workflow_design_id: None,
            workflow_version: None,
        }),
        AgentMessageSource::Agent => Some(ExecutorActionProvenance {
            kind: ExecutorActionProvenanceKind::Agent,
            label: "Agent".to_string(),
            workflow_run_id: None,
            workflow_role_id: None,
            workflow_name: None,
            workflow_design_id: None,
            workflow_version: None,
        }),
        AgentMessageSource::System => Some(ExecutorActionProvenance {
            kind: ExecutorActionProvenanceKind::System,
            label: "System".to_string(),
            workflow_run_id: None,
            workflow_role_id: None,
            workflow_name: None,
            workflow_design_id: None,
            workflow_version: None,
        }),
    }
}

async fn cancel_queued_messages(
    Extension(session): Extension<Session>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<QueueStatus>>, ApiError> {
    deployment
        .queued_message_service()
        .cancel_queued(session.id)
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    deployment
        .track_if_analytics_allowed(
            "follow_up_queue_cancelled",
            serde_json::json!({
                "session_id": session.id.to_string(),
                "workspace_id": session.workspace_id.to_string(),
            }),
        )
        .await;

    get_queue_status(Extension(session), State(deployment)).await
}

async fn cancel_queued_message_by_id(
    Extension(session): Extension<Session>,
    State(deployment): State<DeploymentImpl>,
    Path(queue_item_id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<QueueStatus>>, ApiError> {
    deployment
        .queued_message_service()
        .cancel_queued_item(session.id, queue_item_id)
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    get_queue_status(Extension(session), State(deployment)).await
}

async fn get_queue_status(
    Extension(session): Extension<Session>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<QueueStatus>>, ApiError> {
    let status = deployment
        .queued_message_service()
        .get_status(session.id)
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    Ok(ResponseJson(ApiResponse::success(status)))
}

async fn get_by_operation_key(
    Extension(session): Extension<Session>,
    State(deployment): State<DeploymentImpl>,
    Path((_session_id, operation_key)): Path<(Uuid, String)>,
) -> Result<ResponseJson<ApiResponse<Option<AgentMessageQueueItem>>>, ApiError> {
    if operation_key.len() > 160
        || operation_key.is_empty()
        || !operation_key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:-".contains(c))
    {
        return Err(ApiError::BadRequest(
            "invalid queue operation identity".to_string(),
        ));
    }
    let item = deployment
        .queued_message_service()
        .find_by_operation_key(session.id, &operation_key)
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    Ok(ResponseJson(ApiResponse::success(item)))
}

pub(super) fn router(deployment: &DeploymentImpl) -> Router<DeploymentImpl> {
    Router::new()
        .route(
            "/",
            get(get_queue_status)
                .post(queue_message)
                .delete(cancel_queued_messages),
        )
        .route(
            "/{queue_item_id}",
            axum::routing::delete(cancel_queued_message_by_id),
        )
        .route("/operations/{operation_key}", get(get_by_operation_key))
        .layer(from_fn_with_state(
            deployment.clone(),
            load_session_middleware,
        ))
}

#[cfg(test)]
mod tests {
    use axum::{Router, body::Body};
    use db::models::{
        agent_message_queue::{CreateAgentMessageQueueItem, QueuedFollowUpData},
        session::CreateSession,
    };
    use http::{Request, StatusCode};
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn operation_lookup_route_returns_complete_identity_and_terminal_status() {
        let deployment = DeploymentImpl::new(CancellationToken::new()).await.unwrap();
        let workspace_id = Uuid::new_v4();
        sqlx::query("INSERT INTO workspaces(id,branch) VALUES(?1,'queue-route-test')")
            .bind(workspace_id)
            .execute(&deployment.db().pool)
            .await
            .unwrap();
        let session = Session::create(
            &deployment.db().pool,
            &CreateSession {
                executor: Some("CODEX".into()),
                name: Some("Dev".into()),
            },
            Uuid::new_v4(),
            workspace_id,
        )
        .await
        .unwrap();
        let operation_key = format!("native-turn:{}", Uuid::new_v4());
        let item = AgentMessageQueueItem::create(
            &deployment.db().pool,
            &CreateAgentMessageQueueItem {
                session_id: session.id,
                workspace_id,
                source: AgentMessageSource::Workflow,
                priority: Some(60),
                data: QueuedFollowUpData {
                    message: "Do the task.".into(),
                    executor_config: Some(ExecutorConfig {
                        executor: executors::executors::BaseCodingAgent::Codex,
                        variant: None,
                        model_id: Some("gpt-5.3-codex".into()),
                        agent_id: None,
                        reasoning_id: Some("high".into()),
                        permission_policy: None,
                    }),
                    session_command: None,
                    provenance: Some(ExecutorActionProvenance {
                        kind: ExecutorActionProvenanceKind::Workflow,
                        label: "Native workflow role turn".to_string(),
                        workflow_run_id: Some("native-run-1".to_string()),
                        workflow_role_id: Some("dev".to_string()),
                        workflow_name: None,
                        workflow_design_id: None,
                        workflow_version: None,
                    }),
                    operation_key: Some(operation_key.clone()),
                },
            },
            Uuid::new_v4(),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE agent_message_queue SET status='failed' WHERE id=?1")
            .bind(item.id)
            .execute(&deployment.db().pool)
            .await
            .unwrap();
        let app = Router::new()
            .nest("/sessions/{session_id}/queue", router(&deployment))
            .with_state(deployment);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/sessions/{}/queue/operations/{operation_key}",
                        session.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let data = &value["data"];
        assert_eq!(data["session_id"], session.id.to_string());
        assert_eq!(data["workspace_id"], workspace_id.to_string());
        assert_eq!(data["status"], "failed");
        assert_eq!(data["source"], "workflow");
        assert_eq!(data["priority"], 60);
        assert_eq!(data["data"]["message"], "Do the task.");
        assert_eq!(data["data"]["operation_key"], operation_key);
        assert_eq!(data["data"]["executor_config"]["model_id"], "gpt-5.3-codex");
        assert_eq!(data["data"]["executor_config"]["reasoning_id"], "high");
        assert_eq!(data["data"]["provenance"]["kind"], "workflow");
        assert_eq!(data["data"]["provenance"]["workflow_role_id"], "dev");
    }
}
