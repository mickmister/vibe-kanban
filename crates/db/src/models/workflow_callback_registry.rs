use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool, Type};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[ts(use_ts_enum)]
pub enum WorkflowCallbackKind {
    WorkflowCompletion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[ts(use_ts_enum)]
pub enum WorkflowCallbackStatus {
    Pending,
    Delivered,
    Failed,
    Superseded,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, TS)]
pub struct WorkflowCallbackRegistryItem {
    pub id: Uuid,
    pub callback_key: String,
    pub workspace_id: Uuid,
    pub target_session_id: Uuid,
    pub kind: WorkflowCallbackKind,
    pub status: WorkflowCallbackStatus,
    pub workflow_run_id: String,
    pub workflow_name: Option<String>,
    pub workflow_design_id: Option<String>,
    pub workflow_version: Option<i64>,
    pub delivered_ref: Option<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct UpsertWorkflowCallbackRegistryItem {
    pub callback_key: String,
    pub workspace_id: Uuid,
    pub target_session_id: Uuid,
    pub kind: WorkflowCallbackKind,
    pub workflow_run_id: String,
    pub workflow_name: Option<String>,
    pub workflow_design_id: Option<String>,
    pub workflow_version: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct UpdateWorkflowCallbackRegistryStatus {
    pub callback_key: String,
    pub status: WorkflowCallbackStatus,
    pub delivered_ref: Option<String>,
    pub error_message: Option<String>,
}

impl WorkflowCallbackRegistryItem {
    pub async fn upsert_pending(
        pool: &SqlitePool,
        input: &UpsertWorkflowCallbackRegistryItem,
    ) -> Result<Self, sqlx::Error> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        sqlx::query(
            r#"INSERT OR IGNORE INTO workflow_callback_registry (
                id, callback_key, workspace_id, target_session_id, kind, status,
                workflow_run_id, workflow_name, workflow_design_id, workflow_version,
                created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7, ?8, ?9, ?10, ?10)"#,
        )
        .bind(id)
        .bind(&input.callback_key)
        .bind(input.workspace_id)
        .bind(input.target_session_id)
        .bind(input.kind)
        .bind(&input.workflow_run_id)
        .bind(&input.workflow_name)
        .bind(&input.workflow_design_id)
        .bind(input.workflow_version)
        .bind(now)
        .execute(pool)
        .await?;
        let existing = Self::find_by_key(pool, &input.callback_key)
            .await?
            .ok_or(sqlx::Error::RowNotFound)?;
        if !existing.matches_identity(input) {
            return Err(sqlx::Error::Protocol(
                "Callback key already exists for a different workflow callback.".to_string(),
            ));
        }
        Ok(existing)
    }

    pub async fn update_status(
        pool: &SqlitePool,
        input: &UpdateWorkflowCallbackRegistryStatus,
    ) -> Result<Self, sqlx::Error> {
        let existing = Self::find_by_key(pool, &input.callback_key)
            .await?
            .ok_or(sqlx::Error::RowNotFound)?;
        if existing.status.is_terminal() || input.status == WorkflowCallbackStatus::Pending {
            return Ok(existing);
        }

        let now = Utc::now();
        sqlx::query(
            r#"UPDATE workflow_callback_registry
               SET status = ?2,
                   delivered_ref = CASE WHEN ?2 = 'delivered' THEN COALESCE(?3, delivered_ref) ELSE delivered_ref END,
                   error_message = CASE WHEN ?2 = 'failed' THEN ?4 ELSE NULL END,
                   updated_at = ?5
               WHERE callback_key = ?1"#,
        )
        .bind(&input.callback_key)
        .bind(input.status)
        .bind(&input.delivered_ref)
        .bind(&input.error_message)
        .bind(now)
        .execute(pool)
        .await?;
        Self::find_by_key(pool, &input.callback_key)
            .await?
            .ok_or(sqlx::Error::RowNotFound)
    }

    pub async fn find_by_key(
        pool: &SqlitePool,
        callback_key: &str,
    ) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as::<_, Self>(
            r#"SELECT id, callback_key, workspace_id, target_session_id, kind, status,
                      workflow_run_id, workflow_name, workflow_design_id, workflow_version,
                      delivered_ref, error_message, created_at, updated_at
               FROM workflow_callback_registry
               WHERE callback_key = ?1"#,
        )
        .bind(callback_key)
        .fetch_optional(pool)
        .await
    }

    pub async fn list_recent(
        pool: &SqlitePool,
        workspace_id: Option<Uuid>,
        session_id: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<Self>, sqlx::Error> {
        let mut query = String::from(
            r#"SELECT id, callback_key, workspace_id, target_session_id, kind, status,
                      workflow_run_id, workflow_name, workflow_design_id, workflow_version,
                      delivered_ref, error_message, created_at, updated_at
               FROM workflow_callback_registry
               WHERE 1 = 1"#,
        );
        if workspace_id.is_some() {
            query.push_str(" AND workspace_id = ?");
        }
        if session_id.is_some() {
            query.push_str(" AND target_session_id = ?");
        }
        query.push_str(" ORDER BY updated_at DESC LIMIT ?");
        let mut query = sqlx::query_as::<_, Self>(&query);
        if let Some(workspace_id) = workspace_id {
            query = query.bind(workspace_id);
        }
        if let Some(session_id) = session_id {
            query = query.bind(session_id);
        }
        query.bind(limit).fetch_all(pool).await
    }

    fn matches_identity(&self, input: &UpsertWorkflowCallbackRegistryItem) -> bool {
        self.workspace_id == input.workspace_id
            && self.target_session_id == input.target_session_id
            && self.kind == input.kind
            && self.workflow_run_id == input.workflow_run_id
            && self.workflow_name == input.workflow_name
            && self.workflow_design_id == input.workflow_design_id
            && self.workflow_version == input.workflow_version
    }
}

impl WorkflowCallbackStatus {
    fn is_terminal(self) -> bool {
        matches!(
            self,
            WorkflowCallbackStatus::Delivered
                | WorkflowCallbackStatus::Failed
                | WorkflowCallbackStatus::Superseded
        )
    }
}
