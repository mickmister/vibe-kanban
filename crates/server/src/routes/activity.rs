use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, Query, State, ws::Message},
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use db::{
    DBService,
    models::{
        agent_message_queue::{AgentMessageQueueStatus, AgentMessageSource, QueuedFollowUpData},
        execution_process::{ExecutionProcessRunReason, ExecutionProcessStatus},
        workflow_callback_registry::{
            UpdateWorkflowCallbackRegistryStatus, UpsertWorkflowCallbackRegistryItem,
            WorkflowCallbackKind, WorkflowCallbackRegistryItem, WorkflowCallbackStatus,
        },
    },
};
use deployment::Deployment;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Row, SqlitePool};
use ts_rs::TS;
use utils::{log_msg::LogMsg, response::ApiResponse};
use uuid::Uuid;

use crate::{
    DeploymentImpl,
    error::ApiError,
    middleware::signed_ws::{MaybeSignedWebSocket, SignedWsUpgrade},
};

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActivitySnapshot {
    pub generated_at: DateTime<Utc>,
    pub callback_state_available: bool,
    pub workspaces: Vec<ActivityWorkspace>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActivityWorkspace {
    pub workspace_id: Uuid,
    pub active_turn_count: usize,
    pub running_turn_count: usize,
    pub running_dev_server_count: usize,
    pub queued_count: usize,
    pub sessions: Vec<ActivitySession>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(use_ts_enum)]
#[ts(export)]
pub enum ActivitySessionStatus {
    Idle,
    Queued,
    Running,
    CallbackWaiting,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActivitySession {
    pub workspace_id: Uuid,
    pub session_id: Uuid,
    pub status: ActivitySessionStatus,
    pub active_turn_count: usize,
    pub running_execution_processes: Vec<ActivityExecutionProcess>,
    pub queue: ActivityQueueSummary,
    pub callback: ActivityCallbackSummary,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActivityExecutionProcess {
    pub execution_process_id: Uuid,
    pub run_reason: ExecutionProcessRunReason,
    pub status: ExecutionProcessStatus,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActivityQueueSummary {
    pub count: usize,
    pub queued_count: usize,
    pub leased_count: usize,
    pub starting_count: usize,
    pub running_count: usize,
    pub first_item_id: Option<Uuid>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActivityCallbackSummary {
    pub available: bool,
    pub waiting_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActivityV1Snapshot {
    pub schema_version: String,
    pub generated_at: DateTime<Utc>,
    pub scope: ActivityV1Scope,
    pub summary: ActivityV1Summary,
    pub workspaces: Vec<ActivityV1Workspace>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActivityV1Scope {
    pub workspace_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, Default)]
#[ts(export)]
pub struct ActivityV1Summary {
    pub active_turn_count: usize,
    pub pending_turn_count: usize,
    pub callback_waiting_count: usize,
    pub recent_callback_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActivityV1Workspace {
    pub subject: ActivityV1Subject,
    pub summary: ActivityV1Summary,
    pub sessions: Vec<ActivityV1Session>,
    pub links: Vec<ActivityV1Link>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActivityV1Session {
    pub subject: ActivityV1Subject,
    pub status: ActivityV1SessionStatus,
    pub summary_text: String,
    pub summary: ActivityV1Summary,
    pub callbacks: Vec<ActivityV1Callback>,
    pub links: Vec<ActivityV1Link>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActivityV1Callback {
    pub callback_id: String,
    pub kind: ActivityV1CallbackKind,
    pub status: ActivityV1CallbackStatus,
    pub summary_text: String,
    pub workflow: Option<ActivityV1WorkflowRef>,
    pub links: Vec<ActivityV1Link>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActivityV1WorkflowRef {
    pub run_id: Option<String>,
    pub name: Option<String>,
    pub design_id: Option<String>,
    pub version: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActivityV1Subject {
    pub kind: ActivityV1SubjectKind,
    pub id: String,
    pub workspace_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(use_ts_enum)]
#[ts(export)]
pub enum ActivityV1SubjectKind {
    Workspace,
    Session,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(use_ts_enum)]
#[ts(export)]
pub enum ActivityV1SessionStatus {
    Idle,
    Pending,
    Active,
    WaitingForCallback,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(use_ts_enum)]
#[ts(export)]
pub enum ActivityV1CallbackKind {
    WorkflowCompletion,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(use_ts_enum)]
#[ts(export)]
pub enum ActivityV1CallbackStatus {
    Waiting,
    Delivered,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActivityV1Link {
    pub rel: String,
    pub href: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActivityV1WsEvent {
    pub schema_version: String,
    pub event_id: String,
    pub cursor: String,
    pub event_type: ActivityV1WsEventType,
    pub generated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<ActivityV1Snapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(use_ts_enum)]
#[ts(export)]
pub enum ActivityV1WsEventType {
    Snapshot,
    RefreshSnapshot,
    Heartbeat,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ActivityV1WsQuery {
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
pub struct UpsertWorkflowCallbackRequest {
    pub callback_key: String,
    pub workspace_id: Uuid,
    pub target_session_id: Uuid,
    #[serde(default = "default_workflow_callback_kind")]
    pub kind: WorkflowCallbackKind,
    pub workflow_run_id: String,
    #[serde(default)]
    pub workflow_name: Option<String>,
    #[serde(default)]
    pub workflow_design_id: Option<String>,
    #[serde(default)]
    pub workflow_version: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
pub struct UpdateWorkflowCallbackStatusRequest {
    pub status: WorkflowCallbackStatus,
    #[serde(default)]
    pub delivered_ref: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
}

fn default_workflow_callback_kind() -> WorkflowCallbackKind {
    WorkflowCallbackKind::WorkflowCompletion
}

#[derive(Debug, Clone, Default)]
pub struct ActivityV1Filters {
    pub workspace_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
}

impl ActivityV1Filters {
    fn from_query(query: &HashMap<String, String>) -> Self {
        Self {
            workspace_id: query
                .get("workspace_id")
                .and_then(|value| Uuid::parse_str(value).ok()),
            session_id: query
                .get("session_id")
                .and_then(|value| Uuid::parse_str(value).ok()),
        }
    }
}

#[derive(Debug)]
struct RunningProcessRow {
    workspace_id: Uuid,
    session_id: Uuid,
    execution_process_id: Uuid,
    run_reason: ExecutionProcessRunReason,
    status: ExecutionProcessStatus,
    started_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug)]
struct QueueRow {
    workspace_id: Uuid,
    session_id: Uuid,
    queue_item_id: Uuid,
    status: AgentMessageQueueStatus,
    updated_at: DateTime<Utc>,
}

#[derive(Debug)]
struct QueueActivityRow {
    workspace_id: Uuid,
    session_id: Uuid,
    status: AgentMessageQueueStatus,
    source: AgentMessageSource,
    data: QueuedFollowUpData,
    queued_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug)]
struct SessionAccumulator {
    workspace_id: Uuid,
    session_id: Uuid,
    running_execution_processes: Vec<ActivityExecutionProcess>,
    queue: ActivityQueueSummary,
    callback: ActivityCallbackSummary,
    updated_at: DateTime<Utc>,
}

impl SessionAccumulator {
    fn new(workspace_id: Uuid, session_id: Uuid, updated_at: DateTime<Utc>) -> Self {
        Self {
            workspace_id,
            session_id,
            running_execution_processes: Vec::new(),
            queue: ActivityQueueSummary {
                count: 0,
                queued_count: 0,
                leased_count: 0,
                starting_count: 0,
                running_count: 0,
                first_item_id: None,
                updated_at: None,
            },
            callback: ActivityCallbackSummary {
                available: false,
                waiting_count: 0,
            },
            updated_at,
        }
    }

    fn to_session(&self) -> ActivitySession {
        let active_turn_count = self
            .running_execution_processes
            .iter()
            .filter(|process| process.run_reason != ExecutionProcessRunReason::DevServer)
            .count();
        let status = if active_turn_count > 0 {
            ActivitySessionStatus::Running
        } else if self.callback.waiting_count > 0 {
            ActivitySessionStatus::CallbackWaiting
        } else if self.queue.count > 0 {
            ActivitySessionStatus::Queued
        } else {
            ActivitySessionStatus::Idle
        };
        ActivitySession {
            workspace_id: self.workspace_id,
            session_id: self.session_id,
            status,
            active_turn_count,
            running_execution_processes: self.running_execution_processes.clone(),
            queue: self.queue.clone(),
            callback: self.callback.clone(),
            updated_at: self.updated_at,
        }
    }
}

#[derive(Debug)]
struct WorkspaceAccumulator {
    workspace_id: Uuid,
    sessions: BTreeMap<Uuid, SessionAccumulator>,
    updated_at: DateTime<Utc>,
}

pub fn router(deployment: &DeploymentImpl) -> Router<DeploymentImpl> {
    Router::new()
        .route("/activity", get(get_activity_snapshot))
        .route("/activity/v1", get(get_activity_v1_snapshot))
        .route(
            "/activity/v1/workflow-callbacks",
            post(upsert_workflow_callback),
        )
        .route(
            "/activity/v1/workflow-callbacks/{callback_key}/status",
            post(update_workflow_callback_status),
        )
        .route("/activity/ws", get(stream_activity_ws))
        .route("/activity/v1/ws", get(stream_activity_v1_ws))
        .with_state(deployment.clone())
}

async fn get_activity_snapshot(
    State(deployment): State<DeploymentImpl>,
) -> Result<axum::Json<ApiResponse<ActivitySnapshot>>, ApiError> {
    let snapshot = build_activity_snapshot(deployment.db()).await?;
    Ok(axum::Json(ApiResponse::success(snapshot)))
}

async fn get_activity_v1_snapshot(
    Query(query): Query<HashMap<String, String>>,
    State(deployment): State<DeploymentImpl>,
) -> Result<axum::Json<ApiResponse<ActivityV1Snapshot>>, ApiError> {
    let filters = ActivityV1Filters::from_query(&query);
    let snapshot = build_activity_v1_snapshot(deployment.db(), &filters).await?;
    Ok(axum::Json(ApiResponse::success(snapshot)))
}

async fn upsert_workflow_callback(
    State(deployment): State<DeploymentImpl>,
    Json(payload): Json<UpsertWorkflowCallbackRequest>,
) -> Result<axum::Json<ApiResponse<WorkflowCallbackRegistryItem>>, ApiError> {
    validate_callback_key(&payload.callback_key)?;
    validate_callback_ref(&payload.workflow_run_id, "workflow_run_id")?;
    if let Some(design_id) = &payload.workflow_design_id {
        validate_callback_ref(design_id, "workflow_design_id")?;
    }
    ensure_session_in_workspace(
        &deployment.db().pool,
        payload.target_session_id,
        payload.workspace_id,
    )
    .await?;
    let input = UpsertWorkflowCallbackRegistryItem {
        callback_key: payload.callback_key,
        workspace_id: payload.workspace_id,
        target_session_id: payload.target_session_id,
        kind: payload.kind,
        workflow_run_id: payload.workflow_run_id,
        workflow_name: payload
            .workflow_name
            .map(|value| scrub_product_text(&value, "Workflow", 160)),
        workflow_design_id: payload.workflow_design_id,
        workflow_version: payload.workflow_version,
    };
    let item = WorkflowCallbackRegistryItem::upsert_pending(&deployment.db().pool, &input)
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    deployment.queued_message_service().notify();
    Ok(axum::Json(ApiResponse::success(item)))
}

async fn update_workflow_callback_status(
    Path(callback_key): Path<String>,
    State(deployment): State<DeploymentImpl>,
    Json(payload): Json<UpdateWorkflowCallbackStatusRequest>,
) -> Result<axum::Json<ApiResponse<WorkflowCallbackRegistryItem>>, ApiError> {
    validate_callback_key(&callback_key)?;
    let input = UpdateWorkflowCallbackRegistryStatus {
        callback_key,
        status: payload.status,
        delivered_ref: payload
            .delivered_ref
            .as_deref()
            .map(|value| scrub_identifier(value, 160)),
        error_message: payload
            .error_message
            .as_deref()
            .map(|value| scrub_product_text(value, "Callback status changed", 300)),
    };
    let item = WorkflowCallbackRegistryItem::update_status(&deployment.db().pool, &input)
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    deployment.queued_message_service().notify();
    Ok(axum::Json(ApiResponse::success(item)))
}

async fn stream_activity_v1_ws(
    ws: SignedWsUpgrade,
    Query(query): Query<ActivityV1WsQuery>,
    State(deployment): State<DeploymentImpl>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        if let Err(error) = handle_activity_v1_ws(socket, deployment, query).await {
            tracing::warn!("activity v1 WS closed: {}", error);
        }
    })
}

async fn handle_activity_v1_ws(
    mut socket: MaybeSignedWebSocket,
    deployment: DeploymentImpl,
    query: ActivityV1WsQuery,
) -> anyhow::Result<()> {
    let filters = ActivityV1Filters {
        workspace_id: query.workspace_id,
        session_id: query.session_id,
    };
    if query
        .cursor
        .as_deref()
        .is_some_and(|cursor| !cursor.is_empty())
    {
        send_activity_v1_refresh_snapshot(&mut socket).await?;
    }
    send_activity_v1_snapshot(&mut socket, deployment.db(), &filters).await?;

    let mut db_events = deployment.events().msg_store().get_receiver();
    let queue_notifier = deployment.queued_message_service().notifier();
    let mut heartbeat = tokio::time::interval(activity_v1_heartbeat_interval());
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if send_activity_v1_heartbeat(&mut socket).await.is_err() {
                    break;
                }
            }
            event = db_events.recv() => {
                match event {
                    Ok(msg) if activity_relevant_msg(&msg) => {
                        if send_activity_v1_snapshot(&mut socket, deployment.db(), &filters).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if send_activity_v1_refresh_snapshot(&mut socket).await.is_err()
                            || send_activity_v1_snapshot(&mut socket, deployment.db(), &filters).await.is_err()
                        {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = queue_notifier.notified() => {
                if send_activity_v1_snapshot(&mut socket, deployment.db(), &filters).await.is_err() {
                    break;
                }
            }
            inbound = socket.recv() => {
                match inbound {
                    Ok(Some(Message::Close(_))) => break,
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        }
    }

    let _ = socket.close().await;
    Ok(())
}

async fn send_activity_v1_snapshot(
    socket: &mut MaybeSignedWebSocket,
    db: &DBService,
    filters: &ActivityV1Filters,
) -> anyhow::Result<()> {
    let snapshot = build_activity_v1_snapshot(db, filters).await?;
    send_activity_v1_event(socket, activity_v1_snapshot_event(snapshot)).await
}

async fn send_activity_v1_refresh_snapshot(
    socket: &mut MaybeSignedWebSocket,
) -> anyhow::Result<()> {
    send_activity_v1_event(socket, activity_v1_refresh_snapshot_event()).await
}

async fn send_activity_v1_heartbeat(socket: &mut MaybeSignedWebSocket) -> anyhow::Result<()> {
    send_activity_v1_event(socket, activity_v1_heartbeat_event()).await
}

async fn send_activity_v1_event(
    socket: &mut MaybeSignedWebSocket,
    event: ActivityV1WsEvent,
) -> anyhow::Result<()> {
    socket
        .send(Message::Text(serde_json::to_string(&event)?.into()))
        .await?;
    Ok(())
}

fn activity_v1_snapshot_event(snapshot: ActivityV1Snapshot) -> ActivityV1WsEvent {
    let generated_at = snapshot.generated_at;
    ActivityV1WsEvent {
        schema_version: "activity.v1.ws".to_string(),
        event_id: activity_v1_cursor("snapshot", generated_at),
        cursor: activity_v1_cursor("snapshot", generated_at),
        event_type: ActivityV1WsEventType::Snapshot,
        generated_at,
        snapshot: Some(snapshot),
        reason: None,
    }
}

fn activity_v1_refresh_snapshot_event() -> ActivityV1WsEvent {
    let generated_at = Utc::now();
    ActivityV1WsEvent {
        schema_version: "activity.v1.ws".to_string(),
        event_id: activity_v1_cursor("refresh", generated_at),
        cursor: activity_v1_cursor("refresh", generated_at),
        event_type: ActivityV1WsEventType::RefreshSnapshot,
        generated_at,
        snapshot: None,
        reason: Some(
            "Replay is not available for this activity cursor. Refresh the activity snapshot."
                .to_string(),
        ),
    }
}

fn activity_v1_heartbeat_event() -> ActivityV1WsEvent {
    let generated_at = Utc::now();
    ActivityV1WsEvent {
        schema_version: "activity.v1.ws".to_string(),
        event_id: activity_v1_cursor("heartbeat", generated_at),
        cursor: activity_v1_cursor("heartbeat", generated_at),
        event_type: ActivityV1WsEventType::Heartbeat,
        generated_at,
        snapshot: None,
        reason: None,
    }
}

fn activity_v1_cursor(prefix: &str, generated_at: DateTime<Utc>) -> String {
    format!("{prefix}:{}", generated_at.timestamp_millis())
}

fn activity_v1_heartbeat_interval() -> Duration {
    Duration::from_secs(15)
}

async fn stream_activity_ws(
    ws: SignedWsUpgrade,
    State(deployment): State<DeploymentImpl>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        if let Err(error) = handle_activity_ws(socket, deployment).await {
            tracing::warn!("activity WS closed: {}", error);
        }
    })
}

async fn handle_activity_ws(
    mut socket: MaybeSignedWebSocket,
    deployment: DeploymentImpl,
) -> anyhow::Result<()> {
    send_activity_snapshot(&mut socket, deployment.db()).await?;
    socket.send(LogMsg::Ready.to_ws_message_unchecked()).await?;

    let mut db_events = deployment.events().msg_store().get_receiver();
    let queue_notifier = deployment.queued_message_service().notifier();

    loop {
        tokio::select! {
            event = db_events.recv() => {
                match event {
                    Ok(msg) if activity_relevant_msg(&msg) => {
                        if send_activity_snapshot(&mut socket, deployment.db()).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = queue_notifier.notified() => {
                if send_activity_snapshot(&mut socket, deployment.db()).await.is_err() {
                    break;
                }
            }
            inbound = socket.recv() => {
                match inbound {
                    Ok(Some(Message::Close(_))) => break,
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        }
    }

    let _ = socket.close().await;
    Ok(())
}

async fn send_activity_snapshot(
    socket: &mut MaybeSignedWebSocket,
    db: &DBService,
) -> anyhow::Result<()> {
    let snapshot = build_activity_snapshot(db).await?;
    socket
        .send(activity_snapshot_msg(snapshot).to_ws_message_unchecked())
        .await?;
    Ok(())
}

fn activity_snapshot_msg(snapshot: ActivitySnapshot) -> LogMsg {
    LogMsg::JsonPatch(
        serde_json::from_value(json!([{
            "op": "replace",
            "path": "/activity",
            "value": snapshot
        }]))
        .expect("Activity snapshot patch should be valid"),
    )
}

fn activity_relevant_msg(msg: &LogMsg) -> bool {
    let LogMsg::JsonPatch(patch) = msg else {
        return false;
    };
    patch.0.iter().any(|op| {
        let path = op.path();
        path.starts_with("/execution_processes") || path.starts_with("/workspaces")
    })
}

pub async fn build_activity_snapshot(db: &DBService) -> Result<ActivitySnapshot, sqlx::Error> {
    build_activity_snapshot_from_pool(&db.pool).await
}

pub async fn build_activity_v1_snapshot(
    db: &DBService,
    filters: &ActivityV1Filters,
) -> Result<ActivityV1Snapshot, sqlx::Error> {
    build_activity_v1_snapshot_from_pool(&db.pool, filters).await
}

async fn build_activity_v1_snapshot_from_pool(
    pool: &SqlitePool,
    filters: &ActivityV1Filters,
) -> Result<ActivityV1Snapshot, sqlx::Error> {
    let legacy = build_activity_snapshot_from_pool(pool).await?;
    let registry_rows = WorkflowCallbackRegistryItem::list_recent(
        pool,
        filters.workspace_id,
        filters.session_id,
        100,
    )
    .await?;
    let registered_keys = registry_rows
        .iter()
        .map(|row| (row.workflow_run_id.clone(), row.target_session_id))
        .collect::<std::collections::BTreeSet<_>>();
    let callback_rows = load_workflow_callback_rows(pool, filters).await?;
    let mut callbacks_by_session = BTreeMap::<(Uuid, Uuid), Vec<ActivityV1Callback>>::new();

    for row in registry_rows {
        let workspace_id = row.workspace_id;
        let session_id = row.target_session_id;
        callbacks_by_session
            .entry((workspace_id, session_id))
            .or_default()
            .push(callback_from_registry(row));
    }

    for (index, row) in callback_rows.into_iter().enumerate() {
        if row
            .data
            .provenance
            .as_ref()
            .and_then(|provenance| provenance.workflow_run_id.as_ref())
            .is_some_and(|run_id| registered_keys.contains(&(run_id.clone(), row.session_id)))
        {
            continue;
        }
        if let Some(callback) = workflow_callback_from_row(row, index) {
            if filters
                .workspace_id
                .is_some_and(|workspace_id| callback.0 != workspace_id)
                || filters
                    .session_id
                    .is_some_and(|session_id| callback.1 != session_id)
            {
                continue;
            }
            callbacks_by_session
                .entry((callback.0, callback.1))
                .or_default()
                .push(callback.2);
        }
    }

    let mut summary = ActivityV1Summary::default();
    let mut workspaces = Vec::new();
    for workspace in legacy.workspaces.into_iter() {
        if filters
            .workspace_id
            .is_some_and(|workspace_id| workspace.workspace_id != workspace_id)
        {
            continue;
        }

        let mut workspace_summary = ActivityV1Summary::default();
        let mut sessions = Vec::new();
        for session in workspace.sessions.into_iter() {
            if filters
                .session_id
                .is_some_and(|session_id| session.session_id != session_id)
            {
                continue;
            }
            let callbacks = callbacks_by_session
                .remove(&(session.workspace_id, session.session_id))
                .unwrap_or_default();
            let callback_waiting_count = callbacks
                .iter()
                .filter(|callback| callback.status == ActivityV1CallbackStatus::Waiting)
                .count();
            let recent_callback_count = callbacks.len();
            let active_turn_count = session.active_turn_count;
            let pending_turn_count = session.queue.count;
            let session_summary = ActivityV1Summary {
                active_turn_count,
                pending_turn_count,
                callback_waiting_count,
                recent_callback_count,
            };
            add_summary(&mut workspace_summary, &session_summary);
            let status = if active_turn_count > 0 {
                ActivityV1SessionStatus::Active
            } else if callback_waiting_count > 0 {
                ActivityV1SessionStatus::WaitingForCallback
            } else if pending_turn_count > 0 {
                ActivityV1SessionStatus::Pending
            } else {
                ActivityV1SessionStatus::Idle
            };
            sessions.push(ActivityV1Session {
                subject: ActivityV1Subject {
                    kind: ActivityV1SubjectKind::Session,
                    id: session.session_id.to_string(),
                    workspace_id: Some(session.workspace_id),
                    session_id: Some(session.session_id),
                },
                status,
                summary_text: session_summary_text(&session_summary),
                summary: session_summary,
                callbacks,
                links: vec![ActivityV1Link {
                    rel: "session".to_string(),
                    href: format!("/api/sessions/{}", session.session_id),
                }],
                updated_at: session.updated_at,
            });
        }

        if sessions.is_empty() {
            continue;
        }
        add_summary(&mut summary, &workspace_summary);
        workspaces.push(ActivityV1Workspace {
            subject: ActivityV1Subject {
                kind: ActivityV1SubjectKind::Workspace,
                id: workspace.workspace_id.to_string(),
                workspace_id: Some(workspace.workspace_id),
                session_id: None,
            },
            summary: workspace_summary,
            sessions,
            links: vec![ActivityV1Link {
                rel: "workspace".to_string(),
                href: format!("/api/workspaces/{}", workspace.workspace_id),
            }],
            updated_at: workspace.updated_at,
        });
    }

    for ((workspace_id, session_id), callbacks) in callbacks_by_session {
        if filters
            .workspace_id
            .is_some_and(|filter_workspace_id| workspace_id != filter_workspace_id)
            || filters
                .session_id
                .is_some_and(|filter_session_id| session_id != filter_session_id)
        {
            continue;
        }
        let callback_waiting_count = callbacks
            .iter()
            .filter(|callback| callback.status == ActivityV1CallbackStatus::Waiting)
            .count();
        let session_summary = ActivityV1Summary {
            active_turn_count: 0,
            pending_turn_count: 0,
            callback_waiting_count,
            recent_callback_count: callbacks.len(),
        };
        add_summary(&mut summary, &session_summary);
        let updated_at = callbacks
            .iter()
            .map(|callback| callback.updated_at)
            .max()
            .unwrap_or_else(Utc::now);
        let session = ActivityV1Session {
            subject: ActivityV1Subject {
                kind: ActivityV1SubjectKind::Session,
                id: session_id.to_string(),
                workspace_id: Some(workspace_id),
                session_id: Some(session_id),
            },
            status: if callback_waiting_count > 0 {
                ActivityV1SessionStatus::WaitingForCallback
            } else {
                ActivityV1SessionStatus::Idle
            },
            summary_text: session_summary_text(&session_summary),
            summary: session_summary.clone(),
            callbacks,
            links: vec![ActivityV1Link {
                rel: "session".to_string(),
                href: format!("/api/sessions/{session_id}"),
            }],
            updated_at,
        };
        workspaces.push(ActivityV1Workspace {
            subject: ActivityV1Subject {
                kind: ActivityV1SubjectKind::Workspace,
                id: workspace_id.to_string(),
                workspace_id: Some(workspace_id),
                session_id: None,
            },
            summary: session_summary,
            sessions: vec![session],
            links: vec![ActivityV1Link {
                rel: "workspace".to_string(),
                href: format!("/api/workspaces/{workspace_id}"),
            }],
            updated_at,
        });
    }

    Ok(ActivityV1Snapshot {
        schema_version: "activity.v1".to_string(),
        generated_at: legacy.generated_at,
        scope: ActivityV1Scope {
            workspace_id: filters.workspace_id,
            session_id: filters.session_id,
            user_id: None,
        },
        summary,
        workspaces,
    })
}

fn callback_from_registry(row: WorkflowCallbackRegistryItem) -> ActivityV1Callback {
    let status = match row.status {
        WorkflowCallbackStatus::Pending => ActivityV1CallbackStatus::Waiting,
        WorkflowCallbackStatus::Delivered => ActivityV1CallbackStatus::Delivered,
        WorkflowCallbackStatus::Failed => ActivityV1CallbackStatus::Failed,
        WorkflowCallbackStatus::Superseded => ActivityV1CallbackStatus::Cancelled,
    };
    let summary_text = match status {
        ActivityV1CallbackStatus::Waiting => "Workflow completion response pending".to_string(),
        ActivityV1CallbackStatus::Delivered => "Workflow completion response delivered".to_string(),
        ActivityV1CallbackStatus::Failed => {
            if let Some(message) = &row.error_message {
                format!(
                    "Workflow completion response needs attention: {}",
                    scrub_product_text(message, "Delivery failed", 180)
                )
            } else {
                "Workflow completion response needs attention".to_string()
            }
        }
        ActivityV1CallbackStatus::Cancelled => {
            "Workflow completion response was superseded".to_string()
        }
    };
    let workflow_run_id = scrub_identifier(&row.workflow_run_id, 160);
    let mut links = vec![ActivityV1Link {
        rel: "session".to_string(),
        href: format!("/api/sessions/{}", row.target_session_id),
    }];
    links.push(ActivityV1Link {
        rel: "workflow_run".to_string(),
        href: format!("/dashboard/workflows/{workflow_run_id}"),
    });
    ActivityV1Callback {
        callback_id: scrub_identifier(&row.callback_key, 220),
        kind: ActivityV1CallbackKind::WorkflowCompletion,
        status,
        summary_text,
        workflow: Some(ActivityV1WorkflowRef {
            run_id: Some(workflow_run_id),
            name: row
                .workflow_name
                .as_deref()
                .map(|value| scrub_product_text(value, "Workflow", 120)),
            design_id: row
                .workflow_design_id
                .as_deref()
                .map(|value| scrub_identifier(value, 160)),
            version: row.workflow_version,
        }),
        links,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn add_summary(target: &mut ActivityV1Summary, source: &ActivityV1Summary) {
    target.active_turn_count += source.active_turn_count;
    target.pending_turn_count += source.pending_turn_count;
    target.callback_waiting_count += source.callback_waiting_count;
    target.recent_callback_count += source.recent_callback_count;
}

fn session_summary_text(summary: &ActivityV1Summary) -> String {
    if summary.active_turn_count > 0 {
        return format!(
            "{} active turn{}",
            summary.active_turn_count,
            plural(summary.active_turn_count)
        );
    }
    if summary.callback_waiting_count > 0 {
        return format!(
            "{} callback{} waiting",
            summary.callback_waiting_count,
            plural(summary.callback_waiting_count)
        );
    }
    if summary.pending_turn_count > 0 {
        return format!(
            "{} pending turn{}",
            summary.pending_turn_count,
            plural(summary.pending_turn_count)
        );
    }
    if summary.recent_callback_count > 0 {
        return format!(
            "{} recent callback{}",
            summary.recent_callback_count,
            plural(summary.recent_callback_count)
        );
    }
    "No current activity".to_string()
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

async fn build_activity_snapshot_from_pool(
    pool: &SqlitePool,
) -> Result<ActivitySnapshot, sqlx::Error> {
    let generated_at = Utc::now();
    let mut workspaces = BTreeMap::<Uuid, WorkspaceAccumulator>::new();

    for row in load_running_processes(pool).await? {
        let workspace =
            workspaces
                .entry(row.workspace_id)
                .or_insert_with(|| WorkspaceAccumulator {
                    workspace_id: row.workspace_id,
                    sessions: BTreeMap::new(),
                    updated_at: row.updated_at,
                });
        workspace.updated_at = workspace.updated_at.max(row.updated_at);
        let session = workspace.sessions.entry(row.session_id).or_insert_with(|| {
            SessionAccumulator::new(row.workspace_id, row.session_id, row.updated_at)
        });
        session.updated_at = session.updated_at.max(row.updated_at);
        session
            .running_execution_processes
            .push(ActivityExecutionProcess {
                execution_process_id: row.execution_process_id,
                run_reason: row.run_reason,
                status: row.status,
                started_at: row.started_at,
                updated_at: row.updated_at,
            });
    }

    for row in load_pending_queue_items(pool).await? {
        let workspace =
            workspaces
                .entry(row.workspace_id)
                .or_insert_with(|| WorkspaceAccumulator {
                    workspace_id: row.workspace_id,
                    sessions: BTreeMap::new(),
                    updated_at: row.updated_at,
                });
        workspace.updated_at = workspace.updated_at.max(row.updated_at);
        let session = workspace.sessions.entry(row.session_id).or_insert_with(|| {
            SessionAccumulator::new(row.workspace_id, row.session_id, row.updated_at)
        });
        session.updated_at = session.updated_at.max(row.updated_at);
        session.queue.count += 1;
        session.queue.updated_at = Some(
            session
                .queue
                .updated_at
                .unwrap_or(row.updated_at)
                .max(row.updated_at),
        );
        if session.queue.first_item_id.is_none() {
            session.queue.first_item_id = Some(row.queue_item_id);
        }
        match row.status {
            AgentMessageQueueStatus::Queued => session.queue.queued_count += 1,
            AgentMessageQueueStatus::Leased => session.queue.leased_count += 1,
            AgentMessageQueueStatus::Starting => session.queue.starting_count += 1,
            AgentMessageQueueStatus::Running => session.queue.running_count += 1,
            AgentMessageQueueStatus::Completed
            | AgentMessageQueueStatus::Failed
            | AgentMessageQueueStatus::Cancelled => {}
        }
    }

    Ok(ActivitySnapshot {
        generated_at,
        callback_state_available: false,
        workspaces: workspaces
            .into_values()
            .map(|workspace| {
                let sessions: Vec<ActivitySession> = workspace
                    .sessions
                    .into_values()
                    .map(|session| session.to_session())
                    .collect();
                let active_turn_count = sessions
                    .iter()
                    .map(|session| session.active_turn_count)
                    .sum();
                let running_turn_count = sessions
                    .iter()
                    .flat_map(|session| &session.running_execution_processes)
                    .filter(|process| process.run_reason != ExecutionProcessRunReason::DevServer)
                    .count();
                let running_dev_server_count = sessions
                    .iter()
                    .flat_map(|session| &session.running_execution_processes)
                    .filter(|process| process.run_reason == ExecutionProcessRunReason::DevServer)
                    .count();
                let queued_count = sessions.iter().map(|session| session.queue.count).sum();
                ActivityWorkspace {
                    workspace_id: workspace.workspace_id,
                    active_turn_count,
                    running_turn_count,
                    running_dev_server_count,
                    queued_count,
                    sessions,
                    updated_at: workspace.updated_at,
                }
            })
            .collect(),
    })
}

async fn load_running_processes(pool: &SqlitePool) -> Result<Vec<RunningProcessRow>, sqlx::Error> {
    let rows = sqlx::query(
        r#"SELECT
             s.workspace_id,
             ep.session_id,
             ep.id AS execution_process_id,
             ep.run_reason,
             ep.status,
             ep.started_at,
             ep.updated_at
           FROM execution_processes ep
           JOIN sessions s ON s.id = ep.session_id
           WHERE ep.status = 'running'
             AND ep.dropped = FALSE
           ORDER BY s.workspace_id ASC, ep.session_id ASC, ep.started_at ASC, ep.id ASC"#,
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(RunningProcessRow {
                workspace_id: row.try_get("workspace_id")?,
                session_id: row.try_get("session_id")?,
                execution_process_id: row.try_get("execution_process_id")?,
                run_reason: row.try_get("run_reason")?,
                status: row.try_get("status")?,
                started_at: row.try_get("started_at")?,
                updated_at: row.try_get("updated_at")?,
            })
        })
        .collect()
}

async fn load_pending_queue_items(pool: &SqlitePool) -> Result<Vec<QueueRow>, sqlx::Error> {
    let rows = sqlx::query(
        r#"SELECT
             id AS queue_item_id,
             session_id,
             workspace_id,
             status,
             queued_at,
             updated_at
           FROM agent_message_queue
           WHERE status IN ('queued','leased','starting','running')
           ORDER BY workspace_id ASC, session_id ASC, queued_at ASC, id ASC"#,
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(QueueRow {
                workspace_id: row.try_get("workspace_id")?,
                session_id: row.try_get("session_id")?,
                queue_item_id: row.try_get("queue_item_id")?,
                status: row.try_get("status")?,
                updated_at: row.try_get("updated_at")?,
            })
        })
        .collect()
}

async fn load_workflow_callback_rows(
    pool: &SqlitePool,
    filters: &ActivityV1Filters,
) -> Result<Vec<QueueActivityRow>, sqlx::Error> {
    let mut query = String::from(
        r#"SELECT
             session_id,
             workspace_id,
             status,
             source,
             data,
             queued_at,
             created_at,
             updated_at
           FROM agent_message_queue
           WHERE source = 'workflow'
             AND status IN ('queued','leased','starting','running','completed','failed','cancelled')"#,
    );
    if filters.workspace_id.is_some() {
        query.push_str(" AND workspace_id = ?");
    }
    if filters.session_id.is_some() {
        query.push_str(" AND session_id = ?");
    }
    query.push_str(" ORDER BY updated_at DESC LIMIT 50");

    let mut query = sqlx::query(&query);
    if let Some(workspace_id) = filters.workspace_id {
        query = query.bind(workspace_id);
    }
    if let Some(session_id) = filters.session_id {
        query = query.bind(session_id);
    }

    let rows = query.fetch_all(pool).await?;
    rows.into_iter()
        .map(|row| {
            let data_json: String = row.try_get("data")?;
            let data =
                serde_json::from_str(&data_json).map_err(|source| sqlx::Error::ColumnDecode {
                    index: "data".to_string(),
                    source: Box::new(source),
                })?;
            Ok(QueueActivityRow {
                workspace_id: row.try_get("workspace_id")?,
                session_id: row.try_get("session_id")?,
                status: row.try_get("status")?,
                source: row.try_get("source")?,
                data,
                queued_at: row.try_get("queued_at")?,
                created_at: row.try_get("created_at")?,
                updated_at: row.try_get("updated_at")?,
            })
        })
        .collect()
}

fn workflow_callback_from_row(
    row: QueueActivityRow,
    index: usize,
) -> Option<(Uuid, Uuid, ActivityV1Callback)> {
    if row.source != AgentMessageSource::Workflow {
        return None;
    }
    let provenance = row.data.provenance?;
    let label = scrub_product_text(&provenance.label, "Workflow activity", 120);
    let looks_like_completion =
        label.to_ascii_lowercase().contains("completion") || provenance.workflow_run_id.is_some();
    if !looks_like_completion {
        return None;
    }
    let status = match row.status {
        AgentMessageQueueStatus::Queued
        | AgentMessageQueueStatus::Leased
        | AgentMessageQueueStatus::Starting
        | AgentMessageQueueStatus::Running => ActivityV1CallbackStatus::Waiting,
        AgentMessageQueueStatus::Completed => ActivityV1CallbackStatus::Delivered,
        AgentMessageQueueStatus::Failed => ActivityV1CallbackStatus::Failed,
        AgentMessageQueueStatus::Cancelled => ActivityV1CallbackStatus::Cancelled,
    };
    let run_id = provenance
        .workflow_run_id
        .as_deref()
        .map(|value| scrub_identifier(value, 160));
    let workflow = ActivityV1WorkflowRef {
        run_id: run_id.clone(),
        name: provenance
            .workflow_name
            .as_deref()
            .map(|value| scrub_product_text(value, "Workflow", 120)),
        design_id: provenance
            .workflow_design_id
            .as_deref()
            .map(|value| scrub_identifier(value, 160)),
        version: provenance.workflow_version,
    };
    let summary_text = match status {
        ActivityV1CallbackStatus::Waiting => "Workflow completion response pending".to_string(),
        ActivityV1CallbackStatus::Delivered => "Workflow completion response delivered".to_string(),
        ActivityV1CallbackStatus::Failed => {
            "Workflow completion response needs attention".to_string()
        }
        ActivityV1CallbackStatus::Cancelled => {
            "Workflow completion response was cancelled".to_string()
        }
    };
    let mut links = vec![ActivityV1Link {
        rel: "session".to_string(),
        href: format!("/api/sessions/{}", row.session_id),
    }];
    if let Some(run_id) = &run_id {
        links.push(ActivityV1Link {
            rel: "workflow_run".to_string(),
            href: format!("/dashboard/workflows/{run_id}"),
        });
    }
    Some((
        row.workspace_id,
        row.session_id,
        ActivityV1Callback {
            callback_id: format!(
                "workflow-completion:{}:{index}",
                row.created_at.timestamp_millis()
            ),
            kind: ActivityV1CallbackKind::WorkflowCompletion,
            status,
            summary_text,
            workflow: Some(workflow),
            links,
            created_at: row.created_at.max(row.queued_at),
            updated_at: row.updated_at,
        },
    ))
}

fn scrub_identifier(value: &str, max_chars: usize) -> String {
    if contains_unsafe_text(value) || looks_like_local_path(value) {
        return "opaque-ref".to_string();
    }
    let cleaned: String = value
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | ':' | '.' | '_' | '-' => ch,
            _ => '-',
        })
        .take(max_chars)
        .collect();
    if cleaned.trim_matches('-').is_empty() {
        "opaque-ref".to_string()
    } else {
        cleaned
    }
}

fn scrub_product_text(value: &str, fallback: &str, max_chars: usize) -> String {
    let mut text = value.trim().replace('\n', " ").replace('\r', " ");
    if text.is_empty() {
        return fallback.to_string();
    }
    text = text
        .split_whitespace()
        .map(|part| {
            if looks_like_local_path(part) {
                "workspace location".to_string()
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    for (needle, replacement) in [
        ("queue_item", "pending work"),
        ("queue-item", "pending work"),
        ("queue item", "pending work"),
        ("webhook", "connection"),
        ("hmac", "signature"),
        ("trigger", "automation event"),
        ("delivery id", "delivery"),
        ("delivery_id", "delivery"),
        ("execution process id", "run"),
        ("execution_process_id", "run"),
        ("execution process", "run"),
        ("raw xml", "structured response"),
        ("raw json", "structured response"),
        ("workflowstepstate", "workflow state"),
        ("runready", "ready"),
        ("provider diagnostics", "status details"),
        ("bd show", "task details"),
        ("git ", "version-control "),
        ("shell", "automation"),
        ("prompt", "message"),
    ] {
        text = replace_case_insensitive(&text, needle, replacement);
    }
    let truncated: String = text.chars().take(max_chars).collect();
    if truncated.trim().is_empty() {
        fallback.to_string()
    } else {
        truncated
    }
}

fn contains_unsafe_text(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "queue_item",
        "queue-item",
        "queue item",
        "webhook",
        "hmac",
        "delivery id",
        "delivery_id",
        "execution process id",
        "execution_process_id",
        "raw xml",
        "raw json",
        "workflowstepstate",
        "runready",
        "provider diagnostics",
        "bd show",
        "git ",
        "shell",
        "prompt",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn replace_case_insensitive(input: &str, needle: &str, replacement: &str) -> String {
    let mut output = String::new();
    let lower_input = input.to_ascii_lowercase();
    let lower_needle = needle.to_ascii_lowercase();
    let mut index = 0;
    while let Some(relative) = lower_input[index..].find(&lower_needle) {
        let start = index + relative;
        output.push_str(&input[index..start]);
        output.push_str(replacement);
        index = start + needle.len();
    }
    output.push_str(&input[index..]);
    output
}

fn looks_like_local_path(part: &str) -> bool {
    let trimmed = part.trim_matches(|ch: char| ch == ',' || ch == '.' || ch == ';' || ch == ':');
    trimmed.starts_with("/Users/")
        || trimmed.starts_with("/tmp/")
        || trimmed.starts_with("/private/var/")
        || trimmed.starts_with("/var/folders/")
}

fn validate_callback_key(value: &str) -> Result<(), ApiError> {
    let valid = !value.is_empty()
        && value.len() <= 220
        && value
            .chars()
            .all(|ch| matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | ':' | '.' | '_' | '-'));
    if valid {
        Ok(())
    } else {
        Err(ApiError::BadRequest(
            "Callback key must be a stable opaque identifier.".to_string(),
        ))
    }
}

fn validate_callback_ref(value: &str, field: &str) -> Result<(), ApiError> {
    let valid = !value.is_empty()
        && value.len() <= 180
        && value
            .chars()
            .all(|ch| matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | ':' | '.' | '_' | '-'));
    if valid {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!(
            "{field} must be a stable opaque identifier."
        )))
    }
}

async fn ensure_session_in_workspace(
    pool: &SqlitePool,
    session_id: Uuid,
    workspace_id: Uuid,
) -> Result<(), ApiError> {
    let found: Option<Uuid> = sqlx::query_scalar("SELECT workspace_id FROM sessions WHERE id = ?1")
        .bind(session_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    match found {
        Some(found_workspace_id) if found_workspace_id == workspace_id => Ok(()),
        Some(_) => Err(ApiError::BadRequest(
            "Callback target session does not belong to the requested workspace.".to_string(),
        )),
        None => Err(ApiError::BadRequest(
            "Callback target session was not found.".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use db::DBService;
    use sqlx::{Executor, sqlite::SqlitePoolOptions};
    use uuid::Uuid;

    use super::{
        ActivitySessionStatus, ActivityV1CallbackStatus, ActivityV1Filters,
        ActivityV1SessionStatus, ActivityV1WsEventType, activity_v1_heartbeat_event,
        activity_v1_refresh_snapshot_event, activity_v1_snapshot_event, build_activity_snapshot,
        build_activity_v1_snapshot,
    };

    async fn test_db() -> DBService {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        pool.execute(
            r#"CREATE TABLE sessions (
                id BLOB PRIMARY KEY,
                workspace_id BLOB NOT NULL
            )"#,
        )
        .await
        .unwrap();
        pool.execute(
            r#"CREATE TABLE execution_processes (
                id BLOB PRIMARY KEY,
                session_id BLOB NOT NULL,
                run_reason TEXT NOT NULL,
                executor_action TEXT NOT NULL DEFAULT '{}',
                status TEXT NOT NULL,
                exit_code INTEGER,
                dropped INTEGER NOT NULL DEFAULT 0,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )"#,
        )
        .await
        .unwrap();
        pool.execute(
            r#"CREATE TABLE agent_message_queue (
                id BLOB PRIMARY KEY,
                session_id BLOB NOT NULL,
                workspace_id BLOB NOT NULL,
                status TEXT NOT NULL,
                source TEXT NOT NULL,
                priority INTEGER NOT NULL,
                data TEXT NOT NULL,
                started_execution_process_id BLOB,
                lease_owner TEXT,
                lease_expires_at TEXT,
                attempt_count INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                queued_at TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )"#,
        )
        .await
        .unwrap();
        pool.execute(
            r#"CREATE TABLE workflow_callback_registry (
                id BLOB PRIMARY KEY,
                callback_key TEXT NOT NULL UNIQUE,
                workspace_id BLOB NOT NULL,
                target_session_id BLOB NOT NULL,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                workflow_run_id TEXT NOT NULL,
                workflow_name TEXT,
                workflow_design_id TEXT,
                workflow_version INTEGER,
                delivered_ref TEXT,
                error_message TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )"#,
        )
        .await
        .unwrap();
        DBService { pool }
    }

    #[tokio::test]
    async fn activity_snapshot_groups_running_and_queued_sessions() {
        let db = test_db().await;
        let workspace_id = Uuid::new_v4();
        let running_session_id = Uuid::new_v4();
        let queued_session_id = Uuid::new_v4();
        let execution_id = Uuid::new_v4();
        let queue_item_id = Uuid::new_v4();
        let running_updated_at = Utc.with_ymd_and_hms(2026, 7, 31, 12, 0, 0).unwrap();
        let queue_updated_at = Utc.with_ymd_and_hms(2026, 7, 31, 12, 5, 0).unwrap();

        sqlx::query("INSERT INTO sessions (id, workspace_id) VALUES (?1, ?2), (?3, ?2)")
            .bind(running_session_id)
            .bind(workspace_id)
            .bind(queued_session_id)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO execution_processes
               (id, session_id, run_reason, executor_action, status, dropped, started_at, created_at, updated_at)
               VALUES (?1, ?2, 'codingagent', '{}', 'running', 0, ?3, ?3, ?3)"#,
        )
        .bind(execution_id)
        .bind(running_session_id)
        .bind(running_updated_at)
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO agent_message_queue
               (id, session_id, workspace_id, status, source, priority, data, attempt_count, queued_at, created_at, updated_at)
               VALUES (?1, ?2, ?3, 'queued', 'workflow', 60, '{"message":"redacted"}', 0, ?4, ?4, ?4)"#,
        )
        .bind(queue_item_id)
        .bind(queued_session_id)
        .bind(workspace_id)
        .bind(queue_updated_at)
        .execute(&db.pool)
        .await
        .unwrap();

        let snapshot = build_activity_snapshot(&db).await.unwrap();

        assert!(!snapshot.callback_state_available);
        assert_eq!(snapshot.workspaces.len(), 1);
        let workspace = &snapshot.workspaces[0];
        assert_eq!(workspace.workspace_id, workspace_id);
        assert_eq!(workspace.active_turn_count, 1);
        assert_eq!(workspace.running_turn_count, 1);
        assert_eq!(workspace.queued_count, 1);
        assert_eq!(workspace.sessions.len(), 2);
        assert_eq!(workspace.updated_at, queue_updated_at);

        let running_session = workspace
            .sessions
            .iter()
            .find(|session| session.session_id == running_session_id)
            .unwrap();
        assert_eq!(running_session.status, ActivitySessionStatus::Running);
        assert_eq!(running_session.active_turn_count, 1);
        assert_eq!(
            running_session.running_execution_processes[0].execution_process_id,
            execution_id
        );
        assert_eq!(running_session.updated_at, running_updated_at);

        let queued_session = workspace
            .sessions
            .iter()
            .find(|session| session.session_id == queued_session_id)
            .unwrap();
        assert_eq!(queued_session.status, ActivitySessionStatus::Queued);
        assert_eq!(queued_session.queue.count, 1);
        assert_eq!(queued_session.queue.first_item_id, Some(queue_item_id));
        assert_eq!(queued_session.updated_at, queue_updated_at);
    }

    #[tokio::test]
    async fn activity_snapshot_separates_dev_servers_from_active_turns() {
        let db = test_db().await;
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let execution_id = Uuid::new_v4();
        let now = Utc.with_ymd_and_hms(2026, 7, 31, 12, 0, 0).unwrap();

        sqlx::query("INSERT INTO sessions (id, workspace_id) VALUES (?1, ?2)")
            .bind(session_id)
            .bind(workspace_id)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO execution_processes
               (id, session_id, run_reason, executor_action, status, dropped, started_at, created_at, updated_at)
               VALUES (?1, ?2, 'devserver', '{}', 'running', 0, ?3, ?3, ?3)"#,
        )
        .bind(execution_id)
        .bind(session_id)
        .bind(now)
        .execute(&db.pool)
        .await
        .unwrap();

        let snapshot = build_activity_snapshot(&db).await.unwrap();
        let workspace = &snapshot.workspaces[0];
        let session = &workspace.sessions[0];

        assert_eq!(workspace.active_turn_count, 0);
        assert_eq!(workspace.running_turn_count, 0);
        assert_eq!(workspace.running_dev_server_count, 1);
        assert_eq!(session.active_turn_count, 0);
        assert_eq!(session.status, ActivitySessionStatus::Idle);
        assert_eq!(
            session.running_execution_processes[0].execution_process_id,
            execution_id
        );
    }

    #[tokio::test]
    async fn activity_v1_snapshot_exposes_product_safe_callback_summaries() {
        let db = test_db().await;
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let queue_item_id = Uuid::new_v4();
        let now = Utc.with_ymd_and_hms(2026, 8, 28, 12, 0, 0).unwrap();
        let data = serde_json::json!({
            "message": "Do not expose this prompt body with /Users/me/private raw XML webhook queue_item bd show git status shell provider diagnostics trigger delivery ID execution process ID.",
            "session_command": null,
            "provenance": {
                "kind": "workflow",
                "label": "Workflow completion response via webhook queue_item /Users/me/private",
                "workflow_run_id": "run-abc",
                "workflow_name": "Review /tmp/secret raw JSON",
                "workflow_design_id": "design-1",
                "workflow_version": 2
            }
        });

        sqlx::query("INSERT INTO sessions (id, workspace_id) VALUES (?1, ?2)")
            .bind(session_id)
            .bind(workspace_id)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO agent_message_queue
               (id, session_id, workspace_id, status, source, priority, data, attempt_count, queued_at, created_at, updated_at)
               VALUES (?1, ?2, ?3, 'queued', 'workflow', 60, ?4, 0, ?5, ?5, ?5)"#,
        )
        .bind(queue_item_id)
        .bind(session_id)
        .bind(workspace_id)
        .bind(data.to_string())
        .bind(now)
        .execute(&db.pool)
        .await
        .unwrap();

        let snapshot = build_activity_v1_snapshot(&db, &ActivityV1Filters::default())
            .await
            .unwrap();

        assert_eq!(snapshot.schema_version, "activity.v1");
        assert_eq!(snapshot.scope.workspace_id, None);
        assert_eq!(snapshot.summary.pending_turn_count, 1);
        assert_eq!(snapshot.summary.callback_waiting_count, 1);
        assert_eq!(snapshot.workspaces.len(), 1);
        let session = &snapshot.workspaces[0].sessions[0];
        assert_eq!(session.status, ActivityV1SessionStatus::WaitingForCallback);
        assert_eq!(session.callbacks.len(), 1);
        assert_eq!(
            session.callbacks[0].status,
            ActivityV1CallbackStatus::Waiting
        );
        assert_eq!(
            session.callbacks[0].summary_text,
            "Workflow completion response pending"
        );
        assert_eq!(
            session.callbacks[0]
                .workflow
                .as_ref()
                .unwrap()
                .name
                .as_deref(),
            Some("Review workspace location structured response")
        );

        let serialized = serde_json::to_string(&snapshot)
            .unwrap()
            .to_ascii_lowercase();
        for forbidden in [
            "do not expose this",
            "/users/",
            "/tmp/",
            "webhook",
            "queue_item",
            "queue item",
            "bd show",
            "git status",
            "shell",
            "provider diagnostics",
            "trigger",
            "delivery id",
            "execution process id",
            "raw xml",
            "raw json",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "activity v1 payload leaked forbidden term: {forbidden}\n{serialized}"
            );
        }
    }

    #[tokio::test]
    async fn activity_v1_ws_snapshot_event_uses_product_safe_contract() {
        let db = test_db().await;
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let now = Utc.with_ymd_and_hms(2026, 8, 28, 12, 0, 0).unwrap();
        sqlx::query("INSERT INTO sessions (id, workspace_id) VALUES (?1, ?2)")
            .bind(session_id)
            .bind(workspace_id)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO workflow_callback_registry
               (id, callback_key, workspace_id, target_session_id, kind, status, workflow_run_id, workflow_name, workflow_design_id, workflow_version, error_message, created_at, updated_at)
               VALUES (?1, 'workflow-completion:run-ws:session-a', ?2, ?3, 'workflow_completion', 'failed', 'run-ws', 'Webhook /tmp/raw XML workflow', 'design-a', 3, 'queue_item /Users/me shell bd show git status provider diagnostics trigger delivery ID execution process ID', ?4, ?4)"#,
        )
        .bind(Uuid::new_v4())
        .bind(workspace_id)
        .bind(session_id)
        .bind(now)
        .execute(&db.pool)
        .await
        .unwrap();

        let snapshot = build_activity_v1_snapshot(&db, &ActivityV1Filters::default())
            .await
            .unwrap();
        let event = activity_v1_snapshot_event(snapshot);
        assert_eq!(event.schema_version, "activity.v1.ws");
        assert_eq!(event.event_type, ActivityV1WsEventType::Snapshot);
        assert!(event.cursor.starts_with("snapshot:"));
        assert!(event.snapshot.is_some());

        let serialized = serde_json::to_string(&event).unwrap().to_ascii_lowercase();
        for forbidden in [
            "/users/",
            "/tmp/",
            "webhook",
            "queue_item",
            "bd show",
            "git status",
            "shell",
            "provider diagnostics",
            "trigger",
            "delivery id",
            "execution process id",
            "raw xml",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "activity v1 websocket event leaked forbidden term: {forbidden}
{serialized}"
            );
        }
    }

    #[test]
    fn activity_v1_ws_heartbeat_and_reconnect_events_are_versioned() {
        let heartbeat = activity_v1_heartbeat_event();
        assert_eq!(heartbeat.schema_version, "activity.v1.ws");
        assert_eq!(heartbeat.event_type, ActivityV1WsEventType::Heartbeat);
        assert!(heartbeat.snapshot.is_none());
        assert!(heartbeat.cursor.starts_with("heartbeat:"));

        let refresh = activity_v1_refresh_snapshot_event();
        assert_eq!(refresh.schema_version, "activity.v1.ws");
        assert_eq!(refresh.event_type, ActivityV1WsEventType::RefreshSnapshot);
        assert!(refresh.snapshot.is_none());
        assert_eq!(
            refresh.reason.as_deref(),
            Some(
                "Replay is not available for this activity cursor. Refresh the activity snapshot."
            )
        );
        assert!(refresh.cursor.starts_with("refresh:"));
    }

    #[tokio::test]
    async fn activity_v1_snapshot_applies_workspace_and_session_scope() {
        let db = test_db().await;
        let workspace_a = Uuid::new_v4();
        let workspace_b = Uuid::new_v4();
        let session_a = Uuid::new_v4();
        let session_b = Uuid::new_v4();
        let now = Utc.with_ymd_and_hms(2026, 8, 28, 12, 0, 0).unwrap();
        sqlx::query("INSERT INTO sessions (id, workspace_id) VALUES (?1, ?2), (?3, ?4)")
            .bind(session_a)
            .bind(workspace_a)
            .bind(session_b)
            .bind(workspace_b)
            .execute(&db.pool)
            .await
            .unwrap();
        for (queue_id, session_id, workspace_id, run_id) in [
            (Uuid::new_v4(), session_a, workspace_a, "run-a"),
            (Uuid::new_v4(), session_b, workspace_b, "run-b"),
        ] {
            let data = serde_json::json!({
                "message": "Callback body",
                "provenance": {
                    "kind": "workflow",
                    "label": "Workflow completion response",
                    "workflow_run_id": run_id,
                    "workflow_name": "Workflow",
                    "workflow_design_id": "design",
                    "workflow_version": 1
                }
            });
            sqlx::query(
                r#"INSERT INTO agent_message_queue
                   (id, session_id, workspace_id, status, source, priority, data, attempt_count, queued_at, created_at, updated_at)
                   VALUES (?1, ?2, ?3, 'completed', 'workflow', 60, ?4, 0, ?5, ?5, ?5)"#,
            )
            .bind(queue_id)
            .bind(session_id)
            .bind(workspace_id)
            .bind(data.to_string())
            .bind(now)
            .execute(&db.pool)
            .await
            .unwrap();
        }

        let snapshot = build_activity_v1_snapshot(
            &db,
            &ActivityV1Filters {
                workspace_id: Some(workspace_a),
                session_id: Some(session_a),
            },
        )
        .await
        .unwrap();

        assert_eq!(snapshot.scope.workspace_id, Some(workspace_a));
        assert_eq!(snapshot.scope.session_id, Some(session_a));
        assert_eq!(snapshot.summary.recent_callback_count, 1);
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(
            snapshot.workspaces[0].subject.workspace_id,
            Some(workspace_a)
        );
        assert_eq!(snapshot.workspaces[0].sessions.len(), 1);
        assert_eq!(
            snapshot.workspaces[0].sessions[0].subject.session_id,
            Some(session_a)
        );
        assert_eq!(
            snapshot.workspaces[0].sessions[0].callbacks[0]
                .workflow
                .as_ref()
                .unwrap()
                .run_id
                .as_deref(),
            Some("run-a")
        );
    }

    #[tokio::test]
    async fn activity_v1_snapshot_prefers_stable_registry_callback_ids() {
        let db = test_db().await;
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let now = Utc.with_ymd_and_hms(2026, 8, 28, 12, 0, 0).unwrap();
        sqlx::query("INSERT INTO sessions (id, workspace_id) VALUES (?1, ?2)")
            .bind(session_id)
            .bind(workspace_id)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO workflow_callback_registry
               (id, callback_key, workspace_id, target_session_id, kind, status, workflow_run_id, workflow_name, workflow_design_id, workflow_version, created_at, updated_at)
               VALUES (?1, 'workflow-completion:run-stable:session-a', ?2, ?3, 'workflow_completion', 'pending', 'run-stable', 'Stable Workflow', 'design-a', 3, ?4, ?4)"#,
        )
        .bind(Uuid::new_v4())
        .bind(workspace_id)
        .bind(session_id)
        .bind(now)
        .execute(&db.pool)
        .await
        .unwrap();
        let data = serde_json::json!({
            "message": "Callback body",
            "provenance": {
                "kind": "workflow",
                "label": "Workflow completion response",
                "workflow_run_id": "run-stable",
                "workflow_name": "Stable Workflow",
                "workflow_design_id": "design-a",
                "workflow_version": 3
            }
        });
        sqlx::query(
            r#"INSERT INTO agent_message_queue
               (id, session_id, workspace_id, status, source, priority, data, attempt_count, queued_at, created_at, updated_at)
               VALUES (?1, ?2, ?3, 'queued', 'workflow', 60, ?4, 0, ?5, ?5, ?5)"#,
        )
        .bind(Uuid::new_v4())
        .bind(session_id)
        .bind(workspace_id)
        .bind(data.to_string())
        .bind(now)
        .execute(&db.pool)
        .await
        .unwrap();

        let snapshot = build_activity_v1_snapshot(&db, &ActivityV1Filters::default())
            .await
            .unwrap();
        let callbacks = &snapshot.workspaces[0].sessions[0].callbacks;
        assert_eq!(callbacks.len(), 1);
        assert_eq!(
            callbacks[0].callback_id,
            "workflow-completion:run-stable:session-a"
        );
        assert_eq!(callbacks[0].status, ActivityV1CallbackStatus::Waiting);
    }

    #[tokio::test]
    async fn workflow_callback_registry_upsert_and_status_are_idempotent() {
        use db::models::workflow_callback_registry::{
            UpdateWorkflowCallbackRegistryStatus, UpsertWorkflowCallbackRegistryItem,
            WorkflowCallbackKind, WorkflowCallbackRegistryItem, WorkflowCallbackStatus,
        };

        let db = test_db().await;
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, workspace_id) VALUES (?1, ?2)")
            .bind(session_id)
            .bind(workspace_id)
            .execute(&db.pool)
            .await
            .unwrap();
        let input = UpsertWorkflowCallbackRegistryItem {
            callback_key: "workflow-completion:run-1:session-1".to_string(),
            workspace_id,
            target_session_id: session_id,
            kind: WorkflowCallbackKind::WorkflowCompletion,
            workflow_run_id: "run-1".to_string(),
            workflow_name: Some("Workflow".to_string()),
            workflow_design_id: Some("design-1".to_string()),
            workflow_version: Some(1),
        };
        let first = WorkflowCallbackRegistryItem::upsert_pending(&db.pool, &input)
            .await
            .unwrap();
        let second = WorkflowCallbackRegistryItem::upsert_pending(&db.pool, &input)
            .await
            .unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(second.status, WorkflowCallbackStatus::Pending);

        let delivered = WorkflowCallbackRegistryItem::update_status(
            &db.pool,
            &UpdateWorkflowCallbackRegistryStatus {
                callback_key: input.callback_key.clone(),
                status: WorkflowCallbackStatus::Delivered,
                delivered_ref: Some("vk:opaque".to_string()),
                error_message: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(delivered.status, WorkflowCallbackStatus::Delivered);

        let replay = WorkflowCallbackRegistryItem::upsert_pending(&db.pool, &input)
            .await
            .unwrap();
        assert_eq!(replay.status, WorkflowCallbackStatus::Delivered);
        assert_eq!(replay.delivered_ref.as_deref(), Some("vk:opaque"));

        let stale_failed = WorkflowCallbackRegistryItem::update_status(
            &db.pool,
            &UpdateWorkflowCallbackRegistryStatus {
                callback_key: input.callback_key.clone(),
                status: WorkflowCallbackStatus::Failed,
                delivered_ref: None,
                error_message: Some("webhook /Users/me queue_item raw XML".to_string()),
            },
        )
        .await
        .unwrap();
        assert_eq!(stale_failed.status, WorkflowCallbackStatus::Delivered);
        assert_eq!(stale_failed.delivered_ref.as_deref(), Some("vk:opaque"));

        let snapshot = build_activity_v1_snapshot(&db, &ActivityV1Filters::default())
            .await
            .unwrap();
        let callback = &snapshot.workspaces[0].sessions[0].callbacks[0];
        assert_eq!(callback.status, ActivityV1CallbackStatus::Delivered);
        let serialized = serde_json::to_string(callback)
            .unwrap()
            .to_ascii_lowercase();
        assert!(!serialized.contains("webhook"));
        assert!(!serialized.contains("/users/"));
        assert!(!serialized.contains("queue_item"));
        assert!(!serialized.contains("raw xml"));
    }

    #[tokio::test]
    async fn workflow_callback_registry_rejects_mismatched_idempotency_replay() {
        use db::models::workflow_callback_registry::{
            UpsertWorkflowCallbackRegistryItem, WorkflowCallbackKind, WorkflowCallbackRegistryItem,
        };

        let db = test_db().await;
        let workspace_id = Uuid::new_v4();
        let other_workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let other_session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, workspace_id) VALUES (?1, ?2), (?3, ?4)")
            .bind(session_id)
            .bind(workspace_id)
            .bind(other_session_id)
            .bind(other_workspace_id)
            .execute(&db.pool)
            .await
            .unwrap();
        let input = UpsertWorkflowCallbackRegistryItem {
            callback_key: "workflow-completion:run-1:session-1".to_string(),
            workspace_id,
            target_session_id: session_id,
            kind: WorkflowCallbackKind::WorkflowCompletion,
            workflow_run_id: "run-1".to_string(),
            workflow_name: Some("Workflow".to_string()),
            workflow_design_id: Some("design-1".to_string()),
            workflow_version: Some(1),
        };
        WorkflowCallbackRegistryItem::upsert_pending(&db.pool, &input)
            .await
            .unwrap();

        let mut mismatched_run = input.clone();
        mismatched_run.workflow_run_id = "run-2".to_string();
        let err = WorkflowCallbackRegistryItem::upsert_pending(&db.pool, &mismatched_run)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("different workflow callback"));

        let mut mismatched_workspace = input.clone();
        mismatched_workspace.workspace_id = other_workspace_id;
        let err = WorkflowCallbackRegistryItem::upsert_pending(&db.pool, &mismatched_workspace)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("different workflow callback"));

        let mut mismatched_session = input.clone();
        mismatched_session.target_session_id = other_session_id;
        let err = WorkflowCallbackRegistryItem::upsert_pending(&db.pool, &mismatched_session)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("different workflow callback"));

        let stored = WorkflowCallbackRegistryItem::find_by_key(&db.pool, &input.callback_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.workflow_run_id, "run-1");
        assert_eq!(stored.workspace_id, workspace_id);
        assert_eq!(stored.target_session_id, session_id);
    }

    #[tokio::test]
    async fn workflow_callback_registry_keeps_failed_and_superseded_terminal() {
        use db::models::workflow_callback_registry::{
            UpdateWorkflowCallbackRegistryStatus, UpsertWorkflowCallbackRegistryItem,
            WorkflowCallbackKind, WorkflowCallbackRegistryItem, WorkflowCallbackStatus,
        };

        let db = test_db().await;
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, workspace_id) VALUES (?1, ?2)")
            .bind(session_id)
            .bind(workspace_id)
            .execute(&db.pool)
            .await
            .unwrap();
        let input = UpsertWorkflowCallbackRegistryItem {
            callback_key: "workflow-completion:run-terminal:session-1".to_string(),
            workspace_id,
            target_session_id: session_id,
            kind: WorkflowCallbackKind::WorkflowCompletion,
            workflow_run_id: "run-terminal".to_string(),
            workflow_name: Some("Workflow".to_string()),
            workflow_design_id: Some("design-1".to_string()),
            workflow_version: Some(1),
        };
        WorkflowCallbackRegistryItem::upsert_pending(&db.pool, &input)
            .await
            .unwrap();
        let failed = WorkflowCallbackRegistryItem::update_status(
            &db.pool,
            &UpdateWorkflowCallbackRegistryStatus {
                callback_key: input.callback_key.clone(),
                status: WorkflowCallbackStatus::Failed,
                delivered_ref: None,
                error_message: Some("first failure".to_string()),
            },
        )
        .await
        .unwrap();
        assert_eq!(failed.status, WorkflowCallbackStatus::Failed);

        let stale_pending = WorkflowCallbackRegistryItem::update_status(
            &db.pool,
            &UpdateWorkflowCallbackRegistryStatus {
                callback_key: input.callback_key.clone(),
                status: WorkflowCallbackStatus::Pending,
                delivered_ref: None,
                error_message: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(stale_pending.status, WorkflowCallbackStatus::Failed);
        assert_eq!(
            stale_pending.error_message.as_deref(),
            Some("first failure")
        );

        let stale_delivered = WorkflowCallbackRegistryItem::update_status(
            &db.pool,
            &UpdateWorkflowCallbackRegistryStatus {
                callback_key: input.callback_key.clone(),
                status: WorkflowCallbackStatus::Delivered,
                delivered_ref: Some("vk:late".to_string()),
                error_message: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(stale_delivered.status, WorkflowCallbackStatus::Failed);
        assert_eq!(stale_delivered.delivered_ref, None);

        let superseded_input = UpsertWorkflowCallbackRegistryItem {
            callback_key: "workflow-completion:run-superseded:session-1".to_string(),
            workflow_run_id: "run-superseded".to_string(),
            ..input
        };
        WorkflowCallbackRegistryItem::upsert_pending(&db.pool, &superseded_input)
            .await
            .unwrap();
        WorkflowCallbackRegistryItem::update_status(
            &db.pool,
            &UpdateWorkflowCallbackRegistryStatus {
                callback_key: superseded_input.callback_key.clone(),
                status: WorkflowCallbackStatus::Superseded,
                delivered_ref: None,
                error_message: None,
            },
        )
        .await
        .unwrap();
        let stale = WorkflowCallbackRegistryItem::update_status(
            &db.pool,
            &UpdateWorkflowCallbackRegistryStatus {
                callback_key: superseded_input.callback_key,
                status: WorkflowCallbackStatus::Delivered,
                delivered_ref: Some("vk:late".to_string()),
                error_message: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(stale.status, WorkflowCallbackStatus::Superseded);
        assert_eq!(stale.delivered_ref, None);
    }
}
