use std::{sync::Arc, time::Duration};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::services::config::{Config, WebhookConfig, WebhookProvider};

/// Metadata about the task/execution for webhook payloads
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebhookMetadata {
    pub event_type: Option<String>,
    pub delivery_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub task_title: Option<String>,
    pub project_id: Option<Uuid>,
    pub project_name: Option<String>,
    pub workspace_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub execution_id: Option<Uuid>,
    pub exit_code: Option<i64>,
}

impl WebhookMetadata {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_task(mut self, id: Uuid, title: &str) -> Self {
        self.task_id = Some(id);
        self.task_title = Some(title.to_string());
        self
    }

    pub fn with_event_type(mut self, event_type: &str) -> Self {
        self.event_type = Some(event_type.to_string());
        self
    }

    pub fn with_project(mut self, id: Uuid, name: &str) -> Self {
        self.project_id = Some(id);
        self.project_name = Some(name.to_string());
        self
    }

    pub fn with_workspace(mut self, id: Uuid) -> Self {
        self.workspace_id = Some(id);
        self
    }

    pub fn with_session(mut self, id: Uuid) -> Self {
        self.session_id = Some(id);
        self
    }

    pub fn with_execution(mut self, id: Uuid) -> Self {
        self.execution_id = Some(id);
        self
    }

    pub fn with_exit_code(mut self, code: i64) -> Self {
        self.exit_code = Some(code);
        self
    }
}

const WEBHOOK_DELIVERY_ATTEMPTS: usize = 3;
const WEBHOOK_RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// Service for sending webhook notifications to various platforms
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
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    /// Send webhook notifications if enabled
    pub async fn send_notification(&self, title: &str, message: &str, metadata: &WebhookMetadata) {
        let webhooks = {
            let config = self.config.read().await;

            if !config.notifications.webhook_notifications_enabled {
                return;
            }

            config
                .notifications
                .webhooks
                .iter()
                .filter(|webhook| webhook.enabled)
                .cloned()
                .collect::<Vec<_>>()
        };

        for webhook in webhooks {
            let service = self.clone();
            let title = title.to_string();
            let message = message.to_string();
            let mut metadata = metadata.clone();
            metadata.delivery_id.get_or_insert_with(Uuid::new_v4);

            // Deliveries are intentionally off the execution critical path. They are
            // retried in-memory with a stable delivery_id, but not durably queued; if
            // the VK process exits before completion, the notification is best-effort.
            tokio::spawn(async move {
                let provider = webhook.provider.clone();

                for attempt in 1..=WEBHOOK_DELIVERY_ATTEMPTS {
                    let result = match provider.clone() {
                        WebhookProvider::Slack => {
                            service
                                .send_slack_notification(&webhook, &title, &message, &metadata)
                                .await
                        }
                        WebhookProvider::Discord => {
                            service
                                .send_discord_notification(&webhook, &title, &message, &metadata)
                                .await
                        }
                        WebhookProvider::Pushover => {
                            service
                                .send_pushover_notification(&webhook, &title, &message, &metadata)
                                .await
                        }
                        WebhookProvider::Telegram => {
                            service
                                .send_telegram_notification(&webhook, &title, &message, &metadata)
                                .await
                        }
                        WebhookProvider::Generic => {
                            service
                                .send_generic_notification(&webhook, &title, &message, &metadata)
                                .await
                        }
                    };

                    match result {
                        Ok(()) => return,
                        Err(error) if attempt < WEBHOOK_DELIVERY_ATTEMPTS => {
                            tracing::warn!(
                                "Failed to send {:?} webhook notification on attempt {}/{}: {}",
                                provider,
                                attempt,
                                WEBHOOK_DELIVERY_ATTEMPTS,
                                error
                            );
                            tokio::time::sleep(WEBHOOK_RETRY_BACKOFF).await;
                        }
                        Err(error) => {
                            tracing::warn!(
                                "Failed to send {:?} webhook notification after {} attempts: {}",
                                provider,
                                WEBHOOK_DELIVERY_ATTEMPTS,
                                error
                            );
                        }
                    }
                }
            });
        }
    }

    /// Send notification to Slack with blocks format
    async fn send_slack_notification(
        &self,
        webhook: &WebhookConfig,
        title: &str,
        message: &str,
        metadata: &WebhookMetadata,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut context_elements = vec![];

        if let Some(project_name) = &metadata.project_name {
            context_elements.push(json!({
                "type": "mrkdwn",
                "text": format!("*Project:* {}", project_name)
            }));
        }
        if let Some(task_id) = metadata.task_id {
            context_elements.push(json!({
                "type": "mrkdwn",
                "text": format!("*Task ID:* {}", task_id)
            }));
        }
        if let Some(exit_code) = metadata.exit_code {
            context_elements.push(json!({
                "type": "mrkdwn",
                "text": format!("*Exit Code:* {}", exit_code)
            }));
        }

        let mut blocks = vec![
            json!({
                "type": "header",
                "text": {
                    "type": "plain_text",
                    "text": title,
                }
            }),
            json!({
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": message,
                }
            }),
        ];

        if !context_elements.is_empty() {
            blocks.push(json!({
                "type": "context",
                "elements": context_elements
            }));
        }

        let payload = json!({ "blocks": blocks });

        self.post_json(webhook, &payload)
            .await?
            .error_for_status()?;

        Ok(())
    }

    /// Send notification to Discord with embeds format
    async fn send_discord_notification(
        &self,
        webhook: &WebhookConfig,
        title: &str,
        message: &str,
        metadata: &WebhookMetadata,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let timestamp = chrono::Utc::now().to_rfc3339();

        let mut fields = vec![];

        if let Some(project_name) = &metadata.project_name {
            fields.push(json!({
                "name": "Project",
                "value": project_name,
                "inline": true
            }));
        }
        if let Some(task_id) = metadata.task_id {
            fields.push(json!({
                "name": "Task ID",
                "value": task_id.to_string(),
                "inline": true
            }));
        }
        if let Some(project_id) = metadata.project_id {
            fields.push(json!({
                "name": "Project ID",
                "value": project_id.to_string(),
                "inline": true
            }));
        }
        if let Some(exit_code) = metadata.exit_code {
            fields.push(json!({
                "name": "Exit Code",
                "value": exit_code.to_string(),
                "inline": true
            }));
        }

        let mut embed = json!({
            "title": title,
            "description": message,
            "timestamp": timestamp,
            "color": 5814783, // Blue color
        });

        if !fields.is_empty() {
            embed["fields"] = json!(fields);
        }

        let payload = json!({ "embeds": [embed] });

        self.post_json(webhook, &payload)
            .await?
            .error_for_status()?;

        Ok(())
    }

    /// Send notification to Pushover
    async fn send_pushover_notification(
        &self,
        webhook: &WebhookConfig,
        title: &str,
        message: &str,
        metadata: &WebhookMetadata,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let user_key = webhook
            .pushover_user_key
            .as_ref()
            .ok_or("Pushover user key not configured")?;

        // Extract token from webhook_url (format: https://api.pushover.net/1/messages.json?token=TOKEN)
        let token = webhook
            .webhook_url
            .split("token=")
            .nth(1)
            .ok_or("Invalid Pushover webhook URL format")?;

        // Build message with metadata
        let mut full_message = message.to_string();
        let mut details = vec![];

        if let Some(project_name) = &metadata.project_name {
            details.push(format!("Project: {}", project_name));
        }
        if let Some(task_id) = metadata.task_id {
            details.push(format!("Task ID: {}", task_id));
        }
        if let Some(exit_code) = metadata.exit_code {
            details.push(format!("Exit Code: {}", exit_code));
        }

        if !details.is_empty() {
            full_message.push_str("\n\n");
            full_message.push_str(&details.join("\n"));
        }

        let payload = json!({
            "token": token,
            "user": user_key,
            "title": title,
            "message": full_message,
        });

        self.client
            .post("https://api.pushover.net/1/messages.json")
            .json(&payload)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    /// Send notification to Telegram
    async fn send_telegram_notification(
        &self,
        webhook: &WebhookConfig,
        title: &str,
        message: &str,
        metadata: &WebhookMetadata,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let chat_id = webhook
            .telegram_chat_id
            .as_ref()
            .ok_or("Telegram chat ID not configured")?;

        let mut full_message = format!("<b>{}</b>\n\n{}", title, message);

        let mut details = vec![];
        if let Some(project_name) = &metadata.project_name {
            details.push(format!("<b>Project:</b> {}", project_name));
        }
        if let Some(task_id) = metadata.task_id {
            details.push(format!("<b>Task ID:</b> {}", task_id));
        }
        if let Some(exit_code) = metadata.exit_code {
            details.push(format!("<b>Exit Code:</b> {}", exit_code));
        }

        if !details.is_empty() {
            full_message.push_str("\n\n");
            full_message.push_str(&details.join("\n"));
        }

        let payload = json!({
            "chat_id": chat_id,
            "text": full_message,
            "parse_mode": "HTML",
        });

        self.post_json(webhook, &payload)
            .await?
            .error_for_status()?;

        Ok(())
    }

    /// Send generic JSON notification
    async fn send_generic_notification(
        &self,
        webhook: &WebhookConfig,
        title: &str,
        message: &str,
        metadata: &WebhookMetadata,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let timestamp = chrono::Utc::now().to_rfc3339();
        let delivery_id = metadata.delivery_id.unwrap_or_else(Uuid::new_v4);

        let payload = json!({
            "event_type": metadata.event_type,
            "delivery_id": delivery_id,
            "title": title,
            "message": message,
            "timestamp": timestamp,
            "task_id": metadata.task_id,
            "task_title": metadata.task_title,
            "project_id": metadata.project_id,
            "project_name": metadata.project_name,
            "workspace_id": metadata.workspace_id,
            "session_id": metadata.session_id,
            "execution_id": metadata.execution_id,
            "exit_code": metadata.exit_code,
        });

        self.post_json(webhook, &payload)
            .await?
            .error_for_status()?;

        Ok(())
    }

    async fn post_json(
        &self,
        webhook: &WebhookConfig,
        payload: &serde_json::Value,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let body = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
        let timestamp = chrono::Utc::now().timestamp().to_string();

        let mut request = self
            .client
            .post(&webhook.webhook_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("X-VK-Webhook-Timestamp", &timestamp)
            .body(body.clone());

        if let Some(secret) = webhook
            .signing_secret
            .as_deref()
            .map(str::trim)
            .filter(|secret: &&str| !secret.is_empty())
        {
            let signature = sign_webhook_payload(secret, &timestamp, &body);
            request = request
                .header("X-VK-Webhook-Algorithm", "hmac-sha256")
                .header("X-VK-Webhook-Signature", signature);
        }

        request.send().await
    }
}

pub(crate) fn sign_webhook_payload(secret: &str, timestamp: &str, body: &str) -> String {
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
    use super::sign_webhook_payload;

    #[test]
    fn signs_webhook_payload_with_hmac_sha256_prefix() {
        let signature = sign_webhook_payload("secret", "1710000000", r#"{"event_type":"x"}"#);

        assert!(signature.starts_with("sha256="));
        assert_eq!(signature.len(), "sha256=".len() + 64);
        assert_eq!(
            signature,
            sign_webhook_payload("secret", "1710000000", r#"{"event_type":"x"}"#)
        );
        assert_ne!(
            signature,
            sign_webhook_payload("other", "1710000000", r#"{"event_type":"x"}"#)
        );
    }
}
