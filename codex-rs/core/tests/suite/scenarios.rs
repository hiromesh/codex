//! Multi-turn Astra scenarios snapshot the model-visible request history of shipped features.

#[path = "scenarios_turn_input_agent_message.rs"]
mod turn_input_agent_message;

use super::image_rollout::RecordingFileAttachmentStore;
use super::permissions_messages::PermissionPathScenario;
use codex_attachment_store::AttachmentStoreErrorKind;
use codex_protocol_test_support::ContentItemTestExt;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

use anyhow::Result;
use base64::Engine;
use codex_analytics::CompactionTrigger;
use codex_config::ConfigLayerSource;
use codex_config::ConfigLayerStack;
use codex_context_fragments::AnsweredQuestion;
use codex_context_fragments::ContextualUserFragment;
use codex_core::ForkSnapshot;
use codex_core::StartThreadOptions;
use codex_core::TurnInput;
use codex_core::TurnInputRequest;
use codex_core::compact::AutoCompactionDisabled;
use codex_core::config::Config;
use codex_core::config::TokenBudgetConfig;
use codex_exec_server::CreateDirectoryOptions;
use codex_exec_server::EnvironmentManager;
use codex_extension_api::ExtensionRegistry;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_features::Feature;
use codex_history::RolloutItem;
use codex_login::CodexAuth;
use codex_models_manager::bundled_models_response;
use codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem;
use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
use codex_protocol::dynamic_tools::DynamicToolResponse;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::items::AgentMessageDelivery;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ImageDetail;
use codex_protocol::models::ImageReference;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ToolMessage;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::AdditionalContextEntry;
use codex_protocol::protocol::AdditionalContextKind;
use codex_protocol::protocol::EnvironmentConfigState;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::TurnEnvironmentSelection;
use codex_protocol::protocol::TurnSettingsUpdate;
use codex_protocol::protocol::TurnSettingsUpdateOutcome;
use codex_protocol::request_user_input::RequestUserInputAnswer;
use codex_protocol::request_user_input::RequestUserInputResponse;
use codex_protocol::user_input::UserInput;
use codex_skills_extension::ExecutorSkillProvider;
use codex_skills_extension::SkillProviders;
use codex_skills_extension::SkillsExtensionConfig;
use codex_skills_extension::install;
use codex_skills_extension::install_with_providers;
use codex_utils_path_uri::PathUri;
use core_test_support::context_snapshot;
use core_test_support::context_snapshot::ContextSnapshotOptions;
use core_test_support::context_snapshot::SnapshotEntry;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_message_item_added;
use core_test_support::responses::ev_output_text_delta;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::stdio_server_bin;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::environment_config_for_selection;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use core_test_support::wait_for_mcp_server;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::oneshot;

const ONE_PIXEL_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

#[path = "scenarios_dynamic_tools.rs"]
mod dynamic_tools;

#[path = "scenarios_retained_tool_outputs.rs"]
mod retained_tool_outputs;

#[path = "scenarios_model_instructions.rs"]
mod model_instructions;

#[path = "scenarios_catalog_approval_instructions_tests.rs"]
mod catalog_approval_instructions;

#[path = "scenarios_managed_human_approval_tests.rs"]
mod managed_human_approval;

#[path = "scenarios_context_management_tools.rs"]
mod context_management_tools;

#[path = "scenarios_agent_message_board.rs"]
mod agent_message_board;

#[path = "scenarios_mailbox_preemption_tests.rs"]
mod mailbox_preemption;

#[path = "scenarios_agent_eviction_tests.rs"]
mod agent_eviction;

#[path = "scenarios_reserved_spawn.rs"]
mod reserved_spawn;

#[path = "scenarios_partial_answers.rs"]
mod partial_answers;

#[path = "scenarios_mcp_resource_messages.rs"]
mod mcp_resource_messages;

#[path = "scenarios_mcp_sharing.rs"]
mod mcp_sharing;

#[path = "scenarios_preparation.rs"]
mod preparation;

#[path = "scenarios_content_filter.rs"]
mod content_filter;

#[path = "scenarios_provider_capabilities.rs"]
mod provider_capabilities;

#[path = "scenarios_web_search.rs"]
mod web_search;

#[path = "scenarios_shared_instructions.rs"]
mod shared_instructions;

#[path = "scenarios_tools_namespace_budget.rs"]
mod tools_namespace_budget;

#[path = "scenarios_skill_catalog_dedup.rs"]
mod skill_catalog_dedup;

#[path = "scenarios_world_state_prefix_tests.rs"]
mod world_state_prefix;

fn skills_extensions() -> Arc<ExtensionRegistry<Config>> {
    let mut extensions = ExtensionRegistryBuilder::<Config>::new();
    install(&mut extensions, |config: &Config| SkillsExtensionConfig {
        include_instructions: config.include_skill_instructions,
        catalog_deltas_enabled: config.features.enabled(Feature::SkillCatalogDeltas),
        omit_descriptions: config
            .features
            .enabled(codex_features::Feature::OmitSkillDescriptions),
        max_context_tokens: config.skill_max_context_tokens,
        bundled_skills_enabled: config.bundled_skills_enabled(),
        cloud_skill_enabled: config.cloud_skill_enabled,
        shadow_selection_enabled: config.features.enabled(Feature::SkillSearch),
    });
    Arc::new(extensions.build())
}

fn text(value: &str) -> UserInput {
    UserInput::Text {
        text: value.to_string(),
        text_elements: Vec::new(),
    }
}

fn configure_scenario_catalog(config: &mut Config) {
    // Keep the fixture independent of the checkout's project configuration.
    let stack = &config.config_layer_stack;
    config.config_layer_stack = ConfigLayerStack::new(
        stack
            .all_layers_low_to_high()
            .filter(|layer| !matches!(&layer.name, ConfigLayerSource::Project { .. }))
            .cloned()
            .collect(),
        stack.requirements().clone(),
        stack.requirements_toml().clone(),
    )
    .expect("fixture config layers")
    .with_cloud_config_binding(stack.cloud_config_binding().cloned());
    config.model_catalog = Some(bundled_models_response().expect("bundled model catalog"));
    config.cloud_skill_enabled = false;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_prompt_id_reaches_responses() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let test = test_codex()
        .with_config(|config| {
            configure_scenario_catalog(config);
            config.include_environment_context = false;
        })
        .build_with_auto_env(&server)
        .await?;
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![ev_completed("first")]),
            sse(vec![ev_completed("second")]),
        ],
    )
    .await;
    let uuid = uuid::Uuid::new_v4();
    for id in [Some(uuid), None] {
        let mut request = TurnInputRequest::user_input(vec![text("Repeat this input.")]);
        request.user_item_uuid = id;
        test.codex.start_or_steer_turn(request).await?;
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await;
    }

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let reserved = codex_protocol::ResponseItemId::for_admitted_user_input(uuid);
    for (index, request) in requests.iter().enumerate() {
        let input = request.input();
        let prompts: Vec<_> = input
            .iter()
            .filter(|item| {
                item["role"] == "user" && item["content"][0]["text"] == "Repeat this input."
            })
            .collect();
        assert_eq!(prompts.len(), index + 1);
        assert_eq!(prompts[0]["id"], reserved.as_str());
        if let Some(legacy) = prompts.get(1) {
            let id = legacy["id"].as_str().expect("legacy message id");
            assert!(id.starts_with("msg_") && !id.starts_with("msg_input_"));
        }
    }
    insta::assert_snapshot!(context_snapshot::format_request_history_snapshot(
        "An admitted prompt and an ordinary prompt with identical text reach Responses on successive turns.",
        &requests,
        &ContextSnapshotOptions::default().rewrite_known_segments(),
    ));
    Ok(())
}

#[test_case::test_case(false; "messages")]
#[test_case::test_case(true; "tool_outputs")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn additional_context_tool_outputs(tool_outputs: bool) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let test = test_codex()
        .with_config(move |config| {
            configure_scenario_catalog(config);
            config.include_environment_context = false;
            config
                .features
                .set_enabled(Feature::UntrustedAsToolOutput, tool_outputs)
                .expect("test config should allow feature update");
        })
        .build_with_auto_env(&server)
        .await?;
    let mock = mount_sse_sequence(
        &server,
        (0..4)
            .map(|index| sse(vec![ev_completed(&format!("response-{index}"))]))
            .collect(),
    )
    .await;

    for (prompt, name, value) in [
        (
            "Summarize the selected document.",
            Some("browser"),
            "The launch is on Monday.",
        ),
        (
            "The same document is now open in the editor.",
            Some("editor"),
            "The launch is on Monday.",
        ),
        (
            "Check the date again.",
            Some("editor"),
            "The launch is on Monday.",
        ),
        (
            "Summarize the updated document.",
            None,
            "The launch is on Tuesday.",
        ),
    ] {
        test.codex
            .start_or_steer_turn(
                TurnInputRequest::user_input(vec![text(prompt)]).with_additional_context(
                    std::collections::BTreeMap::from([
                        (
                            "selection".to_owned(),
                            AdditionalContextEntry {
                                value: value.to_owned(),
                                kind: AdditionalContextKind::Untrusted,
                                name: name.map(str::to_owned),
                            },
                        ),
                        (
                            "application".to_owned(),
                            AdditionalContextEntry::new(
                                "Keep the answer brief.".to_owned(),
                                AdditionalContextKind::Application,
                            ),
                        ),
                    ]),
                ),
            )
            .await?;
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await;
    }
    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    if tool_outputs {
        assert_eq!(
            requests
                .iter()
                .map(|request| request.inputs_of_type("function_call_output").len())
                .collect::<Vec<_>>(),
            vec![1, 2, 2, 3]
        );
        assert_eq!(requests[3].message_input_texts("user").len(), 4);
        let input = requests[0].input();
        let context_index = input
            .iter()
            .position(|item| item["type"] == "function_call_output")
            .expect("context output");
        let user_index = input
            .iter()
            .position(|item| item["role"] == "user")
            .expect("user message");
        assert!(context_index < user_index);
    } else {
        assert!(
            requests
                .iter()
                .all(|request| request.inputs_of_type("function_call_output").is_empty())
        );
        assert_eq!(
            requests
                .iter()
                .map(|request| request.message_input_texts("user").len())
                .collect::<Vec<_>>(),
            vec![2, 3, 4, 6]
        );
    }
    insta::assert_snapshot!(
        if tool_outputs {
            "additional_context_tool_outputs"
        } else {
            "additional_context_legacy_messages"
        },
        context_snapshot::format_request_history_snapshot(
            "Named context changes are appended before the user; unchanged values are deduplicated.",
            &requests,
            &ContextSnapshotOptions::default().rewrite_known_segments(),
        )
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_images_are_rechecked_without_rewriting_history() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let image_store = Arc::new(RecordingFileAttachmentStore {
        resolve_barrier: Some(Arc::new(tokio::sync::Barrier::new(/*n*/ 3))),
        ..Default::default()
    });
    let test = test_codex()
        .with_model("gpt-6-astra")
        .with_image_store(image_store.clone())
        .with_config(configure_scenario_catalog)
        .build_with_auto_env(&server)
        .await?;
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("first", "The images are available."),
                ev_completed("first"),
            ]),
            sse(vec![
                ev_assistant_message("second", "Some images are no longer available."),
                ev_completed("second"),
            ]),
        ],
    )
    .await;
    let file_ids = [
        "file_expiring",
        "file_available",
        "file_expiring",
        "file_backend_error",
    ];
    let mut input = vec![text("Compare these images.")];
    input.extend(file_ids.map(|file_id| UserInput::Image {
        image: ImageReference::File {
            file_id: file_id.to_string(),
        },
        detail: Some(ImageDetail::High),
    }));
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(input))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(
        *image_store.resolves.lock().expect("resolves lock"),
        HashMap::from([
            ("file_expiring".to_string(), 1),
            ("file_available".to_string(), 1),
            ("file_backend_error".to_string(), 1),
        ])
    );

    image_store
        .resolve_errors
        .lock()
        .expect("resolve errors lock")
        .extend([
            (
                "file_expiring".to_string(),
                AttachmentStoreErrorKind::NotFound,
            ),
            (
                "file_backend_error".to_string(),
                AttachmentStoreErrorKind::Backend,
            ),
        ]);
    test.submit_text_turn("Compare them again.").await?;
    assert_eq!(
        *image_store.resolves.lock().expect("resolves lock"),
        HashMap::from([
            ("file_expiring".to_string(), 2),
            ("file_available".to_string(), 2),
            ("file_backend_error".to_string(), 2),
        ])
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .body_json()
            .to_string()
            .contains("file_expiring")
    );
    let unavailable_parts = requests[1]
        .input()
        .into_iter()
        .filter_map(|item| {
            item["internal_chat_message_metadata_passthrough"]["content_item_kinds"]
                .as_array()
                .cloned()
        })
        .flatten()
        .filter(|kind| kind == "images.unavailable")
        .count();
    assert_eq!(unavailable_parts, 2);
    assert!(requests[1].body_contains_text("[image unavailable]"));
    assert!(
        !requests[1]
            .body_json()
            .to_string()
            .contains("file_expiring")
    );
    assert!(
        requests[1]
            .body_json()
            .to_string()
            .contains("file_backend_error")
    );
    assert!(
        requests[1]
            .body_json()
            .to_string()
            .contains("file_available")
    );

    let recorded_images = test
        .codex
        .load_history(/*include_archived*/ false)
        .await?
        .items
        .into_iter()
        .filter_map(|item| match item {
            RolloutItem::ResponseItem(envelope) => match envelope.item {
                ResponseItem::Message { content, .. } => Some(content),
                _ => None,
            },
            _ => None,
        })
        .flatten()
        .filter(|content| matches!(content, ContentItem::InputImage { .. }))
        .collect::<Vec<_>>();
    assert_eq!(
        recorded_images,
        file_ids.map(|file_id| ContentItem::InputImage {
            image: ImageReference::File {
                file_id: file_id.to_string()
            },
            detail: Some(ImageDetail::High),
        })
    );
    insta::assert_snapshot!(context_snapshot::format_request_history_snapshot(
        "Available file references pass through. A later request replaces missing files, preserves references on backend errors, rechecks each distinct ID once, and keeps canonical history intact.",
        &requests,
        &ContextSnapshotOptions::default().rewrite_known_segments(),
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_tool_discovery_in_same_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let requests = super::rmcp_client::mcp_tool_discovery_scenario(
        /*invalidation*/ None, /*enabled*/ true,
    )
    .await?;
    let mut bodies = requests
        .iter()
        .map(core_test_support::responses::ResponsesRequest::body_json)
        .collect::<Vec<_>>();
    // Cargo and Bazel can serialize embedded JSON in different key orders.
    // Normalize each output text before snapshot truncation and hashing.
    for body in &mut bodies {
        for item in body["input"].as_array_mut().expect("request input") {
            if item["type"] != "function_call_output" && item["type"] != "custom_tool_call_output" {
                continue;
            }
            let parts = match &mut item["output"] {
                serde_json::Value::Array(parts) => parts
                    .iter_mut()
                    .map(|part| &mut part["text"])
                    .collect::<Vec<_>>(),
                output => vec![output],
            };
            for part in parts {
                let text = part.as_str().expect("text output");
                let mut normalized = context_snapshot::normalize_json_lines(text);
                if text.ends_with('\n') {
                    normalized.push('\n');
                }
                *part = normalized.into();
            }
        }
    }
    let entries = bodies.iter().map(SnapshotEntry::body).collect::<Vec<_>>();
    insta::assert_snapshot!(
        format!("mcp_tool_discovery_Direct"),
        context_snapshot::format_context_snapshot(
            "An MCP discovery call announces a new tool, which is called in the same turn.",
            &entries,
            &ContextSnapshotOptions::default()
                .rewrite_known_segments()
                .include_request_settings(),
        )
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn astra_asks_an_async_question_and_receives_the_answer_while_working() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let question = "Who should receive the launch update?";
    let parameters = json!({
        "type": "object",
        "properties": {
            "questions": {
                "type": "array",
                "description": "Catalog questions to ask while continuing work.",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": {"type": "string", "description": "Catalog question title."},
                        "options": {"type": "array", "items": {"type": "string"}}
                    },
                    "required": ["title"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["questions"],
        "additionalProperties": false
    });
    let catalog_parameters = parameters.to_string();
    let (release_continuation, continuation_gate) = oneshot::channel();
    let mut working_message = ev_assistant_message("working", "I drafted a short launch update.");
    working_message["item"]["phase"] = json!("commentary");
    let (streaming, _completions) = start_streaming_sse_server(vec![
        vec![StreamingSseChunk {
            gate: None,
            body: sse(vec![
                ev_response_created("question-response"),
                ev_function_call_with_namespace(
                    "audience-question",
                    "functions",
                    "request_user_input_async",
                    &json!({"questions": [{"title": question, "options": ["Internal team", "Customers"]}]}).to_string(),
                ),
                ev_completed("question-response"),
            ]),
        }],
        vec![
            StreamingSseChunk {
                gate: None,
                body: sse(vec![ev_response_created("working-response"), working_message]),
            },
            StreamingSseChunk {
                gate: Some(continuation_gate),
                body: sse(vec![ev_completed("working-response")]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: sse(vec![
                ev_response_created("answered-response"),
                ev_assistant_message("answered", "Here is the launch update for customers."),
                ev_completed("answered-response"),
            ]),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: sse(vec![
                ev_response_created("follow-up-response"),
                ev_assistant_message("follow-up", "The email subject is: Launch update."),
                ev_completed("follow-up-response"),
            ]),
        }],
    ])
    .await;
    let config_server = start_mock_server().await;
    let base_url = format!("{}/v1", streaming.uri());
    let test = test_codex()
        .with_model("gpt-6-astra")
        .with_auth(CodexAuth::from_api_key("test-api-key"))
        .with_config(move |config| {
            configure_scenario_catalog(config);
            config
                .model_catalog
                .as_mut()
                .expect("scenario model catalog")
                .models
                .iter_mut()
                .find(|model| model.slug == "gpt-6-astra")
                .expect("Astra model")
                .model_messages
                .as_mut()
                .expect("model messages")
                .tools
                .get_or_insert_with(Default::default)
                .send_user_message_async = Some(ToolMessage {
                parameters: Some(catalog_parameters),
                ..Default::default()
            });
            config.model_provider.base_url = Some(base_url);
            // The gated mock records raw request bodies for the shared snapshot renderer.
            config
                .features
                .disable(Feature::EnableRequestCompression)
                .expect("disable compression for the gated mock");
        })
        .build_with_auto_env(&config_server)
        .await?;

    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![text(
            "Draft a short launch update. Ask me who it is for and keep working while I answer.",
        )]))
        .await?;
    let turn_id = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::TurnStarted(event) => Some(event.turn_id.clone()),
        _ => None,
    })
    .await;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ItemCompleted(event)
            if matches!(&event.item, TurnItem::AgentMessage(message)
                if message.delivery == Some(AgentMessageDelivery::Async)))
    })
    .await;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 10),
        streaming.wait_for_request_count(/*count*/ 2),
    )
    .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ItemCompleted(event)
            if matches!(&event.item, TurnItem::AgentMessage(message) if message.id == "working"))
    })
    .await;

    let question_id = json!(["request_user_input_async", "audience-question", 0]).to_string();
    let answer = AnsweredQuestion::new(&question_id, question, "Customers")
        .render()
        .expect_text();
    test.codex
        .steer_turn(TurnInputRequest::user_input(vec![text(&answer)]), turn_id)
        .await?;
    release_continuation.send(()).expect("release continuation");
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    test.submit_turn("Now give it an email subject.").await?;

    let requests = streaming
        .requests()
        .await
        .iter()
        .map(|body| serde_json::from_slice(body))
        .collect::<serde_json::Result<Vec<serde_json::Value>>>()?;
    for request in &requests {
        let tool = request["input"][0]["tools"]
            .as_array()
            .expect("Responses Lite tools")
            .iter()
            .flat_map(|namespace| namespace["tools"].as_array().into_iter().flatten())
            .find(|tool| tool["name"] == "request_user_input_async")
            .expect("the async question tool should be directly visible to the model");
        assert_eq!(tool["parameters"], parameters);
    }
    let entries = requests.iter().map(SnapshotEntry::body).collect::<Vec<_>>();
    insta::assert_snapshot!(
        "astra_async_question_and_answer",
        context_snapshot::format_context_snapshot(
            "Astra uses the catalog question schema to ask who a launch update is for, keeps working, and receives the user's answer in the active turn.",
            &entries,
            &ContextSnapshotOptions::default().rewrite_known_segments(),
        )
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn astra_switches_environments_for_the_rest_of_the_active_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call_with_namespace(
                    "pause-for-switch",
                    "functions",
                    "request_user_input",
                    &json!({"questions": [{
                        "id": "continue", "header": "Continue", "question": "Continue in the other workspace?",
                        "options": [
                            {"label": "Yes", "description": "Continue working."},
                            {"label": "No", "description": "Stop working."}
                        ]
                    }]}).to_string(),
                ),
                ev_completed("first-step"),
            ]),
            sse(vec![
                ev_assistant_message("active-turn", "I continued in the other workspace."),
                ev_completed("second-step"),
            ]),
            sse(vec![
                ev_assistant_message("next-turn", "I am back in the original workspace."),
                ev_completed("next-turn"),
            ]),
        ],
    )
    .await;
    let test = test_codex()
        .with_model("gpt-6-astra")
        .with_auth(CodexAuth::from_api_key("test-api-key"))
        .with_config(|config| {
            configure_scenario_catalog(config);
            config.workspace_roots = vec![config.cwd.clone()];
            config
                .features
                .enable(Feature::DefaultModeRequestUserInput)
                .expect("enable request_user_input");
        })
        .build_with_auto_env(&server)
        .await?;
    let original = test.executor_environment().selection().clone();
    let mut other = original.clone();
    other.cwd = original.cwd.join("other-workspace")?;
    other.workspace_roots = vec![other.cwd.clone()];
    other.config =
        EnvironmentConfigState::Ready(environment_config_for_selection(&test.config, &other));
    test.fs()
        .create_directory(
            &other.cwd,
            CreateDirectoryOptions {
                recursive: false,
                follow_symlinks: true,
            },
            /*sandbox*/ None,
        )
        .await?;

    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![text(
            "Ask before continuing in the other workspace.",
        )]))
        .await?;
    let request = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::RequestUserInput(request) => Some(request.clone()),
        _ => None,
    })
    .await;
    let (reply, outcome) = oneshot::channel();
    test.codex
        .submit(Op::TurnSettings {
            turn_id: request.turn_id.clone(),
            update: TurnSettingsUpdate {
                environments: Some(vec![other.clone().into_request()]),
                ..Default::default()
            },
            reply,
        })
        .await?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(/*secs*/ 10), outcome).await??,
        TurnSettingsUpdateOutcome::Applied
    );
    test.codex
        .submit(Op::UserInputAnswer {
            id: request.turn_id,
            response: RequestUserInputResponse {
                answers: HashMap::from([(
                    "continue".to_string(),
                    RequestUserInputAnswer {
                        answers: vec!["Yes".to_string()],
                    },
                )]),
            },
        })
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    test.submit_text_turn("Which workspace are you using now?")
        .await?;

    let requests = mock.requests();
    insta::assert_snapshot!(
        "astra_active_environment_selection",
        context_snapshot::format_request_history_snapshot(
            "Astra starts in one workspace, switches for the next step of the active turn, and returns to the original workspace on the next turn.",
            &requests,
            &ContextSnapshotOptions::default(),
        )
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_completion_wakes_parent_after_final_answer() -> Result<()> {
    use core_test_support::ThreadIdle;
    use core_test_support::responses;
    use wiremock::matchers::header;

    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut extensions = ExtensionRegistryBuilder::<Config>::new();
    extensions.thread_lifecycle_contributor(Arc::new(ThreadIdle));
    let mut test = test_codex()
        .with_model("gpt-6-astra")
        .with_extensions(Arc::new(extensions.build()))
        .with_config(|config| {
            configure_scenario_catalog(config);
            config.model_reasoning_effort = Some(ReasoningEffort::Medium);
            config.include_environment_context = false;
            for feature in [
                Feature::Collab,
                Feature::MultiAgentV2,
                Feature::MultiAgentV2DynamicTools,
                Feature::WakeOnAgentMessages,
            ] {
                config.features.enable(feature).unwrap();
            }
        })
        .build_with_auto_env(&server)
        .await?;
    // A pending client tool holds the real child result until the parent is idle.
    test.codex.shutdown_and_wait().await?;
    let root = test
        .thread_manager
        .start_thread(StartThreadOptions {
            dynamic_tools: vec![DynamicToolSpec::Function(DynamicToolFunctionSpec {
                name: "check_release".to_string(),
                description: "Check whether the release is ready.".to_string(),
                input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
                defer_loading: false,
            })],
            environments: Some(vec![test.executor_environment().request()]),
            ..StartThreadOptions::new(test.config.clone())
        })
        .await?;
    test.codex = root.thread;
    test.session_configured = root.session_configured;
    let parent_id = test.session_configured.thread_id.to_string();
    let mut created = test.thread_manager.subscribe_thread_created();

    responses::mount_sse_once_match(
        &server,
        header("thread-id", parent_id.clone()),
        sse(vec![
            ev_function_call_with_namespace(
                "spawn-worker",
                "collaboration",
                "spawn_agent",
                r#"{"task_name":"worker","fork_turns":"none","message":"Check whether the release is ready."}"#,
            ),
            ev_completed("parent-spawn"),
        ]),
    )
    .await;
    let expected_parent = parent_id.clone();
    let mut final_answer =
        ev_assistant_message("parent-final", "I have delegated the release check.");
    final_answer["item"]["phase"] = json!("final_answer");
    let parent_finished = responses::mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            request
                .headers
                .get("thread-id")
                .is_some_and(|id| id == expected_parent.as_str())
                && String::from_utf8_lossy(&request.body).contains("spawn-worker")
        },
        sse(vec![final_answer, ev_completed("parent-finished")]),
    )
    .await;
    let expected_parent = parent_id.clone();
    responses::mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            request
                .headers
                .get("thread-id")
                .is_some_and(|id| id != expected_parent.as_str())
        },
        sse(vec![
            responses::ev_function_call("worker-check", "check_release", "{}"),
            ev_completed("child-check"),
        ]),
    )
    .await;

    test.submit_text_turn("Ask a worker to check the release, then finish your turn.")
        .await?;
    ThreadIdle::wait(&test.codex).await;
    let child_id = created.try_recv()?;
    let child = test.thread_manager.get_thread(child_id).await?;
    let call = wait_for_event_match(&child, |event| match event {
        EventMsg::DynamicToolCallRequest(call) => Some(call.clone()),
        _ => None,
    })
    .await;
    assert_eq!(call.call_id, "worker-check");

    responses::mount_sse_once_match(
        &server,
        header("thread-id", child_id.to_string()),
        sse(vec![
            ev_assistant_message("child-final", "All release checks passed."),
            ev_completed("child-finished"),
        ]),
    )
    .await;
    let wake = responses::mount_sse_once_match(
        &server,
        header("thread-id", parent_id.clone()),
        sse(vec![
            ev_assistant_message(
                "parent-update",
                "The worker confirmed that the release is ready.",
            ),
            ev_completed("parent-woken"),
        ]),
    )
    .await;
    child
        .submit(Op::DynamicToolResponse {
            id: call.call_id,
            response: DynamicToolResponse {
                content_items: vec![DynamicToolCallOutputContentItem::InputText {
                    text: "All release checks passed.".to_string(),
                }],
                success: true,
            },
        })
        .await?;
    wait_for_event(&child, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    ThreadIdle::wait(&test.codex).await;
    assert_ne!(
        parent_finished.single_request().body_json()["client_metadata"]["turn_id"],
        wake.single_request().body_json()["client_metadata"]["turn_id"],
    );

    let mut requests = responses::received_responses_requests(&server).await;
    assert_eq!(requests.len(), 5);
    // Preserve each complete stream while removing parent/child scheduling order.
    requests
        .sort_by_key(|request| request.header("thread-id").as_deref() != Some(parent_id.as_str()));
    let history = context_snapshot::format_request_history_snapshot(
        "The parent finishes at medium reasoning without persistent mode. A real child then completes its release check and wakes a new parent turn. Complete model streams are grouped as parent, then worker.",
        &requests,
        &ContextSnapshotOptions::default()
            .rewrite_known_segments()
            .include_request_settings(),
    );
    // The shared formatter labels agent messages without rendering their payload.
    let mut mailbox =
        responses::strip_metadata_from_json(responses::strip_response_item_ids_from_json(
            serde_json::Value::Array(wake.single_request().inputs_of_type("agent_message")),
        ));
    mailbox.sort_all_objects();
    insta::assert_snapshot!(format!(
        "{history}\n\nWoken parent mailbox:\n{}",
        serde_json::to_string_pretty(&mailbox)?,
    ));
    test.thread_manager
        .shutdown_all_threads_bounded(Duration::from_secs(5))
        .await;
    Ok(())
}

#[test_case::test_case(false; "native_tools")]
#[test_case::test_case(true; "host_tools")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_agent_catalog_parameters(disable_native_tools: bool) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_codex()
        .with_config(move |config| {
            configure_scenario_catalog(config);
            config.multi_agent_v2.disable_native_tools = disable_native_tools;
            config.workspace_roots = vec![config.cwd.clone()];
        })
        .with_model_info_override("gpt-6-astra", |model| {
            model.model_messages.as_mut().expect("model messages").tools = Some(
                serde_json::from_value(json!({"multi_agent": {"list_agents": {
                    "parameters": json!({
                        "type": "object",
                        "properties": {"path_prefix": {
                            "type": "string",
                            "description": "Inspect agents within this task path.",
                            "minLength": 1,
                            "maxLength": 128,
                        }},
                        "required": ["path_prefix"],
                        "additionalProperties": false,
                    }).to_string(),
                }}}))
                .expect("catalog tool messages"),
            );
        })
        .build_with_auto_env(&server)
        .await?;
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("agents-response"),
                ev_function_call_with_namespace(
                    "agents-call",
                    "collaboration",
                    "list_agents",
                    r#"{"path_prefix":"/root"}"#,
                ),
                ev_completed("agents-response"),
            ]),
            sse(vec![
                ev_assistant_message("final", "Only the root agent is working on this task."),
                ev_completed("final-response"),
            ]),
        ],
    )
    .await;
    test.submit_turn("Check which agents are working under /root before delegating more work.")
        .await?;
    assert_eq!(
        test.codex.multi_agent_version(),
        Some(codex_protocol::protocol::MultiAgentVersion::V2)
    );
    let (name, description) = if disable_native_tools {
        (
            "multi_agent_native_tools_hidden",
            "Native tools are unavailable while the V2 runtime and its instructions remain enabled.",
        )
    } else {
        (
            "multi_agent_catalog_parameters",
            "Astra calls list_agents using the selected catalog parameter schema.",
        )
    };
    insta::assert_snapshot!(
        name,
        context_snapshot::format_request_history_snapshot(
            description,
            &mock.requests(),
            &ContextSnapshotOptions::default().include_request_settings(),
        )
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn astra_continues_after_a_stream_is_interrupted() -> Result<()> {
    skip_if_no_network!(Ok(()));

    // Keep the first response unfinished until after the replacement request.
    let (release_interrupted_response, interrupted_response_gate) = oneshot::channel();
    let mut commentary = ev_assistant_message("commentary", "I will draft the update.");
    commentary["item"]["phase"] = json!("commentary");
    let (streaming, _completions) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: sse(vec![
                    ev_response_created("interrupted-response"),
                    commentary,
                    ev_message_item_added("unfinished-message", ""),
                    ev_output_text_delta("The launch is "),
                ]),
            },
            StreamingSseChunk {
                gate: Some(interrupted_response_gate),
                body: sse(vec![
                    ev_assistant_message("unfinished-message", "The launch is tomorrow."),
                    ev_completed("interrupted-response"),
                ]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: sse(vec![
                ev_response_created("replacement-response"),
                ev_assistant_message("final", "The customer update is ready."),
                ev_completed("replacement-response"),
            ]),
        }],
    ])
    .await;
    let config_server = start_mock_server().await;
    let base_url = format!("{}/v1", streaming.uri());
    let test = test_codex()
        .with_model("gpt-6-astra")
        .with_auth(CodexAuth::from_api_key("test-api-key"))
        .with_config(move |config| {
            configure_scenario_catalog(config);
            config.model_provider.base_url = Some(base_url);
            config.model_provider.supports_websockets = false;
            config
                .features
                .enable(Feature::InstantInterrupt)
                .expect("enable instant interrupt");
            config
                .features
                .disable(Feature::EnableRequestCompression)
                .expect("disable compression for the streaming test server");
        })
        .build_with_auto_env(&config_server)
        .await?;

    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![text(
            "Draft a launch update.",
        )]))
        .await?;
    tokio::time::timeout(
        Duration::from_secs(10),
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::AgentMessageContentDelta(delta)
                if delta.item_id == "unfinished-message" && delta.delta == "The launch is ")
        }),
    )
    .await?;
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![text(
            "Make it a customer update.",
        )]))
        .await?;
    // A replacement request must arrive while the original stream is still held open.
    tokio::time::timeout(
        Duration::from_secs(10),
        streaming.wait_for_request_count(/*count*/ 2),
    )
    .await?;
    let completed = tokio::time::timeout(
        Duration::from_secs(10),
        wait_for_event_match(&test.codex, |event| match event {
            EventMsg::TurnComplete(completed) => Some(completed.clone()),
            _ => None,
        }),
    )
    .await?;
    assert!(
        completed.error.is_none(),
        "turn failed: {:?}",
        completed.error
    );

    let requests = streaming
        .requests()
        .await
        .iter()
        .map(|body| serde_json::from_slice(body))
        .collect::<serde_json::Result<Vec<serde_json::Value>>>()?;
    assert_eq!(requests.len(), 2);
    let replacement = serde_json::to_string(&requests[1])?;
    assert!(replacement.contains("I will draft the update."));
    assert!(replacement.contains("Make it a customer update."));
    assert!(!replacement.contains("The launch is "));
    let entries = requests.iter().map(SnapshotEntry::body).collect::<Vec<_>>();
    let snapshot = context_snapshot::format_context_snapshot(
        "Astra receives new user input after its unfinished response is interrupted.",
        &entries,
        &ContextSnapshotOptions::default().rewrite_known_segments(),
    );
    insta::assert_snapshot!("astra_input_interrupts_response", snapshot);
    test.codex.shutdown_and_wait().await?;
    drop(release_interrupted_response);
    streaming.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_browser_auth_resolves_user_prompt() -> Result<()> {
    skip_if_no_network!(Ok(()));
    use super::mcp_subagent_elicitation::Caller;
    use super::mcp_subagent_elicitation::RequestKind;
    use super::mcp_subagent_elicitation::mcp_server_elicitation_scenario;

    let requests =
        mcp_server_elicitation_scenario(Caller::Subagent, RequestKind::BrowserAuth).await?;
    let snapshot = context_snapshot::format_request_history_snapshot(
        "A subagent waits for an MCP browser sign-in prompt, then receives the accepted response and continues.",
        &requests,
        &ContextSnapshotOptions::default()
            .rewrite_known_segments()
            .include_request_settings(),
    );
    let snapshot = regex_lite::Regex::new(r"Wall time: [0-9]+(?:\.[0-9]+)? seconds")?
        .replace_all(&snapshot, "Wall time: <DURATION> seconds")
        .into_owned();
    insta::assert_snapshot!("subagent_browser_auth", snapshot);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prediction_fork_inherits_parent_context() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let requests = super::reasoning_effort_override::prediction_fork_requests().await?;
    insta::assert_snapshot!(context_snapshot::format_request_history_snapshot(
        "A parent pins high request effort, then selects low. Its ephemeral prediction fork inherits the context and baseline, then selects medium on its first turn. Continuing the parent at medium produces the same request.",
        &requests,
        &ContextSnapshotOptions::default().include_request_settings(),
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prediction_fork_preserves_context_above_compaction_threshold() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("parent", "The release is ready for validation."),
                ev_completed_with_tokens("parent", /*total_tokens*/ 70_000),
            ]),
            sse(vec![
                ev_function_call_with_namespace(
                    "remaining",
                    "functions",
                    "get_context_remaining",
                    "{}",
                ),
                ev_completed_with_tokens("tool", /*total_tokens*/ 70_000),
            ]),
            sse(vec![
                ev_assistant_message("prediction", "Run the release checks."),
                ev_completed_with_tokens("prediction", /*total_tokens*/ 70_000),
            ]),
        ],
    )
    .await;
    let mut test = test_codex()
        .with_model("gpt-6-astra")
        .with_config(|config| {
            configure_scenario_catalog(config);
            config.workspace_roots = vec![config.cwd.clone()];
            config.model_auto_compact_token_limit = Some(50_000);
            config
                .features
                .enable(Feature::TokenBudget)
                .expect("enable token budget");
            config.token_budget = Some(TokenBudgetConfig {
                reminder_threshold_tokens: Some(2_000),
                ..TokenBudgetConfig::default()
            });
        })
        .build_with_auto_env(&server)
        .await?;

    test.submit_turn("Prepare the release.").await?;
    test.codex.flush_rollout().await?;
    let (mut options, settings) = test
        .thread_manager
        .fork_options_from_parent(test.session_configured.thread_id)
        .await?;
    options.config.ephemeral = true;
    options.thread_extension_init.insert(AutoCompactionDisabled);
    let child = test
        .thread_manager
        .fork_legacy_thread(
            ForkSnapshot::Interrupted,
            options,
            test.codex.rollout_path().expect("parent rollout path"),
        )
        .await?;
    child.thread.restore_thread_settings(settings).await?;
    test.codex = child.thread;
    test.submit_turn("Suggest my next message.").await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    insta::assert_snapshot!(context_snapshot::format_request_history_snapshot(
        "The parent finishes above the compaction threshold. Its prediction fork retains the parent's context and reminder, but neither compacts nor adds another reminder after a tool call.",
        &requests,
        &ContextSnapshotOptions::default().include_request_settings(),
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_catalog_refresh_preserves_tools_and_history() -> Result<()> {
    let requests = super::spawn_agent_description::model_catalog_refresh_requests(
        MultiAgentVersion::V1,
        /*model_catalog_in_context*/ true,
    )
    .await?;
    insta::assert_snapshot!(context_snapshot::format_request_history_snapshot(
        "Catalog refreshes append the updated listing once while preserving tools and prior input.",
        &requests,
        &ContextSnapshotOptions::default().include_request_settings(),
    ));
    Ok(())
}

#[test_case::test_case(PermissionPathScenario::OmittedPaths; "omitted_paths")]
#[test_case::test_case(PermissionPathScenario::DeniedReads; "denied_reads")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_permission_paths_follow_selected_model(
    scenario: PermissionPathScenario,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    if matches!(scenario, PermissionPathScenario::DeniedReads) {}
    let requests =
        super::permissions_messages::catalog_permission_paths_model_change_requests(scenario)
            .await?;
    let mut bodies = requests
        .iter()
        .map(core_test_support::responses::ResponsesRequest::body_json)
        .collect::<Vec<_>>();
    // Keep the complete history, but collapse the large platform-specific fixture path lists
    // before snapshot hashing. The helper verifies selected roots, denial facts, and omission.
    let roots = regex_lite::Regex::new(r"(?m)^(Model [AB] roots: ).*$")?;
    let filesystem = regex_lite::Regex::new(r"<filesystem>.*</filesystem>")?;
    for body in &mut bodies {
        for item in body["input"].as_array_mut().expect("request input") {
            for content in item["content"].as_array_mut().into_iter().flatten() {
                if let Some(text) = content["text"].as_str() {
                    let text = roots.replace_all(text, "${1}<BOUNDED_ROOT_LIST>.");
                    content["text"] = filesystem
                        .replace_all(
                            &text,
                            "<filesystem><LARGE_PERMISSION_PATH_FIXTURE/></filesystem>",
                        )
                        .into_owned()
                        .into();
                }
            }
        }
    }
    let entries = bodies.iter().map(SnapshotEntry::body).collect::<Vec<_>>();
    let (snapshot_name, description) = match scenario {
        PermissionPathScenario::OmittedPaths => (
            "catalog_writable_roots_follow_selected_model",
            "Catalog root and omitted-path wording follows the selected model, appending changed permissions once while preserving prior input and the mandatory omission warning.",
        ),
        PermissionPathScenario::DeniedReads => (
            "catalog_denied_reads_follow_selected_model",
            "Catalog denied-read wording follows the selected model, appending changed permissions once while preserving prior input, denial facts, and mandatory policy warnings.",
        ),
    };
    insta::assert_snapshot!(
        snapshot_name,
        context_snapshot::format_context_snapshot(
            description,
            &entries,
            &ContextSnapshotOptions::default().include_request_settings(),
        )
    );
    Ok(())
}

fn compaction_chunk(
    gate: Option<oneshot::Receiver<()>>,
    events: Vec<serde_json::Value>,
) -> StreamingSseChunk {
    StreamingSseChunk {
        gate,
        body: sse(events),
    }
}

fn compaction_output(value: &str) -> serde_json::Value {
    json!({"type": "response.output_item.done", "item": {
        "type": "compaction", "encrypted_content": value
    }})
}

async fn async_compaction_test(
    responses_uri: &str,
    features: Vec<Feature>,
    configure: impl Fn(&mut Config) + Send + Sync + 'static,
) -> Result<(wiremock::MockServer, TestCodex)> {
    let config_server = start_mock_server().await;
    let base_url = format!("{responses_uri}/v1");
    let test = test_codex()
        .with_auth(CodexAuth::from_api_key("test-api-key"))
        .with_config(move |config| {
            configure_scenario_catalog(config);
            config.model_provider.base_url = Some(base_url);
            config.model_context_window = Some(100_000);
            for feature in features {
                config
                    .features
                    .enable(feature)
                    .expect("enable scenario feature");
            }
            config
                .features
                .enable(Feature::AsyncCompaction)
                .expect("enable async compaction");
            config
                .features
                .disable(Feature::EnableRequestCompression)
                .expect("capture raw requests");
            configure(config);
        })
        .with_model("gpt-6-astra")
        .build_with_auto_env(&config_server)
        .await?;
    Ok((config_server, test))
}

/// Pending image checks for optional compaction do not delay foreground turn completion.
/// Its request still replaces files that expire after the sampling request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_compaction_image_checks_do_not_block_turn_completion() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let barrier = Arc::new(tokio::sync::Barrier::new(/*n*/ 2));
    let image_store = Arc::new(RecordingFileAttachmentStore {
        resolve_barrier: Some(Arc::clone(&barrier)),
        ..Default::default()
    });
    let (release_sampling, sampling_gate) = oneshot::channel();
    let (streaming, _completions) = start_streaming_sse_server(vec![
        vec![compaction_chunk(
            Some(sampling_gate),
            vec![
                ev_assistant_message("answer", "I examined the image."),
                ev_completed_with_tokens("answer", /*total_tokens*/ 70_000),
            ],
        )],
        vec![compaction_chunk(
            /*gate*/ None,
            vec![
                compaction_output("IMAGE_CHECKPOINT"),
                ev_completed_with_tokens("compact", /*total_tokens*/ 3_000),
            ],
        )],
    ])
    .await;
    let config_server = start_mock_server().await;
    let base_url = format!("{}/v1", streaming.uri());
    let test = test_codex()
        .with_auth(CodexAuth::from_api_key("test-api-key"))
        .with_image_store(image_store.clone())
        .with_config(move |config| {
            configure_scenario_catalog(config);
            config.model_provider.base_url = Some(base_url);
            config.model_context_window = Some(100_000);
            config.features.enable(Feature::AsyncCompaction).unwrap();
            config
                .features
                .disable(Feature::EnableRequestCompression)
                .unwrap();
        })
        .with_model("gpt-6-astra")
        .build_with_auto_env(&config_server)
        .await?;
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![
            text("Describe this image."),
            UserInput::Image {
                image: ImageReference::File {
                    file_id: "file_expiring".to_string(),
                },
                detail: Some(ImageDetail::High),
            },
        ]))
        .await?;
    // Release the sampling lookup, then expire the file before its response finishes.
    barrier.wait().await;
    image_store
        .resolve_errors
        .lock()
        .expect("resolve errors lock")
        .insert(
            "file_expiring".to_string(),
            AttachmentStoreErrorKind::NotFound,
        );
    release_sampling.send(()).expect("finish sampling");
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(streaming.requests().await.len(), 1);

    // The turn completed without releasing the optional compaction lookup.
    barrier.wait().await;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 20),
        test.codex.wait_for_background_compaction_for_test(),
    )
    .await?;
    let requests = streaming
        .requests()
        .await
        .iter()
        .map(|body| serde_json::from_slice::<serde_json::Value>(body))
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert_eq!(requests.len(), 2);
    assert_eq!(
        *image_store.resolves.lock().expect("resolves lock"),
        HashMap::from([("file_expiring".to_string(), 2)])
    );
    assert!(requests[0]["input"].to_string().contains("file_expiring"));
    assert!(
        requests[1]["input"]
            .to_string()
            .contains("[image unavailable]")
    );
    assert!(!requests[1]["input"].to_string().contains("file_expiring"));
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[derive(Clone, Copy)]
enum LiveSuffix {
    User,
    Tool,
    RetainedTool,
    OverBudgetRetainedTool,
}

#[test_case::test_case(LiveSuffix::User; "newer user")]
#[test_case::test_case(LiveSuffix::Tool; "only assistant and tools")]
#[test_case::test_case(LiveSuffix::RetainedTool; "retained standalone output")]
#[test_case::test_case(LiveSuffix::OverBudgetRetainedTool; "ready adoption still over budget")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn astra_async_compaction_keeps_work_added_after_its_snapshot(
    suffix: LiveSuffix,
) -> Result<()> {
    use codex_protocol::config_types::CollaborationMode;
    use codex_protocol::config_types::ModeKind;
    use codex_protocol::config_types::Settings;
    use codex_protocol::protocol::ThreadSettingsOverrides;
    use core_test_support::responses::ev_completed_with_tokens;

    skip_if_no_network!(Ok(()));
    let over_budget = matches!(suffix, LiveSuffix::OverBudgetRetainedTool);
    let retained_output = if over_budget {
        format!("delegated work {}", "x ".repeat(12_000))
    } else {
        "delegated work".to_string()
    };
    let (release_background, background_gate) = oneshot::channel();
    let (release_foreground, foreground_gate) = oneshot::channel();
    let mut responses = vec![
        vec![compaction_chunk(
            /*gate*/ None,
            vec![
                ev_assistant_message("draft", "I drafted the launch notes."),
                ev_completed_with_tokens(
                    "draft-response",
                    /*total_tokens*/ if over_budget { 2_000 } else { 70_000 },
                ),
            ],
        )],
        vec![compaction_chunk(
            Some(background_gate),
            vec![
                compaction_output("ASYNC_LAUNCH_CHECKPOINT"),
                ev_completed_with_tokens("compact-response", /*total_tokens*/ 3_000),
            ],
        )],
        vec![
            compaction_chunk(
                /*gate*/ None,
                vec![
                    ev_response_created("team-response"),
                    ev_assistant_message("team-check", "I will check who is available."),
                ],
            ),
            compaction_chunk(
                Some(foreground_gate),
                vec![
                    ev_function_call_with_namespace("team", "collaboration", "list_agents", "{}"),
                    ev_completed_with_tokens(
                        "team-response",
                        /*total_tokens*/ if over_budget { 9_000 } else { 75_000 },
                    ),
                ],
            ),
        ],
        vec![compaction_chunk(
            /*gate*/ None,
            vec![
                ev_assistant_message("answer", "I can finish the notes myself."),
                ev_completed_with_tokens(
                    "answer-response",
                    /*total_tokens*/ if over_budget { 10 } else { 3_000 },
                ),
            ],
        )],
    ];
    if over_budget {
        responses.insert(
            3,
            vec![compaction_chunk(
                /*gate*/ None,
                vec![
                    compaction_output("SYNC"),
                    ev_completed_with_tokens("sync", /*total_tokens*/ 3_000),
                ],
            )],
        );
    }
    let (streaming, _completions) = start_streaming_sse_server(responses).await;
    let features = match suffix {
        LiveSuffix::User => vec![Feature::IncrementalTools],
        LiveSuffix::Tool => vec![],
        LiveSuffix::RetainedTool | LiveSuffix::OverBudgetRetainedTool => {
            vec![Feature::RetainClientToolOutputs]
        }
    };
    let (_config_server, test) = async_compaction_test(streaming.uri(), features, move |config| {
        if over_budget {
            config.async_compaction_threshold_percent = 1;
            config.model_auto_compact_token_limit = Some(5_000);
        }
    })
    .await?;
    test.codex.ensure_rollout_materialized().await;
    let compaction_event = |event: &EventMsg| match event {
        EventMsg::ItemStarted(event) if matches!(event.item, TurnItem::ContextCompaction(_)) => {
            Some(("started", event.item.id()))
        }
        EventMsg::ItemCompleted(event) if matches!(event.item, TurnItem::ContextCompaction(_)) => {
            Some(("completed", event.item.id()))
        }
        _ => None,
    };

    test.submit_text_turn("Draft the launch notes.").await?;
    let timeout = Duration::from_secs(/*secs*/ 20);
    tokio::time::timeout(timeout, streaming.wait_for_request_count(/*count*/ 2)).await?;
    let next_input = if matches!(suffix, LiveSuffix::User) {
        TurnInputRequest::user_input(vec![text("Check who can finish the notes.")])
    } else {
        let item = serde_json::from_value(json!({
            "type": "function_call_output",
            "name": "send_message_to_thread",
            "namespace": "codex_app",
            "output": retained_output,
        }))?;
        TurnInputRequest::new(
            if matches!(
                suffix,
                LiveSuffix::RetainedTool | LiveSuffix::OverBudgetRetainedTool
            ) {
                TurnInput::AnnotatedResponseItem(
                    codex_protocol::turn_input::AnnotatedResponseItem {
                        item,
                        annotations: codex_protocol::turn_input::ResponseItemAnnotations {
                            retain: true,
                        },
                    },
                )
            } else {
                TurnInput::ResponseItem(item)
            },
        )
    };
    test.codex
        .start_or_steer_turn(next_input.with_thread_settings(ThreadSettingsOverrides {
            collaboration_mode: Some(CollaborationMode {
                mode: ModeKind::Plan,
                settings: Settings {
                    model: "gpt-6-astra".to_string(),
                    reasoning_effort: None,
                    developer_instructions: Some(
                        "Plan Mode: prepare a plan before making changes.".to_string(),
                    ),
                },
            }),
            ..Default::default()
        }))
        .await?;
    tokio::time::timeout(timeout, streaming.wait_for_request_count(/*count*/ 3)).await?;
    wait_for_event(&test.codex, |event| {
        assert_eq!(compaction_event(event), None);
        matches!(event, EventMsg::ItemCompleted(event)
        if matches!(&event.item, TurnItem::AgentMessage(message) if message.id == "team-check"))
    })
    .await;
    release_background
        .send(())
        .expect("release background compaction");
    // Wait for client publication while foreground work is gated, keeping adoption optional.
    tokio::time::timeout(
        timeout,
        test.codex.wait_for_background_compaction_for_test(),
    )
    .await?;
    release_foreground
        .send(())
        .expect("release foreground work");
    let mut compaction_events = Vec::new();
    wait_for_event(&test.codex, |event| {
        compaction_events.extend(compaction_event(event));
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    tokio::time::timeout(
        timeout,
        test.codex.wait_for_background_compaction_for_test(),
    )
    .await?;
    assert_eq!(compaction_events.len(), if over_budget { 4 } else { 2 });
    for pair in compaction_events.chunks_exact(2) {
        assert_eq!((pair[0].0, pair[1].0), ("started", "completed"));
        assert_eq!(pair[0].1, pair[1].1);
    }
    if over_budget {
        assert_ne!(compaction_events[0].1, compaction_events[2].1);
    }

    let requests = streaming
        .requests()
        .await
        .iter()
        .map(|body| serde_json::from_slice::<serde_json::Value>(body))
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert_eq!(requests.len(), if over_budget { 5 } else { 4 });
    if over_budget {
        let continuation = requests[4]["input"].as_array().expect("continuation input");
        assert!(
            continuation
                .iter()
                .any(|item| item["encrypted_content"] == "SYNC")
        );
        assert!(
            continuation
                .iter()
                .any(|item| item["output"] == retained_output)
        );
    }
    let history = test
        .codex
        .load_history(/*include_archived*/ false)
        .await?
        .items;
    assert!(history.windows(2).any(|items| matches!(items,
        [RolloutItem::Compacted(_), RolloutItem::WorldState(baseline)]
        if baseline.full && baseline.state["collaboration_mode"]["mode"] == "plan"
    )));
    let input = requests[3]["input"].as_array().expect("request input");
    // The full snapshot normalizes mode text; check the cutoff and later update explicitly.
    let position = |text: &str| {
        let positions = input
            .iter()
            .enumerate()
            .filter_map(|(i, item)| item.to_string().contains(text).then_some(i))
            .collect::<Vec<_>>();
        assert_eq!(positions.len(), 1, "{text}");
        positions[0]
    };
    let default = position("# Collaboration Mode: Default");
    let draft = position("Draft the launch notes.");
    let compaction = position("ASYNC_LAUNCH_CHECKPOINT");
    let plan = position("Plan Mode");
    if matches!(suffix, LiveSuffix::User) {
        let user = position("Check who can finish the notes.");
        assert!(draft < compaction && compaction < default && default < plan && plan < user);
        let entries = requests.iter().map(SnapshotEntry::body).collect::<Vec<_>>();
        insta::assert_snapshot!(
            "astra_async_compaction_with_live_suffix",
            context_snapshot::format_context_snapshot(
                "Astra compacts a completed launch-notes turn in the background. The user switches to Plan mode while it runs. Adoption restores Default mode at the cutoff, then preserves the Plan update, newer user message, and assistant/tool exchange in their original order.",
                &entries,
                &ContextSnapshotOptions::default().rewrite_known_segments(),
            )
        );
    } else {
        let tool = position("delegated work");
        if matches!(
            suffix,
            LiveSuffix::RetainedTool | LiveSuffix::OverBudgetRetainedTool
        ) {
            assert!(draft < compaction && compaction < default && default < plan && plan < tool);
        } else {
            assert!(default < draft && draft < compaction && compaction < plan && plan < tool);
        }
        let assistant = position("I will check who is available.");
        let call = input
            .iter()
            .position(|item| item["type"] == "function_call" && item["call_id"] == "team")
            .expect("retained tool call");
        let output = input
            .iter()
            .position(|item| item["type"] == "function_call_output" && item["call_id"] == "team")
            .expect("retained tool output");
        assert!(tool < assistant && assistant < call && call < output);
    }
    Ok(())
}

#[derive(PartialEq)]
enum PendingCompactionOutcome {
    Absent,
    Success,
    SuccessOverBudget,
    Failure,
    ReadyFailure,
    Interrupted,
}

#[test_case::test_case(PendingCompactionOutcome::Success, CompactionTrigger::Auto; "auto uses pending result")]
#[test_case::test_case(PendingCompactionOutcome::Failure, CompactionTrigger::Auto; "auto falls back after failure")]
#[test_case::test_case(PendingCompactionOutcome::ReadyFailure, CompactionTrigger::Auto; "auto falls back after failure before checkpoint")]
#[test_case::test_case(PendingCompactionOutcome::Interrupted, CompactionTrigger::Auto; "auto can be interrupted")]
#[test_case::test_case(PendingCompactionOutcome::Absent, CompactionTrigger::Manual; "manual without background")]
#[test_case::test_case(PendingCompactionOutcome::Success, CompactionTrigger::Manual; "manual uses pending result")]
#[test_case::test_case(PendingCompactionOutcome::Failure, CompactionTrigger::Manual; "manual falls back after failure")]
#[test_case::test_case(PendingCompactionOutcome::Interrupted, CompactionTrigger::Manual; "manual can be interrupted")]
#[test_case::test_case(PendingCompactionOutcome::SuccessOverBudget, CompactionTrigger::Auto; "pending adoption still over budget")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_compaction_reuses_pending_work(
    outcome: PendingCompactionOutcome,
    trigger: CompactionTrigger,
) -> Result<()> {
    use core_test_support::responses::ev_completed_with_tokens;

    skip_if_no_network!(Ok(()));
    let over_budget = outcome == PendingCompactionOutcome::SuccessOverBudget;
    let retained_output = format!("LATE_OUTPUT {}", "x ".repeat(12_000));
    let manual = matches!(trigger, CompactionTrigger::Manual);
    let has_background = outcome != PendingCompactionOutcome::Absent;
    let seed_tokens = if over_budget {
        2_000
    } else if has_background {
        70_000
    } else {
        10_000
    };
    let valid_output = !matches!(
        outcome,
        PendingCompactionOutcome::Failure | PendingCompactionOutcome::ReadyFailure
    );
    let succeeds = !matches!(
        outcome,
        PendingCompactionOutcome::Absent
            | PendingCompactionOutcome::SuccessOverBudget
            | PendingCompactionOutcome::Failure
            | PendingCompactionOutcome::ReadyFailure
    );
    let response = |id, event, tokens| vec![event, ev_completed_with_tokens(id, tokens)];
    let immediate = |events| vec![compaction_chunk(/*gate*/ None, events)];
    let (release_background, background_gate) = oneshot::channel();
    let mut responses = vec![immediate(response(
        "seed",
        ev_assistant_message("seed", "seed"),
        seed_tokens,
    ))];
    if has_background {
        responses.push(vec![compaction_chunk(
            Some(background_gate),
            response(
                "background",
                if valid_output {
                    compaction_output("ASYNC")
                } else {
                    ev_assistant_message("invalid", "missing compaction output")
                },
                /*tokens*/ 3_000,
            ),
        )]);
    }
    if !manual {
        responses.push(immediate(response(
            "second",
            ev_function_call_with_namespace("team", "collaboration", "list_agents", "{}"),
            /*tokens*/ if over_budget { 9_000 } else { 91_000 },
        )));
    }
    if !succeeds {
        responses.push(immediate(response(
            "sync",
            compaction_output("SYNC"),
            /*tokens*/ 3_000,
        )));
    }
    responses.push(immediate(response(
        "continuation",
        ev_assistant_message("continuation", "continuation"),
        /*tokens*/ if over_budget { 10 } else { 3_000 },
    )));
    let (streaming, mut completions) = start_streaming_sse_server(responses).await;
    let (_config_server, test) = async_compaction_test(
        streaming.uri(),
        if over_budget {
            vec![Feature::RetainClientToolOutputs]
        } else {
            vec![]
        },
        move |config| {
            if over_budget {
                config.async_compaction_threshold_percent = 1;
                config.model_auto_compact_token_limit = Some(5_000);
            }
        },
    )
    .await?;
    test.codex.ensure_rollout_materialized().await;
    let timeout = Duration::from_secs(/*secs*/ 20);
    test.submit_text_turn("first").await?;
    if has_background {
        tokio::time::timeout(timeout, streaming.wait_for_request_count(/*count*/ 2)).await?;
    }
    let release_background = if outcome == PendingCompactionOutcome::ReadyFailure {
        release_background
            .send(())
            .expect("fail background compaction before the next checkpoint");
        // The client must observe the failure before take_ready polls the closed receiver.
        tokio::time::timeout(
            timeout,
            test.codex.wait_for_background_compaction_for_test(),
        )
        .await?;
        None
    } else {
        Some(release_background)
    };
    if manual {
        test.codex.submit(Op::Compact).await?;
    } else if over_budget {
        test.codex.start_or_steer_turn(TurnInputRequest::new(TurnInput::AnnotatedResponseItem(
            codex_protocol::turn_input::AnnotatedResponseItem {
                item: serde_json::from_value(json!({"type": "function_call_output", "name": "send_message_to_thread", "namespace": "codex_app", "output": retained_output}))?,
                annotations: codex_protocol::turn_input::ResponseItemAnnotations { retain: true },
            },
        ))).await?;
    } else {
        test.codex
            .start_or_steer_turn(TurnInputRequest::user_input(vec![text("newer")]))
            .await?;
    }
    let started = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ItemStarted(event)
            if matches!(event.item, TurnItem::ContextCompaction(_)))
    })
    .await;
    if has_background && outcome != PendingCompactionOutcome::ReadyFailure {
        assert_eq!(streaming.requests().await.len(), if manual { 2 } else { 3 });
    }
    if outcome == PendingCompactionOutcome::Interrupted {
        test.codex.submit(Op::Interrupt).await?;
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnAborted(_))
        })
        .await;
        release_background
            .expect("pending background compaction")
            .send(())
            .expect("release unused result");
        completions.remove(1).await?;
        test.codex.shutdown_and_wait().await?;
        assert_eq!(streaming.requests().await.len(), if manual { 2 } else { 3 });
        return Ok(());
    }
    if has_background && let Some(release_background) = release_background {
        release_background.send(()).expect("release pending result");
    }
    let EventMsg::ItemStarted(started) = started else {
        unreachable!();
    };
    let mut starts = vec![started.item.id()];
    let mut completed = Vec::new();
    wait_for_event(&test.codex, |event| {
        if let EventMsg::ItemStarted(event) = event
            && matches!(event.item, TurnItem::ContextCompaction(_))
        {
            starts.push(event.item.id());
        }
        if let EventMsg::ItemCompleted(event) = event
            && matches!(event.item, TurnItem::ContextCompaction(_))
        {
            completed.push(event.item.id());
        }
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(completed, starts);
    assert_eq!(starts.len(), if over_budget { 2 } else { 1 });
    assert_eq!(starts.iter().collect::<HashSet<_>>().len(), starts.len());
    tokio::time::timeout(
        timeout,
        test.codex.wait_for_background_compaction_for_test(),
    )
    .await?;
    if manual {
        test.submit_text_turn("after manual compaction").await?;
    }

    let history = test.codex.load_history(/*include_archived*/ false).await?;
    let usage = |id| {
        history
            .items
            .iter()
            .find_map(|item| match item {
                RolloutItem::TokenUsageRecord(record) if record.response_id == id => Some(record),
                _ => None,
            })
            .expect("persisted usage")
    };
    assert!(history.items.iter().all(|item| !matches!(item,
        RolloutItem::TokenUsageRecord(record) if record.response_id == "background"
    )));
    let continuation = usage("continuation");
    assert_eq!(
        (
            continuation.turn_token_usage.total_tokens,
            continuation.thread_token_usage.total_tokens,
        ),
        if over_budget {
            (12_010, 14_010)
        } else if manual {
            (3_000, seed_tokens + if succeeds { 3_000 } else { 6_000 })
        } else if succeeds {
            (94_000, 164_000)
        } else {
            (97_000, 167_000)
        },
    );

    let requests = streaming
        .requests()
        .await
        .iter()
        .map(|body| serde_json::from_slice::<serde_json::Value>(body))
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert_eq!(
        requests.len(),
        (if manual { 2 } else { 3 }) + usize::from(has_background) + usize::from(!succeeds)
    );
    if over_budget {
        for request in &requests[3..] {
            assert!(
                request["input"]
                    .as_array()
                    .expect("compaction or continuation input")
                    .iter()
                    .any(|item| item["output"] == retained_output)
            );
        }
    }
    if manual && !succeeds {
        let foreground = &requests[requests.len() - 2];
        let metadata: serde_json::Value = serde_json::from_str(
            foreground["client_metadata"]["x-codex-turn-metadata"]
                .as_str()
                .expect("manual foreground metadata"),
        )?;
        assert_eq!(metadata["compaction"]["trigger"], "manual");
        assert_eq!(metadata["compaction"]["reason"], "user_requested");
        assert_eq!(metadata["compaction"]["phase"], "standalone_turn");
    }
    let summaries: Vec<_> = requests.last().expect("continuation")["input"]
        .as_array()
        .expect("foreground input")
        .iter()
        .filter(|item| item["type"] == "compaction")
        .map(|item| &item["encrypted_content"])
        .collect();
    assert_eq!(summaries, [&json!(if succeeds { "ASYNC" } else { "SYNC" })]);
    Ok(())
}
