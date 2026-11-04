use super::*;

fn annotations(
    read_only: Option<bool>,
    destructive: Option<bool>,
    open_world: Option<bool>,
) -> ToolAnnotations {
    ToolAnnotations::from_raw(
        /*title*/ None,
        read_only,
        destructive,
        /*idempotent_hint*/ None,
        open_world,
    )
}

#[test]
fn approval_required_when_read_only_false_and_destructive() {
    let annotations = annotations(Some(false), Some(true), /*open_world*/ None);
    assert_eq!(requires_mcp_tool_approval(Some(&annotations)), true);
}

#[test]
fn approval_required_when_read_only_false_and_open_world() {
    let annotations = annotations(Some(false), /*destructive*/ None, Some(true));
    assert_eq!(requires_mcp_tool_approval(Some(&annotations)), true);
}

#[test]
fn approval_required_when_destructive_even_if_read_only_true() {
    let annotations = annotations(Some(true), Some(true), Some(true));
    assert_eq!(requires_mcp_tool_approval(Some(&annotations)), true);
}

#[test]
fn approval_required_when_annotations_are_absent() {
    assert_eq!(requires_mcp_tool_approval(/*annotations*/ None), true);
}

#[test]
fn approval_not_required_when_read_only_and_other_hints_are_absent() {
    let annotations = annotations(
        Some(true),
        /*destructive*/ None,
        /*open_world*/ None,
    );
    assert_eq!(requires_mcp_tool_approval(Some(&annotations)), false);
}

#[test]
fn writes_mode_requires_approval_for_non_read_only_tools() {
    let annotations = annotations(Some(false), Some(false), Some(false));
    assert_eq!(
        requires_mcp_tool_approval_for_mode(Some(&annotations), AppToolApproval::Writes),
        true
    );
    assert_eq!(
        requires_mcp_tool_approval_for_mode(/*annotations*/ None, AppToolApproval::Writes),
        true
    );
}

#[test]
fn writes_mode_does_not_require_approval_for_read_only_tools() {
    let annotations = annotations(Some(true), Some(true), Some(true));
    assert_eq!(
        requires_mcp_tool_approval_for_mode(Some(&annotations), AppToolApproval::Writes),
        false
    );
}

#[test]
fn prompting_modes_do_not_allow_persistent_remember() {
    for approval_mode in [AppToolApproval::Prompt, AppToolApproval::Writes] {
        assert_eq!(
            normalize_approval_decision_for_mode(ReviewDecision::ApprovedForSession, approval_mode,),
            ReviewDecision::Approved
        );
        assert_eq!(
            normalize_approval_decision_for_mode(
                ReviewDecision::ApprovedMcpPolicyAmendment,
                approval_mode,
            ),
            ReviewDecision::Approved
        );
    }
}
