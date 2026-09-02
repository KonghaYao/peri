# MetaHarness 波 4 C2——基础段迁移实施决策记录

**状态**：Closed（C2 已实施）
**优先级**：高（波 4 演进批次）
**类型**：决策记录
**创建日期**：2026-08-14
**来源**：MetaHarness base section 与渲染生成段迁移；当前稳定设计见 `docs/design/meta-harness.md` §2.4/§2.7，过程由本归档 issue 保留
§3.1.1（拆分持有契约 + 归属全景）；批 C2 任务书；C1 决策记录 D7 遗留问题

## 背景

C1 已落地段落持有者基础设施（`PromptSection` 接口 + 装配期收集 +
gate 投影 + `PromptTemplate::new(state, collected)` 物化合并）。本批执行
段落实体迁移：基础段（01-06 / 07_runtime / Persona）→
`DefaultSystemPromptMiddleware`，Language → 新 `LangMiddleware`；boundary
文本标记删除；02 Proactiveness 并入 03；07+14 合并为 07_runtime；
16_workflow 整段删除；段落 + gate 原子迁移（契约 3）。

## 实施决策

### D1：段落位置属性（本批迁移后的事实，锁定排序契约）

| id | zone | order | 持有者 | 说明 |
| --- | --- | --- | --- | --- |
| persona | Uncached | 0 | DefaultSystemPromptMiddleware（Dynamic） | 现状 boundary 后、07 之前 |
| 01_intro .. 06_tone_style | Cached | 1-6 | DefaultSystemPromptMiddleware（Builtin） | 缓存区 |
| 07_runtime | Uncached | 1 | DefaultSystemPromptMiddleware（Builtin） | 07_env + 14_system_reminder 合并 |
| 10_hitl / 11_subagent / 13_skills / 15_channel | Uncached | 3-6 | 内置 GATED（C3 迁移） | 编号沿用 C1 D2 表（16 删除后空缺 7） |
| language | Uncached | 7 | LangMiddleware（Dynamic） | 现状 gated 之后最后段 |

- 02_system / 03_doing_tasks 内容变更（Proactiveness 块移动）不改位置属性。
- 16_workflow 删除后 gated 序号空缺 7：语言段取 7（gated 之后，编号不重排）。

### D2：DefaultSystemPromptMiddleware / LangMiddleware 落点（peri-middlewares）

- 新模块 `peri-middlewares/src/default_system_prompt/`（middleware 持有段落实体；
  渲染面与链收集共用同一段声明函数——单一事实源，禁止双轨）。
- `DefaultSystemPromptMiddleware::sections(overrides: Option<&AgentOverrides>)`：
  静态声明 01-06 + 07_runtime（`include_str!`，文件留在
  `peri-acp/prompts/sections/`，经 `../peri-acp/prompts/sections/` workspace
  相对路径引用——设计 §3.5.1 步骤 3"文件可留在 sections/ 由 middleware
  include_str!"）+ persona 动态段（id `persona`）。
- persona 三态（设计 §3.5.2 步骤 2，现状 render :399-407 语义保持）：
  - full → `persona.body.trim()`；
  - extend → `build_agent_overrides_block`（含尾部 `\n\n`，从 prompt/mod.rs 迁入）；
  - 无 overrides / 空内容 → 空字符串（**段仍声明**——空内容由
    `PromptTemplate::new` 过滤，保证 `.peri/meta/persona.md` 覆盖在无用户
    overrides 时仍可注入，覆盖先于空过滤生效）。
- `LangMiddleware::sections(language: Option<&str>)`：动态段 id `language`，
  `map_language_to_instruction` 逻辑随持有者迁入（从 prompt/mod.rs 移走）；
  `None` → 空字符串段（同样保证 `.peri/meta/language.md` 覆盖可注入）。
- 装配期：`ChainSlot::DefaultSystemPrompt` / `ChainSlot::Lang` 加入
  `production_blueprint` 第一组首位（链序不参与渲染排序，契约 2）；
  assembly 按 disabled 集合过滤（契约 4：默认总是装配，除非显式关闭）。
- `AssemblyContext` 新增 `agent_overrides: Option<AgentOverrides>` 与
  `language: Option<String>` 字段（stage 装配投影），middleware 构造时持入，
  使链收集与渲染面静态声明字节一致。

### D3：渲染面收集来源（C1 遗留问题 1/2 落定：装配面静态声明收集）

- `PromptTemplate` 构造点（冻结渲染 / 主重渲染 / SubAgent builder / workflow
  fallback / workflow agent builder / 测试 helper）统一经新 helper
  `prompt::build_collected_sections(state, overrides, language)` 计算收集
  结果：`DefaultSystemPromptMiddleware` / `LangMiddleware` 不在
  `state.disabled_middlewares` 时收集对应段（**冻结状态驱动**，链未装配的
  构造点也能得到与装配一致的段落；ARC-FROZEN-001 语义保持）。
- 链侧 `collect_prompt_sections()` 与渲染侧 helper 调用同一段声明函数，
  一致性由测试锁定（assembly/chain 测试）。
- 主重渲染闭包（`render_system_prompt`）**补上 language**：现状该闭包
  render 传 `None`（--agent override 时语言段丢失，与 SubAgent builder 传
  language 不一致）；C2 后所有构造点统一经 LangMiddleware 段声明注入语言
  段（设计 §3.5.2 步骤 3"全部构造点同步调整"），行为向 SubAgent 路径对齐。

### D4：PromptTemplate 重构（boundary 删除 + with_overrides 退役）

- `render` 删除 `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 拼接与 Persona 特判
  （persona 是普通收集段）；删除 `language` 参数（LangMiddleware 持有内容）。
- `with_overrides` / `full_body` / `overrides_block` 删除——persona 内容
  在收集期由 DefaultSystemPromptMiddleware 按 overrides 生成。
- 内置数组收敛：`IMMUTABLE_SECTIONS` / `ALWAYS_UNCACHED_SECTIONS` 删除
  （迁移完成，禁止双轨）；`GATED_SECTIONS` 仅剩 10/11/13/15 四项
  （gate 仍由 `PromptFeatures::detect` 硬编码，语义边界 ②——C3 迁移）。
- 渲染顺序不变：cached（01-06，`\n\n` 连接）→ uncached（persona →
  07_runtime → gated → language，每段 `\n\n` 前缀 + gate 判定）→ 占位符替换。

### D5：16_workflow 删除 + 无持有者 gate 清理（任务 6）

- `sections/16_workflow.md` 删除；`GATED_SECTIONS` 项删除；
  `SECTION_HOLDER_MIDDLEWARE` 移除 ("16_workflow", "WorkflowMiddleware") 项。
- `FeatureGate::Workflow`、`PromptFeatures::workflow_enabled`、
  `PromptFeatures::detect` 的 workflow 参数、`detect_without_workflow` 删除
  （无持有者的 gate 清理）。
- `build_frozen_data` 的 `workflow_enabled` 参数删除（唯一用途是子面向
  二次渲染）；`FrozenSessionData::subagent_system_prompt` 字段保留为遗留
  （accessor 回退 `system_prompt()`，两版字节相同），`build_frozen_data`
  传 None 不再二次渲染。
- 相关注释（fork / workflow agent "无 16_workflow 版本"）同步更新。

### D6：段落文件变更（任务 4/5）

- 02_system.md：`# Proactiveness` 块（7 行）移入 03_doing_tasks.md 末尾
  （设计 §3.1.1：Proactiveness 主题 = 执行模式，归 03）。
- 07_runtime.md（新）= 07_env.md 全文 + 14_system_reminder.md 全文
  （信任边界内容保留——安全相关，设计 §3.1.1 归属全景）；删除
  14_system_reminder.md。

### D7：常量与校验面同步（任务 8）

- `SECTION_IDS`：删 "07_env" / "14_system_reminder" / "16_workflow"；
  增 "07_runtime" / "persona" / "language"（persona/language 入表使
  `.peri/meta/persona.md` / `.peri/meta/language.md` 覆盖通过解析期校验）。
- `MIDDLEWARE_NAMES`：增 "DefaultSystemPromptMiddleware" / "LangMiddleware"。

## 遗留问题（C3 前置，非本批实现）

1. **gated 段迁移**：10_hitl / 11_subagent / 13_skills 段落实体 + gate 判定
   （`PromptFeatures::detect` 硬编码 → `project_enabled_sections`）随 C3
   迁移；15_channel 保持 gate 恒 false。
2. **sensitive 列表 / Agent Selection Guide / skills 协议细节**随对应
   middleware 动态生成（设计 §3.1.2）。
3. `FrozenSessionData::subagent_system_prompt` 遗留字段的完整移除（字段 +
   `from_frozen_parts` 参数 + fork 复制路径），待后续批次。

## 涉及文件

- `peri-middlewares/src/default_system_prompt/`（新）— 两个 middleware +
  段声明函数（含 build_agent_overrides_block / map_language_to_instruction
  迁入）
- `peri-middlewares/src/assembly.rs` — 新槽位装配
- `peri-middlewares/src/lib.rs` — 模块导出
- `peri-middlewares/src/assembly_test.rs` — AssemblyContext 新字段
- `peri-agent/src/session/factory.rs` — ChainSlot / blueprint / AssemblyContext
- `peri-agent/src/session/exec/stage_builder.rs` — 装配上下文投影
- `peri-acp-types/src/meta_harness.rs` — SECTION_IDS / MIDDLEWARE_NAMES /
  SECTION_HOLDER_MIDDLEWARE
- `peri-acp/src/prompt/mod.rs` — PromptTemplate 重构 + build_collected_sections
- `peri-acp/src/session/mod.rs` / `host/stage_builder.rs` /
  `host/workflow_agent.rs` — 构造点同步
- `peri-acp/prompts/sections/` — 02/03/07_runtime 变更，14/16 删除
- 测试：`prompt_test.rs` / `executor_flow_test.rs` / `session/mod_test.rs` /
  `chain_test.rs` / `assembly_test.rs`

## 验证结果

- `cargo build --workspace`：✅
- `cargo clippy --workspace --all-targets -- -D warnings`：✅ 零警告（修复
  `assembly_test.rs` 两处 `manual_contains`）
- `cargo test -p peri-acp-types -p peri-acp -p peri-middlewares -p peri-agent
  --lib`：✅ 382 + 1318 + 139 + 672 全绿（3 ignored 为既有）
- `cargo test --workspace --doc`（改过 doc comment）：✅ 15 crate 全绿

行为确认：渲染顺序逐字节回归由 `prompt_test.rs` 锁定（缓存区 01-06 →
persona → 07_runtime → gated → language；boundary 标记不再生成；
16_workflow 任何 gate 组合均不渲染）。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-14 | — | Closed | agent | C2 实施完成 |
| 2026-08-14 | Closed | Closed | agent | 批次复核：补全验证结果；修复 assembly_test.rs 两处 manual_contains（clippy 零警告）；同步波 3 交付物段落 ID 清单（07_runtime / persona / language / 16_workflow 删除） |
