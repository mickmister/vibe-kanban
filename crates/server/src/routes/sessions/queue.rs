use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    middleware::from_fn_with_state,
    response::Json as ResponseJson,
    routing::get,
};
use db::models::{agent_message_queue::AgentMessageSource, session::Session};
use deployment::Deployment;
use serde::Deserialize;
use services::services::{container::ContainerService, queued_message::QueueStatus};
use ts_rs::TS;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError, middleware::load_session_middleware};

#[derive(Debug, Deserialize, TS)]
pub struct QueueMessageRequest {
    pub message: String,
    #[serde(default)]
    pub source: Option<AgentMessageSource>,
    #[serde(default)]
    pub priority: Option<i64>,
}

/// Queue-aware guarded follow-up path. The legacy `/follow-up` endpoint remains
/// immediate-start for compatibility; MCP/VD callers should migrate here when
/// they want VK scheduler concurrency and workspace-exclusivity guardrails.
async fn queue_message(
    Extension(session): Extension<Session>,
    State(deployment): State<DeploymentImpl>,
    Json(payload): Json<QueueMessageRequest>,
) -> Result<ResponseJson<ApiResponse<QueueStatus>>, ApiError> {
    if let Some(message) = super::invalid_session_command_message(&payload.message) {
        return Err(ApiError::BadRequest(message));
    }
    let session_command = super::parse_session_command(&payload.message);
    deployment
        .queued_message_service()
        .queue_message(
            &session,
            payload.message,
            session_command,
            payload.source.unwrap_or(AgentMessageSource::FromUser),
            payload.priority,
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

    get_queue_status(Extension(session), State(deployment)).await
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
        .layer(from_fn_with_state(
            deployment.clone(),
            load_session_middleware,
        ))
}
