# RSI Core 裁剪交接文档

日期：2026-11-04（暂停时点）
仓库：`/Users/hiro/codex`
当前 HEAD：`595497a614`（提交信息为 “1”，由用户提交）
暂停原因：Mac 过载，任务移交。
文档目的：记录项目目标、用户准则、已确认决策、团队分工与管理方式、当前状态、剩余工作，以及新机器上的恢复步骤。

---

## 1. 项目目标

把 `/Users/hiro/codex/codex-rs` 裁剪为 **rsi core**：一个最小可用的 agent 内核。

两份参考文档：

- `docs/codex-内核裁剪分析.md`：规模、耦合、crate 级处理建议。
- `docs/rsi-core/11-codex功能迁移顺序.md`：17 项必须保留/迁移的内核功能，以及明确不在迁移范围的功能。

`docs/rsi-core/` 下已完整复制旧机器 `rsi-core/docs/` 的 01–11 号文档与 README；`docs/rsi-core/memory/` 下复制了旧机器的项目记忆文件。新机器不再依赖 `/Users/hiro/rsi-core` 或 `~/.claude` 的原始路径。

最终代码目标是：内核保留模型会话、工具循环、本地执行、补丁、多模态、上下文装配、压缩、轨迹持久化、MCP、Hooks、Skills、动态工具/文件搜索、网页搜索、多智能体、记忆、长期目标；终端入口只用于调试。

---

## 2. 用户定下的硬准则（最高优先级）

1. **删除的唯一依据是“用户是否需要这个功能”**。行数、耦合度、文档建议都只用于决定“怎么删”，不是删除理由。
2. **每个具体功能/包的删除，必须先与用户逐项确认**，得到明确批准后才能动手。默认不能把“不在迁移文档里”当成“可以删”。
3. **迁移文档列出的 17 项肯定不能移除**。不在文档里的东西**不代表要删除**，仍需逐项确认。
4. **所有 git 提交由用户处理**。任何 agent/worker 都不得 commit、不得 amend、不得 rebase。
5. **commit 不是开工门禁**。用户明确说过无法及时响应、线程太多；各批保持可独立编译/可独立提交即可，不要等提交再推进。
6. **常设授权，无需逐项确认**：
   - 所有 Windows 相关全部删除；
   - 所有音频相关全部删除。
7. **被用户“搁置”的项目，既不代表保留，也不代表删除**。不得擅自删除，也不得当成已批准保留。
8. 工作区长期存在大量未提交改动；任何会话不得回滚、stash、清理或覆盖他人的在途改动。
9. 用户响应是异步的。不要因为等用户确认而停住其他已批准的工作；但涉及新的功能/包删除时必须先问。
10. 用户可能随时提交（HEAD 会前进），worker 必须以当前工作树为准增量修改，不得假设 HEAD 是几小时前的状态。

---

## 3. 已确认的裁剪决策

### 3.1 已批准删除

- **Code mode**：全部删除（包括 code-mode、code-mode-host、code-mode-protocol、code-mode-runtime）。
- **插件 / Apps / connectors**：全部删除，包含：
  - plugin、plugin-host、core-plugins、core-plugin-common、utils/plugins、connectors、ext/connectors；
  - app-server 的 plugin/marketplace/app RPC 与 Apps 测试脚手架；
  - CLI plugin/marketplace 子命令；
  - core 的 plugins/apps/connectors 模块、插件安装工具；
  - config 的 `[plugins]`/`[marketplaces]`/`[apps]`、Apps/Plugin/Marketplace 配置类型与 requirements；
  - 插件归因字段（`plugin_id`、`McpPluginAttribution`、`ToolPluginContext`、`plugin_display_names` 等）；
  - 托管 Apps（HostedApps）、codex_apps 目录发现、connector 审批元数据与模板资产。
- **图片生成/编辑**：全部删除；旧 JSONL 轨迹用 `should_skip_retired_record` 跳过 imagegen 遗留记录。
- **远程执行环境**：remote/relay/client/forward/noise 等全部删除；`exec-server` 只保留本地 process/fs/server/rpc/connection。
- **remote-control**：transport、daemon、CLI 子命令、app-server processor、协议类型、settings/allow_remote_control 全部删除。
- **Windows**：Windows sandbox crate、所有 `cfg(windows)` target 段、windows-sys/uds_windows/winapi-util/winreg、`.cargo/config.toml` 的 Windows rustflags，全部删除。
- **音频**：全部删除。
- **认证**：只保留 API key。删除 ChatGPT OAuth/device code、ChatGPT auth tokens、headers、PAT、Bedrock API key/access keys、agent identity、EMA/enterprise OAuth、cloud-config、backend-client、chatgpt、history-notes 等链。
- **analytics/otel**：保留 crate 名，内部 no-op；feedback 删除。
- **Guardian 自动审查**：删除审查策略/会话机制；保留 `ext/guardian-reviewer` 最小 shim 供多智能体授权（仅证据/上下文/发送者交换类型）。
- **runtime-agent-instance**：删除。
- **E 组 cloud/app-server 10 项**：按用户确认删除。
- **B 组 6 项认证/身份/遥测**：按用户确认删除；其中 `agent-identity` 经确认虽然出现在迁移文档第 15 项源码位置，但实际是 ChatGPT 后端 agent 注册/JWT、全仓无调用者，与本地多智能体无关，用户确认删除。

### 3.2 保留

- 迁移文档 17 项对应功能及其 crate/目录。
- 多智能体：agent-roles、agent-graph-store、message-board*、ext/agent、ext/agent-message-board、collaboration-mode-templates。
- 记忆：memories/read、memories/write、ext/memories、core 记忆上下文、state、rollout。
- 长期目标：ext/goal、core user_goal。
- MCP：codex-mcp、rmcp-client、ext/extension-api、ext/mcp（通用 `McpEventStreamManager` 保留，只删 HostedPluginRuntimeExtension）。
- Hooks、Skills（standalone/local）、ext/skills。
- 网页搜索 ext/web-search（只删 ChatGPT workspace routing 测试）。
- file-search、本地 exec-server、sandboxing、execpolicy、apply-patch。
- keyring-store（用户明确保留）。
- lmstudio。
- TUI 暂时保留（作为调试入口；最后是否删除另议）。
- app-server 产品 RPC 暂不删（由用户后续决定）。
- `state`/`thread-store` 的产品子功能（projects/sections/attachments/queue/read-state/remote-control records）暂不动，属“搁置”，既非保留也非删除。
- ext/git-attribution 保留 crate，只去掉 backend-client policy 查询。
- ext/items、ext/queue、ext/multi-env-copy-file：未获删除批准，保持原样。
- `GatewayAuth` 保留：它是 env/external API key 配置，不是 OAuth。
- gateway_oauth 链：此前 f2 决定本轮不删，留后续。

### 3.3 认证两步走的最终结果

第一步（过渡版）：删 OAuth 登录入口/PAT/Bedrock/headers/external bearer，`from_auth_dot_json` 只接受 API key。
第二步（已在本轮完成）：

- `CodexAuth` 只剩 `ApiKey(ApiKeyAuth)` 单变体 enum；
- `AuthMode`、`ProviderAccount` 只剩 `ApiKey`；
- `AuthDotJson` 只剩 `auth_mode + OPENAI_API_KEY`；
- 删除 `TokenData`、`account_grant`、`token_data`、PAT/Bedrock/AuthHeaders 模块；
- 删除 RefreshTokenError/UnauthorizedRecovery/RefreshTokenFailed 链；
- CLI/TUI/app-server 的 ChatGPT/Bedrock/GCP 登录入口与提示改为 API-key-only；
- 通用 MCP OAuth 保留；MCP EMA/enterprise OAuth 删除。

---

## 4. 团队结构与文件所有权

用户指定 `codex-f2` 作为协调者，负责与用户确认、拆任务、派发、复核，不亲自删除。
6 个 worker 会话：`codex-00`、`codex-81`、`codex-bb`、`codex-c5`、`codex-8b`、`codex-03`。

最终文件所有权（用于避免双写冲突）：

| owner | 范围 |
|---|---|
| codex-03 | `app-server/**`、`tui/**` 唯一写者（含 app-server-protocol 的 app-server 侧消费） |
| codex-8b | 根 `Cargo.toml`/`Cargo.lock`、manifest、schema/SDK 生成、CLI/exec 入口集成、Windows 依赖清理 |
| codex-81 | `core` 生产代码（除 bb 的 MCP 文件）、`protocol`、`config`、`hooks`、`skills`、`ext/skills`、`rollout-trace`、`thread-store`、`features`、Guardian shim |
| codex-00 | `login/**`、认证消费者（core/model-provider 的 C1 部分）、analytics/otel/memories 的认证消费者审计 |
| codex-bb | `codex-mcp/**`、`rmcp-client`、`core/src/mcp_tool_call.rs`、`core/src/session/mcp.rs`、`core/src/mcp.rs`、`core/src/session/mcp_runtime.rs`、`core/src/agent/host/mcp_metadata.rs`、`core/src/mcp_tool_exposure.rs`、`core/src/tools/handlers/mcp.rs`、core/src 内嵌测试 |
| codex-c5 | `exec-server/**`、`exec-server-protocol`、`core/tests/**`、`core/src/tools/mod.rs` 的扁平命名/测试、features 收尾 |

跨域改动规则：只改自己域；必须改别人文件时，把精确 hunk/文件:行发给 owner，由 owner 落地。
`app-server/**`、`tui/**` 永远由 03 改；根 manifest/schema 永远由 8b 改；`core/src` 内嵌测试归 bb，`core/tests/suite` 归 c5。

---

## 5. 协作与执行方式

### 5.1 标准流程

1. **只读盘点**：worker 先 grep/读代码，输出“功能是什么、在哪触发、谁引用、删了影响什么”。
2. **向用户确认**：协调者向用户逐项呈报并询问“你需要这个功能吗”；得到明确批准才派删除。
3. **派发**：给出精确范围、文件所有权、验证命令、禁止事项。
4. **增量执行**：按 crate/批次删；优先 lib target 可编译，测试残留后置。
5. **定向验证**：每个 owner 跑自己 crate 的 `cargo check -p ... --all-targets`；避免所有人同时跑 workspace 全量。
6. **全局收口**：8b 统一跑 `cargo check --workspace --all-targets`；schema/SDK 由 8b 生成并验证。
7. **用户提交**：协调者汇总 diff/统计，用户自己 commit。

### 5.2 并发策略

- 用户要求高并行；但 cargo 全量/大测试不要多人同时跑，会锁 target 并拖慢机器。
- 常规节奏：多个 worker 并行改不同 crate；每批用定向 check；只有收口时跑一次 workspace 全量。
- 临时红灯可接受：同一批的协议字段删除与消费端适配可分步落盘，但要在同一轮内收口。
- 已发生多次 HEAD 前进（用户提交）；worker 必须以当前工作树增量修改，不得 revert 他人改动。

### 5.3 汇报格式

worker 向 codex-f2 汇报时优先给：

- rc / passed / failed 精确数字；
- 改动文件与删除内容；
- 阻塞点与归属（文件:行）；
- 是否还有未落盘/在途状态；
- 明确“未 commit/stash/revert”。

---

## 6. 当前状态（暂停时点）

### 6.1 Git

- HEAD：`595497a614`（用户提交，提交信息 “1”）。
- 主体裁剪已经进入 HEAD；**当前仅有 26 个未提交文件**，全部集中在 `codex-rs/core/tests/suite/`，属于暂停时的测试收敛中间态。
- 未提交文件包括：
  - M：agent_websocket.rs、agents_md.rs、codex_delegate.rs、cyber_exec_policy.rs、git_enrichment.rs、guardian_authorization.rs、guardian_subagent_authorization.rs、hooks_executor.rs、mcp_subagent_elicitation.rs、mod.rs、models_cache_ttl.rs、multi_agent_resume.rs、request_permissions*.rs、rmcp_client.rs、scenarios_shared_instructions.rs、shell_snapshot.rs、step_settings*.rs、subagent_notifications.rs、unified_exec_stdin_*.rs、.approvals.rs.pending-snap
  - D：guardian_authorization_refresh.rs、guardian_sender_messages_tests.rs、guardian_subagent_notification_tests.rs
- **重要**：这些 26 个文件是暂停前 81/c5 正在收紧的测试，未做最终全量复验。新机器第一步应检查它们，不要直接 commit；先判断是完成态还是半成品。

### 6.2 最后一次已验证状态（HEAD 提交前的完整工作树）

- `cargo check --workspace --all-targets` **rc=0**（含 Guardian 依赖瘦身与 Cargo.lock 收敛后）。
- app-server schema fixtures：4 passed / 1 ignored。
- Python SDK contract + signatures：29 passed。
- `codex-cli --all-targets`、`codex-exec --all-targets`、`codex-app-server --all-targets`、`codex-tui --all-targets`、`codex-app-server-protocol --all-targets`、`codex-core --tests` 均曾达到 rc=0。
- `codex-exec-server --tests`：289 passed / 0 failed / 1 ignored（remote 测试清理后）。
- config schema：纯删除 522 行；app-server schema 生成 diff 265 files，+10,821 / -39,217，移除 192 个已删类型的 schema 文件。
- Cargo.lock：约 -347 行。
- 当时完整 diff 统计：约 1,350 files changed，+13,439 / -279,398（已由用户提交）。

### 6.3 core 运行时测试（暂停前最后数字）

在 `NO_PROXY=127.0.0.1,localhost,::1`、`RUST_MIN_STACK=8388608` 下：

- `cargo test -p codex-core --lib`：**2248 passed / 1 failed**
  - 唯一失败：`session::tests::user_shell_commands_do_not_inherit_managed_network_proxy`，原因是本机环境注入 `HTTP_PROXY=http://127.0.0.1:7897`，子 shell 合法继承；干净环境应通过。
- `cargo test -p codex-core --test all`：**1700 passed / 139 failed / 30 ignored**
  - 失败几乎全部是已删功能的过期测试，按簇分派中。
- c5 直属 suites 本次全量：159 passed / 0 failed。
- responses_headers：5 passed；mcp_trace_propagation：1 passed（首跑偶发握手超时）。

### 6.4 139 个 all.rs 失败的簇（暂停时）

- Guardian/自动审查/多智能体授权/approvals/hooks 簇（约 50）：`guardian_subagent_authorization`、`guardian_authorization_refresh`、`guardian_authorization`、`guardian_sender_messages`、`agents_md`、`cyber_exec_policy`、`git_enrichment`、`request_permissions(+tool)`、`shell_snapshot`、`unified_exec_stdin_*`、`subagent_notifications`、`models_cache_ttl`、`codex_delegate`、`mcp_subagent_elicitation`、rmcp `sandbox_state_meta`。
- routing hint（`uses_codex_backend` 恒 false）：agent_websocket 7、client_websockets 4。
- remote-env 过期：hooks_executor 6、mcp_executor_context 1、skills_extension 4、step_settings 2、rmcp local fallback 1。
- rmcp prewarm：3。
- scenarios 36：含 guardian_*、remote slash compact、world_state remote，以及必须保留并修复的真实 MCP 场景（shared_mcp_keeps_tools、mcp_tool_discovery_in_same_turn、model_catalog_refresh_preserves_tools_and_history）。
- 其他：hooks 2、approvals 3。
- 明确的“更像真实功能问题”清单：
  - `suite::scenarios::mcp_sharing::shared_mcp_keeps_tools_and_model_requested_input_per_chat`
  - `suite::scenarios::mcp_tool_discovery_in_same_turn`
  - `suite::scenarios::model_catalog_refresh_preserves_tools_and_history`
  - `suite::client_websockets::responses_websocket_streams_request`
  - `suite::multi_agent_resume::cold_root_resume_restores_agent_identity_and_role_on_followup`
  - `suite::context_annotations::first_request_item_types_roles_and_content_annotations`
  - `suite::mcp_tool_cache::cached_mcp_startup_is_eager_for_root_and_lazy_for_subagents`（等待超时）

处理原则：

- 自动审查专属用例：删。
- 文档第 15 项多智能体授权语义：必须修到过；生产 shim 缺行为则补生产。
- routing hint / remote-env / prewarm / Apps / ChatGPT analytics 断言：过期，删或改。
- 真实 MCP/工具/上下文/websocket 场景：修测试，不删；修不了报生产 bug。
- 环境失败单独标注，不计功能失败。

### 6.5 暂停时各 worker 状态（重要）

- **codex-00**：login 侧清理全部落盘并验证（`--all-targets` rc=0、lib 55 passed）；无后台 cargo、无未落盘中间态。
- **codex-bb**：codex-mcp AuthMode/API-key 适配与 core/src MCP metadata/命名修复均已落盘；最后一次 `core --tests` rc=0；无未落盘中间态。
- **codex-8b**：所有编辑已落盘（config schema 重新生成、悬空 doc 注释修复）；workspace `--all-targets` 与 schema/契约最后一次 rc=0；无后台进程。
- **codex-03**：TUI Bedrock/GCP 死向导、GovCloud 引导删除与 app-server 默认 login methods 改 Api 已完成落盘；双门禁绿；无未落盘中间态。
- **codex-c5**：A/B 簇改动全部已落盘；最近一次全量验证被中止；已知残留 `scenarios::skill_catalog_dedup::skill_catalog_deltas` 与 2 例代理环境问题。无 commit/revert/stash/清理。
- **codex-81**：suite 簇删除/精简改动已全部写入工作树（rmcp 修复、guardian/auto-review 专属用例删除、request_permissions/cyber/codex_delegate 清理），但 **`cargo test --no-run` 编译校验被终止，尚未编译验证**；无未落盘临时文件或后台进程。

因此新机器的第一优先级是验证这 26 个未提交文件的编译状态，尤其是 81 的 suite 改动与 c5 的 A/B 簇改动。

### 6.6 已知环境问题

1. 本机 `~/.codex/.env` 注入 `HTTP(S)_PROXY=http://127.0.0.1:7897` 但该端口无人监听：
   - 跑测试需 `NO_PROXY='127.0.0.1,localhost,::1'`。
   - 该问题也会让 TUI 连本地 ws app-server 失败，因为 tokio-tungstenite `proxy` feature 默认不绕过 loopback；属既有 app-server-client 问题，未修，是否修由用户后续决定。
2. 默认 2 MiB 栈会导致部分测试 SIGABRT：用 `RUST_MIN_STACK=8388608`（部分 core 测试用 67,108,864）。
3. 缺少 `target/debug/codex`、`target/debug/rmcp_test_server` 会造成看似功能失败：先构建再跑。
4. plain `cargo test` 跑整个 `chatwidget::tests::slash_commands` 模块会触发 insta “inline snapshot in loops” 限制；项目实际用 nextest（每用例独立进程）不受影响。
5. 全量并行下 MCP 子进程握手偶发超时；重跑可过。
6. 多 worker 同时跑 cargo 会有分钟级 artifact 锁等待；收口时串行。

### 6.7 warning

最后一次 workspace check 约 931 行 warning（各 target 重复计数），归一后 27 类，涉及 23 个 crate，主要是 core/tui 的未使用导入/变量，来自删除后的残留。**无 error**。可在功能测试收敛后按域清理，不是提交门禁。

---

## 7. 新机器恢复步骤

### 7.1 先看状态，不要急着跑全量

```bash
cd /Users/hiro/codex   # 新机器路径可能不同
git log --oneline -5
git status --porcelain
git diff --shortstat
```

重点检查 `codex-rs/core/tests/suite/` 的 26 个未提交文件：它们是暂停时的测试收敛中间态。先判断哪些已完成、哪些半成品；不要直接 commit，也不要把它们回滚掉。

### 7.2 基线验证

```bash
cd codex-rs
RUST_MIN_STACK=8388608 NO_PROXY='127.0.0.1,localhost,::1' cargo check --workspace --all-targets
```

预期：rc=0（如果 26 个未提交测试文件处于半成品状态，可能暂时红灯；先按报错归属分派）。

### 7.3 继续 core 运行时测试收敛

```bash
RUST_MIN_STACK=8388608 NO_PROXY='127.0.0.1,localhost,::1' \
  cargo test -p codex-core --lib

RUST_MIN_STACK=8388608 NO_PROXY='127.0.0.1,localhost,::1' \
  cargo test -p codex-core --test all --no-fail-fast
```

定向跑某个 suite：

```bash
RUST_MIN_STACK=8388608 NO_PROXY='127.0.0.1,localhost,::1' \
  cargo test -p codex-core --test all guardian_subagent_authorization
```

分工：

- `core/tests/suite/**`：c5 继续收口；routing/remote-env/prewarm/Guardian 过期用例删除；真实 MCP 场景修复。
- `core/src/**` 内嵌测试：bb/81 按域处理。
- 多智能体授权/approvals/hooks：81 判定保留语义 vs 过期。

### 7.4 schema/SDK（只在协议面稳定后）

```bash
cd /Users/hiro/codex
just write-app-server-schema
just write-app-server-schema --experimental
cargo test -p codex-app-server-protocol --lib schema_fixtures_tests
uv run --project sdk/python pytest sdk/python/tests/test_contract_generation.py sdk/python/tests/test_public_api_signatures.py
```

config schema：

```bash
cargo run -p codex-config-schema
# 输出 core/config.schema.json
```

注意：生成命令会先清空并重建整个 schema/typescript、schema/json；不要与其他 worker 并发生成。schema 生成后要复跑 workspace check。

### 7.5 最终门禁

```bash
cargo check --workspace --all-targets
cargo test -p codex-app-server-protocol --lib schema_fixtures_tests
# Python 契约测试
```

再汇总：

```bash
git diff --shortstat
git status --porcelain
```

提交由用户完成。

---

## 8. 遗留风险与注意事项

1. **不要批量删除测试来“变绿”**。Guardian 自动审查专属测试可删；多智能体授权、MCP、上下文、压缩、长期目标等文档第 15/16/17 项相关测试必须保留并修到过。
2. **26 个未提交测试文件是风险点**：可能包含删到一半的 guardian 测试。新机器先阅读 diff，再决定继续完成或恢复（但不要 revert 用户提交）。
3. **app-server-client loopback 代理问题**：用户开代理时本地 ws 连不上；未修，属后续可选任务。
4. **TUI 仍保留**：不要擅自删除 TUI；TUI 内的 ChatGPT/Bedrock/GCP 登录入口已改为 API-key-only 或删除。
5. **state/thread-store 产品子功能、app-server 产品 RPC、ext/items/ext/queue/ext/multi-env-copy-file/ext/git-attribution** 都是“未获删除批准/搁置”状态，不要视为保留，也不要擅自删除。
6. **keyring-store、lmstudio、多智能体、记忆、长期目标、ext/web-search、ext/mcp 通用事件流** 是明确保留项。
7. **不要 commit、不要 stash、不要 revert、不要清理工作区**。
8. 用户 Mac 已过载；新机器上也不要同时跑多个 workspace 全量 cargo，按域定向 check，收口时串行。

---

## 9. 关键文件与参考

- `docs/codex-内核裁剪分析.md`
- `docs/rsi-core/11-codex功能迁移顺序.md`
- `docs/rsi-core/01-项目定义.md` 至 `docs/rsi-core/10-第一刀运行说明.md`、`docs/rsi-core/README.md`
- 项目记忆副本：`docs/rsi-core/memory/rsi-kernel-trimming.md`
- 任务派发关系：`docs/rsi-core/memory/codex-f2-dispatches-tasks.md`
- 任务 C：`docs/rsi-core/memory/task-c-guardian-runtime-deletion.md`
- Worktree CLI 测试债：`docs/rsi-core/memory/worktree-cli-test-debt.md`
- 环回代理坑：`docs/rsi-core/memory/tui-loopback-ws-proxy-gotcha.md`
- 记忆索引：`docs/rsi-core/memory/MEMORY.md`

以上均为本仓库内相对路径（仓库根 `/Users/hiro/codex`），新机器上直接使用 `docs/rsi-core/...` 即可，不依赖旧机器的 `/Users/hiro/rsi-core` 或 `~/.claude`。

日志（旧机器 `/tmp`，不一定随仓库迁移）：
- `/tmp/workspace-check2.log`
- `/tmp/appserver_check7.log`
- `/tmp/tui_check_all3.log`
- `/tmp/final_gate_appserver.log`
- `/tmp/final_gate_tui.log`
- `/tmp/tui_post81.log`
- `/tmp/wizard_gate3_tui.log`
- `/tmp/wizard_gate2_appserver.log`

---

## 10. 交接后第一批建议任务

1. 检查 26 个未提交 `core/tests/suite` 文件，确认 Guardian/approvals 测试是完成态还是半成品。
2. 跑一次 `cargo check --workspace --all-targets`，确认 HEAD 提交后的基线。
3. 按第 6.4 节簇分派：
   - c5：routing/remote-env/prewarm/过期 Guardian 场景删除；真实 MCP/websocket/context 场景修复。
   - 81：Guardian 多智能体授权/approvals/hooks 簇判定与修复。
   - bb：MCP 相关剩余失败（如有）。
4. 跑 `core --lib` 与 `core --test all`，目标：除环境/偶发超时外无失败。
5. 按域清 warning（可选，非门禁）。
6. 最终 workspace + schema/契约门禁，交用户提交。
