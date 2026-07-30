use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

pub const APPROVAL_TIMEOUT_SECONDS: i64 = 36000; // 10 hours

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ApprovalRequest {
    pub id: String,
    pub tool_name: String,
    pub execution_process_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub timeout_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub host_command: Option<HostCommandRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
pub struct HostCommandRequest {
    pub command: String,
    pub cwd: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(default = "default_host_command_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_host_command_output_limit_bytes")]
    pub output_limit_bytes: usize,
}

fn default_host_command_timeout_secs() -> u64 {
    600
}
fn default_host_command_output_limit_bytes() -> usize {
    64 * 1024
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
pub struct HostCommandResult {
    pub exit_code: Option<i64>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

impl ApprovalRequest {
    pub fn new(tool_name: String, execution_process_id: Uuid) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            tool_name,
            execution_process_id,
            created_at: now,
            timeout_at: now + Duration::seconds(APPROVAL_TIMEOUT_SECONDS),
            host_command: None,
        }
    }
}

/// Status of a tool permission request (approve/deny for tool execution).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied {
        #[ts(optional)]
        reason: Option<String>,
    },
    TimedOut,
}

/// A question–answer pair. `answer` holds one or more selected labels/values.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct QuestionAnswer {
    pub question: String,
    pub answer: Vec<String>,
}

/// Status of a question answer request.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum QuestionStatus {
    Answered { answers: Vec<QuestionAnswer> },
    TimedOut,
}

// Tracks both approval and question answers requests
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ApprovalOutcome {
    Approved,
    Denied {
        #[ts(optional)]
        reason: Option<String>,
    },
    Answered {
        answers: Vec<QuestionAnswer>,
    },
    HostCommandCompleted {
        result: HostCommandResult,
    },
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ApprovalResponse {
    pub execution_process_id: Uuid,
    pub status: ApprovalOutcome,
}
