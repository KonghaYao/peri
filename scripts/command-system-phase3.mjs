// Phase 3（协议层）实施：契约层并行 → 发送侧适配 → 整合 → review → fix → 验证
// 依据：.peri/plans/2026-08-15-command-system-rearch/03-plan-phase3-protocol.md
// 设计权威：docs/design/command-system.md

export const meta = {
  name: 'command-system-phase3-implement',
  description: 'Phase 3 协议层并行实施 + code review fix 流程',
}

const PLAN3 = '.peri/plans/2026-08-15-command-system-rearch/03-plan-phase3-protocol.md'
const DESIGN = 'docs/design/command-system.md'
const IMPL = '.peri/plans/2026-08-15-command-system-rearch/.implementation'
const TYPES = 'peri-acp-types/src'
const ACP = 'peri-acp/src'

// ── 阶段 1：契约层并行（A caps 升级 / B 事件链路） ────────────────
phase('契约层并行实施')

const c1 = await parallel([
  () => agent(`你在 Perihelion 实施 Phase 3 契约层【caps 明细化】。依据：${DESIGN}（协议层章节）与 ${PLAN3}（步骤 2，完整读）。Phase 1 已交付 ArgsSchema / CommandEntryKind（snake_case）/ RouteEntry。

任务：修改 ${TYPES}/peri_caps.rs：
1. 新增 pub struct UiCommandSpec { name: String, aliases: Vec<String>（default）, description: String（default）, args: Option<ArgsSchema>（default + skip_serializing_if None） }，serde camelCase + default
2. peri_caps.rs 的 ui_commands 字段：pub bool → pub Vec<UiCommandSpec>（serde default）
3. from_client_meta（:65-83）：'peri.uiCommands' 兼容两态——数组 → Vec<UiCommandSpec>（serde_json::from_value，解析失败按空处理 + warn）；旧客户端 bool true → 退化为 default_ui_commands() 明细（等价现状行为）
4. to_agent_meta（:86-113）：'peri.uiCommands' 序列化为数组
5. all_enabled()（:117-132）：ui_commands 填 default_ui_commands()——11 条 (name, desc) 数据从 ${ACP}/dispatch/commands.rs 的 UI_COMMANDS 常量迁移（先读该常量，再迁移为 UiCommandSpec 字面量集合）；fn default_ui_commands() -> Vec<UiCommandSpec> 定义在本文件（pub(crate) 或 pub，供 from_client_meta 兼容分支复用）
6. ${TYPES}/peri_caps_test.rs 同步：bool 声明兼容用例（旧形态 meta → 退化明细）、数组往返用例、all_enabled 明细断言（11 条）

验证：cargo check -p peri-acp-types && cargo test -p peri-acp-types。
注意：${TYPES}/dispatch/commands.rs 的 UI_COMMANDS 常量删除归发送侧任务，你只读不删。
完成后报告：文件 + 关键签名 + 测试数。`, { label: 'impl:caps' }),
  () => agent(`你在 Perihelion 实施 Phase 3 契约层【CommandFeedback 事件链路】。依据：${PLAN3}（步骤 5，完整读）。Phase 1 已交付 CommandFeedback { level, message, channel } + FeedbackChannel（UiOnly 默认）。

任务（5 处改动，全部按 plan 步骤 5 形态）：
1. ${TYPES}/event.rs：ExecutorEvent 枚举（:307 附近）新增变体 CommandFeedback(crate::command::CommandFeedback)（tag/content snake_case，功能载体事件）
2. ${ACP}/event/mod.rs：AcpEvent 枚举（:42 附近）新增 DTO 变体 CommandFeedback { level: String, message: String, channel: String }（wire string 化；注释注明 channel=session 由 TUI 侧 opt-in 另写系统消息）
3. ${ACP}/event/mapper.rs：穷尽 match（:232-266 分支组）加分支 ExecutorEvent::CommandFeedback(_) => vec![MappedEvent::standard(vec![])]（无标准 SessionUpdate，经 peri/agent_event 通道）
4. ${ACP}/session/event_sink.rs：AcpEvent map 分支（:183-330）加 ExecutorEvent::CommandFeedback(fb) => Some(AcpEvent::CommandFeedback { level: 复用 to_serde_str 先例, message: fb.message.clone(), channel: to_serde_str(&fb.channel) })；沿用 caps.agent_event 门控（:182 附近，不动现有门控逻辑）
5. ${ACP}/event/variant_coverage_test.rs：穷尽变体列表（:31-60）加 'CommandFeedback'；mapper_test.rs 若有穷尽断言同步

**本任务只建事件链路，绝不写任何发射代码**（发射唯一归属 Phase 5 Step 1 编排层 helper）。
验证：cargo check -p peri-acp-types -p peri-acp && cargo test -p peri-acp-types -p peri-acp。
完成后报告：文件 + 新增变体/分支 + 验证结果。`, { label: 'impl:event' }),
])

const c1Status = c1.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`契约层实施: ${JSON.stringify(c1Status)}`)

// ── 阶段 2：发送侧适配（投影生成 + ui 注册 + on_change 切换） ─────
phase('发送侧适配')

const c2 = await agent(`你在 Perihelion 实施 Phase 3【发送侧适配】。依据：${PLAN3}（步骤 3/4/7，完整读）。前置：契约层已交付 PeriCaps.ui_commands: Vec<UiCommandSpec> + default_ui_commands()、CommandEntryKind、RouteEntry（含 kind/category/args_schema/level()）、CommandRegistry（register/snapshot/set_on_change）、ResolvedCommand；${ACP}/dispatch/commands.rs 现有 available_command_from_entry（pub(crate)，带 #[allow(dead_code)]）与 build_available_commands_update。

任务（4 部分）：
1. ${ACP}/dispatch/commands.rs 改造：
   - 删除 UI_COMMANDS 常量（:10-22）与 build_available_commands（:30-45，compact/loop 自造条目消失）
   - 重写 build_available_commands_update 为注册表投影生成：pub(crate) fn build_available_commands_update(entries: &[RouteEntry], caps: &PeriCaps) -> AvailableCommandsUpdate——每条条目 _meta 注入：periKind（CommandEntryKind serde 值）、periLevel（e.level()）、非空 aliases → periAliases、Some(category) → periCategory、Some(args_schema) → periArgs；AvailableCommand::new(e.fullname.clone(), e.description.clone()).meta(Some(map))
   - update 级 meta：skillNames（仅 core 域 Skill kind 条目名）+ mcpSkillNames（仅 McpSkill 条目名）从 entries 推导，不再接收局部参数；保留镜像 key（Phase 4 退役）
   - 不再有 if caps.ui_commands 附加硬编码（门控反转）
   - available_command_from_entry 若被新函数替代则随之消除；确认零 allow(dead_code) 残留（grep 核对）
2. 发送侧调用点（${ACP}/host/stdio/commands.rs:40 附近 + ${ACP}/host/notify.rs:199-279 + ${ACP}/host/requests.rs:176/383 调用点）统一为：
   - ui 域注册（门控反转核心）：session 初始化（caps 处理之后、on_change 挂载之前）把 caps.ui_commands 明细注册进 session 级 CommandRegistry：RouteEntry { fullname: 'ui:' + name 小写, aliases, description, kind: CommandEntryKind::Panel, category: Some('ui'), args_schema: spec.args, handler: 占位 handler（返回 CommandOutcome::Delegate(format!('ui:{}', name)) 或记录性 handler——Phase 4 落 TUI 本地拦截）, provenance: CommandSource::Ui + CommandLifecycle::Connected }；注册失败（冲突）走注册表纯拒绝（warn 即可）
   - 本地 skill 桥接：skills_port 扫描结果转为 RouteEntry { fullname: 'core:' + name 小写, kind: CommandEntryKind::Skill, aliases: [], description, args_schema: None, handler: 占位（AgentPassthrough 语义——注入指令文本进 agent 管线；若无法构造可先放记录性 handler，Phase 5 迁移时完善）, provenance: core + Discovered }——保持现有广播行为
   - 投影输入 = 注册表 snapshot() 投影 ∪ 本地 skill 桥接条目 → build_available_commands_update(&entries, caps)
   - on_change：stdio/commands.rs:73 与 notify.rs:228 的 McpSkillRegistry on_change 覆盖注册移除；注册表 set_on_change 为唯一触发源（投影重建重发）
   - 时序：ui 注册必须在 set_on_change 挂载之前完成（防双发）
3. 测试面更新：${ACP}/dispatch/commands_test.rs 重写（:26-59 的 2+11=13 条断言）——无 cap → 仅注册表投影条目；all_enabled → 基座 + 11 条默认明细；断言每条 _meta.periKind / periLevel 存在、name = 全名；${ACP}/host/requests_test.rs:1359+ on_change 重发链路用例更新（断言触发源变为注册表、ui 条目随 caps 明细出现/消失）
4. 顺手清理：peri-agent/src/session/exec/bg_test.rs:2 文档注释引用旧名（default_command_registry / find）→ 更新为现状描述

验证：cargo check --workspace && cargo test -p peri-acp -p peri-acp-types -p peri-agent && cargo clippy -p peri-acp -p peri-acp-types --all-targets -- -D warnings。
完成后报告：文件清单 + 关键改动 + 验证结果。`, { label: 'impl:dispatch' })

// ── 阶段 3：整合编译 ─────────────────────────────────────────────
phase('整合与编译验证')

const integration = await agent(`你是 Phase 3 整合 agent。并行实施已完成：peri_caps.rs 升级（Vec<UiCommandSpec> + default_ui_commands + 兼容分支）、事件链路（ExecutorEvent::CommandFeedback + AcpEvent DTO + mapper + event_sink + variant_coverage）、发送侧适配（投影生成 + ui 注册 + on_change 切换 + 测试重写）。

任务：
1. 检查文件改动完整性（git status / git diff --stat 查看改动面）
2. 编译修复：cargo check --workspace 全绿；cargo test -p peri-acp-types -p peri-acp -p peri-agent 全绿；cargo clippy -p peri-acp -p peri-acp-types --all-targets -- -D warnings 全绿
3. 修复并行任务接口漂移（UiCommandSpec 字段名、default_ui_commands 可见性、build_available_commands_update 签名、event_sink 分支匹配）
4. 语义抽查：all_enabled caps 下投影含 11 条 ui: 条目（periKind=panel）+ core:compact（periKind=command）+ core:loop；无 cap 时仅注册表投影；_meta.periLevel 存在
5. 确认遗留清理：grep allow(dead_code) 在 dispatch/commands.rs 零残留；bg_test.rs:2 注释已更新
6. 写实施状态到 ${IMPL}/phase3-integration.md

完成后报告：三绿状态 + 修复问题清单 + 遗留清理确认。`, { label: 'integrate' })

// ── 阶段 4：并行 review ─────────────────────────────────────────
phase('并行 code review')

const REVIEW_BASE = `你是 Phase 3 的 code reviewer。权威依据：${DESIGN}（协议层/Feedback 层）与 ${PLAN3}（步骤 2-7 验收标准）。目标：对照设计与验收标准审查实施质量，输出可执行问题清单。只读不改代码。

输出：问题清单写入 ${IMPL}/phase3-review-<主题>.md：
- 每条：严重度（P0 阻断编译或语义错误 / P1 违背设计或验收标准 / P2 建议）+ 文件:行号 + 问题描述 + 修复建议
- 末尾：无问题项确认列表`

const reviews = await parallel([
  () => agent(`${REVIEW_BASE}

主题：caps。审查 ${TYPES}/peri_caps.rs 的 UiCommandSpec 升级。
核对点：
1. UiCommandSpec 四字段（name/aliases/description/args?）serde camelCase + default
2. from_client_meta 双态兼容：数组解析失败按空 + warn；bool true → default_ui_commands()（等价现状）
3. to_agent_meta 数组序列化
4. all_enabled 填 11 条默认明细（与旧 UI_COMMANDS 数据一致——逐条核对 name/desc 是否原样迁移）
5. UI_COMMANDS 常量已从 dispatch/commands.rs 删除（无残留引用）
6. 门控反转语义（TUI 声明明细 → 注册，非附加硬编码）`, { label: 'review:caps' }),
  () => agent(`${REVIEW_BASE}

主题：dispatch。审查 ${ACP}/dispatch/commands.rs 投影生成 + 发送侧（host/stdio/commands.rs + host/notify.rs + host/requests.rs）。
核对点：
1. build_available_commands_update 投影：_meta 注入 periKind/periLevel（恒有）/periAliases（非空）/periCategory（Some）/periArgs（Some）；name = fullname
2. update 级 skillNames/mcpSkillNames 从 entries 推导（core+Skill / McpSkill kind）保留镜像
3. ui 注册：fullname = ui:name 小写、kind=Panel、provenance=Ui+Connected、handler 占位（Delegate 语义）；时序在 on_change 挂载前（防双发）
4. 本地 skill 桥接：core:* 条目 kind=Skill；与注册表投影合并无重复（同名去重逻辑）
5. on_change 唯一触发源 = 注册表（McpSkillRegistry 覆盖注册移除）
6. 无 cap 时行为：仅注册表投影；all_enabled：基座 + 11 条 ui 明细
7. allow(dead_code) 零残留
8. requests_test.rs on_change 链路用例更新质量`, { label: 'review:dispatch' }),
  () => agent(`${REVIEW_BASE}

主题：event。审查 CommandFeedback 事件链路（${TYPES}/event.rs + ${ACP}/event/mod.rs + event/mapper.rs + session/event_sink.rs + event/variant_coverage_test.rs）。
核对点：
1. ExecutorEvent::CommandFeedback 变体（snake_case tag）
2. AcpEvent DTO 三字段 string 化（level/message/channel）
3. mapper 穷尽 match 分支（无标准 SessionUpdate）
4. event_sink map 分支 + to_serde_str 复用 + agent_event 门控沿用
5. variant_coverage_test 穷尽列表已含
6. **零发射代码**（发射唯一归属 Phase 5 Step 1）——grep 确认没有 emit 调用
7. 与 Phase 1 CommandFeedback 类型的一致性（level/channel 序列化形态）`, { label: 'review:event' }),
])

const reviewStatus = reviews.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`Review 完成: ${JSON.stringify(reviewStatus)}`)

// ── 阶段 5：并行 fix ────────────────────────────────────────────
phase('并行 fix')

const fixes = await parallel([
  () => agent(`你是 Phase 3 fix agent（caps）。读取 ${IMPL}/phase3-review-caps.md 问题清单，逐一修复 ${TYPES}/peri_caps.rs 及测试。P0/P1 必修，P2 视质量修。修复后 cargo check -p peri-acp-types && cargo test -p peri-acp-types 验证。完成后报告：修复条目 + 验证结果`, { label: 'fix:caps' }),
  () => agent(`你是 Phase 3 fix agent（dispatch）。读取 ${IMPL}/phase3-review-dispatch.md 问题清单，逐一修复 dispatch/commands.rs + host/stdio/commands.rs + host/notify.rs + host/requests.rs 及测试。P0/P1 必修。修复后 cargo check --workspace && cargo test -p peri-acp -p peri-acp-types 验证。完成后报告：修复条目 + 验证结果`, { label: 'fix:dispatch' }),
  () => agent(`你是 Phase 3 fix agent（event）。读取 ${IMPL}/phase3-review-event.md 问题清单，逐一修复事件链路文件。P0/P1 必修。修复后 cargo check -p peri-acp -p peri-acp-types && cargo test -p peri-acp 验证。完成后报告：修复条目 + 验证结果`, { label: 'fix:event' }),
])

const fixStatus = fixes.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`Fix 完成: ${JSON.stringify(fixStatus)}`)

// ── 阶段 6：最终验证 ────────────────────────────────────────────
phase('最终验证')

const final = await agent(`你是 Phase 3 最终验证 agent。

任务：
1. 全量验证：cargo check --workspace && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings（至少覆盖 -p peri-acp-types -p peri-acp -p peri-agent -p peri-tui），三绿
2. 验收核对：读 ${PLAN3} 验收标准小节（条目携带 fullname/kind/aliases/category/args/level 明细；ui_commands 为 Vec<UiCommandSpec> 且 UI_COMMANDS 删除门控反转；CommandFeedback 事件链路存在且零发射；TUI 兼容确认）逐条 通过/不通过 + 证据
3. 回归面复核：peri-tui acp_notifier_test（_meta/meta 双写键名测试 :675-706 与 mcpSkillNames 清空语义——应仍绿，TUI 手工解析只读 name/description 忽略未知键）；requests_test.rs McpSkillRegistry 链路（触发源切换后仍绿）；peri-agent bg_test.rs
4. 抽查：_meta 注入逐条（grep periKind 出现处）；ui: 条目注册路径（caps 明细 → RouteEntry → 投影）；on_change 唯一触发源（grep McpSkillRegistry on_change 覆盖注册残留）
5. 遗留清理确认：allow(dead_code) 零残留、bg_test.rs 注释已更新、LoopPlaceholder 仍为计划内占位（Phase 5）
6. 写验证报告到 ${IMPL}/phase3-final.md

完成后报告：三绿状态 + 验收通过数/总条数 + 遗留问题`, { label: 'verify:final' })

return {
  contract: c1Status,
  dispatch: c2 ? 'ok' : 'FAILED',
  integration: integration ? 'ok' : 'FAILED',
  review: reviewStatus,
  fix: fixStatus,
  final: final ? 'ok' : 'FAILED',
}
