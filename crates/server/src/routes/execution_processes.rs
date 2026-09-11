use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, OnceLock},
};

use anyhow;
use axum::{
    Extension, Router,
    extract::{ConnectInfo, Path, Query, State, ws::Message},
    middleware::from_fn_with_state,
    response::{IntoResponse, Json as ResponseJson},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use db::models::{
    coding_agent_turn::{
        CODING_AGENT_PROMPT_PREVIEW_MAX_CHARS, CODING_AGENT_RESPONSE_SUMMARY_MAX_CHARS,
        CodingAgentResponseRecord, CodingAgentTurn,
    },
    execution_process::{ExecutionProcess, ExecutionProcessError, ExecutionProcessStatus},
    execution_process_repo_state::ExecutionProcessRepoState,
    session::Session,
};
use deployment::Deployment;
use futures_util::{
    FutureExt, StreamExt, TryStreamExt,
    future::{BoxFuture, Shared},
    stream::{self, BoxStream},
};
use serde::{Deserialize, Serialize};
use services::services::container::ContainerService;
use tokio::sync::Mutex;
use tracing::Instrument;
use ts_rs::TS;
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

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentResponseSourceKind {
    CodingAgentTurnSummary,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentPromptSourceKind {
    CodingAgentTurnPrompt,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct AgentResponse {
    pub execution_process_id: Uuid,
    pub session_id: Uuid,
    pub workspace_id: Uuid,
    pub status: ExecutionProcessStatus,
    pub completed_at: Option<DateTime<Utc>>,
    pub coding_agent_turn_id: Option<Uuid>,
    pub agent_session_id: Option<String>,
    pub agent_message_id: Option<String>,
    pub content: Option<String>,
    pub truncated: bool,
    pub max_chars: usize,
    pub source_kind: AgentResponseSourceKind,
    pub prompt_preview: Option<String>,
    pub prompt_truncated: bool,
    pub prompt_max_chars: usize,
    pub prompt_source_kind: AgentPromptSourceKind,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExternalStartStatus {
    pub state: String,
    pub message: String,
    pub can_confirm_stopped: bool,
    pub recovery_token: Option<String>,
    pub recovery_generation: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfirmExternalProcessStoppedRequest {
    recovery_token: String,
    recovery_generation: i64,
}

fn authorize_external_start_recovery(
    authorized_workspace_id: Uuid,
    process_workspace_id: Uuid,
    request: &ConfirmExternalProcessStoppedRequest,
    request_signature: Option<&crate::middleware::RelayRequestSignatureContext>,
    peer: Option<SocketAddr>,
) -> Result<String, ApiError> {
    let actor = verified_recovery_actor(request_signature, peer)?;
    if request.recovery_token.trim().is_empty() || request.recovery_generation < 1 {
        return Err(ApiError::Unauthorized);
    }
    if process_workspace_id != authorized_workspace_id {
        return Err(ApiError::Forbidden(
            "Process workspace does not match.".into(),
        ));
    }
    Ok(actor)
}

fn verified_recovery_actor(
    request_signature: Option<&crate::middleware::RelayRequestSignatureContext>,
    peer: Option<SocketAddr>,
) -> Result<String, ApiError> {
    if let Some(signature) = request_signature {
        return Ok(format!("relay:{}", signature.signing_session_id));
    }
    match peer {
        Some(peer) if peer.ip().is_loopback() => Ok("local_ui".to_string()),
        _ => Err(ApiError::Unauthorized),
    }
}

impl AgentResponse {
    pub fn from_record(record: CodingAgentResponseRecord) -> Self {
        let truncated = record
            .summary
            .as_deref()
            .is_some_and(CodingAgentTurn::summary_is_truncated);
        let (prompt_preview, prompt_truncated) = record
            .prompt
            .as_deref()
            .map(truncate_prompt_preview)
            .unwrap_or((None, false));

        AgentResponse {
            execution_process_id: record.execution_process_id,
            session_id: record.session_id,
            workspace_id: record.workspace_id,
            status: record.status,
            completed_at: record.completed_at,
            coding_agent_turn_id: record.coding_agent_turn_id,
            agent_session_id: record.agent_session_id,
            agent_message_id: record.agent_message_id,
            content: record.summary,
            truncated,
            max_chars: CODING_AGENT_RESPONSE_SUMMARY_MAX_CHARS,
            source_kind: AgentResponseSourceKind::CodingAgentTurnSummary,
            prompt_preview,
            prompt_truncated,
            prompt_max_chars: CODING_AGENT_PROMPT_PREVIEW_MAX_CHARS,
            prompt_source_kind: AgentPromptSourceKind::CodingAgentTurnPrompt,
        }
    }
}

fn truncate_prompt_preview(prompt: &str) -> (Option<String>, bool) {
    if prompt.chars().count() > CODING_AGENT_PROMPT_PREVIEW_MAX_CHARS {
        let preview = prompt
            .chars()
            .take(CODING_AGENT_PROMPT_PREVIEW_MAX_CHARS)
            .collect::<String>();
        (Some(format!("{preview}...")), true)
    } else {
        (Some(prompt.to_string()), false)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum NormalizedLogReplayMode {
    Historic,
}

type SharedNormalizedLogHistoryFuture = Shared<BoxFuture<'static, Option<Arc<Vec<String>>>>>;
type NormalizedLogHistoryInflight =
    Arc<Mutex<HashMap<(Uuid, NormalizedLogReplayMode), SharedNormalizedLogHistoryFuture>>>;

#[derive(Clone, Debug, Default)]
struct LiveNormalizedLogMessages {
    payloads: Vec<String>,
    finished: bool,
}

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

async fn get_execution_process_final_message(
    Extension(execution_process): Extension<ExecutionProcess>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<AgentResponse>>, ApiError> {
    let record = CodingAgentTurn::find_response_by_execution_process_id(
        &deployment.db().pool,
        execution_process.id,
    )
    .await?
    .ok_or(ApiError::ExecutionProcess(
        ExecutionProcessError::ExecutionProcessNotFound,
    ))?;
    Ok(ResponseJson(ApiResponse::success(
        AgentResponse::from_record(record),
    )))
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
        if let Some(store) = async { deployment.container().get_msg_store_by_id(&exec_id).await }
            .instrument(tracing::debug_span!(
                "normalized_logs.lookup_live_store",
                execution_process_id = %exec_id,
            ))
            .await
        {
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

#[tracing::instrument(level = "debug", skip(store), fields(execution_process_id = %exec_id))]
async fn build_live_normalized_logs_stream(
    exec_id: Uuid,
    store: Arc<MsgStore>,
) -> BoxStream<'static, anyhow::Result<Message>> {
    let receiver = store.get_receiver();
    let messages = collect_live_normalized_log_messages(&store);
    tracing::debug!(
        execution_process_id = %exec_id,
        history_message_count = messages.payloads.len(),
        history_finished = messages.finished,
        "normalized_logs.live_history_loaded"
    );
    let history_stream = stream::iter(
        messages
            .payloads
            .clone()
            .into_iter()
            .map(|payload| Ok::<_, anyhow::Error>(Message::Text(payload.into()))),
    );

    if messages.finished {
        return history_stream
            .chain(stream::once(async {
                Ok::<_, anyhow::Error>(LogMsg::Finished.to_ws_message_unchecked())
            }))
            .boxed();
    }

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

#[tracing::instrument(level = "debug", skip(deployment), fields(execution_process_id = %exec_id))]
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

#[tracing::instrument(
    level = "debug",
    skip(future),
    fields(execution_process_id = %exec_id, mode = ?mode)
)]
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

#[tracing::instrument(level = "debug", skip(store))]
fn collect_live_normalized_log_messages(store: &MsgStore) -> LiveNormalizedLogMessages {
    let history = store.get_history();
    let finished = history.iter().any(|msg| matches!(msg, LogMsg::Finished));
    let payloads: Vec<String> = history
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

    tracing::debug!(
        history_message_count = payloads.len(),
        history_finished = finished,
        "normalized_logs.live_history_collected"
    );

    LiveNormalizedLogMessages { payloads, finished }
}

#[tracing::instrument(level = "debug", skip(deployment), fields(execution_process_id = %exec_id))]
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

    let messages = Arc::new(messages);
    tracing::debug!(
        execution_process_id = %exec_id,
        history_message_count = messages.len(),
        "normalized_logs.historic_history_collected"
    );
    Some(messages)
}

#[tracing::instrument(level = "debug", skip(socket, stream))]
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

pub(crate) async fn confirm_external_process_stopped_for_workspace(
    Extension(workspace): Extension<db::models::workspace::Workspace>,
    State(deployment): State<DeploymentImpl>,
    Path(path): Path<HashMap<String, String>>,
    request_signature: Option<Extension<crate::middleware::RelayRequestSignatureContext>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ResponseJson(request): ResponseJson<ConfirmExternalProcessStoppedRequest>,
) -> Result<ResponseJson<ApiResponse<()>>, ApiError> {
    let process_id = path
        .get("process_id")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| ApiError::BadRequest("Invalid process identifier.".into()))?;
    let execution_process = ExecutionProcess::find_by_id(&deployment.db().pool, process_id)
        .await?
        .ok_or(ApiError::ExecutionProcess(
            ExecutionProcessError::ExecutionProcessNotFound,
        ))?;
    let session = Session::find_by_id(&deployment.db().pool, execution_process.session_id)
        .await?
        .ok_or_else(|| ApiError::BadRequest("Session not found".into()))?;
    if session.workspace_id != workspace.id {
        return Err(ApiError::Forbidden(
            "Process workspace does not match.".into(),
        ));
    }
    let actor = authorize_external_start_recovery(
        workspace.id,
        session.workspace_id,
        &request,
        request_signature
            .as_ref()
            .map(|Extension(signature)| signature),
        Some(peer),
    )?;
    confirm_external_process_stopped_with_context(
        &deployment,
        execution_process.id,
        workspace.id,
        &actor,
        &request,
    )
    .await
}

pub(crate) async fn get_external_start_status_for_workspace(
    Extension(workspace): Extension<db::models::workspace::Workspace>,
    State(deployment): State<DeploymentImpl>,
    Path(path): Path<HashMap<String, String>>,
    request_signature: Option<Extension<crate::middleware::RelayRequestSignatureContext>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<ResponseJson<ApiResponse<Option<ExternalStartStatus>>>, ApiError> {
    verified_recovery_actor(
        request_signature
            .as_ref()
            .map(|Extension(signature)| signature),
        Some(peer),
    )?;
    let process_id = path
        .get("process_id")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| ApiError::BadRequest("Invalid process identifier.".into()))?;
    let execution_process = ExecutionProcess::find_by_id(&deployment.db().pool, process_id)
        .await?
        .ok_or(ApiError::ExecutionProcess(
            ExecutionProcessError::ExecutionProcessNotFound,
        ))?;
    let session = Session::find_by_id(&deployment.db().pool, execution_process.session_id)
        .await?
        .ok_or_else(|| ApiError::BadRequest("Session not found".into()))?;
    if session.workspace_id != workspace.id {
        return Err(ApiError::Forbidden(
            "Process workspace does not match.".into(),
        ));
    }
    get_external_start_status_by_process(&deployment, process_id).await
}

async fn confirm_external_process_stopped_with_context(
    deployment: &DeploymentImpl,
    process_id: Uuid,
    workspace_id: Uuid,
    actor: &str,
    request: &ConfirmExternalProcessStoppedRequest,
) -> Result<ResponseJson<ApiResponse<()>>, ApiError> {
    use db::models::execution_external_start::ExternalStartRecoveryResult;
    let recovery = deployment
        .container()
        .confirm_external_process_stopped(
            workspace_id,
            process_id,
            actor,
            &request.recovery_token,
            request.recovery_generation,
        )
        .await;
    let recovery = match recovery {
        Ok(result) => result,
        Err(services::services::container::ContainerError::ExternalProcessUnresolved) => {
            return Err(ApiError::BadRequest(
                "The previous agent process could not be proven stopped. Capacity remains reserved."
                    .into(),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    match recovery {
        ExternalStartRecoveryResult::Reconciled
        | ExternalStartRecoveryResult::AlreadyReconciled => {
            Ok(ResponseJson(ApiResponse::success(())))
        }
        ExternalStartRecoveryResult::NotBlocked => Err(ApiError::BadRequest(
            "This process is not awaiting operator confirmation.".into(),
        )),
        ExternalStartRecoveryResult::WrongWorkspace => Err(ApiError::Forbidden(
            "Process workspace does not match.".into(),
        )),
        ExternalStartRecoveryResult::StaleRecovery => Err(ApiError::Forbidden(
            "Recovery authorization is stale or invalid.".into(),
        )),
    }
}

async fn get_external_start_status_by_process(
    deployment: &DeploymentImpl,
    process_id: Uuid,
) -> Result<ResponseJson<ApiResponse<Option<ExternalStartStatus>>>, ApiError> {
    let record = db::models::execution_external_start::ExecutionExternalStart::record(
        &deployment.db().pool,
        process_id,
    )
    .await?;
    let status = record.map(|record| {
        let blocked = record.state == "blocked";
        let recoverable =
            blocked && record.recovery_token.is_some() && record.recovery_generation > 0;
        ExternalStartStatus {
            state: if blocked {
                "needs_confirmation"
            } else {
                "in_progress"
            }
            .into(),
            message: if blocked {
                "Confirm the previous agent process has stopped before continuing."
            } else {
                "Agent startup is being tracked."
            }
            .into(),
            can_confirm_stopped: recoverable,
            recovery_token: recoverable.then(|| record.recovery_token).flatten(),
            recovery_generation: recoverable.then_some(record.recovery_generation),
        }
    });
    Ok(ResponseJson(ApiResponse::success(status)))
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

#[tracing::instrument(
    level = "debug",
    skip(socket, deployment),
    fields(session_id = %session_id, show_soft_deleted = show_soft_deleted)
)]
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
        .route("/final-message", get(get_execution_process_final_message))
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
    use std::{
        net::SocketAddr,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use axum::{
        Json, Router,
        body::Body,
        extract::{Path as AxumPath, State as AxumState},
        http::StatusCode,
        routing::post,
    };
    use futures_util::{FutureExt, StreamExt};
    use http::Request;
    use serde_json::json;
    use tokio::{
        sync::{Mutex, oneshot},
        time::timeout,
    };
    use tower::ServiceExt;
    use utils::{log_msg::LogMsg, msg_store::MsgStore};
    use uuid::Uuid;

    use super::{
        AgentResponse, ConfirmExternalProcessStoppedRequest, NormalizedLogReplayMode,
        authorize_external_start_recovery, build_live_normalized_logs_stream,
        get_normalized_log_messages_single_flight,
    };

    #[test]
    fn operator_recovery_authorization_requires_capability_and_workspace_scope() {
        let workspace_id = Uuid::new_v4();
        let loopback = "127.0.0.1:4242".parse().unwrap();
        let valid = ConfirmExternalProcessStoppedRequest {
            recovery_token: "opaque-capability".into(),
            recovery_generation: 3,
        };
        assert_eq!(
            authorize_external_start_recovery(
                workspace_id,
                workspace_id,
                &valid,
                None,
                Some(loopback)
            )
            .unwrap(),
            "local_ui"
        );
        let missing = ConfirmExternalProcessStoppedRequest {
            recovery_token: "".into(),
            ..valid.clone()
        };
        assert!(matches!(
            authorize_external_start_recovery(
                workspace_id,
                workspace_id,
                &missing,
                None,
                Some(loopback)
            ),
            Err(crate::error::ApiError::Unauthorized)
        ));
        let relay_id = Uuid::new_v4();
        let relay = crate::middleware::RelayRequestSignatureContext {
            signing_session_id: relay_id,
            timestamp: 1,
            nonce: Uuid::new_v4(),
            signature_b64: "verified-by-middleware".into(),
        };
        assert_eq!(
            authorize_external_start_recovery(
                workspace_id,
                workspace_id,
                &valid,
                Some(&relay),
                Some("203.0.113.2:4242".parse().unwrap()),
            )
            .unwrap(),
            format!("relay:{relay_id}")
        );
        assert!(matches!(
            authorize_external_start_recovery(
                Uuid::new_v4(),
                workspace_id,
                &valid,
                None,
                Some(loopback)
            ),
            Err(crate::error::ApiError::Forbidden(_))
        ));
        assert!(matches!(
            authorize_external_start_recovery(
                workspace_id,
                workspace_id,
                &valid,
                None,
                Some("203.0.113.2:4242".parse().unwrap())
            ),
            Err(crate::error::ApiError::Unauthorized)
        ));
    }

    #[tokio::test]
    async fn operator_recovery_http_endpoint_rejects_missing_invalid_and_cross_workspace_authorization()
     {
        async fn endpoint(
            AxumState((process_workspace, peer)): AxumState<(Uuid, SocketAddr)>,
            AxumPath(workspace): AxumPath<Uuid>,
            Json(request): Json<ConfirmExternalProcessStoppedRequest>,
        ) -> Result<String, crate::error::ApiError> {
            authorize_external_start_recovery(
                workspace,
                process_workspace,
                &request,
                None,
                Some(peer),
            )
        }

        let process_workspace = Uuid::new_v4();
        let app = Router::new()
            .route("/workspaces/{workspace}", post(endpoint))
            .with_state((process_workspace, "127.0.0.1:4242".parse().unwrap()));
        let call = |workspace: Uuid, body: &'static str| {
            app.clone().oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/workspaces/{workspace}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
        };

        assert_eq!(
            call(process_workspace, "{}").await.unwrap().status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            call(
                process_workspace,
                r#"{"recoveryToken":"","recoveryGeneration":1}"#,
            )
            .await
            .unwrap()
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(
                Uuid::new_v4(),
                r#"{"recoveryToken":"opaque","recoveryGeneration":1}"#,
            )
            .await
            .unwrap()
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                process_workspace,
                r#"{"recoveryToken":"opaque","recoveryGeneration":1}"#,
            )
            .await
            .unwrap()
            .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn live_normalized_stream_finishes_when_history_already_finished() {
        let store = Arc::new(MsgStore::new());
        let patch = serde_json::from_value(json!([
            {
                "op": "add",
                "path": "/entries/0",
                "value": "already normalized"
            }
        ]))
        .expect("valid json patch");
        store.push(LogMsg::JsonPatch(patch));
        store.push(LogMsg::Finished);

        let mut stream = build_live_normalized_logs_stream(Uuid::new_v4(), store).await;

        let first = timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("history patch should not hang")
            .expect("history patch should be present")
            .expect("history patch should be ok");
        assert!(
            first.into_text().expect("text message").contains("entries"),
            "expected replayed normalized patch"
        );

        let second = timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("finished message should not hang")
            .expect("finished message should be present")
            .expect("finished message should be ok");
        assert_eq!(
            second.into_text().expect("text message"),
            "{\"finished\":true}"
        );

        let end = timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("stream end should not hang");
        assert!(end.is_none());
    }

    #[test]
    fn agent_response_reports_summary_truncation_metadata() {
        use db::models::{
            coding_agent_turn::{
                CODING_AGENT_PROMPT_PREVIEW_MAX_CHARS, CODING_AGENT_RESPONSE_SUMMARY_MAX_CHARS,
                CodingAgentResponseRecord,
            },
            execution_process::ExecutionProcessStatus,
        };

        let content = format!("{}...", "x".repeat(CODING_AGENT_RESPONSE_SUMMARY_MAX_CHARS));
        let response = AgentResponse::from_record(CodingAgentResponseRecord {
            execution_process_id: uuid::Uuid::new_v4(),
            session_id: uuid::Uuid::new_v4(),
            workspace_id: uuid::Uuid::new_v4(),
            status: ExecutionProcessStatus::Completed,
            completed_at: None,
            coding_agent_turn_id: Some(uuid::Uuid::new_v4()),
            agent_session_id: Some("agent-session".to_string()),
            agent_message_id: Some("agent-message".to_string()),
            summary: Some(content.clone()),
            prompt: Some("initial prompt".to_string()),
        });

        assert_eq!(response.content.as_deref(), Some(content.as_str()));
        assert!(response.truncated);
        assert_eq!(response.max_chars, CODING_AGENT_RESPONSE_SUMMARY_MAX_CHARS);
        assert_eq!(response.prompt_preview.as_deref(), Some("initial prompt"));
        assert!(!response.prompt_truncated);
        assert_eq!(
            response.prompt_max_chars,
            CODING_AGENT_PROMPT_PREVIEW_MAX_CHARS
        );
    }

    #[test]
    fn agent_response_bounds_prompt_preview_without_exposing_full_prompt() {
        use db::models::{
            coding_agent_turn::{CODING_AGENT_PROMPT_PREVIEW_MAX_CHARS, CodingAgentResponseRecord},
            execution_process::ExecutionProcessStatus,
        };

        let prompt = format!(
            "{}SECRET_AFTER_BOUNDARY",
            "p".repeat(CODING_AGENT_PROMPT_PREVIEW_MAX_CHARS)
        );
        let response = AgentResponse::from_record(CodingAgentResponseRecord {
            execution_process_id: uuid::Uuid::new_v4(),
            session_id: uuid::Uuid::new_v4(),
            workspace_id: uuid::Uuid::new_v4(),
            status: ExecutionProcessStatus::Completed,
            completed_at: None,
            coding_agent_turn_id: Some(uuid::Uuid::new_v4()),
            agent_session_id: Some("agent-session".to_string()),
            agent_message_id: Some("agent-message".to_string()),
            summary: Some("done".to_string()),
            prompt: Some(prompt),
        });

        let preview = response.prompt_preview.as_deref().expect("prompt preview");
        assert!(response.prompt_truncated);
        assert_eq!(
            response.prompt_max_chars,
            CODING_AGENT_PROMPT_PREVIEW_MAX_CHARS
        );
        assert!(preview.ends_with("..."));
        assert!(!preview.contains("SECRET_AFTER_BOUNDARY"));

        let serialized = serde_json::to_value(&response).expect("serializes response");
        assert!(serialized.get("prompt").is_none());
        assert_eq!(serialized["prompt_preview"], preview);
        assert_eq!(serialized["prompt_truncated"], true);
    }

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
}
