// Phase 2（注册表）实施：并行实施 → 整合编译 → 并行 review → 并行 fix → 最终验证
// 依据：.peri/plans/2026-08-15-command-system-rearch/02-plan-phase2-registry.md
// 设计权威：docs/design/command-system.md
// 注意：注册表本体按终态放 peri-acp-types（Phase 6 A1 定案，检查修正共识「Phase 2 按终态实现」），
// 组合根（LegacyAdapter/register_builtins）留在 peri-acp。

export const meta = {
  name: 'command-system-phase2-implement',
  description: 'Phase 2 注册表并行实施 + code review fix 流程',
}

const PLAN2 = '.peri/plans/2026-08-15-command-system-rearch/02-plan-phase2-registry.md'
const DESIGN = 'docs/design/command-system.md'
const IMPL = '.peri/plans/2026-08-15-command-system-rearch/.implementation'
const TYPES = 'peri-acp-types/src'
const ACP = 'peri-acp/src'

// ── 阶段 1：契约层并行（A1 注册表本体 / A2 Context 拆层） ─────────
phase('契约层并行实施')

const c1 = await parallel([
  () => agent(`你在 Perihelion 实施 Phase 2 契约层【注册表本体】。依据：${DESIGN}（Routing 层章节，完整读）与 ${PLAN2}（Step 2/3，读对应小节）。Phase 1 已交付 RouteEntry（顶层扁平：fullname/aliases/description/kind/category/args_schema/handler: Arc<dyn CommandHandler>/provenance + level()）、CommandName（含 FromStr 词法解析）、CommandOutcome 三态。

任务：
1. 新文件 ${TYPES}/command_registry.rs + ${TYPES}/command_registry_test.rs：
   - pub struct ResolvedCommand { pub entry: Arc<RouteEntry>, pub args: String }（P1-6 定案，查找唯一出口返回值）
   - pub enum RegisterError { Conflict{key}, ProvenanceMismatch, MalformedName }
   - pub struct CommandRegistry（parking_lot::RwLock，三字段）：entries: RwLock<HashMap<String, Arc<RouteEntry>>>（fullname 小写键）+ alias_index: RwLock<HashMap<String, String>>（alias 小写 / 第一等级裸名小写 → fullname）+ on_change: RwLock<Option<Arc<dyn Fn() + Send + Sync>>>
   - API：new() / register(entry) -> Result<(), RegisterError> / unregister(fullname) -> bool / unregister_namespace(domain, namespace) -> usize / resolve(input) -> Option<ResolvedCommand> / snapshot() -> Vec<Arc<RouteEntry>> / set_on_change(cb)
   - register 语义：词法校验（CommandName 解析失败 → MalformedName）→ 域校验（parsed.domain() == provenance.source.domain() → 否则 ProvenanceMismatch）→ 同键冲突（entries 已占 / alias 已占 / 裸名被占 → Conflict + tracing::warn!）→ 全部通过才写入；Err 时注册表内容不变（纯拒绝，无替换分支）
   - alias_index 登记：显式 aliases + 第一等级域（core/ui）条目的裸名（name 段）；第二等级（mcp/plugin/user）不登记裸名
   - resolve 三段（仅精确，无前缀匹配）：trim_start_matches('/') + split_once(' ') 切词法 → entries 全名键（小写）→ alias_index（小写）；任何失败 → None（**全部 fall through 裁决**：词法非法/mcp__ 形态/lookup 未命中一律 None，不报错）
   - unregister_namespace：前缀 'domain:namespace:' 批量删除，返回移除数
   - on_change：锁内取回调克隆、锁外调用；内容变化（register Ok / unregister 命中 / unregister_namespace n>0）才触发；Err/未命中不触发
   - 模块只 import peri-acp-types 契约 + std + parking_lot，不 import 任何 handler 实现
2. 测试覆盖（照 plan Step 3 用例清单）：严格精确（裸名/core:compact 全名/alias 命中）、/rew 前缀 → None、歧义前缀 → None、大小写不敏感、冲突裁决（已占 → Conflict 且 snapshot 不变、alias 冲突、裸名冲突）、词法校验（mcp__/a:b:c:d/第一等级双层 → MalformedName）、域校验（plugin 条目注册 mcp:* → ProvenanceMismatch）、unregister（命中+on_change/未命中不触发）、unregister_namespace（mcp:demo: 批量、旁系保留）、on_change 触发矩阵、resolve 全部失败路径 → None、snapshot 内容
3. 不改动 ${TYPES}/command.rs（避免与其他任务冲突）；模块挂载由整合 agent 做

验证：cargo check -p peri-acp-types && cargo test -p peri-acp-types（若失败属其他任务未完成则记录忽略）。
完成后报告：文件清单 + 关键签名 + 测试数。`, { label: 'impl:registry' }),
  () => agent(`你在 Perihelion 实施 Phase 2 契约层【CommandContext 拆层】。依据：${DESIGN}（Execution 层 Context 小节）与 ${PLAN2}（Step 5.5，完整读）。

任务：修改 ${TYPES}/command.rs 的 CommandContext 部分（**只碰 CommandContext 相关代码，不动 CommandResult/CommandFeedback/词法/其他类型**）：
1. 结构改造：core 5 字段常驻（session_id: String / history: Vec<BaseMessage> / cwd: String / event_sink: Arc<dyn EventSink> / cancel_token: AgentCancellationToken，全部照现状类型），扩展依赖收进私有字段 deps: DependencyBag
2. pub type DependencyBag = HashMap<TypeId, Arc<dyn Any + Send + Sync>>
3. impl CommandContext：pub fn dep<T: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<T>>（TypeId::of::<T>() 查表）；pub fn new(session_id, history, cwd, event_sink, cancel_token, deps) -> Self
4. 现状 17 字段中 core 之外的字段（compact_config/auxiliary_model/args/thread_store/frozen_*/bg_*/task_manager 等）**本步全部保留为旧字段**——本任务只加新结构能力，**不做破坏性迁移**：为避免与消费方适配任务冲突，CommandContext 保持现状全部字段不变（17 字段原样保留），仅新增 deps 字段与 dep::<T>()/new() 方法，并在 doc 注释标注「Phase 2 适配完成后旧字段将随消费方迁移逐步退役」
5. 不要删除任何现有字段/方法（删除动作归消费方适配任务），保证 cargo check --workspace 仍绿

验证：cargo check --workspace 全绿（不破坏现状消费方）。
完成后报告：改动摘要 + 新增 API 签名 + 验证结果。`, { label: 'impl:context' }),
])

const c1Status = c1.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`契约层实施: ${JSON.stringify(c1Status)}`)

// ── 阶段 2：消费方并行（B1 换型适配 / B2 常驻化 / C 投影闭环） ────
phase('消费方并行实施')

const c2 = await parallel([
  () => agent(`你在 Perihelion 实施 Phase 2【换型适配】。依据：${PLAN2}（Step 4，完整读）。前置：契约层已交付 CommandRegistry/RegisterError/ResolvedCommand（${TYPES}/command_registry.rs）、CommandContext 已加 deps/dep::<T>()/new()（旧字段仍在）、RouteEntry 扁平形态（handler 为 pub 字段）。

任务（全部在 ${ACP} 与 peri-agent/src）：
1. ${ACP}/session/command/mod.rs：删除旧 Vec 注册表实现（find/find_arc/list/default_command_registry/default_prompt_command_registry 等，对齐 plan Step 4 删除清单）；改为 re-export registry 类型 + 组合根：
   - pub use peri_acp_types::command_registry::{CommandRegistry, RegisterError, ResolvedCommand};
   - LegacyAdapter<A: AgentCommand>（实现 CommandHandler，execute 映射 CommandResult → CommandOutcome::Done）
   - legacy_entry::<C>("core:compact") 辅助（fullname=core:{name}，aliases/description 取 A::aliases()/A::description()，args_schema=None）
   - register_builtins(reg)：注册 core:compact / core:bg / core:clear / core:rewind + loop 占位（LoopPlaceholder：Done + UI-only feedback(Info, 'loop 命令尚未实现')）
   - 保留 pub use registry 的对外路径（Phase 3/5 消费方引用 CommandRegistry 类型）
2. peri-agent/src/session/exec/executor_helpers.rs：CommandLookupFn 改 Arc<dyn Fn(&str) -> Option<ResolvedCommand> + Send + Sync>；拦截处改 resolved.entry.handler.execute(ctx)（删除 cmd.kind() != Immediate 判断），Outcome 匹配：Done → 现状 PromptResult 手拼 + push_done；Inject/Delegate → tracing::warn! + return None（fall-through）
3. peri-agent/src/session/exec/executor.rs：SessionContext.command_lookup 类型随编译提示微调
4. ${ACP}/dispatch/execute_command.rs：改 CommandRegistry::new() + register_builtins + resolve；kind() 判断改 Outcome 匹配（Done 走现状 JSON 序列化；Inject/Delegate → AcpError）；resolve None → AcpError（execute-command RPC 无 agent 管线，显式错误）
5. ${ACP}/session/command/mod_test.rs：删前缀匹配/list/default_* 用例，保留 ClearCommand 行为用例，新增 register_builtins 集成用例
6. peri-agent/src/session/exec/executor_helpers_test.rs + executor_test.rs：mock CommandLookupFn 改 ResolvedCommand 构造（假 handler）

验证：cargo check --workspace && cargo test -p peri-acp -p peri-agent && cargo clippy -p peri-acp -p peri-agent --all-targets -- -D warnings 全绿。
完成后报告：文件清单 + 关键改动 + 验证结果。`, { label: 'impl:switchover' }),
  () => agent(`你在 Perihelion 实施 Phase 2【会话级常驻化】。依据：${PLAN2}（Step 5，完整读）。前置：契约层已交付 CommandRegistry；换型适配任务可能同时在改 executor 相关文件——**你只碰以下三个文件，绝不碰 executor_helpers.rs/execute_command.rs/mod.rs**。

任务：
1. ${ACP}/session/mod.rs：AcpSession 加 pub command_registry: Arc<CommandRegistry>（照 mcp_skill_registry 先例 :112），两处构造点（:248/:285）初始化（创建时调用 register_builtins 注册内置命令——register_builtins 来自 ${ACP}/session/command/mod.rs 组合根），访问器 pub fn command_registry_for(&self, session_id: &str) -> Option<Arc<CommandRegistry>>（照 :558 mcp_skill_registry_for 先例）
2. ${ACP}/host/prompt.rs：:388-390 闭包从「每轮 default_prompt_command_registry()」改为捕获会话级注册表（session_manager 参数已存在）：session_manager.command_registry_for(&session_id) → Arc::new(move |text| registry.resolve(text))
3. ${ACP}/host/stdio/session/prompt_exec.rs：:322-324 同模式（从 session_manager 取或闭包外提升）
4. 删除 default_command_registry()/default_prompt_command_registry() 的最后引用（若换型适配任务已删旧实现，这里只需确认无残留引用）

验证：cargo check --workspace && cargo test -p peri-acp -p peri-agent。
注意行为变化（plan Step 5 风险）：非 TUI 客户端的 /clear /rewind 从 fall-through 变为 ACP 确定性执行——在完成报告里注明。
完成后报告：文件清单 + 关键改动 + 验证结果。`, { label: 'impl:session' }),
  () => agent(`你在 Perihelion 实施 Phase 2【on_change 投影闭环】。依据：${PLAN2}（Step 6，完整读）。前置：契约层已交付 CommandRegistry（含 snapshot()/set_on_change()）。

任务：
1. ${ACP}/dispatch/commands.rs（add-only，保持现有 build_available_commands_update 原样）：新增 pub(crate) fn available_command_from_entry(entry: &RouteEntry) -> AvailableCommand（AvailableCommand::new(entry.fullname.clone(), entry.description.clone())，仅 name/description，wire 形态与现状一致；_meta 扩展留 Phase 3）
2. 闭环测试：在 ${TYPES}/command_registry_test.rs 末尾**追加**（该文件由 registry 实施任务创建，已存在；只追加不修改已有用例）：on_change → 投影闭环用例——register_builtins 等价物（手工注册几个 core: 条目）→ set_on_change → register 新条目 → 断言回调触发 + 回调内 snapshot() + available_command_from_entry 可重建投影（条目数 +1）
   - 注意：command_registry_test.rs 在契约层，不能 import peri-acp 的 available_command_from_entry（pub(crate) 且依赖 agent-client-protocol-schema）——**改用测试内自建等价投影闭包**（fn entry_to_name_desc(entry) -> (String, String)），断言「snapshot 数据源可重建投影列表」这一闭环语义；真实的 available_command_from_entry 单测放 ${ACP}/dispatch/commands_test.rs（追加一个用例，若该文件存在；否则新建）
3. 若 ${ACP}/dispatch/commands_test.rs 不存在则跳过该处，只做契约层闭环用例

验证：cargo test -p peri-acp-types && cargo check --workspace。
完成后报告：改动文件 + 用例名 + 验证结果。`, { label: 'impl:projection' }),
])

const c2Status = c2.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`消费方实施: ${JSON.stringify(c2Status)}`)

// ── 阶段 3：整合编译 ─────────────────────────────────────────────
phase('整合与编译验证')

const integration = await agent(`你是 Phase 2 整合 agent。并行实施已完成：契约层（${TYPES}/command_registry.rs + command_registry_test.rs、command.rs 的 CommandContext deps/dep::<T>()/new()）、消费方（peri-acp session/command/mod.rs 组合根 + dispatch/execute_command.rs + mod_test.rs、peri-agent executor_helpers.rs/executor.rs/测试、session/mod.rs 常驻化、host/prompt.rs、prompt_exec.rs、dispatch/commands.rs 投影函数）。

任务：
1. 检查文件存在与完整性（ls + 快速读关键部分）
2. 模块挂载：${TYPES}/command.rs 或 lib.rs 挂 mod command_registry（若未挂）；确认 re-export 链（peri-acp-types::command_registry::* 可达）
3. 编译修复：cargo check --workspace 全绿；cargo test -p peri-acp-types -p peri-acp -p peri-agent 全绿；cargo clippy -p peri-acp-types -p peri-acp -p peri-agent --all-targets -- -D warnings 全绿
4. 修复所有编译/测试/clippy 错误（含并行任务间接口漂移：ResolvedCommand 字段、CommandRegistry API 签名、LegacyAdapter 泛型约束、LookupFn 闭包类型）
5. 语义一致性抽查：register_builtins 注册的条目能 resolve（裸名/全名/alias 各抽查一条）；/rew 前缀 → None；冲突拒绝不覆盖
6. 写实施状态到 ${IMPL}/phase2-integration.md：挂载清单、编译结果、修复记录、剩余问题

完成后报告：三绿状态 + 修复问题清单。`, { label: 'integrate' })

// ── 阶段 4：并行 review ─────────────────────────────────────────
phase('并行 code review')

const REVIEW_BASE = `你是 Phase 2 的 code reviewer。权威依据：${DESIGN}（Routing 层/Execution 层/Feedback 层）与 ${PLAN2}（Step 2-6 验收标准）。目标：对照设计与验收标准审查实施质量，输出可执行问题清单。只读不改代码。

输出：问题清单写入 ${IMPL}/phase2-review-<主题>.md，格式：
- 每条：严重度（P0 阻断编译或语义错误 / P1 违背设计或验收标准 / P2 建议）+ 文件:行号 + 问题描述 + 修复建议（具体到代码形态）
- 末尾：无问题项确认列表（已核对：xxx 符合）`

const reviews = await parallel([
  () => agent(`${REVIEW_BASE}

主题：registry。审查 ${TYPES}/command_registry.rs 与 command_registry_test.rs。
核对点：
1. register 语义：词法校验（CommandName 失败 → MalformedName）→ 域校验（→ ProvenanceMismatch）→ 冲突（Conflict + warn + 内容不变，纯拒绝无替换）——设计 §64 逐条
2. alias_index 登记规则：显式 alias + 第一等级裸名；第二等级不登记裸名
3. resolve 三段严格精确：全名键 / alias / 裸名；无前缀匹配（/rew → None）；**全部失败路径 → None（2026-08-15 裁决：词法非法/mcp__ 形态一律 None 不报错）**
4. unregister / unregister_namespace 语义与 on_change 触发矩阵（内容变化才触发、锁外回调）
5. snapshot 作为投影数据源
6. 模块依赖约束：不 import 任何 handler 实现
7. 测试覆盖与 plan Step 3 用例清单对照`, { label: 'review:registry' }),
  () => agent(`${REVIEW_BASE}

主题：context。审查 ${TYPES}/command.rs 的 CommandContext 改动。
核对点：
1. core 5 字段常驻 + deps 私有 + DependencyBag = HashMap<TypeId, Arc<dyn Any + Send + Sync>>（设计 §74）
2. dep::<T: ?Sized + Send + Sync + 'static>() -> Option<Arc<T>>（TypeId::of 按接口取，可注入 mock）
3. new() 构造辅助形态
4. 本步承诺：旧字段保留（破坏性迁移归消费方）——是否真的零破坏（cargo check 全绿）？
5. 与不变式 5 一致（新增依赖不动结构体）`, { label: 'review:context' }),
  () => agent(`${REVIEW_BASE}

主题：consumption。审查消费方适配：peri-acp/session/command/mod.rs 组合根（LegacyAdapter/register_builtins/loop 占位）、peri-agent executor_helpers.rs 拦截处、dispatch/execute_command.rs、session/mod.rs 常驻化、host/prompt.rs、prompt_exec.rs、dispatch/commands.rs 投影函数。
核对点：
1. LegacyAdapter：execute 映射 CommandResult → CommandOutcome::Done；无 Inject/Delegate 语义泄漏
2. register_builtins：五个条目（compact/bg/clear/rewind/loop 占位）、aliases/description 单一事实源（取 A::aliases()/A::description()）、注册顺序即优先级（内置先注册）
3. 拦截处：resolved.entry.handler.execute（RouteEntry 扁平 pub 字段）、Outcome 匹配（Done → PromptResult+push_done；Inject/Delegate → warn + fall-through）、删除 kind() 判断
4. execute_command.rs：resolve None → AcpError（RPC 显式错误，与 prompt fall-through 区分）
5. 常驻化：session 构造点两处初始化 + command_registry_for 访问器 + prompt 路径闭包捕获会话级注册表；default_* 无残留引用
6. 行为变化记录：非 TUI 客户端 /clear /rewind 从 fall-through 变确定性执行（plan Step 5 风险，报告中是否注明）
7. on_change 投影闭环测试存在性
8. 测试面：mod_test.rs 重写质量、mock LookupFn 适配、回归面（bg_test.rs push_done、requests_test.rs McpSkillRegistry 链路）未破坏`, { label: 'review:consumption' }),
])

const reviewStatus = reviews.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`Review 完成: ${JSON.stringify(reviewStatus)}`)

// ── 阶段 5：并行 fix ────────────────────────────────────────────
phase('并行 fix')

const fixes = await parallel([
  () => agent(`你是 Phase 2 fix agent（registry）。读取 ${IMPL}/phase2-review-registry.md 问题清单，逐一修复 ${TYPES}/command_registry.rs 及测试。P0/P1 必修，P2 视质量修。修复后 cargo check -p peri-acp-types && cargo test -p peri-acp-types 验证（注意不动模块声明区与 command.rs）。完成后报告：修复条目 + 验证结果`, { label: 'fix:registry' }),
  () => agent(`你是 Phase 2 fix agent（context）。读取 ${IMPL}/phase2-review-context.md 问题清单，逐一修复 ${TYPES}/command.rs 的 CommandContext 部分。P0/P1 必修。修复后 cargo check --workspace 验证。完成后报告：修复条目 + 验证结果`, { label: 'fix:context' }),
  () => agent(`你是 Phase 2 fix agent（consumption）。读取 ${IMPL}/phase2-review-consumption.md 问题清单，逐一修复消费方文件（mod.rs 组合根 / executor_helpers.rs / execute_command.rs / session/mod.rs / host/prompt.rs / prompt_exec.rs / dispatch/commands.rs）。P0/P1 必修。修复后 cargo check --workspace && cargo test -p peri-acp -p peri-agent 验证。完成后报告：修复条目 + 验证结果`, { label: 'fix:consumption' }),
])

const fixStatus = fixes.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`Fix 完成: ${JSON.stringify(fixStatus)}`)

// ── 阶段 6：最终验证 ────────────────────────────────────────────
phase('最终验证')

const final = await agent(`你是 Phase 2 最终验证 agent。

任务：
1. 全量验证：cargo check --workspace && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings（至少覆盖 -p peri-acp-types -p peri-acp -p peri-agent -p peri-tui），三绿
2. 验收核对：读 ${PLAN2} 验收标准小节（② 冲突拒绝不覆盖、③ 严格精确无前缀、④ 会话级常驻、⑤ Context 拆层、⑥ on_change 投影等），逐条给出 通过/不通过 + 证据（文件:行号或测试名）
3. 回归面复核：peri-tui input_area_test / acp_notifier_test（mcp__ 展示用例应仍绿——Phase 4 才清理）；peri-acp requests_test.rs（McpSkillRegistry on_change 链路）；peri-agent bg_test.rs（push_done 归 executor 断言）
4. 抽查：command_registry.rs 全文通读（无 TODO/半成品/死代码）；register_builtins 与 resolve 抽查（裸名/全名/alias 命中、/rew → None、冲突拒绝）
5. 写验证报告到 ${IMPL}/phase2-final.md：三绿状态 + 验收逐条 + 回归面 + 遗留问题

完成后报告：三绿状态 + 验收通过数/总条数 + 遗留问题`, { label: 'verify:final' })

return {
  contract: c1Status,
  consumption: c2Status,
  integration: integration ? 'ok' : 'FAILED',
  review: reviewStatus,
  fix: fixStatus,
  final: final ? 'ok' : 'FAILED',
}
