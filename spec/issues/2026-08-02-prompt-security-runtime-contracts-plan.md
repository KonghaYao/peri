# Prompt 安全边界与运行时契约收敛：实施计划

**状态**：Plan ready
**关联 PRD**：`2026-08-02-prompt-security-runtime-contracts.md`
**关联审计**：`docs/design/prompt-sections-audit.md`（以其中“对抗验证记录”的修正后判定为准）
**优先级**：P0 安全层与 transcript 信任边界优先
**计划范围**：仅规划，不包含实现代码变更
**创建日期**：2026-08-02

## 0. 计划目标与边界

本计划把 PRD 拆为可回滚、可验证的实现阶段。核心目标不是润色 `prompts/sections/`，而是让：

1. 安全与授权不变量不再能被 `prompt_mode: full` 删除；
2. prompt 的能力声明、工具注册与 deferred-tool 搜索共享同一运行时事实源；
3. 用户文本、仓库元数据和异步运行时事件有明确且可测试的可信度边界；
4. 会话与 subagent 继续遵守 frozen prompt 前缀契约，不通过每轮重渲染换取状态同步。

### 固定约束

- 以 `FrozenSessionData::build` 为会话冻结点，遵守 `ARC-FROZEN-001`：不得因 PermissionMode、skill 目录变化或 subagent 创建而每轮重渲染完整 system prompt。
- 以 ACP builder 为生产中间件顺序、条件注册和工具注册的唯一事实源，遵守 `ARC-MIDDLEWARE-001` 与 `ARC-TOOLS-001`。
- 本轮**不**把 subagent 改为逐工具 HITL 审批；目前“批准启动 = 授权内部继承工具”的模型只做准确披露和测试，行为变更另行决策。
- 当前 ChannelOwner 没有在生产路径装配。本计划只做防御性序列化与未来启用准入测试，不把它描述为现行生产攻击路径，也不启用 channel 功能。
- fork 生产路径已经继承父冻结 system prompt 和调用时完整历史快照；不重构该继承机制，只修正文档边界。
- Workflow 的不匹配实际暴露于 `-p` print 模式；stdio 与 ACP server 正常路径会创建 workflow executor，不得将它们回归为“无 workflow”的场景。

### 主要测试 seam

| Seam | 验证的外部契约 | 适用阶段 |
|---|---|---|
| `PromptTemplate::render` 最终输出 | 分层顺序、full/persona override、安全 section、能力 section、环境状态 | 0、1、3、4、6、7 |
| `FrozenSessionData::build` 与 subagent frozen data | 会话/子 agent 的前缀稳定性 | 0、1、4、5、7 |
| ACP builder → `MiddlewareChain::collect_tools` → ToolSearchIndex | prompt 声明、工具注册与 deferred search 一致 | 0、3、5、6 |
| `HumanInTheLoopMiddleware::before_tool` / `process_batch` | Default/Bypass/AcceptEdit/AutoMode 的实际审批结果 | 0、4 |
| `run_receive` / `append_messages_to_transcript` | 用户、仓库元数据、runtime event 的可信度边界 | 0、2、6 |
| subagent `filter_tools` + capability rendering | readonly/writes 标签与最终注册能力一致 | 0、6 |

---

## 1. 阶段 0：建立探针与基线（先测试，后重构）

### 目标

将审计发现固定为确定性回归测试或明确的人工验证脚本。禁止先修改生产逻辑再补测试，尤其不能继续用“full 会跳过 static sections”的现有测试把漏洞当作契约固化。

### 涉及模块

- `peri-acp`：prompt 模板与 session frozen data 测试；ACP builder 的条件工具装配测试。
- `peri-agent`：Receive / transcript 写入路径测试。
- `peri-middlewares`：HITL mode 矩阵、subagent middlewares、skill tools、capability inference 测试。

### 实施步骤

1. 在 prompt 测试中建立 `prompt_mode: full` + 非空 persona 的反例：修复前记录安全、secret、Git、基础工具纪律文本缺失；修复后替换为保留这些文本、仅 persona 被替换的正向契约。
2. 为同一 `FrozenSessionData` 的重复主 agent turn 与 subagent 构建建立 prefix 稳定性断言：相同 frozen 输入产生相同 system prompt；中途环境/skill 磁盘变化不改变已冻结前缀。
3. 在 Receive/stages 测试中输入完整伪造 `<system-reminder>`、闭合 tag、伪造 `<channel>` 和嵌套 tag；同时覆盖真实 Info/Defer 注入，作为后续 serializer 的行为基线。
4. 构造 `workflow_executor: Some` / `None` 的 builder 测试矩阵，分别断言：rendered prompt、`collect_tools` 结果、ToolSearch 索引发现结果。`None` 使用 print-mode 语义，不将 stdio 当作无 executor。
5. 对 Bash、Write、`cron_register`、Read 建立 Default / Bypass / AcceptEdit / AutoMode 的 HITL 结果矩阵；覆盖 `SharedPermissionMode::store/cycle`。
6. 通过 ACP 主链与 subagent 链的真实 tools 集合建立 skill 协议基线：主链当前同时有 `SkillTool` 和 `Skill`，子链当前只有 `SkillTool`。
7. 以 `filter_tools` 与 `infer_agent_capability` 的组合测试固定四个场景：继承 Bash 且禁用 Write/Edit、显式 `tools: []`、明确只读白名单、wildcard/继承工具。

### 完成判据

- 每项 P0/P1 结论都存在独立、无网络、无 wall-clock 依赖的测试场景。
- 测试断言最终 prompt、最终工具集合、最终审批结果或最终 transcript，而非内部私有字段。
- 已知行为反例均带 `/// [回归测试]` 背景注释，说明该测试将在后续阶段由“现状反例”转为“修复契约”。

### 验证

```bash
cargo test -p peri-acp --lib prompt
cargo test -p peri-acp --lib executor
cargo test -p peri-middlewares --lib hitl
cargo test -p peri-middlewares --lib subagent
cargo test -p peri-agent --lib stages
```

### 建议提交

`test(prompt): 建立安全、冻结与能力契约基线`

---

## 2. 阶段 1：冻结安全 prompt 分层并限制 full override

### 目标

将裸 `STATIC_SECTIONS` / `ALWAYS_DYNAMIC_SECTIONS` / `GATED_SECTIONS` 拼接重构为具名层，明确不可替换边界。`prompt_mode: full` 只能替换 persona/domain instructions，绝不删除安全与授权、稳定工程行为、能力契约或必要 runtime boundary。

### 涉及模块与符号

- `peri-acp/src/prompt/mod.rs`
  - `PromptTemplate`
  - `PromptFeatures`（在阶段 3 演进为 capability descriptor 的输入）
  - static / gated section 编排
  - `render`
- `peri-acp/src/session/executor.rs`
  - `FrozenSessionData::build`
- `peri-middlewares/src/subagent/tool/build_agent.rs`
  - agent definition 的 override 传入链

### 设计决定

将渲染逻辑按以下固定顺序表达，优先用轻量 enum/struct 而非新增泛化框架：

1. `SafetyAuthorization`：防御性安全、secret、破坏性 Git、不可绕过的授权说明；
2. `EngineeringBehavior`：任务执行、工具纪律、语气等稳定工程行为；
3. `CapabilityContract`：只声明当前实际具备的能力；
4. `RuntimeStateBoundary`：冻结环境快照和受控 runtime-event 语义说明；
5. `PersonaDomain`：agent persona、domain instruction、允许替换的 body。

`prompt_mode: full` 的兼容策略必须在实现前确定。推荐策略是保留字段名但改变其语义为“replace PersonaDomain”，随后在 parser 文档中标为受限覆盖；若引入新名称（如 `replace_persona`），必须提供明确迁移期，且不得保留能删除安全层的旧兼容路径。

### 实施步骤

1. 引入具名层与固定渲染顺序，保留现有 section 内容及 feature gate 的行为，避免同时改变所有文本。
2. 将现有 `full_body` 从“跳过 static sections”改为“仅替换默认 persona/domain 内容”。
3. 将 immutable safety/authorization 和工程行为的 section 置于 persona override 之前，并确保任何 override 分支都执行它们。
4. 保持 `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 的前缀位置确定；阶段 7 再改名/释义，不在本阶段引入 per-turn 重渲染。
5. 更新 agent definition 解析/描述与 prompt 测试，使 override 语义可见、可测试。

### 完成判据

- 任意非空 full/persona override 都保留 safety、secret、Git guardrails、基础工具纪律。
- persona/domain 文本仍能被预期替换，不退化为只能 append。
- 同一 frozen session 与 subagent 的 system prompt 前缀稳定；未破坏 `ARC-FROZEN-001`。
- 不通过重新排序生产 middleware 链实现该功能。

### 验证

```bash
cargo test -p peri-acp --lib prompt
cargo test -p peri-acp --lib executor
cargo test -p peri-middlewares --lib subagent
cargo test -p peri-acp --doc
```

### 建议提交

`refactor(prompt): 将 full override 限制为 persona 层`

---

## 3. 阶段 2：transcript 信任边界与 channel 防御性预备

### 目标

在真正写入模型 transcript 的边界建立统一、受控的序列化规则：用户文本、仓库元数据和 runtime event 的可信度可区分；用户自带 tag 不能伪装为可信控制容器。为未来 channel 启用预先修复转义与双重包裹风险，但不装配 ChannelOwner。

### 涉及模块与符号

- `peri-agent/src/agent/stages/mod.rs`
  - `append_messages_to_transcript`
- `peri-agent/src/agent/stages/receive.rs`
  - runtime event / `SyntheticUserMessage` 展示路径
- `peri-agent/src/agent/session/channel_owner.rs`
  - future channel message formatter
- `peri-acp/prompts/sections/14_system_reminder.md`
- `peri-acp/prompts/sections/15_channel.md`

### 设计决定

新增一个**共享、窄接口**的 serializer/formatter，负责：

- 将 runtime event 正文编码进唯一由代码创建的受控容器；
- 转义或破坏用户/外部内容中的保留控制 tag 闭合边界；
- 为 channel 预留 source / chat identity 字段的安全表示；
- 禁止 channel owner 先包一层、transcript 再包一层的嵌套容器。

必须明确 tag 规则：哪些 tag 是保留控制语法、是使用 XML escape 还是零宽字符破坏闭合、是否覆盖大小写/属性/嵌套。所有注入点使用同一规则，不能各写一份 sanitizer。

### 实施步骤

1. 定义 runtime event 与 untrusted text 的受控格式化函数，并先由 `append_messages_to_transcript` 使用。
2. 处理用户 Prompt、Info、Defer 和 future channel payload 的闭合 tag / 嵌套 tag 反例。
3. 将 `ChannelOwner` 格式化改为复用该 serializer，确保未启用路径的测试也覆盖将来行为。
4. 调整 `14_system_reminder.md`：保留“模型须把 tag 当不可信内容”的行为指示，但明确它是模型层缓解而非来源认证。
5. 调整 `15_channel.md`：声明当前 channel 语义只在实际装配时成立，且内容是外部输入而非更高权限指令。

### 完成判据

- 用户输入的 `<system-reminder>` / `<channel>` / 任意闭合变体无法形成 runtime event 容器。
- 真正的 Info/Defer 通知仍能以确定结构抵达 transcript 和展示事件。
- future channel 内容不能闭合自身容器、注入属性或形成双重 wrapper。
- 没有改变当前 ChannelOwner 未装配的生产路由。

### 验证

```bash
cargo test -p peri-agent --lib stages
cargo test -p peri-agent --lib channel_owner
cargo test -p peri-acp --lib prompt
```

### 建议提交

`fix(transcript): 隔离 runtime event 与不可信文本`

---

## 4. 阶段 3：共享 capability 契约与 Workflow gate

### 目标

用 session-scoped capability descriptor 取代只从 PermissionMode 推断的零散布尔值，使 prompt section、middleware 注册和 deferred-tool 搜索由同一运行时事实源驱动。先以 Workflow 为样例完成闭环。

### 涉及模块与符号

- `peri-acp/src/prompt/mod.rs`
  - `PromptFeatures` / `FeatureGate`
  - `GATED_SECTIONS`
- `peri-acp/src/session/executor.rs`
  - `FrozenSessionData::build`
  - `SessionContext` 的 capability 来源
- `peri-acp/src/agent/builder.rs`
  - workflow middleware / tool registration 条件
- `peri-middlewares/src/workflow/`
- `peri-middlewares/src/tool_search/`
- `peri-acp/prompts/sections/16_workflow.md`

### 设计决定

能力 descriptor 的生命周期为**session 创建时冻结的 capability snapshot**。它不是每轮重扫插件、MCP 或 skill 的动态目录。它至少明确：

- Workflow executor 是否存在；
- HITL 机制是否装配；
- SubAgent、Skills、Channel 是否真正可用；
- 必要时另分 `prompt visibility` 与 `tool visibility`，但两者从同一 descriptor 生成。

`workflow_executor.is_some()` 必须同时控制：workflow middleware adaptor、WorkflowTool 的 `collect_tools`、ToolSearch 索引可发现性和 `16_workflow.md` 渲染。对于 print mode 的 None，四处都关闭。

### 实施步骤

1. 设计最小 capability descriptor，避免把所有 session config 复制为另一个巨型 struct。
2. 在 session 创建/agent 构建边界生成 descriptor，并传入 prompt template 与 builder 条件注册。
3. 将 Workflow 从无条件 static section 改为 capability-gated section。
4. 为 `SearchExtraTools` / shared tool map 建立与 descriptor 一致的注册/发现测试。
5. 评估并修正当前 `channel_enabled: true` 一类非装配事实；未装配 channel 不应被 prompt 宣称为可用能力。

### 完成判据

- `workflow_executor: Some`：prompt 声明、WorkflowTool 注册和搜索发现均存在。
- `workflow_executor: None`：三者均不存在，尤其是 `-p` print 模式。
- stdio/ACP 默认路径仍保持 workflow executor 可用。
- 新 descriptor 不导致会话中途重建 frozen system prompt。

### 验证

```bash
cargo test -p peri-acp --lib prompt
cargo test -p peri-acp --lib executor
cargo test -p peri-middlewares --lib workflow
cargo test -p peri-middlewares --lib core_tools
```

### 建议提交

`refactor(capabilities): 统一 workflow 注册与 prompt gate`

---

## 5. 阶段 4：HITL 机制说明与 PermissionMode 状态同步

### 目标

让 `10_hitl.md` 反映真实机制，而非把固定工具列为所有模式下“always require approval”；解决 mode 会话内切换后模型说明陈旧的问题，同时不破坏 frozen prefix，也不改变 subagent 的现有授权模型。

### 涉及模块与符号

- `peri-middlewares/src/hitl/mod.rs`
  - `default_requires_approval`
  - `HumanInTheLoopMiddleware::decide_by_mode`
- `peri-middlewares/src/hitl/shared_mode.rs`
- mode 切换入口（TUI / ACP session config）
- `peri-acp/src/session/executor.rs` / session inbox 运行时事件路径
- `peri-acp/prompts/sections/10_hitl.md`
- `peri-acp/prompts/sections/11_subagent.md`
- `peri-middlewares/src/subagent/tool/descriptions/agent.md`

### 设计决定

- 冻结 prompt 仅呈现**初始 mode / 审批机制**，不承诺“该清单永远审批”。
- mode 切换后不重渲染 system prompt；通过阶段 2 的受控 runtime event 注入简短、可验证的状态变更通知。
- 初期只披露 subagent 边界：批准启动会授予内部继承工具的执行权，子 agent 不经逐工具 HITL，且无法递归启动 Agent。是否向子 agent 安装 SharedPermissionMode/HITL 是独立产品决策。

### 实施步骤

1. 将 `10_hitl.md` 改为：敏感工具判定由运行时、最终决策由 PermissionMode、模式可变、当前 mode 为会话状态；补齐 `cron_register`。
2. 定义 mode-change runtime event 的发送时机与格式；优先下一可消费 turn 语义，避免跨正在执行的 batch 改变已拍快照的批处理结果。
3. 从 mode 切换入口把状态变化送入受控 runtime event 路径。
4. 更新 11_subagent 与 Agent tool description，准确披露单层授权传递和无逐工具 HITL。
5. 明确 AutoMode 的模型分类输入不是 prompt 中的工具清单，避免继续写出错误因果关系。

### 完成判据

- Default / Bypass / AcceptEdit / AutoMode 的实际决策与模型可见说明不矛盾。
- mode 切换后的状态更新可被下一可执行 turn 观察，并有稳定测试。
- 不改变 `process_batch` 中的 mode snapshot 语义。
- 不改变子 agent 是否装配 HITL 的现有行为。

### 验证

```bash
cargo test -p peri-middlewares --lib hitl
cargo test -p peri-acp --lib executor
cargo test -p peri-agent --lib stages
cargo test -p peri-middlewares --lib subagent
```

### 建议提交

`fix(hitl): 让 prompt 说明与权限模式一致`

---

## 6. 阶段 5：Skill 单协议与冻结 catalog 语义

### 目标

收敛主 agent 目前并存的 Skill 协议，统一 prompt、主 agent、subagent 和 workflow 调用面的加载语义；保留 frozen skill summary 的缓存稳定性，但让文案和失败行为准确描述冻结 catalog 与实时扫描之间的边界。

### 涉及模块与符号

- `peri-middlewares/src/skills/tools.rs`
  - `SkillTool(skill_name)` 与 `DiscoverSkillsTool`
- `peri-middlewares/src/tools/skill.rs`
  - `Skill(skill, args)` 与 `SkillToolMiddleware`
- `peri-middlewares/src/skills/mod.rs`
  - `build_frozen_summary`
  - `SkillsMiddleware`
- `peri-acp/src/agent/builder.rs`
- `peri-acp/src/agent/workflow_agent.rs`
- `peri-acp/prompts/sections/13_skills.md`

### 设计决定

推荐规范接口为 `SkillTool(skill_name)` + `DiscoverSkillsTool`：已在 subagent 链自然使用，且其语义是按名加载完整 `SKILL.md`。`Skill(skill, args)` 必须在实现前选择以下一种迁移模式：

1. 立即删除并更新所有调用方；
2. 保留实现作为短期兼容层，但不注册到模型可见 tools；
3. 明确版本化弃用期及移除条件。

无论选择哪一种，主 agent 与 subagent 对模型公开的协议必须一致。Builtin 是正式的编译期 skill 来源，文案必须列入发现语义。

### 实施步骤

1. 全仓定位 `Skill` / `SkillTool` 的构造、注册与调用点，特别检查 workflow agent。
2. 确定规范协议和兼容策略，并先更新工具注册层，再更新 prompt 文案。
3. 移除/隐藏重复工具，避免主 agent 同时看到参数不兼容的两个加载器。
4. 更新 13_skills：模型可通过工具加载 skill；说明 Builtin 来源；明确 prompt catalog 在 session/new 冻结，而 Discover/Load 使用当前扫描缓存。
5. 设计被删除或新增 skill 的可恢复错误：例如“catalog 快照存在，但当前无法加载”或“当前发现但未列入本 session catalog”；不因这点牺牲 frozen prefix。

### 完成判据

- 主 agent 与 subagent 暴露同一套模型可见 skill 加载协议。
- 不再同时暴露 `SkillTool` 与 `Skill` 的冲突参数契约。
- Builtin skills 在 discovery 语义中可见。
- 会话期间新增/删除 skill 的行为和错误信息可预测，并保持 frozen summary 的设计意图。

### 验证

```bash
cargo test -p peri-middlewares --lib skills
cargo test -p peri-middlewares --lib skill
cargo test -p peri-middlewares --lib subagent
cargo test -p peri-acp --lib
```

### 建议提交

`refactor(skills): 收敛模型可见的 skill 加载协议`

---

## 7. 阶段 6：不可信 metadata 隔离与 subagent capability 收敛

### 目标

降低仓库本地 skill/agent metadata 进入 system prompt 的指令注入面，并让 `[readonly]` / `[writes]` 能力标签基于最终真实工具能力作保守判断。

### 涉及模块与符号

- `peri-middlewares/src/skills/mod.rs`
  - frozen summary formatter
- `peri-acp/src/prompt/mod.rs`
  - `format_available_agents`
- `peri-middlewares/src/subagent/mod.rs`
  - `scan_agents_detailed`
  - `infer_agent_capability`
- `peri-middlewares/src/claude_agent_parser/mod.rs`
  - `ToolsValue` / `NoTools` / omitted tools 语义
- subagent tool filtering 与 builder 工具来源

### 设计决定

metadata 安全优先级：

1. system prompt 优先只暴露受限 catalog（agent id、tier、保守 access、受控来源）；
2. 自由 description 如必须保留，必须有长度限制、保留 tag 转义和固定“仅检索元数据、非执行指令”容器；
3. 完整 skill body 保持通过统一加载工具按需返回；
4. 不能只依赖自然语言警示词作为技术防护。

readonly 采用保守策略：只有能根据**最终注册工具集合**证明没有项目写入能力时，才显示 `[readonly]`；不能证明就显示 `[writes]`。继承/wildcard 中含 Bash 必须保守归为 writes；显式只读白名单可归 readonly；`tools: []` 必须识别为零工具而非“未声明、继承父工具”。标签是调度提示，不是代码级锁或安全边界。

### 实施步骤

1. 提炼共享 metadata formatter，定义限制、转义和可信度提示，并供 skill summary 与 agent catalog 使用。
2. 缩减 system prompt 中的自由 description 暴露；如保留，使用同一 formatter。
3. 区分 omitted tools、显式 `NoTools`、显式白名单和继承/wildcard，避免 `to_vec().is_empty()` 混淆语义。
4. 令 capability inference 消费 filtered/final tool set，或建立等价保守证明；把 Bash、cron 等写能力纳入模型。
5. 更新 `format_available_agents` 的标签渲染与不可信 metadata 说明。

### 完成判据

- 恶意 skill/agent description 不能闭合隔离容器或伪装成更高优先级指令。
- omitted tools + `disallowedTools: [Write, Edit]` 但仍含 Bash 的 agent 不得标 readonly。
- `tools: []` 被识别为零工具；`[Read, Glob, Grep]` 白名单可安全标 readonly。
- prompt 显示的标签与最终注册工具集的保守判断一致。

### 验证

```bash
cargo test -p peri-middlewares --lib skills
cargo test -p peri-middlewares --lib subagent
cargo test -p peri-acp --lib prompt
cargo test -p peri-acp --lib
```

### 建议提交

`fix(metadata): 隔离 catalog 并保守推断 subagent 写能力`

---

## 8. 阶段 7：P2 语义收尾、fork 文档与全量验证

### 目标

完成不改变核心授权模型的语义修正：Git 上溯探测、uncached 命名、skill snapshot 说明、fork 继承文档边界；随后执行跨 crate 验证与文档路由检查。

### 涉及模块与符号

- `peri-acp/src/prompt/mod.rs`
  - `PromptEnv::detect`
  - `PromptEnv::with_frozen_date`
  - `ALWAYS_DYNAMIC_SECTIONS` 命名/注释
- `peri-acp/prompts/sections/07_env.md`
- `peri-acp/prompts/sections/13_skills.md`
- `peri-middlewares/src/subagent/tool/descriptions/agent.md`
- fork 相关测试（仅回归，不改继承实现）

### 实施步骤

1. 用向上查找的 repository root 语义替换 `cwd/.git.exists()`；覆盖 `.git` 目录、`.git` 文件/worktree、仓库子目录、非仓库目录。
2. 将 `ALWAYS_DYNAMIC_SECTIONS` 改名或说明为“非前缀缓存区”，明确不等于每 turn 重建；保持 07_env 已有“会话开始捕获、必要时用 Bash 验证”的语义。
3. 文档化 skill summary 的有意冻结策略与会话内新增/删除 skill 的行为。
4. 修订 fork 工具描述：继承父 frozen system prompt、调用时完整历史快照与 parent core tools；明确不包含 Agent、Cron、Workflow、LSP、Plugin 等扩展工具，且父 `agent_overrides` 不进入 fork prompt。
5. 审查 docs/standards、模块 CLAUDE 与 active spec 的事实源，按 `DOC-UPDATE-001` 只更新受影响的单一来源。

### 完成判据

- Git 环境提示在子目录/worktree 语义上与 Git 命令一致。
- “dynamic”不再暗示 per-turn 状态刷新。
- fork 文档不再声称错误继承边界，也不触发对正确实现的无谓重构。
- 文档与代码事实源一致，未复制事故叙事进入根 CLAUDE。

### 验证

```bash
cargo test -p peri-acp --lib prompt
cargo test -p peri-middlewares --lib subagent
cargo test -p peri-acp --doc
cargo test -p peri-middlewares --doc
git diff --check
```

### 建议提交

`fix(prompt): 收敛环境、缓存与 fork 语义`

---

## 9. 最终验收与合并前检查

### 分层验收矩阵

| 契约 | 必须证明的行为 |
|---|---|
| 安全层不可替换 | full/persona override 后仍含安全、secret、Git、工具纪律规则 |
| Frozen 前缀 | 同一 session/subagent 不因 mode、磁盘 skill 或 turn 改变而重建 prompt 前缀 |
| Runtime event 可信度 | 用户/外部 tag 不能伪装 runtime event；真实 event 保持结构可辨 |
| 工具可见性 | capability 开/关时，section、collect_tools、deferred search 三者一致 |
| HITL | 四 mode 的实际结果、初始说明和 mode-change runtime notice 不冲突 |
| Subagent 授权 | 仍无逐工具 HITL、不递归 Agent；prompt/工具说明准确披露 |
| Skill | 只暴露一个模型可见协议；Builtin 语义完整；catalog 冻结边界可预测 |
| Metadata | description 不可逃逸隔离、不可充当上层指令；agent 标签由真实工具集保守导出 |
| P2 | Git 上溯正确；uncached 语义清晰；fork 文档匹配生产继承链 |

### 命令序列

先在每一阶段运行针对性测试；所有阶段完成后执行：

```bash
cargo test -p peri-agent --lib
cargo test -p peri-middlewares --lib
cargo test -p peri-acp --lib
cargo test -p peri-acp --doc
cargo test -p peri-middlewares --doc
cargo test --workspace --doc
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

若修改工具注册、搜索或工具序列化，额外运行 canonical tool invocation contract 相关集成测试；若修改 mode-change event 的 ACP/TUI 映射，则按 `ARC-EVENT-001` 人工检查完整发射、映射、caps 门控（如适用）与 TUI 消费链。

### 代码审查重点

1. 是否存在任何 `full`、override 或 fallback 路径仍能跳过 immutable safety 层。
2. capability descriptor 是否真正是注册与 prompt 共用的事实源，而非又一份文案布尔副本。
3. 新 serializer 是否被所有 transcript 注入点使用，且不会把原始用户内容记作 runtime event。
4. 任何 metadata / tag 转义是否有统一实现、长度上限和测试，而非不同模块各自 hand-roll。
5. 是否无意改变生产 middleware 顺序、subagent HITL 授权模型、frozen prompt cache 或 stdio/ACP 的 Workflow 行为。
6. 是否存在 secret 被写入测试 fixture、错误、日志或 telemetry。

---

## 10. 未决产品决策

这些决策不应在实现过程中静默选择；阶段 0 完成后、阶段 1/4/5 开始前应明确记录结论。

### D1：`prompt_mode: full` 的兼容迁移

**推荐**：保留字段名但将语义限制为 replace PersonaDomain，记录兼容性变化；绝不保留可删除安全层的 legacy 分支。

需确认：是否引入新名称（如 `replace_persona`）并为 full 提供弃用窗口；未知 mode 是报错还是回退 extend。

### D2：PermissionMode 切换的模型同步时间

**推荐**：通过受控 runtime event 在下一可消费 turn 生效；不重渲染 frozen system prompt，不改变正在执行 batch 的 mode snapshot。

需确认：切换发生时是否向模型无条件通知、Bypass 是否仍保留机制说明、用户是否可关闭此通知。

### D3：skill 规范接口与兼容期

**推荐**：规范接口为 `SkillTool(skill_name)` + `DiscoverSkillsTool`；`Skill(skill,args)` 从模型可见注册表移除。

需确认：立即删除、隐藏兼容实现，还是设置版本化弃用期；workflow agent 的兼容责任归属。

### D4：metadata 的最小暴露程度

**推荐**：system prompt 默认仅 exposure ID/tier/conservative access；description 只在必须支持自动选择时以受限、转义、非指令 catalog 形式展示。

需确认：是否接受 agent 选择质量可能下降；是否需要新增按需查询 agent metadata 的工具（该选择会扩大本 issue 范围）。

### D5：readonly 标签语义

**推荐**：定义为“在最终注册工具集合下，没有可证明的项目写入能力”，采用保守 false-negative（宁可 `[writes]`）。不将其视为并发锁、安全边界或外部副作用保证。

需确认：Bash 是否一律视为 writes；MCP/cron/网络副作用是否需要另设标签而非复用 readonly。

### D6：保留控制 tag 的编码规则

**推荐**：定义统一 serializer，以结构化转义和单层 runtime container 实现；明确所有保留 tag、闭合 tag、属性、嵌套和大小写行为。

需确认：选择 XML escaping 还是零宽字符破坏闭合；历史 transcript / provider 兼容性要求。

---

## 11. 风险与控制

| 风险 | 影响 | 控制措施 |
|---|---|---|
| 分层重构意外破坏 Anthropic prefix cache | 性能/成本回归 | 阶段 0 先冻结 prefix 测试；仅在 session/new 建 descriptor；禁止 per-turn 完整 render |
| full 语义变化影响现有 agent definition | 行为兼容性 | D1 显式决策；添加 migration 文档和 fixture 覆盖；安全层不可回退 |
| capability descriptor 成为新的平行事实源 | 继续漂移 | descriptor 从 builder/session 注册条件导出；每项能力三面一致性测试 |
| tag sanitizer 各注入点不一致 | 新的注入绕过 | 单一 serializer；用户、Defer、Info、channel formatter 共用；对抗测试覆盖 |
| Skill 协议迁移遗漏 workflow 或子 agent 调用方 | 工具不可用 | 全仓调用点盘点；主链/子链最终 tools 集合测试；必要时先隐藏再删除 |
| 改造 HITL 时不慎改变授权行为 | 用户体验/安全语义突变 | 阶段 4 仅文案+状态同步；断言子 agent 链保持无 HITL；任何传递 HITL 的改动另开 issue |
| readonly 误被理解为强隔离 | 并发或副作用风险 | 文档明确为模型调度提示；以最终工具集保守推断；不引入代码级错误保证 |
| channel 防御修复被误解为启用 channel | 范围膨胀 | 保持 `ChannelOwner` 未装配；只增加 formatter 测试作为未来准入 |

---

## 12. 建议执行顺序与预计提交

1. `test(prompt): 建立安全、冻结与能力契约基线`
2. `refactor(prompt): 将 full override 限制为 persona 层`
3. `fix(transcript): 隔离 runtime event 与不可信文本`
4. `refactor(capabilities): 统一 workflow 注册与 prompt gate`
5. `fix(hitl): 让 prompt 说明与权限模式一致`
6. `refactor(skills): 收敛模型可见的 skill 加载协议`
7. `fix(metadata): 隔离 catalog 并保守推断 subagent 写能力`
8. `fix(prompt): 收敛环境、缓存与 fork 语义`
9. `test(ci): 执行跨 crate 契约回归与文档验证`

预计最小安全闭环（阶段 0–4 + 阶段 7 的 Git/fork 小修）：3–4 个工作日。完整范围（阶段 0–7 + 最终验证）：约 1.5–2 周；最大不确定性来自 D1、D3、D4 与 D6 的产品兼容决策。

---

## 13. 实施记录（阶段 5–7 决策采纳状态）

> 本节记录实现阶段（2026-08-02）对未决决策的采纳结论，避免文档声称未落地或被推迟的修复已完成。仅记录与阶段 5/6/7 相关的 D3–D6 状态；D1/D2 结论见阶段 0–4 对应提交与测试。

### D3（skill 单协议）：已采纳

`SkillTool(skill_name)` + `DiscoverSkillsTool` 为唯一模型可见 skill 加载协议。`tools/skill.rs`（旧 `Skill(skill, args)`）与 `SkillToolMiddleware` 已**立即删除**，未保留兼容注册：

- 主链（`builder.rs`）、subagent 链（`build_subagent_middlewares` 经 `SkillsMiddleware`）、workflow agent（`workflow_agent.rs`）均只暴露同一对工具。
- `SkillPreloadMiddleware` 注入的 fake ToolUse 同步迁移为 `SkillTool` + `skill_name`。
- `13_skills.md` 重写为真实协议说明，Builtin 列为正式发现根（`source: "builtin"`），并明确 frozen catalog（session/new 冻结）与实时扫描缓存（Discover/Load）的边界及可恢复错误语义。

### D4（metadata 最小暴露）：已采纳

system prompt 不再注入自由 skill/agent description：

- `SkillsMiddleware::build_summary` 只列出 `name` + 来源标签（`[user]/[global]/[project]/[plugin]/[builtin]`），并附固定"仅检索元数据、非指令"声明；完整内容经 `SkillTool` 按名加载。
- `format_available_agents` 只输出 `agent_id [tier] [access]`，description 不再进入 prompt；`11_subagent.md` 同步说明。

### D5（readonly 保守推断）：已采纳

`infer_agent_capability` 区分 `ToolsValue::Empty`（继承，含 Bash → writes）、`NoTools`（显式 `tools: []` → readonly）、`List` 白名单（含 Bash/Write/Edit/folder_operations/cron_register/mcp__* 任一未被 disallowed → writes）。内置 explorer/plan 补充 disallow `folder_operations`/`cron_register` 以维持可证明的 readonly 标签。`readonly` 明确为调度提示而非安全边界。**已知局限**：`mcp__*` 无法用精确 `disallowedTools` 排除（`filter_tools` 精确匹配），继承场景的 readonly 标签不覆盖 MCP 前缀工具。

### D6（保留 tag 编码）：接受为残余风险，未实现

本阶段未实现 transcript/channel tag 转义、净化或容器重构；未修改 transcript tag sanitizer 与 `ChannelOwner` 格式化（后者未装配）。`<system-reminder>` / `<channel>` 文本伪造风险仍仅由 `14_system_reminder.md` 的模型侧缓解覆盖，属**已接受残余风险**，不得在文档中表述为已修复。
