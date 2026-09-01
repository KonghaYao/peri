# MetaHarness 波 4 C3——gated 段拆分 + gate 原子迁移实施决策记录

**状态**：Closed（C3 已实施）
**优先级**：高（波 4 演进批次）
**类型**：决策记录
**创建日期**：2026-08-14
**来源**：MetaHarness gated section 持有迁移；当前稳定设计见 `docs/design/meta-harness.md` §2.4/§2.5，过程由本归档 issue 保留
+ §3.1.2（重复段处理）+ §3.5（演进 2）+ §3.5.1（11_subagent 拆分六步）；
批 C3 任务书；C2 决策记录遗留问题 1/2

## 背景

C2 已迁移基础段（01-06 / 07_runtime / persona → DefaultSystemPromptMiddleware，
language → LangMiddleware），gated 段（10_hitl / 11_subagent / 13_skills /
15_channel）仍由 `prompt/mod.rs` 的 `GATED_SECTIONS` 编译期数组持有，gate 由
`PromptFeatures::detect` 硬编码（语义边界 ②）。本批执行 gated 段落实体迁移
+ gate 原子迁移（契约 3）：10_hitl → HumanInTheLoopMiddleware（sensitive
列表改代码事实生成）、11_subagent → SubAgentMiddleware（3.5.1 六步）、
13_skills → SkillsMiddleware（协议细节动态生成）；15_channel 无持有者，
gate 保持恒 false。盲区闭合：关闭持有 middleware → 段落与工具同时消失。

## 实施决策

### D1：10_hitl 拆分（sensitive 列表 → 代码事实生成）

- `sections/10_hitl.md` 重写：删除 sensitive 工具列表行；保留标题 + 机制
  说明（PermissionMode decision / Approval decisions）+ 末尾 `## Which
  tools are sensitive` 小节引导句（以冒号结尾，动态列表后缀拼接）。
- `HumanInTheLoopMiddleware::sections()`（关联函数，渲染面与链收集的单一
  事实源，模式同 C2 D2）返回 Dynamic 段：`id=10_hitl`、Uncached、order=3，
  内容 = 文件机制说明 + `\n\n` + 动态 sensitive 列表 + 尾句
  （"Whether a sensitive tool actually requires approval is decided by the
  current `PermissionMode`..."）。
- 列表生成 `sensitive_tool_entries()`：结构化条目（名称 / 说明 / 前缀匹配
  标记），与 `default_requires_approval`（hitl/mod.rs:38-50）判定一一对应；
  渲染格式：精确匹配 `` `{name}` ``、前缀匹配 `` `{name}*` ``（delete_/rm_
  从现状合一行拆为两行，更结构化；无逐字快照依赖）。
- 一致性测试锁定：每条目（含前缀探测名）在 `default_requires_approval`
  判定为 true；代表性非敏感工具（Read/Glob/Grep/TodoWrite/AskUserQuestion）
  判定为 false。

### D2：11_subagent 拆分（3.5.1 六步）

1. **段落内容重构**：`sections/11_subagent.md` 删除 Agent Selection Guide
   具体映射（coder/explorer/plan/web-researcher/general-purpose 等仓库级
   调度建议）与 Standard pipelines / Parallelization 明细，浓缩为通用选择
   原则 2-3 句（specialized 优先 / general-purpose 兜底 / 按 access 标签
   并行化），不绑定 agent 名；其余小节（委托声明 / Available agent types /
   Authorization boundary / When to use / Writing / Fork / Usage / Background）
   保留。
2. **占位符机制不变**：`{{available_agents}}` 替换留在渲染层
   （`format_available_agents`，prompt/mod.rs），SubAgentMiddleware 持有
   含占位符的 Builtin 段落文本（middleware 仅作内容载体，语义边界 ①）。
   **catalog 同源收敛**：`scan_agents` / `scan_agents_with_extra_dirs`
   收敛为委托 `scan_agents_detailed`（共享实现，`subagent/mod.rs`）——
   render 的 catalog（`SkillsPort::agents` → `scan_agents_detailed`）与
   子链扫描同一实现，防提示词 catalog 与实际可用 agent 不一致。边缘行为
   变化：parse 失败的 agent 文件现在占用 agent_id（detailed 语义：seen_ids
   在 parse 前占用），不再允许 built-in 同名兜底——已由一致性测试锁定。
3. **段落持有迁移**：`SubAgentMiddleware::sections()`（关联函数）返回
   Builtin 段：`id=11_subagent`、Uncached、order=4、
   `include_str!(sections/11_subagent.md)`（文件留在 sections/，内容不复制）。
4. **gate 原子迁移**：`FeatureGate::Subagent` / `PromptFeatures::subagent_enabled`
   删除；11_subagent 收集即渲染（gate=Always，契约 3）。**子链语义保持**：
   SubAgent system_builder（`host/stage_builder.rs`）经 `build_collected_sections`
   （冻结 disabled 集合驱动）收集——主链不关 SubAgentMiddleware 则子 agent
   提示词保留 11_subagent 段落（现状行为）；子链独立装配的关闭过滤不变
   （设计 §2.5，与 C2 相同）。关闭 SubAgentMiddleware → 11_subagent 段落 +
   SubAgentTool/AgentResultTool 同时消失（盲区闭合）。
5. **覆盖语义兼容**：`PromptTemplate::new` 构造期按 ID 替换持有者段落，
   机制与持有者无关（设计 §2.4）——`.peri/meta/11_subagent.md` 覆盖自动
   生效，测试锁定。
6. **测试**：渲染输出无具体 agent 名；catalog 同源一致性
   （`scan_agents` == `scan_agents_detailed` 投影）；assembly 关闭场景
   （段落 + 工具同时消失）；覆盖测试。

### D3：13_skills 拆分（协议细节动态生成）

- `sections/13_skills.md` 重写：删除 discovery roots 列表与扫描参数细节，
  保留机制说明（loading / catalog / using / suggesting）；`## Skill
  discovery` 小节移至文件末尾并只留引导句（冒号结尾，动态内容后缀拼接）。
- `SkillsMiddleware::sections()`（关联函数）返回 Dynamic 段：
  `id=13_skills`、Uncached、order=5，内容 = 文件机制说明 + `\n\n` +
  `format_discovery_protocol()`。
- `format_discovery_protocol()`（skills/mod.rs）从**代码事实**生成：
  roots 优先级（User → Global → Project → Plugin → Builtin，与
  `resolve_skill_roots` 顺序一致）+ 扫描深度（`MAX_SCAN_DEPTH`）+ 目录
  上限（`MAX_SKILLS_DIRS_PER_ROOT`）+ 叶子/SKILL.md 语义 + symlink 防环。
  一致性测试：输出包含常量格式化值（防手写硬编码漂移）。
- 「按实际装配」落地边界：roots 优先级顺序与扫描参数由 loader 常量/代码
  顺序承担（渲染面与链收集同源，无新装配参数注入）；builtin 恒列出（与
  现状段落一致，不随 disableBundledSkills 变化——该配置影响扫描结果而非
  协议描述）。实例级路径配置（with_global_dir 等）不进入段落。

### D4：PromptTemplate / PromptFeatures 重构（gate 原子迁移，契约 3）

- `GATED_SECTIONS` 仅剩 15_channel（10/11/13 迁移完成）；元素形态改为
  四元组 (ID, 内容, Gate, order)，15_channel order=6 显式化（C1 D2 编号
  事实：10=3、11=4、13=5、15=6，迁移后编号不重排，language=7 不变）。
- `FeatureGate` 仅剩 Channel；`PromptFeatures` 仅剩 `channel_enabled`
  （恒 false——15_channel 无持有者，gate 恒 false 直至未来 channel
  middleware）；`detect()` 改无参（permission_mode 不再参与任何 gate
  判定）；`none()` 保留（测试 helper，与 detect 语义等价）。
- `build_collected_sections` 增加三个收集分支：`HumanInTheLoopMiddleware` /
  `SubAgentMiddleware` / `SkillsMiddleware` 不在 `disabled_middlewares` 时
  收集对应段（冻结状态驱动，链未装配构造点同源一致）。
- render 不变：收集段 gate=Always 恒渲染；15_channel 按 Feature(Channel)
  判定。

### D5：Bypass / workflow 渲染行为变化（gate 简化语义的必然结果）

- **10_hitl 在 Bypass 模式下从「不渲染」变「渲染」**：现状 gate 由
  permission_mode 决定（hitl_enabled = mode ≠ Bypass）；C3 后 gate =
  HumanInTheLoopMiddleware 是否在链上，而 Bypass 模式下 Hitl middleware
  仍在链（assembly 只按 disabled 集合过滤，与 mode 无关）→ 10_hitl 渲染。
  段落内容含 Bypass 模式说明（"Bypass: all tool calls are allowed without
  approval"），渲染无安全影响。
- **workflow fallback / workflow agent builder 渲染 10_hitl**：C2 已决定
  workflow 渲染与主链共用同一段落来源（`build_collected_sections`），C3
  延续——workflow 提示词包含 10_hitl 机制说明（描述主会话审批机制）。
- **演进 1（C4 删 permission_mode_notice）交互确认**：10_hitl 机制说明
  含 "the model is informed via a controlled runtime notification on the
  next consumable turn; do not assume the mode you saw earlier is still
  active — check for such notifications"——**依赖**演进 1 要删除的
  permission_mode_notice 注入。C3 保留现状（通知仍在，Bypass 下渲染的
  段落描述的机制与运行时一致）；**C4 删除通知时必须同步调整该句**（演进 1
  语义代价注释已预述：需确认 10_hitl 段落与权限模式相关测试同步调整），
  本批不改（超出 C3 边界）。

### D6：常量与测试同步

- `SECTION_IDS` / `MIDDLEWARE_NAMES` / `SECTION_HOLDER_MIDDLEWARE` 不变
  （10/11/13 持有者映射已就位，C1 建表）。
- prompt_test.rs：gate 相关测试语义反转（收集段恒渲染，与
  `PromptFeatures` 字段无关）；`PromptFeatures` 结构字面量删除；
  `detect()` 无参调用点同步；`section_ids_match_arrays_and_holders` 并集
  加入三个新持有者。
- assembly_test.rs：新增关闭场景测试（关闭 Hitl/SubAgent/Skills → 链收集
  无对应段落 + 工具消失）。
- chain_test.rs：投影一致性测试沿用（16_workflow 断言恒 false 保留）。

## 遗留问题（后续批次，非本批实现）

1. `FrozenSessionData::subagent_system_prompt` 遗留字段完整移除（C2 遗留
   问题 3，待后续批次）。
2. 演进 1（C4）：`permission_mode_notice_if_changed` 删除时，10_hitl 段落
   "controlled runtime notification" 句需同步调整（见 D5）。
3. `project_enabled_sections` 显式投影仍未接入生产渲染面（收集机制天然
   承担 gate 判定；投影保留为契约 3 显式视图与一致性测试载体）。

## 涉及文件

- `peri-acp/prompts/sections/10_hitl.md` / `11_subagent.md` / `13_skills.md`
  — 段落内容重构（机制说明保留、动态细节移除）
- `peri-middlewares/src/hitl/mod.rs` — `sections()` + `sensitive_tool_entries()`
  + `format_sensitive_tools()` + `prompt_sections()` trait 实现
- `peri-middlewares/src/subagent/mod.rs` — `sections()` + scan_agents 收敛
- `peri-middlewares/src/skills/mod.rs` — `sections()` + `format_discovery_protocol()`
- `peri-acp/src/prompt/mod.rs` — GATED_SECTIONS 收敛 + FeatureGate/PromptFeatures
  裁剪 + build_collected_sections 扩展
- `peri-acp/src/session/mod.rs` / `host/stage_builder.rs` /
  `host/workflow_agent.rs` — 构造点同步（detect 无参）
- `peri-acp/src/prompt/prompt_test.rs` / `peri-middlewares/src/assembly_test.rs`
  / `peri-middlewares/src/hitl/mod_test.rs` / `peri-middlewares/src/skills/mod_test.rs`
  / `peri-middlewares/src/subagent/mod_test.rs` — 测试更新与新增

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-14 | — | Closed | agent | C3 gated 段拆分 + gate 原子迁移实施完成；验证序列全绿 |
| 2026-08-14 | Closed | Closed | agent | 逐任务核查（任务书 1-5 对照设计 §3.1.1/3.1.2/3.5/3.5.1）：段落文件重构（10_hitl 列表删除 / 11_subagent Guide 浓缩 / 13_skills 协议细节删除）、三持有者 sections() 声明、scan_agents→scan_agents_detailed 同源收敛、GATED_SECTIONS 收敛至仅 15_channel、FeatureGate/PromptFeatures 裁剪、盲区闭合与投影一致性测试均就位；修复 2 处：chain_test.rs:212 过时注释（16_workflow 已删除，非"关闭"语义）、补 11_subagent 覆盖测试（D2 第 6 步要求的专属覆盖 + 段落顺序不变断言，先前仅 10_hitl/13_skills 覆盖测试）；验证序列重跑全绿 |
