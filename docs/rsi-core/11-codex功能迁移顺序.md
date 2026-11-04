# Codex 功能迁移顺序

本文记录对 `rsi-core` 迁移顺序的建议，供后续逐项讨论和实施。参考源码为 `/Users/hiro/codex`，HEAD 为 `bd199bc4db`，工作区同时存在未提交修改。下文的源码位置相对于 `/Users/hiro/codex/codex-rs`。

本仓库的目标是复现 Codex 的 Agent 内核。每次迁移一个可以编译、运行和验证的功能，相关目录、模块、类型和运行行为以 Codex 源码为依据。内核保留多模态上下文、文件搜索、网页搜索、多智能体、记忆和长期目标。终端入口用于调试，内核通过独立接口提供能力。

模型接入使用 OpenAI Responses API，其他模型服务商的接入功能不在迁移范围内。TUI、图片生成与编辑、远程执行环境和插件功能不在迁移范围内。MCP、Hooks、Skills 和内核扩展注册继续提供各自的能力；模型服务、网页搜索和 MCP 服务可以通过网络访问。RSI 的实验编排、证据、评测和裁决属于后续框架。

## 迁移时如何处理依赖

功能顺序描述的是可验收的交付顺序。每一项实现时，沿实际调用路径引入所需的协议、配置、数据类型和真实实现；某个 crate 中被当前功能调用的部分随该功能一起迁移，整个 crate 的其他功能可以在对应项目中完成。每项交付都需要包含能够真实运行的依赖。

编译依赖与功能前置条件需要分别检查。以下关系来自当前 Cargo 配置：

- `codex-core` 直接依赖 `protocol`、`config`、`tools`、`exec-server`、`extension-api`、`hooks`、`thread-store` 等多个 crate。迁移会话代码时，需要根据实际引用确定同时迁移的源码范围。[core/Cargo.toml](/Users/hiro/codex/codex-rs/core/Cargo.toml)
- `protocol` 直接依赖 `execpolicy`、`http-client`、`network-proxy` 和 `ext/items` 等组件；`codex-api` 依赖 `codex-client`、`http-client`、`protocol` 和 `websocket-client`。[protocol/Cargo.toml](/Users/hiro/codex/codex-rs/protocol/Cargo.toml)、[codex-api/Cargo.toml](/Users/hiro/codex/codex-rs/codex-api/Cargo.toml)
- `file-system` 和 `history` 都依赖 `protocol`；`config` 依赖 `file-system`、`features` 和 `protocol`。[file-system/Cargo.toml](/Users/hiro/codex/codex-rs/file-system/Cargo.toml)、[history/Cargo.toml](/Users/hiro/codex/codex-rs/history/Cargo.toml)、[config/Cargo.toml](/Users/hiro/codex/codex-rs/config/Cargo.toml)
- `tools` 直接依赖 `attachment-store`、`connectors`、`features`、`file-system` 和 `protocol` 等组件。[tools/Cargo.toml](/Users/hiro/codex/codex-rs/tools/Cargo.toml)
- `apply-patch` 依赖 `exec-server`；`exec-server` 又依赖 `file-system`、`config`、`sandboxing`、`skills` 和通信组件。因此文件补丁排在执行环境之后。[apply-patch/Cargo.toml](/Users/hiro/codex/codex-rs/apply-patch/Cargo.toml)、[exec-server/Cargo.toml](/Users/hiro/codex/codex-rs/exec-server/Cargo.toml)
- `thread-store` 依赖 `rollout`、`state` 和 `app-server-protocol`。这些协议类型与完整的应用服务分别处理。[thread-store/Cargo.toml](/Users/hiro/codex/codex-rs/thread-store/Cargo.toml)
- `extension-api` 直接依赖 `codex-mcp`、`tools`、`config` 和 `history`；`ext/skills` 依赖 `codex-mcp`、`exec-server` 和 `skills`。实现这些功能时，需要一并处理对应的编译依赖。[extension-api/Cargo.toml](/Users/hiro/codex/codex-rs/ext/extension-api/Cargo.toml)、[ext/skills/Cargo.toml](/Users/hiro/codex/codex-rs/ext/skills/Cargo.toml)

`exec-server` 中的本地执行、文件系统与共享协议服务于内核工具；远程执行环境的连接、远程进程和远程文件系统功能移除。模型通信、网页搜索和 MCP 所需的网络传输继续保留。插件相关 crate 中若有保留功能使用的共享类型或实现，按实际调用关系处理；插件包加载、插件管理和插件宿主功能移除。

功能之间的主要前置关系如下：

- 第 2 项使用第 1 项的模型会话与事件通道；第 3 项使用第 2 项的工具调用循环；第 4 项使用第 2 项的工具路由和第 3 项的执行环境。
- 第 5 项使用第 1 项的模型会话、第 2 项的工具调用和第 3 项的文件访问；第 6 项使用模型会话和工作区访问；第 7 项使用上下文装配；第 8 项保存并恢复会话和压缩后的历史。
- 第 9 项使用工具调用与执行环境；第 10 项将 Hook 接入模型、工具、压缩和 MCP；第 11 项使用上下文、执行环境和 MCP；第 12 项使用工具注册与 MCP 工具目录。
- 第 13 项使用工作区访问；第 14 项使用模型服务和扩展注册；第 15 项使用会话、工具与状态管理；第 16 项使用会话、轨迹和状态；第 17 项使用扩展注册与状态管理。

当前项目已有 Responses 客户端、部分协议类型、内存会话和调试终端。第一项以当前工作区内容为起点，核对并完成相应的 Codex 模型会话路径。

## 顺序

### 1. 模型会话、协议与事件通道

迁移会话线程的创建与关闭、用户输入、Turn 生命周期、模型请求、流式响应、错误、中断和 Token 用量。迁移 OpenAI Responses API 请求需要的配置、提示词和历史类型，保留 `ResponseItem`、reasoning 等模型返回内容。调试终端通过内核接口或者 app-server WebSocket 提交输入和接收事件。

源码位置：`protocol`、`model-provider-info`、`model-provider`、`models-manager`、`http-client`、`codex-client`、`codex-api`、`core/src/client.rs`、`core/src/thread_manager.rs`、`core/src/codex_thread.rs`、`core/src/session/`、`core/src/context_manager/`、`app-server`、`app-server-protocol`、`app-server-client`、`app-server-transport`。

验收：通过真实 Responses 服务完成连续多轮对话，检查流式文本、历史内容、reasoning 条目、用量、错误和中断事件。使用真实服务验证 HTTP/SSE 和 WebSocket 路径。

当前 HTTP/SSE 路径已通过真实服务验证。模型服务的 Responses WebSocket 路径需要使用支持该传输方式的真实服务完成验证。调试终端与独立 app-server 的 WebSocket 通信已通过真实进程测试。

### 2. 工具调用循环与计划更新

迁移工具定义、注册、路由、调用结果和回合内的再次采样。以 Codex 的 `update_plan` 作为第一个实际工具，接通 `function_call`、`function_call_output`、`call_id`、计划事件和工具错误。按 Codex 工具自身的并行能力处理调度和结果顺序。

源码位置：`tools/`、`core/src/tools/registry.rs`、`core/src/tools/router.rs`、`core/src/tools/parallel.rs`、`core/src/tools/handlers/plan*.rs`、`protocol/src/plan_tool.rs`、`prompts`。

验收：真实模型通过工具更新计划，收到工具结果后继续完成任务；事件中可以看到计划更新和回合完成，工具参数错误可以进入 Codex 的错误处理路径。

### 3. 本地执行环境与命令工具

迁移本地执行环境、文件系统接口和统一进程会话，接通 `exec_command`、`write_stdin`、工作目录、输出读取、输出截断、进程状态、PTY 和进程终止。迁移该路径实际使用的 shell、命令策略、沙箱和工具生命周期代码，执行行为与配置保持 Codex 语义。

源码位置：`file-system`、`exec-server` 的本地环境与进程实现、`exec-server-protocol`、`core/src/unified_exec/`、`core/src/tools/handlers/unified_exec/`、`shell-command`、`execpolicy`、`sandboxing`、`utils/pty`、`utils/process`。

验收：模型执行真实命令并读取输出；长时间运行的进程可以继续读取输出和接收标准输入；验证工作目录、PTY、超时、中断、退出状态和输出截断。

### 4. 文件补丁

迁移 `apply_patch` 的工具描述、自由格式调用、补丁解析、文件系统访问、变更事件和差异记录。补丁通过执行环境的文件系统接口应用。

源码位置：`apply-patch`、`core/src/tools/handlers/apply_patch*.rs`、`core/src/turn_diff_tracker.rs`、`core/assets/tools/apply_patch.lark`。

验收：通过支持该工具形式的真实模型服务发起补丁调用，在实际工作区验证新增、修改、移动和删除文件，并检查失败结果与文件变更事件。

### 5. 多模态上下文与图片读取

迁移多模态输入与工具结果的协议、模型请求装配、附件处理和 `view_image`。支持用户输入和工具结果中的图片，以及 Codex 协议中的其他多模态内容。图片输入支持本地路径、URL、内联数据和文件 ID 等形式，具体格式遵循对应模型服务的约束；处理模型支持的图片细节、编码和尺寸。

源码位置：`protocol/src/user_input.rs`、`protocol/src/models.rs`、`core/src/image_preparation.rs`、`core/src/tools/handlers/view_image*.rs`、`core/src/context_manager/image_availability.rs`、`attachment-store`、`utils/image`。

验收：通过真实多模态模型使用本地图片、图片引用和工具返回的图片完成任务；验证模型服务支持的输入形式、图片细节和多轮上下文中的图片内容。

### 6. 指令与上下文装配

迁移模型指令、基础指令、开发者指令、环境信息和 `AGENTS.md` 的发现与装配。迁移模型目录中影响工具和提示词的配置，以及会话历史和上下文更新所需的类型。

源码位置：`prompts`、`protocol/src/openai_models`、`models-manager`、`context-fragments`、`core/src/context/`、`core/src/context_manager/`、`core/src/agents_md*.rs`。

验收：使用实际工作区和多层 `AGENTS.md` 运行任务，检查不同来源的指令、模型设置、工作目录和工具描述进入模型请求的内容。

### 7. 上下文压缩与用量管理

迁移历史 Token 估算、上下文窗口判断、压缩触发、摘要生成、压缩后历史替换及相关事件。保留 Codex 中适用的本地压缩与模型服务提供的远端压缩路径，以及与压缩关联的工具输出和恢复信息。

源码位置：`core/src/compact*.rs`、`core/src/session/context_window.rs`、`core/src/session/async_compaction.rs`、`core/src/session/token_budget.rs`、`history`、`prompts`。

验收：通过真实模型运行长会话，触发手动与自动压缩，检查压缩事件、Token 用量、多模态内容的处理和压缩后的后续工具调用；使用支持远端压缩的真实服务验证对应路径。

### 8. 会话轨迹、持久化与恢复

迁移会话历史记录、JSONL 轨迹、SQLite 状态和线程存储接口，接通会话查询、恢复和派生。区分运行时 `EventMsg`、持久化 `RolloutItem` 与终端展示事件，保持各自的字段和用途。

源码位置：`history`、`rollout`、`state`、`thread-store`、`core/src/rollout.rs`、`core/src/session/rollout_reconstruction.rs`。

验收：完成包含命令、补丁和多模态输入的真实任务，检查事件、历史、Token 用量和工作区变更；关闭并恢复会话后继续执行，验证中断回合的恢复行为和会话派生。

### 9. 扩展注册与 MCP

迁移扩展注册接口、MCP 服务配置、连接、工具发现和调用，以及资源、资源模板的列出与读取。接通 MCP 工具的名称、参数、结果、生命周期事件和会话上下文。

源码位置：`ext/extension-api`、`ext/mcp`、`codex-mcp`、`rmcp-client`、`core/src/mcp*`、`core/src/session/mcp*`、`core/src/tools/handlers/mcp*`。

验收：连接真实 MCP 服务，分别验证 stdio 和 Streamable HTTP 传输；由模型调用实际工具并读取资源，检查结果、错误、中断和服务重新连接。

### 10. Hooks

迁移 Codex 的 Hook 配置、发现、匹配、执行和结果处理，覆盖会话、模型请求、工具、压缩、停止与中断等生命周期。第 1 至第 9 项沿调用路径需要的 Hook 接口和执行代码随相关功能迁移；本项完成各类 Hook 的完整接入。

源码位置：`hooks`、`core/src/hook_runtime*`、`core/src/hook_environment_executor.rs`、`core/src/hook_mcp_executor.rs`、`protocol/src/hooks.rs`。

验收：通过真实进程和 MCP 服务执行 Hook，检查模型请求前后、工具调用前后、压缩和会话结束事件，以及同步、异步和中断时的行为。

### 11. Skills

迁移 Skill 的目录发现、`SKILL.md` 加载、元数据、选择、调用、相关资源与上下文注入，接通 Skill 所需的 MCP 和执行环境能力。

源码位置：`skills`、`ext/skills`、`core/src/skills.rs`、`core/src/session/step_activation.rs`。

验收：在真实工作区安装并调用 Skill，检查发现结果、模型收到的说明、资源读取和关联工具调用。

### 12. 动态工具与工具发现

迁移动态工具注册、命名空间、工具可见性和 `tool_search`，接通内置工具与 MCP 工具的发现和调用。

源码位置：`protocol/src/dynamic_tools.rs`、`core/src/tools/spec_plan.rs`、`core/src/tools/handlers/dynamic.rs`、`core/src/tools/handlers/tool_search*.rs`。

验收：向会话提供真实工具，验证工具发现、命名空间、参数、调用结果及会话运行中的工具目录更新。

### 13. 文件搜索

迁移 `file-search` 的文件路径发现和模糊匹配，保留通过命令执行进行文件内容搜索的能力。文件搜索使用本地执行环境和工作区访问接口。

源码位置：`file-search`、`core/src/unified_exec/`、`exec-server` 的本地文件系统实现。

验收：在实际工作区按名称搜索文件，并通过命令搜索文件内容，验证匹配结果及工具输出。

### 14. 网页搜索

迁移网页搜索的工具定义、模型服务接入、结果与引用处理，保留源码提供的搜索、打开页面、访问链接和页面内查找等能力。

源码位置：`ext/web-search`、`codex-api/src/search.rs`、`codex-api/src/endpoint/search.rs`、`core/src/web_search.rs`。

验收：通过真实搜索服务执行搜索、打开页面和查找内容，由模型使用搜索结果并保留来源。

### 15. 多智能体

迁移子会话、Agent 角色、任务派发、消息、等待、中断和生命周期管理，以及 Agent 关系和消息所需的存储。

源码位置：`core/src/agent/`、`core/src/session/multi_agents.rs`、`core/src/tools/handlers/multi_agents*`、`agent-identity`、`agent-roles`、`agent-graph-store`、`ext/agent`、`ext/agent-message-board`、`message-board`。

验收：执行真实子任务，检查子智能体创建、上下文、消息传递、等待、中断和结果返回。

### 16. 记忆

迁移记忆的生成、读取、搜索、存储和上下文注入，复用会话、轨迹与状态能力。

源码位置：`memories/read`、`memories/write`、`ext/memories`、`core/src/context/memory.rs`、`state`、`rollout`。

验收：通过实际任务生成并保存记忆，在后续会话中检索、读取和使用记忆。

### 17. 长期目标

迁移目标创建、状态更新、用量统计、目标事件和持续执行所需的会话接线。

源码位置：`ext/goal`、`core/src/context/user_goal.rs`、`state`、`protocol/src/protocol.rs`。

验收：创建并读取真实任务目标，验证进度、用量、完成和中断后的目标状态。

## 每项功能的完成条件

- 记录 Codex 来源提交及实际使用的工作区状态，迁移相关源码与真实依赖。
- 完成编译、格式检查、静态检查和测试；覆盖正常结果、错误、中断和相关状态恢复。
- 使用真实模型网关、操作系统进程、文件系统或 MCP 服务进行该功能的端到端验证。测试产生的文件写入仓库内专门用于中间结果且已被 gitignore 的目录。
- 通过调试终端或内核接口观察实际事件、工具输入、工具结果和 Token 用量。
