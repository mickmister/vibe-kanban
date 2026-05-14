use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
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
use futures_util::{
    FutureExt, StreamExt, TryStreamExt,
    future::{BoxFuture, Shared},
    stream::{self, BoxStream},
};
use serde::Deserialize;
use services::services::container::ContainerService;
use tokio::sync::Mutex;
use utils::{log_msg::LogMsg, msg_store::MsgStore, response::ApiResponse};
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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum NormalizedLogReplayMode {
    Live,
    Historic,
}

type SharedNormalizedLogHistoryFuture = Shared<BoxFuture<'static, Option<Arc<Vec<String>>>>>;
type NormalizedLogHistoryInflight =
    Arc<Mutex<HashMap<(Uuid, NormalizedLogReplayMode), SharedNormalizedLogHistoryFuture>>>;

fn normalized_log_history_inflight() -> &'static NormalizedLogHistoryInflight {
    static INFLIGHT: OnceLock<NormalizedLogHistoryInflight> = OnceLock::new();
    INFLIGHT.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
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
        if let Some(store) = deployment.container().get_msg_store_by_id(&exec_id).await {
            let stream = build_live_normalized_logs_stream(exec_id, store).await;
            if let Err(e) = handle_normalized_logs_ws(socket, stream).await {
                tracing::warn!("normalized logs WS closed: {}", e);
            }
            return;
        }

        match get_historic_normalized_log_messages_single_flight(&deployment, exec_id).await {
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

async fn build_live_normalized_logs_stream(
    exec_id: Uuid,
    store: Arc<MsgStore>,
) -> BoxStream<'static, anyhow::Result<Message>> {
    let receiver = store.get_receiver();
    let payloads = get_live_normalized_log_messages_single_flight(exec_id, store).await;
    let history_stream = stream::iter(
        (*payloads)
            .clone()
            .into_iter()
            .map(|payload| Ok::<_, anyhow::Error>(Message::Text(payload.into()))),
    );

    let live_stream = stream::unfold(receiver, move |mut receiver| async move {
        loop {
            match receiver.recv().await {
                Ok(LogMsg::JsonPatch(patch)) => {
                    return Some((
                        Ok::<_, anyhow::Error>(LogMsg::JsonPatch(patch).to_ws_message_unchecked()),
                        receiver,
                    ));
                }
                Ok(LogMsg::Finished) => return None,
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::error!(
                        skipped = n,
                        execution_process_id = %exec_id,
                        "normalized log stream lagged for subscriber"
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    history_stream
        .chain(live_stream)
        .chain(stream::once(async {
            Ok::<_, anyhow::Error>(LogMsg::Finished.to_ws_message_unchecked())
        }))
        .boxed()
}

async fn get_live_normalized_log_messages_single_flight(
    exec_id: Uuid,
    store: Arc<MsgStore>,
) -> Arc<Vec<String>> {
    get_normalized_log_messages_single_flight(
        NormalizedLogReplayMode::Live,
        exec_id,
        async move { Some(collect_live_normalized_log_messages(&store)) }.boxed(),
    )
    .await
    .unwrap_or_else(|| Arc::new(Vec::new()))
}

async fn get_historic_normalized_log_messages_single_flight(
    deployment: &DeploymentImpl,
    exec_id: Uuid,
) -> Option<Arc<Vec<String>>> {
    get_normalized_log_messages_single_flight(NormalizedLogReplayMode::Historic, exec_id, {
        let deployment = deployment.clone();
        async move { collect_historic_normalized_log_messages(&deployment, exec_id).await }.boxed()
    })
    .await
}

async fn get_normalized_log_messages_single_flight(
    mode: NormalizedLogReplayMode,
    exec_id: Uuid,
    future: BoxFuture<'static, Option<Arc<Vec<String>>>>,
) -> Option<Arc<Vec<String>>> {
    let inflight = normalized_log_history_inflight();
    let key = (exec_id, mode);
    let (future, created_here) = {
        let mut guard = inflight.lock().await;
        if let Some(future) = guard.get(&key) {
            (future.clone(), false)
        } else {
            let future = future.shared();
            guard.insert(key, future.clone());
            (future, true)
        }
    };

    let result = future.await;

    if created_here {
        let mut guard = inflight.lock().await;
        guard.remove(&key);
    }

    result
}

fn collect_live_normalized_log_messages(store: &MsgStore) -> Arc<Vec<String>> {
    let messages = store
        .get_history()
        .into_iter()
        .take_while(|msg| !matches!(msg, LogMsg::Finished))
        .filter_map(|msg| match msg {
            LogMsg::JsonPatch(patch) => match LogMsg::JsonPatch(patch).to_ws_message_unchecked() {
                Message::Text(payload) => Some(payload.to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect();

    Arc::new(messages)
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

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use futures_util::FutureExt;
    use tokio::sync::{Mutex, oneshot};
    use uuid::Uuid;

    use super::{NormalizedLogReplayMode, get_normalized_log_messages_single_flight};

    #[tokio::test]
    async fn single_flight_shares_same_mode_requests() {
        let exec_id = Uuid::new_v4();
        let call_count = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let started_tx = Arc::new(Mutex::new(Some(started_tx)));
        let shared_rx = Arc::new(Mutex::new(Some(release_rx)));

        let task1 = {
            let call_count = call_count.clone();
            let started_tx = started_tx.clone();
            let shared_rx = shared_rx.clone();
            tokio::spawn(async move {
                get_normalized_log_messages_single_flight(
                    NormalizedLogReplayMode::Historic,
                    exec_id,
                    async move {
                        call_count.fetch_add(1, Ordering::SeqCst);
                        if let Some(tx) = started_tx.lock().await.take() {
                            let _ = tx.send(());
                        }
                        if let Some(rx) = shared_rx.lock().await.take() {
                            let _ = rx.await;
                        }
                        Some(Arc::new(vec!["historic".to_string()]))
                    }
                    .boxed(),
                )
                .await
            })
        };

        started_rx.await.unwrap();

        let task2 = {
            let call_count = call_count.clone();
            tokio::spawn(async move {
                get_normalized_log_messages_single_flight(
                    NormalizedLogReplayMode::Historic,
                    exec_id,
                    async move {
                        call_count.fetch_add(1, Ordering::SeqCst);
                        Some(Arc::new(vec!["duplicate".to_string()]))
                    }
                    .boxed(),
                )
                .await
            })
        };

        release_tx.send(()).unwrap();

        let result1 = task1.await.unwrap().unwrap();
        let result2 = task2.await.unwrap().unwrap();

        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        assert_eq!(&*result1, &vec!["historic".to_string()]);
        assert_eq!(&*result2, &vec!["historic".to_string()]);
    }

    #[tokio::test]
    async fn single_flight_does_not_share_across_modes() {
        let exec_id = Uuid::new_v4();
        let live_count = Arc::new(AtomicUsize::new(0));
        let historic_count = Arc::new(AtomicUsize::new(0));

        let live = {
            let live_count = live_count.clone();
            tokio::spawn(async move {
                get_normalized_log_messages_single_flight(
                    NormalizedLogReplayMode::Live,
                    exec_id,
                    async move {
                        live_count.fetch_add(1, Ordering::SeqCst);
                        Some(Arc::new(vec!["live".to_string()]))
                    }
                    .boxed(),
                )
                .await
            })
        };

        let historic = {
            let historic_count = historic_count.clone();
            tokio::spawn(async move {
                get_normalized_log_messages_single_flight(
                    NormalizedLogReplayMode::Historic,
                    exec_id,
                    async move {
                        historic_count.fetch_add(1, Ordering::SeqCst);
                        Some(Arc::new(vec!["historic".to_string()]))
                    }
                    .boxed(),
                )
                .await
            })
        };

        let live_result = live.await.unwrap().unwrap();
        let historic_result = historic.await.unwrap().unwrap();

        assert_eq!(live_count.load(Ordering::SeqCst), 1);
        assert_eq!(historic_count.load(Ordering::SeqCst), 1);
        assert_eq!(&*live_result, &vec!["live".to_string()]);
        assert_eq!(&*historic_result, &vec!["historic".to_string()]);
    }
}
