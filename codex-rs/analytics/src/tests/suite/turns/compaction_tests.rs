//! Compaction fact, wire payload, and implementation tests.

use crate::events::CodexCompactionEventRequest;
use crate::events::TrackEventRequest;
use crate::facts::CodexCompactionEvent;
use crate::facts::CompactionImplementation;
use crate::facts::CompactionPhase;
use crate::facts::CompactionReason;
use crate::facts::CompactionStatus;
use crate::facts::CompactionStrategy;
use crate::facts::CompactionTrigger;
use crate::tests::support::sample_app_server_client_metadata;
use crate::tests::support::sample_runtime_metadata;
use codex_login::default_client::DEFAULT_ORIGINATOR;
use codex_protocol::protocol::ThreadSource;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn compaction_event_serializes_expected_shape() {
    let mut event_params = crate::events::codex_compaction_event_params(
        CodexCompactionEvent {
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            trigger: CompactionTrigger::Auto,
            reason: CompactionReason::ContextLimit,
            implementation: CompactionImplementation::ResponsesCompactionV2,
            phase: CompactionPhase::MidTurn,
            strategy: CompactionStrategy::Memento,
            status: CompactionStatus::Completed,
            codex_error_kind: None,
            codex_error_http_status_code: None,
            usage_limit_window_minutes: None,
            active_context_tokens_before: 120_000,
            active_context_tokens_after: 18_000,
            retained_image_count: None,
            compaction_summary_tokens: None,
            cached_input_tokens: None,
            cache_write_input_tokens: Some(456),
            started_at: 100,
            completed_at: 106,
            duration_ms: Some(6543),
        },
        "session-thread-1".to_string(),
        sample_app_server_client_metadata(),
        sample_runtime_metadata(),
        Some(ThreadSource::User),
        /*subagent_source*/ None,
        /*parent_thread_id*/ None,
    );
    let event = TrackEventRequest::Compaction(Box::new(CodexCompactionEventRequest {
        event_type: "codex_compaction_event",
        event_params,
    }));

    let payload = serde_json::to_value(&event).expect("serialize compaction event");

    assert_eq!(
        payload,
        json!({
            "event_type": "codex_compaction_event",
            "event_params": {
                "thread_id": "thread-1",
                "session_id": "session-thread-1",
                "turn_id": "turn-1",
                "app_server_client": {
                    "product_client_id": DEFAULT_ORIGINATOR,
                    "client_name": "codex-tui",
                    "client_version": "1.0.0",
                    "rpc_transport": "stdio",
                    "experimental_api_enabled": true
                },
                "runtime": {
                    "codex_rs_version": "0.1.0",
                    "runtime_os": "macos",
                    "runtime_os_version": "15.3.1",
                    "runtime_arch": "aarch64"
                },
                "thread_source": "user",
                "subagent_source": null,
                "parent_thread_id": null,
                "trigger": "auto",
                "reason": "context_limit",
                "implementation": "responses_compaction_v2",
                "phase": "mid_turn",
                "strategy": "memento",
                "status": "completed",
                "codex_error_kind": null,
                "codex_error_http_status_code": null,
                "usage_limit_window_minutes": null,
                "active_context_tokens_before": 120000,
                "active_context_tokens_after": 18000,
                "retained_image_count": null,
                "compaction_summary_tokens": null,
                "cached_input_tokens": null,
                "cache_write_input_tokens": 456,
                "started_at": 100,
                "completed_at": 106,
                "duration_ms": 6543
            }
        })
    );
}

#[test]
fn compaction_implementation_serializes_remote_v2() {
    let payload = serde_json::to_value(CompactionImplementation::ResponsesCompactionV2)
        .expect("serialize compaction implementation");

    assert_eq!(payload, json!("responses_compaction_v2"));
}
