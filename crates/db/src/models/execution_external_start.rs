use chrono::{Duration, Utc};
use sqlx::SqlitePool;
use uuid::Uuid;

pub struct ExternalStartClaim {
    pub fence: i64,
    pub start_key: String,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;

    async fn pool() -> SqlitePool {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn process(pool: &SqlitePool) -> Uuid {
        let workspace = Uuid::new_v4();
        let session = Uuid::new_v4();
        let process = Uuid::new_v4();
        sqlx::query("INSERT INTO workspaces(id,branch) VALUES(?1,'test')")
            .bind(workspace)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO sessions(id,workspace_id) VALUES(?1,?2)")
            .bind(session)
            .bind(workspace)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO execution_processes(id,session_id,run_reason,executor_action,status,dropped) VALUES(?1,?2,'codingagent','{}','running',FALSE)")
            .bind(process).bind(session).execute(pool).await.unwrap();
        process
    }

    #[tokio::test]
    async fn claim_is_fenced_restart_safe_and_spawn_confirmation_is_idempotent() {
        let pool = pool().await;
        let process = process(&pool).await;
        ExecutionExternalStart::authorize(&pool, process)
            .await
            .unwrap();
        let first = ExecutionExternalStart::claim(&pool, process, Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.start_key, format!("execution:{process}"));
        assert!(
            ExecutionExternalStart::claim(&pool, process, Duration::seconds(30))
                .await
                .unwrap()
                .is_none()
        );
        sqlx::query("UPDATE execution_external_starts SET claim_expires_at=?2 WHERE execution_process_id=?1")
            .bind(process).bind(Utc::now()-Duration::seconds(1)).execute(&pool).await.unwrap();
        let recovered = ExecutionExternalStart::claim(&pool, process, Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        assert!(recovered.fence > first.fence);
        assert!(
            ExecutionExternalStart::mark_spawn_requested(&pool, process, recovered.fence)
                .await
                .unwrap()
        );
        assert!(
            ExecutionExternalStart::record_spawn_identity(
                &pool,
                process,
                recovered.fence,
                "pid-1",
                "start-1",
            )
            .await
            .unwrap()
        );
        assert!(
            !ExecutionExternalStart::confirm_spawned(
                &pool,
                process,
                first.fence,
                Some("stale"),
                Some("old")
            )
            .await
            .unwrap()
        );
        assert!(
            ExecutionExternalStart::confirm_spawned(
                &pool,
                process,
                recovered.fence,
                Some("pid-1"),
                Some("start-1")
            )
            .await
            .unwrap()
        );
        assert!(
            ExecutionExternalStart::is_spawned(&pool, process)
                .await
                .unwrap()
        );
        assert_eq!(
            ExecutionExternalStart::external_process_id(&pool, process)
                .await
                .unwrap()
                .as_deref(),
            Some("pid-1")
        );

        ExecutionExternalStart::mark_blocked(&pool, process, "Needs confirmation")
            .await
            .unwrap();
        let blocked = ExecutionExternalStart::record(&pool, process)
            .await
            .unwrap()
            .unwrap();
        let token = blocked.recovery_token.as_deref().unwrap();
        let generation = blocked.recovery_generation;
        let workspace: Uuid = sqlx::query_scalar("SELECT s.workspace_id FROM execution_processes ep JOIN sessions s ON s.id=ep.session_id WHERE ep.id=?1")
            .bind(process).fetch_one(&pool).await.unwrap();
        assert_eq!(
            ExecutionExternalStart::confirm_stopped(
                &pool,
                process,
                Uuid::new_v4(),
                "operator",
                token,
                generation
            )
            .await
            .unwrap(),
            ExternalStartRecoveryResult::WrongWorkspace
        );
        assert_eq!(
            ExecutionExternalStart::confirm_stopped(
                &pool, process, workspace, "operator", "stale", generation
            )
            .await
            .unwrap(),
            ExternalStartRecoveryResult::StaleRecovery
        );
        assert_eq!(
            ExecutionExternalStart::confirm_stopped(
                &pool, process, workspace, "operator", token, generation
            )
            .await
            .unwrap(),
            ExternalStartRecoveryResult::Reconciled
        );
        assert_eq!(
            ExecutionExternalStart::confirm_stopped(
                &pool, process, workspace, "operator", token, generation
            )
            .await
            .unwrap(),
            ExternalStartRecoveryResult::AlreadyReconciled
        );
        assert_eq!(
            ExecutionExternalStart::confirm_stopped(
                &pool,
                process,
                workspace,
                "different-operator",
                token,
                generation
            )
            .await
            .unwrap(),
            ExternalStartRecoveryResult::StaleRecovery
        );
        let audit_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM execution_external_start_recoveries WHERE execution_process_id=?1")
            .bind(process).fetch_one(&pool).await.unwrap();
        assert_eq!(audit_count, 1);
        let audit_actor: String = sqlx::query_scalar(
            "SELECT actor FROM execution_external_start_recoveries WHERE execution_process_id=?1",
        )
        .bind(process)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audit_actor, "operator");
    }

    #[tokio::test]
    async fn delayed_recovery_capability_cannot_reconcile_a_newer_generation() {
        let pool = pool().await;
        let process = process(&pool).await;
        ExecutionExternalStart::authorize(&pool, process)
            .await
            .unwrap();
        ExecutionExternalStart::mark_blocked(&pool, process, "first")
            .await
            .unwrap();
        let first = ExecutionExternalStart::record(&pool, process)
            .await
            .unwrap()
            .unwrap();
        sqlx::query(
            "UPDATE execution_external_starts SET state='spawned' WHERE execution_process_id=?1",
        )
        .bind(process)
        .execute(&pool)
        .await
        .unwrap();
        ExecutionExternalStart::mark_blocked(&pool, process, "second")
            .await
            .unwrap();
        let second = ExecutionExternalStart::record(&pool, process)
            .await
            .unwrap()
            .unwrap();
        assert!(second.recovery_generation > first.recovery_generation);
        assert_ne!(second.recovery_token, first.recovery_token);
        let workspace: Uuid = sqlx::query_scalar("SELECT s.workspace_id FROM execution_processes ep JOIN sessions s ON s.id=ep.session_id WHERE ep.id=?1")
            .bind(process).fetch_one(&pool).await.unwrap();
        assert_eq!(
            ExecutionExternalStart::confirm_stopped(
                &pool,
                process,
                workspace,
                "operator",
                first.recovery_token.as_deref().unwrap(),
                first.recovery_generation,
            )
            .await
            .unwrap(),
            ExternalStartRecoveryResult::StaleRecovery
        );
        assert_eq!(
            ExecutionExternalStart::state(&pool, process)
                .await
                .unwrap()
                .as_deref(),
            Some("blocked")
        );
    }
}

pub struct ExecutionExternalStart;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ExecutionExternalStartRecord {
    pub execution_process_id: Uuid,
    pub start_key: String,
    pub state: String,
    pub claim_fence: i64,
    pub spawn_requested_at: Option<chrono::DateTime<Utc>>,
    pub external_process_id: Option<String>,
    pub external_process_started_at: Option<String>,
    pub blocked_reason: Option<String>,
    pub recovery_token: Option<String>,
    pub recovery_generation: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalStartRecoveryResult {
    Reconciled,
    AlreadyReconciled,
    NotBlocked,
    WrongWorkspace,
    StaleRecovery,
}
impl ExecutionExternalStart {
    pub async fn authorize(pool: &SqlitePool, process_id: Uuid) -> Result<(), sqlx::Error> {
        let now = Utc::now();
        sqlx::query("INSERT INTO execution_external_starts(execution_process_id,start_key,state,created_at,updated_at) VALUES(?1,?2,'authorized',?3,?3) ON CONFLICT(execution_process_id) DO NOTHING")
   .bind(process_id).bind(format!("execution:{process_id}")).bind(now).execute(pool).await?;
        Ok(())
    }
    pub async fn claim(
        pool: &SqlitePool,
        process_id: Uuid,
        ttl: Duration,
    ) -> Result<Option<ExternalStartClaim>, sqlx::Error> {
        let now = Utc::now();
        let expires = now + ttl;
        let result=sqlx::query("UPDATE execution_external_starts SET state='claiming',claim_fence=claim_fence+1,claim_expires_at=?2,updated_at=?3 WHERE execution_process_id=?1 AND (state='authorized' OR (state='claiming' AND claim_expires_at<=?3))")
   .bind(process_id).bind(expires).bind(now).execute(pool).await?;
        if result.rows_affected() != 1 {
            return Ok(None);
        }
        let (fence,key):(i64,String)=sqlx::query_as("SELECT claim_fence,start_key FROM execution_external_starts WHERE execution_process_id=?1").bind(process_id).fetch_one(pool).await?;
        Ok(Some(ExternalStartClaim {
            fence,
            start_key: key,
        }))
    }
    pub async fn confirm_spawned(
        pool: &SqlitePool,
        process_id: Uuid,
        fence: i64,
        external_id: Option<&str>,
        external_started_at: Option<&str>,
    ) -> Result<bool, sqlx::Error> {
        let result=sqlx::query("UPDATE execution_external_starts SET state='spawned',external_process_id=?3,external_process_started_at=?4,claim_expires_at=NULL,updated_at=?5 WHERE execution_process_id=?1 AND claim_fence=?2 AND state='claiming' AND spawn_requested_at IS NOT NULL")
   .bind(process_id).bind(fence).bind(external_id).bind(external_started_at).bind(Utc::now()).execute(pool).await?;
        Ok(result.rows_affected() == 1)
    }
    pub async fn record_spawn_identity(
        pool: &SqlitePool,
        process_id: Uuid,
        fence: i64,
        external_id: &str,
        external_started_at: &str,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("UPDATE execution_external_starts SET external_process_id=?3,external_process_started_at=?4,updated_at=?5 WHERE execution_process_id=?1 AND claim_fence=?2 AND state='claiming' AND spawn_requested_at IS NOT NULL")
            .bind(process_id).bind(fence).bind(external_id).bind(external_started_at).bind(Utc::now()).execute(pool).await?;
        Ok(result.rows_affected() == 1)
    }
    pub async fn mark_spawn_requested(
        pool: &SqlitePool,
        process_id: Uuid,
        fence: i64,
    ) -> Result<bool, sqlx::Error> {
        let now = Utc::now();
        let result = sqlx::query("UPDATE execution_external_starts SET spawn_requested_at=?3,updated_at=?3 WHERE execution_process_id=?1 AND claim_fence=?2 AND state='claiming'")
            .bind(process_id).bind(fence).bind(now).execute(pool).await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn record(
        pool: &SqlitePool,
        process_id: Uuid,
    ) -> Result<Option<ExecutionExternalStartRecord>, sqlx::Error> {
        sqlx::query_as("SELECT execution_process_id,start_key,state,claim_fence,spawn_requested_at,external_process_id,external_process_started_at,blocked_reason,recovery_token,recovery_generation FROM execution_external_starts WHERE execution_process_id=?1")
            .bind(process_id).fetch_optional(pool).await
    }
    pub async fn mark_blocked(
        pool: &SqlitePool,
        process_id: Uuid,
        reason: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE execution_external_starts SET state='blocked',blocked_reason=?2,claim_expires_at=NULL,recovery_token=CASE WHEN state='blocked' AND recovery_token IS NOT NULL THEN recovery_token ELSE ?3 END,recovery_generation=CASE WHEN state='blocked' AND recovery_token IS NOT NULL THEN recovery_generation ELSE recovery_generation+1 END,updated_at=?4 WHERE execution_process_id=?1 AND state IN ('authorized','claiming','spawned','blocked')")
            .bind(process_id).bind(reason).bind(Uuid::new_v4().to_string()).bind(Utc::now()).execute(pool).await?;
        Ok(())
    }

    pub async fn confirm_stopped(
        pool: &SqlitePool,
        process_id: Uuid,
        workspace_id: Uuid,
        actor: &str,
        recovery_token: &str,
        recovery_generation: i64,
    ) -> Result<ExternalStartRecoveryResult, sqlx::Error> {
        let actor = actor.trim();
        if actor.is_empty() {
            return Err(sqlx::Error::Protocol("Recovery actor is required".into()));
        }
        let mut tx = pool.begin().await?;
        let actual_workspace: Option<Uuid> = sqlx::query_scalar("SELECT s.workspace_id FROM execution_processes ep JOIN sessions s ON s.id=ep.session_id WHERE ep.id=?1")
            .bind(process_id).fetch_optional(&mut *tx).await?;
        if actual_workspace != Some(workspace_id) {
            tx.rollback().await?;
            return Ok(ExternalStartRecoveryResult::WrongWorkspace);
        }
        let state: Option<String> = sqlx::query_scalar(
            "SELECT state FROM execution_external_starts WHERE execution_process_id=?1",
        )
        .bind(process_id)
        .fetch_optional(&mut *tx)
        .await?;
        let recovery: Option<(String, i64)> = sqlx::query_as("SELECT recovery_token,recovery_generation FROM execution_external_starts WHERE execution_process_id=?1 AND recovery_token IS NOT NULL")
            .bind(process_id).fetch_optional(&mut *tx).await?;
        if recovery.as_ref().is_none_or(|(token, generation)| {
            token != recovery_token || *generation != recovery_generation
        }) {
            tx.rollback().await?;
            return Ok(ExternalStartRecoveryResult::StaleRecovery);
        }
        if state.as_deref() == Some("failed") {
            let matching: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM execution_external_start_recoveries WHERE execution_process_id=?1 AND operation='confirm_stopped' AND recovery_token=?2 AND recovery_generation=?3 AND actor=?4")
                .bind(process_id).bind(recovery_token).bind(recovery_generation).bind(actor).fetch_one(&mut *tx).await?;
            tx.rollback().await?;
            return Ok(if matching > 0 {
                ExternalStartRecoveryResult::AlreadyReconciled
            } else {
                ExternalStartRecoveryResult::StaleRecovery
            });
        }
        if state.as_deref() != Some("blocked") {
            tx.rollback().await?;
            return Ok(ExternalStartRecoveryResult::NotBlocked);
        }
        sqlx::query("INSERT INTO execution_external_start_recoveries(id,execution_process_id,workspace_id,operation,actor,created_at,recovery_token,recovery_generation) VALUES(?1,?2,?3,'confirm_stopped',?4,?5,?6,?7) ON CONFLICT(execution_process_id,recovery_generation,operation) DO NOTHING")
            .bind(Uuid::new_v4()).bind(process_id).bind(workspace_id).bind(actor).bind(Utc::now()).bind(recovery_token).bind(recovery_generation).execute(&mut *tx).await?;
        let changed = sqlx::query("UPDATE execution_external_starts SET state='failed',blocked_reason=NULL,updated_at=?4 WHERE execution_process_id=?1 AND state='blocked' AND recovery_token=?2 AND recovery_generation=?3")
            .bind(process_id).bind(recovery_token).bind(recovery_generation).bind(Utc::now()).execute(&mut *tx).await?;
        if changed.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(ExternalStartRecoveryResult::StaleRecovery);
        }
        tx.commit().await?;
        Ok(ExternalStartRecoveryResult::Reconciled)
    }
    pub async fn is_spawned(pool: &SqlitePool, process_id: Uuid) -> Result<bool, sqlx::Error> {
        Ok(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM execution_external_starts WHERE execution_process_id=?1 AND state='spawned'").bind(process_id).fetch_one(pool).await?>0)
    }
    pub async fn fail(pool: &SqlitePool, process_id: Uuid) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE execution_external_starts SET state='failed',claim_expires_at=NULL,updated_at=?2 WHERE execution_process_id=?1").bind(process_id).bind(Utc::now()).execute(pool).await?;
        Ok(())
    }
    pub async fn state(pool: &SqlitePool, process_id: Uuid) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT state FROM execution_external_starts WHERE execution_process_id=?1",
        )
        .bind(process_id)
        .fetch_optional(pool)
        .await
    }

    pub async fn external_process_id(
        pool: &SqlitePool,
        process_id: Uuid,
    ) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT external_process_id FROM execution_external_starts WHERE execution_process_id=?1 AND state='spawned'",
        )
        .bind(process_id)
        .fetch_optional(pool)
        .await
        .map(Option::flatten)
    }
}
