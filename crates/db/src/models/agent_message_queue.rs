use chrono::{DateTime, Duration, Utc};
use executors::actions::session_command::SessionCommand;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row, SqlitePool, Type, sqlite::SqliteRow};
use ts_rs::TS;
use uuid::Uuid;

use super::execution_process::{ExecutionProcess, ExecutionProcessStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[ts(use_ts_enum)]
pub enum AgentMessageQueueStatus {
    Queued,
    Leased,
    Starting,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[ts(use_ts_enum)]
pub enum AgentMessageSource {
    FromUser,
    Workflow,
    Agent,
    System,
}

impl AgentMessageSource {
    pub fn default_priority(self) -> i64 {
        match self {
            Self::FromUser => 100,
            Self::Workflow => 60,
            Self::Agent => 50,
            Self::System => 25,
        }
    }
}

impl Default for AgentMessageSource {
    fn default() -> Self {
        Self::Agent
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct QueuedFollowUpData {
    pub message: String,
    #[serde(default)]
    pub session_command: Option<SessionCommand>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct CreateAgentMessageQueueItem {
    pub session_id: Uuid,
    pub workspace_id: Uuid,
    pub source: AgentMessageSource,
    pub priority: Option<i64>,
    pub data: QueuedFollowUpData,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct AgentMessageQueueItem {
    pub id: Uuid,
    pub session_id: Uuid,
    pub workspace_id: Uuid,
    pub status: AgentMessageQueueStatus,
    pub source: AgentMessageSource,
    pub priority: i64,
    pub data: QueuedFollowUpData,
    pub started_execution_process_id: Option<Uuid>,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub attempt_count: i64,
    pub last_error: Option<String>,
    pub queued_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl<'r> FromRow<'r, SqliteRow> for AgentMessageQueueItem {
    fn from_row(row: &'r SqliteRow) -> Result<Self, sqlx::Error> {
        let data_json: String = row.try_get("data")?;
        let data =
            serde_json::from_str(&data_json).map_err(|source| sqlx::Error::ColumnDecode {
                index: "\"data\"".to_string(),
                source: Box::new(source),
            })?;

        Ok(Self {
            id: row.try_get("id")?,
            session_id: row.try_get("session_id")?,
            workspace_id: row.try_get("workspace_id")?,
            status: row.try_get("status")?,
            source: row.try_get("source")?,
            priority: row.try_get("priority")?,
            data,
            started_execution_process_id: row.try_get("started_execution_process_id")?,
            lease_owner: row.try_get("lease_owner")?,
            lease_expires_at: row.try_get("lease_expires_at")?,
            attempt_count: row.try_get("attempt_count")?,
            last_error: row.try_get("last_error")?,
            queued_at: row.try_get("queued_at")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

impl AgentMessageQueueItem {
    fn select_sql(where_clause: &str) -> String {
        format!(
            r#"SELECT id, session_id, workspace_id, status, source, priority,
                      data, started_execution_process_id, lease_owner,
                      lease_expires_at, attempt_count, last_error, queued_at,
                      created_at, updated_at
               FROM agent_message_queue {where_clause}"#
        )
    }

    pub async fn create(
        pool: &SqlitePool,
        input: &CreateAgentMessageQueueItem,
        id: Uuid,
    ) -> Result<Self, sqlx::Error> {
        let now = Utc::now();
        let priority = input
            .priority
            .unwrap_or_else(|| input.source.default_priority());
        let data = serde_json::to_string(&input.data).map_err(sqlx::Error::decode)?;
        sqlx::query(
            r#"INSERT INTO agent_message_queue (
                id, session_id, workspace_id, status, source, priority, data,
                attempt_count, queued_at, created_at, updated_at
            ) VALUES (?1, ?2, ?3, 'queued', ?4, ?5, ?6, 0, ?7, ?7, ?7)"#,
        )
        .bind(id)
        .bind(input.session_id)
        .bind(input.workspace_id)
        .bind(input.source)
        .bind(priority)
        .bind(data)
        .bind(now)
        .execute(pool)
        .await?;
        Self::find_by_id(pool, id)
            .await?
            .ok_or(sqlx::Error::RowNotFound)
    }

    pub async fn find_by_id(pool: &SqlitePool, id: Uuid) -> Result<Option<Self>, sqlx::Error> {
        let sql = Self::select_sql("WHERE id = ?1");
        sqlx::query_as::<_, Self>(&sql)
            .bind(id)
            .fetch_optional(pool)
            .await
    }

    pub async fn list_pending_for_session(
        pool: &SqlitePool,
        session_id: Uuid,
    ) -> Result<Vec<Self>, sqlx::Error> {
        let sql = Self::select_sql(
            "WHERE session_id = ?1 AND status IN ('queued','leased','starting','running') ORDER BY queued_at ASC, id ASC",
        );
        sqlx::query_as::<_, Self>(&sql)
            .bind(session_id)
            .fetch_all(pool)
            .await
    }

    pub async fn cancel_pending_for_session(
        pool: &SqlitePool,
        session_id: Uuid,
    ) -> Result<Vec<Self>, sqlx::Error> {
        let items = {
            let sql = Self::select_sql(
                "WHERE session_id = ?1 AND status IN ('queued','leased') ORDER BY queued_at ASC, id ASC",
            );
            sqlx::query_as::<_, Self>(&sql)
                .bind(session_id)
                .fetch_all(pool)
                .await?
        };
        sqlx::query(
            r#"UPDATE agent_message_queue
               SET status = 'cancelled', lease_owner = NULL, lease_expires_at = NULL, updated_at = ?2
               WHERE session_id = ?1 AND status IN ('queued','leased')"#,
        )
        .bind(session_id)
        .bind(Utc::now())
        .execute(pool)
        .await?;
        Ok(items)
    }

    pub async fn cancel_by_id(
        pool: &SqlitePool,
        session_id: Uuid,
        id: Uuid,
    ) -> Result<Option<Self>, sqlx::Error> {
        let existing = Self::find_by_id(pool, id).await?;
        let Some(item) = existing else {
            return Ok(None);
        };
        if item.session_id != session_id
            || !matches!(
                item.status,
                AgentMessageQueueStatus::Queued | AgentMessageQueueStatus::Leased
            )
        {
            return Ok(Some(item));
        }
        sqlx::query(
            r#"UPDATE agent_message_queue
               SET status = 'cancelled', lease_owner = NULL, lease_expires_at = NULL, updated_at = ?2
               WHERE id = ?1 AND status IN ('queued','leased')"#,
        )
        .bind(id)
        .bind(Utc::now())
        .execute(pool)
        .await?;
        Self::find_by_id(pool, id).await
    }

    pub async fn count_running_coding_agents(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM execution_processes WHERE status = 'running' AND run_reason = 'codingagent'",
        )
        .fetch_one(pool)
        .await
    }

    pub async fn lease_next_batch(
        pool: &SqlitePool,
        lease_owner: &str,
        limit: i64,
        lease_for: Duration,
    ) -> Result<Vec<Self>, sqlx::Error> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let now = Utc::now();
        let expires = now + lease_for;
        let candidates: Vec<Uuid> = sqlx::query_scalar(
            r#"SELECT q.id FROM agent_message_queue q
               WHERE q.status = 'queued'
                 AND NOT EXISTS (
                   SELECT 1 FROM execution_processes ep
                   JOIN sessions s ON s.id = ep.session_id
                   WHERE s.workspace_id = q.workspace_id
                     AND ep.status = 'running'
                     AND ep.run_reason != 'devserver'
                 )
                 AND NOT EXISTS (
                   SELECT 1 FROM agent_message_queue earlier
                   WHERE earlier.workspace_id = q.workspace_id
                     AND earlier.status = 'queued'
                     AND (
                       earlier.priority > q.priority
                       OR (earlier.priority = q.priority AND earlier.queued_at < q.queued_at)
                       OR (earlier.priority = q.priority AND earlier.queued_at = q.queued_at AND earlier.id < q.id)
                     )
                 )
               ORDER BY q.priority DESC, q.queued_at ASC, q.id ASC
               LIMIT ?1"#,
        )
        .bind(limit)
        .fetch_all(pool)
        .await?;
        let mut leased = Vec::new();
        for id in candidates {
            let result = sqlx::query(
                r#"UPDATE agent_message_queue
                   SET status = 'leased', lease_owner = ?2, lease_expires_at = ?3,
                       attempt_count = attempt_count + 1, updated_at = ?4
                   WHERE id = ?1 AND status = 'queued'"#,
            )
            .bind(id)
            .bind(lease_owner)
            .bind(expires)
            .bind(now)
            .execute(pool)
            .await?;
            if result.rows_affected() == 1
                && let Some(item) = Self::find_by_id(pool, id).await?
            {
                leased.push(item);
            }
        }
        Ok(leased)
    }

    pub async fn mark_starting(
        pool: &SqlitePool,
        id: Uuid,
        execution_process_id: Uuid,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"UPDATE agent_message_queue
               SET status = 'starting', started_execution_process_id = ?2, updated_at = ?3
               WHERE id = ?1 AND status = 'leased'"#,
        )
        .bind(id)
        .bind(execution_process_id)
        .bind(Utc::now())
        .execute(pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn mark_running(pool: &SqlitePool, id: Uuid) -> Result<(), sqlx::Error> {
        Self::set_status(pool, id, AgentMessageQueueStatus::Running, None).await
    }

    pub async fn mark_failed(pool: &SqlitePool, id: Uuid, error: &str) -> Result<(), sqlx::Error> {
        Self::set_status(pool, id, AgentMessageQueueStatus::Failed, Some(error)).await
    }

    pub async fn requeue(pool: &SqlitePool, id: Uuid) -> Result<(), sqlx::Error> {
        Self::set_status(pool, id, AgentMessageQueueStatus::Queued, None).await
    }

    pub async fn mark_terminal_for_execution_process(
        pool: &SqlitePool,
        execution_process_id: Uuid,
        status: ExecutionProcessStatus,
    ) -> Result<(), sqlx::Error> {
        let queue_status = match status {
            ExecutionProcessStatus::Completed => AgentMessageQueueStatus::Completed,
            ExecutionProcessStatus::Failed | ExecutionProcessStatus::Killed => {
                AgentMessageQueueStatus::Failed
            }
            ExecutionProcessStatus::Running => return Ok(()),
        };
        sqlx::query(
            r#"UPDATE agent_message_queue
               SET status = ?2, lease_owner = NULL, lease_expires_at = NULL, updated_at = ?3
               WHERE started_execution_process_id = ?1 AND status IN ('starting','running')"#,
        )
        .bind(execution_process_id)
        .bind(queue_status)
        .bind(Utc::now())
        .execute(pool)
        .await?;
        Ok(())
    }

    pub async fn recover_stale(pool: &SqlitePool, lease_owner: &str) -> Result<(), sqlx::Error> {
        let now = Utc::now();
        sqlx::query(
            r#"UPDATE agent_message_queue
               SET status = 'queued', lease_owner = NULL, lease_expires_at = NULL, updated_at = ?1
               WHERE status = 'leased' AND lease_expires_at IS NOT NULL AND lease_expires_at < ?1"#,
        )
        .bind(now)
        .execute(pool)
        .await?;
        let starting_sql = Self::select_sql(
            "WHERE status = 'starting' AND lease_owner = ?1 AND lease_expires_at IS NOT NULL AND lease_expires_at < ?2",
        );
        let starting: Vec<Self> = sqlx::query_as::<_, Self>(&starting_sql)
            .bind(lease_owner)
            .bind(now)
            .fetch_all(pool)
            .await?;
        for item in starting {
            match item.started_execution_process_id.and_then(|id| Some(id)) {
                Some(process_id) => match ExecutionProcess::find_by_id(pool, process_id).await? {
                    Some(process) if process.status == ExecutionProcessStatus::Running => {
                        Self::set_status(pool, item.id, AgentMessageQueueStatus::Running, None)
                            .await?;
                    }
                    Some(process) => {
                        Self::mark_terminal_for_execution_process(pool, process.id, process.status)
                            .await?
                    }
                    None => {
                        Self::set_status(pool, item.id, AgentMessageQueueStatus::Queued, None)
                            .await?
                    }
                },
                None => {
                    Self::set_status(pool, item.id, AgentMessageQueueStatus::Queued, None).await?
                }
            }
        }
        let running_sql = Self::select_sql("WHERE status = 'running'");
        let running: Vec<Self> = sqlx::query_as::<_, Self>(&running_sql)
            .fetch_all(pool)
            .await?;
        for item in running {
            if let Some(process_id) = item.started_execution_process_id {
                match ExecutionProcess::find_by_id(pool, process_id).await? {
                    Some(process) => {
                        Self::mark_terminal_for_execution_process(pool, process.id, process.status)
                            .await?
                    }
                    None => {
                        Self::mark_failed(
                            pool,
                            item.id,
                            "execution process missing during queue recovery",
                        )
                        .await?
                    }
                }
            }
        }
        Ok(())
    }

    async fn set_status(
        pool: &SqlitePool,
        id: Uuid,
        status: AgentMessageQueueStatus,
        error: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"UPDATE agent_message_queue
               SET status = ?2, last_error = COALESCE(?3, last_error),
                   lease_owner = CASE WHEN ?2 IN ('queued','completed','failed','cancelled') THEN NULL ELSE lease_owner END,
                   lease_expires_at = CASE WHEN ?2 IN ('queued','completed','failed','cancelled') THEN NULL ELSE lease_expires_at END,
                   updated_at = ?4
               WHERE id = ?1"#,
        )
        .bind(id)
        .bind(status)
        .bind(error)
        .bind(Utc::now())
        .execute(pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use sqlx::{Executor, SqlitePool, sqlite::SqlitePoolOptions};
    use uuid::Uuid;

    use super::{
        AgentMessageQueueItem, AgentMessageQueueStatus, AgentMessageSource,
        CreateAgentMessageQueueItem, QueuedFollowUpData,
    };

    async fn test_pool() -> SqlitePool {
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
                dropped BOOLEAN NOT NULL DEFAULT FALSE,
                started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                completed_at TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
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
        pool
    }

    async fn insert_session(pool: &SqlitePool, session_id: Uuid, workspace_id: Uuid) {
        sqlx::query("INSERT INTO sessions (id, workspace_id) VALUES (?1, ?2)")
            .bind(session_id)
            .bind(workspace_id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn create_item(
        pool: &SqlitePool,
        session_id: Uuid,
        workspace_id: Uuid,
        source: AgentMessageSource,
        priority: Option<i64>,
        message: &str,
    ) -> AgentMessageQueueItem {
        AgentMessageQueueItem::create(
            pool,
            &CreateAgentMessageQueueItem {
                session_id,
                workspace_id,
                source,
                priority,
                data: QueuedFollowUpData {
                    message: message.to_string(),
                    session_command: None,
                },
            },
            Uuid::new_v4(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn enqueues_multiple_items_and_lists_fifo() {
        let pool = test_pool().await;
        let session_id = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();
        insert_session(&pool, session_id, workspace_id).await;

        let first = create_item(
            &pool,
            session_id,
            workspace_id,
            AgentMessageSource::Agent,
            None,
            "first",
        )
        .await;
        let second = create_item(
            &pool,
            session_id,
            workspace_id,
            AgentMessageSource::Agent,
            None,
            "second",
        )
        .await;

        let pending = AgentMessageQueueItem::list_pending_for_session(&pool, session_id)
            .await
            .unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].id, first.id);
        assert_eq!(pending[1].id, second.id);
        assert_eq!(pending[0].data.message, "first");
        assert_eq!(pending[1].data.message, "second");
    }

    #[tokio::test]
    async fn lease_uses_strict_priority_then_fifo() {
        let pool = test_pool().await;
        let workspace_id = Uuid::new_v4();
        let low_session = Uuid::new_v4();
        let high_session = Uuid::new_v4();
        insert_session(&pool, low_session, workspace_id).await;
        insert_session(&pool, high_session, Uuid::new_v4()).await;

        let low = create_item(
            &pool,
            low_session,
            workspace_id,
            AgentMessageSource::Agent,
            Some(10),
            "low",
        )
        .await;
        let high = create_item(
            &pool,
            high_session,
            Uuid::new_v4(),
            AgentMessageSource::FromUser,
            Some(100),
            "high",
        )
        .await;

        let leased =
            AgentMessageQueueItem::lease_next_batch(&pool, "test-owner", 2, Duration::seconds(60))
                .await
                .unwrap();

        assert_eq!(
            leased.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![high.id, low.id]
        );
        assert!(
            leased
                .iter()
                .all(|item| item.status == AgentMessageQueueStatus::Leased)
        );
    }

    #[tokio::test]
    async fn lease_selects_at_most_one_item_per_workspace() {
        let pool = test_pool().await;
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        insert_session(&pool, session_id, workspace_id).await;

        let first = create_item(
            &pool,
            session_id,
            workspace_id,
            AgentMessageSource::FromUser,
            Some(100),
            "first",
        )
        .await;
        let _second = create_item(
            &pool,
            session_id,
            workspace_id,
            AgentMessageSource::FromUser,
            Some(100),
            "second",
        )
        .await;

        let leased =
            AgentMessageQueueItem::lease_next_batch(&pool, "test-owner", 10, Duration::seconds(60))
                .await
                .unwrap();

        assert_eq!(
            leased.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![first.id]
        );
    }

    #[tokio::test]
    async fn cancellation_supports_session_and_item_scope() {
        let pool = test_pool().await;
        let session_id = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();
        insert_session(&pool, session_id, workspace_id).await;

        let first = create_item(
            &pool,
            session_id,
            workspace_id,
            AgentMessageSource::Agent,
            None,
            "first",
        )
        .await;
        let second = create_item(
            &pool,
            session_id,
            workspace_id,
            AgentMessageSource::Agent,
            None,
            "second",
        )
        .await;

        let cancelled = AgentMessageQueueItem::cancel_by_id(&pool, session_id, first.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cancelled.status, AgentMessageQueueStatus::Cancelled);

        let cancelled_all = AgentMessageQueueItem::cancel_pending_for_session(&pool, session_id)
            .await
            .unwrap();
        assert_eq!(
            cancelled_all.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![second.id]
        );

        let pending = AgentMessageQueueItem::list_pending_for_session(&pool, session_id)
            .await
            .unwrap();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn lease_skips_workspaces_with_running_non_devserver_processes_and_recovers_stale_leases()
    {
        let pool = test_pool().await;
        let busy_workspace = Uuid::new_v4();
        let free_workspace = Uuid::new_v4();
        let busy_session = Uuid::new_v4();
        let free_session = Uuid::new_v4();
        insert_session(&pool, busy_session, busy_workspace).await;
        insert_session(&pool, free_session, free_workspace).await;

        let busy = create_item(
            &pool,
            busy_session,
            busy_workspace,
            AgentMessageSource::FromUser,
            None,
            "busy",
        )
        .await;
        let free = create_item(
            &pool,
            free_session,
            free_workspace,
            AgentMessageSource::Agent,
            None,
            "free",
        )
        .await;

        sqlx::query(
            "INSERT INTO execution_processes (id, session_id, run_reason, status) VALUES (?1, ?2, 'codingagent', 'running')",
        )
        .bind(Uuid::new_v4())
        .bind(busy_session)
        .execute(&pool)
        .await
        .unwrap();

        let leased =
            AgentMessageQueueItem::lease_next_batch(&pool, "test-owner", 10, Duration::seconds(60))
                .await
                .unwrap();
        assert_eq!(
            leased.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![free.id]
        );

        sqlx::query(
            "UPDATE agent_message_queue SET status = 'leased', lease_owner = 'old-owner', lease_expires_at = ?2 WHERE id = ?1",
        )
        .bind(busy.id)
        .bind(Utc::now() - Duration::seconds(1))
        .execute(&pool)
        .await
        .unwrap();

        AgentMessageQueueItem::recover_stale(&pool, "test-owner")
            .await
            .unwrap();
        let recovered = AgentMessageQueueItem::find_by_id(&pool, busy.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered.status, AgentMessageQueueStatus::Queued);
        assert!(recovered.lease_owner.is_none());
    }
}
