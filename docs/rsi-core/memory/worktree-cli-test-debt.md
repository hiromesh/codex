---
name: worktree-cli-test-debt
description: CLI worktree start/fork E2E tests deleted pending an API-key rewrite after auth/cloud-config trimming
metadata:
  node_type: memory
  type: project
  originSessionId: 3339fd40-fbe7-4c52-8581-b8e99f7689a0
  modified: 2026-11-03T15:30:44.694Z
---

2026-11-03: 删除 `cli/tests/worktree.rs` 中 `interactive_worktree_start_and_fork_bind_owner_before_turn`、`daemon_worktree_start_and_fork_bind_owner_before_turn` 及共享 helper `worktree_start_and_fork`（该文件现仅保留终端查询测试）。原因：测试 seed ChatGPT 认证存储并依赖 cloud-config bundle；认证已收敛为 API-key-only、cloud-config crate 已删除，测试不可运行。worktree 功能本体未删除。

**Why:** 这是真实的 worktree 功能覆盖，被删只是因测试基建失效，不要把功能本体当作已获批准删除。

**How to apply:** 后续若需恢复覆盖，用本地 API-key auth.json 重写 start/fork 测试，并移除 cloud bundle mock 与“云端 distrust 阻止 fork”断言。

相关：[[rsi-kernel-trimming]]、[[codex-f2-dispatches-tasks]]
