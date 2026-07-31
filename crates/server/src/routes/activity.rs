use std::collections::BTreeMap;

use axum::{
    Router,
    extract::{State, ws::Message},
    response::IntoResponse,
    routing::get,
};
use chrono::{DateTime, Utc};
use db::{
    DBService,
    models::{
        agent_message_queue::AgentMessageQueueStatus,
        execution_process::{ExecutionProcessRunReason, ExecutionProcessStatus},
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
struct SessionAccumulator {
    workspace_id: Uuid,
    session_id: Uuid,
    running_execution_processes: Vec<ActivityExecutionProcess>,
    queue: ActivityQueueSummary,
    callback: ActivityCallbackSummary,
    updated_at: DateTime<Utc>,
}

impl SessionAccumulator {
    fn new(workspace_id: Uuid, session_id: Uuid, generated_at: DateTime<Utc>) -> Self {
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
            updated_at: generated_at,
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
        .route("/activity/ws", get(stream_activity_ws))
        .with_state(deployment.clone())
}

async fn get_activity_snapshot(
    State(deployment): State<DeploymentImpl>,
) -> Result<axum::Json<ApiResponse<ActivitySnapshot>>, ApiError> {
    let snapshot = build_activity_snapshot(deployment.db()).await?;
    Ok(axum::Json(ApiResponse::success(snapshot)))
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
                    updated_at: generated_at,
                });
        workspace.updated_at = workspace.updated_at.max(row.updated_at);
        let session = workspace.sessions.entry(row.session_id).or_insert_with(|| {
            SessionAccumulator::new(row.workspace_id, row.session_id, generated_at)
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
                    updated_at: generated_at,
                });
        workspace.updated_at = workspace.updated_at.max(row.updated_at);
        let session = workspace.sessions.entry(row.session_id).or_insert_with(|| {
            SessionAccumulator::new(row.workspace_id, row.session_id, generated_at)
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

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use db::DBService;
    use sqlx::{Executor, sqlite::SqlitePoolOptions};
    use uuid::Uuid;

    use super::{ActivitySessionStatus, build_activity_snapshot};

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
        let now = Utc.with_ymd_and_hms(2026, 7, 31, 12, 0, 0).unwrap();

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
        .bind(now)
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
        .bind(now)
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

        let queued_session = workspace
            .sessions
            .iter()
            .find(|session| session.session_id == queued_session_id)
            .unwrap();
        assert_eq!(queued_session.status, ActivitySessionStatus::Queued);
        assert_eq!(queued_session.queue.count, 1);
        assert_eq!(queued_session.queue.first_item_id, Some(queue_item_id));
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
}
