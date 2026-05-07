use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use anyhow;
use axum::{
    Extension, Router,
    extract::{Path, Query, State, ws::Message},
    middleware::from_fn_with_state,
    response::{IntoResponse, Json as ResponseJson},
    routing::{get, post},
};
use db::models::{
    execution_process::{ExecutionProcess, ExecutionProcessStatus},
    execution_process_repo_state::ExecutionProcessRepoState,
};
use deployment::Deployment;
use futures_util::{StreamExt, TryStreamExt};
use serde::Deserialize;
use services::services::container::ContainerService;
use tokio::sync::{Mutex, Notify};
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{
    DeploymentImpl,
    error::ApiError,
    middleware::{
        load_execution_process_middleware,
        signed_ws::{MaybeSignedWebSocket, SignedWsUpgrade},
    },
};

#[derive(Debug, Deserialize)]
struct SessionExecutionProcessQuery {
    pub session_id: Uuid,
    /// If true, include soft-deleted (dropped) processes in results/stream
    #[serde(default)]
    pub show_soft_deleted: Option<bool>,
}

const NORMALIZED_LOG_HISTORY_CACHE_TTL: Duration = Duration::from_secs(15);

#[derive(Clone)]
struct CachedNormalizedLogHistory {
    messages: Arc<Vec<String>>,
    computed_at: Instant,
}

enum NormalizedLogHistoryCacheState {
    Ready(Option<CachedNormalizedLogHistory>),
    Computing { notify: Arc<Notify> },
}

impl Default for NormalizedLogHistoryCacheState {
    fn default() -> Self {
        Self::Ready(None)
    }
}

type NormalizedLogHistoryCache = Arc<Mutex<HashMap<Uuid, NormalizedLogHistoryCacheState>>>;

fn normalized_log_history_cache() -> &'static NormalizedLogHistoryCache {
    static CACHE: OnceLock<NormalizedLogHistoryCache> = OnceLock::new();
    CACHE.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

fn normalized_log_history_fresh(entry: &CachedNormalizedLogHistory) -> bool {
    entry.computed_at.elapsed() < NORMALIZED_LOG_HISTORY_CACHE_TTL
}

async fn get_execution_process_by_id(
    Extension(execution_process): Extension<ExecutionProcess>,
    State(_deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<ExecutionProcess>>, ApiError> {
    Ok(ResponseJson(ApiResponse::success(execution_process)))
}

async fn stream_raw_logs_ws(
    ws: SignedWsUpgrade,
    State(deployment): State<DeploymentImpl>,
    Path(exec_id): Path<Uuid>,
) -> impl IntoResponse {
    // Always accept the WebSocket upgrade — handle "not found" inside the
    // connection by sending `finished` and closing cleanly, instead of
    // rejecting with HTTP 404 which the browser surfaces as an opaque
    // connection failure.
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = handle_raw_logs_ws(socket, deployment, exec_id).await {
            tracing::warn!("raw logs WS closed: {}", e);
        }
    })
}

async fn handle_raw_logs_ws(
    mut socket: MaybeSignedWebSocket,
    deployment: DeploymentImpl,
    exec_id: Uuid,
) -> anyhow::Result<()> {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use executors::logs::utils::patch::ConversationPatch;
    use utils::log_msg::LogMsg;

    // Get the raw stream — if not found, send finished and close cleanly
    let raw_stream = match deployment.container().stream_raw_logs(&exec_id).await {
        Some(stream) => stream,
        None => {
            // No logs available: send finished so the client gets a clean
            // close instead of retrying endlessly.
            let _ = socket
                .send(LogMsg::Finished.to_ws_message_unchecked())
                .await;
            let _ = socket.close().await;
            return Ok(());
        }
    };

    let counter = Arc::new(AtomicUsize::new(0));
    let mut stream = raw_stream.map_ok({
        let counter = counter.clone();
        move |m| match m {
            LogMsg::Stdout(content) => {
                let index = counter.fetch_add(1, Ordering::SeqCst);
                let patch = ConversationPatch::add_stdout(index, content);
                LogMsg::JsonPatch(patch).to_ws_message_unchecked()
            }
            LogMsg::Stderr(content) => {
                let index = counter.fetch_add(1, Ordering::SeqCst);
                let patch = ConversationPatch::add_stderr(index, content);
                LogMsg::JsonPatch(patch).to_ws_message_unchecked()
            }
            LogMsg::Finished => LogMsg::Finished.to_ws_message_unchecked(),
            _ => unreachable!("Raw stream should only have Stdout/Stderr/Finished"),
        }
    });

    loop {
        tokio::select! {
            item = stream.next() => {
                match item {
                    Some(Ok(msg)) => {
                        if socket.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        tracing::error!("stream error: {}", e);
                        break;
                    }
                    None => break,
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
    // Send a proper close frame so the client sees code 1000 (normal closure)
    // instead of an abnormal TCP drop that triggers reconnection attempts.
    let _ = socket.close().await;
    Ok(())
}

async fn stream_normalized_logs_ws(
    ws: SignedWsUpgrade,
    State(deployment): State<DeploymentImpl>,
    Path(exec_id): Path<Uuid>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        if deployment
            .container()
            .get_msg_store_by_id(&exec_id)
            .await
            .is_some()
        {
            let stream = deployment
                .container()
                .stream_normalized_logs(&exec_id)
                .await;
            match stream {
                Some(stream) => {
                    let stream = stream
                        .map_ok(|msg| msg.to_ws_message_unchecked())
                        .err_into::<anyhow::Error>()
                        .into_stream();
                    if let Err(e) = handle_normalized_logs_ws(socket, stream).await {
                        tracing::warn!("normalized logs WS closed: {}", e);
                    }
                }
                None => {
                    let mut socket = socket;
                    let _ = socket
                        .send(utils::log_msg::LogMsg::Finished.to_ws_message_unchecked())
                        .await;
                    let _ = socket.close().await;
                }
            }
            return;
        }

        match get_cached_historic_normalized_log_messages(&deployment, exec_id).await {
            Some(messages) => {
                let payloads = (*messages).clone();
                let stream = futures_util::stream::iter(
                    payloads
                        .into_iter()
                        .map(|payload| Ok::<_, anyhow::Error>(Message::Text(payload.into()))),
                );
                if let Err(e) = handle_normalized_logs_ws(socket, stream).await {
                    tracing::warn!("normalized logs WS closed: {}", e);
                }
            }
            None => {
                let mut socket = socket;
                let _ = socket
                    .send(utils::log_msg::LogMsg::Finished.to_ws_message_unchecked())
                    .await;
                let _ = socket.close().await;
            }
        }
    })
}

async fn get_cached_historic_normalized_log_messages(
    deployment: &DeploymentImpl,
    exec_id: Uuid,
) -> Option<Arc<Vec<String>>> {
    let cache = normalized_log_history_cache();

    loop {
        let mut cache_guard = cache.lock().await;
        let state = cache_guard.entry(exec_id).or_default();

        match state {
            NormalizedLogHistoryCacheState::Ready(Some(entry))
                if normalized_log_history_fresh(entry) =>
            {
                return Some(entry.messages.clone());
            }
            NormalizedLogHistoryCacheState::Ready(_) => {
                let notify = Arc::new(Notify::new());
                *state = NormalizedLogHistoryCacheState::Computing {
                    notify: notify.clone(),
                };
                drop(cache_guard);

                let compute_result =
                    collect_historic_normalized_log_messages(deployment, exec_id).await;

                let mut cache_guard = cache.lock().await;
                let state = cache_guard.entry(exec_id).or_default();
                match &compute_result {
                    Some(messages) => {
                        *state = NormalizedLogHistoryCacheState::Ready(Some(
                            CachedNormalizedLogHistory {
                                messages: messages.clone(),
                                computed_at: Instant::now(),
                            },
                        ));
                    }
                    None => {
                        *state = NormalizedLogHistoryCacheState::Ready(None);
                    }
                }
                drop(cache_guard);
                notify.notify_waiters();
                return compute_result;
            }
            NormalizedLogHistoryCacheState::Computing { notify } => {
                let notify = notify.clone();
                drop(cache_guard);
                notify.notified().await;
            }
        }
    }
}

async fn collect_historic_normalized_log_messages(
    deployment: &DeploymentImpl,
    exec_id: Uuid,
) -> Option<Arc<Vec<String>>> {
    let stream = deployment
        .container()
        .stream_normalized_logs(&exec_id)
        .await?;
    let mut stream = stream.err_into::<anyhow::Error>().into_stream();
    let mut messages = Vec::new();

    while let Some(item) = stream.next().await {
        match item {
            Ok(msg) => match msg.to_ws_message_unchecked() {
                Message::Text(payload) => messages.push(payload.to_string()),
                _ => continue,
            },
            Err(e) => {
                tracing::warn!(
                    execution_process_id = %exec_id,
                    error = %e,
                    "failed to collect historic normalized logs"
                );
                return None;
            }
        }
    }

    Some(Arc::new(messages))
}

async fn handle_normalized_logs_ws(
    mut socket: MaybeSignedWebSocket,
    stream: impl futures_util::Stream<Item = anyhow::Result<Message>> + Unpin + Send + 'static,
) -> anyhow::Result<()> {
    let mut stream = stream;
    loop {
        tokio::select! {
            item = stream.next() => {
                match item {
                    Some(Ok(msg)) => {
                        if socket.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        tracing::error!("stream error: {}", e);
                        break;
                    }
                    None => break,
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

async fn stop_execution_process(
    Extension(execution_process): Extension<ExecutionProcess>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<()>>, ApiError> {
    deployment
        .container()
        .stop_execution(&execution_process, ExecutionProcessStatus::Killed)
        .await?;

    Ok(ResponseJson(ApiResponse::success(())))
}

async fn stream_execution_processes_by_session_ws(
    ws: SignedWsUpgrade,
    State(deployment): State<DeploymentImpl>,
    Query(query): Query<SessionExecutionProcessQuery>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = handle_execution_processes_by_session_ws(
            socket,
            deployment,
            query.session_id,
            query.show_soft_deleted.unwrap_or(false),
        )
        .await
        {
            tracing::warn!("execution processes by session WS closed: {}", e);
        }
    })
}

async fn handle_execution_processes_by_session_ws(
    mut socket: MaybeSignedWebSocket,
    deployment: DeploymentImpl,
    session_id: uuid::Uuid,
    show_soft_deleted: bool,
) -> anyhow::Result<()> {
    // Get the raw stream and convert LogMsg to WebSocket messages
    let mut stream = deployment
        .events()
        .stream_execution_processes_for_session_raw(session_id, show_soft_deleted)
        .await?
        .map_ok(|msg| msg.to_ws_message_unchecked());

    loop {
        tokio::select! {
            item = stream.next() => {
                match item {
                    Some(Ok(msg)) => {
                        if socket.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        tracing::error!("stream error: {}", e);
                        break;
                    }
                    None => break,
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
    Ok(())
}

async fn get_execution_process_repo_states(
    Extension(execution_process): Extension<ExecutionProcess>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<Vec<ExecutionProcessRepoState>>>, ApiError> {
    let pool = &deployment.db().pool;
    let repo_states =
        ExecutionProcessRepoState::find_by_execution_process_id(pool, execution_process.id).await?;
    Ok(ResponseJson(ApiResponse::success(repo_states)))
}

pub(super) fn router(deployment: &DeploymentImpl) -> Router<DeploymentImpl> {
    let workspace_id_router = Router::new()
        .route("/", get(get_execution_process_by_id))
        .route("/stop", post(stop_execution_process))
        .route("/repo-states", get(get_execution_process_repo_states))
        .route("/raw-logs/ws", get(stream_raw_logs_ws))
        .route("/normalized-logs/ws", get(stream_normalized_logs_ws))
        .layer(from_fn_with_state(
            deployment.clone(),
            load_execution_process_middleware,
        ));

    let workspaces_router = Router::new()
        .route(
            "/stream/session/ws",
            get(stream_execution_processes_by_session_ws),
        )
        .nest("/{id}", workspace_id_router);

    Router::new().nest("/execution-processes", workspaces_router)
}
