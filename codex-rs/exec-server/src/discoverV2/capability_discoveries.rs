//! Loads metadata within the request's sandbox and builds a bounded response.

use std::io;
use std::sync::Arc;

use codex_file_system::ExecutorFileSystem;
use codex_skills::parse_skill_frontmatter_metadata;
use codex_skills::parse_skill_metadata;
use futures::StreamExt;

use crate::CapabilityRootDiscoverRequest;
use crate::CapabilityRootDiscovery;
use crate::DiscoverV2CapabilitiesResponse;
use crate::ExecutorSkill;
use crate::LocalFileSystem;
use crate::SkillDiscoveryRoot;
use crate::capability_discovery::MAX_CONCURRENT_ROOTS;
use crate::capability_discovery::discover_root;
use crate::discover_v2::capability_locations::CapabilityLocation;
use crate::discover_v2::capability_locations::CapabilityLocations;

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CAPABILITIES: usize = 2_048;
const MAX_WARNINGS: usize = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CapabilityDiscovery {
    skills: Vec<ExecutorSkill>,
    warnings: Vec<String>,
}

pub(super) async fn load_capability_discoveries(
    file_system: &LocalFileSystem,
    locations: &CapabilityLocations,
    sandbox: Option<&crate::FileSystemSandboxContext>,
) -> io::Result<Arc<DiscoverV2CapabilitiesResponse>> {
    let sandbox = sandbox.filter(|sandbox| sandbox.should_read_from_sandbox());
    let warnings = if !locations.warnings.is_empty() {
        vec!["some capability locations could not be resolved; discovery may be incomplete".into()]
    } else {
        Vec::new()
    };
    let response = match sandbox {
        Some(sandbox) if !locations.locations.is_empty() => {
            file_system
                .sandboxed()?
                .load_sandboxed_capability_discoveries(
                    locations.locations.clone(),
                    warnings,
                    sandbox,
                )
                .await?
        }
        _ => load_capability_discovery_batch(file_system, &locations.locations, warnings).await?,
    };
    Ok(Arc::new(response))
}

/// Loads roots concurrently and bounds the combined response.
pub(crate) async fn load_capability_discovery_batch(
    file_system: &LocalFileSystem,
    locations: &[CapabilityLocation],
    warnings: Vec<String>,
) -> io::Result<DiscoverV2CapabilitiesResponse> {
    let mut response = DiscoverV2CapabilitiesResponse {
        warnings,
        ..Default::default()
    };
    let mut budget = ResponseBudget::default();
    // Use V1 concurrency; admit results in location order.
    // Yield owned locations to satisfy the RPC router's Send future bound.
    let mut discoveries = futures::stream::iter(locations.iter().cloned())
        .map(|location| load_capability_discovery(file_system, location))
        .buffered(MAX_CONCURRENT_ROOTS)
        .enumerate();
    while let Some((index, discovery)) = discoveries.next().await {
        let mut discovery = discovery?;
        if !discovery.warnings.is_empty() {
            discovery.warnings = vec![
                "some capability metadata could not be loaded; discovery may be incomplete".into(),
            ];
        }
        append_capability_discovery(&mut response, discovery, &mut budget)?;
        // A rejected oversized entry alone does not rule out smaller entries later.
        if (budget.capabilities == MAX_CAPABILITIES || budget.bytes == MAX_RESPONSE_BYTES)
            && index + 1 < locations.len()
        {
            budget.truncated = true;
            break;
        }
    }
    finish_response(response, budget.truncated)
}

async fn load_capability_discovery(
    file_system: &LocalFileSystem,
    location: CapabilityLocation,
) -> io::Result<CapabilityDiscovery> {
    // Missing roots are normal.
    match file_system
        .get_metadata(&location.root, Default::default(), /*sandbox*/ None)
        .await
    {
        Ok(metadata) if metadata.is_directory => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(CapabilityDiscovery::default());
        }
        Ok(_) => return Ok(CapabilityDiscovery::default()),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Ok(CapabilityDiscovery::default());
        }
        Err(error) => {
            return Ok(CapabilityDiscovery {
                warnings: vec![format!(
                    "could not inspect capability root {}: {error}",
                    location.root
                )],
                ..Default::default()
            });
        }
    }
    let discovery = discover_root(
        file_system,
        CapabilityRootDiscoverRequest {
            id: location.standalone_root_id.clone().unwrap_or_default(),
            path: location.root.clone(),
            sandbox: None,
        },
    )
    .await;
    Ok(parse_root(location, discovery))
}

fn parse_root(
    location: CapabilityLocation,
    discovery: CapabilityRootDiscovery,
) -> CapabilityDiscovery {
    let mut warnings = discovery.warnings;
    warnings.extend(discovery.error);

    // Reuse the contents already read by V1.
    let mut skills = Vec::new();
    for skill in discovery.skills {
        let parsed = parse_skill_frontmatter_metadata(&skill.instructions.contents, || {
            skill
                .instructions
                .path
                .parent()
                .and_then(|path| path.basename())
                .map(|name| name.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "skill".to_string())
        });
        let parsed = match parsed {
            Ok(parsed) => parsed,
            Err(error) => {
                warnings.push(format!(
                    "invalid skill metadata at {}: {error}",
                    skill.instructions.path
                ));
                continue;
            }
        };
        let path = skill.instructions.path;
        let metadata = skill.metadata.as_ref().and_then(|metadata| {
            parse_skill_metadata(&metadata.contents).map_err(|error| {
                tracing::warn!(path = %metadata.path, %error, "ignoring invalid discovered skill metadata");
            }).ok()
        }).unwrap_or_default();
        skills.push(ExecutorSkill {
            name: parsed.name,
            description: parsed.description,
            short_description: parsed.short_description,
            path,
            discovery_root: location
                .standalone_root_id
                .as_ref()
                .map(|id| SkillDiscoveryRoot {
                    id: id.clone(),
                    path: location.root.clone(),
                }),
            dependencies: metadata.dependencies,
            policy: metadata.policy,
        });
    }
    CapabilityDiscovery { skills, warnings }
}

#[derive(Default)]
struct ResponseBudget {
    capabilities: usize,
    bytes: usize,
    truncated: bool,
}

impl ResponseBudget {
    fn admit(&mut self, capabilities: usize, bytes: usize) -> bool {
        if self.capabilities + capabilities > MAX_CAPABILITIES
            || self.bytes + bytes > MAX_RESPONSE_BYTES
        {
            self.truncated = true;
            return false;
        }
        self.capabilities += capabilities;
        self.bytes += bytes;
        true
    }
}

/// Appends within budget; duplicate skills count until consumers deduplicate them.
fn append_capability_discovery(
    response: &mut DiscoverV2CapabilitiesResponse,
    discovery: CapabilityDiscovery,
    budget: &mut ResponseBudget,
) -> io::Result<()> {
    response.warnings.extend(
        discovery
            .warnings
            .into_iter()
            .take(MAX_WARNINGS.saturating_sub(response.warnings.len())),
    );

    for skill in discovery.skills {
        if budget.capabilities == MAX_CAPABILITIES {
            budget.truncated = true;
            break;
        }
        let size = serde_json::to_vec(&skill).map_err(io::Error::other)?.len();
        if budget.admit(/*capabilities*/ 1, size) {
            response.skills.push(skill);
        }
    }
    Ok(())
}

fn finish_response(
    mut response: DiscoverV2CapabilitiesResponse,
    truncated: bool,
) -> io::Result<DiscoverV2CapabilitiesResponse> {
    // Keep standalone skill precedence.
    response.warnings.truncate(MAX_WARNINGS);

    enforce_response_size(&mut response, truncated)?;
    Ok(response)
}

// Include JSON overhead and warnings in the final size check.
fn enforce_response_size(
    response: &mut DiscoverV2CapabilitiesResponse,
    truncated: bool,
) -> io::Result<()> {
    if truncated
        || serde_json::to_vec(&*response)
            .map_err(io::Error::other)?
            .len()
            > MAX_RESPONSE_BYTES
    {
        response.warnings.truncate(MAX_WARNINGS - 1);
        response
            .warnings
            .insert(0, "capability discovery size limit reached".to_string());
        while serde_json::to_vec(&*response)
            .map_err(io::Error::other)?
            .len()
            > MAX_RESPONSE_BYTES
        {
            // Drop extra warnings first; keep the limit warning.
            if response.warnings.len() > 1 {
                response.warnings.truncate(1);
            } else if response.skills.pop().is_none() {
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/capability_discoveries_tests.rs"]
mod tests;
