use std::{net::IpAddr, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::RwLock;
use ts_rs::TS;
use url::{Host, Url};
use uuid::Uuid;

use crate::services::config::{Config, WebhookSubscription};

pub const WEBHOOK_HEADER_TIMESTAMP: &str = "X-VK-Webhook-Timestamp";
pub const WEBHOOK_HEADER_ALGORITHM: &str = "X-VK-Webhook-Algorithm";
pub const WEBHOOK_HEADER_SIGNATURE: &str = "X-VK-Webhook-Signature";
pub const WEBHOOK_ALGORITHM_HMAC_SHA256: &str = "hmac-sha256";

const WEBHOOK_DELIVERY_ATTEMPTS: usize = 2;
const WEBHOOK_RETRY_BACKOFF: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[ts(export)]
pub struct WebhookSubscriptionPublic {
    pub id: Uuid,
    pub name: String,
    pub upsert_key: Option<String>,
    pub url: String,
    pub enabled: bool,
    pub event_filters: Vec<String>,
    pub signing_secret_set: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateWebhookSubscription {
    #[serde(default)]
    pub id: Option<Uuid>,
    pub name: String,
    #[serde(default)]
    pub upsert_key: Option<String>,
    pub url: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub event_filters: Vec<String>,
    pub signing_secret: String,
    #[serde(default)]
    pub allow_external_url: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateWebhookSubscription {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub upsert_key: Option<Option<String>>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub event_filters: Option<Vec<String>>,
    #[serde(default)]
    pub signing_secret: Option<String>,
    #[serde(default)]
    pub allow_external_url: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpsertWebhookSubscriptionResponse {
    pub subscription: WebhookSubscriptionPublic,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalExecutionWebhookPayload {
    pub event_type: String,
    pub delivery_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub workspace_id: Uuid,
    pub session_id: Uuid,
    pub execution_id: Uuid,
    pub execution_process_id: Uuid,
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub queue_item_id: Option<Uuid>,
    pub exit_code: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct TerminalExecutionWebhookEvent {
    pub workspace_id: Uuid,
    pub session_id: Uuid,
    pub execution_process_id: Uuid,
    pub status: TerminalExecutionStatus,
    pub completed_at: Option<DateTime<Utc>>,
    pub queue_item_id: Option<Uuid>,
    pub exit_code: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalExecutionStatus {
    Completed,
    Failed,
    Killed,
}

impl TerminalExecutionStatus {
    pub fn as_status(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Killed => "killed",
        }
    }

    pub fn event_type(self) -> &'static str {
        match self {
            Self::Completed => "execution.completed",
            Self::Failed => "execution.failed",
            Self::Killed => "execution.killed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookDeliveryResult {
    pub subscription_id: Uuid,
    pub delivery_id: Uuid,
    pub event_type: String,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WebhookSubscriptionError {
    #[error("Webhook subscription name is required")]
    MissingName,
    #[error("Webhook signing secret is required")]
    MissingSigningSecret,
    #[error("Unsupported webhook event filter: {0}")]
    UnsupportedEventFilter(String),
    #[error("Invalid webhook URL: {0}")]
    InvalidUrl(String),
    #[error(
        "Webhook URL host is not allowed by default policy: {host}. Use explicit external URL opt-in to allow it."
    )]
    UnsafeExternalUrl { host: String },
    #[error("Webhook subscription upsert key already exists: {0}")]
    DuplicateUpsertKey(String),
    #[error("Webhook subscription not found")]
    NotFound,
}

#[derive(Debug, Clone)]
pub struct WebhookNotificationService {
    config: Arc<RwLock<Config>>,
    client: Client,
}

impl WebhookNotificationService {
    pub fn new(config: Arc<RwLock<Config>>) -> Self {
        Self {
            config,
            client: Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    pub async fn list_subscriptions(&self) -> Vec<WebhookSubscriptionPublic> {
        self.config
            .read()
            .await
            .webhook_subscriptions
            .iter()
            .map(WebhookSubscriptionPublic::from)
            .collect()
    }

    pub async fn get_subscription(&self, id: Uuid) -> Option<WebhookSubscriptionPublic> {
        self.config
            .read()
            .await
            .webhook_subscriptions
            .iter()
            .find(|subscription| subscription.id == id)
            .map(WebhookSubscriptionPublic::from)
    }

    pub async fn create_or_upsert_subscription(
        &self,
        input: CreateWebhookSubscription,
    ) -> Result<UpsertWebhookSubscriptionResponse, WebhookSubscriptionError> {
        validate_name(&input.name)?;
        validate_signing_secret(&input.signing_secret)?;
        let url = validate_subscription_url(&input.url, input.allow_external_url)?;
        let event_filters = normalize_event_filters(input.event_filters)?;
        let now = Utc::now();
        let mut config = self.config.write().await;
        let id_index = input.id.and_then(|id| {
            config
                .webhook_subscriptions
                .iter()
                .position(|subscription| subscription.id == id)
        });
        let key_index = input.upsert_key.as_ref().and_then(|key| {
            config
                .webhook_subscriptions
                .iter()
                .position(|subscription| subscription.upsert_key.as_deref() == Some(key.as_str()))
        });
        let existing_index = match (id_index, key_index) {
            (Some(id_index), Some(key_index)) if id_index != key_index => {
                return Err(WebhookSubscriptionError::DuplicateUpsertKey(
                    input.upsert_key.clone().unwrap_or_default(),
                ));
            }
            (Some(id_index), _) => Some(id_index),
            (None, Some(key_index)) if input.id.is_none() => Some(key_index),
            (None, Some(_)) => {
                return Err(WebhookSubscriptionError::DuplicateUpsertKey(
                    input.upsert_key.clone().unwrap_or_default(),
                ));
            }
            (None, None) => None,
        };

        let (subscription, created) = if let Some(index) = existing_index {
            let existing = &mut config.webhook_subscriptions[index];
            existing.name = input.name;
            existing.upsert_key = input.upsert_key;
            existing.url = url;
            existing.enabled = input.enabled;
            existing.event_filters = event_filters;
            existing.signing_secret = input.signing_secret;
            existing.updated_at = now;
            (WebhookSubscriptionPublic::from(&*existing), false)
        } else {
            let subscription = WebhookSubscription {
                id: input.id.unwrap_or_else(Uuid::new_v4),
                name: input.name,
                upsert_key: input.upsert_key,
                url,
                enabled: input.enabled,
                event_filters,
                signing_secret: input.signing_secret,
                created_at: now,
                updated_at: now,
            };
            let public = WebhookSubscriptionPublic::from(&subscription);
            config.webhook_subscriptions.push(subscription);
            (public, true)
        };

        Ok(UpsertWebhookSubscriptionResponse {
            subscription,
            created,
        })
    }

    pub async fn update_subscription(
        &self,
        id: Uuid,
        input: UpdateWebhookSubscription,
    ) -> Result<WebhookSubscriptionPublic, WebhookSubscriptionError> {
        let mut config = self.config.write().await;
        let index = config
            .webhook_subscriptions
            .iter()
            .position(|subscription| subscription.id == id)
            .ok_or(WebhookSubscriptionError::NotFound)?;

        if let Some(upsert_key) = input.upsert_key.as_ref()
            && let Some(key) = upsert_key.as_deref()
            && config
                .webhook_subscriptions
                .iter()
                .any(|existing| existing.id != id && existing.upsert_key.as_deref() == Some(key))
        {
            return Err(WebhookSubscriptionError::DuplicateUpsertKey(
                key.to_string(),
            ));
        }

        let subscription = &mut config.webhook_subscriptions[index];

        if let Some(name) = input.name {
            validate_name(&name)?;
            subscription.name = name;
        }
        if let Some(upsert_key) = input.upsert_key {
            subscription.upsert_key = upsert_key;
        }
        if let Some(url) = input.url {
            subscription.url = validate_subscription_url(&url, input.allow_external_url)?;
        }
        if let Some(enabled) = input.enabled {
            subscription.enabled = enabled;
        }
        if let Some(event_filters) = input.event_filters {
            subscription.event_filters = normalize_event_filters(event_filters)?;
        }
        if let Some(signing_secret) = input.signing_secret {
            validate_signing_secret(&signing_secret)?;
            subscription.signing_secret = signing_secret;
        }
        subscription.updated_at = Utc::now();
        Ok(WebhookSubscriptionPublic::from(&*subscription))
    }

    pub async fn set_subscription_enabled(
        &self,
        id: Uuid,
        enabled: bool,
    ) -> Result<WebhookSubscriptionPublic, WebhookSubscriptionError> {
        self.update_subscription(
            id,
            UpdateWebhookSubscription {
                name: None,
                upsert_key: None,
                url: None,
                enabled: Some(enabled),
                event_filters: None,
                signing_secret: None,
                allow_external_url: false,
            },
        )
        .await
    }

    pub async fn emit_terminal_execution_event(&self, event: TerminalExecutionWebhookEvent) {
        let subscriptions = self.matching_subscriptions(event.status.event_type()).await;
        if subscriptions.is_empty() {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            let results = service.deliver_to_subscriptions(event, subscriptions).await;
            for result in results {
                if !result.success {
                    tracing::warn!(
                        subscription_id = %result.subscription_id,
                        delivery_id = %result.delivery_id,
                        event_type = %result.event_type,
                        error = ?result.error,
                        "failed to deliver workflow webhook"
                    );
                }
            }
        });
    }

    pub async fn deliver_terminal_execution_event(
        &self,
        event: TerminalExecutionWebhookEvent,
    ) -> Vec<WebhookDeliveryResult> {
        let subscriptions = self.matching_subscriptions(event.status.event_type()).await;
        self.deliver_to_subscriptions(event, subscriptions).await
    }

    async fn matching_subscriptions(&self, event_type: &str) -> Vec<WebhookSubscription> {
        self.config
            .read()
            .await
            .webhook_subscriptions
            .iter()
            .filter(|subscription| {
                subscription.enabled
                    && (subscription.event_filters.is_empty()
                        || subscription
                            .event_filters
                            .iter()
                            .any(|filter| filter == event_type))
            })
            .cloned()
            .collect()
    }

    async fn deliver_to_subscriptions(
        &self,
        event: TerminalExecutionWebhookEvent,
        subscriptions: Vec<WebhookSubscription>,
    ) -> Vec<WebhookDeliveryResult> {
        futures::future::join_all(subscriptions.into_iter().map(|subscription| {
            let service = self.clone();
            let event = event.clone();
            async move {
                let delivery_id = Uuid::new_v4();
                let payload = terminal_execution_payload(&event, delivery_id);
                let event_type = payload.event_type.clone();
                let result = service
                    .post_payload_with_retries(&subscription, &payload)
                    .await;
                WebhookDeliveryResult {
                    subscription_id: subscription.id,
                    delivery_id,
                    event_type,
                    success: result.is_ok(),
                    error: result.err().map(|error| error.to_string()),
                }
            }
        }))
        .await
    }

    async fn post_payload_with_retries(
        &self,
        subscription: &WebhookSubscription,
        payload: &TerminalExecutionWebhookPayload,
    ) -> Result<(), reqwest::Error> {
        let mut last_error = None;
        for attempt in 1..=WEBHOOK_DELIVERY_ATTEMPTS {
            match self.post_payload(subscription, payload).await {
                Ok(()) => return Ok(()),
                Err(error) if attempt < WEBHOOK_DELIVERY_ATTEMPTS => {
                    tracing::warn!(
                        subscription_id = %subscription.id,
                        attempt,
                        "workflow webhook delivery failed; retrying: {error}"
                    );
                    last_error = Some(error);
                    tokio::time::sleep(WEBHOOK_RETRY_BACKOFF).await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.expect("at least one webhook attempt ran"))
    }

    async fn post_payload(
        &self,
        subscription: &WebhookSubscription,
        payload: &TerminalExecutionWebhookPayload,
    ) -> Result<(), reqwest::Error> {
        let body = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
        let timestamp = Utc::now().timestamp().to_string();
        self.client
            .post(&subscription.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(WEBHOOK_HEADER_TIMESTAMP, &timestamp)
            .header(WEBHOOK_HEADER_ALGORITHM, WEBHOOK_ALGORITHM_HMAC_SHA256)
            .header(
                WEBHOOK_HEADER_SIGNATURE,
                sign_webhook_payload(&subscription.signing_secret, &timestamp, &body),
            )
            .body(body)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

impl From<&WebhookSubscription> for WebhookSubscriptionPublic {
    fn from(subscription: &WebhookSubscription) -> Self {
        Self {
            id: subscription.id,
            name: subscription.name.clone(),
            upsert_key: subscription.upsert_key.clone(),
            url: subscription.url.clone(),
            enabled: subscription.enabled,
            event_filters: subscription.event_filters.clone(),
            signing_secret_set: !subscription.signing_secret.trim().is_empty(),
            created_at: subscription.created_at,
            updated_at: subscription.updated_at,
        }
    }
}

pub fn terminal_execution_payload(
    event: &TerminalExecutionWebhookEvent,
    delivery_id: Uuid,
) -> TerminalExecutionWebhookPayload {
    TerminalExecutionWebhookPayload {
        event_type: event.status.event_type().to_string(),
        delivery_id,
        timestamp: Utc::now(),
        workspace_id: event.workspace_id,
        session_id: event.session_id,
        execution_id: event.execution_process_id,
        execution_process_id: event.execution_process_id,
        status: event.status.as_status().to_string(),
        completed_at: event.completed_at,
        queue_item_id: event.queue_item_id,
        exit_code: event.exit_code,
    }
}

pub fn sign_webhook_payload(secret: &str, timestamp: &str, body: &str) -> String {
    let mut key = secret.as_bytes().to_vec();
    if key.len() > 64 {
        key = Sha256::digest(&key).to_vec();
    }
    key.resize(64, 0);

    let mut outer_key_pad = [0x5c_u8; 64];
    let mut inner_key_pad = [0x36_u8; 64];
    for (idx, byte) in key.iter().enumerate() {
        outer_key_pad[idx] ^= byte;
        inner_key_pad[idx] ^= byte;
    }

    let signed_payload = format!("{timestamp}.{body}");
    let mut inner = Sha256::new();
    inner.update(inner_key_pad);
    inner.update(signed_payload.as_bytes());
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_key_pad);
    outer.update(inner_hash);
    let signature = outer.finalize();

    format!("sha256={}", to_hex(&signature))
}

pub fn validate_subscription_url(
    url: &str,
    allow_external_url: bool,
) -> Result<String, WebhookSubscriptionError> {
    let parsed =
        Url::parse(url).map_err(|error| WebhookSubscriptionError::InvalidUrl(error.to_string()))?;
    match parsed.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(WebhookSubscriptionError::InvalidUrl(format!(
                "unsupported scheme {scheme}; expected http or https"
            )));
        }
    }
    let host = parsed
        .host()
        .ok_or_else(|| WebhookSubscriptionError::InvalidUrl("missing host".to_string()))?;
    if allow_external_url || external_urls_allowed_by_env() || is_local_or_private_host(&host) {
        return Ok(parsed.to_string());
    }
    Err(WebhookSubscriptionError::UnsafeExternalUrl {
        host: host.to_string(),
    })
}

pub fn normalize_event_filters(
    filters: Vec<String>,
) -> Result<Vec<String>, WebhookSubscriptionError> {
    let mut normalized = Vec::new();
    for filter in filters {
        let filter = filter.trim().to_string();
        if filter.is_empty() {
            continue;
        }
        if !matches!(
            filter.as_str(),
            "execution.completed" | "execution.failed" | "execution.killed"
        ) {
            return Err(WebhookSubscriptionError::UnsupportedEventFilter(filter));
        }
        if !normalized.contains(&filter) {
            normalized.push(filter);
        }
    }
    Ok(normalized)
}

fn validate_name(name: &str) -> Result<(), WebhookSubscriptionError> {
    if name.trim().is_empty() {
        return Err(WebhookSubscriptionError::MissingName);
    }
    Ok(())
}

fn validate_signing_secret(secret: &str) -> Result<(), WebhookSubscriptionError> {
    if secret.trim().is_empty() {
        return Err(WebhookSubscriptionError::MissingSigningSecret);
    }
    Ok(())
}

fn default_enabled() -> bool {
    true
}

fn external_urls_allowed_by_env() -> bool {
    std::env::var("VK_WEBHOOK_ALLOW_EXTERNAL_URLS")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn is_local_or_private_host(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost" || domain.ends_with(".localhost")
        }
        Host::Ipv4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Host::Ipv6(ip) => {
            ip.is_loopback()
                || matches!(ip.segments()[0] & 0xfe00, 0xfc00 | 0xfe00)
                || IpAddr::V6(*ip).is_unspecified()
        }
    }
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    #[test]
    fn signs_webhook_payload_with_hmac_sha256_prefix() {
        let signature = sign_webhook_payload("secret", "1710000000", r#"{"event_type":"x"}"#);

        assert!(signature.starts_with("sha256="));
        assert_eq!(signature.len(), "sha256=".len() + 64);
        assert_eq!(
            signature,
            "sha256=fa70ad2491edfa48559c61658507c9fb6fb2e897afa256a4c43f8ef882de0675"
        );
        assert_ne!(
            signature,
            sign_webhook_payload("other", "1710000000", r#"{"event_type":"x"}"#)
        );
    }

    #[test]
    fn validates_localhost_and_private_urls_by_default() {
        assert!(validate_subscription_url("http://localhost:3000/webhook", false).is_ok());
        assert!(validate_subscription_url("http://127.0.0.1:3000/webhook", false).is_ok());
        assert!(validate_subscription_url("http://10.1.2.3/webhook", false).is_ok());
        assert!(validate_subscription_url("http://192.168.1.20/webhook", false).is_ok());
    }

    #[test]
    fn rejects_unsafe_external_urls_by_default() {
        let error = validate_subscription_url("https://example.com/webhook", false).unwrap_err();
        assert!(matches!(
            error,
            WebhookSubscriptionError::UnsafeExternalUrl { .. }
        ));
        assert!(validate_subscription_url("https://example.com/webhook", true).is_ok());
    }

    #[test]
    fn terminal_payload_is_refs_only() {
        let delivery_id = Uuid::parse_str("99999999-9999-9999-9999-999999999999").unwrap();
        let event = TerminalExecutionWebhookEvent {
            workspace_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            session_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            execution_process_id: Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
            status: TerminalExecutionStatus::Killed,
            completed_at: None,
            queue_item_id: Some(Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap()),
            exit_code: None,
        };
        let payload = terminal_execution_payload(&event, delivery_id);
        let payload_json = serde_json::to_string(&payload).unwrap();
        assert_eq!(payload.event_type, "execution.killed");
        assert_eq!(payload.status, "killed");
        assert!(payload_json.contains("workspace_id"));
        assert!(!payload_json.contains("prompt"));
        assert!(!payload_json.contains("transcript"));
        assert!(!payload_json.contains("message"));
        assert!(!payload_json.contains("content"));
    }

    #[test]
    fn normalizes_supported_terminal_filters() {
        assert_eq!(
            normalize_event_filters(vec![
                "execution.completed".to_string(),
                "execution.completed".to_string(),
                "execution.killed".to_string(),
            ])
            .unwrap(),
            vec![
                "execution.completed".to_string(),
                "execution.killed".to_string()
            ]
        );
        assert!(normalize_event_filters(vec!["execution.started".to_string()]).is_err());
    }

    #[tokio::test]
    async fn delivers_terminal_events_to_multiple_subscriptions_independently() {
        let (ok_url, ok_rx) = spawn_http_server(200).await;
        let (fail_url, fail_rx) = spawn_http_server(500).await;
        let now = Utc::now();
        let mut config = Config::default();
        config.webhook_subscriptions = vec![
            WebhookSubscription {
                id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
                name: "failing".to_string(),
                upsert_key: Some("failing".to_string()),
                url: fail_url,
                enabled: true,
                event_filters: vec![],
                signing_secret: "secret".to_string(),
                created_at: now,
                updated_at: now,
            },
            WebhookSubscription {
                id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
                name: "ok".to_string(),
                upsert_key: Some("ok".to_string()),
                url: ok_url,
                enabled: true,
                event_filters: vec!["execution.completed".to_string()],
                signing_secret: "secret".to_string(),
                created_at: now,
                updated_at: now,
            },
        ];
        let service = WebhookNotificationService::new(Arc::new(RwLock::new(config)));

        let results = service
            .deliver_terminal_execution_event(sample_event(TerminalExecutionStatus::Completed))
            .await;

        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .any(|result| result.subscription_id.to_string()
                    == "22222222-2222-2222-2222-222222222222"
                    && result.success)
        );
        assert!(
            results
                .iter()
                .any(|result| result.subscription_id.to_string()
                    == "11111111-1111-1111-1111-111111111111"
                    && !result.success)
        );

        let ok_request = ok_rx.await.unwrap();
        let ok_request_lower = ok_request.to_ascii_lowercase();
        assert!(ok_request_lower.contains("x-vk-webhook-timestamp:"));
        assert!(ok_request_lower.contains("x-vk-webhook-algorithm: hmac-sha256"));
        assert!(ok_request_lower.contains("x-vk-webhook-signature: sha256="));
        let ok_body = ok_request.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(ok_body.contains(r#""event_type":"execution.completed""#));
        assert!(ok_body.contains(r#""workspace_id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa""#));
        assert!(!ok_body.contains("prompt"));
        assert!(!ok_body.contains("transcript"));
        assert!(!ok_body.contains("content"));

        let fail_request = fail_rx.await.unwrap();
        let fail_body = fail_request.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(fail_body.contains(r#""event_type":"execution.completed""#));
    }

    #[tokio::test]
    async fn disabled_or_non_matching_subscriptions_do_not_receive_events() {
        let (url, rx) = spawn_http_server(200).await;
        let now = Utc::now();
        let mut config = Config::default();
        config.webhook_subscriptions = vec![WebhookSubscription {
            id: Uuid::new_v4(),
            name: "failed-only".to_string(),
            upsert_key: None,
            url,
            enabled: true,
            event_filters: vec!["execution.failed".to_string()],
            signing_secret: "secret".to_string(),
            created_at: now,
            updated_at: now,
        }];
        let service = WebhookNotificationService::new(Arc::new(RwLock::new(config)));

        let results = service
            .deliver_terminal_execution_event(sample_event(TerminalExecutionStatus::Completed))
            .await;

        assert!(results.is_empty());
        rx.abort();
    }

    #[tokio::test]
    async fn create_upsert_validates_url_policy_and_secrets() {
        let config = Arc::new(RwLock::new(Config::default()));
        let service = WebhookNotificationService::new(config.clone());

        let external = service
            .create_or_upsert_subscription(CreateWebhookSubscription {
                id: None,
                name: "external".to_string(),
                upsert_key: Some("vd".to_string()),
                url: "https://example.com/webhook".to_string(),
                enabled: true,
                event_filters: vec!["execution.killed".to_string()],
                signing_secret: "secret".to_string(),
                allow_external_url: false,
            })
            .await
            .unwrap_err();
        assert!(matches!(
            external,
            WebhookSubscriptionError::UnsafeExternalUrl { .. }
        ));

        let created = service
            .create_or_upsert_subscription(CreateWebhookSubscription {
                id: None,
                name: "vd".to_string(),
                upsert_key: Some("vd".to_string()),
                url: "http://localhost:1234/webhook".to_string(),
                enabled: true,
                event_filters: vec!["execution.killed".to_string()],
                signing_secret: "secret".to_string(),
                allow_external_url: false,
            })
            .await
            .unwrap();
        assert!(created.created);
        assert!(created.subscription.signing_secret_set);

        let updated = service
            .create_or_upsert_subscription(CreateWebhookSubscription {
                id: None,
                name: "vd repaired".to_string(),
                upsert_key: Some("vd".to_string()),
                url: "http://127.0.0.1:1234/webhook".to_string(),
                enabled: false,
                event_filters: vec!["execution.completed".to_string()],
                signing_secret: "secret-2".to_string(),
                allow_external_url: false,
            })
            .await
            .unwrap();
        assert!(!updated.created);
        assert_eq!(config.read().await.webhook_subscriptions.len(), 1);
        assert_eq!(updated.subscription.name, "vd repaired");
        assert!(!updated.subscription.enabled);
    }

    #[tokio::test]
    async fn create_or_upsert_rejects_duplicate_upsert_key_for_different_id() {
        let config = Arc::new(RwLock::new(Config::default()));
        let service = WebhookNotificationService::new(config);
        let first_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let second_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        service
            .create_or_upsert_subscription(CreateWebhookSubscription {
                id: Some(first_id),
                name: "first".to_string(),
                upsert_key: Some("shared".to_string()),
                url: "http://localhost:1234/first".to_string(),
                enabled: true,
                event_filters: vec![],
                signing_secret: "secret".to_string(),
                allow_external_url: false,
            })
            .await
            .unwrap();

        let error = service
            .create_or_upsert_subscription(CreateWebhookSubscription {
                id: Some(second_id),
                name: "second".to_string(),
                upsert_key: Some("shared".to_string()),
                url: "http://localhost:1234/second".to_string(),
                enabled: true,
                event_filters: vec![],
                signing_secret: "secret".to_string(),
                allow_external_url: false,
            })
            .await
            .unwrap_err();

        assert_eq!(
            error,
            WebhookSubscriptionError::DuplicateUpsertKey("shared".to_string())
        );
    }

    #[tokio::test]
    async fn update_rejects_duplicate_upsert_key() {
        let config = Arc::new(RwLock::new(Config::default()));
        let service = WebhookNotificationService::new(config);
        let first_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let second_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        for (id, key) in [(first_id, "first"), (second_id, "second")] {
            service
                .create_or_upsert_subscription(CreateWebhookSubscription {
                    id: Some(id),
                    name: key.to_string(),
                    upsert_key: Some(key.to_string()),
                    url: format!("http://localhost:1234/{key}"),
                    enabled: true,
                    event_filters: vec![],
                    signing_secret: "secret".to_string(),
                    allow_external_url: false,
                })
                .await
                .unwrap();
        }

        let error = service
            .update_subscription(
                second_id,
                UpdateWebhookSubscription {
                    name: None,
                    upsert_key: Some(Some("first".to_string())),
                    url: None,
                    enabled: None,
                    event_filters: None,
                    signing_secret: None,
                    allow_external_url: false,
                },
            )
            .await
            .unwrap_err();

        assert_eq!(
            error,
            WebhookSubscriptionError::DuplicateUpsertKey("first".to_string())
        );
    }

    fn sample_event(status: TerminalExecutionStatus) -> TerminalExecutionWebhookEvent {
        TerminalExecutionWebhookEvent {
            workspace_id: Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap(),
            session_id: Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap(),
            execution_process_id: Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap(),
            status,
            completed_at: Some(Utc::now()),
            queue_item_id: Some(Uuid::parse_str("dddddddd-dddd-dddd-dddd-dddddddddddd").unwrap()),
            exit_code: Some(0),
        }
    }

    async fn spawn_http_server(status: u16) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0_u8; 8192];
            let read = stream.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..read]).to_string();
            let reason = if status == 200 { "OK" } else { "ERR" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            request
        });
        (format!("http://{addr}/webhook"), handle)
    }
}
