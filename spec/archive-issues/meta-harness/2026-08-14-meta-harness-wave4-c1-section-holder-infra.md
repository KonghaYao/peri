# MetaHarness 波 4 C1——段落持有者基础设施实施决策记录

**状态**：Closed（C1 已实施）
**优先级**：高（波 4 演进基础设施批次）
**类型**：决策记录
**创建日期**：2026-08-14
**来源**：`docs/design/meta-harness-design.md` §3.1.1（拆分持有契约 2/3/4）+ §3.5
（演进方向/语义边界）；批 C1 任务书

## 背景

波 4 演进 2（系统提示词段拆分持有）的前置基础设施：middleware 持有段落
需要"段落持有者接口 + 位置属性 + 装配期收集 + gate 投影"四件套，段落实体
迁移（C2/C3）在其上执行。本批只建机制，**现有段落仍由编译期常量数组持有
（未迁移前行为不变）**。

## 实施决策

### D1：段落持有者接口落点（peri-agent）

- `PromptSection { id, zone, order, content }` / `PromptSectionZone { Cached,
  Uncached }` / `PromptSectionContent { Builtin(&'static str), Dynamic(String) }`
  定义于新文件 `peri-agent/src/middleware/prompt_sections.rs`（Middleware trait
  所在 crate，内容载体与渲染消费方 `peri-acp` 依赖方向一致：
  `peri-acp → peri-agent`）。
- trait 方法 `Middleware::prompt_sections(&self) -> Vec<PromptSection>`（默认空，
  契约 4：未提供段落 = 跳过渲染不 fail）。
- 收集：`MiddlewareChain::collect_prompt_sections()`（与 `collect_tools` 同构
  flat_map；收集不承诺顺序，排序由渲染面执行）。

### D2：位置属性编码（契约 2）

- zone = boundary 前缓存区（Cached）/ boundary 后非缓存区（Uncached）；
  order = 段内序号（u16）。
- 内置数组段落编号（C1 起即显式化，渲染顺序与现状逐字节一致）：
  - `IMMUTABLE_SECTIONS`（01-06）：Cached，order 1-6；
  - `ALWAYS_UNCACHED_SECTIONS`（07, 14）：Uncached，order 1-2；
  - `GATED_SECTIONS`（10/11/13/15/16）：Uncached，order 3-7。
- **C2/C3 迁移时持有者声明的 order 必须与上表一致**（10_hitl=3、11_subagent=4、
  13_skills=5、15_channel=6、16_workflow=7），否则渲染顺序漂移——一致性由
  C2/C3 迁移测试锁定（本批已记录编号事实于 `PromptTemplate::new` 注释）。

### D3：PromptTemplate 按收集结果物化（任务 2/3）

- `PromptTemplate::new(state, collected: &[PromptSection])`：
  1. 内置数组 → 段落声明（zone/order/gate 显式化）；
  2. collected 按 ID 覆盖内置（位置属性以持有者声明为准，gate = Always——
     收集即持有者已装配，契约 3）；
  3. `state.section_overrides` 按 ID 替换内容（覆盖 = 替换持有者对应段落贡献，
     机制与持有者无关，设计 §2.4）；
  4. 空内容段落过滤（契约 4 防御）+ 按 (zone, order) 稳定排序（契约 2）。
- 内部容器从三容器（immutable/always_uncached/gated）合并为两容器
  （cached/uncached + `SectionGate { Always, Feature(FeatureGate) }`），
  render 顺序语义不变：缓存区前段 → BOUNDARY → Persona → 缓存区后段 →
  gated → Language（任务 2 顺序描述；字节一致性由
  `test_prompt_template_byte_identical_to_build_system_prompt` 等回归测试锁定）。
- `SectionContent` 新增 `Dynamic(String)` 变体（middleware 动态生成段落，
  如 10_hitl sensitive 列表、13_skills 协议细节）；零拷贝双态（Q2）不变。

### D4：构造点同步（任务 3，同源一致性）

全部构造点显式传 `&[]`（C1 无 middleware 贡献，行为不变）：

| 构造点 | 位置 | 说明 |
| --- | --- | --- |
| 冻结渲染 | `session/mod.rs` build_frozen_data | 收集结果传空 |
| 主重渲染闭包 | `host/stage_builder.rs` render_system_prompt | 同上 |
| SubAgent system_builder | `host/stage_builder.rs` system_builder | 同上 |
| workflow fallback | `host/workflow_agent.rs` build_workflow_system_prompt_fallback | 同上 |
| workflow agent builder | `host/workflow_agent.rs` build_workflow_agent_prompt_builder | 同上 |
| 测试 helper | `prompt/mod.rs` build_system_prompt | 仅测试；签名不变（传 `&[]`），40+ 调用点零改动 |

### D5：gate 判定基础设施（任务 4，只建机制）

- 映射表 `SECTION_HOLDER_MIDDLEWARE: &[(&str, &str)]`（`peri-acp-types/src/
  meta_harness.rs`）：10_hitl→HumanInTheLoopMiddleware、11_subagent→
  SubAgentMiddleware、13_skills→SkillsMiddleware、16_workflow→
  WorkflowMiddleware；15_channel 无持有者不入表（gate 恒 false 直至未来
  channel middleware）。一致性测试锁定：段落 ID ∈ `SECTION_IDS`、持有者 ∈
  `MIDDLEWARE_NAMES`、无重复。
- 装配期投影 `project_enabled_sections(chain_names) -> HashSet<&'static str>`
  （`peri-agent/src/middleware/prompt_sections.rs`）：遍历映射表，持有者在链上
  即段落 gate 开启。纯函数（输入名集合），供 C2/C3 迁移时替换
  `PromptFeatures::detect` 硬编码与过渡期一致性检查。
- **本批不启用**（设计 §3.5 语义边界 ②）：未迁移段落的 gate 仍由
  `PromptFeatures::detect` 硬编码（`SectionGate::Feature`）；收集段 gate =
  Always（收集即装配，契约 3 语义）。

### D6：语义边界确认（设计 §3.5 ①）

`prompt_contribution`（首轮一次性通知）与 `prompt_sections`（段落内容载体）
**两个通道并行、语义互不替代**：本批未改动 `prompt_contribution` 任何行为；
middleware 持有的段落仍走 `PromptTemplate` 段落装配渲染。

## 遗留问题（C2/C3 前置，非本批实现）

1. **冻结渲染的收集来源**：`build_frozen_data`（session/new）时链未装配，
   收集结果只能为空。C2/C3 迁移段落时，冻结渲染需改为从冻结载体传播的
   收集结果（`FrozenContext` 新增字段）或装配面静态声明收集——本批未定，
   记入未完成清单。
2. **重渲染闭包捕获**：`render_system_prompt` / `system_builder` /
   workflow 构造点闭包在链装配前构造，C2/C3 时需捕获收集结果（随冻结传播
   或装配后注入）。
3. **gate 投影接线**：`project_enabled_sections` 在 C2/C3 迁移首批段落时
   替换 `PromptFeatures::detect` 对应字段（契约 3 原子迁移，禁止"段落已移走、
   gate 仍硬编码"双轨）。

## 验证结果

- `cargo test -p peri-acp --lib prompt`：81 passed（含 6 个新收集合并测试）
- `cargo test -p peri-agent --lib middleware`：30 passed（含 3 个新收集/投影测试）
- `cargo test -p peri-acp-types --lib meta_harness`：4 passed（含 1 个新映射表一致性测试）
- 全量验证序列见批次输出（build/clippy/四 crate lib 测试）

## 涉及文件

- `peri-agent/src/middleware/prompt_sections.rs`（新）— 段落持有者接口类型 +
  投影函数
- `peri-agent/src/middleware/trait.rs` — `prompt_sections()` trait 方法
- `peri-agent/src/middleware/chain.rs` — `collect_prompt_sections()`
- `peri-agent/src/middleware/mod.rs` — re-export
- `peri-agent/src/middleware/chain_test.rs` — 收集/投影测试
- `peri-agent/src/session/exec/stage_builder.rs` — 去除既有 `unused_braces`
  warning（clippy -D warnings 前置）
- `peri-acp-types/src/meta_harness.rs` — `SECTION_HOLDER_MIDDLEWARE` 映射表 +
  一致性测试
- `peri-acp/src/prompt/mod.rs` — `PromptTemplate::new(state, collected)` 重构
  （两容器 + SectionGate + Dynamic 变体）+ `build_system_prompt` 同步
- `peri-acp/src/prompt/prompt_test.rs` — 构造点同步 + 6 个收集合并测试
- `peri-acp/src/session/mod.rs` / `host/stage_builder.rs` /
  `host/workflow_agent.rs` — 构造点同步（`&[]`）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-14 | — | Closed | agent | C1 段落持有者基础设施实施完成；测试全绿 |
