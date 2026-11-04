use std::collections::HashMap;
use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;

use codex_mcp::McpBinding;
use codex_mcp::ToolInfo as McpToolInfo;
use codex_mcp::tool_is_model_visible;
use codex_tools::ToolExposure;
use codex_tools::ToolName;
use tracing::instrument;
use tracing::warn;

use crate::config::Config;
use crate::tools::handlers::McpHandler;
use crate::tools::handlers::McpHandlerDefinition;
use crate::tools::registry::ToolRegistry;

#[derive(Default)]
pub(crate) struct McpHandlerCache {
    handlers: Mutex<HashMap<ToolName, CachedMcpHandler>>,
}

struct CachedMcpHandler {
    schema_max_bytes: Option<NonZeroUsize>,
    handler: Arc<McpHandler>,
}

/// Shares definitions for already-shared catalogs without keeping completed sessions alive.
#[derive(Default)]
pub(crate) struct SharedMcpDefinitions {
    definitions: Mutex<HashMap<McpDefinitionKey, Weak<McpHandlerDefinition>>>,
}

#[derive(Eq, Hash, PartialEq)]
struct McpDefinitionKey {
    input_schema: usize,
    tool_name: ToolName,
    schema_max_bytes: Option<NonZeroUsize>,
}

impl SharedMcpDefinitions {
    fn get_or_create(
        &self,
        tool: &McpToolInfo,
        schema_max_bytes: Option<NonZeroUsize>,
    ) -> Result<Arc<McpHandlerDefinition>, serde_json::Error> {
        let key = McpDefinitionKey {
            // A live definition owns this Arc. The address only selects a candidate;
            // complete prepared metadata equality is still required below.
            input_schema: Arc::as_ptr(&tool.tool.input_schema) as usize,
            tool_name: tool.canonical_tool_name(),
            schema_max_bytes,
        };
        let budget = schema_max_bytes.map(NonZeroUsize::get);
        let existing = self
            .definitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .and_then(Weak::upgrade);
        if let Some(existing) =
            existing.filter(|definition| definition.matches(tool, budget))
        {
            return Ok(existing);
        }

        // Normalization can be expensive and must not hold the manager's index lock.
        let definition = Arc::new(McpHandlerDefinition::new(tool.clone(), budget)?);
        let mut definitions = self
            .definitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = definitions.get(&key).and_then(Weak::upgrade);
        let winner = current
            .as_ref()
            .filter(|current| current.matches(tool, budget))
            .cloned();
        if winner.is_none() {
            definitions.insert(key, Arc::downgrade(&definition));
        }
        // An upgraded or losing definition may own a large schema graph.
        drop(definitions);
        drop(current);
        Ok(winner.unwrap_or(definition))
    }
}

impl McpHandlerCache {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn append_mcp_tools(
        &self,
        binding: &Arc<McpBinding>,
        config: &Config,
        mcp_server_catalog: &codex_mcp::ResolvedMcpCatalog,
        search_tool_enabled: bool,
        shared_definitions: &SharedMcpDefinitions,
        registry: &mut ToolRegistry,
    ) -> HashSet<ToolName> {
        let mut handlers = self
            .handlers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        append_mcp_tools(
            binding.tools(),
            config,
            mcp_server_catalog,
            search_tool_enabled,
            &mut handlers,
            shared_definitions,
            registry,
        )
    }
}

#[instrument(level = "trace", skip_all)]
#[allow(clippy::too_many_arguments)]
fn append_mcp_tools(
    all_mcp_tools: &[McpToolInfo],
    config: &Config,
    mcp_server_catalog: &codex_mcp::ResolvedMcpCatalog,
    search_tool_enabled: bool,
    handlers: &mut HashMap<ToolName, CachedMcpHandler>,
    shared_definitions: &SharedMcpDefinitions,
    registry: &mut ToolRegistry,
) -> HashSet<ToolName> {
    let current_tools: HashSet<_> = all_mcp_tools
        .iter()
        .map(McpToolInfo::canonical_tool_name)
        .collect();
    handlers.retain(|name, _| current_tools.contains(name));

    let tools = all_mcp_tools
        .iter()
        .filter(|tool| tool_is_model_visible(tool));
    let _ = config;
    let exposure = if search_tool_enabled {
        ToolExposure::Deferred
    } else {
        ToolExposure::Direct
    };
    let mut registered_tools = HashSet::new();
    let mut pruned_shared_definitions = false;
    for tool in tools {
        let tool_name = tool.canonical_tool_name();
        let server = mcp_server_catalog.server(&tool.server_name);
        let tool_input_schema_max_bytes =
            server.and_then(|server| server.config().tool_input_schema_max_bytes);
        // Handlers contain immutable tool metadata, not a connection or authorization snapshot.
        // Preserve their identity across equivalent bindings so the search index can also be reused.
        let handler = if let Some(cached) = handlers.get(&tool_name).filter(|cached| {
            cached.handler.tool_info() == tool
                && cached.schema_max_bytes == tool_input_schema_max_bytes
        }) {
            Arc::clone(&cached.handler)
        } else {
            handlers.remove(&tool_name);
            if !pruned_shared_definitions {
                // Sweep once per miss-containing pass, never on the unchanged hot path.
                // Idle managers can retain dead keys/control blocks until their next miss.
                shared_definitions
                    .definitions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .retain(|_, definition| definition.strong_count() > 0);
                pruned_shared_definitions = true;
            }
            let handler = shared_definitions
                .get_or_create(tool, tool_input_schema_max_bytes)
                .map(McpHandler::from_definition);

            let handler = match handler {
                Ok(handler) => Arc::new(handler),
                Err(err) => {
                    warn!("Skipping MCP tool `{tool_name}`: failed to build tool spec: {err}");
                    continue;
                }
            };
            handlers.insert(
                tool_name.clone(),
                CachedMcpHandler {
                    schema_max_bytes: tool_input_schema_max_bytes,
                    handler: Arc::clone(&handler),
                },
            );
            handler
        };

        if registry.register_external_with_exposure(handler, exposure) {
            registered_tools.insert(tool_name);
        }
    }
    registered_tools
}

#[cfg(test)]
#[path = "mcp_tool_exposure_test.rs"]
mod tests;
