use std::fs;

use codex_exec_server::CapabilityRootDiscoverRequest;
use codex_exec_server::CapabilityRootsDiscoverParams;
use codex_exec_server::FileSystemEnvironmentAccessor;
use codex_exec_server::LOCAL_FS;
use codex_exec_server::discover_capability_roots;
use codex_protocol::protocol::Product;
use codex_skills::EnvironmentSkillMetadata;
use codex_skills::SkillDependencies;
use codex_skills::SkillPolicy;
use codex_skills::SkillToolDependency;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

use super::load_environment_skills_from_discovery;
use super::load_environment_skills_from_root;

#[tokio::test]
async fn executor_bundle_preserves_skill_metadata() {
    let plugin = tempdir().expect("tempdir");
    let skills_root = plugin.path().join("skills");
    let skill_path = skills_root.join("search/SKILL.md");
    fs::create_dir_all(skill_path.parent().expect("skill parent")).expect("skill directory");
    fs::write(
        &skill_path,
        "---\nname: search\ndescription: Search the project.\n---\n\nSearch.\n",
    )
    .expect("skill");

    let root_uri = PathUri::from_host_native_path(&skills_root).expect("skills root URI");
    let existing = load_environment_skills_from_root(
        &FileSystemEnvironmentAccessor::unrestricted(&LOCAL_FS),
        &root_uri,
        /*restriction_product*/ None,
    )
    .await;
    let response = discover_capability_roots(
        LOCAL_FS.as_ref(),
        CapabilityRootsDiscoverParams {
            roots: vec![CapabilityRootDiscoverRequest {
                id: "skills-only".to_string(),
                path: root_uri,
                sandbox: None,
            }],
        },
    )
    .await
    .expect("capability discovery");
    let discovery = response.roots.first().expect("discovered root");
    let bundled =
        load_environment_skills_from_discovery(discovery, /*restriction_product*/ None);

    assert_eq!(bundled.warnings, existing.warnings);
    assert_eq!(
        bundled
            .skills
            .iter()
            .map(|skill| skill.metadata.clone())
            .collect::<Vec<_>>(),
        existing.skills
    );
    assert_eq!(bundled.skills[0].metadata.name, "search");
}
