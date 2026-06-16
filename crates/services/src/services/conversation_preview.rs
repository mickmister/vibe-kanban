use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, OnceLock},
};

use chrono::{DateTime, Utc};
use db::models::{execution_process::ExecutionProcess, session::Session};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use thiserror::Error;
use tokio::sync::Mutex;
use ts_rs::TS;
use uuid::Uuid;

const DEFAULT_PREVIEW_MESSAGE_LIMIT: usize = 3;
const MAX_PREVIEW_MESSAGE_LIMIT: usize = 50;
const PREVIEW_CACHE_SESSION_CAPACITY: usize = 25;
const RECENT_TURN_SCAN_LIMIT: i64 = 32;

#[derive(Debug, Error)]
pub enum ConversationPreviewError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error("Session not found")]
    SessionNotFound,
}

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
    pub session_id: Option<Uuid>,
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
    pub session_id: Option<Uuid>,
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

#[derive(Clone)]
struct CachedConversationPreview {
    preview: ConversationPreview,
    max_messages: usize,
    message_count: usize,
    latest_activity_at: Option<DateTime<Utc>>,
}

#[derive(Default)]
struct ConversationPreviewCache {
    by_session: HashMap<Uuid, CachedConversationPreview>,
    recency: VecDeque<Uuid>,
}

struct ComputedConversationPreview {
    preview: ConversationPreview,
    message_count: usize,
    latest_activity_at: Option<DateTime<Utc>>,
}

type SharedConversationPreviewCache = Arc<Mutex<ConversationPreviewCache>>;

fn conversation_preview_cache() -> &'static SharedConversationPreviewCache {
    static CACHE: OnceLock<SharedConversationPreviewCache> = OnceLock::new();
    CACHE.get_or_init(|| Arc::new(Mutex::new(ConversationPreviewCache::default())))
}

fn default_true() -> bool {
    true
}

pub fn normalize_limit(limit: Option<usize>) -> usize {
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

    fn insert(
        &mut self,
        session_id: Uuid,
        computed: ComputedConversationPreview,
        max_messages: usize,
    ) {
        self.by_session.insert(
            session_id,
            CachedConversationPreview {
                preview: computed.preview,
                max_messages,
                message_count: computed.message_count,
                latest_activity_at: computed.latest_activity_at,
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

pub fn take_latest_messages(
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
}

async fn empty_preview_for_session(
    pool: &SqlitePool,
    session_id: Uuid,
) -> Result<ComputedConversationPreview, ConversationPreviewError> {
    let session = Session::find_by_id(pool, session_id)
        .await?
        .ok_or(ConversationPreviewError::SessionNotFound)?;

    Ok(ComputedConversationPreview {
        preview: ConversationPreview {
            workspace_id: session.workspace_id,
            session_id: Some(session.id),
            messages: Vec::new(),
            has_running_turn: false,
            source: ConversationPreviewSource::Computed,
            warmed_at: Utc::now(),
        },
        message_count: 0,
        latest_activity_at: None,
    })
}

async fn compute_preview_for_session(
    pool: &SqlitePool,
    session_id: Uuid,
    limit: usize,
) -> Result<ComputedConversationPreview, ConversationPreviewError> {
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
                cat.summary AS summary
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

    let Some(workspace_id) = rows.first().map(|row| row.workspace_id) else {
        return empty_preview_for_session(pool, session_id).await;
    };

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
                created_at: row.process_completed_at.unwrap_or(row.process_created_at),
            });
        }
    }

    let message_count = messages.len();
    let latest_activity_at = messages.iter().map(|message| message.created_at).max();

    Ok(ComputedConversationPreview {
        preview: ConversationPreview {
            workspace_id,
            session_id: Some(session_id),
            messages: take_latest_messages(&messages, limit),
            has_running_turn,
            source: ConversationPreviewSource::Computed,
            warmed_at: Utc::now(),
        },
        message_count,
        latest_activity_at,
    })
}

async fn latest_preview_state(
    pool: &SqlitePool,
    session_id: Uuid,
) -> Result<(usize, Option<DateTime<Utc>>), ConversationPreviewError> {
    #[derive(FromRow)]
    struct PreviewStateRow {
        message_count: i64,
        latest_activity_at: Option<DateTime<Utc>>,
    }

    let state = sqlx::query_as::<_, PreviewStateRow>(
        r#"
        SELECT
            COUNT(message_at) AS message_count,
            MAX(message_at) AS latest_activity_at
        FROM (
            SELECT ep.created_at AS message_at
            FROM coding_agent_turns cat
            JOIN execution_processes ep ON ep.id = cat.execution_process_id
            WHERE ep.session_id = ?
              AND ep.dropped = FALSE
              AND cat.prompt IS NOT NULL
              AND trim(cat.prompt) != ''

            UNION ALL

            SELECT COALESCE(ep.completed_at, ep.created_at) AS message_at
            FROM coding_agent_turns cat
            JOIN execution_processes ep ON ep.id = cat.execution_process_id
            WHERE ep.session_id = ?
              AND ep.dropped = FALSE
              AND cat.summary IS NOT NULL
              AND trim(cat.summary) != ''
            ORDER BY message_at DESC
            LIMIT ?
        )
        "#,
    )
    .bind(session_id)
    .bind(session_id)
    .bind(RECENT_TURN_SCAN_LIMIT * 2)
    .fetch_one(pool)
    .await?;

    Ok((state.message_count as usize, state.latest_activity_at))
}

pub async fn get_or_compute_preview(
    pool: &SqlitePool,
    session_id: Uuid,
    limit: usize,
) -> Result<ConversationPreview, ConversationPreviewError> {
    let cached = conversation_preview_cache()
        .lock()
        .await
        .get_cached(session_id, limit);

    if let Some(cached) = cached {
        let (message_count, latest_activity_at) = latest_preview_state(pool, session_id).await?;
        let has_same_message_count = message_count == cached.message_count;
        let has_no_newer_activity = latest_activity_at <= cached.latest_activity_at;
        let is_still_warm = has_same_message_count && has_no_newer_activity;

        if is_still_warm {
            return Ok(conversation_preview_cache()
                .lock()
                .await
                .touch_cached_preview(session_id, cached, limit));
        }
    }

    let computed = compute_preview_for_session(pool, session_id, limit).await?;
    let preview = computed.preview.clone();
    conversation_preview_cache()
        .lock()
        .await
        .insert(session_id, computed, limit);

    Ok(preview)
}

pub async fn get_session_conversation_preview(
    pool: &SqlitePool,
    session_id: Uuid,
    limit: Option<usize>,
) -> Result<ConversationPreview, ConversationPreviewError> {
    get_or_compute_preview(pool, session_id, normalize_limit(limit)).await
}

pub async fn get_workspace_conversation_preview(
    pool: &SqlitePool,
    workspace_id: Uuid,
    limit: Option<usize>,
) -> Result<ConversationPreview, ConversationPreviewError> {
    let Some(session) = Session::find_latest_by_workspace_id(pool, workspace_id).await? else {
        return Ok(ConversationPreview {
            workspace_id,
            session_id: None,
            messages: Vec::new(),
            has_running_turn: false,
            source: ConversationPreviewSource::Computed,
            warmed_at: Utc::now(),
        });
    };

    get_or_compute_preview(pool, session.id, normalize_limit(limit)).await
}

async fn push_validated_workspace_session_id(
    pool: &SqlitePool,
    session_ids: &mut Vec<Uuid>,
    errors: &mut Vec<WarmConversationPreviewError>,
    workspace_id: Uuid,
    session_id: Uuid,
) {
    match Session::find_by_id(pool, session_id).await {
        Ok(Some(session)) if session.workspace_id == workspace_id => session_ids.push(session_id),
        Ok(Some(session)) => errors.push(WarmConversationPreviewError {
            workspace_id: Some(workspace_id),
            session_id: Some(session_id),
            message: format!(
                "Session belongs to workspace {}, not workspace {}",
                session.workspace_id, workspace_id
            ),
        }),
        Ok(None) => errors.push(WarmConversationPreviewError {
            workspace_id: Some(workspace_id),
            session_id: Some(session_id),
            message: "Session not found".to_string(),
        }),
        Err(error) => errors.push(WarmConversationPreviewError {
            workspace_id: Some(workspace_id),
            session_id: Some(session_id),
            message: format!("Failed to validate workspace session: {error}"),
        }),
    }
}

async fn resolve_warm_session_ids(
    pool: &SqlitePool,
    request: &WarmConversationPreviewRequest,
) -> (
    Vec<Uuid>,
    Vec<Uuid>,
    Vec<WarmConversationPreviewError>,
) {
    let mut session_ids = request.session_ids.clone();
    let mut empty_workspace_ids = Vec::new();
    let mut errors = Vec::new();

    for workspace_id in &request.workspace_ids {
        match Session::find_latest_by_workspace_id(pool, *workspace_id).await {
            Ok(Some(session)) => session_ids.push(session.id),
            Ok(None) => empty_workspace_ids.push(*workspace_id),
            Err(error) => errors.push(WarmConversationPreviewError {
                workspace_id: Some(*workspace_id),
                session_id: None,
                message: format!("Failed to resolve latest workspace session: {error}"),
            }),
        }
    }

    for workspace_sessions in &request.workspace_sessions {
        for session_id in &workspace_sessions.session_ids {
            push_validated_workspace_session_id(
                pool,
                &mut session_ids,
                &mut errors,
                workspace_sessions.workspace_id,
                *session_id,
            )
            .await;
        }

        if workspace_sessions.include_latest_session {
            let latest_session =
                Session::find_latest_by_workspace_id(pool, workspace_sessions.workspace_id).await;
            match latest_session {
                Ok(Some(session)) => session_ids.push(session.id),
                Ok(None) => empty_workspace_ids.push(workspace_sessions.workspace_id),
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

    let mut seen_empty_workspaces = HashSet::new();
    empty_workspace_ids.retain(|workspace_id| seen_empty_workspaces.insert(*workspace_id));

    (session_ids, empty_workspace_ids, errors)
}

pub async fn warm_conversation_previews(
    pool: &SqlitePool,
    request: WarmConversationPreviewRequest,
) -> WarmConversationPreviewResponse {
    let limit = normalize_limit(request.message_limit);
    let (session_ids, empty_workspace_ids, mut errors) =
        resolve_warm_session_ids(pool, &request).await;
    let mut warmed = Vec::new();

    for workspace_id in empty_workspace_ids {
        warmed.push(WarmConversationPreviewItem {
            workspace_id,
            session_id: None,
            message_count: 0,
            source: ConversationPreviewSource::Computed,
        });
    }

    for session_id in session_ids {
        match compute_preview_for_session(pool, session_id, limit).await {
            Ok(computed) => {
                let preview = computed.preview.clone();
                conversation_preview_cache()
                    .lock()
                    .await
                    .insert(session_id, computed, limit);
                warmed.push(WarmConversationPreviewItem {
                    workspace_id: preview.workspace_id,
                    session_id: preview.session_id,
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

    WarmConversationPreviewResponse { warmed, errors }
}

pub async fn refresh_session_preview(
    pool: &SqlitePool,
    session_id: Uuid,
) -> Result<(), ConversationPreviewError> {
    let computed =
        compute_preview_for_session(pool, session_id, DEFAULT_PREVIEW_MESSAGE_LIMIT).await?;
    conversation_preview_cache()
        .lock()
        .await
        .insert(session_id, computed, DEFAULT_PREVIEW_MESSAGE_LIMIT);

    Ok(())
}

pub async fn refresh_execution_process_preview(
    pool: &SqlitePool,
    execution_process_id: Uuid,
) -> Result<(), ConversationPreviewError> {
    let Some(execution_process) = ExecutionProcess::find_by_id(pool, execution_process_id).await?
    else {
        return Ok(());
    };

    refresh_session_preview(pool, execution_process.session_id).await
}

#[cfg(test)]
mod tests {
    use super::{
        ConversationPreviewMessage, ConversationPreviewMessageRole, normalize_limit,
        take_latest_messages,
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

    #[test]
    fn normalize_limit_clamps_to_supported_range() {
        assert_eq!(normalize_limit(None), 3);
        assert_eq!(normalize_limit(Some(0)), 1);
        assert_eq!(normalize_limit(Some(100)), 50);
    }
}
