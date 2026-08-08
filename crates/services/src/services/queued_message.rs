use std::sync::Arc;

use db::{
    DBService,
    models::{
        agent_message_queue::{
            AgentMessageQueueItem, AgentMessageSource, CreateAgentMessageQueueItem,
            QueuedFollowUpData,
        },
        session::Session,
    },
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, Notify};
use ts_rs::TS;
use uuid::Uuid;

const LEASE_SECONDS: i64 = 120;

#[derive(Debug, Error)]
pub enum QueueError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(
        "session has no configured executor; start the session once before queueing follow-ups"
    )]
    SessionExecutorMissing,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct QueueStatusSummary {
    pub status: QueueStatusKind,
    pub count: usize,
    pub messages: Vec<AgentMessageQueueItem>,
    pub message: Option<AgentMessageQueueItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(use_ts_enum)]
pub enum QueueStatusKind {
    Empty,
    Queued,
}

pub type QueueStatus = QueueStatusSummary;
pub type QueuedMessage = AgentMessageQueueItem;

#[derive(Clone)]
pub struct QueuedMessageService {
    db: DBService,
    notify: Arc<Notify>,
    pump_lock: Arc<Mutex<()>>,
    lease_owner: String,
}

impl QueuedMessageService {
    pub fn new(db: DBService) -> Self {
        Self {
            db,
            notify: Arc::new(Notify::new()),
            pump_lock: Arc::new(Mutex::new(())),
            lease_owner: format!("vk-{}", Uuid::new_v4()),
        }
    }
    pub fn notify(&self) {
        self.notify.notify_waiters();
    }
    pub fn notifier(&self) -> Arc<Notify> {
        self.notify.clone()
    }
    pub fn pump_lock(&self) -> Arc<Mutex<()>> {
        self.pump_lock.clone()
    }
    pub fn lease_owner(&self) -> &str {
        &self.lease_owner
    }
    pub fn lease_duration(&self) -> chrono::Duration {
        chrono::Duration::seconds(LEASE_SECONDS)
    }

    pub async fn queue_message(
        &self,
        session: &Session,
        message: String,
        session_command: Option<executors::actions::session_command::SessionCommand>,
        source: AgentMessageSource,
        priority: Option<i64>,
    ) -> Result<AgentMessageQueueItem, QueueError> {
        if session.executor.as_deref().is_none_or(str::is_empty) {
            return Err(QueueError::SessionExecutorMissing);
        }
        let item = AgentMessageQueueItem::create(
            &self.db.pool,
            &CreateAgentMessageQueueItem {
                session_id: session.id,
                workspace_id: session.workspace_id,
                source,
                priority,
                data: QueuedFollowUpData {
                    message,
                    session_command,
                },
            },
            Uuid::new_v4(),
        )
        .await?;
        self.notify();
        Ok(item)
    }
    pub async fn cancel_queued(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<AgentMessageQueueItem>, QueueError> {
        let v =
            AgentMessageQueueItem::cancel_pending_for_session(&self.db.pool, session_id).await?;
        self.notify();
        Ok(v)
    }
    pub async fn cancel_queued_item(
        &self,
        session_id: Uuid,
        item_id: Uuid,
    ) -> Result<Option<AgentMessageQueueItem>, QueueError> {
        let v = AgentMessageQueueItem::cancel_by_id(&self.db.pool, session_id, item_id).await?;
        self.notify();
        Ok(v)
    }
    pub async fn get_status(&self, session_id: Uuid) -> Result<QueueStatus, QueueError> {
        Ok(Self::status_from_messages(
            AgentMessageQueueItem::list_pending_for_session(&self.db.pool, session_id).await?,
        ))
    }
    pub async fn has_queued(&self, session_id: Uuid) -> Result<bool, QueueError> {
        Ok(
            !AgentMessageQueueItem::list_pending_for_session(&self.db.pool, session_id)
                .await?
                .is_empty(),
        )
    }
    pub async fn recover_stale(&self) -> Result<(), QueueError> {
        AgentMessageQueueItem::recover_stale(&self.db.pool, self.lease_owner()).await?;
        Ok(())
    }
    pub async fn lease_next_batch(
        &self,
        max_concurrent: usize,
    ) -> Result<Vec<AgentMessageQueueItem>, QueueError> {
        self.recover_stale().await?;
        let running = AgentMessageQueueItem::count_running_coding_agents(&self.db.pool).await?;
        let available = (max_concurrent as i64).saturating_sub(running);
        Ok(AgentMessageQueueItem::lease_next_batch(
            &self.db.pool,
            self.lease_owner(),
            available,
            self.lease_duration(),
        )
        .await?)
    }
    pub async fn mark_starting(
        &self,
        item_id: Uuid,
        execution_process_id: Uuid,
    ) -> Result<bool, QueueError> {
        Ok(
            AgentMessageQueueItem::mark_starting(&self.db.pool, item_id, execution_process_id)
                .await?,
        )
    }
    pub async fn mark_running(
        &self,
        item_id: Uuid,
        execution_process_id: Uuid,
    ) -> Result<(), QueueError> {
        Ok(
            AgentMessageQueueItem::mark_running(&self.db.pool, item_id, execution_process_id)
                .await?,
        )
    }
    pub async fn mark_failed(&self, item_id: Uuid, error: &str) -> Result<(), QueueError> {
        Ok(AgentMessageQueueItem::mark_failed(&self.db.pool, item_id, error).await?)
    }

    pub async fn requeue(&self, item_id: Uuid) -> Result<(), QueueError> {
        Ok(AgentMessageQueueItem::requeue(&self.db.pool, item_id).await?)
    }
    pub async fn mark_terminal_for_execution_process(
        &self,
        execution_process_id: Uuid,
        status: db::models::execution_process::ExecutionProcessStatus,
    ) -> Result<(), QueueError> {
        AgentMessageQueueItem::mark_terminal_for_execution_process(
            &self.db.pool,
            execution_process_id,
            status,
        )
        .await?;
        self.notify();
        Ok(())
    }

    fn status_from_messages(messages: Vec<AgentMessageQueueItem>) -> QueueStatus {
        let count = messages.len();
        QueueStatusSummary {
            status: if count == 0 {
                QueueStatusKind::Empty
            } else {
                QueueStatusKind::Queued
            },
            message: messages.first().cloned(),
            count,
            messages,
        }
    }
}
