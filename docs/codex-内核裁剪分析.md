# Codex 内核裁剪分析

日期：2026-11-02
对象：本仓库 `codex-rs` 工作区，提交 `3026a8e54e`
用途：确定 `rsi-kernel` 保留、删除、替换的边界

---

## 1 规模

| 指标 | 数值 |
|---|---|
| Rust crate 数（含嵌套成员） | 171 |
| 总行数 | 2,222,386 |
| 生产代码 | 1,185,629 |
| 测试代码 | 1,036,757 |

测试占 47%。"大"这个印象有一半来自测试，评估裁剪效果应看生产行数。

关键入口的依赖闭包：

| 入口 | 闭包 crate 数 | 闭包总行数 |
|---|---|---|
| `codex-cli` | 148 | 2,182,979 |
| `codex-tui` | 138 | 2,107,366 |
| `codex-exec` | 130 | 1,635,128 |
| `codex-core` | 98 | 1,340,905 |

三个结论：

1. `codex-core` 的闭包就有 98 个 crate、134 万行。core 承担的不只是 agent 循环，它是整个产品的中枢。
2. `codex-tui` 的闭包覆盖 95% 的仓库。保留 TUI 等于保留几乎全部源码。
3. 删除的驱动力不是"哪些 crate 不在闭包里"，而是"core 对每个 crate 的耦合有多深"。

---

## 2 耦合密度

core 中有多少文件引用了每个外围 crate。这个数字直接决定删除的改动量。

| crate | core 中引用文件数 | 说明 |
|---|---|---|
| `codex-features` | 115 | 功能开关，几乎每个模块都读 |
| `codex-extension-api` | 79 | 扩展注册表，提示词与工具都经过它 |
| `codex-history` | 67 | 会话历史与压缩 |
| `codex-otel` | 65 | 遥测，遍布工具与会话 |
| `codex-exec-server` | 64 | 执行环境抽象 |
| `codex-login` | 56 | 认证 |
| `codex-analytics` | 47 | 埋点 |
| `codex-mcp` | 41 | MCP 客户端 |
| `codex-network-proxy` | 39 | 沙箱网络出口 |
| `codex-guardian-reviewer` | 34 | 自动审查 |
| `codex-thread-store` | 33 | 线程持久化 |
| `codex-core-plugins` | 32 | 插件运行时 |
| `codex-code-mode` | 28 | 代码模式工具面 |
| `codex-models-manager` | 26 | 模型目录 |
| `codex-rollout-trace` | 20 | 轨迹追踪 |
| `codex-guardian-context` | 19 | 审查上下文 |
| `codex-rollout` | 16 | 轨迹记录 |
| `codex-hooks` | 15 | 工具前后钩子 |
| `codex-connectors` | 13 | 云端连接器 |
| `codex-state` | 10 | 状态库 |
| `codex-rmcp-client` | 10 | MCP 传输 |
| `codex-plugin` | 8 | 插件清单 |
| `codex-agent-graph-store` | 7 | 多智能体图 |
| `codex-diagnostics` | 7 | 诊断 |
| `codex-direct-tool-timing` | 7 | 工具计时 |
| `codex-feedback` | 6 | 用户反馈 |
| `codex-app-server-protocol` | 5 | 只用到线程历史与回合状态 |
| `codex-skills` | 5 | 技能加载 |
| `codex-attachment-store` | 9 | 附件 |
| `codex-memories-read` | 2 | 记忆 |
| `codex-message-board` | 3 | 智能体留言板 |
| `codex-install-context` | 2 | 安装上下文 |
| `codex-terminal-detection` | 1 | 终端探测 |
| `codex-response-debug-context` | 1 | 调试上下文 |

引用文件数少于 10 的可以整块移除；10 到 40 的按模块剥离；超过 40 的要么保留，要么改成空实现（把 crate 留下、内部清空），避免改动上百个文件。

---

## 3 三种处理方式

| 方式 | 做法 | 适用 |
|---|---|---|
| 删除 | 从工作区与依赖中移除，修掉引用点 | 耦合低、功能与 RSI 无关 |
| 替换实现 | 保留 crate 名与公开接口，内部改成空实现或最简实现 | 耦合高、但功能不需要（遥测、分析、认证） |
| 保留 | 原样保留 | 内核运转必需 |

---

## 4 分组决策

行数为该目录的全部 Rust 行，括号内为生产行。

### 4.1 会话内核：保留

| 目录 | 行数 | 职责 |
|---|---|---|
| `core` | 489,085（152,237） | 会话、回合、工具调度、模型客户端、压缩 |
| `protocol` | 33,965（30,115） | 消息、事件、条目类型 |
| `tools` | 8,264 | 工具定义与执行契约 |
| `config` | 33,673（24,847） | 配置解析 |
| `config-schema` | 22 | 配置结构 |
| `features` | 4,126 | 功能开关 |
| `context-fragments` | 765 | 上下文片段 |
| `prompts` | 3,541 | 提示词装配 |
| `history` | 4,402 | 历史与截断 |
| `apply-patch` | 5,588 | 补丁应用 |
| `model-provider` | 11,660 | 模型供应商 |
| `model-provider-info` | 2,455 | 供应商配置 |
| `http-client` | 13,267 | HTTP 与 CA |
| `async-utils` | 110 | 异步工具 |
| `utils/*` | 25,980 | 路径、进程、字符串、流解析等 |
| `skills` | 2,840 | 技能加载 |

`features` 保留的理由是它足够小，而且给了我们一条"编译期保留、运行期关闭"的过渡路径，比一次性删代码安全。

### 4.2 执行与环境：保留，`exec-server` 内部再切

| 目录 | 行数 | 决定 |
|---|---|---|
| `exec-server` | 72,282（38,631） | 保留本地一半：`local_process`、`local_file_system`、`fs_sandbox`、`process_sandbox`、`environment`、`capability_discovery`；删除远端一半：`relay`、`remote`、`remote_process`、`remote_file_system`、`client*`、`connection`、`network_policy_decisions` |
| `exec-server-protocol` | 3,153 | 保留 |
| `sandboxing` | 11,683 | 保留 |
| `linux-sandbox` | 12,852 | 保留（landlock / seccomp） |
| `shell-command` | 10,547 | 保留（命令解析、危险命令识别） |
| `execpolicy` | 3,124 | 保留（命令白名单） |
| `file-system` | 1,322 | 保留 |
| `file-search` | 1,513 | 保留（`grep` 类工具） |
| `uds` | 1,090 | 保留（exec-server 与沙箱子进程通信） |
| `process-hardening` | 193 | 保留 |
| `git-utils` | 5,359 | 保留 |
| `linux-sandbox` 之外的平台沙箱 | — | 删除，见 4.7 |

### 4.3 工具与扩展：部分保留

| 目录 | 行数 | 决定 | 理由 |
|---|---|---|---|
| `codex-mcp` | 31,525 | 保留 | 框架向内核注入工具走 MCP |
| `rmcp-client` | 42,632 | 保留 | MCP 传输实现 |
| `hooks` | 17,222 | 保留 | 工具前后钩子，用于审计、预算、内容脱敏 |
| `ext/extension-api` | 3,526 | 保留 | 扩展注册表，提示词片段与工具策略的挂载点 |
| `ext/skills` | 30,236 | 阶段二删 | 技能发现与目录，与 `skills` crate 重叠；RSI 的阶段说明由框架注入 |
| `ext/items` | 520 | 阶段二删 | 条目扩展 |
| `ext/history-notes` | 1,809 | 删除 | 历史注记 |
| `ext/image-generation` | 1,784 | 删除 | 图像生成 |
| `ext/web-search` | 1,126 | 删除 | 联网搜索 |
| `ext/goal` | 5,879 | 删除 | 目标追踪 |
| `ext/queue` | 1,708 | 删除 | 队列 |
| `ext/multi-env-copy-file` | 1,394 | 删除 | 多环境文件复制 |
| `ext/git-attribution` | 438 | 删除 | Git 归属 |
| `ext/mcp` | 5,956 | 删除 | MCP 插件市场 |
| `ext/connectors` | 74 | 删除 | 连接器 |

### 4.4 会话持久化：阶段一保留，阶段二评估

| 目录 | 行数 | 决定 |
|---|---|---|
| `state` | 29,985（23,379） | 阶段二评估。RSI 的会话是短生命周期，是否需要数据库取决于断点续跑的要求 |
| `thread-store` | 37,273（24,563） | 同上 |
| `rollout` | 19,254（12,522） | 保留。轨迹是证据的一部分，面板与回放都要读 |
| `rollout-trace` | 14,821（9,986） | 保留。与 `rollout` 一起构成事件流 |
| `message-history` | 1,437 | 阶段二删 |

### 4.5 遥测与认证：替换实现

| 目录 | 行数 | 处理 |
|---|---|---|
| `otel` | 10,824 | 保留 crate 名，实现改成空写入 |
| `otel-trace-websocket` | 179 | 删除 |
| `analytics` | 18,375（8,529） | 保留 crate 名，事件构造保留、上报清空 |
| `feedback` | 4,032 | 替换为空实现 |
| `direct-tool-timing` | 776 | 保留（工具耗时对预算核算有用） |
| `diagnostics` | 268 | 保留 |
| `login` | 25,907（12,198） | 阶段二替换。第一期只走 `env_key` 的 API key，认证路径保留但不再被调用 |
| `chatgpt` | 992 | 删除 |
| `aws-auth` | 1,314 | 删除 |
| `keyring-store` | 453 | 删除 |
| `secrets` | 1,163 | 保留（供 MCP 令牌读取） |
| `workload-identity` | 972 | 删除 |
| `agent-identity` | 1,000 | 删除 |
| `user-verification` | 1,440 | 删除 |
| `models-manager` | 4,285 | 阶段二删。模型目录由框架配置提供 |

### 4.6 云端与应用服务：删除

| 目录 | 行数 |
|---|---|
| `app-server` | 210,934（56,571） |
| `app-server-transport` | 19,127（14,526） |
| `app-server-daemon` | 12,221（8,073） |
| `app-server-test-client` | 3,936 |
| `core-api` | 153 |
| `cloud-config` | 4,079 |
| `cloud-tasks` | 5,411 |
| `cloud-tasks-client` | 1,117 |
| `cloud-tasks-mock-client` | 270 |
| `cloud-client` | 694 |
| `backend-client` | 5,038 |
| `codex-api` | 18,812（14,217） |
| `codex-backend-openapi-models` | 1,258 |
| `codex-client` | 300 |
| `external-agent-migration` | 16,593（13,320） |
| `responses-api-proxy` | 2,815 |
| `stdio-to-uds` | 223 |
| `tcp-tunnel` | 1,412 |
| `v8-poc` | 427 |
| `thread-manager-sample` | 515 |
| `test-binary-support` | 77 |

`app-server` 一项就 21 万行。它承担三件事：JSON-RPC 服务、编辑器与桌面端集成、事件投影。前两件我们不需要；第三件里的事件投影由我们自己的事件流取代，条目类型保留在 `app-server-protocol` 的 v2 部分。

### 4.7 多智能体、守卫、记忆、插件、代码模式：删除

| 目录 | 行数 | 说明 |
|---|---|---|
| `agent-roles` | 592 | 多智能体角色 |
| `agent-graph-store` | 481 | 智能体图存储 |
| `agent-message-board-client` | 982 | 智能体留言板客户端 |
| `message-board` | 6,305 | 留言板服务 |
| `message-board-mcp` | 173 | 留言板 MCP |
| `runtime-agent-instance` | 1,855 | 运行时实例 |
| `collaboration-mode-templates` | 2 | 协作模式 |
| `guardian-context` | 11,777 | 自动审查上下文 |
| `ext/guardian-v2` | 17,796 | 自动审查二代 |
| `ext/guardian-reviewer` | 7,420 | 自动审查器 |
| `ext/guardian-v3` | 22 | 自动审查三代 |
| `memories`（read/write） | 6,548 | 记忆读写 |
| `ext/memories` | 2,612 | 记忆扩展 |
| `plugin` | 971 | 插件清单 |
| `plugin-host` | 676 | V8 插件宿主 |
| `core-plugins` | 51,032（27,003） | 插件运行时 |
| `core-plugin-common` | 232 | 插件公共类型 |
| `connectors` | 5,646 | 云端连接器 |
| `code-mode` | 9,863 | 代码模式 |
| `code-mode-host` | 11,638 | 代码模式宿主 |
| `code-mode-protocol` | 5,244 | 代码模式协议 |
| `code-mode-runtime` | 12,314（7,157） | 代码模式运行时 |

插件、代码模式、自动审查三项合计约 13 万行，全部与 RSI 无关。它们对 core 的耦合在 28 到 34 个文件之间，属于阶段二的工作量。

### 4.8 沙箱网络与平台

| 目录 | 行数 | 决定 |
|---|---|---|
| `network-proxy` | 35,081（25,024） | 阶段二删。容器内的网络边界由 Docker 网络与出口白名单负责 |
| `windows-sandbox-rs` | 30,369（25,347） | 删除。Linux 容器不需要 |
| `windows-sandbox-service` | 6,204 | 删除 |
| `mxc-sandbox` | 1,951 | 删除 |
| `bwrap` | 151 | 删除。bubblewrap 封装，容器内已有命名空间隔离 |
| `worktree` | 2,118 | 删除。Git 工作区由框架管理 |
| `file-watcher` | 1,626 | 删除 |
| `terminal-detection` | 1,382 | 删除 |
| `arg0` | 847 | 删除。单二进制单角色，不需要多路复用入口 |
| `install-context` | 957 | 删除 |
| `build-info` | 360 | 保留（版本号进事件流） |

### 4.9 入口：替换

| 目录 | 行数 | 处理 |
|---|---|---|
| `cli` | 44,125（28,027） | 阶段一保留并精简。它是 `codex` 二进制，TUI 与 `exec` 都由它启动；删掉依赖已删 crate 的子命令（登录、云端任务、应用服务、代理、守护进程），只留 TUI 与 `exec`。阶段三随 TUI 一起删除，换成 `rsi-kernel` |
| `exec` | 13,459（4,657） | 阶段一保留，作为对照实现与冒烟入口；阶段三删除 |
| `tui` | 479,568（310,337） | 临时保留 |
| `ansi-escape` | 142 | 随 TUI 保留 |
| `mermaid` | 2,445 | 随 TUI 保留 |
| `utils/cli`、`utils/fuzzy-match`、`utils/oss`、`utils/approval-presets`、`utils/sandbox-summary`、`utils/sleep-inhibitor`、`utils/elapsed` | 约 2,000 | 随 TUI 保留 |

---

## 5 core 内部模块

core 生产代码约 14.9 万行，按模块拆开。

| 模块 | 行数（生产） | 决定 |
|---|---|---|
| `session/` | 37,283 | 保留。回合循环、压缩、输入队列、上下文预算 |
| `tools/` | 29,899 | 保留，删处理器 |
| `config/` | 9,941 | 保留 |
| `context/` | 7,018 | 保留。提示词组装 |
| `unified_exec/` | 4,104 | 保留。命令执行工具 |
| `context_manager/` | 2,340 | 保留 |
| `tasks/` | 2,229 | 保留（后台任务表） |
| `state/` | 1,354 | 阶段二评估 |
| `guardian/` | 8,621 | 删除 |
| `agent/` | 7,334 | 删除（多智能体） |
| `plugins/` | 575 | 删除 |
| `realtime_conversation/`、`realtime_history/` | 616 | 删除 |
| `apps/` | 69 | 删除 |
| 顶层 `realtime_*.rs` | 约 4,000 | 删除 |
| 顶层 `codex_delegate.rs`、`agent_*.rs` | 约 900 | 删除 |
| 顶层 `connectors.rs` | 553 | 删除 |
| 顶层 `guardian_review.rs`、`attestation.rs` | 约 40（实现已迁到 `guardian` crate） | 删除 |
| 顶层 `mcp_skill_dependencies.rs` | 524 | 删除 |
| 顶层 `environment_selection.rs` | 2,665 | 保留（选择执行环境） |
| 顶层 `shell_snapshot.rs` | 1,210 | 保留 |
| 顶层 `exec_policy.rs` | 1,231 | 保留 |
| 顶层 `compact*.rs` | 约 4,000 | 保留（上下文压缩） |

`tools/handlers/` 逐个决定：

| 处理器 | 行数 | 决定 |
|---|---|---|
| `apply_patch` | 627 | 保留 |
| `shell_spec` + `unified_exec` | 523 | 保留 |
| `view_image` | 533 | 保留（证据里可能需要看图） |
| `mcp` + `mcp_resource` | 1,446 | 保留 |
| `current_time` | 135 | 保留 |
| `sleep` | 167 | 保留 |
| `plan` | 112 | 删除。RSI 的阶段由框架决定 |
| `request_user_input`、`request_user_input_async`、`send_message_to_user_async` | 约 700 | 删除。无人在环 |
| `request_permissions` | 264 | 删除。权限由 job 描述固定 |
| `tool_search` + `tool_search_spec` | 833 | 删除 |
| `list_available_plugins_to_install`、`request_plugin_install` | 约 1,100 | 删除 |
| `multi_agents*`（含 `multi_agents/`、`multi_agents_v2/`） | 约 9,000 | 删除 |
| `extension_tools` | 693 | 删除 |
| `dynamic` | 282 | 保留。允许框架注入工具 |
| `get_context_remaining` | 109 | 保留 |
| `wait_for_environment` | 180 | 保留 |
| `test_sync` | 196 | 删除 |

---

## 6 TUI 的约束

TUI 临时保留，代价是它把一批 crate 固定在依赖链上。TUI 中引用这些 crate 的文件数：

| crate | TUI 引用文件数 | 影响 |
|---|---|---|
| `app-server-protocol` | 330 | 不能删。它是 TUI 渲染用的条目与回合模型 |
| `app-server-client` | 110 | 不能删 |
| `state` | 30 | 不能删 |
| `feedback` | 27 | 不能删（可先改实现） |
| `backend-client` | 17 | 不能删 |
| `exec-server` | 17 | 保留，本来就要 |
| `login` | 14 | 不能删 |
| `rollout` | 13 | 保留，本来就要 |
| `models-manager` | 12 | 不能删 |
| `realtime-webrtc` | 10 | 改约 10 个文件可释放 |
| `connectors` | 10 | 改约 10 个文件可释放 |
| `worktree` | 10 | 改约 10 个文件可释放 |
| `plugin` | 8 | 改 8 个文件可释放 |
| `app-server-daemon` | 6 | 改 6 个文件可释放 |
| `cloud-config` | 3 | 改 3 个文件可释放 |
| `core-plugins` | 2 | 改 2 个文件可释放 |
| `mermaid` | 2 | 随 TUI 保留 |
| `windows-sandbox` | 1 | 改 1 个文件可释放 |

结论：

1. `app-server-protocol` 与 `app-server-client` 在 TUI 存续期间不能动。两者合计 4.2 万行，其中 `app-server-protocol` 里真正被 TUI 使用的是 v2 条目模型与线程历史投影，JSON-RPC 部分可以切掉。
2. TUI 里约 50 个文件引用了可释放的 crate。改这 50 个文件，能释放插件、实时语音、连接器、工作区、云配置、应用服务守护进程六个方向约 8 万行。
3. TUI 移除以外的部分，按第 4 节表格执行。

因此 TUI 的存续成本明确写成一条：TUI 在，`app-server-protocol`、`app-server-client`、`state`、`backend-client`、`login`、`models-manager`、`feedback` 七个 crate 不能删。

TUI 的用途是调试。内核本身没有界面，日常调试有两条通道：`exec --json` 的事件流，与在 TUI 里跑一次同样的会话看上下文与工具调用的实际形态。第二条是保留 TUI 的唯一理由，因此 `cli` 也只保留让它起来的两个入口。

`cli` 精简后的样子：`codex`（无子命令）进 TUI，`codex exec` 跑无头会话。登录、云端任务、应用服务、代理、守护进程、插件、市场这些子命令连同它们依赖的 crate 一起删除。删完之后 `cli` 从 4.4 万行降到一万行以内。

---

## 7 规模阶梯

每个 crate 只算自己的目录，嵌套成员单独计算，四种口径互不重叠：

| 口径 | crate 数 | 总行数 | 生产行数 | 生产占比 |
|---|---|---|---|---|
| 全仓 | 171 | 2,222,386 | 1,185,629 | 100% |
| 只删 `tui` 一个目录 | 170 | 1,742,818 | 875,292 | 73.8% |
| 删 TUI 与只被它需要的 39 个 crate | 131 | 1,411,094 | 738,902 | 62.3% |
| 再删只服务 `cli`、`exec` 等入口的 33 个 crate | 98 | 1,282,092 | 671,102 | 56.6% |
| 再删内核闭包里计划删除的 31 个 crate | 67 | 898,622 | 420,357 | 35.5% |

第二行与第三行之间差的 81 万行，是 TUI 拖着的整条应用服务线：`app-server` 及其传输与守护进程、`backend-client`、`cloud-config`、`chatgpt`、`external-agent-migration`、实时语音、工作区、终端界面组件，以及七个 `ext/*` 插件。TUI 一个目录只占 48 万行，它带进来的还有 33 万行。

第三行与第五行之间差的 38 万行在内核闭包内部：`core-plugins`、代码模式四个 crate、`network-proxy`、Windows 沙箱、`login`、`analytics`、`otel`、`guardian`、`memories`、`codex-api`、`app-server-protocol`、`state`、`thread-store`、`rollout-trace`。

剩下 67 个 crate、42 万生产行的构成：

| crate | 总行数 | 生产行数 |
|---|---|---|
| `core` | 480,229 | 152,237 |
| `exec-server` | 72,157 | 38,631 |
| `rmcp-client` | 42,632 | 22,226 |
| `protocol` | 33,944 | 30,115 |
| `config` | 33,673 | 24,847 |
| `codex-mcp` | 31,525 | 16,786 |
| `rollout` | 19,254 | 12,522 |
| `hooks` | 17,222 | 12,792 |
| `http-client` | 13,267 | 7,751 |
| `linux-sandbox` | 12,852 | 4,825 |
| `sandboxing` | 11,683 | 4,548 |
| `model-provider` | 11,660 | 7,836 |
| `shell-command` | 10,547 | 8,152 |
| 其余 54 个 crate | 100,000 左右 | 约 7 万 |

`state` 与 `thread-store` 合计 6.7 万行在第五行里已经减掉。如果保留断点续跑，这两个留下，最终规模多 6.7 万行，67 个 crate 变成 69 个。

42 万生产行对一个完整的 agent 运行时是合理量级：流式模型客户端、工具调度、沙箱、MCP、上下文压缩、事件流都在里面。

---

## 8 执行顺序

删除顺序由编译关系决定，不能按行数从大到小。

1. **立边界**。新建 `rsi-kernel` crate 并加入 workspace，依赖只写 `core`、`protocol`、`tools`、`config`、`model-provider`、`rmcp-client`。从这一步起，构建与验收都用 `cargo build -p rsi-kernel`，其余 crate 是否还能编译不再是门禁。
2. **关功能**。用 `features` 与配置把 Guardian、Code Mode、Plugins、Multi-agent、Realtime、Memories、Message Board 全部关掉，确认跑起来没有副作用。关闭比删除先做，因为关闭可以立刻回退。
3. **精简 `cli`**。删掉依赖已删 crate 的子命令，只留 TUI 与 `exec` 两个入口，保证 TUI 还能用来调试内核。
3. **删无引用**。第 4.6 与 4.8 的表格里，core 引用为零或个位数的先删：`chatgpt`、`aws-auth`、`keyring-store`、`workload-identity`、`agent-*`、`message-board*`、`runtime-agent-instance`、`guardian-v3`、`v8-poc`、`tcp-tunnel`、`stdio-to-uds`、`responses-api-proxy`、`thread-manager-sample`、`test-binary-support`、`windows-*`、`mxc-sandbox`、`bwrap`、`worktree`、`file-watcher`、`terminal-detection`、`arg0`、`install-context`、`mermaid`（等 TUI 处理）。
4. **剥模块**。按第 5 节处理 core 内部：先删 `guardian/`、`agent/`、`realtime_*`、`plugins/`、`apps/`、`connectors.rs`，再删 `tools/handlers/` 里的多智能体、插件安装、用户提问、权限申请。
5. **换实现**。`otel`、`analytics`、`feedback` 三个 crate 内部清空。`login` 与 `models-manager` 在阶段二换成最简实现。
6. **切 exec-server**。保留本地环境实现，删除 relay、remote、client 三组文件。
7. **删 TUI**。前六步稳定、冒烟脚本覆盖常用路径之后，连同 `cli`、`exec` 一起删除。

每一步的验收是同一条命令：`cargo build -p rsi-kernel`，加上一个冒烟会话，覆盖读文件、改文件、跑命令、调 MCP 工具、上下文压缩五条路径。

---

## 9 新增部分

裁剪之外要新增的内容，规模远小于删除。

| 新增 | 内容 | 规模 |
|---|---|---|
| `rsi-kernel` 二进制 | 读 job 描述、装配工具面、跑一次会话、写 JSONL 事件与结构化输出 | 约 3,000 行 |
| 事件模型 | 会话事件、工具调用、用量、错误，稳定字段 | 约 500 行 |
| 预算与权限执行 | 轮数、token、工具面、路径的硬约束，落在工具调度层 | 约 1,000 行 |
| MCP 服务端 | 框架侧提供证据查询与实验执行工具（在 Python 框架里，不在内核） | — |

内核本身不实现证据、统计、裁决、Git。这些都在 `rsi-core`。

---

## 10 未决

| 项 | 需要确认 |
|---|---|
| `state` 与 `thread-store` | 会话是否需要落库支持断点续跑。如果要，保留；不要，阶段二删 |
| `rollout` 与 `rollout-trace` | 事件流是自己写还是复用这两者。复用省事，自己写可控 |
| `ext/skills` | 内核是否需要技能机制。RSI 的阶段说明由框架注入，技能可能多余 |
| `models-manager` | 模型目录由配置提供后，这个 crate 的替代实现要提供哪些接口 |
| TUI 释放的顺序 | 先改 TUI 释放插件与代码模式，还是等 TUI 整体移除 |
