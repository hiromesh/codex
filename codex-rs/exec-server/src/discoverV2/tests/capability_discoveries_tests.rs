//! Covers metadata parity and inventory discovery independent of enablement.

use pretty_assertions::assert_eq;

use super::MAX_CAPABILITIES;
use super::MAX_RESPONSE_BYTES;
use super::MAX_WARNINGS;
use crate::DiscoverV2CapabilitiesRequest;
use crate::ExecutorSkill;
use crate::LocalFileSystem;
use crate::SkillDiscoveryRoot;
use crate::discover_v2::capability_locations::tests::request;
use crate::discover_v2::capability_locations::tests::write;
use crate::discover_v2::capability_manager::CapabilityManager;
use codex_utils_path_uri::PathUri;

const SKILL: &str =
    "---\nname: deploy\ndescription: Deploy the service.\n---\nPRIVATE_INSTRUCTION_BODY_SENTINEL\n";

#[tokio::test]
async fn discovery_preserves_inventory_and_skill_metadata() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let request = request(temp.path())?;
    for relative in [
        "codex/skills/deploy/SKILL.md",
        "codex/skills/examples/audit/SKILL.md",
        "codex/skills/nested/deploy/SKILL.md",
    ] {
        write(temp.path(), relative, SKILL)?;
    }
    write(
        temp.path(),
        "codex/skills/deploy/agents/openai.yaml",
        "policy:\n  allow_implicit_invocation: false\n",
    )?;
    // A frontmatter without a name falls back to its directory name.
    write(
        temp.path(),
        "codex/skills/deploy  skill/SKILL.md",
        &SKILL.replace("name: deploy\n", ""),
    )?;
    let manager = CapabilityManager::new(LocalFileSystem::unsandboxed());
    manager.seed_prewarmed_locations(request.clone()).await?;
    let response = manager
        .get_or_refresh_discovery(DiscoverV2CapabilitiesRequest {
            cwd: PathUri::from_host_native_path(temp.path().join("project"))?,
            sandbox: None,
        })
        .await?;
    let root = request.codex_home.join("skills")?;
    let mut expected = vec![
        expected_skill(root.join("deploy  skill/SKILL.md")?, "deploy skill"),
        expected_skill(root.join("deploy/SKILL.md")?, "deploy"),
        expected_skill(root.join("examples/audit/SKILL.md")?, "deploy"),
        expected_skill(root.join("nested/deploy/SKILL.md")?, "deploy"),
    ];
    expected[1].policy = Some(codex_skills::SkillPolicy {
        allow_implicit_invocation: Some(false),
        products: vec![],
    });
    for skill in &mut expected {
        skill.discovery_root = Some(SkillDiscoveryRoot {
            id: "codex-home-skills".to_string(),
            path: root.clone(),
        });
    }
    assert_eq!(response.skills, expected);
    assert_eq!(response.warnings, Vec::<String>::new());
    let json = serde_json::to_string(&response)?;
    assert!(response.skills.len() <= MAX_CAPABILITIES);
    assert!(json.len() <= MAX_RESPONSE_BYTES);
    assert!(response.warnings.len() <= MAX_WARNINGS);
    assert!(!json.contains("PRIVATE_INSTRUCTION_BODY_SENTINEL"));
    Ok(())
}

fn expected_skill(path: PathUri, name: &str) -> ExecutorSkill {
    ExecutorSkill {
        name: name.to_string(),
        description: "Deploy the service.".to_string(),
        short_description: None,
        path,
        discovery_root: None,
        dependencies: None,
        policy: None,
    }
}

#[tokio::test]
async fn discovery_reports_location_failures_without_paths() -> anyhow::Result<()> {
    use codex_protocol::models::PermissionProfile;
    use codex_protocol::permissions::FileSystemSandboxPolicy;
    use codex_protocol::permissions::NetworkSandboxPolicy;

    let temp = tempfile::tempdir()?;
    request(temp.path())?;
    let locations = crate::discover_v2::capability_locations::CapabilityLocations {
        warnings: vec!["could not scan skill roots /private/skills".into()],
        ..Default::default()
    };
    let sandbox = crate::FileSystemSandboxContext::from_permission_profile(
        PermissionProfile::from_runtime_permissions(
            &FileSystemSandboxPolicy::restricted(Vec::new()),
            NetworkSandboxPolicy::Restricted,
        ),
        PathUri::from_host_native_path(temp.path().join("project"))?,
    );
    for sandbox in [None, Some(&sandbox)] {
        let response = super::load_capability_discoveries(
            &LocalFileSystem::unsandboxed(),
            &locations,
            sandbox,
        )
        .await?;
        assert_eq!(
            *response,
            crate::DiscoverV2CapabilitiesResponse {
                warnings: vec![
                    "some capability locations could not be resolved; discovery may be incomplete"
                        .into(),
                ],
                ..Default::default()
            }
        );
    }
    Ok(())
}

#[tokio::test]
async fn discovery_resolves_optional_skill_metadata() -> anyhow::Result<()> {
    use codex_protocol::protocol::Product;
    use codex_skills::SkillDependencies;
    use codex_skills::SkillPolicy;
    use codex_skills::SkillToolDependency;

    let dependencies = Some(SkillDependencies {
        tools: vec![SkillToolDependency {
            r#type: "mcp".into(),
            value: "deploy".into(),
            description: None,
            transport: Some("stdio".into()),
            command: Some("deploy-mcp".into()),
            url: None,
            oauth_callback_port: Some(3118),
        }],
    });
    let policy = Some(SkillPolicy {
        allow_implicit_invocation: Some(false),
        products: vec![Product::Codex, Product::Chatgpt],
    });
    for (yaml, dependencies, policy) in [
        (None, None, None),
        (Some("interface: [invalid]"), None, None),
        (Some("dependencies:\n  tools: [{type: mcp}]"), None, None),
        (Some("dependencies: {tools: []}"), None, None),
        (
            Some(
                r#"dependencies:
  tools:
    - type: " mcp "
      value: " deploy "
      transport: stdio
      command: deploy-mcp
      oauth: {callback_port: 3118}
    - type: mcp
      value: " "
policy:
  allow_implicit_invocation: false
  products: [codex, CHATGPT]
"#,
            ),
            dependencies,
            policy,
        ),
    ] {
        let temp = tempfile::tempdir()?;
        let request = request(temp.path())?;
        let root = "codex/skills/deploy";
        write(temp.path(), &format!("{root}/SKILL.md"), SKILL)?;
        if let Some(yaml) = yaml {
            write(temp.path(), &format!("{root}/agents/openai.yaml"), yaml)?;
        }
        let manager = CapabilityManager::new(LocalFileSystem::unsandboxed());
        manager.seed_prewarmed_locations(request.clone()).await?;
        let response = manager
            .get_or_refresh_discovery(DiscoverV2CapabilitiesRequest {
                cwd: PathUri::from_host_native_path(temp.path().join("project"))?,
                sandbox: None,
            })
            .await?;
        let mut expected =
            expected_skill(request.codex_home.join("skills/deploy/SKILL.md")?, "deploy");
        expected.dependencies = dependencies;
        expected.policy = policy;
        expected.discovery_root = Some(SkillDiscoveryRoot {
            id: "codex-home-skills".to_string(),
            path: request.codex_home.join("skills")?,
        });
        assert_eq!(response.skills, vec![expected]);
        assert_eq!(response.warnings, Vec::<String>::new());
    }
    Ok(())
}

#[tokio::test]
async fn standalone_skills_retain_their_search_roots() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let request = request(temp.path())?;
    let roots = [
        ("codex-home-skills", "codex/skills"),
        ("user-agent-skills", "home/.agents/skills"),
        ("system-skills", "etc/codex/skills"),
    ];
    let mut expected = Vec::new();
    for (id, root) in roots {
        for relative in ["deploy/SKILL.md", "nested/deploy/SKILL.md"] {
            let path = format!("{root}/{relative}");
            write(temp.path(), &path, SKILL)?;
            let mut skill = expected_skill(
                PathUri::from_host_native_path(temp.path().join(path))?,
                "deploy",
            );
            skill.discovery_root = Some(SkillDiscoveryRoot {
                id: id.to_string(),
                path: PathUri::from_host_native_path(temp.path().join(root))?,
            });
            expected.push(skill);
        }
    }
    let manager = CapabilityManager::new(LocalFileSystem::unsandboxed());
    manager.seed_prewarmed_locations(request.clone()).await?;
    let query = DiscoverV2CapabilitiesRequest {
        cwd: PathUri::from_host_native_path(temp.path().join("project"))?,
        sandbox: None,
    };
    let response = manager.get_or_refresh_discovery(query.clone()).await?;
    assert_eq!(response.skills, expected);
    // Cached discovery must retain the same root metadata.
    assert_eq!(manager.get_or_refresh_discovery(query).await?, response);
    Ok(())
}
