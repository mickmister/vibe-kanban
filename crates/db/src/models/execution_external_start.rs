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
            !ExecutionExternalStart::confirm_spawned(&pool, process, first.fence, Some("stale"))
                .await
                .unwrap()
        );
        assert!(
            ExecutionExternalStart::confirm_spawned(&pool, process, recovered.fence, Some("pid-1"))
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
    }
}

pub struct ExecutionExternalStart;
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
    ) -> Result<bool, sqlx::Error> {
        let result=sqlx::query("UPDATE execution_external_starts SET state='spawned',external_process_id=?3,claim_expires_at=NULL,updated_at=?4 WHERE execution_process_id=?1 AND claim_fence=?2 AND state='claiming'")
   .bind(process_id).bind(fence).bind(external_id).bind(Utc::now()).execute(pool).await?;
        Ok(result.rows_affected() == 1)
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
