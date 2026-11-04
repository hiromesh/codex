use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;

use codex_config::McpServerConfig;
use codex_config::McpServerDisabledReason;
use codex_config::RequirementSource;
use codex_protocol::mcp_policy::EnvironmentMcpPolicy;

use crate::McpProtocolMode;
use crate::server::McpCredentialPolicy;

/// The component that declared an MCP server registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpServerSource {
    Config,
    Compatibility {
        id: String,
    },
    Extension {
        id: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RegistrationPrecedence {
    Config,
    Compatibility,
    Extension(usize),
}

impl RegistrationPrecedence {
    fn tier(self) -> u8 {
        match self {
            Self::Config => 0,
            Self::Compatibility => 1,
            Self::Extension(_) => 2,
        }
    }
}

/// One named MCP server declaration before source resolution.
#[derive(Clone, Debug, PartialEq)]
pub struct McpServerRegistration {
    name: String,
    source: McpServerSource,
    config: McpServerConfig,
    credential_policy: McpCredentialPolicy,
    protocol_mode: Option<McpProtocolMode>,
    precedence: RegistrationPrecedence,
}

impl McpServerRegistration {
    /// Registers host-owned configuration, which may resolve credentials on the host.
    pub fn from_config(name: String, config: McpServerConfig) -> Self {
        Self::new(
            name,
            McpServerSource::Config,
            config,
            RegistrationPrecedence::Config,
            McpCredentialPolicy::HostFallbackAllowed,
        )
    }

    /// Registers executor-discovered configuration without authority to read
    /// host environment credentials.
    pub fn from_executor_config(name: String, config: McpServerConfig) -> Self {
        Self::new(
            name,
            McpServerSource::Config,
            config,
            RegistrationPrecedence::Config,
            McpCredentialPolicy::ExecutorOnly,
        )
    }

    pub fn from_compatibility(
        name: String,
        id: impl Into<String>,
        config: McpServerConfig,
    ) -> Self {
        Self::new(
            name,
            McpServerSource::Compatibility { id: id.into() },
            config,
            RegistrationPrecedence::Compatibility,
            McpCredentialPolicy::HostFallbackAllowed,
        )
    }

    pub fn from_extension(
        name: String,
        id: impl Into<String>,
        contribution_order: usize,
        config: McpServerConfig,
    ) -> Self {
        Self::new(
            name,
            McpServerSource::Extension { id: id.into() },
            config,
            RegistrationPrecedence::Extension(contribution_order),
            McpCredentialPolicy::HostFallbackAllowed,
        )
    }

    /// Overrides the protocol for this registration if it wins HTTP server resolution.
    pub fn with_protocol_mode(mut self, protocol_mode: McpProtocolMode) -> Self {
        self.protocol_mode = Some(protocol_mode);
        self
    }

    fn new(
        name: String,
        source: McpServerSource,
        config: McpServerConfig,
        precedence: RegistrationPrecedence,
        credential_policy: McpCredentialPolicy,
    ) -> Self {
        Self {
            name,
            source,
            config,
            credential_policy,
            protocol_mode: None,
            precedence,
        }
    }
}

/// The authority available for MCP servers running in one environment.
#[derive(Clone, Copy, Debug)]
pub enum McpEnvironmentAuthority<'a> {
    /// The selected environment adds no restrictions to the controller policy.
    Unrestricted,
    /// The owner supplied the final restrictions for this environment.
    Restricted(&'a EnvironmentMcpPolicy),
    /// The attachment is pending or failed, so its owner policy is not available.
    Unavailable,
}

/// One side of an MCP server conflict, including whether it registers or
/// removes the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpServerConflictAction {
    Register(McpServerSource),
    Remove(McpServerSource),
}

/// A same-tier name collision and the final outcome after all precedence is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerConflict {
    pub name: String,
    pub outcome: McpServerConflictAction,
    pub contenders: Vec<McpServerConflictAction>,
}

#[derive(Clone, Debug)]
enum CatalogAction {
    Register(Box<McpServerRegistration>),
    Remove {
        name: String,
        source: McpServerSource,
        precedence: RegistrationPrecedence,
    },
}

impl CatalogAction {
    fn name(&self) -> &str {
        match self {
            Self::Register(registration) => &registration.name,
            Self::Remove { name, .. } => name,
        }
    }

    fn precedence(&self) -> RegistrationPrecedence {
        match self {
            Self::Register(registration) => registration.precedence,
            Self::Remove { precedence, .. } => *precedence,
        }
    }

    fn conflict_action(&self) -> McpServerConflictAction {
        match self {
            Self::Register(registration) => {
                McpServerConflictAction::Register(registration.source.clone())
            }
            Self::Remove { source, .. } => McpServerConflictAction::Remove(source.clone()),
        }
    }
}

/// Mutable inputs used to produce an immutable resolved catalog.
#[derive(Clone, Debug, Default)]
pub struct McpCatalogBuilder {
    actions: Vec<CatalogAction>,
    disabled_server_names: BTreeSet<String>,
    requirements_disabled_server_names: BTreeMap<String, RequirementSource>,
}

impl McpCatalogBuilder {
    pub fn register(&mut self, registration: McpServerRegistration) {
        self.actions
            .push(CatalogAction::Register(Box::new(registration)));
    }

    /// Applies the legacy name-scoped disabled veto after source resolution.
    pub fn disable(&mut self, name: String) {
        self.disabled_server_names.insert(name);
    }

    pub fn remove_compatibility(&mut self, name: String, id: impl Into<String>) {
        self.actions.push(CatalogAction::Remove {
            name,
            source: McpServerSource::Compatibility { id: id.into() },
            precedence: RegistrationPrecedence::Compatibility,
        });
    }

    pub fn remove_extension(
        &mut self,
        name: String,
        id: impl Into<String>,
        contribution_order: usize,
    ) {
        self.actions.push(CatalogAction::Remove {
            name,
            source: McpServerSource::Extension { id: id.into() },
            precedence: RegistrationPrecedence::Extension(contribution_order),
        });
    }

    /// Applies environment authority before resolving immutable server registrations.
    pub fn build_with_environment_authority<'a>(
        mut self,
        mut authority_for_environment: impl FnMut(&str) -> McpEnvironmentAuthority<'a>,
    ) -> ResolvedMcpCatalog {
        for action in &mut self.actions {
            let CatalogAction::Register(registration) = action else {
                continue;
            };
            // Existing managed denials are not attachment-owned.
            if !registration.config.enabled {
                continue;
            }

            let allowed = match authority_for_environment(&registration.config.environment_id) {
                McpEnvironmentAuthority::Unrestricted => true,
                McpEnvironmentAuthority::Unavailable => false,
                McpEnvironmentAuthority::Restricted(policy) => {
                    policy.servers.as_ref().is_none_or(|requirements| {
                        requirements
                            .get(&registration.name)
                            .is_some_and(|requirement| {
                                registration.config.matches_requirement(requirement)
                            })
                    })
                }
            };

            if !allowed {
                registration.config.enabled = false;
                registration.config.disabled_reason = Some(McpServerDisabledReason::Requirements {
                    source: RequirementSource::Unknown,
                });
            }
        }
        self.build()
    }

    pub fn build(mut self) -> ResolvedMcpCatalog {
        // Stable sorting makes action order the tie-breaker when precedence is equal.
        self.actions.sort_by_key(CatalogAction::precedence);

        let mut winners = BTreeMap::<String, CatalogAction>::new();
        let mut actions_by_name_and_tier = BTreeMap::<(String, u8), Vec<&CatalogAction>>::new();
        for action in &self.actions {
            winners.insert(action.name().to_string(), action.clone());
            actions_by_name_and_tier
                .entry((action.name().to_string(), action.precedence().tier()))
                .or_default()
                .push(action);
        }

        let mut conflicts = Vec::new();
        for ((name, _), actions) in actions_by_name_and_tier {
            if actions.len() < 2 {
                continue;
            }
            let Some(outcome) = winners.get(&name).map(CatalogAction::conflict_action) else {
                continue;
            };
            conflicts.push(McpServerConflict {
                name,
                outcome,
                contenders: actions
                    .into_iter()
                    .map(CatalogAction::conflict_action)
                    .collect(),
            });
        }

        let mut disabled_server_names = self.disabled_server_names;
        let mut requirements_disabled_server_names = self.requirements_disabled_server_names;
        let servers = winners
            .into_iter()
            .filter_map(|(name, action)| match action {
                CatalogAction::Register(registration) => {
                    let mut registration = *registration;
                    if !registration.config.enabled {
                        match &registration.config.disabled_reason {
                            Some(
                                McpServerDisabledReason::Requirements { source }
                                | McpServerDisabledReason::ExplicitAndRequirements { source },
                            ) => {
                                requirements_disabled_server_names
                                    .insert(name.clone(), source.clone());
                                if matches!(
                                    registration.config.disabled_reason,
                                    Some(McpServerDisabledReason::ExplicitAndRequirements { .. })
                                ) {
                                    disabled_server_names.insert(name.clone());
                                }
                            }
                            Some(McpServerDisabledReason::Unknown) | None => {
                                disabled_server_names.insert(name.clone());
                            }
                        }
                    }
                    if disabled_server_names.contains(&name) {
                        registration.config.enabled = false;
                    }
                    // Retain an earlier requirements denial so a later custom replacement is blocked.
                    if let Some(source) = requirements_disabled_server_names.get(&name) {
                        registration.config.disable_by_requirements(source.clone());
                    }
                    if matches!(
                        registration.config.auth,
                        codex_config::McpServerAuth::EmaAuth
                    ) {
                        registration.config.enabled = false;
                    }
                    Some((
                        name,
                        ResolvedMcpServer {
                            source: registration.source,
                            config: registration.config,
                            credential_policy: registration.credential_policy,
                            protocol_mode: registration.protocol_mode,
                        },
                    ))
                }
                CatalogAction::Remove { .. } => None,
            })
            .collect();

        ResolvedMcpCatalog {
            actions: self.actions,
            disabled_server_names,
            requirements_disabled_server_names,
            servers,
            conflicts,
        }
    }
}

/// A single winning MCP registration.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedMcpServer {
    source: McpServerSource,
    config: McpServerConfig,
    credential_policy: McpCredentialPolicy,
    protocol_mode: Option<McpProtocolMode>,
}

impl ResolvedMcpServer {
    pub fn source(&self) -> &McpServerSource {
        &self.source
    }

    pub fn config(&self) -> &McpServerConfig {
        &self.config
    }

    pub(crate) fn credential_policy(&self) -> McpCredentialPolicy {
        self.credential_policy
    }

    pub fn protocol_mode(&self) -> Option<McpProtocolMode> {
        self.protocol_mode
    }
}

/// Immutable result of MCP registration resolution.
#[derive(Clone, Debug, Default)]
pub struct ResolvedMcpCatalog {
    actions: Vec<CatalogAction>,
    disabled_server_names: BTreeSet<String>,
    requirements_disabled_server_names: BTreeMap<String, RequirementSource>,
    servers: BTreeMap<String, ResolvedMcpServer>,
    conflicts: Vec<McpServerConflict>,
}

impl ResolvedMcpCatalog {
    pub fn builder() -> McpCatalogBuilder {
        McpCatalogBuilder::default()
    }

    pub fn to_builder(&self) -> McpCatalogBuilder {
        McpCatalogBuilder {
            actions: self.actions.clone(),
            disabled_server_names: self.disabled_server_names.clone(),
            requirements_disabled_server_names: self.requirements_disabled_server_names.clone(),
        }
    }

    pub fn server(&self, name: &str) -> Option<&ResolvedMcpServer> {
        self.servers.get(name)
    }

    pub fn configured_servers(&self) -> HashMap<String, McpServerConfig> {
        self.servers
            .iter()
            .map(|(name, server)| (name.clone(), server.config.clone()))
            .collect()
    }

    /// Returns whether both catalogs have the same winning servers and sources.
    pub fn has_same_servers(&self, other: &Self) -> bool {
        self.servers == other.servers
    }

    /// Replaces the resolved server set while preserving known sources.
    ///
    /// # Panics
    ///
    /// Panics if a materialized server is missing from the catalog.
    pub fn with_materialized_servers(&self, servers: HashMap<String, McpServerConfig>) -> Self {
        let mut builder = McpCatalogBuilder {
            disabled_server_names: self.disabled_server_names.clone(),
            requirements_disabled_server_names: self.requirements_disabled_server_names.clone(),
            ..Default::default()
        };
        for (name, config) in servers {
            #[expect(
                clippy::expect_used,
                reason = "materialized servers must have catalog registrations"
            )]
            let previous = self
                .server(&name)
                .expect("materialized MCP server must have a catalog registration");
            let source = previous.source.clone();
            let precedence = match &source {
                McpServerSource::Config => RegistrationPrecedence::Config,
                McpServerSource::Compatibility { .. } => RegistrationPrecedence::Compatibility,
                McpServerSource::Extension { .. } => RegistrationPrecedence::Extension(0),
            };
            let credential_policy = previous.credential_policy();
            let mut registration =
                McpServerRegistration::new(name, source, config, precedence, credential_policy);
            registration.protocol_mode = previous.protocol_mode();
            builder.register(registration);
        }
        builder.build()
    }

    pub fn conflicts(&self) -> &[McpServerConflict] {
        &self.conflicts
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
