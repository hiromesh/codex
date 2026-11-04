//! Watch events invalidate discovery; no subscription keeps requests uncached.

use super::*;
use crate::DiscoverV2CapabilitiesRequest;
use crate::LocalFileSystem;
use crate::discover_v2::capability_locations::tests::request;
use crate::discover_v2::capability_locations::tests::write;
use anyhow::Context;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use std::time::Duration;

// Keep native events from racing deterministic cache assertions.
pub(in crate::discover_v2::capability_manager) async fn stop_listener(
    manager: &CapabilityManager,
) -> anyhow::Result<()> {
    manager
        .watchers
        .subscription
        .get()
        .context("watcher unavailable")?
        ._listener
        .abort();
    drop(manager.location_refresh_lock.acquire().await?);
    Ok(())
}

#[tokio::test]
async fn watch_events_invalidate_discovery_and_no_subscription_disables_caching()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let root = std::fs::canonicalize(directory.path())?;
    write(
        &root,
        "codex/skills/demo/SKILL.md",
        "---\nname: demo\ndescription: original\n---\n",
    )?;
    let request = request(&root)?;
    let demo_skill = request.codex_home.join("skills/demo/SKILL.md")?;
    let manager = CapabilityManager::new(LocalFileSystem::unsandboxed());
    manager.seed_prewarmed_locations(request.clone()).await?;
    let params = DiscoverV2CapabilitiesRequest {
        cwd: PathUri::from_host_native_path(&root)?,
        sandbox: None,
    };
    let original = manager.get_or_refresh_discovery(params.clone()).await?;
    assert!(
        original
            .skills
            .iter()
            .any(|skill| skill.path == demo_skill && skill.description == "original")
    );
    let initial = manager
        .location_snapshot()
        .await
        .context("missing initial snapshot")?;

    // Wait for native delivery before testing one-shot creation of missing roots.
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        loop {
            interval.tick().await;
            write(
                &root,
                "codex/skills/demo/SKILL.md",
                "---\nname: demo\ndescription: ready\n---\n",
            )?;
            let snapshot = manager
                .location_snapshot()
                .await
                .context("missing snapshot")?;
            if !Arc::ptr_eq(&initial, &snapshot) {
                assert!(snapshot.sandbox_discoveries.lock().await.is_empty());
                break anyhow::Ok(());
            }
        }
    })
    .await
    .context("native watcher did not deliver its first content event")??;

    // The user skills directory did not exist at startup.
    write(
        &root,
        "home/.agents/skills/added/SKILL.md",
        "---\nname: added\ndescription: new global root\n---\n",
    )?;
    let home_skills = PathUri::from_host_native_path(root.join("home/.agents/skills"))?;
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        loop {
            interval.tick().await;
            // Inspect only locations: a request-time rescan must not hide a missed event.
            let snapshot = manager
                .location_snapshot()
                .await
                .context("missing snapshot")?;
            if snapshot
                .locations
                .locations
                .iter()
                .any(|location| location.root == home_skills)
            {
                assert!(snapshot.sandbox_discoveries.lock().await.is_empty());
                break anyhow::Ok(());
            }
        }
    })
    .await
    .context("native watcher missed creation of a global root")??;
    // Drive subsequent refreshes explicitly so native delivery cannot race cache assertions.
    stop_listener(&manager).await?;
    let refreshed = manager.get_or_refresh_discovery(params.clone()).await?;
    let added_skill = home_skills.join("added/SKILL.md")?;
    assert!(
        refreshed
            .skills
            .iter()
            .any(|skill| skill.path == added_skill)
    );
    assert!(
        refreshed
            .skills
            .iter()
            .any(|skill| skill.path == demo_skill && skill.description == "ready")
    );
    let sandbox_params = DiscoverV2CapabilitiesRequest {
        sandbox: Some(crate::FileSystemSandboxContext::from_legacy_sandbox_policy(
            codex_protocol::protocol::SandboxPolicy::DangerFullAccess,
            params.cwd.clone(),
        )?),
        ..params.clone()
    };
    manager.get_or_refresh_discovery(sandbox_params).await?;
    let retained = manager
        .location_snapshot()
        .await
        .context("missing populated snapshot")?;
    assert_eq!(retained.sandbox_discoveries.lock().await.len(), 2);
    // The system skills root did not exist when the retained snapshot was built.
    write(
        &root,
        "etc/codex/skills/system/SKILL.md",
        "---\nname: system\ndescription: system skill\n---\n",
    )?;
    write(
        &root,
        "codex/skills/demo/SKILL.md",
        "---\nname: demo\ndescription: refreshed\n---\n",
    )?;
    manager.refresh_locations().await;
    let next = manager
        .location_snapshot()
        .await
        .context("missing refreshed snapshot")?;
    let system_skills = PathUri::from_host_native_path(root.join("etc/codex/skills"))?;
    assert!(next.locations.locations.iter().any(|location| {
        location.root == system_skills && location.standalone_root_id.as_deref() == Some("system-skills")
    }));
    assert!(next.sandbox_discoveries.lock().await.is_empty());
    assert_eq!(retained.sandbox_discoveries.lock().await.len(), 2);
    assert!(
        !retained
            .locations
            .locations
            .iter()
            .any(|location| location.root == system_skills)
    );
    let after = manager.get_or_refresh_discovery(params.clone()).await?;
    assert!(
        after
            .skills
            .iter()
            .any(|skill| skill.path == demo_skill && skill.description == "refreshed")
    );
    let system_skill = system_skills.join("system/SKILL.md")?;
    assert!(
        after
            .skills
            .iter()
            .any(|skill| skill.path == system_skill)
    );
    // Only the requested context is rebuilt; subsequent requests reuse it.
    assert_eq!(next.sandbox_discoveries.lock().await.len(), 1);
    assert!(Arc::ptr_eq(
        &after,
        &manager.get_or_refresh_discovery(params.clone()).await?
    ));

    #[cfg(unix)]
    {
        // A failed event refresh retains the response but retries without another event.
        let skills_root = root.join("codex/skills");
        let saved_root = root.join("codex/saved-skills");
        std::fs::rename(&skills_root, &saved_root)?;
        std::os::unix::fs::symlink(&skills_root, &skills_root)?;
        manager.refresh_locations().await;
        let fallback = manager.get_or_refresh_discovery(params.clone()).await?;
        assert!(Arc::ptr_eq(&after, &fallback));
        std::fs::remove_file(&skills_root)?;
        std::fs::rename(&saved_root, &skills_root)?;
        write(
            &root,
            "codex/skills/recovered/SKILL.md",
            "---\nname: recovered\ndescription: recovered after failed refresh\n---\n",
        )?;
        let recovered = manager.get_or_refresh_discovery(params.clone()).await?;
        assert!(
            recovered
                .skills
                .iter()
                .any(|skill| skill.name == "recovered")
        );
        assert!(!Arc::ptr_eq(&fallback, &recovered));
        assert!(Arc::ptr_eq(
            &recovered,
            &manager.get_or_refresh_discovery(params.clone()).await?
        ));
    }

    // Without a subscription, requests reload both membership and content.
    let mut uncached = CapabilityManager::new(LocalFileSystem::unsandboxed());
    Arc::get_mut(&mut uncached)
        .context("new manager unexpectedly shared")?
        .watchers
        .file_watcher = None;
    uncached.seed_prewarmed_locations(request.clone()).await?;
    uncached.get_or_refresh_discovery(params.clone()).await?;
    assert!(uncached.watchers.subscription.get().is_none());
    write(
        &root,
        "codex/skills/demo/SKILL.md",
        "---\nname: demo\ndescription: uncached edit\n---\n",
    )?;
    write(
        &root,
        "codex/skills/uncached/SKILL.md",
        "---\nname: uncached\ndescription: added without subscription\n---\n",
    )?;
    let after = uncached.get_or_refresh_discovery(params.clone()).await?;
    assert!(
        after
            .skills
            .iter()
            .any(|skill| skill.name == "uncached")
    );
    assert!(
        after
            .skills
            .iter()
            .any(|skill| skill.path == demo_skill && skill.description == "uncached edit")
    );
    let repeated = uncached.get_or_refresh_discovery(params).await?;
    assert_eq!(after, repeated);
    assert!(!Arc::ptr_eq(&after, &repeated));
    Ok(())
}

#[tokio::test]
async fn failed_watch_registration_reloads_locations_and_contents() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let root = std::fs::canonicalize(directory.path())?;
    write(
        &root,
        "codex/skills/demo/SKILL.md",
        "---\nname: demo\ndescription: original\n---\n",
    )?;
    let request = request(&root)?;
    let params = DiscoverV2CapabilitiesRequest {
        cwd: PathUri::from_host_native_path(&root)?,
        sandbox: None,
    };
    let mut manager = CapabilityManager::new(LocalFileSystem::unsandboxed());
    // A subscriber exists, but no OS watches can be registered.
    Arc::get_mut(&mut manager)
        .context("new manager unexpectedly shared")?
        .watchers
        .file_watcher = Some(Arc::new(FileWatcher::noop()));
    manager.seed_prewarmed_locations(request.clone()).await?;
    let before = manager.get_or_refresh_discovery(params.clone()).await?;
    assert_eq!(before.skills.len(), 1);
    assert!(manager.watchers.subscription.get().is_some());
    let initial = manager
        .location_snapshot()
        .await
        .context("missing snapshot")?;
    assert!(!initial.registration.registration_succeeded());

    write(
        &root,
        "codex/skills/demo/SKILL.md",
        "---\nname: demo\ndescription: edited\n---\n",
    )?;
    write(
        &root,
        "codex/skills/extra/SKILL.md",
        "---\nname: extra\ndescription: added while watches are unavailable\n---\n",
    )?;
    let after = manager.get_or_refresh_discovery(params.clone()).await?;
    assert_eq!(after.skills.len(), 2);
    assert!(
        after
            .skills
            .iter()
            .any(|skill| skill.name == "extra")
    );
    assert!(
        after
            .skills
            .iter()
            .any(|skill| skill.name == "demo" && skill.description == "edited")
    );
    let snapshot = manager
        .location_snapshot()
        .await
        .context("missing refreshed snapshot")?;
    assert!(!Arc::ptr_eq(&initial, &snapshot));
    assert!(
        snapshot
            .sandbox_discoveries
            .lock()
            .await
            .iter()
            .all(|discovery| discovery.response.get().is_none())
    );
    let repeated = manager.get_or_refresh_discovery(params).await?;
    assert_eq!(after, repeated);
    assert!(!Arc::ptr_eq(&after, &repeated));
    Ok(())
}
