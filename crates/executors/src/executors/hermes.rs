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

const KNOWN_PROVIDER_LABELS: &[(&str, &str)] = &[
    ("openrouter", "OpenRouter"),
    ("nous", "Nous Portal"),
    ("anthropic", "Anthropic"),
    ("gemini", "Google Gemini"),
    ("openai", "OpenAI"),
    ("openai-api", "OpenAI API"),
    ("openai-codex", "OpenAI Codex"),
    ("copilot", "GitHub Copilot"),
    ("minimax-oauth", "MiniMax OAuth"),
    ("xai", "xAI"),
    ("xai-oauth", "xAI OAuth"),
    ("opencode-go", "OpenCode Go"),
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
            harness = harness.with_model(to_hermes_model_choice(
                model,
                &configured_provider_ids(&self.cmd),
            ));
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
    hermes_home_from_var(std::env::var("HERMES_HOME").ok().as_deref())
}

fn hermes_home_for_cmd(cmd: &CmdOverrides) -> Option<PathBuf> {
    let configured_hermes_home = cmd
        .env
        .as_ref()
        .and_then(|env| env.get("HERMES_HOME"))
        .map(String::as_str);
    if configured_hermes_home.is_some_and(|home| !home.trim().is_empty()) {
        hermes_home_from_var(configured_hermes_home)
    } else {
        hermes_home()
    }
}

fn hermes_home_from_var(hermes_home: Option<&str>) -> Option<PathBuf> {
    if let Some(home) = hermes_home
        && !home.trim().is_empty()
    {
        return Some(PathBuf::from(home));
    }
    dirs::home_dir().map(|home| home.join(".hermes"))
}

fn hermes_config_path_for_cmd(cmd: &CmdOverrides) -> Option<PathBuf> {
    hermes_home_for_cmd(cmd).map(|home| home.join("config.yaml"))
}

fn configured_provider_ids(cmd: &CmdOverrides) -> BTreeSet<String> {
    let Some(content) =
        hermes_config_path_for_cmd(cmd).and_then(|path| std::fs::read_to_string(path).ok())
    else {
        return BTreeSet::new();
    };
    let config = parse_hermes_config(&content);
    let mut providers = BTreeSet::new();
    if let Some(provider) = config.provider.as_deref() {
        providers.insert(normalize_provider(Some(provider)));
    } else if config.model.is_some() {
        providers.insert(normalize_provider(None));
    }
    providers.extend(
        config
            .configured_models
            .iter()
            .map(|(provider, _)| normalize_provider(Some(provider))),
    );
    providers
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
                    "default" | "model" | "name" => {
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
            if let Some(item) = trimmed.strip_prefix("- ")
                && let Some((key, value)) = split_yaml_key_value(item)
                && matches!(key, "name" | "id" | "provider")
                && let Some(provider) = parse_yaml_scalar(value)
            {
                current_provider = Some((indent, provider));
                in_provider_models = false;
                continue;
            }

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
    KNOWN_PROVIDER_LABELS
        .iter()
        .find_map(|(id, label)| (*id == provider).then(|| (*label).to_string()))
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

fn encode_model_selector_choice(provider: &str, model: &str) -> String {
    let model = model.trim();
    if provider.trim().is_empty() {
        model.to_string()
    } else {
        format!("{}/{model}", normalize_provider(Some(provider)))
    }
}

fn to_hermes_model_choice(model_choice: &str, configured_providers: &BTreeSet<String>) -> String {
    let model_choice = model_choice.trim();

    if let Some((provider, model)) = model_choice.split_once('/') {
        let provider = normalize_provider(Some(provider));
        if configured_providers.contains(&provider)
            || (is_known_provider(&provider) && model.contains('/'))
        {
            return format!("{provider}:{model}");
        }
    }

    model_choice.to_string()
}

fn is_known_provider(provider: &str) -> bool {
    KNOWN_PROVIDER_LABELS.iter().any(|(id, _)| *id == provider)
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
    let id = model.trim().to_string();
    if id.is_empty() {
        return;
    }
    let seen_key = encode_model_selector_choice(&provider, &id);
    if !seen_models.insert(seen_key) {
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
    for (provider, _) in KNOWN_PROVIDER_LABELS {
        if config.provider.as_deref().is_some_and(|current| {
            normalize_provider(Some(current)) == normalize_provider(Some(provider))
        }) {
            provider_ids.insert((*provider).to_string());
        }
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

    ModelSelectorConfig {
        providers,
        models,
        default_model: config
            .model
            .as_ref()
            .map(|model| encode_model_selector_choice(&current_provider, model)),
        agents: vec![],
        permissions: vec![PermissionPolicy::Auto, PermissionPolicy::Supervised],
    }
}

async fn run_hermes_acp_check(hermes: &Hermes) -> Option<String> {
    let command_parts = match hermes
        .build_command_builder()
        .and_then(|builder| builder.build_follow_up(&["--check".to_string()]))
    {
        Ok(parts) => parts,
        Err(error) => return Some(format!("Failed to build Hermes ACP check command: {error}")),
    };

    let (program_path, args) = match command_parts.into_resolved().await {
        Ok(resolved) => resolved,
        Err(error) => {
            return Some(format!(
                "Failed to resolve Hermes ACP check command: {error}"
            ));
        }
    };

    let mut command = Command::new(program_path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(env) = &hermes.cmd.env {
        command.envs(env);
    }

    match tokio::time::timeout(HERMES_ACP_CHECK_TIMEOUT, command.output()).await {
        Ok(Ok(output)) if output.status.success() => None,
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let details = if !stderr.is_empty() { stderr } else { stdout };
            Some(if details.is_empty() {
                format!(
                    "Hermes ACP check command failed with status {}",
                    output.status
                )
            } else {
                format!("Hermes ACP check command failed: {details}")
            })
        }
        Ok(Err(error)) => Some(format!("Failed to run Hermes ACP check command: {error}")),
        Err(_) => Some(format!(
            "Hermes ACP check command timed out after {} seconds",
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
        let home = hermes_home_for_cmd(&self.cmd);
        let has_command_override = self
            .cmd
            .base_command_override
            .as_deref()
            .is_some_and(|command| !command.trim().is_empty());
        let installation_found = executable_in_path("hermes") || has_command_override;
        let config_paths = home
            .as_ref()
            .map(|home| vec![home.join("auth.json"), home.join(".env")])
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

        let config_content = hermes_config_path_for_cmd(&self.cmd)
            .and_then(|path| std::fs::read_to_string(path).ok());
        let hermes_config = config_content
            .as_deref()
            .map(parse_hermes_config)
            .unwrap_or_default();
        options.model_selector = build_model_selector_config(&hermes_config);

        let mut errors = Vec::new();
        if let Some(error) = run_hermes_acp_check(self).await {
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
    use std::{
        collections::HashMap,
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn hermes_for_test(cmd: CmdOverrides) -> Hermes {
        Hermes {
            append_prompt: AppendPrompt::default(),
            model: None,
            auto_approve: None,
            cmd,
            approvals: None,
        }
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "vktest-hermes-{name}-{}-{unique}",
            std::process::id()
        ))
    }

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
    fn parses_custom_provider_list_config() {
        let parsed = parse_hermes_config(
            r#"
model:
  provider: openrouter
  name: anthropic/claude-sonnet-4.6
custom_providers:
  - name: local
    base_url: http://localhost:1234/v1
    models:
      - llama:3.3
      - qwen3-coder
"#,
        );

        assert_eq!(parsed.model.as_deref(), Some("anthropic/claude-sonnet-4.6"));
        assert!(
            parsed
                .configured_models
                .contains(&("local".to_string(), "llama:3.3".to_string()))
        );
        assert!(
            parsed
                .configured_models
                .contains(&("local".to_string(), "qwen3-coder".to_string()))
        );
    }

    #[test]
    fn model_selector_uses_vibe_provider_model_ids() {
        let parsed = HermesConfig {
            provider: Some("openrouter".to_string()),
            model: Some("anthropic/claude-sonnet-4.6".to_string()),
            configured_models: vec![("local".to_string(), "qwen3-coder".to_string())],
        };

        let selector = build_model_selector_config(&parsed);

        assert_eq!(
            selector.default_model.as_deref(),
            Some("openrouter/anthropic/claude-sonnet-4.6")
        );
        assert!(selector.models.iter().any(|model| {
            model.id == "anthropic/claude-sonnet-4.6"
                && model.provider_id.as_deref() == Some("openrouter")
        }));
        assert!(selector.models.iter().any(|model| {
            model.id == "qwen3-coder" && model.provider_id.as_deref() == Some("local")
        }));
        assert_eq!(
            selector.permissions,
            vec![PermissionPolicy::Auto, PermissionPolicy::Supervised]
        );
    }

    #[test]
    fn converts_vibe_model_choice_to_hermes_model_choice() {
        let configured_providers = BTreeSet::from(["local".to_string()]);
        assert_eq!(
            to_hermes_model_choice(
                "openrouter/anthropic/claude-sonnet-4.6",
                &configured_providers
            ),
            "openrouter:anthropic/claude-sonnet-4.6"
        );
        assert_eq!(
            to_hermes_model_choice(
                "openrouter/meta-llama/llama-3.1:free",
                &configured_providers
            ),
            "openrouter:meta-llama/llama-3.1:free"
        );
        assert_eq!(
            to_hermes_model_choice(
                "openrouter:anthropic/claude-sonnet-4.6",
                &configured_providers
            ),
            "openrouter:anthropic/claude-sonnet-4.6"
        );
        assert_eq!(
            to_hermes_model_choice("local/qwen3-coder", &configured_providers),
            "local:qwen3-coder"
        );
        assert_eq!(
            to_hermes_model_choice("custom-provider/model-with/slash", &configured_providers),
            "custom-provider/model-with/slash"
        );
        assert_eq!(
            to_hermes_model_choice("anthropic/claude-sonnet-4.6", &BTreeSet::new()),
            "anthropic/claude-sonnet-4.6"
        );
    }

    #[test]
    fn configured_provider_ids_include_default_provider_for_scalar_model() {
        let hermes_home = unique_temp_dir("scalar-model");
        fs::create_dir_all(&hermes_home).expect("create temp Hermes home");
        fs::write(hermes_home.join("config.yaml"), "model: gpt-5\n")
            .expect("write temp Hermes config");

        let cmd = CmdOverrides {
            env: Some(HashMap::from([(
                "HERMES_HOME".to_string(),
                hermes_home.to_string_lossy().into_owned(),
            )])),
            ..Default::default()
        };

        let providers = configured_provider_ids(&cmd);
        assert!(providers.contains("openrouter"));
        assert_eq!(
            to_hermes_model_choice("openrouter/gpt-5", &providers),
            "openrouter:gpt-5"
        );

        let _ = fs::remove_dir_all(hermes_home);
    }

    #[test]
    fn availability_uses_command_override_and_profile_hermes_home() {
        let hermes_home = unique_temp_dir("availability");
        fs::create_dir_all(&hermes_home).expect("create temp Hermes home");
        fs::write(hermes_home.join("auth.json"), "{}").expect("write temp auth");

        let hermes = hermes_for_test(CmdOverrides {
            base_command_override: Some("uvx hermes-acp".to_string()),
            env: Some(HashMap::from([(
                "HERMES_HOME".to_string(),
                hermes_home.to_string_lossy().into_owned(),
            )])),
            ..Default::default()
        });

        assert!(matches!(
            hermes.get_availability_info(),
            AvailabilityInfo::LoginDetected { .. }
        ));

        let _ = fs::remove_dir_all(hermes_home);
    }
}
