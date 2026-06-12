use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use derivative::Derivative;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use workspace_utils::msg_store::MsgStore;

use crate::{
    approvals::ExecutorApprovalService,
    command::{CmdOverrides, CommandBuildError, CommandBuilder, apply_overrides},
    env::ExecutionEnv,
    executors::{
        AppendPrompt, BaseCodingAgent, ExecutorError, SpawnedChild, StandardCodingAgentExecutor,
        gemini::AcpAgentHarness,
    },
    profile::ExecutorConfig,
};

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

    fn get_preset_options(&self) -> ExecutorConfig {
        use crate::model_selector::PermissionPolicy;
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
}
