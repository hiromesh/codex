pub(crate) mod command_runner;
pub(crate) mod discovery;
pub(crate) mod dispatcher;
mod matcher;
pub(crate) mod mcp_runner;
pub(crate) mod output_parser;
pub(crate) mod schema_loader;

pub(crate) use matcher::HookMatcher;

use crate::events::compact::PostCompactRequest;
use crate::events::compact::PreCompactOutcome;
use crate::events::compact::PreCompactRequest;
use crate::events::compact::StatelessHookOutcome;
use crate::events::interrupt::InterruptOutcome;
use crate::events::interrupt::InterruptRequest;
use crate::events::permission_request::PermissionRequestOutcome;
use crate::events::permission_request::PermissionRequestRequest;
use crate::events::post_tool_use::PostToolUseOutcome;
use crate::events::post_tool_use::PostToolUseRequest;
use crate::events::pre_tool_use::PreToolUseOutcome;
use crate::events::pre_tool_use::PreToolUseRequest;
use crate::events::session_end::SessionEndOutcome;
use crate::events::session_end::SessionEndRequest;
use crate::events::session_start::SessionStartOutcome;
use crate::events::session_start::SessionStartRequest;
use crate::events::stop::StopOutcome;
use crate::events::stop::StopRequest;
use crate::events::user_prompt_submit::UserPromptSubmitOutcome;
use crate::events::user_prompt_submit::UserPromptSubmitRequest;
use crate::mcp::HookMcpExecutor;
use crate::output_spill::AdditionalContextLimit;
use codex_config::ConfigLayerStack;
use codex_protocol::protocol::HookEventName;
use codex_protocol::protocol::HookExecutionMode;
use codex_protocol::protocol::HookHandlerType;
use codex_protocol::protocol::HookRunSummary;
use codex_protocol::protocol::HookSource;
use codex_protocol::protocol::HookTrustStatus;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use command_runner::CommandHookRuntime;

#[derive(Debug, Clone)]
pub(crate) struct CommandShell {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfiguredHandler {
    /// Internally admitted cleanup hook, enabled independently of per-hook state.
    pub builtin: bool,
    pub event_name: codex_protocol::protocol::HookEventName,
    pub matcher: Option<HookMatcher>,
    pub timeout_sec: u64,
    pub status_message: Option<String>,
    pub additional_context_limit: AdditionalContextLimit,
    pub source_path: HandlerSourcePath,
    pub source: HookSource,
    pub display_order: i64,
    pub kind: ConfiguredHandlerKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HandlerSourcePath {
    Local(AbsolutePathBuf),
    Environment {
        environment_id: String,
        cwd: PathUri,
        source_path: PathUri,
    },
}

impl HandlerSourcePath {
    /// Use the environment's working directory and withhold controller-owned transcript paths.
    fn event_input<'a>(&self, input: &'a Value) -> Cow<'a, Value> {
        match self {
            Self::Environment { cwd, .. } => {
                let mut input = input.clone();
                input["cwd"] = Value::String(cwd.inferred_native_path_string());
                input["transcript_path"] = Value::Null;
                if let Some(path) = input.get_mut("agent_transcript_path") {
                    *path = Value::Null;
                }
                Cow::Owned(input)
            }
            Self::Local(_) => Cow::Borrowed(input),
        }
    }
}

impl From<AbsolutePathBuf> for HandlerSourcePath {
    fn from(path: AbsolutePathBuf) -> Self {
        Self::Local(path)
    }
}

impl std::fmt::Display for HandlerSourcePath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(path) => write!(formatter, "{}", path.display()),
            Self::Environment { source_path, .. } => {
                write!(formatter, "{}", source_path.inferred_native_path_string())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfiguredHandlerKind {
    Command {
        command: String,
        env: HashMap<String, String>,
        r#async: bool,
    },
    McpTool {
        server: String,
        tool: String,
        input: serde_json::Map<String, serde_json::Value>,
    },
}

#[derive(Debug)]
pub(crate) struct HandlerRunResult {
    pub context_metadata: codex_protocol::models::ContentItemMetadata,
    pub started_at: i64,
    pub completed_at: i64,
    pub duration_ms: i64,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
}

impl ConfiguredHandler {
    /// Internal memory work runs policy and executor hooks, not user or project hooks.
    pub(crate) fn runs_for_memory_consolidation(&self) -> bool {
        match self.source {
            HookSource::User
            | HookSource::Project
            | HookSource::SessionFlags => false,
            HookSource::System
            | HookSource::Mdm
            | HookSource::CloudRequirements
            | HookSource::CloudManagedConfig
            | HookSource::LegacyManagedConfigFile
            | HookSource::LegacyManagedConfigMdm
            // Required hooks can have unknown attribution; retain them fail-closed.
            | HookSource::Unknown => true,
        }
    }

    pub(crate) fn execution_mode(&self) -> HookExecutionMode {
        match self.kind {
            ConfiguredHandlerKind::Command { r#async: true, .. } => HookExecutionMode::Async,
            ConfiguredHandlerKind::Command { r#async: false, .. }
            | ConfiguredHandlerKind::McpTool { .. } => HookExecutionMode::Sync,
        }
    }

    /// Only synchronous hooks can apply control effects.
    pub(crate) fn can_apply_control_effects(&self) -> bool {
        self.execution_mode() == HookExecutionMode::Sync
    }

    pub fn run_id(&self) -> String {
        format!(
            "{}:{}:{}",
            self.event_name_label(),
            self.display_order,
            self.source_path
        )
    }

    fn event_name_label(&self) -> &'static str {
        match self.event_name {
            codex_protocol::protocol::HookEventName::PreToolUse => "pre-tool-use",
            codex_protocol::protocol::HookEventName::PermissionRequest => "permission-request",
            codex_protocol::protocol::HookEventName::PostToolUse => "post-tool-use",
            codex_protocol::protocol::HookEventName::PreCompact => "pre-compact",
            codex_protocol::protocol::HookEventName::PostCompact => "post-compact",
            codex_protocol::protocol::HookEventName::PreModelRequest => "pre-model-request",
            codex_protocol::protocol::HookEventName::PostModelRequest => "post-model-request",
            codex_protocol::protocol::HookEventName::SessionStart => "session-start",
            codex_protocol::protocol::HookEventName::SessionEnd => "session-end",
            codex_protocol::protocol::HookEventName::UserPromptSubmit => "user-prompt-submit",
            codex_protocol::protocol::HookEventName::SubagentStart => "subagent-start",
            codex_protocol::protocol::HookEventName::SubagentStop => "subagent-stop",
            codex_protocol::protocol::HookEventName::Stop => "stop",
            codex_protocol::protocol::HookEventName::Interrupt => "interrupt",
        }
    }

    fn handler_type(&self) -> HookHandlerType {
        match &self.kind {
            ConfiguredHandlerKind::Command { .. } => HookHandlerType::Command,
            ConfiguredHandlerKind::McpTool { .. } => HookHandlerType::McpTool,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookListEntryHandler {
    Command { command: String, r#async: bool },
    McpTool { server: String, tool: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookListEntry {
    /// Builtin hooks remain available internally but are omitted from the public hooks list.
    pub builtin: bool,
    pub key: String,
    pub event_name: HookEventName,
    pub handler: HookListEntryHandler,
    pub matcher: Option<String>,
    pub timeout_sec: u64,
    pub status_message: Option<String>,
    pub additional_context_limit: Option<usize>,
    pub source_path: AbsolutePathBuf,
    pub source: HookSource,
    /// The owning execution environment, if this hook came from its requirements.
    pub environment_id: Option<String>,
    pub display_order: i64,
    pub enabled: bool,
    pub is_managed: bool,
    pub current_hash: String,
    pub trust_status: HookTrustStatus,
}

#[derive(Clone)]
pub(crate) struct ClaudeHooksEngine {
    pub(crate) enabled: bool,
    // Per-turn views share configuration; executor overlays copy handlers on mutation.
    pub(crate) handlers: Arc<Vec<ConfiguredHandler>>,
    /// Retain the session inventory without copying it for every hook dispatch.
    pub(crate) hook_entries: Arc<Vec<HookListEntry>>,
    pub(crate) warnings: Arc<[String]>,
    required_load_errors: Arc<[String]>,
    pub(crate) command_runtime: CommandHookRuntime,
    pub(crate) mcp_executor: Arc<dyn HookMcpExecutor>,
}

impl ClaudeHooksEngine {
    pub(crate) fn new(
        enabled: bool,
        bypass_hook_trust: bool,
        config_layer_stack: Option<&ConfigLayerStack>,
        command_runtime: CommandHookRuntime,
        mcp_executor: Arc<dyn HookMcpExecutor>,
    ) -> Self {
        if !enabled {
            return Self {
                enabled,
                handlers: Arc::default(),
                hook_entries: Arc::default(),
                warnings: Arc::default(),
                required_load_errors: Arc::default(),
                command_runtime,
                mcp_executor,
            };
        }

        let _ = schema_loader::generated_hook_schemas();
        let mut discovered = discovery::discover_handlers(
            config_layer_stack,
            bypass_hook_trust,
            discovery::DiscoveryMode::Runtime,
        );
        if !enabled {
            discovered.handlers.retain(|handler| handler.builtin);
            discovered.hook_entries.clear();
            // Disabled ordinary hooks must not emit warnings or reject session startup.
            discovered.warnings.clear();
            discovered.required_load_errors.clear();
        }
        Self {
            enabled,
            handlers: Arc::new(discovered.handlers),
            hook_entries: Arc::new(discovered.hook_entries),
            warnings: discovered.warnings.into(),
            required_load_errors: discovered.required_load_errors.into(),
            command_runtime,
            mcp_executor,
        }
    }

    pub(crate) fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub(crate) fn required_load_errors(&self) -> &[String] {
        &self.required_load_errors
    }

    pub(crate) fn preview_session_start(
        &self,
        request: &SessionStartRequest,
    ) -> Vec<HookRunSummary> {
        crate::events::session_start::preview(&self.handlers, request)
    }

    pub(crate) fn preview_pre_tool_use(&self, request: &PreToolUseRequest) -> Vec<HookRunSummary> {
        crate::events::pre_tool_use::preview(&self.handlers, request)
    }

    pub(crate) fn preview_permission_request(
        &self,
        request: &PermissionRequestRequest,
    ) -> Vec<HookRunSummary> {
        crate::events::permission_request::preview(&self.handlers, request)
    }

    pub(crate) fn max_permission_request_timeout(&self) -> Duration {
        Duration::from_secs(
            self.handlers
                .iter()
                .filter(|handler| {
                    handler.event_name == HookEventName::PermissionRequest
                        && handler.can_apply_control_effects()
                })
                .map(|handler| handler.timeout_sec)
                .max()
                .unwrap_or_default(),
        )
    }

    pub(crate) fn preview_post_tool_use(
        &self,
        request: &PostToolUseRequest,
    ) -> Vec<HookRunSummary> {
        crate::events::post_tool_use::preview(&self.handlers, request)
    }

    pub(crate) async fn run_session_start(
        &self,
        request: SessionStartRequest,
        turn_id: Option<String>,
    ) -> SessionStartOutcome {
        crate::events::session_start::run(self, request, turn_id).await
    }

    pub(crate) async fn run_pre_tool_use(&self, request: PreToolUseRequest) -> PreToolUseOutcome {
        crate::events::pre_tool_use::run(self, request).await
    }

    pub(crate) async fn run_permission_request(
        &self,
        request: PermissionRequestRequest,
    ) -> PermissionRequestOutcome {
        crate::events::permission_request::run(self, request).await
    }

    pub(crate) async fn run_post_tool_use(
        &self,
        request: PostToolUseRequest,
    ) -> PostToolUseOutcome {
        let mut outcome = crate::events::post_tool_use::run(self, request).await;
        if let Some(feedback_message) = outcome.feedback_message.take() {
            outcome.feedback_message = Some(
                self.command_runtime
                    .output_spiller()
                    .maybe_spill_text(feedback_message)
                    .await,
            );
        }
        outcome
    }

    pub(crate) fn preview_pre_compact(&self, request: &PreCompactRequest) -> Vec<HookRunSummary> {
        crate::events::compact::preview_pre(&self.handlers, request)
    }

    pub(crate) async fn run_pre_compact(&self, request: PreCompactRequest) -> PreCompactOutcome {
        crate::events::compact::run_pre(self, request).await
    }

    pub(crate) fn preview_post_compact(&self, request: &PostCompactRequest) -> Vec<HookRunSummary> {
        crate::events::compact::preview_post(&self.handlers, request)
    }

    pub(crate) async fn run_post_compact(
        &self,
        request: PostCompactRequest,
    ) -> StatelessHookOutcome {
        crate::events::compact::run_post(self, request).await
    }

    pub(crate) fn preview_user_prompt_submit(
        &self,
        request: &UserPromptSubmitRequest,
    ) -> Vec<HookRunSummary> {
        crate::events::user_prompt_submit::preview(&self.handlers, request)
    }

    pub(crate) async fn run_user_prompt_submit(
        &self,
        request: UserPromptSubmitRequest,
    ) -> UserPromptSubmitOutcome {
        crate::events::user_prompt_submit::run(self, request).await
    }

    pub(crate) fn preview_stop(&self, request: &StopRequest) -> Vec<HookRunSummary> {
        crate::events::stop::preview(&self.handlers, request)
    }

    pub(crate) fn preview_session_end(&self) -> Vec<HookRunSummary> {
        crate::events::session_end::preview(&self.handlers)
    }

    pub(crate) async fn run_session_end(&self, request: SessionEndRequest) -> SessionEndOutcome {
        crate::events::session_end::run(self, request).await
    }

    pub(crate) async fn run_stop(&self, request: StopRequest) -> StopOutcome {
        let mut outcome = crate::events::stop::run(self, request).await;
        outcome.continuation_fragments = self
            .command_runtime
            .output_spiller()
            .maybe_spill_prompt_fragments(outcome.continuation_fragments)
            .await;
        outcome
    }

    pub(crate) fn preview_interrupt(&self) -> Vec<HookRunSummary> {
        crate::events::interrupt::preview(&self.handlers)
    }

    pub(crate) async fn run_interrupt(&self, request: InterruptRequest) -> InterruptOutcome {
        crate::events::interrupt::run(self, request).await
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
