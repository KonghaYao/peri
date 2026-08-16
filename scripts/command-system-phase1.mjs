// Phase 1（契约层）实施：并行实施 → 整合编译 → 并行 review → 并行 fix → 最终验证
// 依据：.peri/plans/2026-08-15-command-system-rearch/01-plan-phase1-contract.md
// 设计权威：docs/design/command-system.md

export const meta = {
  name: 'command-system-phase1-implement',
  description: 'Phase 1 契约层并行实施 + code review fix 流程',
}

const PLAN1 = '.peri/plans/2026-08-15-command-system-rearch/01-plan-phase1-contract.md'
const DESIGN = 'docs/design/command-system.md'
const IMPL = '.peri/plans/2026-08-15-command-system-rearch/.implementation'
const SRC = 'peri-acp-types/src'

const COMMON = `你在 Perihelion 仓库实施 Phase 1（契约层）的代码。权威依据：${DESIGN}（设计文档，完整读词法/协议/不变式章节）与 ${PLAN1}（实施计划，读对应步骤与验收标准）。

约束（所有实施 agent 共同遵守）：
- 只写你负责的文件；禁止改动 peri-acp-types/src/command.rs 的模块声明区（mod xxx; 与 pub mod 区）与 src/lib.rs——模块挂载由整合 agent 统一做
- 新类型放独立新文件，测试同目录 *_test.rs（如现有 command_test.rs 风格，用 #[cfg(test)] #[path=...] 或同文件内 mod tests 均可，参照现有仓库惯例）
- 代码注释中文，风格对齐现有 command.rs
- serde 序列化齐备（Serialize + Deserialize + 必要属性）
- 验证：cargo check -p peri-acp-types 与 cargo test -p peri-acp-types；若失败原因属于其他 agent 未完成的部分（如 CommandResult 字段变更），记录并忽略，注明"非本组原因"
- 完成后报告：新增/修改文件清单 + 类型签名摘要 + 验证结果`

// ── Phase 1：并行实施（文件级隔离，5 组） ────────────────────────
phase('Phase1 并行实施')

const impls = await parallel([
  () => agent(`${COMMON}

你的任务：**CommandName 词法类型**（plan 步骤 2）。
- 新文件：${SRC}/command_name.rs + 对应测试
- 内容：命令全名的词法结构。解析规则（设计文档"权威词法"章节）：最右冒号切分（rsplit_once）；2 段冒号上限（3 段词）；三段形态 = 裸名 / domain:name（第一等级）/ domain:namespace:name（第二等级）；第一等级域 = core / ui；第二等级域 = mcp / plugin / user；裸名 = 第一等级域快捷匹配（非独立键）；'mcp__' 双下划线形态解析即失败（非法）；空段/空名非法
- 建议形态：enum CommandName { Bare(String), Level1 { domain, name }, Level2 { domain, namespace, name } } + 解析函数（TryFrom<&str> 或 from_str）+ CommandNameError（词法非法/超层/未知域/空段，错误信息可读）
- 测试覆盖：三形态解析、最右冒号（mcp:demo:hello）、超层拒绝（a:b:c:d）、未知域拒绝、mcp__ 拒绝、空段拒绝、roundtrip Display（全名小写规范化）`, { label: 'impl:name' }),
  () => agent(`${COMMON}

你的任务：**ArgsSchema serde 完整模型**（plan 步骤 3）。
- 新文件：${SRC}/command_args.rs + 对应测试
- 内容：ArgsSchema { positionals: Vec<ArgSpec>, named: Vec<ArgSpec>, flags: Vec<FlagSpec> }；ArgKind = String | Int | Choice(Vec<String>) | Path；ArgSpec { name, kind, required, description }；FlagSpec { name, short: Option<char>, description }
- 全部 serde 序列化（设计文档"Execution 层"：模型第一版即完整，TUI 补全/校验器依赖其形状；wire 投影经 _meta 通道）
- 测试覆盖：serde 往返（含 object 数组形态）、四类 kind、缺省字段默认值、序列化键名 snake_case 稳定`, { label: 'impl:args' }),
  () => agent(`${COMMON}

你的任务：**CommandFeedback + CommandResult.feedback 字段**（plan 步骤 4，唯一破坏性改动）。
- 修改：${SRC}/command.rs（定义 CommandFeedback，给 CommandResult 增加 feedback: Option<CommandFeedback> 字段，默认 None）
- CommandFeedback { level: FeedbackLevel, message: String, channel: FeedbackChannel }；FeedbackLevel = Info | Warning | Error；FeedbackChannel = UiOnly（默认）| Session（opt-in）
- **破坏面处理**：全仓库（peri-acp-types / peri-agent / peri-acp / peri-tui 如涉及）CommandResult 构造点补齐 feedback: None——这是本任务的核心，用 grep 找到全部构造点逐一补；构造点至少包括 peri-agent/src/session/exec/executor_helpers.rs、peri-acp/src/host/ 与 src/dispatch/ 相关位置（可参考 .implementation 前的调查证据 .investigation/01-registry.md 与 04-feedback.md 的构造点清单）
- 不动 command.rs 的模块声明区；保留现有字段与语义
- 验证：cargo check --workspace 全绿为本任务的完成标准（若其他 agent 的新文件未挂载导致失败，忽略并注明）`, { label: 'impl:feedback' }),
  () => agent(`${COMMON}

你的任务：**CommandOutcome 三态 + CommandHandler trait**（plan 步骤 5）。
- 新文件：${SRC}/command_handler.rs + 对应测试
- 内容：CommandOutcome { Done(CommandResult) | Inject(String) | Delegate(String) }；trait CommandHandler: Send + Sync { fn execute(&self, ctx: &CommandContext) -> CommandOutcome; }——注意：CommandContext 是现有类型（command.rs），execute 签名用 &CommandContext 或现有约定（读 command.rs 的 AgentCommand::execute 现状再定，若 AgentCommand 用值语义则保持一致并注明取舍）
- 与现有 AgentCommand trait 共存（Phase 5 才切换），本任务只新增不动旧 trait
- 测试：Outcome 三态构造与模式匹配（无需 handler 实现，可做假 handler）`, { label: 'impl:handler' }),
  () => agent(`${COMMON}

你的任务：**RouteEntry 三属性 + provenance 类型 + UiCommandSpec**（plan 步骤 6/7）。
- 新文件：${SRC}/command_route.rs + 对应测试
- 内容：
  - CommandSource = Core | Ui | Mcp | Plugin | User（来源域枚举，对应词法保留域）
  - CommandProvenance { source: CommandSource, origin: Option<String> }（origin 如 server 名/插件名）
  - CommandLifecycle = Static | Dynamic（静态内置 vs 动态注入）
  - RouteEntry { fullname: String, aliases: Vec<String>, description: String, kind: CommandEntryKind, category: Option<String>, args_schema: Option<ArgsSchema>, handler: Arc<dyn CommandHandler>, provenance: CommandProvenance, lifecycle: CommandLifecycle }——**顶层扁平形态**（全局检查 P1-1 定案，fullname 含域全名小写规范化）
  - CommandEntryKind = Command | Skill | McpSkill | Panel（serde snake_case，注册时由 handler 域推导一次）
  - UiCommandSpec { name, aliases: Vec<String>, description, args: Option<ArgsSchema> }（Phase 3 caps 上送用，本次仅定义类型）
  - RouteEntry::level() 推导：Core/Ui → 1，其余 → 2
- 测试：RouteEntry 构造 + level 推导 + provenance 组合`, { label: 'impl:route' }),
])

const implStatus = impls.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`实施完成: ${JSON.stringify(implStatus)}`)

// ── 整合：模块挂载 + 编译修复 ────────────────────────────────────
phase('整合与编译验证')

const integration = await agent(`你是 Phase 1 整合 agent。5 个并行实施 agent 已完成契约层类型（${SRC}/command_name.rs、command_args.rs、command_handler.rs、command_route.rs 及测试，以及 command.rs 的 CommandFeedback/CommandResult.feedback 改动与全仓构造点补齐）。

你的任务：
1. 检查上述文件是否存在与完整（ls + 快速读关键部分）
2. 在 ${SRC}/command.rs 的模块声明区（mod xxx 区，注意别与其他改动冲突）挂载新模块：mod command_name; mod command_args; mod command_handler; mod command_route;（如已有 pub use 模式则仿照；lib.rs 不必动，command.rs 已在 lib 导出）
3. 编译修复：cargo check --workspace 全绿；cargo test -p peri-acp-types 全绿；cargo clippy -p peri-acp-types -- -D warnings 全绿
4. 修复所有编译/测试/clippy 错误（含并行 agent 之间的接口不一致：如 handler.rs 引用的 CommandResult/CommandContext 与 command.rs 实际定义、route.rs 的 Arc<dyn CommandHandler> 与 handler.rs 的 trait）
5. 若某类型语义与设计文档冲突（如 ArgsSchema 字段名），以设计文档为准修正并记录
6. 写实施状态到 ${IMPL}/integration.md：挂载清单、编译结果、修复记录、剩余问题

完成后报告：编译/测试/clippy 三绿状态 + 修复的问题清单`, { label: 'integrate' })

// ── Review：并行 code review（写问题清单文件） ───────────────────
phase('并行 code review')

const REVIEW_BASE = `你是 Phase 1 契约层的 code reviewer。权威依据：${DESIGN}（设计文档）与 ${PLAN1}（实施计划验收标准）。目标：对照设计与验收标准审查实施质量，输出可执行的问题清单。只读，不改代码。

输出：问题清单写入 ${IMPL}/review-<主题>.md，格式：
- 每条问题：严重度（P0 阻断编译或语义错误 / P1 违背设计或验收标准 / P2 建议）+ 文件:行号 + 问题描述 + 修复建议（具体到代码形态）
- 末尾：无问题项确认列表（写明"已核对：xxx 符合"）`

const reviews = await parallel([
  () => agent(`${REVIEW_BASE}

主题：lexical。审查 ${SRC}/command_name.rs 与测试。
核对点（设计文档"权威词法"逐条）：
1. 最右冒号切分、2 段冒号上限
2. 三形态（裸名 / domain:name / domain:namespace:name）与第一/第二等级域（core/ui vs mcp/plugin/user）
3. mcp__ 双下划线拒绝、空段拒绝、未知域拒绝
4. 裸名语义（第一等级域快捷匹配，非独立键）——类型层面如何表达（是否给解析器留了"裸名 → core/ui 域内精确匹配"的接口）
5. 错误信息可读性、全名小写规范化、测试覆盖
6. 与 plan 步骤 2 验收标准逐条核对`, { label: 'review:lexical' }),
  () => agent(`${REVIEW_BASE}

主题：args。审查 ${SRC}/command_args.rs 与测试。
核对点：
1. ArgsSchema 模型完整性（positionals/named/flags + String|Int|Choice|Path + FlagSpec{name,short,description}）——设计文档"Execution 层"要求模型第一版即完整
2. serde 往返稳定性（object 数组形态、snake_case、缺省字段）——wire _meta 通道依赖它
3. Choice 的 values 承载、required 语义、description 可空性
4. 与 plan 步骤 3 验收标准逐条核对`, { label: 'review:args' }),
  () => agent(`${REVIEW_BASE}

主题：contract。审查 command.rs 的 CommandFeedback/CommandResult.feedback 改动、${SRC}/command_handler.rs、${SRC}/command_route.rs。
核对点：
1. CommandFeedback 三要素（level/message/channel，UiOnly 默认、Session opt-in）与设计文档 Feedback 层一致
2. CommandResult.feedback 破坏面：全仓构造点是否补齐（grep 确认无遗漏）、默认 None 语义、15+ 处构造点完整性
3. CommandOutcome 三态（Done/Inject/Delegate）与设计文档 Execution 层一致
4. CommandHandler trait 签名与 CommandContext 的借用/值语义是否合理（与现有 AgentCommand 共存评估）
5. RouteEntry 顶层扁平形态（P1-1 定案：fullname/aliases/description/kind/category/args_schema/handler/provenance/lifecycle）字段齐全、level() 推导正确（Core/Ui→1）
6. provenance 类型（CommandSource/CommandProvenance/CommandLifecycle）与词法保留域对应
7. UiCommandSpec 形态正确
8. 与 plan 步骤 4/5/6/7 验收标准逐条核对`, { label: 'review:contract' }),
])

const reviewStatus = reviews.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`Review 完成: ${JSON.stringify(reviewStatus)}`)

// ── Fix：并行修复（读对应 review 文件） ──────────────────────────
phase('并行 fix')

const fix = await parallel([
  () => agent(`你是 Phase 1 fix agent（lexical）。读取 ${IMPL}/review-lexical.md 的问题清单，逐一修复 ${SRC}/command_name.rs 及其测试。修复原则：P0/P1 必修，P2 视质量判断（可修可不修时修）。修复后运行 cargo check -p peri-acp-types 与 cargo test -p peri-acp-types 验证。完成后报告：修复条目（每条一行：编号 + 一句话）+ 验证结果`, { label: 'fix:lexical' }),
  () => agent(`你是 Phase 1 fix agent（args）。读取 ${IMPL}/review-args.md 的问题清单，逐一修复 ${SRC}/command_args.rs 及其测试。修复原则：P0/P1 必修，P2 视质量判断。修复后运行 cargo check -p peri-acp-types 与 cargo test -p peri-acp-types 验证。完成后报告：修复条目 + 验证结果`, { label: 'fix:args' }),
  () => agent(`你是 Phase 1 fix agent（contract）。读取 ${IMPL}/review-contract.md 的问题清单，逐一修复 command.rs 的 CommandFeedback/CommandResult.feedback 改动、${SRC}/command_handler.rs、${SRC}/command_route.rs 及测试。修复原则：P0/P1 必修。修复后运行 cargo check --workspace 与 cargo test -p peri-acp-types 验证（注意不要动模块声明区）。完成后报告：修复条目 + 验证结果`, { label: 'fix:contract' }),
])

const fixStatus = fix.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`Fix 完成: ${JSON.stringify(fixStatus)}`)

// ── 最终验证 ────────────────────────────────────────────────────
phase('最终验证')

const final = await agent(`你是 Phase 1 最终验证 agent。对契约层实施做全量验证与验收核对。

任务：
1. 全量验证：cargo check --workspace && cargo test -p peri-acp-types && cargo clippy -p peri-acp-types -- -D warnings，三绿
2. 验收核对：读 ${PLAN1} 的验收标准小节，逐条验证（如"ArgsSchema serde 完整模型"、"Outcome 三态"、"mcp__ 解析即失败"等），每条给出 通过/不通过 + 证据（文件:行号或测试名）
3. 代码抽查：快速读 command_name.rs / command_args.rs / command_handler.rs / command_route.rs 各 30 行，确认无半成品/TODO 残留、无死代码
4. 写验证报告到 ${IMPL}/final.md：三绿状态 + 验收标准逐条结果 + 遗留问题（若有）

完成后报告：三绿状态 + 验收通过数/总条数 + 遗留问题`, { label: 'verify:final' })

return {
  implementation: implStatus,
  integration: integration ? 'ok' : 'FAILED',
  review: reviewStatus,
  fix: fixStatus,
  final: final ? 'ok' : 'FAILED',
}
