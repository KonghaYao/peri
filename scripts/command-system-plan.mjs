// command 系统重设计：调查现状 → 撰写分阶段实施计划
// 依赖 plan 骨架：.peri/plans/2026-08-15-command-system-rearch/（已由 subagent 创建）

export const meta = {
  name: 'command-system-plan-write',
  description: '调查 command 系统现状并撰写分阶段实施计划',
}

const PLAN_DIR = '.peri/plans/2026-08-15-command-system-rearch'
const INV = `${PLAN_DIR}/.investigation`
const DESIGN = 'docs/design/command-system.md'

// ── 调查阶段：4 路并行，各自写证据文件 ─────────────────────────────
phase('调查现状')

const INV_REG = `你在 Perihelion 仓库调查 Command 系统【注册表与执行】现状，为实施计划提供证据。只读，不改任何文件。

必读：
- ${DESIGN}（目标架构）
- peri-acp/src/session/command/mod.rs（CommandRegistry / default_prompt_command_registry）
- peri-acp-types/src/command.rs（AgentCommand / CommandContext / CommandResult / BgForkRequest）
- peri-agent/src/session/exec/executor_helpers.rs（intercept_immediate_command / InterceptRequest / push_done）
- peri-agent/src/session/exec/executor.rs（拦截调用点约 857 行）

调查问题：
1. 注册表数据结构与查找：find / find_arc 是否双份实现？前缀匹配语义？alias 处理？
2. CommandContext 全部字段清单：哪些该进 core 常驻、哪些该改为 ctx.dep::<dyn Trait>() 注入
3. Immediate 命令执行路径完整链：拦截 → 执行 → push_done → PromptResult
4. 未知命令 / 非 Immediate 的 fall-through 证据（intercept 返回 None 后文本如何进 agent 管线）
5. 四个内置命令（bg/compact/clear/rewind）的注册点与 execute 签名形态

输出：把证据报告写入 ${INV}/01-registry.md（Write 自动建目录），格式：
- 现状数据流（文字 + 文件:行号证据）
- 与目标架构的差距（逐条，引用设计文档章节）
- 迁移风险点（谁构造 CommandContext、谁消费 CommandResult、波及面）
- 对 Phase 1（契约层）/ Phase 2（注册表）/ Phase 5（命令迁移）实施的建议

完成后报告：证据文件路径 + 3 条最关键的差距。`

const INV_TUI = `你在 Perihelion 仓库调查 Command 系统【TUI 侧】现状，为实施计划提供证据。只读，不改任何文件。

必读：
- ${DESIGN}（目标架构）
- peri-tui/src/kit/input_area.rs（build_slash_items / slash_items_cache / refresh_slash_items / SLASH_* 状态）
- peri-tui/src/kit/slash_completion.rs（SlashCompletionItem / SlashActionKind）
- peri-tui/src/kit/acp_notifier.rs（AVAILABLE_SLASH_COMMANDS 更新路径）
- peri-tui/src/kit/submit_request.rs（ui 命令本地拦截）
- peri-tui/src/kit/panel_registry.rs（PANELS / panel_for_slash_command）

调查问题：
1. slash items 的构建来源与 kind 推断（SKILL_NAMES / MCP_SKILL_NAMES 集合反推的证据位置）
2. ui 命令的本地拦截路径：submit_request 拦截哪些命令、如何拦截
3. 补全弹窗渲染：kind 分类、选中后的插入形态（裸名？mcp__ 全名？）
4. available_commands_update 的消费点与对条目结构的现状假设
5. panel_for_slash_command 映射表（TUI 本地命令名 → 面板）的完整清单

输出：证据报告写入 ${INV}/02-tui.md：
- 现状数据流 / 与目标架构差距（逐条）/ 迁移风险（删除反推后谁提供 kind、ui 上送注册的接入点）/ 对 Phase 4 实施的建议

完成后报告：证据文件路径 + 3 条最关键的差距。`

const INV_PROTO = `你在 Perihelion 仓库调查 Command 系统【协议与发现】现状，为实施计划提供证据。只读，不改任何文件。

必读：
- ${DESIGN}（目标架构，重点协议层）
- peri-acp/src/dispatch/commands.rs（build_available_commands / build_available_commands_update / UI_COMMANDS 常量）
- peri-acp-types/src/peri_caps.rs（ui_commands bool 字段与序列化）
- peri-acp-types/src/mcp_skills.rs（McpSkillRegistry / on_change / HandleToken / find / 断连清理）
- peri-acp-types/src/skills.rs（SkillMetadata 字段）
- 确认 agent-client-protocol-schema 依赖形态：在 peri-acp/Cargo.toml（及 workspace Cargo.toml）中如何引入（registry 版本 / [patch] / path？）；AvailableCommand / AvailableCommandsUpdate 定义在 ~/.cargo/registry/ 的哪个源码位置（grep 定位）

调查问题：
1. AvailableCommand 现状字段（是否仅 name / description）
2. UI_COMMANDS 硬编码清单（11 条）与 caps.ui_commands 布尔门控的完整逻辑
3. McpSkillRegistry 动态机制的可泛化程度（on_change 回调 / HandleToken 防 ABA / project_connected 断连清理——逐项评估能否直接泛化为注册表生命周期）
4. agent-client-protocol-schema 外部依赖约束：修改协议 schema 的现实路径（vendored / [patch] / fork / 上流 PR），给出推荐
5. TUI 与 stdio 两侧对 available_commands_update 的结构假设差异

输出：证据报告写入 ${INV}/03-protocol.md：
- 现状数据流 / 与目标架构差距（含外部依赖约束裁决）/ 迁移风险 / 对 Phase 3 实施的建议

完成后报告：证据文件路径 + 外部依赖裁决结论。`

const INV_FEED = `你在 Perihelion 仓库调查 Command 系统【反馈与事件】现状，为实施计划提供证据。只读，不改任何文件。

必读：
- ${DESIGN}（目标架构，重点 Feedback 层）
- peri-acp/src/session/command/clear.rs / compact.rs / rewind.rs / bg.rs
- peri-acp-types/src/command.rs（CommandResult 定义）
- peri-agent/src/session/exec/executor_helpers.rs（push_done 发送时机与语义）
- 在 peri-acp / peri-agent 中 grep：CompactCompleted / push_done / ExecutorEvent / CommandFeedback

调查问题：
1. 各命令的反馈形态清单：自造事件（CompactCompleted 等）与错误串（rewind 的 format!）分别在哪、怎么流向 TUI
2. push_done 机制：命令 turn 的 request_id None 语义、TUI 侧 id 配对与代际兜底
3. CommandResult.messages 的消费方：谁拿它接续会话、与事件流的关系
4. 命令执行失败的错误路径现状（rewind 未找到目标消息的显示方式）
5. CommandFeedback{level, message, channel: UiOnly|Session} 的接入点建议：事件类型放哪、TUI 通知条从哪消费、channel=Session 时谁写系统消息进会话

输出：证据报告写入 ${INV}/04-feedback.md：
- 现状数据流 / 与目标架构差距（逐条）/ 迁移风险 / 对 Phase 5 实施的建议

完成后报告：证据文件路径 + 自造事件清单。`

const invs = await parallel([
  () => agent(INV_REG, { label: 'investigate:registry' }),
  () => agent(INV_TUI, { label: 'investigate:tui' }),
  () => agent(INV_PROTO, { label: 'investigate:protocol' }),
  () => agent(INV_FEED, { label: 'investigate:feedback' }),
])

const invStatus = invs.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`调查完成: ${JSON.stringify(invStatus)}`)

// ── 撰写阶段：6 阶段并行，填充骨架的实施步骤 ───────────────────────
phase('撰写实施计划')

const WRITE_COMMON = `你在为 Perihelion command 系统重设计撰写分阶段实施计划。设计文档：${DESIGN}（完整阅读）。plan 骨架目录：${PLAN_DIR}。

任务：把骨架文件的「实施步骤」占位替换为可执行的分步计划。约束：
- 每步格式：具体文件路径 + 改动类型（新增/修改/删除）+ 关键代码形态（类型签名级，不必完整实现）+ 验证命令（cargo check / cargo test / cargo clippy 具体命令）
- 步骤顺序合理（先类型后使用方；跨阶段依赖以骨架的「依赖」小节为准）
- 标注风险点与回滚策略
- 保留骨架文件的 目标 / 范围 / 关键设计约束 / 验收标准 / 依赖 小节原文（可微调措辞但不得改变语义；与设计文档冲突时以设计文档为准）
- 中文撰写；完成后报告：修改的文件 + 步骤数 + 关键风险一句话`

const writes = await parallel([
  () => agent(`${WRITE_COMMON}

阶段文件：${PLAN_DIR}/01-plan-phase1-contract.md
证据文件：${INV}/01-registry.md、${INV}/04-feedback.md

Phase 1（契约层）要点：在 peri-acp-types 落地 CommandName 词法类型（最右冒号切分、2 层上限、第一/第二等级）、ArgsSchema serde 完整模型（positionals/named/flags + String|Int|Choice|Path）、CommandFeedback{level,message,channel}（默认 UiOnly）、CommandOutcome{Done,Inject,Delegate}、RouteEntry 三属性。`, { label: 'write:phase1' }),
  () => agent(`${WRITE_COMMON}

阶段文件：${PLAN_DIR}/02-plan-phase2-registry.md
证据文件：${INV}/01-registry.md、${INV}/03-protocol.md

Phase 2（注册表）要点：扁平 HashMap<fullname, RouteEntry> + alias 索引；register/unregister（含 namespace 前缀批量注销）；冲突裁决（内置 > 本地 skill > 动态注入，低优先级重名拒绝+警告）；on_change → 投影；注册表不 import handler 实现；find/find_arc 双份消除。`, { label: 'write:phase2' }),
  () => agent(`${WRITE_COMMON}

阶段文件：${PLAN_DIR}/03-plan-phase3-protocol.md
证据文件：${INV}/03-protocol.md

Phase 3（协议）要点：AvailableCommand 升级（fullname/kind/aliases/category/args/level）；caps.ui_commands 布尔 → Vec<UiCommandSpec>；UI_COMMANDS 常量删除；CommandFeedback 事件；agent-client-protocol-schema 外部依赖的修改路径裁决（vendored / [patch] / fork / 上流 PR）必须先给结论再写步骤。`, { label: 'write:phase3' }),
  () => agent(`${WRITE_COMMON}

阶段文件：${PLAN_DIR}/04-plan-phase4-tui.md
证据文件：${INV}/02-tui.md、${INV}/03-protocol.md

Phase 4（TUI）要点：build_slash_items 反推逻辑删除、纯消费投影；level 1 裸名 / level 2 全名渲染（display 即 lexical）；ui 域命令提交前本地拦截；schema 驱动补全（Choice 枚举 / Flag 名 / required 提示）；UiOnly 反馈进通知条不进会话；mcp__ 不出现在补全。`, { label: 'write:phase4' }),
  () => agent(`${WRITE_COMMON}

阶段文件：${PLAN_DIR}/05-plan-phase5-migration.md
证据文件：${INV}/01-registry.md、${INV}/04-feedback.md

Phase 5（命令迁移）要点：compact/bg/clear/rewind 迁移到 CommandHandler trait + Outcome 三态；参数改为 ArgsSchema 声明；反馈收敛为 CommandFeedback 双通道（自造事件 CompactCompleted 等删除）；mcp__ 解析与补全全移除；/etc/hosts 未命中 fall through 行为保留（回归测试）。`, { label: 'write:phase5' }),
  () => agent(`${WRITE_COMMON}

阶段文件：${PLAN_DIR}/06-plan-phase6-dynamic-injection.md
证据文件：${INV}/03-protocol.md

Phase 6（动态注入）要点：MCP skills / 插件经注册表动态注册/注销（Started 不占位、Discovered 才注册）；断连按 namespace 前缀批量注销、投影自动收缩；重连 = 注销 → 重发现 → 重注册（无 ABA）；provenance 校验（mcp:* / plugin:* 不可越权）；变更统一经 on_change 推送。`, { label: 'write:phase6' }),
])

const writeStatus = writes.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`撰写完成: ${JSON.stringify(writeStatus)}`)

return {
  investigation: invStatus,
  writing: writeStatus,
  planDir: PLAN_DIR,
}
