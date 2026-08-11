//! QA Mode: Mock executor for testing
//!
//! This module provides a mock executor that:
//! 1. Performs random file operations (create, delete, modify)
//! 2. Streams 10 mock log entries over 10 seconds
//! 3. Outputs logs in ClaudeJson format for compatibility with existing log normalization

use std::{
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};

use async_trait::async_trait;
use rand::seq::SliceRandom as _;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use ts_rs::TS;
use workspace_utils::{
    approvals::HostCommandRequest, command_ext::GroupSpawnNoWindowExt, msg_store::MsgStore,
};

use crate::{
    approvals::ExecutorApprovalService,
    command::CmdOverrides,
    env::ExecutionEnv,
    executors::{
        BaseCodingAgent, ExecutorError, SpawnedChild, StandardCodingAgentExecutor,
        claude::{
            ClaudeContentItem, ClaudeJson, ClaudeMessage, ClaudeMessageContent, ClaudeToolData,
        },
    },
    logs::utils::EntryIndexProvider,
    profile::ExecutorConfig,
    sandbox::prepare_agent_command,
};

/// Mock executor for QA testing
#[derive(Clone, Serialize, Deserialize, Default, TS, JsonSchema)]
pub struct QaMockExecutor {
    #[serde(skip)]
    #[schemars(skip)]
    #[ts(skip)]
    approvals: Option<Arc<dyn ExecutorApprovalService>>,
}

impl fmt::Debug for QaMockExecutor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QaMockExecutor")
            .field("approvals", &self.approvals.is_some())
            .finish()
    }
}

impl PartialEq for QaMockExecutor {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

#[async_trait]
impl StandardCodingAgentExecutor for QaMockExecutor {
    fn apply_overrides(&mut self, _executor_config: &ExecutorConfig) {}

    fn use_approvals(&mut self, approvals: Arc<dyn ExecutorApprovalService>) {
        self.approvals = Some(approvals);
    }

    async fn spawn(
        &self,
        current_dir: &Path,
        prompt: &str,
        _env: &ExecutionEnv,
    ) -> Result<SpawnedChild, ExecutorError> {
        info!("QA Mock Executor: spawning mock execution");

        // 1. Perform legacy mock file operations before spawning the log output process.
        // These operations are intentionally not used as sandbox proof; sandbox probes
        // below run inside the prepared child process.
        perform_file_operations(current_dir).await;

        // 2. Generate mock logs and write them under the workspace so sandboxed
        // child processes can read them even when /tmp is private.
        let run_id = uuid::Uuid::new_v4();
        let qa_dir = current_dir
            .join(".vibe")
            .join("qa-mock")
            .join(run_id.to_string());
        tokio::fs::create_dir_all(&qa_dir)
            .await
            .map_err(|e| ExecutorError::Io(std::io::Error::other(e)))?;
        let log_file = qa_dir.join("logs.jsonl");
        let done_file = qa_dir.join("host-command.done");
        let host_result_file = qa_dir.join("host-command-result.txt");

        let logs = generate_mock_logs(prompt);
        let content = logs.join("\n") + "\n";
        tokio::fs::write(&log_file, &content)
            .await
            .map_err(|e| ExecutorError::Io(std::io::Error::other(e)))?;

        let wants_sandbox_probe = qa_prompt_contains_marker(prompt, "VK_QA_SANDBOX_PROBE");
        let wants_host_command = qa_prompt_contains_marker(prompt, "VK_QA_HOST_COMMAND");
        let cancel = CancellationToken::new();
        if wants_host_command {
            if let Some(approvals) = self.approvals.clone() {
                spawn_host_command_trigger(
                    approvals,
                    current_dir.to_path_buf(),
                    done_file.clone(),
                    host_result_file.clone(),
                    cancel.clone(),
                );
            } else {
                tokio::fs::write(
                    &host_result_file,
                    "QA host-command trigger failed: approval service unavailable\n",
                )
                .await
                .map_err(|e| ExecutorError::Io(std::io::Error::other(e)))?;
                tokio::fs::write(&done_file, "done\n")
                    .await
                    .map_err(|e| ExecutorError::Io(std::io::Error::other(e)))?;
            }
        }

        // 3. Create shell script that performs requested probes inside the
        // sandboxed execution path, then streams Claude-compatible mock logs.
        let script = qa_runner_script(
            &log_file,
            wants_sandbox_probe,
            wants_host_command,
            &done_file,
            &host_result_file,
        );

        let prepared = prepare_agent_command(
            PathBuf::from("/bin/sh"),
            vec!["-c".to_string(), script],
            current_dir,
            _env,
            &CmdOverrides::default(),
        )
        .await?;
        let mut cmd = tokio::process::Command::new(&prepared.program);
        prepared.apply_env_to_command(&mut cmd);
        cmd.args(&prepared.args)
            .current_dir(&prepared.current_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let child = cmd.group_spawn_no_window().map_err(ExecutorError::Io)?;
        let mut spawned = SpawnedChild::from(child);
        if wants_host_command {
            spawned.cancel = Some(cancel);
        }
        Ok(spawned)
    }

    async fn spawn_follow_up(
        &self,
        current_dir: &Path,
        prompt: &str,
        _session_id: &str,
        _reset_to_message_id: Option<&str>,
        env: &ExecutionEnv,
    ) -> Result<SpawnedChild, ExecutorError> {
        // QA mode doesn't support real sessions, just spawn fresh
        info!("QA Mock Executor: follow-up request treated as new spawn");
        self.spawn(current_dir, prompt, env).await
    }

    fn normalize_logs(
        &self,
        msg_store: Arc<MsgStore>,
        current_dir: &Path,
    ) -> Vec<tokio::task::JoinHandle<()>> {
        // Reuse Claude's log processor since we output ClaudeJson format
        let entry_index_provider = EntryIndexProvider::start_from(&msg_store);
        let h1 = crate::executors::claude::ClaudeLogProcessor::process_logs(
            msg_store,
            current_dir,
            entry_index_provider,
            crate::executors::claude::HistoryStrategy::Default,
        );
        vec![h1]
    }

    fn default_mcp_config_path(&self) -> Option<std::path::PathBuf> {
        None // QA mock doesn't need MCP config
    }

    fn get_preset_options(&self) -> ExecutorConfig {
        ExecutorConfig {
            executor: BaseCodingAgent::QaMock,
            variant: None,
            model_id: Some("qa-mock".to_string()),
            agent_id: None,
            reasoning_id: None,
            permission_policy: Some(crate::model_selector::PermissionPolicy::Auto),
            sandbox: None,
        }
    }
}

fn qa_runner_script(
    log_file: &Path,
    wants_sandbox_probe: bool,
    wants_host_command: bool,
    done_file: &Path,
    host_result_file: &Path,
) -> String {
    let mut script = String::from("set +e\n");
    if wants_sandbox_probe {
        script.push_str(QA_SANDBOX_PROBE_SCRIPT);
    }
    if wants_host_command {
        script.push_str(&format!(
            r#"echo 'QA host-command approval requested; waiting for user response...'
i=0
while [ ! -f {done_file} ] && [ "$i" -lt 3000 ]; do
  i=$((i + 1))
  sleep 0.2
done
if [ -f {result_file} ]; then
  cat {result_file}
else
  echo 'QA host-command approval did not complete before timeout.'
fi
"#,
            done_file = shell_single_quote(done_file),
            result_file = shell_single_quote(host_result_file),
        ));
    }
    script.push_str(&format!(
        r#"while IFS= read -r line; do
  echo "$line"
  sleep "${{VK_QA_MOCK_LOG_DELAY_SECS:-1}}"
done < {log_file}
"#,
        log_file = shell_single_quote(log_file),
    ));
    script
}

const QA_SANDBOX_PROBE_SCRIPT: &str = r#"probe_file="qa_sandbox_probe_result.json"
workspace_file="qa_sandbox_probe_workspace_write.txt"
readonly_file="node_modules/qa_sandbox_probe_readonly_write.txt"

if printf 'workspace-write-ok\n' > "$workspace_file" 2>/dev/null; then
  workspace_write="allowed"
else
  workspace_write="denied"
fi

readonly_present="false"
if [ -d "node_modules" ]; then
  readonly_present="true"
  if printf 'readonly-write-should-fail\n' > "$readonly_file" 2>/dev/null; then
    readonly_write="allowed"
    rm -f "$readonly_file" 2>/dev/null
  else
    readonly_write="denied"
  fi
else
  readonly_write="skipped_missing_path"
fi

if command -v curl >/dev/null 2>&1; then
  if curl -fsS --max-time 3 https://example.com >/dev/null 2>&1; then
    network="allowed"
  else
    network="denied_or_unreachable"
  fi
elif command -v python3 >/dev/null 2>&1; then
  if python3 -c 'import socket; socket.create_connection(("example.com", 443), timeout=3).close()' >/dev/null 2>&1; then
    network="allowed"
  else
    network="denied_or_unreachable"
  fi
else
  network="probe_tool_unavailable"
fi

cat > "$probe_file" <<EOF
{
  "probe": "VK_QA_SANDBOX_PROBE",
  "workspace_write": "$workspace_write",
  "readonly_path": "node_modules",
  "readonly_path_present": $readonly_present,
  "readonly_write": "$readonly_write",
  "network": "$network"
}
EOF
cat "$probe_file"
"#;

fn spawn_host_command_trigger(
    approvals: Arc<dyn ExecutorApprovalService>,
    cwd: PathBuf,
    done_file: PathBuf,
    result_file: PathBuf,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let request = HostCommandRequest {
            command: "printf 'VK_QA_HOST_COMMAND_OK\\n' > qa_host_command_approved.txt && printf 'VK_QA_HOST_COMMAND_STDOUT\\n'".to_string(),
            cwd: cwd.to_string_lossy().to_string(),
            reason: "QA mode deterministic host-command approval trigger".to_string(),
            env: HashMap::new(),
            timeout_secs: 30,
            output_limit_bytes: 4096,
        };
        let text = match approvals.create_host_command_approval(request).await {
            Ok(approval_id) => match approvals
                .wait_host_command_result(&approval_id, cancel)
                .await
            {
                Ok(result) => format!(
                    "QA host-command completed: exit_code={:?} timed_out={} truncated={} stdout={} stderr={}\n",
                    result.exit_code,
                    result.timed_out,
                    result.truncated,
                    result.stdout.replace('\n', "\\n"),
                    result.stderr.replace('\n', "\\n")
                ),
                Err(err) => format!("QA host-command denied or failed: {err}\n"),
            },
            Err(err) => format!("QA host-command approval request failed: {err}\n"),
        };
        if let Some(parent) = result_file.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let _ = tokio::fs::write(&result_file, text).await;
        let _ = tokio::fs::write(&done_file, "done\n").await;
    });
}

fn shell_single_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn qa_prompt_contains_marker(prompt: &str, marker: &str) -> bool {
    if prompt.contains(marker) {
        return true;
    }
    // VK's markdown editor may escape underscores before sending the prompt
    // through the API (for example `VK_QA_HOST_COMMAND` becomes
    // `VK\_QA\_HOST\_COMMAND`). Normalize that markdown escaping only for
    // qa-mode trigger detection so tester/user prompt text remains unchanged.
    prompt.replace("\\_", "_").contains(marker)
}

/// Perform random file operations in the worktree
async fn perform_file_operations(dir: &Path) {
    info!("QA Mock: performing file operations in {:?}", dir);

    // Create: qa_created_{uuid}.txt
    let uuid = uuid::Uuid::new_v4();
    let new_file = dir.join(format!("qa_created_{}.txt", uuid));
    match tokio::fs::write(&new_file, "QA mode created this file\n").await {
        Ok(_) => info!("QA Mock: created file {:?}", new_file),
        Err(e) => warn!("QA Mock: failed to create file: {}", e),
    }

    // Find files (excluding .git and binary files)
    let files: Vec<_> = walkdir::WalkDir::new(dir)
        .max_depth(3) // Limit depth to avoid long walks
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| !e.path().to_string_lossy().contains(".git"))
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ["rs", "ts", "js", "txt", "md", "json"].contains(&ext))
        })
        .collect();

    if files.len() >= 2 {
        // Pick random indices before any await points (thread_rng is not Send)
        let (remove_idx, modify_idx) = {
            let mut rng = rand::thread_rng();
            let mut indices: Vec<usize> = (0..files.len()).collect();
            indices.shuffle(&mut rng);
            (indices.first().copied(), indices.get(1).copied())
        };

        // Remove a random file (first shuffled index)
        if let Some(idx) = remove_idx {
            let file_to_remove = files[idx].path().to_path_buf();
            // Don't remove the file we just created
            if file_to_remove != new_file {
                match tokio::fs::remove_file(&file_to_remove).await {
                    Ok(_) => info!("QA Mock: removed file {:?}", file_to_remove),
                    Err(e) => warn!("QA Mock: failed to remove file: {}", e),
                }
            }
        }

        // Modify a different random file (second shuffled index)
        if let Some(idx) = modify_idx {
            let file_to_modify = files[idx].path().to_path_buf();
            // Don't modify the file we just created
            if file_to_modify != new_file {
                match tokio::fs::read_to_string(&file_to_modify).await {
                    Ok(content) => {
                        let modified = format!(
                            "{}\n// QA modification at {}\n",
                            content,
                            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
                        );
                        match tokio::fs::write(&file_to_modify, modified).await {
                            Ok(_) => info!("QA Mock: modified file {:?}", file_to_modify),
                            Err(e) => warn!("QA Mock: failed to write modified file: {}", e),
                        }
                    }
                    Err(e) => warn!("QA Mock: failed to read file for modification: {}", e),
                }
            }
        }
    } else {
        info!(
            "QA Mock: not enough files found for remove/modify operations (found {})",
            files.len()
        );
    }
}

/// Generate 10 mock log entries in ClaudeJson format using strongly-typed structs
fn generate_mock_logs(prompt: &str) -> Vec<String> {
    let session_id = uuid::Uuid::new_v4().to_string();

    let logs: Vec<ClaudeJson> = vec![
        // 1. System init
        ClaudeJson::System {
            subtype: Some("init".to_string()),
            session_id: Some(session_id.clone()),
            cwd: None,
            tools: None,
            model: Some("qa-mock-executor".to_string()),
            api_key_source: Some("unknown".to_string()),
            status: None,
            slash_commands: vec![],
            plugins: vec![],
            agents: vec![],
            task_id: None,
            tool_use_id: None,
            description: None,
            task_type: None,
            prompt: None,
            summary: None,
            last_tool_name: None,
        },
        // 2. Assistant thinking
        ClaudeJson::Assistant {
            message: ClaudeMessage {
                id: Some("msg-qa-1".to_string()),
                message_type: Some("message".to_string()),
                role: "assistant".to_string(),
                model: Some("qa-mock".to_string()),
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::Thinking {
                    thinking: "Analyzing the QA task and preparing mock execution...".to_string(),
                }]),
                stop_reason: None,
            },
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-1".to_string()),
        },
        // 3. Read tool use
        ClaudeJson::Assistant {
            message: ClaudeMessage {
                id: Some("msg-qa-2".to_string()),
                message_type: Some("message".to_string()),
                role: "assistant".to_string(),
                model: Some("qa-mock".to_string()),
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::ToolUse {
                    id: "qa-tool-1".to_string(),
                    tool_data: ClaudeToolData::Read {
                        file_path: "README.md".to_string(),
                    },
                }]),
                stop_reason: None,
            },
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-2".to_string()),
        },
        // 4. Read tool result
        ClaudeJson::User {
            message: ClaudeMessage {
                id: Some("msg-qa-3".to_string()),
                message_type: Some("message".to_string()),
                role: "user".to_string(),
                model: None,
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::ToolResult {
                    tool_use_id: "qa-tool-1".to_string(),
                    content: serde_json::json!(
                        "# Project README\\n\\nThis is a QA test repository."
                    ),
                    is_error: Some(false),
                }]),
                stop_reason: None,
            },
            is_synthetic: false,
            is_replay: false,
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-3".to_string()),
        },
        // 5. Write tool use
        ClaudeJson::Assistant {
            message: ClaudeMessage {
                id: Some("msg-qa-4".to_string()),
                message_type: Some("message".to_string()),
                role: "assistant".to_string(),
                model: Some("qa-mock".to_string()),
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::ToolUse {
                    id: "qa-tool-2".to_string(),
                    tool_data: ClaudeToolData::Write {
                        file_path: "qa_output.txt".to_string(),
                        content: "QA generated content".to_string(),
                    },
                }]),
                stop_reason: None,
            },
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-4".to_string()),
        },
        // 6. Write tool result
        ClaudeJson::User {
            message: ClaudeMessage {
                id: Some("msg-qa-5".to_string()),
                message_type: Some("message".to_string()),
                role: "user".to_string(),
                model: None,
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::ToolResult {
                    tool_use_id: "qa-tool-2".to_string(),
                    content: serde_json::json!("File written successfully"),
                    is_error: Some(false),
                }]),
                stop_reason: None,
            },
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-5".to_string()),
            is_synthetic: false,
            is_replay: false,
        },
        // 7. Bash tool use
        ClaudeJson::Assistant {
            message: ClaudeMessage {
                id: Some("msg-qa-6".to_string()),
                message_type: Some("message".to_string()),
                role: "assistant".to_string(),
                model: Some("qa-mock".to_string()),
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::ToolUse {
                    id: "qa-tool-3".to_string(),
                    tool_data: ClaudeToolData::Bash {
                        command: "echo 'QA test complete'".to_string(),
                        description: Some("Run QA test command".to_string()),
                    },
                }]),
                stop_reason: None,
            },
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-6".to_string()),
        },
        // 8. Bash tool result
        ClaudeJson::User {
            message: ClaudeMessage {
                id: Some("msg-qa-7".to_string()),
                message_type: Some("message".to_string()),
                role: "user".to_string(),
                model: None,
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::ToolResult {
                    tool_use_id: "qa-tool-3".to_string(),
                    content: serde_json::json!("QA test complete\\n"),
                    is_error: Some(false),
                }]),
                stop_reason: None,
            },
            is_synthetic: false,
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-7".to_string()),
            is_replay: false,
        },
        // 9. Assistant final message
        ClaudeJson::Assistant {
            message: ClaudeMessage {
                id: Some("msg-qa-8".to_string()),
                message_type: Some("message".to_string()),
                role: "assistant".to_string(),
                model: Some("qa-mock".to_string()),
                content: ClaudeMessageContent::Array(vec![ClaudeContentItem::Text {
                    text: format!(
                        "QA mode execution completed successfully.\\n\\nI performed the following operations:\\n1. Read README.md\\n2. Created qa_output.txt\\n3. Ran a test command\\nOriginal prompt: {}",
                        prompt
                    ),
                }]),
                stop_reason: Some("end_turn".to_string()),
            },
            session_id: Some(session_id.clone()),
            uuid: Some("uuid-qa-8".to_string()),
        },
        // 10. Result success
        ClaudeJson::Result {
            subtype: Some("success".to_string()),
            is_error: Some(false),
            duration_ms: Some(10000),
            result: None,
            error: None,
            num_turns: Some(3),
            session_id: Some(session_id),
            model_usage: None,
            usage: None,
        },
    ];

    // Serialize to JSON strings - this ensures proper escaping
    logs.into_iter()
        .map(|log| serde_json::to_string(&log).expect("ClaudeJson should serialize"))
        .collect()
}

#[cfg(test)]
mod tests {
    use workspace_utils::approvals::{ApprovalStatus, HostCommandResult, QuestionStatus};

    use super::*;

    #[test]
    fn test_generate_mock_logs_count() {
        let logs = generate_mock_logs("test prompt");
        assert_eq!(logs.len(), 10, "Should generate exactly 10 log entries");
    }

    #[test]
    fn test_generate_mock_logs_valid_json() {
        let logs = generate_mock_logs("test prompt");
        for (i, log) in logs.iter().enumerate() {
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(log);
            assert!(
                parsed.is_ok(),
                "Log entry {} should be valid JSON: {}",
                i,
                log
            );
        }
    }

    #[test]
    fn test_generate_mock_logs_deserializes_to_claudejson() {
        let logs = generate_mock_logs("test prompt");
        for (i, log) in logs.iter().enumerate() {
            let parsed: Result<ClaudeJson, _> = serde_json::from_str(log);
            assert!(
                parsed.is_ok(),
                "Log entry {} should deserialize to ClaudeJson: {} - error: {:?}",
                i,
                log,
                parsed.err()
            );
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn qa_sandbox_probe_runs_inside_prepared_sandbox() {
        use tokio::io::AsyncReadExt as _;

        use crate::{
            env::RepoContext,
            sandbox::{AgentSandboxConfig, SandboxNetworkMode},
        };

        let backend_executable = if cfg!(target_os = "linux") {
            "bwrap"
        } else {
            "sandbox-exec"
        };
        if workspace_utils::shell::resolve_executable_path(backend_executable)
            .await
            .is_none()
        {
            eprintln!("skipping qa sandbox runtime probe: {backend_executable} is unavailable");
            return;
        }

        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join("node_modules")).unwrap();
        std::fs::write(
            repo.join("README.md"),
            "qa repo
",
        )
        .unwrap();

        let mut env = ExecutionEnv::new(
            RepoContext::new(root.path().to_path_buf(), vec!["repo".to_string()]),
            false,
            String::new(),
        )
        .with_sandbox(Some(AgentSandboxConfig {
            enabled: true,
            network: SandboxNetworkMode::None,
            readonly_repo_paths: vec!["node_modules".to_string()],
            ..Default::default()
        }));
        env.insert("VK_QA_MOCK_LOG_DELAY_SECS", "0");

        let mut spawned = QaMockExecutor::default()
            .spawn(&repo, "VK_QA_SANDBOX_PROBE", &env)
            .await
            .unwrap();
        let mut stdout = spawned.child.inner().stdout.take().unwrap();
        let mut output = String::new();
        stdout.read_to_string(&mut output).await.unwrap();
        let status = tokio::time::timeout(std::time::Duration::from_secs(20), spawned.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(
            status.success(),
            "probe process failed: {status:?}
{output}"
        );

        let result_path = repo.join("qa_sandbox_probe_result.json");
        let result = std::fs::read_to_string(&result_path).unwrap();
        assert!(
            result.contains(r#""workspace_write": "allowed""#),
            "{result}"
        );
        assert!(
            result.contains(r#""readonly_path_present": true"#),
            "{result}"
        );
        assert!(result.contains(r#""readonly_write": "denied""#), "{result}");
        assert!(
            !repo
                .join("node_modules/qa_sandbox_probe_readonly_write.txt")
                .exists()
        );
        assert!(repo.join("qa_sandbox_probe_workspace_write.txt").exists());
        assert!(
            result.contains(r#""network": "denied_or_unreachable""#)
                || result.contains(r#""network": "probe_tool_unavailable""#),
            "network should not be allowed when sandbox network mode is none: {result}"
        );
    }

    #[test]
    fn sandbox_probe_script_is_opt_in_and_records_expected_checks() {
        let script = qa_runner_script(
            Path::new("/workspace/repo/.vibe/qa-mock/run/logs.jsonl"),
            true,
            false,
            Path::new("/workspace/repo/.vibe/qa-mock/run/host-command.done"),
            Path::new("/workspace/repo/.vibe/qa-mock/run/host-command-result.txt"),
        );
        assert!(script.contains("qa_sandbox_probe_result.json"));
        assert!(script.contains("qa_sandbox_probe_workspace_write.txt"));
        assert!(script.contains("node_modules/qa_sandbox_probe_readonly_write.txt"));
        assert!(script.contains("https://example.com"));
        assert!(script.contains("logs.jsonl"));
    }

    #[test]
    fn host_command_trigger_script_waits_for_approval_result() {
        let script = qa_runner_script(
            Path::new("/workspace/repo/.vibe/qa-mock/run/logs.jsonl"),
            false,
            true,
            Path::new("/workspace/repo/.vibe/qa-mock/run/host-command.done"),
            Path::new("/workspace/repo/.vibe/qa-mock/run/host-command-result.txt"),
        );
        assert!(script.contains("QA host-command approval requested"));
        assert!(script.contains("host-command.done"));
        assert!(script.contains("host-command-result.txt"));
        assert!(!script.contains("qa_sandbox_probe_result.json"));
    }

    #[test]
    fn qa_marker_detection_accepts_plain_and_markdown_escaped_underscores() {
        assert!(qa_prompt_contains_marker(
            "please run VK_QA_SANDBOX_PROBE now",
            "VK_QA_SANDBOX_PROBE"
        ));
        assert!(qa_prompt_contains_marker(
            r"please run VK\_QA\_SANDBOX\_PROBE now",
            "VK_QA_SANDBOX_PROBE"
        ));
        assert!(qa_prompt_contains_marker(
            r"please run VK\_QA\_HOST\_COMMAND now",
            "VK_QA_HOST_COMMAND"
        ));
        assert!(!qa_prompt_contains_marker(
            r"please run VK\_QA\_UNKNOWN now",
            "VK_QA_HOST_COMMAND"
        ));
    }

    #[derive(Debug)]
    struct FakeHostCommandApprovals {
        request: tokio::sync::Mutex<Option<HostCommandRequest>>,
        result: HostCommandResult,
    }

    #[async_trait]
    impl ExecutorApprovalService for FakeHostCommandApprovals {
        async fn create_tool_approval(
            &self,
            _tool_name: &str,
        ) -> Result<String, crate::approvals::ExecutorApprovalError> {
            unreachable!("qa host-command trigger should not request tool approvals")
        }

        async fn create_question_approval(
            &self,
            _tool_name: &str,
            _question_count: usize,
        ) -> Result<String, crate::approvals::ExecutorApprovalError> {
            unreachable!("qa host-command trigger should not request question approvals")
        }

        async fn wait_tool_approval(
            &self,
            _approval_id: &str,
            _cancel: CancellationToken,
        ) -> Result<ApprovalStatus, crate::approvals::ExecutorApprovalError> {
            unreachable!("qa host-command trigger should not wait for tool approvals")
        }

        async fn create_host_command_approval(
            &self,
            request: HostCommandRequest,
        ) -> Result<String, crate::approvals::ExecutorApprovalError> {
            *self.request.lock().await = Some(request);
            Ok("qa-host-command-approval".to_string())
        }

        async fn wait_host_command_result(
            &self,
            approval_id: &str,
            _cancel: CancellationToken,
        ) -> Result<HostCommandResult, crate::approvals::ExecutorApprovalError> {
            assert_eq!(approval_id, "qa-host-command-approval");
            Ok(self.result.clone())
        }

        async fn wait_question_answer(
            &self,
            _approval_id: &str,
            _cancel: CancellationToken,
        ) -> Result<QuestionStatus, crate::approvals::ExecutorApprovalError> {
            unreachable!("qa host-command trigger should not wait for question approvals")
        }
    }

    #[tokio::test]
    async fn host_command_trigger_requests_first_class_approval_and_records_result() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("repo");
        std::fs::create_dir(&cwd).unwrap();
        let done_file = cwd.join(".vibe/qa-mock/run/host-command.done");
        let result_file = cwd.join(".vibe/qa-mock/run/host-command-result.txt");
        let approvals = Arc::new(FakeHostCommandApprovals {
            request: tokio::sync::Mutex::new(None),
            result: HostCommandResult {
                exit_code: Some(0),
                timed_out: false,
                stdout: "VK_QA_HOST_COMMAND_STDOUT\n".to_string(),
                stderr: String::new(),
                truncated: false,
            },
        });

        spawn_host_command_trigger(
            approvals.clone(),
            cwd.clone(),
            done_file.clone(),
            result_file.clone(),
            CancellationToken::new(),
        );

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !done_file.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("host-command trigger should write completion marker");

        let request = approvals.request.lock().await.clone().unwrap();
        assert_eq!(
            request.command,
            "printf 'VK_QA_HOST_COMMAND_OK\\n' > qa_host_command_approved.txt && printf 'VK_QA_HOST_COMMAND_STDOUT\\n'"
        );
        assert_eq!(request.cwd, cwd.to_string_lossy());
        assert_eq!(
            request.reason,
            "QA mode deterministic host-command approval trigger"
        );
        assert!(request.env.is_empty());
        assert_eq!(request.timeout_secs, 30);
        assert_eq!(request.output_limit_bytes, 4096);

        let result = std::fs::read_to_string(result_file).unwrap();
        assert!(result.contains("QA host-command completed"));
        assert!(result.contains("VK_QA_HOST_COMMAND_STDOUT\\n"));
    }

    #[test]
    fn test_escape_special_characters() {
        let logs = generate_mock_logs("test with \"quotes\" and\nnewlines");
        // The final assistant message (index 8) should contain the prompt
        let final_log = &logs[8];
        let parsed: ClaudeJson = serde_json::from_str(final_log).unwrap();

        if let ClaudeJson::Assistant { message, .. } = parsed {
            if let ClaudeMessageContent::Array(items) = &message.content {
                if let Some(ClaudeContentItem::Text { text }) = items.first() {
                    assert!(text.contains("test with \"quotes\" and\nnewlines"));
                } else {
                    panic!("Expected Text content item");
                }
            } else {
                panic!("Expected Array content");
            }
        } else {
            panic!("Expected Assistant variant");
        }
    }
}
