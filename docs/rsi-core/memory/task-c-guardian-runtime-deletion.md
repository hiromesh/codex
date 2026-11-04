---
name: task-c-guardian-runtime-deletion
description: 任务 C 完成情况：Guardian 自动审查与 runtime-agent-instance 已删除，保留多智能体 shim；记录验证与残留
metadata:
  node_type: memory
  type: project
  originSessionId: df69a396-fe1c-4729-94c3-754ce53b8dc4
  modified: 2026-11-03T12:44:57.399Z
---

任务 C（用户经 codex-f2 于 2026-11-03 确认）：删除 Guardian 自动审查与 `runtime-agent-instance`，多智能体全部保留，记忆/插件搁置。2026-11-03 已完成主体删除；用户/其他 worker 已提交 `a49c0288e8` 等提交，剩余清理为工作区未提交改动。

关键决策与结果：
- Guardian 保留最小 shim（仅多智能体实际引用）：`ext/guardian-reviewer/src/{review_evidence,context_mode,sender_evidence}.rs`；core `guardian/mod.rs` 保留 `GuardianReviewContext`、`NetworkAccessTrigger`、`tool_permission_context`、`is_basic_session_source`、`guardian_truncate_text`、`GUARDIAN_MAX_ROOT_MESSAGE_TOKENS`、`AUTO_REVIEW_DENIED_ACTION_APPROVAL_DEVELOPER_PREFIX`；guardian-context 保留 `Root*`/`ContextBudgeting`/`PreparedRootContext`/`prepare_root_context`/`render_original_verified_answer`/`PermissionContext`/`HistoryTruncation`/`SectionHistory`/`TranscriptHistory`/`is_inherited_manual_approval`/`retained_assistant_message`/`GuardianRootMessage` 等。
- 已删除 core review 引擎（decision/review/review_session/prompt/budget/routing/score/approval_request）、`guardian_review.rs`、native tool review、MCP elicitation 自动审查、turn 预算/评分、request_permissions 自动审查快路径、Guardian prompt 注入、GuardianV2/V3 feature/config/prompts 与相关测试。
- `NodeReplReviewEvidence` 已随自动审查删除；复核 CodeMode 分支保留 shell，不再采集 evidence。
- `attestation.rs` 保留（普通 OpenAI attestation）。
- 四条验证均通过：`cargo check -p codex-cli`、`-p codex-core --tests`、`-p codex-app-server --all-targets`、`--workspace --exclude codex-voice-host`。
- 残留（未扩大清理）：`Feature::GuardianApproval` 与 `ApprovalsReviewer::AutoReview` 配置仍存在但不再执行自动审查；app-server 的 `ThreadGuardianReview` RPC 暂留并返回 “not available”；`ThreadApproveGuardianDeniedAction` 与 Guardian 通知/analytics 协议类型保留；core history 的 Guardian review checkpoint/context-mode 字段保留；guardian-context crate 未物理裁剪（其 preparation 闭包与保留 root authorization 共用）。
- 未触碰 realtime/voice/webrtc 文件，未修改保留多智能体/记忆/插件 crate。

**Why:** 在保留多智能体功能的前提下删除自动审查，shim 边界由“保留代码实际引用”决定。

**How to apply:** 后续若继续裁剪，优先移除 `Feature::GuardianApproval`/AutoReview 残留、app-server Guardian RPC 与协议类型、guardian-context 未用 section；先跑上述四条验证。相关：[[rsi-kernel-trimming]]、[[codex-f2-dispatches-tasks]]。
