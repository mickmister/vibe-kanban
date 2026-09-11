use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use deployment::{Deployment, DeploymentError};
use services::services::container::ContainerService;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tower_http::{trace::TraceLayer, validate_request::ValidateRequestHeaderLayer};
use utils::{
    assets::asset_dir,
    perf_trace,
    process_diag::{self, ProcessSnapshot},
};

use crate::{
    DeploymentImpl,
    middleware::{
        make_http_span,
        origin::{validate_allowed_origins_config, validate_origin},
    },
    routes,
    runtime::relay_registration,
};

/// A running server instance. Callers can read the port, then call `serve()`
/// to run the server until the shutdown token is cancelled.
pub struct ServerHandle {
    pub port: u16,
    pub proxy_port: u16,
    pub deployment: DeploymentImpl,
    shutdown_token: CancellationToken,
    main_listener: tokio::net::TcpListener,
    proxy_listener: tokio::net::TcpListener,
}

impl ServerHandle {
    /// The base URL the main server is listening on.
    ///
    /// Uses `localhost` rather than `127.0.0.1` so that macOS ATS
    /// (App Transport Security) exception domains apply correctly in
    /// the Tauri desktop app — IP address literals aren't reliably
    /// matched by ATS, which causes WebSocket connections to fail.
    pub fn url(&self) -> String {
        format!("http://localhost:{}", self.port)
    }

    /// Run both the main and proxy servers until the shutdown token is cancelled.
    pub async fn serve(self) -> anyhow::Result<()> {
        // Start relay tunnel so the host registers with the relay server.
        // This must happen after the port is known (it's needed for local
        // proxying) and is shared between the standalone binary and Tauri.
        self.deployment
            .client_info()
            .set_server_addr(self.main_listener.local_addr()?)
            .expect("client server address already set");
        self.deployment
            .client_info()
            .set_preview_proxy_port(self.proxy_port)
            .expect("client preview proxy port already set");
        log_startup_phase("client_info_registered");
        relay_registration::spawn_relay(&self.deployment).await;
        log_startup_phase("relay_startup_spawn_complete");

        let perf_tracing_enabled = perf_trace::enabled();
        let app_router = routes::router(self.deployment.clone(), perf_tracing_enabled);
        let proxy_router: axum::Router = {
            let router = routes::preview::subdomain_router(self.deployment.clone());
            let router = if perf_tracing_enabled {
                router.layer(
                    TraceLayer::new_for_http()
                        .make_span_with(|request: &axum::extract::Request| make_http_span(request)),
                )
            } else {
                router
            };
            router.layer(ValidateRequestHeaderLayer::custom(validate_origin))
        };

        let main_shutdown = self.shutdown_token.clone();
        let proxy_shutdown = self.shutdown_token.clone();

        let main_server = axum::serve(self.main_listener, app_router)
            .with_graceful_shutdown(async move { main_shutdown.cancelled().await });
        let proxy_server = axum::serve(self.proxy_listener, proxy_router)
            .with_graceful_shutdown(async move { proxy_shutdown.cancelled().await });

        let mut main_handle = tokio::spawn(async move {
            if let Err(e) = main_server.await {
                tracing::error!("Main server error: {}", e);
            }
        });
        let mut proxy_handle = tokio::spawn(async move {
            if let Err(e) = proxy_server.await {
                tracing::error!("Preview proxy error: {}", e);
            }
        });
        log_startup_phase("server_ready");

        let mut main_done = false;
        let mut proxy_done = false;
        tokio::select! {
            result = &mut main_handle => {
                main_done = true;
                if let Err(error) = result {
                    tracing::error!(%error, "Main server task failed");
                } else {
                    tracing::warn!("Main server task completed; shutting down");
                }
            }
            result = &mut proxy_handle => {
                proxy_done = true;
                if let Err(error) = result {
                    tracing::error!(%error, "Preview proxy task failed");
                } else {
                    tracing::warn!("Preview proxy task completed; shutting down");
                }
            }
        }

        self.shutdown_token.cancel();

        if !main_done && let Err(error) = main_handle.await {
            tracing::error!(%error, "Main server task failed during shutdown");
        }
        if !proxy_done && let Err(error) = proxy_handle.await {
            tracing::error!(%error, "Preview proxy task failed during shutdown");
        }

        perform_cleanup_actions(&self.deployment).await;
        Ok(())
    }

    /// Return a clone of the shutdown token. Cancel it to stop `serve()`.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown_token.clone()
    }
}

/// Initialize the deployment, bind listeners on `localhost` with OS-assigned
/// ports, and return a handle that is ready to serve.
///
/// Uses `localhost` rather than `127.0.0.1` so the bind address matches
/// the hostname the frontend connects to. On modern macOS, `localhost`
/// resolves to `::1` (IPv6) first — binding to `127.0.0.1` (IPv4) while
/// the browser connects via `::1` causes "connection refused".
pub async fn start() -> anyhow::Result<ServerHandle> {
    start_with_bind("localhost:0", "localhost:0", CancellationToken::new()).await
}

/// Like [`start`], but lets the caller specify the bind addresses for the main
/// server and the preview proxy (e.g. `"0.0.0.0:8080"`).
pub async fn start_with_bind(
    main_addr: &str,
    proxy_addr: &str,
    shutdown_token: CancellationToken,
) -> anyhow::Result<ServerHandle> {
    begin_startup_diagnostics();
    let deployment = initialize_deployment(shutdown_token.clone()).await?;

    let listener = tokio::net::TcpListener::bind(main_addr).await?;
    let port = listener.local_addr()?.port();

    let proxy_listener = tokio::net::TcpListener::bind(proxy_addr).await?;
    let proxy_port = proxy_listener.local_addr()?.port();
    log_startup_phase("http_bind_complete");

    tracing::info!("Server on :{port}, Preview proxy on :{proxy_port}");

    Ok(ServerHandle {
        port,
        proxy_port,
        deployment,
        shutdown_token,
        main_listener: listener,
        proxy_listener,
    })
}

/// Initialize the deployment: create asset directory, run migrations, backfill data,
/// and pre-warm caches. Shared between the standalone server and the Tauri app.
pub async fn initialize_deployment(
    shutdown: CancellationToken,
) -> Result<DeploymentImpl, DeploymentError> {
    begin_startup_diagnostics();
    validate_allowed_origins_config()
        .map_err(|error| DeploymentError::Other(anyhow::anyhow!(error)))?;

    // Create asset directory if it doesn't exist
    if !asset_dir().exists() {
        std::fs::create_dir_all(asset_dir()).map_err(|e| {
            DeploymentError::Other(anyhow::anyhow!("Failed to create asset directory: {}", e))
        })?;
    }

    // Copy old database to new location for safe downgrades
    let old_db = asset_dir().join("db.sqlite");
    let new_db = asset_dir().join("db.v2.sqlite");
    if !new_db.exists() && old_db.exists() {
        tracing::info!(
            "Copying database to new location: {:?} -> {:?}",
            old_db,
            new_db
        );
        std::fs::copy(&old_db, &new_db).expect("Failed to copy database file");
        tracing::info!("Database copy complete");
    }
    log_startup_phase("db_path_resolution_complete");

    let deployment = DeploymentImpl::new(shutdown).await?;
    log_startup_phase("deployment_init_complete");
    migrate_legacy_attachment_directories(&deployment).await?;
    log_startup_phase("legacy_attachment_migration_complete");
    deployment.update_sentry_scope().await?;
    log_startup_phase("sentry_scope_update_complete");
    deployment
        .container()
        .cleanup_orphan_executions()
        .await
        .map_err(DeploymentError::from)?;
    log_startup_phase("cleanup_orphan_executions_complete");
    deployment
        .queued_message_service()
        .recover_stale()
        .await
        .map_err(|error| DeploymentError::Other(anyhow::anyhow!(error)))?;
    deployment
        .container()
        .try_start_queued_messages(deployment.queued_message_service())
        .await
        .map_err(DeploymentError::from)?;
    log_startup_phase("queued_message_recovery_complete");
    run_startup_backfills(&deployment).await?;
    deployment
        .track_if_analytics_allowed("session_start", serde_json::json!({}))
        .await;
    log_startup_phase("session_start_analytics_complete");

    // Preload global executor options cache for all executors with DEFAULT presets
    spawn_executor_preload_task();

    Ok(deployment)
}

/// Gracefully shut down running execution processes.
pub async fn perform_cleanup_actions(deployment: &DeploymentImpl) {
    deployment
        .container()
        .kill_all_running_processes()
        .await
        .expect("Failed to cleanly kill running execution processes");
}

static STARTUP_DIAGNOSTICS_INIT: OnceLock<()> = OnceLock::new();
static STARTUP_DIAGNOSTICS_SAMPLER: OnceLock<()> = OnceLock::new();

pub fn begin_startup_diagnostics() {
    process_diag::mark_process_start();

    if !debug_memory_logs_enabled() {
        return;
    }

    let initialized = STARTUP_DIAGNOSTICS_INIT.get().is_some();
    STARTUP_DIAGNOSTICS_INIT.get_or_init(|| ());
    if !initialized {
        log_startup_phase("process_start");
    }

    STARTUP_DIAGNOSTICS_SAMPLER.get_or_init(|| {
        if let Some((interval, duration)) = startup_sampling_config() {
            tracing::info!(
                interval_ms = interval.as_millis() as u64,
                duration_secs = duration.as_secs(),
                "startup_diag_sampler_started"
            );
            tokio::spawn(async move {
                let started_at = Instant::now();
                while started_at.elapsed() < duration {
                    tokio::time::sleep(interval).await;
                    log_startup_phase("periodic_sample");
                }
                tracing::info!("startup_diag_sampler_finished");
            });
        }
    });
}

pub fn log_startup_phase(phase: &str) {
    if !debug_memory_logs_enabled() {
        return;
    }
    let snapshot = process_diag::sample_current_process();
    emit_startup_diag_log("startup_diag", phase, &snapshot);
}

pub fn runtime_diagnostics_enabled() -> bool {
    debug_memory_logs_enabled()
}

fn log_startup_skip(phase: &str, env_var: &str) {
    let snapshot = process_diag::sample_current_process();
    tracing::info!(
        phase,
        env_var,
        rss_mb = process_diag::bytes_to_mb(snapshot.rss_bytes),
        vm_size_mb = process_diag::bytes_to_mb(snapshot.virtual_bytes),
        threads = snapshot.thread_count,
        fds = snapshot.open_fd_count,
        child_processes = snapshot.child_process_count,
        elapsed_ms = process_diag::elapsed_since_start().as_millis() as u64,
        "startup_diag_skipped"
    );
}

fn emit_startup_diag_log(message: &str, phase: &str, snapshot: &ProcessSnapshot) {
    tracing::info!(
        phase,
        rss_mb = process_diag::bytes_to_mb(snapshot.rss_bytes),
        vm_size_mb = process_diag::bytes_to_mb(snapshot.virtual_bytes),
        threads = snapshot.thread_count,
        fds = snapshot.open_fd_count,
        child_processes = snapshot.child_process_count,
        elapsed_ms = process_diag::elapsed_since_start().as_millis() as u64,
        "{message}"
    );
}

fn spawn_executor_preload_task() {
    if env_flag("VK_DISABLE_EXECUTOR_PRELOAD") {
        log_startup_skip(
            "executor_preload_spawn_skipped",
            "VK_DISABLE_EXECUTOR_PRELOAD",
        );
        return;
    }

    log_startup_phase("executor_preload_spawned");
    tokio::spawn(async move {
        log_startup_phase("executor_preload_task_started");
        executors::executors::utils::preload_global_executor_options_cache().await;
        log_startup_phase("executor_preload_task_finished");
    });
}

async fn run_startup_backfills(deployment: &DeploymentImpl) -> Result<(), DeploymentError> {
    let disable_all_backfills = env_flag("VK_DISABLE_STARTUP_BACKFILLS");

    if disable_all_backfills || env_flag("VK_DISABLE_BACKFILL_BEFORE_HEAD_COMMITS") {
        let env_var = if disable_all_backfills {
            "VK_DISABLE_STARTUP_BACKFILLS"
        } else {
            "VK_DISABLE_BACKFILL_BEFORE_HEAD_COMMITS"
        };
        log_startup_skip("backfill_before_head_commits_skipped", env_var);
    } else {
        deployment
            .container()
            .backfill_before_head_commits()
            .await
            .map_err(DeploymentError::from)?;
        log_startup_phase("backfill_before_head_commits_complete");
    }

    if disable_all_backfills || env_flag("VK_DISABLE_BACKFILL_REPO_NAMES") {
        let env_var = if disable_all_backfills {
            "VK_DISABLE_STARTUP_BACKFILLS"
        } else {
            "VK_DISABLE_BACKFILL_REPO_NAMES"
        };
        log_startup_skip("backfill_repo_names_skipped", env_var);
    } else {
        deployment
            .container()
            .backfill_repo_names()
            .await
            .map_err(DeploymentError::from)?;
        log_startup_phase("backfill_repo_names_complete");
    }

    Ok(())
}

fn startup_sampling_config() -> Option<(Duration, Duration)> {
    if !debug_memory_logs_enabled() {
        return None;
    }

    let interval_ms = std::env::var("VK_STARTUP_DIAGNOSTICS_INTERVAL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)?;
    let duration_secs = std::env::var("VK_STARTUP_DIAGNOSTICS_DURATION_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(600);

    Some((
        Duration::from_millis(interval_ms),
        Duration::from_secs(duration_secs),
    ))
}

fn debug_memory_logs_enabled() -> bool {
    env_flag("VK_DEBUG_MEMORY_LOGS")
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

const LEGACY_ATTACHMENT_MIGRATION_MARKER: &str = ".attachment-directories-migrated-v1";

#[derive(Default)]
struct DirectoryMigrationStats {
    moved_files: u64,
    removed_duplicates: u64,
    created_directories: u64,
    failures: u64,
}

impl DirectoryMigrationStats {
    fn merge(&mut self, other: DirectoryMigrationStats) {
        self.moved_files += other.moved_files;
        self.removed_duplicates += other.removed_duplicates;
        self.created_directories += other.created_directories;
        self.failures += other.failures;
    }
}

async fn migrate_legacy_attachment_directories(
    deployment: &DeploymentImpl,
) -> Result<(), DeploymentError> {
    let marker_path = asset_dir().join(LEGACY_ATTACHMENT_MIGRATION_MARKER);
    if marker_path.exists() {
        return Ok(());
    }

    let mut stats = DirectoryMigrationStats::default();

    let cache_root = utils::cache_dir();
    stats.merge(migrate_legacy_directory(
        &cache_root.join("images"),
        &cache_root.join("attachments"),
        false,
    ));

    for base_path in collect_attachment_migration_paths(deployment).await? {
        stats.merge(migrate_legacy_directory(
            &base_path.join(".vibe-images"),
            &base_path.join(utils::path::VIBE_ATTACHMENTS_DIR),
            true,
        ));
    }

    if stats.failures == 0 {
        fs::write(&marker_path, b"ok")?;
        tracing::info!(
            "Legacy attachment directory migration completed: moved {}, removed duplicates {}, created directories {}",
            stats.moved_files,
            stats.removed_duplicates,
            stats.created_directories
        );
    } else {
        tracing::warn!(
            "Legacy attachment directory migration completed with {} failures; will retry on next startup",
            stats.failures
        );
    }

    Ok(())
}

async fn collect_attachment_migration_paths(
    deployment: &DeploymentImpl,
) -> Result<Vec<PathBuf>, DeploymentError> {
    use db::models::{session::Session, workspace::Workspace, workspace_repo::WorkspaceRepo};

    let workspaces = Workspace::fetch_all(&deployment.db().pool).await?;
    let mut paths = HashSet::new();

    for workspace in workspaces {
        let Some(container_ref) = workspace.container_ref.as_deref() else {
            continue;
        };
        if container_ref.is_empty() {
            continue;
        }

        let workspace_root = PathBuf::from(container_ref);
        paths.insert(workspace_root.clone());

        for repo in
            WorkspaceRepo::find_repos_for_workspace(&deployment.db().pool, workspace.id).await?
        {
            let repo_base = match repo.default_working_dir.as_deref() {
                Some(default_dir) if !default_dir.is_empty() => {
                    workspace_root.join(&repo.name).join(default_dir)
                }
                _ => workspace_root.join(&repo.name),
            };
            paths.insert(repo_base);
        }

        for session in Session::find_by_workspace_id(&deployment.db().pool, workspace.id).await? {
            let base_path = match session.agent_working_dir.as_deref() {
                Some(dir) if !dir.is_empty() => workspace_root.join(dir),
                _ => workspace_root.clone(),
            };
            paths.insert(base_path);
        }
    }

    let mut paths = paths.into_iter().collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn migrate_legacy_directory(
    src_dir: &Path,
    dst_dir: &Path,
    ensure_gitignore: bool,
) -> DirectoryMigrationStats {
    let mut stats = DirectoryMigrationStats::default();

    if !src_dir.exists() {
        return stats;
    }

    if let Err(error) = fs::create_dir_all(dst_dir) {
        tracing::warn!(
            "Failed to create attachment directory {}: {}",
            dst_dir.display(),
            error
        );
        stats.failures += 1;
        return stats;
    }
    stats.created_directories += 1;

    if let Err(error) = migrate_directory_contents(src_dir, dst_dir, ensure_gitignore, &mut stats) {
        tracing::warn!(
            "Failed to migrate legacy attachment directory {} -> {}: {}",
            src_dir.display(),
            dst_dir.display(),
            error
        );
        stats.failures += 1;
    }

    if ensure_gitignore && let Err(error) = ensure_attachments_gitignore(dst_dir) {
        tracing::warn!(
            "Failed to ensure .gitignore in {}: {}",
            dst_dir.display(),
            error
        );
        stats.failures += 1;
    }

    if let Err(error) = remove_empty_dir_tree(src_dir) {
        tracing::warn!(
            "Failed to clean up legacy attachment directory {}: {}",
            src_dir.display(),
            error
        );
        stats.failures += 1;
    }

    stats
}

fn migrate_directory_contents(
    src_dir: &Path,
    dst_dir: &Path,
    ensure_gitignore: bool,
    stats: &mut DirectoryMigrationStats,
) -> io::Result<()> {
    for entry in fs::read_dir(src_dir)? {
        let entry = entry?;
        let src_path = entry.path();
        let file_name = entry.file_name();

        if ensure_gitignore && file_name == ".gitignore" {
            continue;
        }

        let dst_path = dst_dir.join(&file_name);
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            fs::create_dir_all(&dst_path)?;
            migrate_directory_contents(&src_path, &dst_path, false, stats)?;
            remove_empty_dir_tree(&src_path)?;
            continue;
        }

        if dst_path.exists() {
            fs::remove_file(&src_path)?;
            stats.removed_duplicates += 1;
            continue;
        }

        move_path(&src_path, &dst_path)?;
        stats.moved_files += 1;
    }

    Ok(())
}

fn move_path(src_path: &Path, dst_path: &Path) -> io::Result<()> {
    match fs::rename(src_path, dst_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            fs::copy(src_path, dst_path)?;
            fs::remove_file(src_path)
        }
        Err(error) => Err(error),
    }
}

fn ensure_attachments_gitignore(dir: &Path) -> io::Result<()> {
    let gitignore_path = dir.join(".gitignore");
    if !gitignore_path.exists() {
        fs::write(gitignore_path, "*\n")?;
    }
    Ok(())
}

fn remove_empty_dir_tree(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    if fs::read_dir(path)?.next().is_none() {
        fs::remove_dir(path)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, extract::ConnectInfo};
    use chrono::Utc;
    use db::models::execution_external_start::ExecutionExternalStart;
    use ed25519_dalek::SigningKey;
    use http::{Request, StatusCode};
    use rand::rngs::OsRng;
    use relay_client::RELAY_HEADER;
    use relay_control::signing::{
        NONCE_HEADER, REQUEST_SIGNATURE_HEADER, RelaySigningService, SIGNING_SESSION_HEADER,
        TIMESTAMP_HEADER,
    };
    use tempfile::TempDir;
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn operator_recovery_production_router_integration() {
        let temp = TempDir::new().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "startup::tests::operator_recovery_production_router_child",
                "--ignored",
                "--nocapture",
            ])
            .env("VK_TEST_ASSET_DIR", temp.path())
            .status()
            .unwrap();
        assert!(status.success());
    }

    async fn make_blocked(
        pool: &sqlx::SqlitePool,
        workspace: Uuid,
        uncertain: bool,
    ) -> (Uuid, String, i64) {
        let session = Uuid::new_v4();
        let process = Uuid::new_v4();
        sqlx::query("INSERT INTO workspaces(id,branch) VALUES(?1,'recovery-test')")
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
        sqlx::query("INSERT INTO execution_processes(id,session_id,run_reason,executor_action,status,dropped) VALUES(?1,?2,'codingagent','{}','running',FALSE)").bind(process).bind(session).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO agent_turn_admissions(token_id,operation_key,intended_process_id,workspace_id,fence,status,created_at,updated_at) VALUES(?1,?2,?3,?4,1,'started',?5,?5)").bind(Uuid::new_v4()).bind(format!("test:{process}")).bind(process).bind(workspace).bind(Utc::now()).execute(pool).await.unwrap();
        ExecutionExternalStart::authorize(pool, process)
            .await
            .unwrap();
        if uncertain {
            sqlx::query("UPDATE execution_external_starts SET state='claiming',spawn_requested_at=?2,external_process_id=?3,external_process_started_at='not-the-current-process' WHERE execution_process_id=?1")
                .bind(process).bind(Utc::now()).bind(std::process::id().to_string()).execute(pool).await.unwrap();
        }
        ExecutionExternalStart::mark_blocked(pool, process, "Recovery required")
            .await
            .unwrap();
        let record = ExecutionExternalStart::record(pool, process)
            .await
            .unwrap()
            .unwrap();
        (
            process,
            record.recovery_token.unwrap(),
            record.recovery_generation,
        )
    }

    fn recovery_request(method: &str, path: &str, body: String, peer: &str) -> Request<Body> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo::<std::net::SocketAddr>(peer.parse().unwrap()));
        request
    }

    #[tokio::test]
    #[ignore = "isolated child of operator_recovery_production_router_integration"]
    async fn operator_recovery_production_router_child() {
        let deployment = DeploymentImpl::new(CancellationToken::new()).await.unwrap();
        let pool = &deployment.db().pool;
        let workspace = Uuid::new_v4();
        let (process, token, generation) = make_blocked(pool, workspace, false).await;
        let status_path = format!(
            "/api/workspaces/{workspace}/execution/execution-processes/{process}/external-start-status"
        );
        assert_eq!(
            routes::app_router(deployment.clone(), false)
                .oneshot(recovery_request(
                    "GET",
                    &status_path,
                    String::new(),
                    "127.0.0.1:4100"
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            routes::app_router(deployment.clone(), false)
                .oneshot(recovery_request(
                    "GET",
                    &status_path,
                    String::new(),
                    "203.0.113.9:4100"
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let post_path = format!(
            "/api/workspaces/{workspace}/execution/execution-processes/{process}/confirm-stopped"
        );
        let body =
            serde_json::json!({"recoveryToken":token,"recoveryGeneration":generation}).to_string();
        assert_eq!(
            routes::app_router(deployment.clone(), false)
                .oneshot(recovery_request(
                    "POST",
                    &post_path,
                    body.clone(),
                    "127.0.0.1:4100"
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            ExecutionExternalStart::state(pool, process)
                .await
                .unwrap()
                .as_deref(),
            Some("failed")
        );
        let admission: String = sqlx::query_scalar(
            "SELECT status FROM agent_turn_admissions WHERE intended_process_id=?1",
        )
        .bind(process)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(admission, "released");
        let actor: String = sqlx::query_scalar(
            "SELECT actor FROM execution_external_start_recoveries WHERE execution_process_id=?1",
        )
        .bind(process)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(actor, "local_ui");
        assert_eq!(
            routes::app_router(deployment.clone(), false)
                .oneshot(recovery_request(
                    "POST",
                    &post_path,
                    body.clone(),
                    "127.0.0.1:4100"
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        let legacy = format!("/api/execution-processes/{process}/confirm-stopped");
        assert_eq!(
            routes::app_router(deployment.clone(), false)
                .oneshot(recovery_request(
                    "POST",
                    &legacy,
                    body.clone(),
                    "127.0.0.1:4100"
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        let legacy_get = format!("/api/execution-processes/{process}/external-start-status");
        assert_eq!(
            routes::app_router(deployment.clone(), false)
                .oneshot(recovery_request(
                    "GET",
                    &legacy_get,
                    String::new(),
                    "127.0.0.1:4100"
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        let other = Uuid::new_v4();
        sqlx::query("INSERT INTO workspaces(id,branch) VALUES(?1,'other')")
            .bind(other)
            .execute(pool)
            .await
            .unwrap();
        let wrong = format!(
            "/api/workspaces/{other}/execution/execution-processes/{process}/confirm-stopped"
        );
        assert_eq!(
            routes::app_router(deployment.clone(), false)
                .oneshot(recovery_request(
                    "POST",
                    &wrong,
                    body.clone(),
                    "127.0.0.1:4100"
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            ExecutionExternalStart::state(pool, process)
                .await
                .unwrap()
                .as_deref(),
            Some("failed")
        );

        let stale_workspace = Uuid::new_v4();
        let (stale_process, stale_token, stale_generation) =
            make_blocked(pool, stale_workspace, false).await;
        let stale_path = format!(
            "/api/workspaces/{stale_workspace}/execution/execution-processes/{stale_process}/confirm-stopped"
        );
        let stale_body = serde_json::json!({"recoveryToken":format!("stale-{stale_token}"),"recoveryGeneration":stale_generation}).to_string();
        assert_eq!(
            routes::app_router(deployment.clone(), false)
                .oneshot(recovery_request(
                    "POST",
                    &stale_path,
                    stale_body,
                    "127.0.0.1:4100"
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            ExecutionExternalStart::state(pool, stale_process)
                .await
                .unwrap()
                .as_deref(),
            Some("blocked")
        );
        let stale_generation_body = serde_json::json!({"recoveryToken":stale_token,"recoveryGeneration":stale_generation + 1}).to_string();
        assert_eq!(
            routes::app_router(deployment.clone(), false)
                .oneshot(recovery_request(
                    "POST",
                    &stale_path,
                    stale_generation_body,
                    "127.0.0.1:4100"
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            ExecutionExternalStart::state(pool, stale_process)
                .await
                .unwrap()
                .as_deref(),
            Some("blocked")
        );

        let unresolved_workspace = Uuid::new_v4();
        let (unresolved, unresolved_token, unresolved_generation) =
            make_blocked(pool, unresolved_workspace, true).await;
        let unresolved_path = format!(
            "/api/workspaces/{unresolved_workspace}/execution/execution-processes/{unresolved}/confirm-stopped"
        );
        let unresolved_body = serde_json::json!({"recoveryToken":unresolved_token,"recoveryGeneration":unresolved_generation}).to_string();
        let unresolved_status = routes::app_router(deployment.clone(), false)
            .oneshot(recovery_request(
                "POST",
                &unresolved_path,
                unresolved_body,
                "127.0.0.1:4100",
            ))
            .await
            .unwrap()
            .status();
        assert_eq!(unresolved_status, StatusCode::BAD_REQUEST);
        {
            assert_eq!(
                ExecutionExternalStart::state(pool, unresolved)
                    .await
                    .unwrap()
                    .as_deref(),
                Some("blocked")
            );
            let held: String = sqlx::query_scalar(
                "SELECT status FROM agent_turn_admissions WHERE intended_process_id=?1",
            )
            .bind(unresolved)
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(held, "started");
        }

        let relay_workspace = Uuid::new_v4();
        let (relay_process, relay_token, relay_generation) =
            make_blocked(pool, relay_workspace, false).await;
        let relay_path = format!(
            "/api/workspaces/{relay_workspace}/execution/execution-processes/{relay_process}/confirm-stopped"
        );
        let relay_body =
            serde_json::json!({"recoveryToken":relay_token,"recoveryGeneration":relay_generation})
                .to_string();
        let client_key = SigningKey::generate(&mut OsRng);
        let signing_session = deployment
            .relay_signing()
            .create_session(client_key.verifying_key())
            .await;
        let signer = RelaySigningService::new(client_key);
        let relay_status_path = format!(
            "/api/workspaces/{relay_workspace}/execution/execution-processes/{relay_process}/external-start-status"
        );
        let get_signature = signer.sign_request(signing_session, "GET", &relay_status_path, b"");
        let mut signed_get =
            recovery_request("GET", &relay_status_path, String::new(), "203.0.113.9:4100");
        signed_get
            .headers_mut()
            .insert(RELAY_HEADER, "1".parse().unwrap());
        signed_get.headers_mut().insert(
            SIGNING_SESSION_HEADER,
            get_signature
                .signing_session_id
                .to_string()
                .parse()
                .unwrap(),
        );
        signed_get.headers_mut().insert(
            TIMESTAMP_HEADER,
            get_signature.timestamp.to_string().parse().unwrap(),
        );
        signed_get.headers_mut().insert(
            NONCE_HEADER,
            get_signature.nonce.to_string().parse().unwrap(),
        );
        signed_get.headers_mut().insert(
            REQUEST_SIGNATURE_HEADER,
            get_signature.signature_b64.parse().unwrap(),
        );
        assert_eq!(
            routes::app_router(deployment.clone(), false)
                .oneshot(signed_get)
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let signature =
            signer.sign_request(signing_session, "POST", &relay_path, relay_body.as_bytes());
        let mut signed =
            recovery_request("POST", &relay_path, relay_body.clone(), "203.0.113.9:4100");
        signed
            .headers_mut()
            .insert(RELAY_HEADER, "1".parse().unwrap());
        signed.headers_mut().insert(
            SIGNING_SESSION_HEADER,
            signature.signing_session_id.to_string().parse().unwrap(),
        );
        signed.headers_mut().insert(
            TIMESTAMP_HEADER,
            signature.timestamp.to_string().parse().unwrap(),
        );
        signed
            .headers_mut()
            .insert(NONCE_HEADER, signature.nonce.to_string().parse().unwrap());
        signed.headers_mut().insert(
            REQUEST_SIGNATURE_HEADER,
            signature.signature_b64.parse().unwrap(),
        );
        assert_eq!(
            routes::app_router(deployment.clone(), false)
                .oneshot(signed)
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let relay_actor: String = sqlx::query_scalar(
            "SELECT actor FROM execution_external_start_recoveries WHERE execution_process_id=?1",
        )
        .bind(relay_process)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(relay_actor, format!("relay:{signing_session}"));

        let different_signature =
            signer.sign_request(signing_session, "POST", &post_path, body.as_bytes());
        let mut different_actor = recovery_request("POST", &post_path, body, "203.0.113.9:4100");
        different_actor
            .headers_mut()
            .insert(RELAY_HEADER, "1".parse().unwrap());
        different_actor.headers_mut().insert(
            SIGNING_SESSION_HEADER,
            different_signature
                .signing_session_id
                .to_string()
                .parse()
                .unwrap(),
        );
        different_actor.headers_mut().insert(
            TIMESTAMP_HEADER,
            different_signature.timestamp.to_string().parse().unwrap(),
        );
        different_actor.headers_mut().insert(
            NONCE_HEADER,
            different_signature.nonce.to_string().parse().unwrap(),
        );
        different_actor.headers_mut().insert(
            REQUEST_SIGNATURE_HEADER,
            different_signature.signature_b64.parse().unwrap(),
        );
        assert_eq!(
            routes::app_router(deployment.clone(), false)
                .oneshot(different_actor)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let mut missing =
            recovery_request("POST", &relay_path, relay_body.clone(), "203.0.113.9:4100");
        missing
            .headers_mut()
            .insert(RELAY_HEADER, "1".parse().unwrap());
        assert_eq!(
            routes::app_router(deployment.clone(), false)
                .oneshot(missing)
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let bad = signer.sign_request(signing_session, "POST", &relay_path, relay_body.as_bytes());
        let mut invalid = recovery_request("POST", &relay_path, relay_body, "203.0.113.9:4100");
        invalid
            .headers_mut()
            .insert(RELAY_HEADER, "1".parse().unwrap());
        invalid.headers_mut().insert(
            SIGNING_SESSION_HEADER,
            bad.signing_session_id.to_string().parse().unwrap(),
        );
        invalid
            .headers_mut()
            .insert(TIMESTAMP_HEADER, bad.timestamp.to_string().parse().unwrap());
        invalid
            .headers_mut()
            .insert(NONCE_HEADER, bad.nonce.to_string().parse().unwrap());
        invalid.headers_mut().insert(
            REQUEST_SIGNATURE_HEADER,
            "invalid-signature".parse().unwrap(),
        );
        assert_eq!(
            routes::app_router(deployment, false)
                .oneshot(invalid)
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn migrates_legacy_cache_directory_contents() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("images");
        let dst = temp_dir.path().join("attachments");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("asset.png"), b"hello").unwrap();

        let stats = migrate_legacy_directory(&src, &dst, false);

        assert_eq!(stats.moved_files, 1);
        assert!(dst.join("asset.png").exists());
        assert!(!src.exists());
    }

    #[test]
    fn removes_legacy_duplicates_when_destination_exists() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join(".vibe-images");
        let dst = temp_dir.path().join(".vibe-attachments");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        fs::write(src.join("asset.png"), b"old").unwrap();
        fs::write(dst.join("asset.png"), b"new").unwrap();

        let stats = migrate_legacy_directory(&src, &dst, true);

        assert_eq!(stats.removed_duplicates, 1);
        assert_eq!(fs::read(dst.join("asset.png")).unwrap(), b"new");
        assert!(!src.exists());
    }

    #[test]
    fn ensures_gitignore_for_workspace_attachment_dir() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join(".vibe-images");
        let dst = temp_dir.path().join(".vibe-attachments");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("file.pdf"), b"attachment").unwrap();

        migrate_legacy_directory(&src, &dst, true);

        assert_eq!(fs::read_to_string(dst.join(".gitignore")).unwrap(), "*\n");
        assert!(dst.join("file.pdf").exists());
    }
}
