use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, OnceLock},
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    response::Json as ResponseJson,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use db::models::session::Session;
use deployment::Deployment;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use tokio::sync::Mutex;
use ts_rs::TS;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError};

const DEFAULT_PREVIEW_MESSAGE_LIMIT: usize = 3;
const MAX_PREVIEW_MESSAGE_LIMIT: usize = 50;
const PREVIEW_CACHE_SESSION_CAPACITY: usize = 25;
const RECENT_TURN_SCAN_LIMIT: i64 = 32;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ConversationPreviewMessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ConversationPreviewMessage {
    pub role: ConversationPreviewMessageRole,
    pub content: String,
    pub execution_process_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ConversationPreview {
    pub workspace_id: Uuid,
    pub session_id: Uuid,
    pub messages: Vec<ConversationPreviewMessage>,
    pub has_running_turn: bool,
    pub source: ConversationPreviewSource,
    pub warmed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ConversationPreviewSource {
    Cache,
    Computed,
}

#[derive(Debug, Clone, Deserialize, TS)]
pub struct WarmWorkspaceSessionsRequest {
    pub workspace_id: Uuid,
    #[serde(default)]
    pub session_ids: Vec<Uuid>,
    #[serde(default = "default_true")]
    pub include_latest_session: bool,
}

#[derive(Debug, Clone, Deserialize, TS)]
pub struct WarmConversationPreviewRequest {
    #[serde(default)]
    pub workspace_ids: Vec<Uuid>,
    #[serde(default)]
    pub session_ids: Vec<Uuid>,
    #[serde(default)]
    pub workspace_sessions: Vec<WarmWorkspaceSessionsRequest>,
    pub message_limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, TS)]
pub struct WarmConversationPreviewItem {
    pub workspace_id: Uuid,
    pub session_id: Uuid,
    pub message_count: usize,
    pub source: ConversationPreviewSource,
}

#[derive(Debug, Clone, Serialize, TS)]
pub struct WarmConversationPreviewError {
    pub workspace_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, TS)]
pub struct WarmConversationPreviewResponse {
    pub warmed: Vec<WarmConversationPreviewItem>,
    pub errors: Vec<WarmConversationPreviewError>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConversationPreviewQuery {
    pub limit: Option<usize>,
}

#[derive(Clone)]
struct CachedConversationPreview {
    preview: ConversationPreview,
    max_messages: usize,
}

#[derive(Default)]
struct ConversationPreviewCache {
    by_session: HashMap<Uuid, CachedConversationPreview>,
    recency: VecDeque<Uuid>,
}

type SharedConversationPreviewCache = Arc<Mutex<ConversationPreviewCache>>;

fn conversation_preview_cache() -> &'static SharedConversationPreviewCache {
    static CACHE: OnceLock<SharedConversationPreviewCache> = OnceLock::new();
    CACHE.get_or_init(|| Arc::new(Mutex::new(ConversationPreviewCache::default())))
}

fn default_true() -> bool {
    true
}

fn normalize_limit(limit: Option<usize>) -> usize {
    limit
        .unwrap_or(DEFAULT_PREVIEW_MESSAGE_LIMIT)
        .clamp(1, MAX_PREVIEW_MESSAGE_LIMIT)
}

impl ConversationPreviewCache {
    fn get_cached(
        &self,
        session_id: Uuid,
        limit: usize,
    ) -> Option<CachedConversationPreview> {
        let cached = self.by_session.get(&session_id)?;
        if cached.max_messages < limit {
            return None;
        }

        Some(cached.clone())
    }

    fn touch_cached_preview(
        &mut self,
        session_id: Uuid,
        cached: CachedConversationPreview,
        limit: usize,
    ) -> ConversationPreview {
        let mut preview = cached.preview;
        preview.source = ConversationPreviewSource::Cache;
        preview.messages = take_latest_messages(&preview.messages, limit);
        self.touch(session_id);
        preview
    }

    fn insert(&mut self, session_id: Uuid, preview: ConversationPreview, max_messages: usize) {
        self.by_session.insert(
            session_id,
            CachedConversationPreview {
                preview,
                max_messages,
            },
        );
        self.touch(session_id);
        self.evict_if_needed();
    }

    fn touch(&mut self, session_id: Uuid) {
        self.recency.retain(|id| *id != session_id);
        self.recency.push_back(session_id);
    }

    fn evict_if_needed(&mut self) {
        while self.by_session.len() > PREVIEW_CACHE_SESSION_CAPACITY {
            let Some(oldest) = self.recency.pop_front() else {
                break;
            };
            self.by_session.remove(&oldest);
        }
    }
}

fn take_latest_messages(
    messages: &[ConversationPreviewMessage],
    limit: usize,
) -> Vec<ConversationPreviewMessage> {
    let start = messages.len().saturating_sub(limit);
    messages[start..].to_vec()
}

#[derive(Debug, FromRow)]
struct ConversationTurnRow {
    workspace_id: Uuid,
    execution_process_id: Uuid,
    process_created_at: DateTime<Utc>,
    process_completed_at: Option<DateTime<Utc>>,
    process_status: String,
    prompt: Option<String>,
    summary: Option<String>,
    turn_updated_at: DateTime<Utc>,
}

async fn compute_preview_for_session(
    pool: &SqlitePool,
    session_id: Uuid,
    limit: usize,
) -> Result<ConversationPreview, ApiError> {
    let rows = sqlx::query_as::<_, ConversationTurnRow>(
        r#"
        SELECT *
        FROM (
            SELECT
                s.workspace_id AS workspace_id,
                ep.id AS execution_process_id,
                ep.created_at AS process_created_at,
                ep.completed_at AS process_completed_at,
                ep.status AS process_status,
                cat.prompt AS prompt,
                cat.summary AS summary,
                cat.updated_at AS turn_updated_at
            FROM coding_agent_turns cat
            JOIN execution_processes ep ON ep.id = cat.execution_process_id
            JOIN sessions s ON s.id = ep.session_id
            WHERE ep.session_id = ?
              AND ep.dropped = FALSE
            ORDER BY ep.created_at DESC
            LIMIT ?
        )
        ORDER BY process_created_at ASC
        "#,
    )
    .bind(session_id)
    .bind(RECENT_TURN_SCAN_LIMIT)
    .fetch_all(pool)
    .await?;

    let workspace_id = rows
        .first()
        .map(|row| row.workspace_id)
        .ok_or_else(|| ApiError::BadRequest("No conversation turns found".to_string()))?;

    let has_running_turn = rows.iter().any(|row| row.process_status == "running");
    let mut messages = Vec::new();

    for row in rows {
        if let Some(prompt) = row.prompt.as_deref().map(str::trim)
            && !prompt.is_empty()
        {
            messages.push(ConversationPreviewMessage {
                role: ConversationPreviewMessageRole::User,
                content: prompt.to_string(),
                execution_process_id: row.execution_process_id,
                created_at: row.process_created_at,
            });
        }

        if let Some(summary) = row.summary.as_deref().map(str::trim)
            && !summary.is_empty()
        {
            messages.push(ConversationPreviewMessage {
                role: ConversationPreviewMessageRole::Assistant,
                content: summary.to_string(),
                execution_process_id: row.execution_process_id,
                created_at: row.process_completed_at.unwrap_or(row.turn_updated_at),
            });
        }
    }

    Ok(ConversationPreview {
        workspace_id,
        session_id,
        messages: take_latest_messages(&messages, limit),
        has_running_turn,
        source: ConversationPreviewSource::Computed,
        warmed_at: Utc::now(),
    })
}

async fn latest_preview_activity_at(
    pool: &SqlitePool,
    session_id: Uuid,
) -> Result<Option<DateTime<Utc>>, ApiError> {
    let latest = sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
        r#"
        SELECT MAX(cat.updated_at)
        FROM coding_agent_turns cat
        JOIN execution_processes ep ON ep.id = cat.execution_process_id
        WHERE ep.session_id = ?
          AND ep.dropped = FALSE
        "#,
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?;

    Ok(latest.flatten())
}

async fn get_or_compute_preview(
    pool: &SqlitePool,
    session_id: Uuid,
    limit: usize,
) -> Result<ConversationPreview, ApiError> {
    let cached = conversation_preview_cache()
        .lock()
        .await
        .get_cached(session_id, limit);

    if let Some(cached) = cached {
        let latest_activity_at = latest_preview_activity_at(pool, session_id).await?;
        let is_still_warm = latest_activity_at.is_none_or(|latest_activity_at| {
            latest_activity_at <= cached.preview.warmed_at
        });

        if is_still_warm {
            return Ok(conversation_preview_cache()
                .lock()
                .await
                .touch_cached_preview(session_id, cached, limit));
        }
    }

    let preview = compute_preview_for_session(pool, session_id, limit).await?;
    conversation_preview_cache()
        .lock()
        .await
        .insert(session_id, preview.clone(), limit);

    Ok(preview)
}

async fn resolve_warm_session_ids(
    pool: &SqlitePool,
    request: &WarmConversationPreviewRequest,
) -> (Vec<Uuid>, Vec<WarmConversationPreviewError>) {
    let mut session_ids = request.session_ids.clone();
    let mut errors = Vec::new();

    for workspace_id in &request.workspace_ids {
        match Session::find_latest_by_workspace_id(pool, *workspace_id).await {
            Ok(Some(session)) => session_ids.push(session.id),
            Ok(None) => errors.push(WarmConversationPreviewError {
                workspace_id: Some(*workspace_id),
                session_id: None,
                message: "Workspace has no sessions to warm".to_string(),
            }),
            Err(error) => errors.push(WarmConversationPreviewError {
                workspace_id: Some(*workspace_id),
                session_id: None,
                message: format!("Failed to resolve latest workspace session: {error}"),
            }),
        }
    }

    for workspace_sessions in &request.workspace_sessions {
        session_ids.extend(workspace_sessions.session_ids.iter().copied());

        if workspace_sessions.include_latest_session {
            match Session::find_latest_by_workspace_id(pool, workspace_sessions.workspace_id).await {
                Ok(Some(session)) => session_ids.push(session.id),
                Ok(None) => errors.push(WarmConversationPreviewError {
                    workspace_id: Some(workspace_sessions.workspace_id),
                    session_id: None,
                    message: "Workspace has no latest session to warm".to_string(),
                }),
                Err(error) => errors.push(WarmConversationPreviewError {
                    workspace_id: Some(workspace_sessions.workspace_id),
                    session_id: None,
                    message: format!("Failed to resolve latest workspace session: {error}"),
                }),
            }
        }
    }

    let mut seen = HashSet::new();
    session_ids.retain(|session_id| seen.insert(*session_id));

    (session_ids, errors)
}

pub async fn warm_conversation_previews(
    State(deployment): State<DeploymentImpl>,
    Json(request): Json<WarmConversationPreviewRequest>,
) -> Result<ResponseJson<ApiResponse<WarmConversationPreviewResponse>>, ApiError> {
    let pool = &deployment.db().pool;
    let limit = normalize_limit(request.message_limit);
    let (session_ids, mut errors) = resolve_warm_session_ids(pool, &request).await;
    let mut warmed = Vec::new();

    for session_id in session_ids {
        match compute_preview_for_session(pool, session_id, limit).await {
            Ok(preview) => {
                conversation_preview_cache()
                    .lock()
                    .await
                    .insert(session_id, preview.clone(), limit);
                warmed.push(WarmConversationPreviewItem {
                    workspace_id: preview.workspace_id,
                    session_id,
                    message_count: preview.messages.len(),
                    source: preview.source,
                });
            }
            Err(error) => errors.push(WarmConversationPreviewError {
                workspace_id: None,
                session_id: Some(session_id),
                message: error.to_string(),
            }),
        }
    }

    Ok(ResponseJson(ApiResponse::success(
        WarmConversationPreviewResponse { warmed, errors },
    )))
}

pub async fn get_session_conversation_preview(
    State(deployment): State<DeploymentImpl>,
    Path(session_id): Path<Uuid>,
    Query(query): Query<ConversationPreviewQuery>,
) -> Result<ResponseJson<ApiResponse<ConversationPreview>>, ApiError> {
    let preview =
        get_or_compute_preview(&deployment.db().pool, session_id, normalize_limit(query.limit))
            .await?;
    Ok(ResponseJson(ApiResponse::success(preview)))
}

pub async fn get_workspace_conversation_preview(
    State(deployment): State<DeploymentImpl>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<ConversationPreviewQuery>,
) -> Result<ResponseJson<ApiResponse<ConversationPreview>>, ApiError> {
    let pool = &deployment.db().pool;
    let session = Session::find_latest_by_workspace_id(pool, workspace_id)
        .await?
        .ok_or_else(|| ApiError::BadRequest("Workspace has no sessions".to_string()))?;
    let preview = get_or_compute_preview(pool, session.id, normalize_limit(query.limit)).await?;
    Ok(ResponseJson(ApiResponse::success(preview)))
}

pub fn router() -> Router<DeploymentImpl> {
    Router::new()
        .route("/conversation-preview/warm", post(warm_conversation_previews))
        .route(
            "/sessions/{session_id}/conversation-preview",
            get(get_session_conversation_preview),
        )
        .route(
            "/workspaces/{workspace_id}/conversation-preview",
            get(get_workspace_conversation_preview),
        )
}

#[cfg(test)]
mod tests {
    use super::{
        ConversationPreviewMessage, ConversationPreviewMessageRole, take_latest_messages,
    };
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn take_latest_messages_preserves_chronological_order() {
        let process_id = Uuid::new_v4();
        let messages: Vec<_> = (0..5)
            .map(|idx| ConversationPreviewMessage {
                role: if idx % 2 == 0 {
                    ConversationPreviewMessageRole::User
                } else {
                    ConversationPreviewMessageRole::Assistant
                },
                content: format!("message {idx}"),
                execution_process_id: process_id,
                created_at: Utc::now(),
            })
            .collect();

        let latest = take_latest_messages(&messages, 3);

        assert_eq!(
            latest
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>(),
            vec!["message 2", "message 3", "message 4"]
        );
    }
}
