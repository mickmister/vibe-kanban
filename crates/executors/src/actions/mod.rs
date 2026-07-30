use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use enum_dispatch::enum_dispatch;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    actions::{
        coding_agent_follow_up::CodingAgentFollowUpRequest,
        coding_agent_initial::CodingAgentInitialRequest, review::ReviewRequest,
        script::ScriptRequest, session_command::CodingAgentSessionCommandRequest,
    },
    approvals::ExecutorApprovalService,
    env::ExecutionEnv,
    executors::{BaseCodingAgent, ExecutorError, SpawnedChild},
};
pub mod coding_agent_follow_up;
pub mod coding_agent_initial;
pub mod review;
pub mod script;
pub mod session_command;

pub use review::RepoReviewContext;

#[enum_dispatch]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(tag = "type")]
pub enum ExecutorActionType {
    CodingAgentInitialRequest,
    CodingAgentFollowUpRequest,
    CodingAgentSessionCommandRequest,
    ScriptRequest,
    ReviewRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ExecutorAction {
    pub typ: ExecutorActionType,
    pub next_action: Option<Box<ExecutorAction>>,
}

impl ExecutorAction {
    pub fn new(typ: ExecutorActionType, next_action: Option<Box<ExecutorAction>>) -> Self {
        Self { typ, next_action }
    }
    pub fn append_action(mut self, action: ExecutorAction) -> Self {
        if let Some(next) = self.next_action {
            self.next_action = Some(Box::new(next.append_action(action)));
        } else {
            self.next_action = Some(Box::new(action));
        }
        self
    }

    pub fn typ(&self) -> &ExecutorActionType {
        &self.typ
    }

    pub fn next_action(&self) -> Option<&ExecutorAction> {
        self.next_action.as_deref()
    }

    pub fn base_executor(&self) -> Option<BaseCodingAgent> {
        match self.typ() {
            ExecutorActionType::CodingAgentInitialRequest(request) => Some(request.base_executor()),
            ExecutorActionType::CodingAgentFollowUpRequest(request) => {
                Some(request.base_executor())
            }
            ExecutorActionType::CodingAgentSessionCommandRequest(request) => {
                Some(request.base_executor())
            }
            ExecutorActionType::ReviewRequest(request) => Some(request.base_executor()),
            ExecutorActionType::ScriptRequest(_) => None,
        }
    }
}

#[async_trait]
#[enum_dispatch(ExecutorActionType)]
pub trait Executable {
    async fn spawn(
        &self,
        current_dir: &Path,
        approvals: Arc<dyn ExecutorApprovalService>,
        env: &ExecutionEnv,
    ) -> Result<SpawnedChild, ExecutorError>;
}

#[async_trait]
impl Executable for ExecutorAction {
    async fn spawn(
        &self,
        current_dir: &Path,
        approvals: Arc<dyn ExecutorApprovalService>,
        env: &ExecutionEnv,
    ) -> Result<SpawnedChild, ExecutorError> {
        self.typ.spawn(current_dir, approvals, env).await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ExecutorAction, ExecutorActionType};
    use crate::sandbox::SandboxNetworkMode;

    #[test]
    fn sandbox_config_serializes_on_initial_and_queued_follow_up_actions() {
        let action: ExecutorAction = serde_json::from_value(json!({
            "typ": {
                "type": "CodingAgentInitialRequest",
                "prompt": "start",
                "executor_config": {
                    "executor": "CLAUDE_CODE",
                    "sandbox": {
                        "enabled": true,
                        "network": "none",
                        "readonly_repo_paths": ["node_modules", "target"]
                    }
                },
                "working_dir": "repo"
            },
            "next_action": {
                "typ": {
                    "type": "CodingAgentFollowUpRequest",
                    "prompt": "continue",
                    "session_id": "session-1",
                    "executor_config": {
                        "executor": "CODEX",
                        "sandbox": {
                            "enabled": true,
                            "network": "inherit"
                        }
                    },
                    "working_dir": "repo"
                },
                "next_action": null
            }
        }))
        .unwrap();

        let ExecutorActionType::CodingAgentInitialRequest(initial) = &action.typ else {
            panic!("expected initial action");
        };
        assert!(initial.executor_config.sandbox.as_ref().unwrap().enabled);
        assert_eq!(
            initial.executor_config.sandbox.as_ref().unwrap().network,
            SandboxNetworkMode::None
        );

        let ExecutorActionType::CodingAgentFollowUpRequest(follow_up) =
            &action.next_action.as_ref().unwrap().typ
        else {
            panic!("expected follow-up action");
        };
        assert!(follow_up.executor_config.sandbox.as_ref().unwrap().enabled);

        let serialized = serde_json::to_value(action).unwrap();
        assert_eq!(
            serialized["next_action"]["typ"]["executor_config"]["sandbox"]["enabled"],
            true
        );
    }
}
