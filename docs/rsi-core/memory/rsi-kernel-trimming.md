---
name: rsi-kernel-trimming
description: codex-rs 正在被裁剪为 rsi core；删除与否只看用户是否需要该功能，用户负责提交，codex-f2 协调 6 个 worker
metadata:
  node_type: memory
  type: project
  originSessionId: 2eebdd8b-ef62-400d-b7ce-77d7941ff2bf
  modified: 2026-11-04T03:21:33.237Z
---

进行中的项目（2026-11-03 起）：把 `/Users/hiro/codex/codex-rs` 裁剪成 "rsi core"，参考 `/Users/hiro/codex/docs/codex-内核裁剪分析.md`。

用户定下的规则：
- 删除某个包/功能的唯一依据是**用户是否需要这个功能**。行数、耦合度、文档里的删除建议都不是判断依据；它们只用来决定"怎么删"。
- 每一项删除都必须先与用户逐项确认，用户明确批准后才能动手。
- `/Users/hiro/rsi-core/docs/11-codex功能迁移顺序.md` 列出的 17 项迁移功能**肯定不能移除**；但不在文档里的**不代表要移除**，仍需逐项确认。文档明示不在迁移范围内的有 TUI、图片生成与编辑、远程执行环境、插件、非 OpenAI Responses 模型接入。
- 所有 git 提交由用户处理，任何会话都不要 commit；用户无法及时响应，**commit 不是开工门禁**，各批保持可独立编译/可独立提交即可，不要等提交再推进。
- codex-f2 负责与用户确认和派发，不亲自删除；6 个 worker 会话负责核查/删除：codex-00、codex-81、codex-bb、codex-c5、codex-8b、codex-03。
- 工作区常驻大量未提交改动（用户正在进行的删除），worker 不得提交、回滚或清理。

当前已定决策（2026-11-03）：
- 保留：多智能体（agent-roles、agent-graph-store、message-board*、ext/agent、ext/agent-message-board、collaboration-mode-templates）；keyring-store；lmstudio；记忆（迁移文档第 16 项，按文档保留）。
- 2026-11-03 确认：插件功能不需要支持，**插件含 Apps/connectors 全部删除**（plugin/core-plugins/plugin-host/core-plugin-common/utils/plugins、ext/connectors、app-server plugins/marketplace/app RPC、CLI plugin/marketplace、core/src/plugins、插件安装工具、config [plugins]/[marketplaces]/[apps]、apps_processor、core/src/apps）。
- 2026-11-03 新决策：文档明确排除的**图片生成/编辑、远程执行环境（含 remote-control）**批准删除；TUI 暂不删（最后处理）；app-server 产品 RPC 暂不删。
- 认证只保留 API key：ChatGPT OAuth/device code/access token、cloud-config、backend-client、chatgpt、history-notes 链可删。
- 遥测：analytics/otel 先空实现，feedback 直接删。
- state/thread-store 产品子功能（projects/sections/attachments/queue/read-state/remote-control records）暂不动。
- 执行架构（2026-11-03）：03 是 `app-server/**` 与 `tui/**` 唯一写者；8b 是根 Cargo/manifest/schema 集成唯一写者；其他 worker 只改各自领域，app-server/TUI 改动提交精确 hunk。TUI 保留，但允许为删除功能做编译保持型修改（不删 TUI 本体）。
- imagegen 删除时为旧 JSONL 增加 `should_skip_retired_record` 跳过规则；插件删除包含 `connectors` crate，`mention_syntax` 常量迁移后整删 `utils/plugins`，不留插件 shim；认证保留通用 MCP OAuth，删 EMA/enterprise OAuth。
- G2 v2（2026-11-03）：插件**不做类型迁移**，保留 crate 直接删插件分支；唯一迁移是 `mention_syntax` → `protocol/src/mention_syntax.rs`（core re-export，TUI 由 03 改路径），由 codex-81 落；`core-plugin-common` 仅过渡保留，PB4 清完引用后 PB5 整删。批次 PB0→PB6：PB1 与 03 app-server/TUI 串行，PB3 与 c5 imagegen、T1 analytics 串行，PB5 crate 删除与根 manifest 由 8b 统一收口；PB0 等 8b 的 B manifest `cargo check --workspace` 通过后再开工。
- `app-server-daemon` 不整 crate 删除：保留更新/生命周期/线程恢复，只删 remote-control 客户端、settings 字段、transport remote-control 子树与 CLI remote-control 子命令。
- `ext/git-attribution` 暂不删除（未获用户明确批准），只去掉 backend-client policy 查询/依赖；SDK 生成脚本 `update_sdk_artifacts.py` 等归 8b（schema/生成物 owner）。
- 认证采用两步：先过渡版（删 OAuth 登录入口/PAT/Bedrock/headers/external bearer，from_auth_dot_json 只收 ApiKey，保留 CodexAuth 变体与 ChatGPT refresh/dummy 测试构造）；PB5 后工作区可编译时再做 enum 彻底删除（CodexAuth 只留 ApiKey）。
- 认证过渡补充（2026-11-03）：model-provider 的 GatewayAuthManager/combined_auth/shared_state 与 command-backed provider auth（provider.auth / external bearer）已删除；`resolve_provider_auth` 对 provider.auth 与 Bedrock provider 返回 UnsupportedOperation，导致 core 的 gateway_auth/gcp suite、command-auth 测试、app-server GatewayLoginControl/account Bedrock-ChatGPT 测试需同步删除（app-server 由 03 改）。`model-provider-info` 的 gateway_oauth/OAuthSessionConfig 与 config 的 `[config_server]` 经 f2 决定本轮不删，留 enum/后续阶段。
- PB4（hooks/ext-skills/ext-mcp/extension-api 插件分支）等 PB3 与 PB1/T2 清完后再做；81 先做 T1a（analytics client no-op，保留 41 个方法签名）。
- PB5 状态（2026-11-03）：7 个插件 crate 目录已删除（plugin/plugin-host/core-plugins/core-plugin-common/connectors/ext-connectors/utils-plugins），根条目已清；**ext/mcp 保留**（通用 McpEventStreamManager，只删 HostedPluginRuntimeExtension）。`cargo check --workspace` 已 **rc=0**（PB6 收口，Cargo.lock 已收敛，19 个已删 crate 无残留）。PB5 后剩余清理并行中：03 清 app-server-protocol v2 `ThreadItemPayload::McpToolCall` 的 app_context/mcp_app_resource_uri/mcp_app_ui/plugin_id；81 修 protocol 测试残留并审计 plugin_id 字段；bb 清 codex-mcp 过期单测与 McpPluginAttribution/from_plugin/ToolPluginContext、core mcp_tool_call 的 plugin_id 链；c5 清 exec-server 插件发现残留（A/B/C 全删，不保留 wire 兼容字段）；00 做 auth enum 阶段清单。schema/SDK 重生成须等协议类型冻结（03 v2 清理、00 auth enum、81/bb 归因字段全部落定）后再执行 `just write-app-server-schema`。
- 删除：code mode（已完成）；E 组 cloud/app-server 10 项（已完成）；Guardian 自动审查（保留最小 shim 供多智能体）；runtime-agent-instance；B 组 6 项认证/身份/遥测（排队）；**所有 Windows 相关（常设授权，不再逐项确认）**；ollama。
- 2026-11-03 确认：`agent-identity` 虽被迁移文档第 15 项列为源码位置，但实际是 ChatGPT 后端 agent 注册/JWT、全仓无调用者，与本地多智能体无关；用户确认按 B 决定删除。
- 用户明确：Windows 项不需要再确认；所有提交由用户处理。
- 当前队列：B manifest 收口 → R1/R2（远程执行）→ imagegen → auth → plugins → remote-control → analytics/otel/feedback；R1 可先独立走。
- C1（2026-11-04）：消费端去 ChatGPT 分支已改（core 401 recovery、token_budget experimental plan 预算、cyber、compact fallback、session 身份字段；model-provider account_state/models_identity/AuthManagerAuthProvider；analytics API-key gate 并清 App/plugin 测试；memories 身份字段）。C2 清单已发 03（AuthMode/Account/Login/GatewayOAuth RPC/external_auth）。验证：model-provider/otel/analytics/memories --all-targets 均 rc=0；otel `SkillInvocationEvent.plugin_id` 已删（实际在 otel 非 analytics），core/src/skills.rs 两处 None 由 81 删除。analytics lib 删除 3 个依赖 ChatGPT 发送的用例，CUA 用例改用 node_repl，现 98 passed/0 failed。**core --tests 最终 `cargo check -p codex-core --tests` rc=0（2026-11-04）**。
- C3（2026-11-04，f2 批准拆批）：**3a+3b 已完成**。login 仅留 CodexAuth::ApiKey；删 ChatGPT 变体/方法、刷新/恢复状态机、oauth、workspace_switching、account_grant、token_data/TokenData、PAT/Bedrock/AuthHeaders 类型；AuthDotJson 仅留 auth_mode+OPENAI_API_KEY；保留 ExternalAuth 与 workspace_routing 到 C4。验证 `cargo check -p codex-login --all-targets` rc=0，`cargo test -p codex-login --lib` 55 passed（protocol AuthMode 收敛后复验仍绿）。残留消费端：cli/src/doctor.rs 与 exec/tests/apply_patch.rs 归 8b（已发精确修改点）；model-provider/core/models-manager 由 81 C4 收绿；codex-mcp 归 bb；03 的 app_test_support fixture 已解耦。
- C2（2026-11-04）：app-server-protocol AuthMode 仅 ApiKey；v2 Account/LoginAccount* 仅 ApiKey；删 GatewayOAuth*/ChatgptAuthTokensRefresh/GatewayOAuthChanged/CancelLoginAccount/LoginAppBrand；GatewayAuth 保留。app-server 删 auth_mode.rs/external_auth.rs；TUI 登录菜单固定仅 ApiKey（Bedrock/GCP 入口与 GCP 快照删除）。app-server/app-server-protocol/tui --all-targets rc=0。app-server 测试 `write_chatgpt_auth` 全替换为直写 auth.json 的 `write_api_key_auth`。
- 认证残留审计（2026-11-04，只读）：生产代码已无 CodexAuth 旧变体/ChatgptAuthTokens/RefreshTokenError。真残留：cli/doctor（CODEX_ACCESS_TOKEN、ProviderAuthReachabilityMode::Chatgpt、旧 AuthMode 匹配；归 8b）、TUI Bedrock/GCP setup wizard 仍默认可达但 app-server RPC 已 invalid_request（归 03）、protocol AuthMode 空壳 helper + login/models-manager 死调用、RefreshTokenFailed 死链、默认 allowed_login_methods 仍含 Chatgpt。刻意保留：gateway_oauth 栈、TUI ChatGPT/device-code 不支持提示、TelemetryAuthMode::Chatgpt、MCP ChatGpt auth、SessionSource/Product "chatgpt"、ChatgptBaseUrl、ForcedLoginMethod 枚举兼容、Bedrock 模型 provider 本体。
- 残留清理（2026-11-04，f2 派 login 部分）：已删 login CODEX_ACCESS_TOKEN 常量/导出、current_auth_uses_codex_backend、refresh_failure_for_auth stub、auth/error.rs；validate() 默认只认 Api。`cargo check -p codex-login --all-targets` rc=0，lib 55 passed。81 同步删 protocol RefreshTokenFailed*/AuthMode helper 及 model-provider/models-manager 消费端；8b 已收 cli/doctor+exec patch 文本；03 已收 TUI wizard 死路径。
- C4（2026-11-04）：81 完成 model-provider/core 生产/models-manager/model-provider-info/protocol 的 API-key-only 收口（model-provider/models-manager/model-provider-info/protocol --all-targets 与 core --lib rc=0）；codex-mcp 的旧 CodexAuth 调用由 bb 适配（--all-targets rc=0，lib 167/1，唯一失败是缺 target/debug/codex）；trusted_access 整删。core/src 内嵌测试 auth fixture 由 bb 迁移，core/src test error=0。protocol 的 AuthMode/ProviderAccount 枚举暂留到 03 清完 app-server 匹配点后再由 81 删。
- 测试/构建里程碑（2026-11-04）：`cargo check --workspace` 默认 lib rc=0；codex-cli/core-tests/exec/app-server/tui/app-server-protocol --all-targets 均 rc=0；codex-exec-server --tests 289/0；core --tests 编译 rc=0（运行时测试待 c5 分类）；codex-mcp/core lib 测试仅剩 target/debug/codex 环境问题。Windows 依赖/target 段与 `.cargo/config.toml` rustflags 已按常设授权删除；Cargo.lock 收敛约 -321 行。
- schema/SDK（2026-11-04）：`just write-app-server-schema` + `--experimental` 均成功；移除 `_make_chatgpt_account_email_nullable`，删除 ChatGPT 登录 SDK 面（`login_chatgpt`/`login_chatgpt_device_code`、CancelLoginAccount* 等）；schema fixtures 4 passed/1 ignored，Python contract 9 passed、public API 20 passed；生成 diff 265 files +10821/-39217，192 个已删类型 schema 文件移除，无 RemoteControl/allowRemoteControl。剩余：core tests 运行时分类、protocol enum 收尾、workspace --all-targets 最终验证。

- 暂停交接（2026-11-04）：用户 Mac 过载，已让全部 worker 停手并冻结工作树。完整交接文档写在 `/Users/hiro/codex/docs/rsi-core-裁剪交接.md`。当前 HEAD `595497a614`（用户提交 "1"），主体裁剪已入 HEAD；仅 26 个 `core/tests/suite/**` 测试收敛文件未提交，其中 81 的 suite 改动尚未编译验证、c5 的 A/B 簇已落盘但全量验证被中止——新机器先检查这些。最后已验证：workspace --all-targets rc=0、schema/SDK 绿、core lib 2248/1（环境代理）、all.rs 1700/139。所有 agent 不得 commit/stash/revert。

**Why:** 目标是一个最小 agent 内核，边界由用户的功能需求决定，而不是由代码规模决定。

**How to apply:** 向用户呈报每一项时，先说明"这个功能具体做什么、在哪被触发"，然后问"你需要吗"；得到明确批准后才派 worker 删除并跑编译检查。相关：[[codex-f2-dispatches-tasks]]。
