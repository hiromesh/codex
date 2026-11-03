//! Replays an explicit rollout history through the production resume path.

use std::sync::Arc;

use anyhow::Result;
use codex_core::CodexThread;
use codex_history::InitialHistory;
use codex_history::ResumedHistory;
use codex_history::RolloutItem;
use codex_protocol::mcp::ClientMcpExtensions;
use core_test_support::test_codex::TestCodex;

pub(super) async fn resume(
    test: &TestCodex,
    thread: &CodexThread,
    history: Vec<RolloutItem>,
) -> Result<Arc<CodexThread>> {
    let thread_id = thread.startup_metadata().thread_id;
    thread.shutdown_and_wait().await?;
    test.thread_manager.remove_thread(&thread_id).await;
    Ok(test
        .thread_manager
        .resume_thread_with_history(
            test.config.clone(),
            InitialHistory::Resumed(ResumedHistory {
                history_revision: None,
                conversation_id: thread_id,
                history: Arc::new(history),
                rollout_path: None,
            }),
            test.thread_manager.auth_manager(),
            /*parent_trace*/ None,
            ClientMcpExtensions::default(),
        )
        .await?
        .thread)
}
