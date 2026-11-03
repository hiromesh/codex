//! Approval-time context shared by the host approval pipeline.
//! Automatic Guardian review has been removed; this module retains only the
//! context and permission helpers still used by approvals, hooks, and multi-agent
//! authorization.

mod permissions;
pub(crate) use permissions::for_tool as tool_permission_context;

use codex_protocol::items::ModelInvocationContext;
use codex_protocol::openai_models::CatalogModel;
use std::sync::Arc;

use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::Personality;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::AskForApproval;

use crate::environment_selection::TurnEnvironmentSnapshot;
use crate::session::step_context::StepContext;
use crate::session::step_settings::ResolvedStepSettings;
use crate::session::turn_context::TurnContext;

pub(crate) const AUTO_REVIEW_DENIED_ACTION_APPROVAL_DEVELOPER_PREFIX: &str =
    codex_guardian_context::MANUAL_APPROVAL_DEVELOPER_PREFIX;
pub(crate) const GUARDIAN_MAX_ROOT_MESSAGE_TOKENS: usize = 900;
pub(crate) const GUARDIAN_REVIEWER_NAME: &str = "guardian";

/// Describes the tool call that triggered a network access approval request.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct NetworkAccessTrigger {
    pub(crate) call_id: String,
    pub(crate) tool_name: String,
    pub(crate) command: Vec<String>,
    pub(crate) cwd: codex_utils_path_uri::PathUri,
    pub(crate) sandbox_permissions: crate::sandboxing::SandboxPermissions,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) additional_permissions: Option<codex_protocol::models::AdditionalPermissionProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) justification: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tty: Option<bool>,
}

pub(crate) fn guardian_truncate_text(content: &str, token_cap: usize) -> (String, bool) {
    (
        codex_guardian_context::truncate_text(content, token_cap),
        content.len() > codex_utils_output_truncation::approx_bytes_for_tokens(token_cap),
    )
}

pub(crate) fn is_basic_session_source(
    session_source: &codex_protocol::protocol::SessionSource,
) -> bool {
    use codex_protocol::protocol::InternalSessionSource;
    use codex_protocol::protocol::SessionSource;
    use codex_protocol::protocol::SubAgentSource;

    match session_source {
        SessionSource::SubAgent(SubAgentSource::Other(label)) => label == GUARDIAN_REVIEWER_NAME,
        SessionSource::Internal(InternalSessionSource::Guardian) => true,
        _ => false,
    }
}

/// Captures the context needed by approval routing without retaining MCP bindings
/// or a tool router. Review itself no longer runs in core.
#[derive(Clone)]
pub(crate) struct GuardianReviewContext {
    turn: Arc<TurnContext>,
    environments: TurnEnvironmentSnapshot,
    pub(crate) model_info: Arc<CatalogModel>,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) reasoning_summary: ReasoningSummary,
    pub(crate) personality: Option<Personality>,
    pub(crate) approval_policy: AskForApproval,
    pub(crate) approvals_reviewer: ApprovalsReviewer,
}

impl GuardianReviewContext {
    pub(crate) fn model_context(&self) -> ModelInvocationContext {
        ModelInvocationContext {
            model_slug: self.model_info.slug.clone(),
            reasoning_effort: self
                .reasoning_effort
                .as_ref()
                .or(self.model_info.default_reasoning_level.as_ref())
                .map(ToString::to_string),
        }
    }

    pub(crate) fn from_resolved_settings(
        turn: Arc<TurnContext>,
        settings: &ResolvedStepSettings,
        environments: &TurnEnvironmentSnapshot,
    ) -> Self {
        Self {
            environments: environments.clone(),
            model_info: Arc::clone(&settings.model_info),
            reasoning_effort: settings.reasoning_effort().cloned(),
            reasoning_summary: settings.reasoning_summary,
            personality: settings.personality(),
            approval_policy: settings.approval_policy(),
            approvals_reviewer: settings.approvals_reviewer(),
            turn,
        }
    }

    pub(crate) fn turn(&self) -> &Arc<TurnContext> {
        &self.turn
    }

    pub(crate) fn environments(&self) -> &TurnEnvironmentSnapshot {
        &self.environments
    }
}

impl From<&Arc<StepContext>> for GuardianReviewContext {
    fn from(step: &Arc<StepContext>) -> Self {
        Self {
            turn: Arc::clone(&step.turn),
            environments: step.environments.clone(),
            model_info: Arc::clone(&step.settings.model_info),
            reasoning_effort: step.settings.reasoning_effort().cloned(),
            reasoning_summary: step.settings.reasoning_summary,
            personality: step.settings.personality(),
            approval_policy: step.settings.approval_policy(),
            approvals_reviewer: step.settings.approvals_reviewer(),
        }
    }
}

impl From<Arc<TurnContext>> for GuardianReviewContext {
    fn from(turn: Arc<TurnContext>) -> Self {
        Self {
            environments: turn.initial_environments.clone(),
            model_info: Arc::clone(turn.model_info()),
            reasoning_effort: turn.reasoning_effort().cloned(),
            reasoning_summary: turn.reasoning_summary(),
            personality: turn.personality(),
            approval_policy: turn.approval_policy(),
            approvals_reviewer: turn.config.approvals_reviewer,
            turn,
        }
    }
}

impl From<&Arc<TurnContext>> for GuardianReviewContext {
    fn from(turn: &Arc<TurnContext>) -> Self {
        Self::from(Arc::clone(turn))
    }
}
