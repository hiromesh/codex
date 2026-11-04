# codex 架构地图：模块与功能全景

本文回答两个问题：codex 有哪些模块、各自做什么；这些模块之间的内部依赖是什么。迁移顺序草案放在最后，待能力范围确认后一起定。

## 1. 源码快照

- 路径：`/Users/hiro/codex/codex-rs`
- HEAD：`e8540b346b`（2026-11-03 17:14 +0800，"Remove Guardian v2 and runtime agent instance components"）
- workspace 成员：143 个（含测试支持 crate）
- codex 正在被并行裁剪，本文所有结论以该快照为准；每次复制前重新确认，记录到 [决策 D-008](06-决策记录.md#d-008-记录-codex-来源快照)。
- 遥测类模块（otel、analytics、rollout-trace、direct-tool-timing、response-debug-context）按用户要求只列位置，不展开、不迁移。

## 2. 模块全景（按功能域）

### 2.1 协议与类型

| crate | 功能 |
|---|---|
| `protocol`（33.6k 行） | 全部 wire 类型：`ResponseItem`/`ContentItem`/`TokenUsage`、`EventMsg`/`Op`、配置类型、模型元数据（`openai_models`：`ModelInfo`、instructions 模板类型）、权限与沙箱类型、`exec_output`、`plan_tool`（`UpdatePlanArgs`）、`prompts/base_instructions/default.md` |
| `exec-server-protocol`（3.1k） | 执行环境服务的协议类型 |
| `app-server-protocol`（37.3k） | 前端应用的 JSON-RPC 协议（TUI/IDE 用） |
| `app-server-protocol-noop-macros` | app-server 协议 schema 派生的空宏 |
| `context-fragments` | 上下文片段类型 |
| `ext/items`（extension-items） | 扩展拥有的展示条目类型 |
| `codex-backend-openapi-models` | 云后端的 OpenAPI 模型 |
| `codex-experimental-api-macros` | 实验 API 的过程宏 |

### 2.2 模型链路

| crate | 功能 |
|---|---|
| `model-provider-info`（2.4k） | provider 注册表：`base_url`、`env_key`、`wire_api`、重试与超时 |
| `model-provider` | 运行时 provider 抽象：Bedrock/GCP、认证组合、请求准备、workspace 路由 |
| `models-manager` | 模型目录（`models.json`）：`ModelInfo`、每模型 `instructions_template` 渲染 |
| `prompts` | 指令模板：model_instructions、update_plan_instructions、permissions_instructions、multi_agent_instructions、compact、review、realtime |
| `http-client`（11.6k） | reqwest 封装：TLS（rustls/native roots）、代理、请求/响应类型、流式 body |
| `codex-client`（300 行） | `Provider`、重试策略、通用 SSE 转发、传输 trait |
| `codex-api`（16k） | `/responses` 请求构造（`ResponsesApiRequest`）、SSE 事件解析、endpoint session、auth header；另含 realtime websocket、images/search/memories 端点 |
| `websocket-client` / `websocket-auth` | WS 连接建立与鉴权 |
| `network-proxy` | 代理与网络出口策略 |
| `utils/rustls-provider` | TLS 后端选择 |
| `login`（21.9k） | ChatGPT OAuth 与 API key 认证 |
| `chatgpt` | ChatGPT 后端客户端 |
| `backend-client` | Codex 云后端客户端（含 analytics session） |
| `lmstudio` | LM Studio 本地模型接入 |

### 2.3 内核 `core`（242.7k 行，内部依赖 71 个 crate）

| 子系统 | 位置 | 功能 |
|---|---|---|
| 线程与任务 | `codex_thread.rs`、`thread_manager/`、`tasks/`、`spawn.rs` | 会话（thread）生命周期、任务调度、子线程派生 |
| 会话与回合 | `session/`（`session.rs`、`turn.rs`、`step_context`、`step_settings`、`world_state`、`submission`、`input_queue`、`token_budget`、`config_refresh`） | 回合循环、输入队列、步设置、世界状态、token 账本 |
| 模型客户端 | `client.rs`、`client_common.rs` | `ModelClient`/`Prompt` 组装、auth、重试路由、`ResponsesHttpExecutor` |
| 上下文 | `context/`、`context_manager/` | 上下文片段：base_instructions、environment_context、user_instructions（AGENTS.md）、developer_instructions、guardian、压缩摘要等 |
| 压缩 | `compact*.rs` | 本地/远端上下文压缩、工具输出保留、token 预算 |
| 工具系统 | `tools/`（registry、router、orchestrator、parallel、approvals、sandboxing、events、lifecycle、handlers/、runtimes/） | 工具注册、路由、并发调度、审批、沙箱接线、handler 实现 |
| 统一执行 | `unified_exec/` | 进程会话：PTY、后台会话、异步输出、`write_stdin` |
| 命令执行 | `exec.rs`、`exec_env.rs`、`exec_policy*` | 命令执行与策略接线 |
| 安全决策 | `safety.rs`、`sandboxing/`、`sandbox_tags.rs` | 审批与沙箱决策、沙箱路由 |
| Shell | `shell.rs`、`shell_snapshot*.rs` | 用户 shell 检测与环境快照 |
| AGENTS.md | `agents_md*.rs` | 发现与合并 AGENTS.md |
| Guardian | `guardian/` | 安全审查接线 |
| 持久化接线 | `rollout.rs`、`state_db_bridge.rs` | 轨迹与状态库 |
| 配置接线 | `config/` | keyring、permission profile、网络配置、配置编辑 |
| 其他接线 | `mcp*`、`elicitation`、`connectors`、`apps/`、`web_search`、`skills`、`realtime_*`、`agent*` | MCP、征询、连接器、应用、搜索、技能、实时语音、多智能体 |

### 2.4 工具与执行环境

| crate | 功能 |
|---|---|
| `tools`（7.2k） | 工具原语：`ToolSpec`/`JsonSchema`/`ResponsesApiTool`、`ToolExecutor`、`ToolCall`、动态工具、MCP 工具转换 |
| `apply-patch`（4.8k） | 补丁格式解析（parser + streaming_parser）与应用，文件操作走 `ExecutorFileSystem` |
| `exec-server`（52k） | 执行环境服务：环境注册（本地/远端）、文件系统抽象（`ExecutorFileSystem`）、进程执行、能力发现、客户端传输 |
| `exec-server-cli` | 独立执行器启动入口 |
| `file-system` | 文件系统抽象与沙箱上下文类型 |
| `sandboxing`（10.5k） | seatbelt（macOS）、landlock + bwrap（Linux）、策略变换、spawn、violation |
| `linux-sandbox` / `bwrap` | Linux 沙箱启动器 / bubblewrap 封装 |
| `execpolicy` | 命令策略：rule、decision、parser、amend（白名单追加） |
| `shell-command`（9.4k） | 命令解析与安全工具 |
| `utils/pty` | PTY 封装 |
| `utils/process` | 后台进程构造 |
| `utils/output-truncation` | 工具与命令输出截断 |
| `utils/path-uri`、`utils/absolute-path`、`utils/path-utils` | 路径类型与规范化 |
| `utils/stream-parser`、`utils/head-tail-buffer` | 流式解析、头尾缓冲 |
| `file-search` / `file-watcher` | 文件模糊搜索 / 文件监视 |
| `worktree`、`git-utils`、`utils/git-discovery` | git worktree 与仓库信息 |

### 2.5 配置与特性

| crate | 功能 |
|---|---|
| `config`（32.6k） | `config.toml` 加载、分层与合并、application requirements、MCP/skills 配置、权限 profile |
| `config-schema` | 配置 schema fixture 生成 |
| `codex-home` | `CODEX_HOME` 解析 |
| `features`（3.3k） | 集中式功能开关 |
| `cloud-config` | 云端下发配置 |
| `install-context` / `build-info` | 安装来源上下文 / 版本与构建信息 |

### 2.6 持久化与状态

| crate | 功能 |
|---|---|
| `history`（4.4k） | 模型历史与 rollout 领域类型 |
| `state` | SQLite 的 rollout 元数据 |
| `rollout`（19.3k） | 会话轨迹 JSONL 持久化与发现 |
| `thread-store` | 存储无关的线程持久化接口 |
| `attachment-store` | 附件持久化 |
| `agent-graph-store` | 多智能体父子拓扑存储 |
| `keyring-store` / `secrets` | 系统 keyring 凭据 / 敏感字符串处理 |
| `memories/read`、`memories/write` | 记忆读写路径 |

### 2.7 扩展、插件与 MCP

| crate | 功能 |
|---|---|
| `ext/extension-api` | 扩展框架 API |
| `ext/*`（16 个） | agent、agent-message-board、connectors、git-attribution、goal、guardian-reviewer、history-notes、image-generation、mcp、memories、multi-env-copy-file、queue、skills、web-search |
| `plugin`、`plugin-host`、`core-plugins`、`core-plugin-common`、`utils/plugins` | V8 插件进程、插件包模型 |
| `skills`（2.8k） | SKILL.md 加载与目录 |
| `codex-mcp`（31.5k）/ `rmcp-client`（33.1k） | MCP 工具桥接 / MCP 传输 |
| `message-board`、`message-board-mcp` | 消息板与 MCP 适配 |
| `connectors` | ChatGPT 连接器与应用声明 |
| `hooks`（17.2k） | PreToolUse/PostToolUse 钩子引擎 |
| `collaboration-mode-templates` | 协作模式模板 |
| `external-agent-migration` | 外部智能体配置迁移 |

### 2.8 多智能体

`agent-identity`、`agent-roles`、`agent-graph-store`、`agent-message-board-client`、`ext/agent`、`ext/agent-message-board`，以及 core 内 `session/multi_agents.rs`、`tools/handlers/multi_agents*`。

### 2.9 Guardian（安全审查）

`guardian-context`、`ext/guardian-reviewer`、core 内 `guardian/`、`tools/native_tool_review`。当前 HEAD 刚移除 Guardian v2，该子系统仍在变动。

### 2.10 前端与入口

| crate | 功能 | 规模 |
|---|---|---|
| `cli` | 主二进制 `codex` | 27.8k 行 |
| `exec` | 无头执行 `codex-exec` | 6.6k 行 |
| `tui` | ratatui 终端界面 | 470k 行 |
| `app-server`（+client/daemon/transport） | 面向 GUI 的 JSON-RPC 服务 | 68.8k 行 |
| `arg0` | argv[0] 多入口分发 | — |
| `terminal-detection`、`ansi-escape`、`mermaid` | 终端渲染辅助 | — |
| `feedback`、`diagnostics`、`user-verification` | 反馈、诊断、设备凭据 | — |

### 2.11 语音与实时

`realtime-webrtc`、`voice-host`、`utils/audio`，以及 core 内 `realtime_*`。

### 2.12 遥测（按要求忽略）

`otel`（7k）、`otel-trace-websocket`、`analytics`（18.4k）、`rollout-trace`（14.6k）、`direct-tool-timing`、`response-debug-context`。

### 2.13 测试支持与其余 utils

`test-binary-support`、`protocol/tests/support`、`exec-server/tests/support`、`utils/cargo-bin`；
`async-utils`、`uds`、`process-hardening`、`aws-auth`、`workload-identity`、`utils/cache`、`utils/critical-path`、`utils/elapsed`、`utils/fuzzy-match`、`utils/home-dir`、`utils/json-to-toml`、`utils/redacted-string`、`utils/sleep-inhibitor`、`utils/string`、`utils/template`、`utils/approval-presets`、`utils/cli`、`utils/oss`、`utils/image`、`utils/audio`、`utils/sandbox-summary`。

## 3. 内部依赖事实（快照 HEAD）

只列 codex 内部边，第三方依赖省略：

```text
protocol          -> execpolicy, http-client, network-proxy, ext/items, async-utils,
                     utils{absolute-path, image, path-uri, redacted-string, string}
execpolicy        -> utils/absolute-path
http-client       -> utils/rustls-provider
codex-client      -> http-client
codex-api         -> codex-client, http-client, protocol, websocket-client, utils/rustls-provider
model-provider-info -> codex-client, http-client, protocol, utils/redacted-string
models-manager    -> http-client, login, otel, protocol, prompts, utils/output-truncation
features          -> network-proxy, protocol, utils/redacted-string
shell-command     -> protocol, utils/absolute-path
file-system       -> protocol, utils/absolute-path, utils/path-uri
sandboxing        -> network-proxy, protocol, uds, utils/absolute-path, utils/path-uri, utils/pty
history           -> protocol
config            -> execpolicy, features, file-system, git-utils, http-client,
                     model-provider-info, network-proxy, protocol,
                     utils{absolute-path, path, path-uri, redacted-string}
state             -> file-system, history, protocol, utils/absolute-path
rollout           -> history, state, file-search, git-utils, ext/items, otel, protocol, utils{...}
thread-store      -> app-server-protocol, rollout, state, git-utils, install-context, otel, ext/items, protocol
exec-server       -> codex-api, config, sandboxing, shell-command, skills, file-system,
                     file-watcher, network-proxy, exec-server-protocol, build-info,
                     websocket-auth, websocket-client, otel, protocol,
                     utils{process, home-dir, path-uri, pty, redacted-string, rustls-provider, absolute-path}
apply-patch       -> exec-server, utils/absolute-path, utils/path-uri
tools             -> connectors, attachment-store, features, file-system, ext/items, protocol,
                     utils{output-truncation, path-uri, string}
core              -> 以上几乎全部，共 71 个内部 crate
```

## 4. 三个工具的忠实依赖闭包

以 system prompt 承诺的三个工具为准：

- `update_plan`：`tools`（spec 原语）+ `protocol`（`UpdatePlanArgs`、`EventMsg::PlanUpdate`）+ `prompts/update_plan_instructions` + core 的 session 事件通道。`tools` 自身再拉入 `connectors`、`attachment-store`、`features`、`file-system`、`ext/items`。
- `exec_command`：core 的 `tools/handlers/unified_exec` + `unified_exec/`（进程会话、PTY、`write_stdin`）+ `utils/pty` + `utils/process` + `utils/output-truncation` + `sandboxing` + `execpolicy` + `shell-command` + core 的 `safety`/`approvals` + `config` 的权限 profile + `file-system` + `hooks`（工具前后钩子）。
- `apply_patch`：`apply-patch` crate + `exec-server`（`ExecutorFileSystem`）+ `file-system` + `utils/path-uri` + `utils/absolute-path` + `sandboxing`（策略）+ core 的 `turn_diff_tracker` 与事件。`exec-server` 再拉入 `config`、`codex-api`、`skills`、`file-watcher`、`network-proxy`、`websocket-*`。

结论：忠实复刻这三个工具的依赖闭包，覆盖 protocol、config、features、file-system、shell-command、execpolicy、sandboxing、exec-server、apply-patch、tools、utils 家族，以及 core 的 session/tools 子系统；即除前端、云、遥测、多智能体、MCP、持久化之外的大部分底座。

## 5. 迁移顺序草案（待能力范围确认）

按依赖图自底向上分层；同层内可并行：

| 层 | 内容 | 说明 |
|---|---|---|
| L0 | utils 家族（absolute-path、path-uri、path、string、redacted-string、output-truncation、process、pty、home-dir、head-tail-buffer、stream-parser）、async-utils、uds | 几乎无内部依赖 |
| L1 | execpolicy、ext/items、network-proxy、http-client（+rustls-provider）、file-system、history | 依赖 L0 |
| L2 | protocol、features、shell-command、sandboxing | protocol 依赖 execpolicy/http-client/network-proxy |
| L3 | codex-client、model-provider-info、prompts、config | config 依赖 L1/L2 多个 crate |
| L4 | codex-api、state、tools | codex-api 的 websocket 部分可按范围裁剪 |
| L5 | exec-server、apply-patch、rollout、skills | exec-server 依赖 config+api+sandboxing |
| L6 | core：config 接线 → context → client → session/turn → tools/unified_exec → compact | 按子系统推进 |
| L7 | cli 入口 | 最后 |

待定决策点：

1. `models-manager` 依赖 `login` 与 `otel`：网关场景没有模型目录，是整体跳过（放弃每模型 instructions 渲染），还是裁剪迁移。
2. `sandboxing`：seatbelt（macOS）与 landlock/bwrap（Linux）迁哪个平台，还是都迁。
3. 持久化（`history`/`state`/`rollout`/`thread-store`）：本轮是否纳入。
4. `tools` crate 依赖 `connectors`/`attachment-store`：整体迁移，还是只迁工具原语模块。
5. `hooks`：exec_command 忠实实现包含钩子调用点，是否本轮纳入。

`exec-server` 迁移本地执行环境及所需共享协议；模型网关、网页搜索和 MCP 按各自功能保留网络传输。

## 6. rsi-core 当前状态与差距

已迁移（均为简化实现，非复刻）：

- 模型链路薄层：`rsi-core-protocol`（消息子集）、`rsi-core-client`、`rsi-core-http-client`、`rsi-core-api`、`rsi-core-model-provider-info`。
- 配置子集：`rsi-core/src/config.rs`。
- 会话：`Conversation`（多轮历史 + usage）。
- base instructions：`default.md` 已翻译为中文，以 developer message 发送。

2026-11-03 的简化版工具实现（`exec_command`、`apply_patch`、`update_plan`、工具循环、`rsi-core-tools`、`rsi-core-apply-patch`）已全部回滚删除；工具按第 4、5 节的忠实依赖闭包与分层顺序重新迁移。

未迁移：工具与工具循环、sandboxing、execpolicy、审批、PTY 会话、hooks、skills、MCP、持久化、压缩、多智能体、Guardian、前端矩阵。

## 7. 一次模型请求与一次回合的路径（参考）

```text
core::session::turn::run_turn
  -> core::client::ModelClientSession::stream
  -> codex_api::endpoint::responses::ResponsesClient::stream_request
  -> codex_api::endpoint::session::EndpointSession
  -> codex_client::Provider + RetryPolicy
  -> codex_http_client::HttpClient (reqwest)
  -> POST {base_url}/responses
  -> SSE -> codex_api::sse::responses::process_responses_event
  -> ResponseEvent -> 回合循环
  -> function_call -> ToolRouter -> handlers -> function_call_output -> 下一轮采样
```
