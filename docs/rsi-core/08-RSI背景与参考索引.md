# RSI 背景与参考索引

记录“为什么内核需要这些能力”。细节以原始文档为准，这里只做导航与约束提炼。

## 1. RSI 要做什么

RSI 关心的是：Agent 完成任务后留下成功路径、失败记录、工具观察和产物；系统怎样利用这些信息改进自己，并让后续执行与后续改进继续利用已经接受的变化。

三个层次要分开：

| 层次 | 发生什么变化 | 验收 |
|---|---|---|
| 任务内改进 | 当前计划/上下文/工具动作改变 | 这次任务是否完成 |
| 自演化 | 技能、规则、配置、代码被保留 | 新任务是否受益 |
| 最小递归 | 后续周期加载并继续优化已接受的状态 | 改进过程是否利用历史 |
| 递归进步 | 继承让改进过程本身变强 | 需要单独实验，第一期不声称 |

设计文档的目标是“自演化 + 最小递归”，不是“递归进步”。

## 2. 系统组成

设计文档 §3 的形态：

```text
RSI 容器
  rsi-core     编排 / 证据 / 评测 / 裁决 / 交付（设计文档里是 Python）
  rsi-kernel   裁剪后的 Codex，按阶段启动，跑一次会话（Rust）
  rsi-panel    只读面板
  rsi-ctl      初始化 / 自检 / 手动触发
  证据库        SQLite，可切 Mongo

业务系统
  rsi-adapter  适配器端点
  生产链路
  产物 / 审核 / 回流

外部
  GitLab / 模型网关 / IM
```

当前项目的调整：全 Rust；本仓库承担 `rsi-kernel` 的角色，后续 RSI 框架也是 Rust。

## 3. 三条边界

设计文档 §2：

1. **执行留在业务系统里**。RSI 不复制 target 的执行逻辑；只通过适配器要一次执行，拿产物、轨迹、成本、延迟。
2. **判定留在 RSI 系统里**。守卫、rubric、统计、裁决是 RSI 自己的代码，对 Agent 不可写；Agent 可以读结果、提改动，不能改判定规则与阈值。
3. **实验自由，交付受限**。容器里对业务仓库有完全写权限，不需要审批；生效唯一通道是 PR。框架在提交时检查允许路径、冻结段、约束。

对内核的直接含义：

- 不需要审批/提问/权限申请工具；没有人在环。
- 工作区可以完全开放，但“工作区外不可读写”仍要由沙箱/权限保证。
- 内核产出改动，不负责提交/PR（框架负责）。

## 4. 一个周期

设计文档 §7 的九个阶段（图中另含 `record`/`experience`/`notify`）：

```text
sync -> measure -> [系统故障占主导?]
  -> record（只出诊断报告）
  -> diagnose（内核读证据，产出假设）
  -> propose（内核在工作区写改动）
  -> validate（纯代码：路径/冻结段/约束/成本）
  -> experiment（候选 vs 基线，配对回放）
  -> adjudicate（纯代码：accept/reject/inconclusive）
  -> deliver（提交分支/PR）
  -> record / experience / notify
```

需要内核的只有 `diagnose` 与 `propose`：

- `diagnose`：宽视野，只读证据 + 用例抽样；输出带证据引用的假设。
- `propose`：窄约束，增加 `apply_patch`；输出改动 + 自我批评。

这个区分直接影响工具面：诊断阶段不应有写工具；提案阶段才有。

## 5. 证据、实验、裁决

- 证据来源：生产运行、审核结果、发布回流、case、judge。
- 实验：同一变体在冻结 case 集上跑若干次，基线同样；配对比较、bootstrap、A/A 噪声。
- 运行分类：正常、模型侧失败、基础设施失败、输入无效；后两类不计入分母。
- 裁决：`accept` / `reject` / `inconclusive`；`inconclusive` 是合法结论。
- 经验条目：`cue`、`intervention`、`evidence`、`limits`、`lifecycle`、`usage`；缺项不入库。
- 接受/拒绝/无结论都留痕，不静默丢弃。

对内核的含义：

- 事件流要能区分模型失败、工具失败、基础设施失败；错误要分类，不能只给一个字符串。
- usage、耗时、工具调用要可核算；预算要与实验记录对得上。
- 输出要能引用证据（case id、运行 id、指标），不能只有自然语言。

## 6. 参考材料怎么读

| 材料 | 路径 | 重点 |
|---|---|---|
| RSI 平台设计讨论 | [RSI平台设计讨论.md](/Users/hiro/ugc-platform/docs/rsi/RSI平台设计讨论.md) | §2 三条边界、§3 组成、§6 内核、§7 周期、§8 证据、§10 第一个 one-shot |
| RSI 调研与改进报告 | [RSI_调研与Agent系统改进报告.md](/Users/hiro/rsi/RSI_调研与Agent系统改进报告.md) | 八个部分的实现选择；§13 起步顺序 |
| RSI 研究资料与详细分析 | [RSI_研究资料与详细分析.md](/Users/hiro/rsi/RSI_研究资料与详细分析.md) | 论文证据、方法比较 |
| Agentic RL Infra 综述 | [Agentic_RL_Infra_主流技术路线综述.md](/Users/hiro/rsi/Agentic_RL_Infra_主流技术路线综述.md) | 训练/rollout/奖励/版本一致性；后续阶段 |
| codex 裁剪分析 | [codex-内核裁剪分析.md](/Users/hiro/codex/docs/codex-内核裁剪分析.md) | 逐 crate/模块的保留/删除/替换 |
| HarnessPro | `/Users/hiro/rsi/HarnessPro` | policy/hook、多目标保留、生成测试 |
| harness2 | `/Users/hiro/rsi/harness2` | runmark、递归模式、Codex substrate |
| ugc 业务侧方案 | `/Users/hiro/ugc-platform/docs/rsi/*.md` | 话题帖链路、小红书改造、工作项 |

建议阅读顺序：

1. [RSI平台设计讨论.md](/Users/hiro/ugc-platform/docs/rsi/RSI平台设计讨论.md) §1-§3、§6-§8。
2. [codex-内核裁剪分析.md](/Users/hiro/codex/docs/codex-内核裁剪分析.md) §1-§5。
3. [RSI_调研与Agent系统改进报告.md](/Users/hiro/rsi/RSI_调研与Agent系统改进报告.md) §1-§2、§13。
4. 需要具体实现选择时，再查报告 §3-§10 与 HarnessPro/harness2 代码。
5. Agentic RL Infra 综述留到训练/rollout 阶段。

## 7. 术语

| 术语 | 含义 |
|---|---|
| target | 被改进的业务链路 |
| harness | 模型周围的执行支撑：指令、工具、技能、上下文、运行控制 |
| case | 可重建的一次输入/场景 |
| 变体 | harness 的一组改动 |
| runmark / 指纹 | 变体哈希 + case 集哈希 + 模型版本 + 判据版本 + 温度 + 次数 |
| judge | 判据/rubric，对输出打分 |
| 证据 | 运行、产物、审核、回流、judge 结果 |
| 裁决 | 根据实验指标决定接受/拒绝/无结论 |
| 经验条目 | 可复用的改进知识，带 cue/limits/lifecycle |
