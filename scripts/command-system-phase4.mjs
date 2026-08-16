// Phase 4（TUI）实施：并行实施 → 整合 → review → fix → 验证
// 依据：.peri/plans/2026-08-15-command-system-rearch/04-plan-phase4-tui.md
// 设计权威：docs/design/command-system.md
// 前置（Phase 3 交付）：投影 _meta 字段（periKind/periLevel/periArgs 等）、CommandFeedback 事件、caps 升级 + 上送端点

export const meta = {
  name: 'command-system-phase4-implement',
  description: 'Phase 4 TUI 并行实施 + code review fix 流程',
}

const PLAN4 = '.peri/plans/2026-08-15-command-system-rearch/04-plan-phase4-tui.md'
const DESIGN = 'docs/design/command-system.md'
const IMPL = '.peri/plans/2026-08-15-command-system-rearch/.implementation'
const TUI = 'peri-tui/src'

// ── 阶段 1：并行（A 投影 DTO / D ui 单源+上送 / F submit 拦截） ────
phase('阶段 1 并行实施')

const s1 = await parallel([
  () => agent(`你在 Perihelion 实施 Phase 4 TUI【投影 DTO 与结构化 atom】。依据：${PLAN4}（步骤 1，完整读）与 ${DESIGN}（协议层渲染形态）。前置：Phase 3 已交付投影 _meta（periKind/periLevel/periAliases/periCategory/periArgs，wire key 以实际交付为准）。

任务：
1. 新文件 ${TUI}/kit/slash_projection.rs：TUI 手工 serde_json 解析的投影 DTO（不依赖 schema 类型，沿用 acp_notifier 先例）：
   - pub struct SlashCommandEntry { fullname: String, kind: SlashActionKind（复用 ${TUI}/kit/slash_completion.rs 的渲染枚举）, description: String, aliases: Vec<String>, category: Option<String>, args: Option<ArgsSchema>, level: u8 }
   - ArgsSchema 本地镜像（与 Phase 1 peri-acp-types serde 模型对齐）：pub struct ArgsSchema { positionals: Vec<ArgSpec>, named: Vec<NamedArgSpec>, flags: Vec<FlagSpec> }；ArgSpec { name, kind: ArgKind, required: bool, description: Option<String> }；NamedArgSpec 同形；FlagSpec { name, short: Option<String>, description: Option<String> }（**object 数组形态，与 wire 对齐**）；pub enum ArgKind { String, Int, Choice(Vec<String>), Path }
   - 实现 pub fn parse_projection_kind(s: &str) -> Option<SlashActionKind>（'command'→Command / 'skill'→Skill / 'mcp_skill'→McpSkill / 'panel'→Panel，Phase 1 CommandEntryKind snake_case 对应）
   - 实现 pub fn display_name(fullname: &str, level: u8) -> String（level 1 → 最右冒号后段裸名（rsplit_once(':') 先例）；level 2 → 全名原样）
2. ${TUI}/kit/atoms.rs：AVAILABLE_SLASH_COMMANDS: AtomStatic<Vec<(String, String)>>（:433）→ AtomStatic<Vec<SlashCommandEntry>>（import slash_projection 类型）；SKILL_NAMES / MCP_SKILL_NAMES / ACP_COMMANDS（:425-429）**本步不动**（步骤 2 同批删除）
3. 为 DTO 与 display_name 写单元测试（新文件 slash_projection_test.rs 或模块内测试：display level1/level2、parse_projection_kind 全变体+未知回退）

验证：cargo check -p peri-tui && cargo test -p peri-tui slash_projection。
注意：SlashActionKind 现有定义与变体名先读 ${TUI}/kit/slash_completion.rs（:54-62）确认再复用；不要改 slash_completion.rs（其他任务在用）。
完成后报告：文件 + 关键签名 + 测试数。`, { label: 'impl:dto' }),
  () => agent(`你在 Perihelion 实施 Phase 4 TUI【ui 命令单源模块 + 上送注册】。依据：${PLAN4}（步骤 5，完整读）与 ${DESIGN}（ui 域归属 TUI）。前置：Phase 3 已交付 caps.ui_commands: Vec<UiCommandSpec>（peri-acp-types::peri_caps）+ ACP 注册端点。

任务：
1. 新文件 ${TUI}/kit/ui_command.rs：
   - pub enum UiCommandAction { OpenPanel(PanelKind), ToggleSetup }
   - pub struct UiCommandSpec { name: &'static str, aliases: &'static [&'static str], description: &'static str }（本地字面量表，勿直接序列化）
   - pub fn ui_command_specs() -> Vec<UiCommandSpec>：清单 = PANELS（slash_command 非空的条目，读 ${TUI}/kit/panel_registry.rs:172-377 提取）+ /setup + 面板别名（history/resume/his 等，:389-391 迁入）
   - pub fn resolve_ui_command(name: &str) -> Option<UiCommandAction>：裸名 / 'ui:' 前缀 / aliases 归一化查找（最右冒号切分）
2. ${TUI}/kit/panel_registry.rs：panel_for_slash_command（:385-396）降级为纯 PANELS 表查找（别名表迁移至 ui_command.rs）
3. ${TUI}/kit/entry.rs（:340-359 session 创建处）：new_session 成功后上送 ui 明细——读 Phase 3 交付的注册入口（caps session/new meta 携带 uiCommands 明细数组或独立 RPC，以实际交付为准），把 ui_command_specs() 经转换层（&'static → String 字段 + args: None）构造为 peri_acp_types::peri_caps::UiCommandSpec 并调用注册；上送失败 warn 不阻断 session 创建
4. 测试：resolve_ui_command 命中/别名/前缀/未命中；ui_command_specs 清单非空且含 history/setup

验证：cargo check -p peri-tui && cargo test -p peri-tui ui_command。
完成后报告：文件 + 清单条目数 + 注册端点确认。`, { label: 'impl:uicommand' }),
  () => agent(`你在 Perihelion 实施 Phase 4 TUI【submit ui: 域本地拦截】。依据：${PLAN4}（步骤 7，完整读）。前置：ui_command.rs 模块由并行任务创建（pub fn resolve_ui_command / UiCommandAction{OpenPanel, ToggleSetup}）——按其签名使用，若编译时该文件未就绪则先按签名写代码并注明。

任务：${TUI}/kit/submit_request.rs（:56-80 区域）：
1. parse_submit_request 的 panel 分支（:76-80）替换为 ui 域拦截（命中即本地执行，不发 ACP）：command.starts_with('/') 且 resolve_ui_command(&command[1..]) 命中 → OpenPanel(kind) → SubmitRequest::OpenPanel(kind)；ToggleSetup → SubmitRequest::SessionControl(SessionControlRequest::ToggleSetup)
2. 现状硬编码 match（:56-74：clear/setup/rewind/view_action）**保留不动**（/clear 等属 core 域，Phase 5 迁移；双轨保留防退化）
3. 优先级注释（:42-47）更新为「session control / view action / ui 域 / agent text」
4. ${TUI}/kit/submit_request_test.rs：新增 test_parse_submit_request_intercepts_ui_domain（/ui:history → OpenPanel）、test_parse_submit_request_ui_bare_name（/history → OpenPanel）、test_parse_submit_request_ui_setup_explicit（/ui:setup → ToggleSetup）；现有 /compact → AgentText 断言不动

验证：cargo test -p peri-tui submit_request && cargo check -p peri-tui。
完成后报告：改动 + 测试数。`, { label: 'impl:submit' }),
])

const s1Status = s1.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`阶段 1: ${JSON.stringify(s1Status)}`)

// ── 阶段 2：反推删除原子切换（最大块，单 agent） ──────────────────
phase('阶段 2 原子切换')

const s2 = await agent(`你在 Perihelion 实施 Phase 4 TUI【反推删除原子切换】（步骤 2+3 同批，原子发布）。依据：${PLAN4}（步骤 2/3，完整读）。前置：投影 DTO 已交付（slash_projection.rs：SlashCommandEntry / ArgsSchema 镜像 / parse_projection_kind / display_name）；AVAILABLE_SLASH_COMMANDS atom 已改为 Vec<SlashCommandEntry>。

任务：
1. ${TUI}/kit/acp_notifier.rs 投影解析升级（:366-419）：available_commands_update 分支改为每条解析 name（=全名）+ description + _meta（沿用 _meta 优先、meta 兜底先例 :387）的 periKind/periLevel/periAliases/periCategory/periArgs；缺省回退 kind=Command / level=1 / args=None / aliases=[]；**单 atom 原子写**（只写 AVAILABLE_SLASH_COMMANDS 后 refresh_slash_items()）；**删除** meta.skillNames/mcpSkillNames 解析（:384-411）
2. ${TUI}/kit/atoms.rs：删除 SKILL_NAMES（:426）/ MCP_SKILL_NAMES（:429）/ 死代码 ACP_COMMANDS（:425）
3. ${TUI}/kit/slash_completion.rs：SlashCompletionItem（:54-62）扩展为 { label, insert_text, description, kind, label_lowercase, fullname, args: Option<ArgsSchema> }（label 显示形态、insert_text 提交形态 == label）
4. ${TUI}/kit/input_area.rs build_slash_items 重写（:1307-1365）：删除 SKILL_NAMES/MCP_SKILL_NAMES 集合构建（:1309-1320）与 kind 反推分支（:1343-1353）；数据源 = AVAILABLE_SLASH_COMMANDS 投影映射（display_name 变换 label）；**过渡期保留** PANELS 合成（:1321-1334）与 /setup 硬编码（:1336-1342），注释标注「步骤 6 上送落地后删除」
5. 测试同步改写：input_area_test.rs 删除 test_build_slash_items_classifies_mcp_skill_skill_command（:621-644）与 test_build_slash_items_mcp_skill_priority_over_skill（:648-663）；新增 test_build_slash_items_uses_projection_kind_level（预置结构化 atom：mcp:demo:hello kind=McpSkill level=2 / MySkill kind=Skill level=1 / core:compact kind=Command level=1 → label 分别 mcp:demo:hello / MySkill / compact，insert_text == label，kind 正确）与 test_build_slash_items_display_is_lexical（label == insert_text）；acp_notifier_test.rs 新增 test_handle_session_update_parses_projection_fields（fullname/kind/level/args 全字段，含 flags object 数组 FlagSpec 往返）与 test_handle_session_update_projection_missing_meta_defaults（缺省回退）；删除 acp_notifier_test 中锁定 skillNames/mcpSkillNames 的旧断言

验证：cargo test -p peri-tui test_build_slash_items && cargo test -p peri-tui test_handle_session_update && cargo check -p peri-tui && cargo clippy -p peri-tui --all-targets -- -D warnings。
完成后报告：改动文件 + 删除项清单 + 验证结果。`, { label: 'impl:switchover' })

// ── 阶段 3：并行（C 分级渲染收敛 / E 纯投影收口 / G Feedback 消费） ─
phase('阶段 3 并行实施')

const s3 = await parallel([
  () => agent(`你在 Perihelion 实施 Phase 4 TUI【分级渲染与选中行为收敛】。依据：${PLAN4}（步骤 4，完整读）。前置：SlashCompletionItem 已扩展（label/insert_text/fullname/args，由原子切换任务完成）。

任务：
1. ${TUI}/kit/slash_completion.rs：fuzzy 双索引（:88 附近）——匹配串改为 label_lowercase + fullname_lowercase 合并（预计算 search_lowercase 字段），保证 level 2 全名可被 /mcp:demo 前缀搜到；max_label_width（:215-219）不动
2. ${TUI}/kit/input_area.rs on_select（:898-926）收敛：
   - Panel 分支（:900-907）：panel_for_slash_command 反查 → 改为 ui_command::resolve_ui_command(&item.insert_text)（模块已由阶段 1 创建）
   - Command/Skill/McpSkill 分支：删除「再次 panel_for_slash_command 反查」（:913）的语义二义——统一先 resolve_ui_command 命中（如裸名 history）→ 清空输入框 + open_panel；未命中 → apply_slash_selection（:920-922）
3. 测试：input_area_test 相关用例更新（on_select 走 resolve_ui_command）；fuzzy 双索引用例（/mcp:demo 搜到 mcp:demo:hello）

验证：cargo test -p peri-tui input_area && cargo check -p peri-tui。
完成后报告：改动 + 测试数。`, { label: 'impl:select' }),
  () => agent(`你在 Perihelion 实施 Phase 4 TUI【纯投影收口】（步骤 6）。依据：${PLAN4}（步骤 6，完整读）。前置：步骤 5 上送注册已完成（ui_command.rs + entry.rs 上送链路）；反推已删（步骤 2+3）。

任务：${TUI}/kit/input_area.rs build_slash_items：
1. 删除 PANELS 合成（:1321-1334）与 /setup 硬编码（:1336-1342）——补全条目全部由投影生成
2. 双写窗口去重规则（防御）：同 display 名多条时优先保留携带 kind 元数据（非缺省回退）的 ui 域条目
3. 测试：test_build_slash_items 断言纯投影（不预置投影则 history/setup 条目不存在）

验证：cargo test -p peri-tui test_build_slash_items && cargo check -p peri-tui。
完成后报告：改动 + 验证结果。`, { label: 'impl:projection-only' }),
  () => agent(`你在 Perihelion 实施 Phase 4 TUI【CommandFeedback 事件消费】（步骤 8）。依据：${PLAN4}（步骤 8，完整读）。前置：Phase 3 已交付 CommandFeedback 事件链路（SessionUpdate 新 tag，字段 level/message/channel 字符串化——以实际交付的 tag 名为准，先 grep 确认）。

任务：
1. ${TUI}/kit/acp_types.rs：AcpEventData（:1170 附近）新增变体 CommandFeedback(TuiCommandFeedback)；pub struct TuiCommandFeedback { level: FeedbackLevel, message: String, channel: FeedbackChannel }；pub enum FeedbackLevel { Info, Warning, Error }；pub enum FeedbackChannel { UiOnly, Session }
2. ${TUI}/kit/acp_notifier.rs convert_agent_event（AgentEvent 通道分支，:210 附近）：新增 AcpEvent::CommandFeedback 分支（Phase 3 实际交付为 peri/agent_event 通道，无标准 SessionUpdate tag）→ 解析字段 → bridge_tx 发送（走既有 dual-bridge 通路）
3. acp_events：新 handler（acp_events/command_feedback.rs 或并入 system.rs，以现有结构为准）→ BridgeState::inject_system_note(message, TuiNoteLevel 映射 Info→Info/Warning→Warning/Error→Error)；v1 不建独立通知条组件，UiOnly/Session 均走 inject_system_note（SystemNote 是 TUI 渲染层概念，不进 ACP 消息）
4. 测试：acp_notifier_test.rs 新增 test_agent_event_parses_command_feedback（AgentEvent 通道解析 AcpEvent::CommandFeedback 字段）；handler 侧测试（inject 后 current_turn 含 SystemNote，参照 system.rs 既有测试模式）

验证：cargo test -p peri-tui test_agent_event_parses_command_feedback && cargo check -p peri-tui。
完成后报告：改动 + 测试数。`, { label: 'impl:feedback' }),
])

const s3Status = s3.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`阶段 3: ${JSON.stringify(s3Status)}`)

// ── 阶段 4：整合 ─────────────────────────────────────────────────
phase('整合与编译验证')

const integration = await agent(`你是 Phase 4 整合 agent。并行实施已完成：投影 DTO（slash_projection.rs）、ui_command 单源+上送（ui_command.rs/panel_registry.rs/entry.rs）、submit 拦截（submit_request.rs）、反推删除原子切换（acp_notifier/atoms/slash_completion/input_area + 测试）、分级渲染收敛、纯投影收口、CommandFeedback 消费（acp_types/acp_events）。

任务：
1. git diff --stat 查看改动面完整性
2. 编译修复：cargo check --workspace && cargo test -p peri-tui && cargo clippy -p peri-tui --all-targets -- -D warnings 全绿；修复并行接口漂移（SlashCommandEntry 字段、resolve_ui_command 签名、SlashCompletionItem 字段、CommandFeedback tag 名）
3. 语义抽查：build_slash_items 无 SKILL_NAMES/MCP_SKILL_NAMES 引用（grep 零命中）；AVAILABLE_SLASH_COMMANDS 单 atom；submit_request ui 拦截测试绿；display_name level 规则
4. 清理：确认无死代码（ACP_COMMANDS 已删）、无 mcp__ 展示用例残留（input_area_test / acp_notifier_test 中 mcp__ 断言应为零）
5. 写实施状态到 ${IMPL}/phase4-integration.md

完成后报告：三绿状态 + 修复问题清单 + 清理确认。`, { label: 'integrate' })

// ── 阶段 5：并行 review ─────────────────────────────────────────
phase('并行 code review')

const REVIEW_BASE = `你是 Phase 4 的 code reviewer。权威依据：${DESIGN}（词法渲染形态 / ui 域归属 / Feedback 层）与 ${PLAN4}（步骤 1-8 验收标准）。目标：对照设计与验收标准审查实施质量，输出可执行问题清单。只读不改代码。

输出：问题清单写入 ${IMPL}/phase4-review-<主题>.md：
- 每条：严重度（P0 阻断编译或语义错误 / P1 违背设计或验收标准 / P2 建议）+ 文件:行号 + 问题描述 + 修复建议
- 末尾：无问题项确认列表`

const reviews = await parallel([
  () => agent(`${REVIEW_BASE}

主题：projection。审查 ${TUI}/kit/slash_projection.rs + acp_notifier.rs 投影解析 + atoms.rs。
核对点：
1. SlashCommandEntry 字段与 wire _meta 映射（periKind/periLevel/periAliases/periCategory/periArgs）一一对应；缺省回退（kind=Command/level=1/args=None）
2. ArgsSchema 镜像与 Phase 1 serde 模型对齐（FlagSpec object 数组）
3. 单 atom 原子写（只写 AVAILABLE_SLASH_COMMANDS）+ refresh_slash_items
4. display_name：level 1 裸名（rsplit_once 最右冒号）/ level 2 全名
5. SKILL_NAMES / MCP_SKILL_NAMES / ACP_COMMANDS 零残留（grep）
6. build_slash_items 无反推（kind 来自投影）`, { label: 'review:projection' }),
  () => agent(`${REVIEW_BASE}

主题：uidomain。审查 ${TUI}/kit/ui_command.rs + panel_registry.rs + entry.rs + submit_request.rs。
核对点：
1. ui_command_specs 清单完整性（PANELS 非空 slash_command 条目 + setup + 别名迁移）
2. resolve_ui_command：裸名 / ui: 前缀 / aliases 归一化；与 submit 拦截一致性
3. entry.rs 上送：转换层（&'static → String）+ 失败 warn 不阻断
4. submit_request 拦截：命中即本地执行不发 ACP；硬编码 match 保留（clear/setup/rewind/view_action 双轨）
5. on_select 收敛：统一 resolve_ui_command 先查，未命中 apply_slash_selection
6. 测试覆盖（拦截三用例 + resolve 用例）`, { label: 'review:uidomain' }),
  () => agent(`${REVIEW_BASE}

主题：feedback。审查 CommandFeedback 消费链路（${TUI}/kit/acp_types.rs + acp_notifier.rs tag 分支 + acp_events handler）。
核对点：
1. AcpEventData 变体与 Phase 3 事件契约 tag/字段对齐（level/message/channel 字符串化解析）
2. FeedbackLevel/FeedbackChannel 映射完整（Info/Warning/Error；UiOnly/Session）
3. inject_system_note 映射正确（不进 ACP 消息、agent 永不见）
4. bridge_tx 既有通路复用（dual-bridge）
5. 测试覆盖（解析用例 + handler inject 用例）
6. v1 无独立通知条组件（符合 plan 约定）`, { label: 'review:feedback' }),
])

const reviewStatus = reviews.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`Review 完成: ${JSON.stringify(reviewStatus)}`)

// ── 阶段 6：并行 fix ────────────────────────────────────────────
phase('并行 fix')

const fixes = await parallel([
  () => agent(`你是 Phase 4 fix agent（projection）。读取 ${IMPL}/phase4-review-projection.md 问题清单，逐一修复 slash_projection.rs / acp_notifier.rs / atoms.rs / slash_completion.rs / input_area.rs 相关。P0/P1 必修。修复后 cargo test -p peri-tui && cargo check -p peri-tui 验证。完成后报告：修复条目 + 验证结果`, { label: 'fix:projection' }),
  () => agent(`你是 Phase 4 fix agent（uidomain）。读取 ${IMPL}/phase4-review-uidomain.md 问题清单，逐一修复 ui_command.rs / panel_registry.rs / entry.rs / submit_request.rs。P0/P1 必修。修复后 cargo test -p peri-tui submit_request input_area ui_command && cargo check -p peri-tui 验证。完成后报告：修复条目 + 验证结果`, { label: 'fix:uidomain' }),
  () => agent(`你是 Phase 4 fix agent（feedback）。读取 ${IMPL}/phase4-review-feedback.md 问题清单，逐一修复 acp_types.rs / acp_notifier.rs / acp_events。P0/P1 必修。修复后 cargo test -p peri-tui test_handle_session_update_parses_command_feedback && cargo check -p peri-tui 验证。完成后报告：修复条目 + 验证结果`, { label: 'fix:feedback' }),
])

const fixStatus = fixes.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`Fix 完成: ${JSON.stringify(fixStatus)}`)

// ── 阶段 7：最终验证 ────────────────────────────────────────────
phase('最终验证')

const final = await agent(`你是 Phase 4 最终验证 agent。

任务：
1. 全量验证：cargo check --workspace && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings，三绿
2. 验收核对：读 ${PLAN4} 验收标准小节（补全条目全部由投影生成（无本地合成/无反推）；level 分级渲染 display 即 lexical；ui 域命令上送注册 + 本地拦截；CommandFeedback 事件消费）逐条 通过/不通过 + 证据
3. 回归面：cargo test -p peri-tui 全量（input_area / slash_completion / acp_notifier / submit_request / acp_events 既有用例）
4. 清理确认：grep SKILL_NAMES / MCP_SKILL_NAMES / ACP_COMMANDS 在 peri-tui 零命中；mcp__ 展示用例零残留
5. 写验证报告到 ${IMPL}/phase4-final.md

完成后报告：三绿状态 + 验收通过数/总条数 + 遗留问题`, { label: 'verify:final' })

return {
  stage1: s1Status,
  stage2: s2 ? 'ok' : 'FAILED',
  stage3: s3Status,
  integration: integration ? 'ok' : 'FAILED',
  review: reviewStatus,
  fix: fixStatus,
  final: final ? 'ok' : 'FAILED',
}
