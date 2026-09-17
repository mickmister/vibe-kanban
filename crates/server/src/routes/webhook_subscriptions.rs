use axum::{
    Json, Router,
    extract::{Path, State},
    response::Json as ResponseJson,
    routing::{get, post},
};
use deployment::Deployment;
use services::services::{
    config::save_config_to_file,
    webhook_notification::{
        CreateWebhookSubscription, UpdateWebhookSubscription, WebhookNotificationService,
        WebhookSubscriptionError,
    },
};
use utils::{assets::config_path, response::ApiResponse};
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError};

async fn list_subscriptions(
    State(deployment): State<DeploymentImpl>,
) -> ResponseJson<
    ApiResponse<Vec<services::services::webhook_notification::WebhookSubscriptionPublic>>,
> {
    let service = WebhookNotificationService::new(deployment.config().clone());
    ResponseJson(ApiResponse::success(service.list_subscriptions().await))
}

async fn get_subscription(
    State(deployment): State<DeploymentImpl>,
    Path(id): Path<Uuid>,
) -> Result<
    ResponseJson<ApiResponse<services::services::webhook_notification::WebhookSubscriptionPublic>>,
    ApiError,
> {
    let service = WebhookNotificationService::new(deployment.config().clone());
    let Some(subscription) = service.get_subscription(id).await else {
        return Err(ApiError::BadRequest(format!(
            "Webhook subscription {id} not found"
        )));
    };
    Ok(ResponseJson(ApiResponse::success(subscription)))
}

async fn create_subscription(
    State(deployment): State<DeploymentImpl>,
    Json(payload): Json<CreateWebhookSubscription>,
) -> Result<
    ResponseJson<
        ApiResponse<services::services::webhook_notification::UpsertWebhookSubscriptionResponse>,
    >,
    ApiError,
> {
    let service = WebhookNotificationService::new(deployment.config().clone());
    let result = service
        .create_or_upsert_subscription(payload)
        .await
        .map_err(subscription_error_to_api)?;
    persist_config(&deployment).await?;
    Ok(ResponseJson(ApiResponse::success(result)))
}

async fn update_subscription(
    State(deployment): State<DeploymentImpl>,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateWebhookSubscription>,
) -> Result<
    ResponseJson<ApiResponse<services::services::webhook_notification::WebhookSubscriptionPublic>>,
    ApiError,
> {
    let service = WebhookNotificationService::new(deployment.config().clone());
    let result = service
        .update_subscription(id, payload)
        .await
        .map_err(subscription_error_to_api)?;
    persist_config(&deployment).await?;
    Ok(ResponseJson(ApiResponse::success(result)))
}

async fn disable_subscription(
    State(deployment): State<DeploymentImpl>,
    Path(id): Path<Uuid>,
) -> Result<
    ResponseJson<ApiResponse<services::services::webhook_notification::WebhookSubscriptionPublic>>,
    ApiError,
> {
    let service = WebhookNotificationService::new(deployment.config().clone());
    let result = service
        .set_subscription_enabled(id, false)
        .await
        .map_err(subscription_error_to_api)?;
    persist_config(&deployment).await?;
    Ok(ResponseJson(ApiResponse::success(result)))
}

async fn persist_config(deployment: &DeploymentImpl) -> Result<(), ApiError> {
    let config = deployment.config().read().await.clone();
    save_config_to_file(&config, &config_path())
        .await
        .map_err(|error| {
            ApiError::BadRequest(format!(
                "Failed to save webhook subscription config: {error}"
            ))
        })
}

fn subscription_error_to_api(error: WebhookSubscriptionError) -> ApiError {
    match error {
        WebhookSubscriptionError::NotFound => ApiError::BadRequest(error.to_string()),
        _ => ApiError::BadRequest(error.to_string()),
    }
}

pub fn router() -> Router<DeploymentImpl> {
    Router::new()
        .route(
            "/webhook-subscriptions",
            get(list_subscriptions).post(create_subscription),
        )
        .route(
            "/webhook-subscriptions/{id}",
            get(get_subscription).put(update_subscription),
        )
        .route(
            "/webhook-subscriptions/{id}/disable",
            post(disable_subscription).put(disable_subscription),
        )
}
