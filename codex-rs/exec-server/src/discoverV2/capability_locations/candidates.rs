//! Retains standalone skill candidates and assembles their locations.

use super::CapabilityLocation;
use super::CapabilityLocations;
use super::MAX_LOCATIONS;
use crate::LocalFileSystem;
use crate::SkillDiscoveryRoot;
use codex_file_system::ExecutorFileSystem;
use codex_file_watcher::WatchPath;
use std::io;

const MAX_LOCATION_WARNINGS: usize = 64;

/// Existing candidates plus watch paths for existing and not-yet-created roots.
pub(super) struct CandidateCapabilityLocations {
    candidates: Vec<CandidateCapabilityLocation>,
    watches: Vec<WatchPath>,
}

/// A standalone skill root whose locations can be resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CandidateCapabilityLocation {
    Standalone(SkillDiscoveryRoot),
}

impl CandidateCapabilityLocations {
    /// Resolves existing skill roots into one bounded location inventory.
    pub(super) async fn resolve(
        fs: &LocalFileSystem,
        skill_roots: Vec<SkillDiscoveryRoot>,
    ) -> io::Result<CapabilityLocations> {
        let mut standalone = skill_roots;
        let mut seen = std::collections::HashSet::new();
        standalone.retain(|root| seen.insert(root.path.clone()));
        let candidates = standalone
            .into_iter()
            .map(CandidateCapabilityLocation::Standalone)
            .collect::<Vec<_>>();

        let mut watches = Vec::new();
        let mut retained_candidates = Vec::new();
        for candidate in candidates {
            let CandidateCapabilityLocation::Standalone(root) = &candidate;
            let exists = match fs
                .get_metadata(&root.path, Default::default(), /*sandbox*/ None)
                .await
            {
                Ok(metadata) => metadata.is_directory,
                Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                Err(error) => return Err(error),
            };
            // Missing roots use the watcher's ancestor fallback until they are created.
            watches.push(WatchPath {
                path: root.path.to_abs_path()?.into_path_buf(),
                recursive: true,
            });
            if exists {
                retained_candidates.push(candidate);
            }
        }
        let candidates = Self {
            candidates: retained_candidates,
            watches,
        };
        let mut resolved = Vec::new();
        for candidate in &candidates.candidates {
            resolved.push(candidates.resolve_candidates(fs, candidate).await?);
        }
        Ok(candidates.assemble(resolved))
    }

    async fn resolve_candidates(
        &self,
        _fs: &LocalFileSystem,
        candidate: &CandidateCapabilityLocation,
    ) -> io::Result<CapabilityLocations> {
        match candidate {
            CandidateCapabilityLocation::Standalone(root) => Ok(CapabilityLocations {
                locations: vec![CapabilityLocation {
                    root: root.path.clone(),
                    standalone_root_id: Some(root.id.clone()),
                }],
                ..Default::default()
            }),
        }
    }

    /// Combines and bounds inventory; Core applies capability enablement.
    fn assemble(&self, resolved_candidates: Vec<CapabilityLocations>) -> CapabilityLocations {
        let mut result = CapabilityLocations {
            watches: self.watches.clone(),
            ..Default::default()
        };
        // Combine standalone skill locations in discovery order.
        for candidate in resolved_candidates {
            result.locations.extend(candidate.locations);
            result.warnings.extend(candidate.warnings);
        }

        if result.locations.len() > MAX_LOCATIONS {
            result.warnings.truncate(MAX_LOCATION_WARNINGS - 1);
            result.warnings.push(format!(
                "capability discovery reached its {MAX_LOCATIONS}-location limit"
            ));
            result.locations.truncate(MAX_LOCATIONS);
        }
        result.warnings.truncate(MAX_LOCATION_WARNINGS);
        result
    }
}
