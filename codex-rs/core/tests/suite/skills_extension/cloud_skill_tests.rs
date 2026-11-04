//! Core cloud-skill behavior exercised without the concrete MCP-backed provider.

use super::*;
use pretty_assertions::assert_eq;
use tokio::sync::Mutex;

struct FakeCloudSkillProvider {
    catalog: SkillCatalog,
    resources: std::collections::HashMap<String, String>,
    reads: Mutex<Vec<String>>,
    read_barrier: Option<tokio::sync::Barrier>,
}

impl SkillProvider for FakeCloudSkillProvider {
    fn list(&self, _query: SkillListQuery) -> SkillProviderFuture<'_, SkillCatalog> {
        Box::pin(async { Ok(self.catalog.clone()) })
    }

    fn read<'a>(
        &'a self,
        request: SkillReadRequest<'a>,
    ) -> SkillProviderFuture<'a, SkillReadResult> {
        Box::pin(async move {
            assert_eq!(
                request.authority,
                SkillAuthority::new(SkillSourceKind::Cloud, CODEX_APPS_MCP_SERVER_NAME)
            );
            assert!(
                self.catalog
                    .entries
                    .iter()
                    .any(|entry| entry.id == request.package)
            );
            self.reads
                .lock()
                .await
                .push(request.resource.as_str().to_string());
            let contents = self
                .resources
                .get(request.resource.as_str())
                .cloned()
                .ok_or_else(|| SkillProviderError::new("unknown fake skill resource"))?;
            if let Some(barrier) = &self.read_barrier {
                tokio::time::timeout(Duration::from_secs(/*secs*/ 10), barrier.wait())
                    .await
                    .map_err(|_| SkillProviderError::new("skill reads did not overlap"))?;
            }
            Ok(SkillReadResult {
                resource: request.resource,
                contents,
            })
        })
    }

    fn search(&self, _request: SkillSearchRequest) -> SkillProviderFuture<'_, SkillSearchResult> {
        Box::pin(async { Ok(SkillSearchResult::default()) })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_turn_aliases_discovered_singleton_cloud_locator() -> Result<()> {
    skip_if_no_network!(Ok(()));

    const SKILL_ROOT: &str = "skill://plugin_connector_1p_2330815c823c8191941e5dc465bb899f";
    const SKILL_BODY: &str = "CLOUD_SKILL_REMAINS_AVAILABLE_WITHOUT_HOST_DISCOVERY";

    let server = responses::start_mock_server().await;
    let response = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;

    let mut extensions = ExtensionRegistryBuilder::new();
    install_with_providers(
        &mut extensions,
        SkillProviders::new().with_cloud_provider(Arc::new(FakeCloudSkillProvider {
            catalog: SkillCatalog {
                is_partial: false,
                entries: vec![
                    SkillCatalogEntry::new(
                        SkillPackageId(format!("{SKILL_ROOT}/search")),
                        SkillAuthority::new(SkillSourceKind::Cloud, CODEX_APPS_MCP_SERVER_NAME),
                        "demo:search",
                        "Search company knowledge.",
                        SkillResourceId::new(format!("{SKILL_ROOT}/search/SKILL.md")),
                    )
                    .with_display_path(format!("{SKILL_ROOT}/search"))
                    .with_alias_root(SKILL_ROOT),
                ],
                warnings: Vec::new(),
            },
            resources: std::collections::HashMap::from([(
                format!("{SKILL_ROOT}/search/SKILL.md"),
                SKILL_BODY.to_string(),
            )]),
            reads: Mutex::default(),
            read_barrier: None,
        })),
        |config: &Config| SkillsExtensionConfig {
            include_instructions: config.include_skill_instructions,
            catalog_deltas_enabled: config.features.enabled(Feature::SkillCatalogDeltas),
            omit_descriptions: false,
            max_context_tokens: config.skill_max_context_tokens,
            bundled_skills_enabled: false,
            cloud_skill_enabled: true,
            shadow_selection_enabled: false,
        },
    );
    let chatgpt_base_url = server.uri();
    let mut builder = test_codex()
        .with_auth(CodexAuth::from_api_key("test-api-key"))
        .with_config(move |config| config.chatgpt_base_url = chatgpt_base_url)
        .with_exec_server_url("none")
        .with_extensions(Arc::new(extensions.build()))
        .with_model_info_override("gpt-5.5", |model_info| {
            model_info.context_window = Some(2_000);
            model_info.max_context_window = None;
        })
        .with_config(|config| {
            config.include_skill_instructions = true;
            config.cloud_skill_enabled = true;
            config
                .features
                .enable(Feature::SkipHostSkillDiscovery)
                .expect("cloud skills must not depend on host discovery");
        });
    let test = builder.build_with_auto_env(&server).await?;

    test.submit_turn("Use $demo:search.").await?;

    let request = response.single_request();
    let developer_text = request.message_input_texts("developer").join("\n");
    assert!(
        developer_text.lines().any(|line| {
            line.starts_with("- demo:search:") && line.ends_with("(cloud package: c0/search)")
        }),
        "model request should include the aliased cloud package: {developer_text}"
    );
    assert!(developer_text.contains(&format!("- `c0` = `{SKILL_ROOT}`")));
    let user_text = request.message_input_texts("user").join("\n");
    assert!(
        user_text.contains("<skill>\n<name>demo:search</name>") && user_text.contains(SKILL_BODY),
        "cloud instruction reads must remain available without host discovery: {user_text}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_skill_reads_run_in_parallel() -> Result<()> {
    skip_if_no_network!(Ok(()));

    const PACKAGE: &str = "skill://demo/parallel";
    const READS: [(&str, &str, &str); 2] = [
        (
            "read-main",
            "skill://demo/parallel/SKILL.md",
            "Main instructions.",
        ),
        (
            "read-reference",
            "skill://demo/parallel/references/guide.md",
            "Reference instructions.",
        ),
    ];
    let server = responses::start_mock_server().await;
    let mut extensions = ExtensionRegistryBuilder::new();
    install_with_providers(
        &mut extensions,
        SkillProviders::new().with_cloud_provider(Arc::new(FakeCloudSkillProvider {
            catalog: SkillCatalog {
                is_partial: false,
                entries: vec![SkillCatalogEntry::new(
                    SkillPackageId(PACKAGE.to_string()),
                    SkillAuthority::new(SkillSourceKind::Cloud, CODEX_APPS_MCP_SERVER_NAME),
                    "demo:parallel",
                    "Instructions and a reference.",
                    SkillResourceId::new(READS[0].1),
                )],
                warnings: Vec::new(),
            },
            resources: READS
                .iter()
                .map(|(_, resource, contents)| (resource.to_string(), contents.to_string()))
                .collect(),
            reads: Mutex::default(),
            // An exclusive scheduler lock prevents the second read from reaching this barrier.
            read_barrier: Some(tokio::sync::Barrier::new(/*n*/ 2)),
        })),
        |config: &Config| SkillsExtensionConfig {
            include_instructions: config.include_skill_instructions,
            catalog_deltas_enabled: config.features.enabled(Feature::SkillCatalogDeltas),
            omit_descriptions: false,
            max_context_tokens: config.skill_max_context_tokens,
            bundled_skills_enabled: false,
            cloud_skill_enabled: true,
            shadow_selection_enabled: false,
        },
    );
    let chatgpt_base_url = server.uri();
    let mut builder = test_codex()
        .with_auth(CodexAuth::from_api_key("test-api-key"))
        .with_exec_server_url("none")
        .with_extensions(Arc::new(extensions.build()))
        .with_config(move |config| {
            config.chatgpt_base_url = chatgpt_base_url;
            config.include_skill_instructions = true;
            config.cloud_skill_enabled = true;
        });
    let test = builder.build_with_auto_env(&server).await?;
    let mut calls = vec![ev_response_created("read-both")];
    calls.extend(READS.map(|(call_id, resource, _)| {
        responses::ev_function_call_with_namespace(
            call_id,
            "skills",
            "read",
            &json!({"package": PACKAGE, "resource": resource}).to_string(),
        )
    }));
    calls.push(ev_completed("read-both"));
    let response = responses::mount_sse_sequence(
        &server,
        vec![
            sse(calls),
            sse(vec![ev_response_created("done"), ev_completed("done")]),
        ],
    )
    .await;
    test.submit_turn("Read both resources.").await?;

    let requests = response.requests();
    assert_eq!(requests.len(), 2);
    for (call_id, resource, contents) in READS {
        let output = requests[1]
            .function_call_output_text(call_id)
            .expect("each concurrent skill read should return its resource");
        assert_eq!(
            serde_json::from_str::<Value>(&output)?,
            json!({"resource": resource, "contents": contents, "next_cursor": null})
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_skill_can_read_referenced_resource_without_an_executor() -> Result<()> {
    skip_if_no_network!(Ok(()));
    const SKILL_NAME: &str = "demo-plugin:deploy";

    const SKILL_DESCRIPTION: &str = "Deploy through the &lt;hosted&gt; orchestrator.";

    const SKILL_RESOURCE_URI: &str = "skill://plugin_demo/deploy";

    const SKILL_MAIN_PROMPT_URI: &str = "skill://plugin_demo/deploy/SKILL.md";

    const SKILL_REFERENCE_URI: &str = "skill://plugin_demo/deploy/references/deploy.md";

    const SKILL_MARKER: &str = "ORCHESTRATOR_SKILL_BODY_MARKER";

    const SKILL_CONTENTS: &str = concat!(
        "---\n",
        "name: deploy\n",
        "description: Deploy through the orchestrator.\n",
        "---\n\n",
        "# Deploy\n\n",
        "ORCHESTRATOR_SKILL_BODY_MARKER\n\n",
        "Read the [deployment reference](skill://plugin_demo/deploy/references/deploy.md).\n",
    );

    const SKILL_REFERENCE_CONTENTS: &str =
        "# Deploy reference\n\nUse the orchestrator deployment API.\n";

    const SKILLS_LIST_CALL_ID: &str = "skills-list";

    const SKILLS_READ_MAIN_CALL_ID: &str = "skills-read-main";

    const SKILLS_READ_CALL_ID: &str = "skills-read";

    const SKILLS_READ_AGAIN_CALL_ID: &str = "skills-read-again";

    let responses_server = responses::start_mock_server().await;
    let provider = Arc::new(FakeCloudSkillProvider {
        catalog: SkillCatalog {
            is_partial: false,
            entries: vec![
                SkillCatalogEntry::new(
                    SkillPackageId(SKILL_RESOURCE_URI.to_string()),
                    SkillAuthority::new(SkillSourceKind::Cloud, CODEX_APPS_MCP_SERVER_NAME),
                    SKILL_NAME,
                    SKILL_DESCRIPTION,
                    SkillResourceId::new(SKILL_MAIN_PROMPT_URI),
                )
                .with_display_path(SKILL_RESOURCE_URI)
                .with_alias_root("skill://plugin_demo"),
            ],
            warnings: vec!["Cloud discovery returned a partial catalog.".to_string()],
        },
        resources: std::collections::HashMap::from([
            (
                SKILL_MAIN_PROMPT_URI.to_string(),
                SKILL_CONTENTS.to_string(),
            ),
            (
                SKILL_REFERENCE_URI.to_string(),
                SKILL_REFERENCE_CONTENTS.to_string(),
            ),
        ]),
        reads: Mutex::default(),
        read_barrier: None,
    });
    let mut extensions = ExtensionRegistryBuilder::new();
    install_with_providers(
        &mut extensions,
        SkillProviders::new().with_cloud_provider(provider.clone()),
        |config: &Config| SkillsExtensionConfig {
            include_instructions: config.include_skill_instructions,
            catalog_deltas_enabled: config.features.enabled(Feature::SkillCatalogDeltas),
            omit_descriptions: false,
            max_context_tokens: config.skill_max_context_tokens,
            bundled_skills_enabled: false,
            cloud_skill_enabled: true,
            shadow_selection_enabled: false,
        },
    );
    let chatgpt_base_url = responses_server.uri();
    let mut builder = test_codex()
        .with_auth(CodexAuth::from_api_key("test-api-key"))
        .with_config(move |config| config.chatgpt_base_url = chatgpt_base_url)
        .with_exec_server_url("none")
        .with_extensions(Arc::new(extensions.build()))
        .with_config(|config| {
            config.include_skill_instructions = true;
            config.cloud_skill_enabled = true;
        });
    let test = builder.build_with_auto_env(&responses_server).await?;
    let response_mock = responses::mount_sse_sequence(
        &responses_server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-skills-read-main"),
                responses::ev_function_call_with_namespace(
                    SKILLS_READ_MAIN_CALL_ID,
                    "skills",
                    "read",
                    &json!({
                        "package": SKILL_RESOURCE_URI,
                    })
                    .to_string(),
                ),
                responses::ev_completed("resp-skills-read-main"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-skills-list"),
                responses::ev_function_call_with_namespace(
                    SKILLS_LIST_CALL_ID,
                    "skills",
                    "list",
                    &json!({
                        "authority": {
                            "kind": "cloud",
                        },
                    })
                    .to_string(),
                ),
                responses::ev_completed("resp-skills-list"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-skills-read"),
                responses::ev_function_call_with_namespace(
                    SKILLS_READ_CALL_ID,
                    "skills",
                    "read",
                    &json!({
                        "package": SKILL_RESOURCE_URI,
                        "authority": {
                            "kind": "cloud",
                        },
                        "resource": SKILL_REFERENCE_URI,
                    })
                    .to_string(),
                ),
                responses::ev_completed("resp-skills-read"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-skills-read-again"),
                responses::ev_function_call_with_namespace(
                    SKILLS_READ_AGAIN_CALL_ID,
                    "skills",
                    "read",
                    &json!({
                        "package": SKILL_RESOURCE_URI,
                        "resource": SKILL_REFERENCE_URI,
                    })
                    .to_string(),
                ),
                responses::ev_completed("resp-skills-read-again"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-orchestrator-skill"),
                responses::ev_assistant_message("msg-orchestrator-skill", "Done"),
                responses::ev_completed("resp-orchestrator-skill"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-orchestrator-skill-next-turn"),
                responses::ev_assistant_message("msg-orchestrator-skill-next-turn", "Done"),
                responses::ev_completed("resp-orchestrator-skill-next-turn"),
            ]),
        ],
    )
    .await;
    test.submit_turn("Use the deployment capability.").await?;
    let requests = response_mock.requests();
    assert_eq!(requests.len(), 5);
    let first_request = &requests[0];
    assert!(first_request.tool_by_name("skills", "list").is_some());
    let read_tool = first_request
        .tool_by_name("skills", "read")
        .ok_or_else(|| anyhow::anyhow!("skills.read should be available"))?;
    assert_eq!(read_tool["parameters"]["required"], json!(["package"]));
    assert!(
        read_tool["parameters"]["properties"]
            .get("authority")
            .is_none()
    );
    assert!(first_request.tool_by_name("skills", "search").is_none());

    let developer_messages = first_request.message_input_texts("developer");
    let catalog_line = format!("- {SKILL_NAME}: {SKILL_DESCRIPTION} (cloud package: c0/deploy)");
    assert!(
        developer_messages
            .iter()
            .any(|text| text.contains("- `c0` = `skill://plugin_demo`"))
    );
    assert_eq!(
        1,
        developer_messages
            .iter()
            .filter(|text| text.contains(&catalog_line))
            .count()
    );
    assert!(
        developer_messages
            .iter()
            .all(|text| !text.contains("ignored-plugin:ignored"))
    );
    assert!(
        developer_messages
            .iter()
            .any(|text| text.contains("do not treat `skill://` identifiers as filesystem paths"))
    );
    assert!(
        first_request
            .message_input_texts("user")
            .into_iter()
            .all(|text| !text.starts_with("<skill>"))
    );

    let main_read_output = requests[1]
        .function_call_output_text(SKILLS_READ_MAIN_CALL_ID)
        .ok_or_else(|| anyhow::anyhow!("skills.read output should be sent to the model"))?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&main_read_output)?,
        json!({
            "resource": SKILL_MAIN_PROMPT_URI,
            "contents": SKILL_CONTENTS,
            "next_cursor": null,
        })
    );

    let list_output = requests[2]
        .function_call_output_text(SKILLS_LIST_CALL_ID)
        .ok_or_else(|| anyhow::anyhow!("skills.list output should be sent to the model"))?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&list_output)?,
        json!({
            "skills": [{
                "authority": {
                    "kind": "cloud",
                },
                "package": SKILL_RESOURCE_URI,
                "name": SKILL_NAME,
                "description": SKILL_DESCRIPTION,
                "main_resource": SKILL_MAIN_PROMPT_URI,
            }],
            "warnings": ["Cloud discovery returned a partial catalog."],
            "next_cursor": null,
        })
    );

    let read_output = requests[3]
        .function_call_output_text(SKILLS_READ_CALL_ID)
        .ok_or_else(|| anyhow::anyhow!("skills.read output should be sent to the model"))?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&read_output)?,
        json!({
            "resource": SKILL_REFERENCE_URI,
            "contents": SKILL_REFERENCE_CONTENTS,
            "next_cursor": null,
        })
    );
    let repeated_read_output = requests[4]
        .function_call_output_text(SKILLS_READ_AGAIN_CALL_ID)
        .ok_or_else(|| {
            anyhow::anyhow!("repeated skills.read output should be sent to the model")
        })?;
    assert_eq!(read_output, repeated_read_output);
    assert_eq!(
        *provider.reads.lock().await,
        vec![SKILL_MAIN_PROMPT_URI, SKILL_REFERENCE_URI]
    );

    test.submit_turn(&format!("Use ${SKILL_NAME} on the next turn"))
        .await?;
    let requests = response_mock.requests();
    assert_eq!(requests.len(), 6);
    let skill_fragments = requests[5]
        .message_input_texts("user")
        .into_iter()
        .filter(|text| text.starts_with("<skill>"))
        .collect::<Vec<_>>();
    assert_eq!(1, skill_fragments.len());
    assert!(skill_fragments[0].contains(&format!("<name>{SKILL_NAME}</name>")));
    assert!(skill_fragments[0].contains(SKILL_MARKER));
    assert!(skill_fragments[0].contains(SKILL_REFERENCE_URI));
    // Partial discovery retries on the next turn without an MCP or auth invalidation.
    assert_eq!(
        *provider.reads.lock().await,
        vec![
            SKILL_MAIN_PROMPT_URI,
            SKILL_REFERENCE_URI,
            SKILL_MAIN_PROMPT_URI
        ]
    );
    Ok(())
}
