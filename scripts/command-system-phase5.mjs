// Phase 5（命令迁移）实施：并行命令迁移 → compact+rewind 连锁 → 收尾切换 → TUI 联动 → 清理回归 → review → fix → 验证
// 依据：.peri/plans/2026-08-15-command-system-rearch/05-plan-phase5-migration.md
// 设计权威：docs/design/command-system.md

export const meta = {
  name: 'command-system-phase5-implement',
  description: 'Phase 5 命令迁移并行实施 + code review fix 流程',
}

const PLAN5 = '.peri/plans/2026-08-15-command-system-rearch/05-plan-phase5-migration.md'
const DESIGN = 'docs/design/command-system.md'
const IMPL = '.peri/plans/2026-08-15-command-system-rearch/.implementation'
const TYPES = 'peri-acp-types/src'
const ACP = 'peri-acp/src'
const AGENT = 'peri-agent/src'
const TUI = 'peri-tui/src'

// ── 阶段 1：并行实施（编排层反馈接线 / bg / clear / loop） ────────
phase('并行实施：编排层接线 + bg + clear + loop')

const c1 = await parallel([
  () => agent(`你在 Perihelion 实施 Phase 5 Step 1【编排层反馈接线】。依据：${PLAN5}（Step 1，完整读）+ ${DESIGN}（Feedback 层）。
前置（Phase 3 已交付）：ExecutorEvent::CommandFeedback 变体（在 ${TYPES}/event.rs，**以实际形态为准——tuple 或 struct 形态照现状使用**）、AcpEvent::CommandFeedback DTO、CommandFeedback { level, message, channel }（Phase 1）、CommandResult.feedback。

任务（2 处，**dispatch/rewind.rs 的接线明确不在本任务**——归 compact+rewind 迁移组）：
1. ${AGENT}/session/exec/executor_helpers.rs：新增 pub(crate) async fn emit_command_feedback(sink: &Arc<dyn EventSink>, session_id: &str, result: &mut CommandResult)——take 出 result.feedback，push_event ExecutorEvent::CommandFeedback（用实际变体形态构造），channel == Session 时把 message 以 BaseMessage::system 追加进 result.messages；在 intercept_immediate_command 的 handler.execute 之后、push_done 之前调用。函数须可被 peri-acp 复用（pub 可见性 + 经 peri-agent 模块路径可达；若模块链不可达则在本文件提供 pub 且 peri-acp 侧经 re-export——自行选择 plan 允许的暴露方式并记录）。
2. ${ACP}/dispatch/execute_command.rs：RPC 路径 select 之后、push_done 之前同样接线（复用同一 helper）。
3. 测试：executor_helpers_test 新增用例——feedback=Session 时 result.messages 尾元素为系统消息；UiOnly 时 messages 不变、事件仍发射。

验证：cargo check -p peri-agent -p peri-acp && cargo test -p peri-agent executor_helpers_test && cargo test -p peri-acp execute_command_test。
完成后报告：文件 + helper 签名 + 暴露方式 + 验证结果。`, { label: 'impl:orchestrator' }),
  () => agent(`你在 Perihelion 实施 Phase 5 Step 2【bg 命令迁移】。依据：${PLAN5}（Step 2，完整读）。
前置：Phase 1 CommandHandler/CommandOutcome/CommandFeedback；Phase 2 CommandContext 拆层（ctx.dep::<Arc<dyn BgForkSpawner>>()，P1-9 形态）。

任务：
1. ${AGENT}/session/exec/bg.rs：BgCommand 实现 CommandHandler（新增 impl；旧 AgentCommand impl 保留为转发过渡：match CommandHandler::execute → Done(r) => r，unreachable! 其他）——free-form prompt 走 ctx.args.trim()（不声明 ArgsSchema），空参 → Done + feedback(Info, '用法: /bg <任务描述>', UiOnly) + 原样 history；spawner 缺失 / spawn_fork Err → feedback(Error, msg, UiOnly)；成功 → feedback(Info, '◆ 后台任务已启动: …' 前 80 字符, UiOnly)。依赖经 ctx.dep::<Arc<dyn BgForkSpawner>>() 取，缺失优雅报错。
2. 删除 ${AGENT}/session/exec/events.rs:19-59 的三个 emit 函数（emit_bg_usage_hint / emit_bg_spawn_error / emit_bg_confirmation）及 bg.rs 中调用；ExecutorEvent::TextChunk 变体本体保留。
3. 测试：bg_test.rs TextChunk 断言 → feedback 字段断言；push_done 归执行器断言保持（test_bg_command_does_not_call_push_done_itself 不破坏）。

验证：cargo check -p peri-agent && cargo test -p peri-agent bg && cargo test -p peri-tui acp_events（若 peri-tui 可编译）。
完成后报告：文件 + 迁移形态 + 验证结果。`, { label: 'impl:bg' }),
  () => agent(`你在 Perihelion 实施 Phase 5 Step 3【clear 命令迁移】。依据：${PLAN5}（Step 3，完整读）。
前置：Phase 1 CommandHandler/CommandOutcome/CommandFeedback。

任务：
1. ${ACP}/session/command/clear.rs：ClearCommand 实现 CommandHandler——Done(CommandResult { messages: Vec::new(), stop_reason: EndTurn, feedback: Some(Info, '对话已清空', UiOnly) })；无参 ArgsSchema::default()；**删除 :37-63 的 CompactCompleted 占位发射**（20 字段全占位那个）。
2. 旧 AgentCommand impl 保留为转发过渡（同 bg 模式）。
3. 测试：mod_test.rs clear 注册/执行/push_done 断言核对；execute_command_test RPC 路径返回空 messages + 不再产生 CompactCompleted 事件断言更新。

**约束**：TUI 本地 /clear 拦截（submit_consumer.rs）保持不动；ACP 侧 ClearCommand 只服务 RPC 路径。
验证：cargo check -p peri-acp && cargo test -p peri-acp mod_test execute_command_test。
完成后报告：文件 + 迁移形态 + 验证结果。`, { label: 'impl:clear' }),
  () => agent(`你在 Perihelion 实施 Phase 5 Step 5.5【loop 命令迁移】。依据：${PLAN5}（Step 5.5，完整读）。
前置：Phase 2 已预注册 core:loop（占位 handler LoopPlaceholder，${ACP}/session/command/mod.rs）。

任务（产品未裁决执行语义 → **按 plan 二选一的默认：保留占位语义**——确定性执行 + UI-only 反馈，不退役）：
1. 核对 LoopPlaceholder 行为：resolve('/loop') 命中 → 执行返回 Done + feedback(Info, 'loop 命令尚未实现', UiOnly)，杜绝静默 fall through。
2. 若占位 handler 已满足该语义，仅补测试锁定：${ACP}/session/command/mod_test.rs 新增 /loop resolve 命中断言 + 执行返回 Done 断言。
3. 确认注册路径（register_builtins 已含 core:loop）无需改动。

验证：cargo check -p peri-acp && cargo test -p peri-acp mod_test。
完成后报告：核对结论 + 测试 + 验证结果。`, { label: 'impl:loop' }),
])

const c1Status = c1.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`并行实施: ${JSON.stringify(c1Status)}`)

// ── 阶段 2：compact + rewind 迁移（连锁面重叠，串行单 agent） ─────
phase('compact + rewind 迁移（事件收敛连锁）')

const c2 = await agent(`你在 Perihelion 实施 Phase 5 Step 4 + Step 5【compact 与 rewind 命令迁移】（连锁面高度重叠，合并在你一个任务）。依据：${PLAN5}（Step 4/5，完整读）。前置：编排层 emit_command_feedback 已就位（executor_helpers.rs pub 函数，peri-acp 侧经 re-export 可用——先 grep 确认其暴露路径）；bg/clear/loop 迁移已完成（不触碰 bg.rs/clear.rs）。

【Step 4 compact】：
1. ${ACP}/session/command/compact.rs shim 保持，CompactCommand 实现 CommandHandler（旧 AgentCommand 转发过渡同 bg）；无参 ArgsSchema::default()。
2. ${AGENT}/session/exec/compact_pipeline.rs：CompactError 10 处发射收敛——各阶段失败不再直接 emit_compact_error，改返回结构化失败，execute_compact 最外层统一映射 Done + feedback(Error, message, UiOnly) + 原样 history；完成 → feedback(Info, '已压缩 N 条消息' 文案, UiOnly)。CompactStarted 保留（阶段信号）。
3. 事件收敛（桥接 R1/R3，5 处连锁同步）：${TYPES}/event.rs CompactCompleted 20 字段 → 收敛为 { summary, messages, trigger }（trigger 字段必须保留，TUI compact_just_completed 标志链依赖）+ CompactError 变体删除；${ACP}/session/event_sink.rs map 分支同步（删 files/skills/micro_cleared/strategy/outcome 映射，仅留 summary/messages_json/trigger + CompactError 分支删除）；${ACP}/event/mod.rs AcpEvent::CompactCompleted 字段收敛 + CompactError 删除；${TUI}/kit/acp_types.rs + acp_notifier.rs 转换适配；${ACP} 侧 langfuse bridge 在 peri-controller/src/langfuse/bridge.rs:262 同步适配（改读 summary/messages，删除被删字段解构）。

【Step 5 rewind】：
4. ${ACP}/session/command/rewind.rs：RewindCommand 实现 CommandHandler；参数由 RewindArgs（serde_json）迁入 ArgsSchema——positionals: [target_message_id: String required], flags: [FlagSpec { name: 'no-revert-files', short: None, description: None }]（与 Phase 1 FlagSpec 形态一致），named 空；共享执行体 pub(crate) async fn execute_rewind(ctx, target_message_id, revert_files) -> CommandResult（原 execute Step 2-6 逻辑迁入，错误分支改 feedback：解析失败/未找到目标/持久化删除失败 → feedback(Error, UiOnly)；成功 summary → feedback(Info, UiOnly)）；slash 专用 RewindArgs（:27-38）删除。
5. ${ACP}/dispatch/rewind.rs：RPC 前置校验（serde RewindArgs + preview_fingerprint :24-40）保留；:278-299 不再构造 CommandContext.args JSON，改调共享 execute_rewind；**在编排层接线 emit_command_feedback**（select 之后、push_done 之前，本文件即 plan Step 1 的第三处接线点）；RPC wire 形态**零变化**（红线）。
6. 事件：RewindCompleted { summary, messages } 保留原样（TUI 重建 + acp-hub + langfuse 依赖）；RewindError 变体删除（${TYPES}/event.rs + event_sink map + ${ACP}/event/mod.rs + TUI 分支，Step 7 联动）。
7. 测试：rewind_test.rs 解析断言从 serde_json 改 ArgsSchema 形态 + feedback 断言 + RewindCompleted 仍发射；dispatch/rewind_test.rs wire 不变回归；e2e scenarios/rewind-v2.test.ts 断言核对（cd e2e && npm test -- scenarios/rewind-v2.test.ts，环境不可用则记录）。

验证：cargo check --workspace && cargo test -p peri-acp-types -p peri-acp -p peri-agent -p peri-controller && cargo clippy -p peri-acp-types -p peri-acp --all-targets -- -D warnings。
完成后报告：连锁清单（每处文件:改动）+ RPC wire 不变确认 + 验证结果。`, { label: 'impl:compact-rewind' })

// ── 阶段 3：收尾大切换（Step 6） ─────────────────────────────────
phase('执行器与注册表切换（Step 6）')

const c3 = await agent(`你在 Perihelion 实施 Phase 5 Step 6【执行器与注册表切换】（收尾大切换）。依据：${PLAN5}（Step 6，完整读）。前置：Step 2-5 四命令均已提供 CommandHandler impl（旧 AgentCommand 转发过渡存在）；emit_command_feedback 已接线；Phase 2 注册表（CommandRegistry resolve → ResolvedCommand）+ CommandContext 拆层已就位。

任务（4 部分）：
1. ${AGENT}/session/exec/executor_helpers.rs 拦截点消费 ResolvedCommand + CommandOutcome：
   - 拦截处（现状 :314-383 附近）已用 resolved.entry.handler.execute(ctx) 且匹配 CommandOutcome（Phase 2 已改造）——核对并补齐：**args 解析**——构造 CommandContext 前消费 resolved.args（注册表统一切分），并调用 resolved.entry.args_schema.parse(&resolved.args)（Phase 1 解析器，**以实际交付 API 为准**），失败 → 立即返回 Done(CommandResult { messages: history 原样, stop_reason: EndTurn, feedback: Some(Error(解析失败)) })（不进入 handler）；
   - Inject/Delegate 分支（Phase 2 已 fall-through + warn）→ 升级为 InterceptOutcome 三态（Handled(PromptResult) / Inject(String) / PassThrough）——按 plan Step 6 代码形态（Done → emit_command_feedback → push_done → Handled；Inject(s) → 不 push_done，回传 Inject；未命中 / 词法非法 → PassThrough）。
2. ${AGENT}/session/exec/executor.rs:857-906 调用点三态分发：Handled(r) → return Some(r)；Inject(text) → AgentInput::blocks(text) 进 agent 管线；PassThrough → 现状 AgentInput::blocks(content)。
3. ${ACP}/dispatch/execute_command.rs：kind() 检查改 RouteEntry 域/等级检查（Immediate 语义 = core/ui 域第一等级）；handler 调用 + emit_command_feedback 接线核对（Step 1 已接）；消费 resolved.entry 与 resolved.args。
4. 删除（收尾）：${TYPES}/command.rs 的 AgentCommand（:98-113）/ CommandKind（:38-47）删除（CommandKind::Passthrough/Transform 无实现，被 Outcome Inject/Delegate 取代——**删除前 grep 确认全仓引用清单**，TUI/middlewares 若有引用同步收敛）；四个命令的旧 AgentCommand impl 与转发删除；find/find_arc 残留核对零引用。
5. 测试：executor_helpers_test / executor_test（fall-through / push_done / cancel / Inject 分发用例）；execute_command_test / mod_test。

验证：cargo check --workspace && cargo test -p peri-agent executor && cargo test -p peri-acp execute_command_test mod_test && cargo clippy --workspace --all-targets -- -D warnings。
完成后报告：删除清单（每项 grep 证据）+ 三态分发形态 + 验证结果。`, { label: 'impl:switch' })

// ── 阶段 4：TUI 联动（Step 7） ───────────────────────────────────
phase('TUI 联动（Step 7）')

const c4 = await agent(`你在 Perihelion 实施 Phase 5 Step 7【TUI 联动——重建信号消费收敛，文案渲染移交 CommandFeedback】。依据：${PLAN5}（Step 7，完整读）。前置：Step 4/5 事件收敛已交付（CompactCompleted 三字段、CompactError/RewindError 变体已删）；Phase 4 已交付 CommandFeedback 渲染（TUI 通知条/SystemNote 通路）。

任务（全部修改）：
1. ${TUI}/kit/acp_events/compact.rs：handle_compact_completed 收敛为仅保留重建语义——trigger=='manual' 置 compact_just_completed + 消息区重建所需逻辑；**删除** inject_system_note 文案注入（:102 附近）与 PENDING_COMPACT_NOTE 重放复活（:104-109）；handle_compact_error 删除（CompactError 变体已删）。
2. ${TUI}/kit/acp_events/system.rs：handle_rewind_completed 保留 committed 重建 + 输入框回填 + 弹窗关闭，删除 SystemNote 文案注入（若有）；handle_rewind_error 删除（RewindError 变体已删）。
3. ${TUI}/kit/acp_types.rs / acp_notifier.rs：CompactCompleted 收敛字段的转换适配（Step 4 连锁之一，若未随 Step 4 完成则补齐）；CommandFeedback 渲染（Phase 4 交付）承接全部文案。
4. 测试：acp_events 测试更新——compact_just_completed 标志链断言保持、rewind 重建断言保持、文案断言改 CommandFeedback 分支；删除引用已删变体的测试用例。

验证：cargo check --workspace && cargo test -p peri-tui acp_events && cargo clippy -p peri-tui --all-targets -- -D warnings。
完成后报告：文件清单 + 删/留对照 + 验证结果。`, { label: 'impl:tui' })

// ── 阶段 5：mcp__ 清理与 fall through 回归（Step 8） ─────────────
phase('mcp__ 残留清理与 fall through 回归（Step 8）')

const c5 = await agent(`你在 Perihelion 实施 Phase 5 Step 8【mcp__ 残留清理 + fall through 回归 + 全量验证】。依据：${PLAN5}（Step 8，完整读）。前置：Phase 4 已移除 TUI 反推与 mcp__ 补全形态；词法层 mcp__ 解析即失败（Phase 1）。

任务：
1. mcp__ 残留清理：grep -rn 'mcp__' --include='*.rs' . 全仓扫描——测试 fixture 与断言（已知 ${TUI}/kit/input_area_test.rs / acp_notifier_test.rs 等处）改为新形态（mcp:demo:hello）或删除；生产路径残留（若有）删除。**注意**：mcp_skills.rs 的 mcp_skill_name（tool 面）属 Phase 6 范围外，确认其非 mcp__ 形态即可，勿误删。
2. fall through 回归测试：${AGENT}/session/exec/executor_helpers_test.rs 新增——'/etc/hosts' 与 'mcp__demo__hello' 输入均返回 PassThrough（未命中 → agent 管线，不产生错误事件、不硬报错）；execute_command_test.rs 同步核对（RPC unknown command 仍返回 AcpError，行为保持）。
3. 全量验证：cargo check --workspace && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings；e2e 重点场景（cd e2e && npm test，环境可用则跑，不可用记录）。

完成后报告：清理清单（每处文件:行） + 回归用例 + 三绿状态。`, { label: 'impl:cleanup' })

// ── 阶段 6：并行 review ─────────────────────────────────────────
phase('并行 code review')

const REVIEW_BASE = `你是 Phase 5 的 code reviewer。权威依据：${DESIGN}（Execution/Feedback/权威词法章节）与 ${PLAN5}（Step 1-8 + 验收标准 :348-353）。目标：对照设计与验收标准审查实施质量，输出可执行问题清单。只读不改代码。

输出：问题清单写入 ${IMPL}/phase5-review-<主题>.md：
- 每条：严重度（P0 阻断编译或语义错误 / P1 违背设计或验收标准 / P2 建议）+ 文件:行号 + 问题描述 + 修复建议
- 末尾：无问题项确认列表`

const reviews = await parallel([
  () => agent(`${REVIEW_BASE}

主题：commands。审查 bg/clear/loop 迁移（${AGENT}/session/exec/bg.rs + events.rs、${ACP}/session/command/clear.rs、mod.rs LoopPlaceholder）。
核对点：
1. 三命令均以 CommandHandler 实现注册（RouteEntry 扁平字段），执行返回 CommandOutcome 三态
2. bg 三处 TextChunk 伪消息已删除（events.rs:19-59），反馈全部收敛 feedback（UiOnly，不进会话）
3. clear 的 CompactCompleted 占位发射已删除；messages 空语义保持；RPC 路径行为正确
4. loop 占位语义（确定性执行 + UI-only 反馈）与测试锁定
5. 各命令 ArgsSchema 声明正确（bg free-form 不声明参数）
6. 旧 AgentCommand 转发过渡形态正确（Step 6 会删除，无残留隐患）`, { label: 'review:commands' }),
  () => agent(`${REVIEW_BASE}

主题：compact-rewind。审查 Step 4/5（${TYPES}/event.rs + event_sink.rs + ${ACP}/event/mod.rs + compact_pipeline.rs + rewind.rs + dispatch/rewind.rs + peri-controller langfuse bridge + TUI acp_types/acp_notifier 连锁）。
核对点：
1. CompactCompleted 收敛为 { summary, messages, trigger }（trigger 保留，R3 标志链）且 5 处连锁同步（event.rs / event_sink / AcpEvent / TUI 两文件 / langfuse）
2. CompactError / RewindError 变体删除后零残留引用（grep）
3. CompactError 10 处发射收敛：各阶段失败不再直接发射，最外层统一映射 feedback(Error, UiOnly) + 原样 history
4. RewindCompleted 保留原样（TUI 重建 / acp-hub / langfuse 依赖）
5. rewind ArgsSchema 形态（positionals target_message_id required + FlagSpec no-revert-files）与 Phase 1 一致；slash 专用 RewindArgs 删除
6. **RPC wire 零变化**（dispatch/rewind.rs 前置校验 + preview_fingerprint 保留、wire 形态不变）
7. emit_command_feedback 接线正确（execute 之后、push_done 之前，无双发）`, { label: 'review:compact-rewind' }),
  () => agent(`${REVIEW_BASE}

主题：executor-tui。审查 Step 1/6/7（${AGENT}/session/exec/executor_helpers.rs + executor.rs + ${ACP}/dispatch/execute_command.rs + ${TUI}/kit/acp_events/compact.rs + system.rs）。
核对点：
1. emit_command_feedback：take 后发射 + Session 通道写系统消息（BaseMessage::system）；UiOnly 不写 messages；时序在 push_done 之前
2. Step 6 三态分发（Handled/Inject/PassThrough）正确：Done → feedback → push_done → Handled；Inject 不 push_done；未命中/词法非法 → PassThrough 不报错
3. args_schema.parse 失败 → 立即返回 Done + feedback(Error)（不进入 handler），rewind 语义泛化
4. AgentCommand / CommandKind 删除后全仓零残留（grep 证据）
5. Step 7：compact.rs 只保留重建语义（标志链 compact_just_completed 依赖 trigger=='manual'）；system.rs 保留 rewind 重建 + 删除文案注入；handle_compact_error/handle_rewind_error 删除后零残留
6. fall through 回归：/etc/hosts 与 mcp__ 形态不产生错误事件、不硬报错`, { label: 'review:executor-tui' }),
])

const reviewStatus = reviews.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`Review 完成: ${JSON.stringify(reviewStatus)}`)

// ── 阶段 7：并行 fix ────────────────────────────────────────────
phase('并行 fix')

const fixes = await parallel([
  () => agent(`你是 Phase 5 fix agent（commands）。读取 ${IMPL}/phase5-review-commands.md 问题清单，逐一修复 bg/clear/loop 相关文件及测试。P0/P1 必修，P2 视质量修。修复后 cargo check -p peri-agent -p peri-acp && cargo test -p peri-agent bg && cargo test -p peri-acp mod_test 验证。完成后报告：修复条目 + 验证结果`, { label: 'fix:commands' }),
  () => agent(`你是 Phase 5 fix agent（compact-rewind）。读取 ${IMPL}/phase5-review-compact-rewind.md 问题清单，逐一修复事件收敛连锁文件。P0/P1 必修。修复后 cargo check --workspace && cargo test -p peri-acp-types -p peri-acp -p peri-agent -p peri-controller 验证。完成后报告：修复条目 + 验证结果`, { label: 'fix:compact-rewind' }),
  () => agent(`你是 Phase 5 fix agent（executor-tui）。读取 ${IMPL}/phase5-review-executor-tui.md 问题清单，逐一修复编排层与 TUI 联动文件。P0/P1 必修。修复后 cargo check --workspace && cargo test -p peri-agent executor && cargo test -p peri-tui acp_events 验证。完成后报告：修复条目 + 验证结果`, { label: 'fix:executor-tui' }),
])

const fixStatus = fixes.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`Fix 完成: ${JSON.stringify(fixStatus)}`)

// ── 阶段 8：最终验证 ────────────────────────────────────────────
phase('最终验证')

const final = await agent(`你是 Phase 5 最终验证 agent。

任务：
1. 全量验证：cargo check --workspace && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings，三绿
2. 验收核对：读 ${PLAN5} 验收标准小节（:348-353，5 条）逐条 通过/不通过 + 证据：
   - compact/bg/clear/rewind 均以 CommandHandler 实现注册，执行返回 CommandOutcome 三态，不再各自自研参数解析
   - 各命令参数由 ArgsSchema 声明，旧自研解析删除
   - 反馈性自造事件删除（clear 占位 CompactCompleted 发射 / CompactError / RewindError / bg TextChunk 伪消息），反馈统一 CommandFeedback 双通道；CompactCompleted（summary/messages/trigger）与 RewindCompleted 保留为重建信号
   - 未解析 /xxx fall through 进管线，无错误事件
   - mcp__ 在解析与补全中完全移除
3. 回归面复核：TUI 本地拦截双路径（submit_consumer /clear /rewind）未被触碰；RPC wire（rewind preview_fingerprint）不变；e2e 断言更新到位（或记录未跑原因）
4. 遗留清理确认：AgentCommand / CommandKind 零残留；LoopPlaceholder 已按占位语义定案；allow(dead_code) 无新增
5. 写验证报告到 ${IMPL}/phase5-final.md

完成后报告：三绿状态 + 验收通过数/总条数 + 遗留问题`, { label: 'verify:final' })

return {
  parallel: c1Status,
  compactRewind: c2 ? 'ok' : 'FAILED',
  switch: c3 ? 'ok' : 'FAILED',
  tui: c4 ? 'ok' : 'FAILED',
  cleanup: c5 ? 'ok' : 'FAILED',
  review: reviewStatus,
  fix: fixStatus,
  final: final ? 'ok' : 'FAILED',
}
