//! Central approval policy-stage execution and reviewer routing.
//! Managed human checkpoints cannot be satisfied by hooks, automated review, or reusable approvals.

use crate::command_canonicalization::canonicalize_command_for_approval;
use crate::exec_policy::prompt_is_rejected_by_policy;
use crate::guardian::GuardianReviewContext;
use crate::guardian::NetworkAccessTrigger;
use crate::hook_runtime::run_permission_request_hooks;
use crate::mcp_tool_call::request_mcp_tool_user_approval;
use crate::sandboxing::SandboxPermissions;
use crate::session::NetworkApprovalTurn;
use crate::session::session::Session;
use crate::tools::hook_names::HookToolName;
use crate::tools::runtimes::apply_patch::ApplyPatchApprovalKey;
use crate::tools::runtimes::unified_exec::UnifiedExecApprovalKey;
use crate::tools::sandboxing::ApprovalRequestReasons;
use crate::tools::sandboxing::ExecApprovalRequirement;
use crate::tools::sandboxing::PermissionRequestPayload;
use crate::tools::sandboxing::ToolError;
use crate::tools::sandboxing::with_cached_approval;
use codex_config::types::AppToolApproval;
use codex_hooks::PermissionRequestDecision;
use codex_otel::ToolDecisionSource;
use codex_prompts::ResolvedModelMessages;
use codex_protocol::approvals::ExecApprovalKind;
use codex_protocol::approvals::ExecPolicyAmendment;
use codex_protocol::approvals::NetworkApprovalContext;
use codex_protocol::approvals::NetworkApprovalProtocol;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::error::CodexErr;
use codex_protocol::mcp_approval_meta::McpApprovalCancellationReason;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::FileChange;
use codex_protocol::protocol::NetworkPolicyRuleAction;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::request_permissions::RequestPermissionProfile;
use codex_tools::ToolName;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathConvention;
use codex_utils_path_uri::PathUri;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::error;
use tracing::warn;

#[derive(Clone)]
pub(crate) struct ApprovalContext {
    /// Preserve the command's policy result through review and retries. Other actions
    /// use their existing reviewer routing without an execution policy requirement.
    pub(crate) exec_requirement: Option<ExecApprovalRequirement>,
    pub(crate) review_context: GuardianReviewContext,
    pub(crate) cancellation_token: Option<CancellationToken>,
    pub(crate) call_id: String,
    pub(crate) tool_name: ToolName,
    pub(crate) strict_auto_review: bool,
    pub(crate) approval_reason: Option<String>,
    pub(crate) retry_reason: Option<String>,
    pub(crate) network_approval_context: Option<NetworkApprovalContext>,
}

impl ApprovalContext {
    fn requires_human_approval(&self) -> bool {
        matches!(
            self.exec_requirement,
            Some(ExecApprovalRequirement::NeedsHumanApproval { .. })
        )
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub(crate) enum ApprovalAction {
    ExecCommand {
        id: String,
        environment_id: String,
        command: Vec<String>,
        #[serde(skip_serializing)]
        hook_command: String,
        cwd: PathUri,
        sandbox_permissions: SandboxPermissions,
        additional_permissions: Option<AdditionalPermissionProfile>,
        justification: Option<String>,
        tty: bool,
        proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
    },
    WriteStdin {
        id: String,
        approval_id: String,
        environment_id: String,
        process_id: i32,
        original_command: String,
        input: String,
        cwd: PathUri,
        tty: bool,
        sandbox_permissions: SandboxPermissions,
        additional_permissions: Option<AdditionalPermissionProfile>,
        #[serde(skip)]
        additional_permissions_preapproved: bool,
    },
    ApplyPatch {
        id: String,
        environment_id: String,
        cwd: PathUri,
        files: Vec<PathUri>,
        patch: String,
        #[serde(skip_serializing)]
        changes: Arc<HashMap<PathBuf, FileChange>>,
        permissions_preapproved: bool,
    },
    McpToolCall {
        id: String,
        environment_id: String,
        server: String,
        tool_name: String,
        arguments: Option<serde_json::Value>,
        connector_id: Option<String>,
        connector_name: Option<String>,
        connector_description: Option<String>,
        connected_account_email: Option<String>,
        tool_title: Option<String>,
        tool_description: Option<String>,
        #[serde(skip_serializing)]
        hook_tool_name: HookToolName,
        approval_policy: AskForApproval,
        approval_mode: AppToolApproval,
        allow_session_remember: bool,
        allow_persistent_approval: bool,
    },
    NetworkAccess {
        id: String,
        turn_id: String,
        environment_id: String,
        target: String,
        host: String,
        protocol: NetworkApprovalProtocol,
        port: u16,
        trigger: Option<NetworkAccessTrigger>,
        #[serde(skip_serializing)]
        hook_command: String,
        #[serde(skip_serializing)]
        hook_run_id: String,
        command: Vec<String>,
        cwd: AbsolutePathBuf,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub(crate) enum ApprovalCacheKey {
    ExecCommand(UnifiedExecApprovalKey),
    ApplyPatch(ApplyPatchApprovalKey),
}

impl ApprovalAction {
    fn hook_environment_id(&self) -> &str {
        match self {
            Self::ExecCommand { environment_id, .. }
            | Self::WriteStdin { environment_id, .. }
            | Self::ApplyPatch { environment_id, .. }
            | Self::McpToolCall { environment_id, .. }
            | Self::NetworkAccess { environment_id, .. } => environment_id,
        }
    }

    pub(crate) fn permission_request_payload(&self) -> PermissionRequestPayload {
        let mut payload = match self {
            Self::ExecCommand {
                hook_command,
                justification,
                cwd,
                ..
            } => PermissionRequestPayload::bash(
                hook_command.clone(),
                justification.clone(),
                cwd.clone(),
            ),
            Self::WriteStdin {
                id,
                approval_id,
                environment_id,
                process_id,
                original_command: _,
                input,
                cwd,
                tty,
                sandbox_permissions,
                additional_permissions,
                additional_permissions_preapproved: _,
            } => PermissionRequestPayload {
                tool_name: HookToolName::new("write_stdin"),
                tool_cwd: Some(cwd.clone()),
                tool_input: serde_json::json!({
                    "session_id": process_id,
                    "chars": input,
                    "parent_call_id": id,
                    "approval_id": approval_id,
                    "environment_id": environment_id,
                    "cwd": cwd,
                    "tty": tty,
                    "sandbox_permissions": sandbox_permissions,
                    "additional_permissions": additional_permissions,
                }),
            },
            Self::ApplyPatch { patch, .. } => PermissionRequestPayload {
                tool_name: HookToolName::apply_patch(),
                tool_cwd: None,
                tool_input: serde_json::json!({ "command": patch }),
            },
            Self::McpToolCall {
                hook_tool_name,
                arguments,
                ..
            } => PermissionRequestPayload {
                tool_name: hook_tool_name.clone(),
                tool_cwd: None,
                tool_input: arguments
                    .clone()
                    .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
            },
            Self::NetworkAccess {
                hook_command,
                target,
                cwd,
                trigger,
                ..
            } => PermissionRequestPayload::bash(
                hook_command.clone(),
                Some(format!("network-access {target}")),
                trigger.as_ref().map_or_else(
                    || PathUri::from_abs_path(cwd),
                    |trigger| trigger.cwd.clone(),
                ),
            ),
        };
        // Command hooks run on the host. Keep remote paths in the action's
        // original tool input, but never use them as the hook's top-level cwd.
        if matches!(
            self,
            Self::ExecCommand { environment_id, .. }
                | Self::WriteStdin { environment_id, .. }
                | Self::NetworkAccess { environment_id, .. }
                if environment_id != codex_exec_server::LOCAL_ENVIRONMENT_ID
        ) {
            payload.tool_cwd = None;
        }
        payload
    }

    pub(crate) fn cache_keys(&self) -> Vec<ApprovalCacheKey> {
        match self {
            Self::ExecCommand {
                environment_id,
                command,
                cwd,
                tty,
                sandbox_permissions,
                additional_permissions,
                ..
            } => vec![ApprovalCacheKey::ExecCommand(UnifiedExecApprovalKey {
                environment_id: environment_id.clone(),
                executable: command.first().cloned(),
                command: canonicalize_command_for_approval(command),
                cwd: cwd.clone(),
                tty: *tty,
                sandbox_permissions: *sandbox_permissions,
                additional_permissions: additional_permissions.clone(),
            })],
            Self::McpToolCall { .. }
            | Self::NetworkAccess { .. }
            | Self::WriteStdin { .. } => Vec::new(),
            Self::ApplyPatch {
                environment_id,
                files,
                ..
            } => files
                .iter()
                .cloned()
                .map(|path| {
                    ApprovalCacheKey::ApplyPatch(ApplyPatchApprovalKey {
                        environment_id: environment_id.clone(),
                        path,
                    })
                })
                .collect(),
        }
    }


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApprovalResolutionSource {
    Hook,
    User,
}

/// Preserves the approval path even when the decision becomes a tool error.
/// The source identifies the reviewer, not the actor that caused cancellation.
#[must_use]
pub(crate) struct ApprovalResult {
    pub(crate) result: Result<ReviewDecision, ToolError>,
    pub(crate) source: Option<ApprovalResolutionSource>,
    pub(crate) cancellation_reason: Option<McpApprovalCancellationReason>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApprovalResolution {
    pub(crate) decision: ReviewDecision,
    pub(crate) source: ApprovalResolutionSource,
    pub(crate) cancellation_reason: Option<McpApprovalCancellationReason>,
}

impl ApprovalResolution {
    pub(super) fn user(decision: ReviewDecision) -> Self {
        Self {
            decision,
            source: ApprovalResolutionSource::User,
            cancellation_reason: None,
        }
    }

    fn into_tool_result(self) -> Result<ReviewDecision, ToolError> {
        let source = self.source;
        match self.decision {
            ReviewDecision::ApprovedMcpPolicyAmendment => {
                error!("Tool approval received ApprovedMcpPolicyAmendment");
                Err(ToolError::Rejected(
                    "Error while requesting approval".to_string(),
                ))
            }
            ReviewDecision::NetworkPolicyAmendment {
                network_policy_amendment,
            } if network_policy_amendment.action == NetworkPolicyRuleAction::Deny => {
                let rejection = match source {
                    ApprovalResolutionSource::Hook => "rejected by configuration",
                    ApprovalResolutionSource::User => "rejected by user",
                };
                Err(ToolError::Rejected(rejection.to_string()))
            }
            ReviewDecision::Denied { rejection } => Err(ToolError::Rejected(rejection)),
            ReviewDecision::TimedOut => Err(ToolError::Rejected(
                "approval request timed out".to_string(),
            )),
            ReviewDecision::Abort => Err(ToolError::Codex(CodexErr::TurnAborted)),
            decision => Ok(decision),
        }
    }
}

impl Session {
    async fn check_approval_requirement(&self, ctx: &ApprovalContext) -> Result<(), ToolError> {
        if ctx.requires_human_approval() {
            if let Some(reason) = prompt_is_rejected_by_policy(
                ctx.review_context.approval_policy,
                /*prompt_is_rule*/ true,
            ) {
                return Err(ToolError::Rejected(reason.to_string()));
            }
            let config = self.get_config().await;
            let requirements = config.config_layer_stack.requirements();
            if ctx.strict_auto_review
                || ctx.review_context.turn().strict_auto_review_enabled()
                || requirements
                    .approvals_reviewer
                    .can_set(&ApprovalsReviewer::User)
                    .is_err()
                || requirements.auto_review_required_for_model(&ctx.review_context.model_info.slug)
            {
                return Err(ToolError::Rejected("Managed command policy requires human approval, but this session requires automatic review.".to_string()));
            }
        }
        Ok(())
    }

    pub(crate) async fn request_approval(
        self: &Arc<Self>,
        action: ApprovalAction,
        ctx: ApprovalContext,
    ) -> ApprovalResult {
        self.request_approval_with_user_prompt(
            &action,
            &ctx,
            self.request_user_approval(&action, &ctx),
        )
        .await
    }

    pub(super) async fn request_approval_with_user_prompt(
        self: &Arc<Self>,
        action: &ApprovalAction,
        ctx: &ApprovalContext,
        user_prompt: impl Future<Output = ApprovalResolution>,
    ) -> ApprovalResult {
        if let Err(err) = self.check_approval_requirement(ctx).await {
            return ApprovalResult {
                result: Err(err),
                source: None,
                cancellation_reason: None,
            };
        }
        // Retained authority outside current grants needs a new sandbox approval.
        // Current grants permit review of the input; they never approve the input itself.
        let policy = ctx.review_context.turn().approval_policy();
        if matches!(action, ApprovalAction::WriteStdin { sandbox_permissions, additional_permissions_preapproved, .. }
            if sandbox_permissions.requests_sandbox_override()
                && (!additional_permissions_preapproved || matches!(policy, AskForApproval::Never)))
            && !(ctx.strict_auto_review && matches!(policy, AskForApproval::Never))
            && let Some(reason) =
                prompt_is_rejected_by_policy(policy, /*prompt_is_rule*/ false)
        {
            return ApprovalResult {
                result: Err(ToolError::Rejected(reason.to_string())),
                source: None,
                cancellation_reason: None,
            };
        }
        let is_mcp_tool_call = matches!(action, ApprovalAction::McpToolCall { .. });
        let is_network_approval = matches!(action, ApprovalAction::NetworkAccess { .. });
        let permission_request_run_id = match action {
            ApprovalAction::NetworkAccess { hook_run_id, .. } => hook_run_id.clone(),
            _ if ctx.retry_reason.is_some() => format!("{}:retry", ctx.call_id),
            _ => ctx.call_id.clone(),
        };

        // Hook denials always win. Managed checkpoints require a person even
        // when a hook allows the action or the session uses automatic review.
        let mut resolution = match run_permission_request_hooks(
            self,
            &ctx.review_context,
            &permission_request_run_id,
            action.permission_request_payload(),
            action.hook_environment_id(),
        )
        .await
        {
            Some(PermissionRequestDecision::Allow) if !ctx.requires_human_approval() => {
                ApprovalResolution {
                    decision: ReviewDecision::Approved,
                    source: ApprovalResolutionSource::Hook,
                    cancellation_reason: None,
                }
            }
            Some(PermissionRequestDecision::Deny { message }) => ApprovalResolution {
                decision: ReviewDecision::denied(message),
                source: ApprovalResolutionSource::Hook,
                cancellation_reason: None,
            },
            Some(PermissionRequestDecision::Allow) | None => user_prompt.await,
        };
        if resolution.decision == ReviewDecision::Approved {
            if let Err(ToolError::Rejected(reason)) = self.check_approval_requirement(ctx).await {
                resolution.decision = ReviewDecision::denied(reason);
            }
            if ctx.requires_human_approval()
                && ctx
                    .cancellation_token
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
            {
                resolution.decision = ReviewDecision::Abort;
            }
        }
        // Network approvals record their final telemetry after validation and persistence.
        if !is_network_approval {
            record_resolution(ctx, &resolution);
        }
        let source = Some(resolution.source);
        let cancellation_reason = resolution.cancellation_reason;
        if is_mcp_tool_call && resolution.decision == ReviewDecision::ApprovedMcpPolicyAmendment {
            return ApprovalResult {
                result: Ok(resolution.decision),
                source,
                cancellation_reason,
            };
        }
        if is_network_approval {
            match (&resolution.decision, resolution.source) {
                (
                    ReviewDecision::NetworkPolicyAmendment {
                        network_policy_amendment,
                    },
                    _,
                ) if network_policy_amendment.action == NetworkPolicyRuleAction::Deny => {
                    return ApprovalResult {
                        result: Ok(resolution.decision),
                        source,
                        cancellation_reason,
                    };
                }
                _ => {}
            }
        }
        ApprovalResult {
            result: resolution.into_tool_result(),
            source,
            cancellation_reason,
        }
    }

    pub(super) async fn request_user_approval(
        &self,
        action: &ApprovalAction,
        ctx: &ApprovalContext,
    ) -> ApprovalResolution {
        let decision = match action {
            ApprovalAction::ExecCommand {
                environment_id,
                command,
                cwd,
                additional_permissions,
                justification,
                proposed_execpolicy_amendment,
                ..
            } => {
                let reason = ctx
                    .retry_reason
                    .clone()
                    .or_else(|| ctx.approval_reason.clone())
                    .or_else(|| justification.clone());
                let human_once = ctx.requires_human_approval();
                let request = || async {
                    self.request_command_approval(
                        ctx.review_context.turn(),
                        if human_once {
                            ExecApprovalKind::HumanCommand
                        } else {
                            ExecApprovalKind::Command
                        },
                        ctx.review_context.model_context(),
                        ctx.call_id.clone(),
                        Some(&ctx.call_id),
                        human_once.then(|| uuid::Uuid::new_v4().to_string()),
                        Some(environment_id.clone()),
                        command.clone(),
                        cwd.clone(),
                        reason,
                        if human_once {
                            None
                        } else {
                            ctx.network_approval_context
                                .clone()
                                .map(|context| (context, NetworkApprovalTurn::AnyActive))
                        },
                        if human_once {
                            None
                        } else {
                            proposed_execpolicy_amendment.clone()
                        },
                        additional_permissions.clone(),
                        human_once.then(|| {
                            vec![
                                ReviewDecision::Approved,
                                ReviewDecision::denied("rejected by user"),
                                ReviewDecision::Abort,
                            ]
                        }),
                        /*plugin_attribution_override*/ None,
                    )
                    .await
                };
                if human_once {
                    request().await
                } else {
                    let tool_name = "unified_exec";
                    let policy_fingerprint = ctx
                        .review_context
                        .environments()
                        .turn_environments()
                        .find(|environment| environment.selection.environment_id == *environment_id)
                        .and_then(|environment| environment.config().exec_policy.as_ref())
                        .map(codex_execpolicy::RequirementsExecPolicy::fingerprint);
                    let cache_keys = action
                        .cache_keys()
                        .into_iter()
                        .map(|key| (key, &policy_fingerprint))
                        .collect();
                    with_cached_approval(&self.services, tool_name, cache_keys, request).await
                }
            }
            ApprovalAction::WriteStdin {
                id,
                approval_id,
                environment_id,
                process_id,
                input,
                cwd,
                additional_permissions,
                ..
            } => {
                self.request_command_approval(
                    ctx.review_context.turn(),
                    ExecApprovalKind::WriteStdin,
                    ctx.review_context.model_context(),
                    id.clone(),
                    Some(&ctx.call_id),
                    Some(approval_id.clone()),
                    Some(environment_id.clone()),
                    vec![
                        "write_stdin".to_string(),
                        "--session-id".to_string(),
                        process_id.to_string(),
                        input.clone(),
                    ],
                    cwd.clone(),
                    ctx.approval_reason.clone(),
                    /*network_approval*/ None,
                    /*proposed_execpolicy_amendment*/ None,
                    additional_permissions.clone(),
                    Some(vec![ReviewDecision::Approved, ReviewDecision::Abort]),
                    /*plugin_attribution_override*/ None,
                )
                .await
            }
            ApprovalAction::ApplyPatch {
                changes,
                permissions_preapproved,
                ..
            } => {
                let reason = ctx
                    .retry_reason
                    .clone()
                    .or_else(|| ctx.approval_reason.clone());
                if *permissions_preapproved && reason.is_none() {
                    ReviewDecision::Approved
                } else if reason.is_some() {
                    self.request_patch_approval(
                        ctx.review_context.turn(),
                        ctx.call_id.clone(),
                        changes.as_ref().clone(),
                        reason,
                        /*grant_root*/ None,
                    )
                    .await
                } else {
                    with_cached_approval(
                        &self.services,
                        "apply_patch",
                        action.cache_keys(),
                        || async {
                            self.request_patch_approval(
                                ctx.review_context.turn(),
                                ctx.call_id.clone(),
                                changes.as_ref().clone(),
                                /*reason*/ None,
                                /*grant_root*/ None,
                            )
                            .await
                        },
                    )
                    .await
                }
            }
            ApprovalAction::McpToolCall { .. } => {
                return request_mcp_tool_user_approval(
                    self,
                    ctx.review_context.turn(),
                    &ctx.call_id,
                    action,
                )
                .await;
            }
            ApprovalAction::NetworkAccess {
                environment_id,
                command,
                cwd,
                trigger,
                ..
            } => {
                self.request_command_approval(
                    ctx.review_context.turn(),
                    ExecApprovalKind::Command,
                    ctx.review_context.model_context(),
                    ctx.call_id.clone(),
                    trigger.as_ref().map(|trigger| trigger.call_id.as_str()),
                    /*approval_id*/ None,
                    Some(environment_id.clone()),
                    command.clone(),
                    cwd.clone().into(),
                    ctx.approval_reason.clone(),
                    ctx.network_approval_context
                        .clone()
                        .map(|context| (context, NetworkApprovalTurn::Originating)),
                    /*proposed_execpolicy_amendment*/ None,
                    /*additional_permissions*/ None,
                    /*available_decisions*/ None,
                    /*plugin_attribution_override*/ None,
                )
                .await
            }
        };
        ApprovalResolution::user(decision)
    }
}

fn record_resolution(ctx: &ApprovalContext, resolution: &ApprovalResolution) {
    let source = match resolution.source {
        ApprovalResolutionSource::Hook => ToolDecisionSource::Config,
        ApprovalResolutionSource::User => ToolDecisionSource::User,
    };
    ctx.review_context.turn().session_telemetry.tool_decision(
        &ctx.tool_name,
        &ctx.call_id,
        &resolution.decision,
        Some(source),
    );
}

#[cfg(all(test, unix))]
#[path = "approvals_tests.rs"]
mod tests;
