use super::*;
use crate::session::tests::make_session_and_context_with_rx;
use crate::session::tests::test_environment_id;
use codex_protocol::approvals::NetworkPolicyAmendment;
use pretty_assertions::assert_eq;

#[test]
fn approval_resolution_rejects_denied_network_policy_amendment() {
    let resolution = ApprovalResolution {
        decision: ReviewDecision::NetworkPolicyAmendment {
            network_policy_amendment: NetworkPolicyAmendment {
                host: "denied.example.com".to_string(),
                action: NetworkPolicyRuleAction::Deny,
            },
        },
        source: ApprovalResolutionSource::User,
        cancellation_reason: None,
    };

    assert!(matches!(
        resolution.into_tool_result(),
        Err(ToolError::Rejected(rejection)) if rejection == "rejected by user"
    ));
}

#[test]
fn approval_resolution_rejects_mcp_policy_amendment() {
    let resolution = ApprovalResolution {
        decision: ReviewDecision::ApprovedMcpPolicyAmendment,
        source: ApprovalResolutionSource::User,
        cancellation_reason: None,
    };

    assert!(matches!(
        resolution.into_tool_result(),
        Err(ToolError::Rejected(rejection)) if rejection == "Error while requesting approval"
    ));
}

#[test]
fn approval_resolution_aborts_turn_when_approval_is_aborted() {
    let resolution = ApprovalResolution {
        decision: ReviewDecision::Abort,
        source: ApprovalResolutionSource::User,
        cancellation_reason: None,
    };

    assert!(matches!(
        resolution.into_tool_result(),
        Err(ToolError::Codex(error))
            if matches!(
                error.details(),
                codex_protocol::error::CodexErrorDetails::TurnAborted
            )
    ));
}

#[test]
fn network_hook_cwd_overrides_only_local_commands() {
    let local_cwd = AbsolutePathBuf::try_from(std::env::temp_dir()).unwrap();
    for (environment_id, uri) in [
        (codex_exec_server::LOCAL_ENVIRONMENT_ID, None),
        (
            codex_exec_server::LOCAL_ENVIRONMENT_ID,
            Some("file:///local/repo"),
        ),
        ("remote", None),
        ("remote", Some("file:///remote/repo")),
        ("remote", Some("file:///C:/remote/repo")),
    ] {
        let expected_cwd = uri.map_or_else(
            || PathUri::from_abs_path(&local_cwd),
            |uri| PathUri::parse(uri).unwrap(),
        );
        let action = ApprovalAction::NetworkAccess {
            id: "network-1".into(),
            turn_id: "turn-1".into(),
            environment_id: environment_id.into(),
            target: "https://example.com:443".into(),
            host: "example.com".into(),
            protocol: NetworkApprovalProtocol::Https,
            port: 443,
            trigger: uri.map(|_| NetworkAccessTrigger {
                call_id: "exec-1".into(),
                tool_name: "exec_command".into(),
                command: vec!["curl".into(), "https://example.com".into()],
                cwd: expected_cwd.clone(),
                sandbox_permissions: SandboxPermissions::UseDefault,
                additional_permissions: None,
                justification: None,
                tty: None,
            }),
            hook_command: "curl https://example.com".into(),
            hook_run_id: "hook-1".into(),
            command: vec!["network-access".into(), "https://example.com:443".into()],
            cwd: local_cwd.clone(),
        };
        assert_eq!(
            action.permission_request_payload(),
            PermissionRequestPayload {
                tool_name: HookToolName::bash(),
                tool_input: serde_json::json!({
                    "command": "curl https://example.com",
                    "description": "network-access https://example.com:443",
                }),
                tool_cwd: (environment_id == codex_exec_server::LOCAL_ENVIRONMENT_ID)
                    .then_some(expected_cwd),
            }
        );
    }
}
