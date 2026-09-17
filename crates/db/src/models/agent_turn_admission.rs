use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum AgentTurnAdmissionStatus {
    Reserved,
    Started,
    Released,
    Expired,
}

#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct AgentTurnAdmission {
    pub token_id: Uuid,
    pub operation_key: String,
    pub queue_item_id: Option<Uuid>,
    pub intended_process_id: Option<Uuid>,
    pub workspace_id: Uuid,
    pub fence: i64,
    pub status: AgentTurnAdmissionStatus,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireAgentTurnAdmission {
    Acquired(AgentTurnAdmission),
    CapacityUnavailable,
    WorkspaceBusy,
    IdentityConflict,
}

impl AgentTurnAdmission {
    async fn find(pool: &SqlitePool, operation_key: &str) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as("SELECT * FROM agent_turn_admissions WHERE operation_key = ?1")
            .bind(operation_key)
            .fetch_optional(pool)
            .await
    }

    /// Reconciles reservations against durable process state. A reservation is
    /// retained while its process is running, but no longer double-counts the
    /// process against global capacity.
    pub async fn reconcile(pool: &SqlitePool, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"UPDATE agent_turn_admissions AS a SET status = 'started', expires_at = NULL, updated_at = ?1
               WHERE a.status = 'reserved' AND EXISTS (
                 SELECT 1 FROM execution_processes ep
                 WHERE ep.id = a.intended_process_id AND ep.status = 'running'
               )"#,
        )
        .bind(now)
        .execute(pool)
        .await?;
        sqlx::query(
            r#"UPDATE agent_turn_admissions SET status = 'expired', updated_at = ?1
               WHERE status = 'reserved' AND expires_at IS NOT NULL AND expires_at <= ?1
                 AND NOT EXISTS (
                   SELECT 1 FROM execution_processes ep
                   WHERE ep.id = agent_turn_admissions.intended_process_id AND ep.status = 'running'
                 )"#,
        )
        .bind(now)
        .execute(pool)
        .await?;
        sqlx::query(
            r#"UPDATE agent_turn_admissions SET status = 'released', updated_at = ?1
               WHERE status = 'started' AND NOT EXISTS (
                 SELECT 1 FROM execution_processes ep
                 WHERE ep.id = agent_turn_admissions.intended_process_id AND ep.status = 'running'
               )"#,
        )
        .bind(now)
        .execute(pool)
        .await?;
        Ok(())
    }

    pub async fn acquire(
        pool: &SqlitePool,
        operation_key: &str,
        queue_item_id: Uuid,
        workspace_id: Uuid,
        capacity: usize,
        ttl: Duration,
    ) -> Result<AcquireAgentTurnAdmission, sqlx::Error> {
        let now = Utc::now();
        Self::reconcile(pool, now).await?;
        if let Some(existing) = Self::find(pool, operation_key).await? {
            if existing.queue_item_id != Some(queue_item_id)
                || existing.workspace_id != workspace_id
            {
                return Ok(AcquireAgentTurnAdmission::IdentityConflict);
            }
            if matches!(existing.status, AgentTurnAdmissionStatus::Reserved) {
                return Ok(AcquireAgentTurnAdmission::Acquired(existing));
            }
            if existing.status == AgentTurnAdmissionStatus::Started {
                return Ok(AcquireAgentTurnAdmission::Acquired(existing));
            }
        }
        if capacity == 0 {
            return Ok(AcquireAgentTurnAdmission::CapacityUnavailable);
        }
        let expires_at = now + ttl;
        let token_id = Self::find(pool, operation_key)
            .await?
            .map(|v| v.token_id)
            .unwrap_or_else(Uuid::new_v4);
        let result = sqlx::query(
            r#"INSERT INTO agent_turn_admissions
                 (token_id, operation_key, queue_item_id, workspace_id, fence, status, expires_at, created_at, updated_at)
               SELECT ?1, ?2, ?3, ?4, 1, 'reserved', ?5, ?6, ?6
               WHERE (SELECT COUNT(*) FROM execution_processes WHERE status = 'running' AND run_reason = 'codingagent')
                       + (SELECT COUNT(*) FROM agent_turn_admissions WHERE status = 'reserved') < ?7
                 AND NOT EXISTS (SELECT 1 FROM agent_turn_admissions WHERE workspace_id = ?4 AND status = 'reserved')
                 AND NOT EXISTS (
                   SELECT 1 FROM execution_processes ep JOIN sessions s ON s.id = ep.session_id
                   WHERE s.workspace_id = ?4 AND ep.status = 'running' AND ep.run_reason != 'devserver'
                 )
               ON CONFLICT(operation_key) DO UPDATE SET
                 status = 'reserved', fence = agent_turn_admissions.fence + 1,
                 expires_at = excluded.expires_at, updated_at = excluded.updated_at
               WHERE agent_turn_admissions.queue_item_id = excluded.queue_item_id
                 AND agent_turn_admissions.workspace_id = excluded.workspace_id
                 AND agent_turn_admissions.status IN ('released','expired')
                 AND (SELECT COUNT(*) FROM execution_processes WHERE status = 'running' AND run_reason = 'codingagent')
                       + (SELECT COUNT(*) FROM agent_turn_admissions WHERE status = 'reserved') < ?7
                 AND NOT EXISTS (SELECT 1 FROM agent_turn_admissions other WHERE other.workspace_id = ?4 AND other.status = 'reserved')"#,
        )
        .bind(token_id).bind(operation_key).bind(queue_item_id).bind(workspace_id)
        .bind(expires_at).bind(now).bind(capacity as i64).execute(pool).await?;
        if result.rows_affected() == 1 {
            return Ok(AcquireAgentTurnAdmission::Acquired(
                Self::find(pool, operation_key).await?.unwrap(),
            ));
        }
        let workspace_busy: i64 = sqlx::query_scalar(
            r#"SELECT EXISTS(SELECT 1 FROM agent_turn_admissions WHERE workspace_id=?1 AND status = 'reserved')
               OR EXISTS(SELECT 1 FROM execution_processes ep JOIN sessions s ON s.id=ep.session_id
                         WHERE s.workspace_id=?1 AND ep.status='running' AND ep.run_reason!='devserver')"#)
            .bind(workspace_id).fetch_one(pool).await?;
        Ok(if workspace_busy != 0 {
            AcquireAgentTurnAdmission::WorkspaceBusy
        } else {
            AcquireAgentTurnAdmission::CapacityUnavailable
        })
    }

    pub async fn prepare_process(
        pool: &SqlitePool,
        token_id: Uuid,
        fence: i64,
        process_id: Uuid,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("UPDATE agent_turn_admissions SET intended_process_id=?3, updated_at=?4 WHERE token_id=?1 AND fence=?2 AND status='reserved' AND (intended_process_id IS NULL OR intended_process_id=?3)")
            .bind(token_id).bind(fence).bind(process_id).bind(Utc::now()).execute(pool).await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn find_by_process(
        pool: &SqlitePool,
        process_id: Uuid,
    ) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as("SELECT * FROM agent_turn_admissions WHERE intended_process_id=?1")
            .bind(process_id)
            .fetch_optional(pool)
            .await
    }

    pub async fn acquire_direct(
        pool: &SqlitePool,
        process_id: Uuid,
        workspace_id: Uuid,
        capacity: usize,
        ttl: Duration,
    ) -> Result<AcquireAgentTurnAdmission, sqlx::Error> {
        let operation_key = format!("process:{process_id}");
        let now = Utc::now();
        Self::reconcile(pool, now).await?;
        if let Some(existing) = Self::find(pool, &operation_key).await? {
            return Ok(AcquireAgentTurnAdmission::Acquired(existing));
        }
        if capacity == 0 {
            return Ok(AcquireAgentTurnAdmission::CapacityUnavailable);
        }
        let result = sqlx::query(r#"INSERT INTO agent_turn_admissions
            (token_id,operation_key,queue_item_id,intended_process_id,workspace_id,fence,status,expires_at,created_at,updated_at)
            SELECT ?1,?2,NULL,?3,?4,1,'reserved',?5,?6,?6
            WHERE (SELECT COUNT(*) FROM execution_processes WHERE status='running' AND run_reason='codingagent')
                    + (SELECT COUNT(*) FROM agent_turn_admissions WHERE status = 'reserved') < ?7
              AND NOT EXISTS (SELECT 1 FROM agent_turn_admissions WHERE workspace_id=?4 AND status = 'reserved')
              AND NOT EXISTS (SELECT 1 FROM execution_processes ep JOIN sessions s ON s.id=ep.session_id
                              WHERE s.workspace_id=?4 AND ep.status='running' AND ep.run_reason!='devserver')"#)
            .bind(Uuid::new_v4()).bind(&operation_key).bind(process_id).bind(workspace_id)
            .bind(now + ttl).bind(now).bind(capacity as i64).execute(pool).await?;
        if result.rows_affected() == 1 {
            return Ok(AcquireAgentTurnAdmission::Acquired(
                Self::find(pool, &operation_key).await?.unwrap(),
            ));
        }
        Ok(AcquireAgentTurnAdmission::CapacityUnavailable)
    }

    pub async fn mark_started(
        pool: &SqlitePool,
        token_id: Uuid,
        fence: i64,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("UPDATE agent_turn_admissions AS a SET status='started', expires_at=NULL, updated_at=?3 WHERE a.token_id=?1 AND a.fence=?2 AND a.status='reserved' AND EXISTS (SELECT 1 FROM execution_processes ep JOIN sessions s ON s.id=ep.session_id WHERE ep.id=a.intended_process_id AND ep.status='running' AND s.workspace_id=a.workspace_id)")
            .bind(token_id).bind(fence).bind(Utc::now()).execute(pool).await?;
        if result.rows_affected() == 1 {
            return Ok(true);
        }
        let current: Option<(i64, AgentTurnAdmissionStatus)> =
            sqlx::query_as("SELECT fence,status FROM agent_turn_admissions WHERE token_id=?1")
                .bind(token_id)
                .fetch_optional(pool)
                .await?;
        Ok(
            matches!(current, Some((current_fence, AgentTurnAdmissionStatus::Started)) if current_fence == fence),
        )
    }

    pub async fn release(
        pool: &SqlitePool,
        token_id: Uuid,
        fence: i64,
    ) -> Result<bool, sqlx::Error> {
        let current: Option<(i64, AgentTurnAdmissionStatus)> =
            sqlx::query_as("SELECT fence,status FROM agent_turn_admissions WHERE token_id=?1")
                .bind(token_id)
                .fetch_optional(pool)
                .await?;
        let Some((current_fence, status)) = current else {
            return Ok(false);
        };
        if current_fence != fence {
            return Ok(false);
        }
        if matches!(
            status,
            AgentTurnAdmissionStatus::Released | AgentTurnAdmissionStatus::Expired
        ) {
            return Ok(true);
        }
        sqlx::query("UPDATE agent_turn_admissions SET status='released', expires_at=NULL, updated_at=?2 WHERE token_id=?1 AND fence=?3")
            .bind(token_id).bind(Utc::now()).bind(fence).execute(pool).await?;
        Ok(true)
    }

    pub async fn release_for_process(
        pool: &SqlitePool,
        process_id: Uuid,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE agent_turn_admissions SET status='released',expires_at=NULL,updated_at=?2 WHERE intended_process_id=?1 AND status IN ('reserved','started')")
            .bind(process_id).bind(Utc::now()).execute(pool).await?;
        Ok(())
    }

    pub async fn queue_item_for_process(
        pool: &SqlitePool,
        process_id: Uuid,
    ) -> Result<Option<Uuid>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT queue_item_id FROM agent_turn_admissions WHERE intended_process_id=?1",
        )
        .bind(process_id)
        .fetch_optional(pool)
        .await
        .map(Option::flatten)
    }
}

#[cfg(test)]
mod tests {
    use sqlx::{Executor, sqlite::SqlitePoolOptions};

    use super::*;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        for sql in [
            "CREATE TABLE sessions(id BLOB PRIMARY KEY, workspace_id BLOB NOT NULL)",
            "CREATE TABLE execution_processes(id BLOB PRIMARY KEY, session_id BLOB NOT NULL, status TEXT NOT NULL, run_reason TEXT NOT NULL)",
            "CREATE TABLE agent_message_queue(id BLOB PRIMARY KEY, started_execution_process_id BLOB)",
            "CREATE TABLE agent_turn_admissions(token_id BLOB PRIMARY KEY, operation_key TEXT NOT NULL UNIQUE, queue_item_id BLOB, intended_process_id BLOB, workspace_id BLOB NOT NULL, fence INTEGER NOT NULL DEFAULT 1, status TEXT NOT NULL, expires_at TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
            "CREATE UNIQUE INDEX active_workspace ON agent_turn_admissions(workspace_id) WHERE status = 'reserved'",
        ] {
            pool.execute(sql).await.unwrap();
        }
        pool
    }

    async fn item(pool: &SqlitePool, id: Uuid) {
        sqlx::query("INSERT INTO agent_message_queue(id) VALUES(?1)")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn admission_is_global_workspace_serial_and_idempotent() {
        let pool = pool().await;
        let (q1, q2, q3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        item(&pool, q1).await;
        item(&pool, q2).await;
        item(&pool, q3).await;
        let w1 = Uuid::new_v4();
        let w2 = Uuid::new_v4();
        let first = AgentTurnAdmission::acquire(&pool, "one", q1, w1, 2, Duration::seconds(60))
            .await
            .unwrap();
        let token = match first {
            AcquireAgentTurnAdmission::Acquired(v) => v,
            _ => panic!(),
        };
        assert!(
            matches!(AgentTurnAdmission::acquire(&pool, "one", q1, w1, 2, Duration::seconds(60)).await.unwrap(), AcquireAgentTurnAdmission::Acquired(v) if v.token_id == token.token_id && v.fence == token.fence)
        );
        assert_eq!(
            AgentTurnAdmission::acquire(&pool, "one", q2, w2, 2, Duration::seconds(60))
                .await
                .unwrap(),
            AcquireAgentTurnAdmission::IdentityConflict
        );
        assert_eq!(
            AgentTurnAdmission::acquire(&pool, "same-workspace", q2, w1, 2, Duration::seconds(60))
                .await
                .unwrap(),
            AcquireAgentTurnAdmission::WorkspaceBusy
        );
        assert!(matches!(
            AgentTurnAdmission::acquire(&pool, "two", q2, w2, 2, Duration::seconds(60))
                .await
                .unwrap(),
            AcquireAgentTurnAdmission::Acquired(_)
        ));
        assert_eq!(
            AgentTurnAdmission::acquire(
                &pool,
                "three",
                q3,
                Uuid::new_v4(),
                2,
                Duration::seconds(60)
            )
            .await
            .unwrap(),
            AcquireAgentTurnAdmission::CapacityUnavailable
        );
        assert!(
            AgentTurnAdmission::release(&pool, token.token_id, token.fence)
                .await
                .unwrap()
        );
        assert!(
            AgentTurnAdmission::release(&pool, token.token_id, token.fence)
                .await
                .unwrap()
        );
        assert!(
            !AgentTurnAdmission::release(&pool, token.token_id, token.fence + 1)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn expiry_reacquires_with_monotonic_fence_and_rejects_stale_token() {
        let pool = pool().await;
        let q = Uuid::new_v4();
        item(&pool, q).await;
        let w = Uuid::new_v4();
        let first = match AgentTurnAdmission::acquire(&pool, "op", q, w, 1, Duration::seconds(-1))
            .await
            .unwrap()
        {
            AcquireAgentTurnAdmission::Acquired(v) => v,
            _ => panic!(),
        };
        AgentTurnAdmission::prepare_process(&pool, first.token_id, first.fence, q)
            .await
            .unwrap();
        let second = match AgentTurnAdmission::acquire(&pool, "op", q, w, 1, Duration::seconds(60))
            .await
            .unwrap()
        {
            AcquireAgentTurnAdmission::Acquired(v) => v,
            _ => panic!(),
        };
        assert_eq!(second.token_id, first.token_id);
        assert!(second.fence > first.fence);
        assert!(
            !AgentTurnAdmission::mark_started(&pool, first.token_id, first.fence)
                .await
                .unwrap()
        );
        AgentTurnAdmission::prepare_process(&pool, second.token_id, second.fence, q)
            .await
            .unwrap();
        assert!(
            !AgentTurnAdmission::mark_started(&pool, second.token_id, second.fence)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn running_process_reconciles_reserved_start_without_double_counting() {
        let pool = pool().await;
        let q1 = Uuid::new_v4();
        let q2 = Uuid::new_v4();
        item(&pool, q1).await;
        item(&pool, q2).await;
        let w1 = Uuid::new_v4();
        let s1 = Uuid::new_v4();
        let p1 = Uuid::new_v4();
        let token =
            match AgentTurnAdmission::acquire(&pool, "one", q1, w1, 1, Duration::seconds(60))
                .await
                .unwrap()
            {
                AcquireAgentTurnAdmission::Acquired(v) => v,
                _ => panic!(),
            };
        sqlx::query("INSERT INTO sessions VALUES(?1,?2)")
            .bind(s1)
            .bind(w1)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO execution_processes VALUES(?1,?2,'running','codingagent')")
            .bind(p1)
            .bind(s1)
            .execute(&pool)
            .await
            .unwrap();
        AgentTurnAdmission::prepare_process(&pool, token.token_id, token.fence, p1)
            .await
            .unwrap();
        AgentTurnAdmission::reconcile(&pool, Utc::now())
            .await
            .unwrap();
        assert_eq!(
            AgentTurnAdmission::find(&pool, "one")
                .await
                .unwrap()
                .unwrap()
                .status,
            AgentTurnAdmissionStatus::Started
        );
        assert_eq!(
            AgentTurnAdmission::acquire(&pool, "two", q2, Uuid::new_v4(), 1, Duration::seconds(60))
                .await
                .unwrap(),
            AcquireAgentTurnAdmission::CapacityUnavailable
        );
        assert!(
            AgentTurnAdmission::release(&pool, token.token_id, token.fence)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn concurrent_server_instances_cannot_over_admit() {
        let path = std::env::temp_dir().join(format!("vk-admission-{}.sqlite", Uuid::new_v4()));
        std::fs::File::create(&path).unwrap();
        let url = format!("sqlite://{}", path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .unwrap();
        for sql in [
            "CREATE TABLE sessions(id BLOB PRIMARY KEY, workspace_id BLOB NOT NULL)",
            "CREATE TABLE execution_processes(id BLOB PRIMARY KEY, session_id BLOB NOT NULL, status TEXT NOT NULL, run_reason TEXT NOT NULL)",
            "CREATE TABLE agent_message_queue(id BLOB PRIMARY KEY, started_execution_process_id BLOB)",
            "CREATE TABLE agent_turn_admissions(token_id BLOB PRIMARY KEY, operation_key TEXT NOT NULL UNIQUE, queue_item_id BLOB, intended_process_id BLOB, workspace_id BLOB NOT NULL, fence INTEGER NOT NULL DEFAULT 1, status TEXT NOT NULL, expires_at TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
            "CREATE UNIQUE INDEX active_workspace ON agent_turn_admissions(workspace_id) WHERE status = 'reserved'",
        ] {
            pool.execute(sql).await.unwrap();
        }
        let q1 = Uuid::new_v4();
        let q2 = Uuid::new_v4();
        item(&pool, q1).await;
        item(&pool, q2).await;
        let p1 = pool.clone();
        let p2 = pool.clone();
        let (a, b) = tokio::join!(
            AgentTurnAdmission::acquire(&p1, "a", q1, Uuid::new_v4(), 1, Duration::seconds(60)),
            AgentTurnAdmission::acquire(&p2, "b", q2, Uuid::new_v4(), 1, Duration::seconds(60))
        );
        let outcomes = [a.unwrap(), b.unwrap()];
        assert_eq!(
            outcomes
                .iter()
                .filter(|v| matches!(v, AcquireAgentTurnAdmission::Acquired(_)))
                .count(),
            1
        );
        drop(pool);
        let _ = std::fs::remove_file(path);
        assert_eq!(
            outcomes
                .iter()
                .filter(|v| matches!(v, AcquireAgentTurnAdmission::CapacityUnavailable))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn capacity_changes_are_deterministic_and_replay_keeps_its_slot() {
        let pool = pool().await;
        let ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        for id in ids {
            item(&pool, id).await;
        }
        assert_eq!(
            AgentTurnAdmission::acquire(
                &pool,
                "zero",
                ids[0],
                Uuid::new_v4(),
                0,
                Duration::seconds(60)
            )
            .await
            .unwrap(),
            AcquireAgentTurnAdmission::CapacityUnavailable
        );
        let one = match AgentTurnAdmission::acquire(
            &pool,
            "one",
            ids[0],
            Uuid::new_v4(),
            2,
            Duration::seconds(60),
        )
        .await
        .unwrap()
        {
            AcquireAgentTurnAdmission::Acquired(v) => v,
            _ => panic!(),
        };
        assert!(
            matches!(AgentTurnAdmission::acquire(&pool,"one",ids[0],one.workspace_id,0,Duration::seconds(60)).await.unwrap(), AcquireAgentTurnAdmission::Acquired(v) if v.token_id==one.token_id)
        );
        let two = match AgentTurnAdmission::acquire(
            &pool,
            "two",
            ids[1],
            Uuid::new_v4(),
            2,
            Duration::seconds(60),
        )
        .await
        .unwrap()
        {
            AcquireAgentTurnAdmission::Acquired(v) => v,
            _ => panic!(),
        };
        assert_eq!(
            AgentTurnAdmission::acquire(
                &pool,
                "three",
                ids[2],
                Uuid::new_v4(),
                1,
                Duration::seconds(60)
            )
            .await
            .unwrap(),
            AcquireAgentTurnAdmission::CapacityUnavailable
        );
        assert!(
            AgentTurnAdmission::release(&pool, two.token_id, two.fence)
                .await
                .unwrap()
        );
        assert_eq!(
            AgentTurnAdmission::acquire(
                &pool,
                "three",
                ids[2],
                Uuid::new_v4(),
                1,
                Duration::seconds(60)
            )
            .await
            .unwrap(),
            AcquireAgentTurnAdmission::CapacityUnavailable
        );
        assert!(matches!(
            AgentTurnAdmission::acquire(
                &pool,
                "three",
                ids[2],
                Uuid::new_v4(),
                2,
                Duration::seconds(60)
            )
            .await
            .unwrap(),
            AcquireAgentTurnAdmission::Acquired(_)
        ));
    }

    #[tokio::test]
    async fn forward_migration_upgrades_the_applied_intermediate_contract() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        pool.execute("PRAGMA foreign_keys=ON").await.unwrap();
        pool.execute("CREATE TABLE workspaces(id BLOB PRIMARY KEY)")
            .await
            .unwrap();
        pool.execute("CREATE TABLE agent_message_queue(id BLOB PRIMARY KEY)")
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../../migrations/20260911000000_add_agent_turn_admission.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        let workspace = Uuid::new_v4();
        let queue = Uuid::new_v4();
        sqlx::query("INSERT INTO workspaces VALUES(?1)")
            .bind(workspace)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO agent_message_queue VALUES(?1)")
            .bind(queue)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO agent_turn_admissions(token_id,operation_key,queue_item_id,intended_process_id,workspace_id,fence,status,expires_at,created_at,updated_at) VALUES(?1,'old',?2,NULL,?3,1,'reserved',NULL,?4,?4)").bind(Uuid::new_v4()).bind(queue).bind(workspace).bind(Utc::now()).execute(&pool).await.unwrap();
        sqlx::raw_sql(include_str!(
            "../../migrations/20260911120000_upgrade_agent_turn_admission.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        let preserved: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_turn_admissions WHERE operation_key='old'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(preserved, 1);
        let direct_workspace = Uuid::new_v4();
        sqlx::query("INSERT INTO workspaces VALUES(?1)")
            .bind(direct_workspace)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO agent_turn_admissions(token_id,operation_key,queue_item_id,workspace_id,fence,status,created_at,updated_at) VALUES(?1,'direct',NULL,?2,1,'starting',?3,?3)").bind(Uuid::new_v4()).bind(direct_workspace).bind(Utc::now()).execute(&pool).await.unwrap();
        sqlx::raw_sql(include_str!(
            "../../migrations/20260911130000_remove_agent_turn_starting_state.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        let status: String = sqlx::query_scalar(
            "SELECT status FROM agent_turn_admissions WHERE operation_key='direct'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, "reserved");
    }
}
