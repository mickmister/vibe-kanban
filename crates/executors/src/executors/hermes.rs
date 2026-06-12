use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

use async_trait::async_trait;
use derivative::Derivative;
use futures::stream::BoxStream;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use ts_rs::TS;
use workspace_utils::msg_store::MsgStore;

use crate::{
    approvals::ExecutorApprovalService,
    command::{CmdOverrides, CommandBuildError, CommandBuilder, apply_overrides},
    env::ExecutionEnv,
    executor_discovery::ExecutorDiscoveredOptions,
    executors::{
        AppendPrompt, AvailabilityInfo, BaseCodingAgent, ExecutorError, SpawnedChild,
        StandardCodingAgentExecutor, gemini::AcpAgentHarness,
    },
    logs::utils::patch,
    model_selector::{ModelInfo, ModelProvider, ModelSelectorConfig, PermissionPolicy},
    profile::ExecutorConfig,
};

const HERMES_ACP_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

const CURATED_MODELS: &[(&str, &str, &[&str])] = &[
    (
        "openrouter",
        "OpenRouter",
        &[
            "anthropic/claude-opus-4.6",
            "anthropic/claude-sonnet-4.6",
            "google/gemini-3-pro-preview",
            "google/gemini-3.1-pro-preview",
            "openai/gpt-5.4",
            "openai/gpt-5.3-codex",
            "x-ai/grok-4.3",
            "moonshotai/kimi-k2.6",
        ],
    ),
    (
        "nous",
        "Nous Portal",
        &[
            "anthropic/claude-opus-4.8",
            "anthropic/claude-sonnet-4.6",
            "openai/gpt-5.5",
            "google/gemini-3-pro-preview",
            "moonshotai/kimi-k2.6",
        ],
    ),
    (
        "anthropic",
        "Anthropic",
        &[
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5",
            "claude-haiku-4-5",
        ],
    ),
    (
        "gemini",
        "Google Gemini",
        &[
            "gemini-3.1-pro-preview",
            "gemini-3-pro-preview",
            "gemini-3.5-flash",
            "gemini-3-flash-preview",
        ],
    ),
    (
        "openai-api",
        "OpenAI API",
        &[
            "gpt-5.5",
            "gpt-5.5-pro",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.3-codex",
            "gpt-4.1",
        ],
    ),
    (
        "copilot",
        "GitHub Copilot",
        &[
            "gpt-5.4",
            "gpt-5.3-codex",
            "claude-sonnet-4.6",
            "gemini-3-pro-preview",
        ],
    ),
    (
        "minimax-oauth",
        "MiniMax OAuth",
        &["MiniMax-M2.7", "MiniMax-M2.7-highspeed"],
    ),
    ("xai", "xAI", &["grok-4", "grok-4-fast"]),
    (
        "opencode-go",
        "OpenCode Go",
        &["claude-sonnet-4.6", "gpt-5.4"],
    ),
];

#[derive(Debug, Default)]
struct HermesConfig {
    provider: Option<String>,
    model: Option<String>,
    configured_models: Vec<(String, String)>,
}

#[derive(Derivative, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[derivative(Debug, PartialEq)]
pub struct Hermes {
    #[serde(default)]
    pub append_prompt: AppendPrompt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_approve: Option<bool>,
    #[serde(flatten)]
    pub cmd: CmdOverrides,
    #[serde(skip)]
    #[ts(skip)]
    #[derivative(Debug = "ignore", PartialEq = "ignore")]
    pub approvals: Option<Arc<dyn ExecutorApprovalService>>,
}

impl Hermes {
    fn build_command_builder(&self) -> Result<CommandBuilder, CommandBuildError> {
        apply_overrides(CommandBuilder::new("hermes acp"), &self.cmd)
    }

    fn build_harness(&self) -> AcpAgentHarness {
        let mut harness = AcpAgentHarness::with_session_namespace("hermes_sessions");
        if let Some(model) = &self.model {
            harness = harness.with_model(model);
        }
        harness
    }

    fn approvals_for_policy(&self) -> Option<Arc<dyn ExecutorApprovalService>> {
        if self.auto_approve.unwrap_or(false) {
            None
        } else {
            self.approvals.clone()
        }
    }
}

fn hermes_home() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HERMES_HOME")
        && !home.trim().is_empty()
    {
        return Some(PathBuf::from(home));
    }
    dirs::home_dir().map(|home| home.join(".hermes"))
}

fn hermes_config_path() -> Option<PathBuf> {
    hermes_home().map(|home| home.join("config.yaml"))
}

fn executable_in_path(binary: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };

    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return true;
        }

        #[cfg(windows)]
        {
            if let Some(pathext) = std::env::var_os("PATHEXT") {
                for ext in std::env::split_paths(&pathext) {
                    let ext = ext.to_string_lossy();
                    let ext = ext.trim_start_matches('.');
                    if dir.join(format!("{binary}.{ext}")).is_file() {
                        return true;
                    }
                }
            }
        }
    }

    false
}

fn latest_auth_timestamp(paths: impl IntoIterator<Item = PathBuf>) -> Option<i64> {
    paths
        .into_iter()
        .filter_map(|path| std::fs::metadata(path).ok())
        .filter_map(|metadata| metadata.modified().ok())
        .filter_map(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .max()
}

fn strip_yaml_comment(line: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    for (idx, ch) in line.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => return &line[..idx],
            _ => {}
        }
    }
    line
}

fn parse_yaml_scalar(raw: &str) -> Option<String> {
    let mut value = raw.trim();
    if value.is_empty() || value == "{}" || value == "[]" {
        return None;
    }
    if matches!(value, "\"\"" | "''" | "null" | "~") {
        return None;
    }
    if (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''))
    {
        value = &value[1..value.len().saturating_sub(1)];
    }
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn split_yaml_key_value(trimmed: &str) -> Option<(&str, &str)> {
    let (key, value) = trimmed.split_once(':')?;
    Some((key.trim(), value.trim()))
}

fn parse_hermes_config(content: &str) -> HermesConfig {
    let mut config = HermesConfig::default();
    let mut in_model = false;
    let mut model_indent = 0;
    let mut in_providers = false;
    let mut providers_indent = 0;
    let mut current_provider: Option<(usize, String)> = None;
    let mut in_provider_models = false;
    let mut provider_models_indent = 0;

    for raw_line in content.lines() {
        let without_comment = strip_yaml_comment(raw_line);
        let trimmed = without_comment.trim();
        if trimmed.is_empty() {
            continue;
        }

        let indent = without_comment.len() - without_comment.trim_start().len();

        if in_model && indent <= model_indent && !trimmed.starts_with('-') {
            in_model = false;
        }
        if in_providers && indent <= providers_indent && !trimmed.starts_with('-') {
            in_providers = false;
            current_provider = None;
            in_provider_models = false;
        }
        if let Some((provider_indent, _)) = &current_provider
            && indent <= *provider_indent
            && !trimmed.starts_with('-')
        {
            current_provider = None;
            in_provider_models = false;
        }
        if in_provider_models && indent <= provider_models_indent && !trimmed.starts_with('-') {
            in_provider_models = false;
        }

        if indent == 0 {
            if let Some((key, value)) = split_yaml_key_value(trimmed) {
                match key {
                    "model" => {
                        if let Some(model) = parse_yaml_scalar(value) {
                            config.model = Some(model);
                        }
                        in_model = value.is_empty();
                        model_indent = indent;
                    }
                    "providers" | "custom_providers" => {
                        in_providers = true;
                        providers_indent = indent;
                    }
                    _ => {}
                }
            }
            continue;
        }

        if in_model {
            if let Some((key, value)) = split_yaml_key_value(trimmed) {
                match key {
                    "default" | "model" => {
                        if let Some(model) = parse_yaml_scalar(value) {
                            config.model = Some(model);
                        }
                    }
                    "provider" => {
                        if let Some(provider) = parse_yaml_scalar(value) {
                            config.provider = Some(provider);
                        }
                    }
                    _ => {}
                }
            }
        }

        if in_providers {
            if !in_provider_models
                && let Some((key, value)) = split_yaml_key_value(trimmed)
                && value.is_empty()
            {
                if indent == providers_indent + 2 {
                    current_provider = Some((indent, key.to_string()));
                } else if key == "models" {
                    in_provider_models = true;
                    provider_models_indent = indent;
                }
            } else if let Some((key, value)) = split_yaml_key_value(trimmed)
                && key == "models"
                && value.is_empty()
            {
                in_provider_models = true;
                provider_models_indent = indent;
            }

            if in_provider_models
                && indent > provider_models_indent
                && let Some((_, provider)) = &current_provider
            {
                if let Some(item) = trimmed.strip_prefix("- ") {
                    if let Some(model) = parse_yaml_scalar(item) {
                        config.configured_models.push((provider.clone(), model));
                    }
                } else if let Some((key, _)) = split_yaml_key_value(trimmed)
                    && !key.is_empty()
                {
                    config
                        .configured_models
                        .push((provider.clone(), key.to_string()));
                }
            }
        }
    }

    config
}

fn normalize_provider(provider: Option<&str>) -> String {
    match provider
        .unwrap_or("openrouter")
        .trim()
        .to_lowercase()
        .as_str()
    {
        "" | "auto" => "openrouter".to_string(),
        "openai" => "openai-api".to_string(),
        "google" | "google-ai" => "gemini".to_string(),
        other => other.to_string(),
    }
}

fn provider_label(provider: &str) -> String {
    CURATED_MODELS
        .iter()
        .find_map(|(id, label, _)| (*id == provider).then(|| (*label).to_string()))
        .unwrap_or_else(|| {
            provider
                .split(['-', '_'])
                .filter(|part| !part.is_empty())
                .map(|part| {
                    let mut chars = part.chars();
                    match chars.next() {
                        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
}

fn encode_model_choice(provider: &str, model: &str) -> String {
    let model = model.trim();
    if provider.trim().is_empty() {
        model.to_string()
    } else {
        format!("{}:{model}", normalize_provider(Some(provider)))
    }
}

fn model_display_name(model: &str) -> String {
    model
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(model)
        .to_string()
}

fn add_model_info(
    models: &mut Vec<ModelInfo>,
    seen_models: &mut BTreeSet<String>,
    provider: &str,
    model: &str,
) {
    let provider = normalize_provider(Some(provider));
    let id = encode_model_choice(&provider, model);
    if !seen_models.insert(id.clone()) {
        return;
    }
    models.push(ModelInfo {
        id,
        name: model_display_name(model),
        provider_id: Some(provider),
        reasoning_options: vec![],
    });
}

fn build_model_selector_config(config: &HermesConfig) -> ModelSelectorConfig {
    let current_provider = normalize_provider(config.provider.as_deref());
    let mut provider_ids = BTreeSet::new();
    provider_ids.insert(current_provider.clone());
    for (provider, _) in &config.configured_models {
        provider_ids.insert(normalize_provider(Some(provider)));
    }
    for (provider, _, _) in CURATED_MODELS {
        provider_ids.insert((*provider).to_string());
    }

    let providers = provider_ids
        .iter()
        .map(|provider| ModelProvider {
            id: provider.clone(),
            name: provider_label(provider),
        })
        .collect::<Vec<_>>();

    let mut seen_models = BTreeSet::new();
    let mut models = Vec::new();

    if let Some(model) = &config.model {
        add_model_info(&mut models, &mut seen_models, &current_provider, model);
    }

    for (provider, model) in &config.configured_models {
        add_model_info(&mut models, &mut seen_models, provider, model);
    }

    for (provider, _, provider_models) in CURATED_MODELS {
        if *provider == current_provider {
            for model in *provider_models {
                add_model_info(&mut models, &mut seen_models, provider, model);
            }
        }
    }

    let only_default_model = models.len() <= usize::from(config.model.is_some());
    if only_default_model {
        for (provider, _, provider_models) in CURATED_MODELS {
            for model in *provider_models {
                add_model_info(&mut models, &mut seen_models, provider, model);
            }
        }
    }

    ModelSelectorConfig {
        providers,
        models,
        default_model: config
            .model
            .as_ref()
            .map(|model| encode_model_choice(&current_provider, model)),
        agents: vec![],
        permissions: vec![PermissionPolicy::Auto, PermissionPolicy::Supervised],
    }
}

async fn run_hermes_acp_check() -> Option<String> {
    if !executable_in_path("hermes") {
        return Some("Hermes executable not found in PATH. Install Hermes and run `hermes model` to configure a provider.".to_string());
    }

    let mut command = Command::new("hermes");
    command
        .args(["acp", "--check"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    match tokio::time::timeout(HERMES_ACP_CHECK_TIMEOUT, command.output()).await {
        Ok(Ok(output)) if output.status.success() => None,
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let details = if !stderr.is_empty() { stderr } else { stdout };
            Some(if details.is_empty() {
                format!("`hermes acp --check` failed with status {}", output.status)
            } else {
                format!("`hermes acp --check` failed: {details}")
            })
        }
        Ok(Err(error)) => Some(format!("Failed to run `hermes acp --check`: {error}")),
        Err(_) => Some(format!(
            "`hermes acp --check` timed out after {} seconds",
            HERMES_ACP_CHECK_TIMEOUT.as_secs()
        )),
    }
}

#[async_trait]
impl StandardCodingAgentExecutor for Hermes {
    fn apply_overrides(&mut self, executor_config: &ExecutorConfig) {
        if let Some(model_id) = &executor_config.model_id {
            self.model = Some(model_id.clone());
        }

        if let Some(permission_policy) = executor_config.permission_policy.clone() {
            self.auto_approve = Some(matches!(
                permission_policy,
                crate::model_selector::PermissionPolicy::Auto
            ));
        }
    }

    fn use_approvals(&mut self, approvals: Arc<dyn ExecutorApprovalService>) {
        self.approvals = Some(approvals);
    }

    async fn spawn(
        &self,
        current_dir: &Path,
        prompt: &str,
        env: &ExecutionEnv,
    ) -> Result<SpawnedChild, ExecutorError> {
        let hermes_command = self.build_command_builder()?.build_initial()?;
        let combined_prompt = self.append_prompt.combine_prompt(prompt);
        self.build_harness()
            .spawn_with_command(
                current_dir,
                combined_prompt,
                hermes_command,
                env,
                &self.cmd,
                self.approvals_for_policy(),
            )
            .await
    }

    async fn spawn_follow_up(
        &self,
        current_dir: &Path,
        prompt: &str,
        session_id: &str,
        _reset_to_message_id: Option<&str>,
        env: &ExecutionEnv,
    ) -> Result<SpawnedChild, ExecutorError> {
        let hermes_command = self.build_command_builder()?.build_follow_up(&[])?;
        let combined_prompt = self.append_prompt.combine_prompt(prompt);
        self.build_harness()
            .spawn_follow_up_with_command(
                current_dir,
                combined_prompt,
                session_id,
                hermes_command,
                env,
                &self.cmd,
                self.approvals_for_policy(),
            )
            .await
    }

    fn normalize_logs(
        &self,
        msg_store: Arc<MsgStore>,
        worktree_path: &Path,
    ) -> Vec<tokio::task::JoinHandle<()>> {
        crate::executors::acp::normalize_logs(msg_store, worktree_path)
    }

    fn default_mcp_config_path(&self) -> Option<std::path::PathBuf> {
        None
    }

    fn get_availability_info(&self) -> AvailabilityInfo {
        let home = hermes_home();
        let installation_found = executable_in_path("hermes");
        let config_paths = home
            .as_ref()
            .map(|home| {
                vec![
                    home.join("auth.json"),
                    home.join(".env"),
                    home.join("config.yaml"),
                ]
            })
            .unwrap_or_default();

        if installation_found && let Some(timestamp) = latest_auth_timestamp(config_paths) {
            return AvailabilityInfo::LoginDetected {
                last_auth_timestamp: timestamp,
            };
        }

        if installation_found || home.is_some_and(|path| path.exists()) {
            AvailabilityInfo::InstallationFound
        } else {
            AvailabilityInfo::NotFound
        }
    }

    fn get_preset_options(&self) -> ExecutorConfig {
        ExecutorConfig {
            executor: BaseCodingAgent::Hermes,
            variant: None,
            model_id: self.model.clone(),
            agent_id: None,
            reasoning_id: None,
            permission_policy: Some(if self.auto_approve.unwrap_or(false) {
                PermissionPolicy::Auto
            } else {
                PermissionPolicy::Supervised
            }),
        }
    }

    async fn discover_options(
        &self,
        _workdir: Option<&std::path::Path>,
        _repo_path: Option<&std::path::Path>,
    ) -> Result<BoxStream<'static, json_patch::Patch>, ExecutorError> {
        let mut options = ExecutorDiscoveredOptions {
            model_selector: ModelSelectorConfig {
                permissions: vec![PermissionPolicy::Auto, PermissionPolicy::Supervised],
                ..Default::default()
            },
            ..Default::default()
        };

        let config_content =
            hermes_config_path().and_then(|path| std::fs::read_to_string(path).ok());
        let hermes_config = config_content
            .as_deref()
            .map(parse_hermes_config)
            .unwrap_or_default();
        options.model_selector = build_model_selector_config(&hermes_config);

        let mut errors = Vec::new();
        if let Some(error) = run_hermes_acp_check().await {
            errors.push(error);
        }
        if hermes_config.model.is_none() {
            errors.push(
                "No Hermes main model found in config.yaml. Run `hermes model` to choose a provider and model."
                    .to_string(),
            );
        }
        if !errors.is_empty() {
            options.error = Some(errors.join("\n"));
        }

        Ok(Box::pin(futures::stream::once(async move {
            patch::executor_discovered_options(options)
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_selector::PermissionPolicy;

    #[test]
    fn builds_default_hermes_acp_command() {
        let hermes = Hermes {
            append_prompt: AppendPrompt::default(),
            model: None,
            auto_approve: None,
            cmd: CmdOverrides::default(),
            approvals: None,
        };

        let builder = hermes.build_command_builder().unwrap();
        assert_eq!(builder.base, "hermes acp");
        assert!(builder.params.is_none());
    }

    #[test]
    fn permission_policy_override_controls_auto_approve() {
        let mut hermes = Hermes {
            append_prompt: AppendPrompt::default(),
            model: None,
            auto_approve: None,
            cmd: CmdOverrides::default(),
            approvals: None,
        };

        hermes.apply_overrides(&ExecutorConfig {
            executor: BaseCodingAgent::Hermes,
            variant: None,
            model_id: Some("openrouter/test-model".to_string()),
            agent_id: None,
            reasoning_id: None,
            permission_policy: Some(PermissionPolicy::Auto),
        });

        assert_eq!(hermes.model.as_deref(), Some("openrouter/test-model"));
        assert_eq!(hermes.auto_approve, Some(true));

        hermes.apply_overrides(&ExecutorConfig {
            executor: BaseCodingAgent::Hermes,
            variant: None,
            model_id: None,
            agent_id: None,
            reasoning_id: None,
            permission_policy: Some(PermissionPolicy::Supervised),
        });

        assert_eq!(hermes.auto_approve, Some(false));
    }

    #[test]
    fn parses_hermes_model_config() {
        let parsed = parse_hermes_config(
            r#"
model:
  provider: openrouter
  default: "anthropic/claude-sonnet-4.6"
providers:
  local:
    base_url: http://localhost:1234/v1
    models:
      - qwen3-coder
      llama-coder:
        timeout_seconds: 120
"#,
        );

        assert_eq!(parsed.provider.as_deref(), Some("openrouter"));
        assert_eq!(parsed.model.as_deref(), Some("anthropic/claude-sonnet-4.6"));
        assert!(
            parsed
                .configured_models
                .contains(&("local".to_string(), "qwen3-coder".to_string()))
        );
        assert!(
            parsed
                .configured_models
                .contains(&("local".to_string(), "llama-coder".to_string()))
        );
    }

    #[test]
    fn model_selector_uses_provider_encoded_ids() {
        let parsed = HermesConfig {
            provider: Some("openrouter".to_string()),
            model: Some("anthropic/claude-sonnet-4.6".to_string()),
            configured_models: vec![("local".to_string(), "qwen3-coder".to_string())],
        };

        let selector = build_model_selector_config(&parsed);

        assert_eq!(
            selector.default_model.as_deref(),
            Some("openrouter:anthropic/claude-sonnet-4.6")
        );
        assert!(selector.models.iter().any(|model| {
            model.id == "openrouter:anthropic/claude-sonnet-4.6"
                && model.provider_id.as_deref() == Some("openrouter")
        }));
        assert!(selector.models.iter().any(|model| {
            model.id == "local:qwen3-coder" && model.provider_id.as_deref() == Some("local")
        }));
        assert_eq!(
            selector.permissions,
            vec![PermissionPolicy::Auto, PermissionPolicy::Supervised]
        );
    }
}
