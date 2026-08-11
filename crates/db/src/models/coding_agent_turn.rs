use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use ts_rs::TS;
use uuid::Uuid;

use super::execution_process::ExecutionProcessStatus;

pub const CODING_AGENT_RESPONSE_SUMMARY_MAX_CHARS: usize = 4096;
pub const CODING_AGENT_PROMPT_PREVIEW_MAX_CHARS: usize = 4096;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, TS)]
pub struct CodingAgentTurn {
    pub id: Uuid,
    pub execution_process_id: Uuid,
    pub agent_session_id: Option<String>,
    pub agent_message_id: Option<String>,
    pub prompt: Option<String>,  // The prompt sent to the executor
    pub summary: Option<String>, // Final assistant message/summary
    pub seen: bool,              // Whether user has viewed this turn
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, TS)]
pub struct CreateCodingAgentTurn {
    pub execution_process_id: Uuid,
    pub prompt: Option<String>,
}

/// Session info from a coding agent turn, used for follow-up requests
#[derive(Debug)]
pub struct CodingAgentResumeInfo {
    pub session_id: String,
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct CodingAgentResponseRecord {
    pub execution_process_id: Uuid,
    pub session_id: Uuid,
    pub workspace_id: Uuid,
    pub status: ExecutionProcessStatus,
    pub completed_at: Option<DateTime<Utc>>,
    pub coding_agent_turn_id: Option<Uuid>,
    pub agent_session_id: Option<String>,
    pub agent_message_id: Option<String>,
    pub summary: Option<String>,
    pub prompt: Option<String>,
}

impl CodingAgentTurn {
    pub fn summary_is_truncated(summary: &str) -> bool {
        summary.len() > CODING_AGENT_RESPONSE_SUMMARY_MAX_CHARS && summary.ends_with("...")
    }

    pub async fn find_response_by_execution_process_id(
        pool: &SqlitePool,
        execution_process_id: Uuid,
    ) -> Result<Option<CodingAgentResponseRecord>, sqlx::Error> {
        sqlx::query_as::<_, CodingAgentResponseRecord>(
            r#"SELECT
                ep.id as execution_process_id,
                ep.session_id,
                s.workspace_id,
                ep.status,
                ep.completed_at,
                cat.id as coding_agent_turn_id,
                cat.agent_session_id,
                cat.agent_message_id,
                cat.summary,
                cat.prompt
               FROM execution_processes ep
               JOIN sessions s ON s.id = ep.session_id
               LEFT JOIN coding_agent_turns cat ON cat.execution_process_id = ep.id
               WHERE ep.id = ?1"#,
        )
        .bind(execution_process_id)
        .fetch_optional(pool)
        .await
    }

    pub async fn find_session_response(
        pool: &SqlitePool,
        session_id: Uuid,
        after_execution_process_id: Option<Uuid>,
        after_completed_at: Option<DateTime<Utc>>,
    ) -> Result<Option<CodingAgentResponseRecord>, sqlx::Error> {
        let has_cursor = after_execution_process_id.is_some() || after_completed_at.is_some();
        let mut sql = String::from(
            r#"SELECT
                ep.id as execution_process_id,
                ep.session_id,
                s.workspace_id,
                ep.status,
                ep.completed_at,
                cat.id as coding_agent_turn_id,
                cat.agent_session_id,
                cat.agent_message_id,
                cat.summary,
                cat.prompt
               FROM execution_processes ep
               JOIN sessions s ON s.id = ep.session_id
               LEFT JOIN coding_agent_turns cat ON cat.execution_process_id = ep.id
               WHERE ep.session_id = ?1
                 AND ep.run_reason = 'codingagent'
                 AND ep.dropped = FALSE
                 AND ep.status = 'completed'"#,
        );

        if after_execution_process_id.is_some() {
            sql.push_str(
                " AND ep.rowid > (SELECT cursor.rowid FROM execution_processes cursor WHERE cursor.id = ?2 AND cursor.session_id = ?1)",
            );
        }
        if after_completed_at.is_some() {
            sql.push_str(if after_execution_process_id.is_some() {
                " AND ep.completed_at > ?3"
            } else {
                " AND ep.completed_at > ?2"
            });
        }
        if has_cursor {
            sql.push_str(" ORDER BY ep.created_at ASC, ep.id ASC LIMIT 1");
        } else {
            sql.push_str(" ORDER BY ep.created_at DESC, ep.id DESC LIMIT 1");
        }

        let mut query = sqlx::query_as::<_, CodingAgentResponseRecord>(&sql).bind(session_id);
        if let Some(after_execution_process_id) = after_execution_process_id {
            query = query.bind(after_execution_process_id);
        }
        if let Some(after_completed_at) = after_completed_at {
            query = query.bind(after_completed_at);
        }

        query.fetch_optional(pool).await
    }

    /// Find session info from the latest coding agent turn for a session.
    /// Only returns turns that have an agent_session_id set.
    pub async fn find_latest_session_info(
        pool: &SqlitePool,
        session_id: Uuid,
    ) -> Result<Option<CodingAgentResumeInfo>, sqlx::Error> {
        sqlx::query_as!(
            CodingAgentResumeInfo,
            r#"SELECT
                cat.agent_session_id as "session_id!",
                cat.agent_message_id as "message_id"
               FROM execution_processes ep
               JOIN coding_agent_turns cat ON ep.id = cat.execution_process_id
               JOIN sessions s ON s.id = ep.session_id
               LEFT JOIN execution_processes reset_ep
                 ON reset_ep.id = s.context_reset_execution_process_id
               WHERE ep.session_id = $1
                 AND ep.run_reason = 'codingagent'
                 AND ep.dropped = FALSE
                 AND cat.agent_session_id IS NOT NULL
                 AND (
                     s.context_reset_execution_process_id IS NULL
                     OR reset_ep.id IS NULL
                     OR ep.rowid > reset_ep.rowid
                 )
               ORDER BY ep.created_at DESC
               LIMIT 1"#,
            session_id
        )
        .fetch_optional(pool)
        .await
    }

    /// Find coding agent turn by execution process ID
    pub async fn find_by_execution_process_id(
        pool: &SqlitePool,
        execution_process_id: Uuid,
    ) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as!(
            CodingAgentTurn,
            r#"SELECT
                id as "id!: Uuid",
                execution_process_id as "execution_process_id!: Uuid",
                agent_session_id,
                agent_message_id,
                prompt,
                summary,
                seen as "seen!: bool",
                created_at as "created_at!: DateTime<Utc>",
                updated_at as "updated_at!: DateTime<Utc>"
               FROM coding_agent_turns
               WHERE execution_process_id = $1"#,
            execution_process_id
        )
        .fetch_optional(pool)
        .await
    }

    /// Create a new coding agent turn
    pub async fn create(
        pool: &SqlitePool,
        data: &CreateCodingAgentTurn,
        id: Uuid,
    ) -> Result<Self, sqlx::Error> {
        let now = Utc::now();

        tracing::debug!(
            "Creating coding agent turn: id={}, execution_process_id={}, agent_session_id=None (will be set later)",
            id,
            data.execution_process_id
        );

        sqlx::query_as!(
            CodingAgentTurn,
            r#"INSERT INTO coding_agent_turns (
                id, execution_process_id, agent_session_id, agent_message_id, prompt, summary, seen,
                created_at, updated_at
               )
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               RETURNING
                id as "id!: Uuid",
                execution_process_id as "execution_process_id!: Uuid",
                agent_session_id,
                agent_message_id,
                prompt,
                summary,
                seen as "seen!: bool",
                created_at as "created_at!: DateTime<Utc>",
                updated_at as "updated_at!: DateTime<Utc>""#,
            id,
            data.execution_process_id,
            None::<String>, // agent_session_id initially None until parsed from output
            None::<String>, // agent_message_id initially None until parsed from output
            data.prompt,
            None::<String>, // summary initially None
            false,          // seen - defaults to unseen
            now,            // created_at
            now             // updated_at
        )
        .fetch_one(pool)
        .await
    }

    /// Update coding agent turn with agent session ID
    pub async fn update_agent_session_id(
        pool: &SqlitePool,
        execution_process_id: Uuid,
        agent_session_id: &str,
    ) -> Result<(), sqlx::Error> {
        let now = Utc::now();
        sqlx::query!(
            r#"UPDATE coding_agent_turns
               SET agent_session_id = $1, updated_at = $2
               WHERE execution_process_id = $3"#,
            agent_session_id,
            now,
            execution_process_id
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    /// Update coding agent turn with agent message ID (for --resume-session-at)
    pub async fn update_agent_message_id(
        pool: &SqlitePool,
        execution_process_id: Uuid,
        agent_message_id: &str,
    ) -> Result<(), sqlx::Error> {
        let now = Utc::now();
        sqlx::query!(
            r#"UPDATE coding_agent_turns
               SET agent_message_id = $1, updated_at = $2
               WHERE execution_process_id = $3"#,
            agent_message_id,
            now,
            execution_process_id
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    /// Update coding agent turn summary
    pub async fn update_summary(
        pool: &SqlitePool,
        execution_process_id: Uuid,
        summary: &str,
    ) -> Result<(), sqlx::Error> {
        let now = Utc::now();
        sqlx::query!(
            r#"UPDATE coding_agent_turns
               SET summary = $1, updated_at = $2
               WHERE execution_process_id = $3"#,
            summary,
            now,
            execution_process_id
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    /// Mark a coding agent turn as unseen by execution process ID.
    pub async fn mark_unseen_by_execution_process_id(
        pool: &SqlitePool,
        execution_process_id: Uuid,
    ) -> Result<(), sqlx::Error> {
        let now = Utc::now();
        sqlx::query(
            r#"UPDATE coding_agent_turns
               SET seen = 0, updated_at = ?
               WHERE execution_process_id = ?
                 AND seen = 1"#,
        )
        .bind(now)
        .bind(execution_process_id.to_string())
        .execute(pool)
        .await?;

        Ok(())
    }

    /// Mark all coding agent turns for a workspace as seen
    pub async fn mark_seen_by_workspace_id(
        pool: &SqlitePool,
        workspace_id: Uuid,
    ) -> Result<(), sqlx::Error> {
        let now = Utc::now();
        sqlx::query!(
            r#"UPDATE coding_agent_turns
               SET seen = 1, updated_at = $1
               WHERE execution_process_id IN (
                   SELECT ep.id FROM execution_processes ep
                   JOIN sessions s ON ep.session_id = s.id
                   WHERE s.workspace_id = $2
               ) AND seen = 0"#,
            now,
            workspace_id
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    /// Check if a workspace has any unseen coding agent turns
    /// Find all workspaces that have unseen coding agent turns, filtered by archived status
    pub async fn find_workspaces_with_unseen(
        pool: &SqlitePool,
        archived: bool,
    ) -> Result<std::collections::HashSet<Uuid>, sqlx::Error> {
        let result: Vec<Uuid> = sqlx::query_scalar!(
            r#"SELECT DISTINCT s.workspace_id as "workspace_id!: Uuid"
               FROM coding_agent_turns cat
               JOIN execution_processes ep ON cat.execution_process_id = ep.id
               JOIN sessions s ON ep.session_id = s.id
               JOIN workspaces w ON s.workspace_id = w.id
               WHERE cat.seen = 0 AND w.archived = $1"#,
            archived
        )
        .fetch_all(pool)
        .await?;

        Ok(result.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use sqlx::{Executor, SqlitePool, sqlite::SqlitePoolOptions};
    use uuid::Uuid;

    use super::{
        CODING_AGENT_RESPONSE_SUMMARY_MAX_CHARS, CodingAgentTurn, CodingAgentTurn as Turn,
    };
    use crate::models::execution_process::ExecutionProcessStatus;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        pool.execute(
            r#"CREATE TABLE sessions (
                id BLOB PRIMARY KEY,
                workspace_id BLOB NOT NULL,
                executor TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
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
                started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                completed_at TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )"#,
        )
        .await
        .unwrap();
        pool.execute(
            r#"CREATE TABLE coding_agent_turns (
                id BLOB PRIMARY KEY,
                execution_process_id BLOB NOT NULL,
                agent_session_id TEXT,
                agent_message_id TEXT,
                prompt TEXT,
                summary TEXT,
                seen INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
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

    async fn insert_process(
        pool: &SqlitePool,
        id: Uuid,
        session_id: Uuid,
        status: &str,
        created_at: chrono::DateTime<Utc>,
        completed_at: Option<chrono::DateTime<Utc>>,
    ) {
        sqlx::query(
            r#"INSERT INTO execution_processes
               (id, session_id, run_reason, executor_action, status, dropped, started_at, completed_at, created_at, updated_at)
               VALUES (?1, ?2, 'codingagent', '{}', ?3, 0, ?4, ?5, ?4, ?4)"#,
        )
        .bind(id)
        .bind(session_id)
        .bind(status)
        .bind(created_at)
        .bind(completed_at)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_turn(pool: &SqlitePool, execution_process_id: Uuid, summary: Option<&str>) {
        sqlx::query(
            r#"INSERT INTO coding_agent_turns
               (id, execution_process_id, agent_session_id, agent_message_id, prompt, summary)
               VALUES (?1, ?2, 'agent-session', 'agent-message', ?3, ?4)"#,
        )
        .bind(Uuid::new_v4())
        .bind(execution_process_id)
        .bind("prompt")
        .bind(summary)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn finds_response_by_execution_process_id_for_completed_and_running_turns() {
        let pool = test_pool().await;
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let completed_id = Uuid::new_v4();
        let running_id = Uuid::new_v4();
        insert_session(&pool, session_id, workspace_id).await;
        insert_process(
            &pool,
            completed_id,
            session_id,
            "completed",
            Utc.with_ymd_and_hms(2026, 7, 30, 10, 0, 0).unwrap(),
            Some(Utc.with_ymd_and_hms(2026, 7, 30, 10, 1, 0).unwrap()),
        )
        .await;
        insert_turn(&pool, completed_id, Some("final response")).await;
        insert_process(
            &pool,
            running_id,
            session_id,
            "running",
            Utc.with_ymd_and_hms(2026, 7, 30, 10, 2, 0).unwrap(),
            None,
        )
        .await;
        insert_turn(&pool, running_id, None).await;

        let completed = CodingAgentTurn::find_response_by_execution_process_id(&pool, completed_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.workspace_id, workspace_id);
        assert_eq!(completed.status, ExecutionProcessStatus::Completed);
        assert_eq!(completed.summary.as_deref(), Some("final response"));
        assert_eq!(completed.prompt.as_deref(), Some("prompt"));

        let running = CodingAgentTurn::find_response_by_execution_process_id(&pool, running_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(running.status, ExecutionProcessStatus::Running);
        assert!(running.summary.is_none());
    }

    #[tokio::test]
    async fn finds_latest_and_next_session_response_after_cursor() {
        let pool = test_pool().await;
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        insert_session(&pool, session_id, workspace_id).await;
        insert_process(
            &pool,
            first_id,
            session_id,
            "completed",
            Utc.with_ymd_and_hms(2026, 7, 30, 10, 0, 0).unwrap(),
            Some(Utc.with_ymd_and_hms(2026, 7, 30, 10, 1, 0).unwrap()),
        )
        .await;
        insert_turn(&pool, first_id, Some("first")).await;
        insert_process(
            &pool,
            second_id,
            session_id,
            "completed",
            Utc.with_ymd_and_hms(2026, 7, 30, 10, 2, 0).unwrap(),
            Some(Utc.with_ymd_and_hms(2026, 7, 30, 10, 3, 0).unwrap()),
        )
        .await;
        insert_turn(&pool, second_id, Some("second")).await;

        let latest = Turn::find_session_response(&pool, session_id, None, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(latest.execution_process_id, second_id);

        let next_after_id = Turn::find_session_response(&pool, session_id, Some(first_id), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next_after_id.execution_process_id, second_id);

        let after_completed_at = Utc.with_ymd_and_hms(2026, 7, 30, 10, 1, 0).unwrap();
        let next_after_completed_at =
            Turn::find_session_response(&pool, session_id, None, Some(after_completed_at))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(next_after_completed_at.execution_process_id, second_id);
    }

    #[tokio::test]
    async fn session_response_skips_failed_and_killed_after_cursor() {
        let pool = test_pool().await;
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let cursor_id = Uuid::new_v4();
        let failed_id = Uuid::new_v4();
        let killed_id = Uuid::new_v4();
        let completed_id = Uuid::new_v4();
        insert_session(&pool, session_id, workspace_id).await;
        insert_process(
            &pool,
            cursor_id,
            session_id,
            "completed",
            Utc.with_ymd_and_hms(2026, 7, 30, 10, 0, 0).unwrap(),
            Some(Utc.with_ymd_and_hms(2026, 7, 30, 10, 1, 0).unwrap()),
        )
        .await;
        insert_turn(&pool, cursor_id, Some("cursor")).await;
        insert_process(
            &pool,
            failed_id,
            session_id,
            "failed",
            Utc.with_ymd_and_hms(2026, 7, 30, 10, 2, 0).unwrap(),
            Some(Utc.with_ymd_and_hms(2026, 7, 30, 10, 3, 0).unwrap()),
        )
        .await;
        insert_turn(&pool, failed_id, Some("failed should not satisfy")).await;
        insert_process(
            &pool,
            killed_id,
            session_id,
            "killed",
            Utc.with_ymd_and_hms(2026, 7, 30, 10, 4, 0).unwrap(),
            Some(Utc.with_ymd_and_hms(2026, 7, 30, 10, 5, 0).unwrap()),
        )
        .await;
        insert_turn(&pool, killed_id, Some("killed should not satisfy")).await;

        let none_after_cursor =
            Turn::find_session_response(&pool, session_id, Some(cursor_id), None)
                .await
                .unwrap();
        assert!(
            none_after_cursor.is_none(),
            "failed/killed turns should not satisfy next completed response lookup"
        );

        insert_process(
            &pool,
            completed_id,
            session_id,
            "completed",
            Utc.with_ymd_and_hms(2026, 7, 30, 10, 6, 0).unwrap(),
            Some(Utc.with_ymd_and_hms(2026, 7, 30, 10, 7, 0).unwrap()),
        )
        .await;
        insert_turn(&pool, completed_id, Some("completed")).await;

        let next_after_cursor =
            Turn::find_session_response(&pool, session_id, Some(cursor_id), None)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(next_after_cursor.execution_process_id, completed_id);
    }

    #[test]
    fn detects_summary_truncation_marker() {
        let truncated = format!("{}...", "x".repeat(CODING_AGENT_RESPONSE_SUMMARY_MAX_CHARS));
        assert!(CodingAgentTurn::summary_is_truncated(&truncated));
        assert!(!CodingAgentTurn::summary_is_truncated("short..."));
    }
}
