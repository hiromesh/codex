mod world_state;

use std::sync::Arc;

use codex_extension_api::ContextContributor;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::WorldStateContributionInput;
use codex_extension_api::WorldStateSectionContribution;
use codex_http_client::HttpClientFactory;
use codex_login::AuthManager;

use crate::world_state::git_attribution_world_state_section;

/// Contributes model instructions for agent-created git commits and pull requests.
#[derive(Clone)]
struct GitAttributionExtension;

impl ContextContributor for GitAttributionExtension {
    fn contribute_world_state<'a>(
        &'a self,
        _input: WorldStateContributionInput<'a>,
    ) -> ExtensionFuture<'a, Vec<WorldStateSectionContribution>> {
        // Attribution policy is backend-owned. API-key auth has no Codex backend
        // policy source, so the extension stays disabled without querying one.
        Box::pin(async move { vec![git_attribution_world_state_section(false)] })
    }
}

/// Installs the git-attribution contributor into the extension registry.
pub fn install<C: Sync>(
    registry: &mut ExtensionRegistryBuilder<C>,
    _auth_manager: Arc<AuthManager>,
    _base_url: String,
    _http_client_factory: HttpClientFactory,
) {
    registry.prompt_contributor(Arc::new(GitAttributionExtension));
}
