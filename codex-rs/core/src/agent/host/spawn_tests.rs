#![cfg(test)]

//! Tests same-manager child bindings and invocation lifetime.

use crate::CodexThread;
use crate::ConfigRefreshOutcome;
use crate::HostToolInvocationLookup;
use crate::ThreadManager;
use crate::agent::host::InvocationReservation;
use crate::config::ConfigBuilder;
use crate::thread_manager::StartThreadOptions;
use crate::tools::context::ToolCallSource;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use codex_config::test_support::CloudConfigBundleFixture;
use codex_execpolicy::Decision;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_tools::ToolName;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

async fn spawn_call(parent: &CodexThread) -> anyhow::Result<ToolInvocation> {
    parent
        .thread_extension_data()
        .insert(HostToolInvocationLookup);
    let session = &parent.session;
    let step = session
        .capture_step_context(session.new_default_turn().await, &CancellationToken::new())
        .await?;
    Ok(ToolInvocation {
        session: Arc::clone(session),
        turn: Arc::clone(&step.turn),
        step_context: step,
        cancellation_token: CancellationToken::new(),
        tracker: Default::default(),
        call_id: "spawn-call".into(),
        tool_name: ToolName::plain("host_spawn"),
        source: ToolCallSource::Direct,
        payload: ToolPayload::Function {
            arguments: "{}".into(),
        },
        native_call: None,
        environments: Default::default(),
    })
}

#[tokio::test]
async fn prepare_child_retains_loaded_exec_policy_requirements_after_mcp_refresh()
-> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let builder =
        || ConfigBuilder::without_managed_config_for_tests().codex_home(home.path().to_path_buf());
    let mut config = builder()
        .cloud_config_bundle(CloudConfigBundleFixture::loader_with_enterprise_requirement(
            "[[rules.prefix_rules]]\npattern = [{ token = 'managed-command' }]\ndecision = 'forbidden'",
        ))
        .build()
        .await?;
    config.features.enable(Feature::MultiAgentV2)?;
    let manager = ThreadManager::with_models_provider_and_home_for_tests(
        CodexAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        home.path().to_path_buf(),
        Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
    );
    let parent = manager
        .start_thread(StartThreadOptions::new(config))
        .await?
        .thread;
    let startup = parent.config().await;
    let loaded = startup
        .config_layer_stack
        .requirements()
        .exec_policy
        .as_deref()
        .expect("startup has a managed exec policy");
    assert_eq!(
        parent
            .refresh_mcp_config(Arc::clone(&startup), builder().build().await?)
            .await,
        ConfigRefreshOutcome::Published
    );
    let call = spawn_call(&parent).await?;
    assert_eq!(
        call.turn
            .config
            .config_layer_stack
            .requirements()
            .exec_policy,
        None
    );
    let reservation = InvocationReservation::default();
    reservation.reserve(&parent.session, &call.turn, &call.call_id)?;
    let _guard = reservation.activate(&call);
    let invocation = parent.tool_invocation(&call.turn.sub_id, &call.call_id)?;
    let prepared = parent.prepare_child(&invocation).await?;
    assert_eq!(
        prepared.exec_policy_source_requirements.as_deref(),
        Some(loaded)
    );
    assert_eq!(
        prepared
            .exec_policy
            .check(&["managed-command".into()], &|_| Decision::Allow)
            .decision,
        Decision::Forbidden
    );
    parent.shutdown_and_wait().await?;
    Ok(())
}
