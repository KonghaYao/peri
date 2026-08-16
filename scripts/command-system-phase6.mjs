// Phase 6（动态注入）实施：注册表生命周期 → MCP 接线 → 插件+本地 skills → 挂点切换 → 测试面/去重并行 → review → fix → 验证
// 依据：.peri/plans/2026-08-15-command-system-rearch/06-plan-phase6-dynamic-injection.md
// 设计权威：docs/design/command-system.md

export const meta = {
  name: 'command-system-phase6-implement',
  description: 'Phase 6 动态注入并行实施 + code review fix 流程',
}

const PLAN6 = '.peri/plans/2026-08-15-command-system-rearch/06-plan-phase6-dynamic-injection.md'
const DESIGN = 'docs/design/command-system.md'
const IMPL = '.peri/plans/2026-08-15-command-system-rearch/.implementation'
const TYPES = 'peri-acp-types/src'
const ACP = 'peri-acp/src'
const MW = 'peri-middlewares/src'
const HUB = 'acp-hub/server/src/protocol'

// ── 阶段 1：并行（A1+A2 注册表生命周期 / B1 插件词法迁移） ────────
phase('并行：注册表生命周期增强 + 插件词法迁移')

const c1 = await parallel([
  () => agent(`你在 Perihelion 实施 Phase 6 A1+A2【CommandRegistry 契约层核对 + 来源生命周期状态机】。依据：${PLAN6}（步骤 A1/A2，完整读）+ ${DESIGN}（Routing 层）。
前置（关键事实）：注册表本体**已在契约层**——Phase 2 已交付 ${TYPES}/command_registry.rs（P2-1 修正，比 plan A1 更早落地）；${ACP}/session/command/mod.rs 已是 re-export + register_builtins 组合根；RegisterError（Conflict/ProvenanceMismatch/MalformedName）与 register 纯拒绝语义已按终态交付。**A1 只需核对零残留**（grep CommandRegistry 结构体定义是否仅在契约层、mod.rs 是否 re-export 形态），无改动或最小修正。

任务（主体是 A2）：
1. A1 核对：grep 确认 CommandRegistry 定义唯一在 ${TYPES}/command_registry.rs；${ACP}/session/command/mod.rs 为 re-export + register_builtins；无需迁移动作。
2. A2 注册表增强（${TYPES}/command_registry.rs 修改 + command_registry_test.rs 新增）：
   - pub use crate::mcp_skills::HandleToken（type-erased Arc + ptr_eq，mcp_skills.rs 先例）
   - SourceDiscoveryState { Started { handle }, Discovered { handle } }（Clone + Debug）
   - SourceProjection { to_discover: Vec<(String, HandleToken)>, removed_any: bool }（Default）
   - 新增 API：register_all(entries: Vec<RouteEntry>) -> (usize, Vec<RegisterError>)；project_sources(&self, connected: &[(String, HandleToken)]) -> SourceProjection；mark_source_started(&self, prefix, handle)；mark_source_completed(&self, prefix, handle, entries) -> usize（返回实际注册成功数）；clear_source_started(&self, prefix, handle)
   - 语义矩阵（照 mcp_skills.rs:70-107/115-134/138-174/225-234 逐条对齐）：project_sources retain 保留 connected 内来源、被移除来源逐个 unregister_namespace 前缀批量注销、removed_any 才锁外触发 on_change 恰一次；mark_source_started 覆盖为 Started 不触发、覆盖前为 Discovered 且有已注册条目 → 先批量注销 + on_change 一次；mark_source_completed Arc::ptr_eq 校验（旧任务回写丢弃防 ABA）→ 清旧 → 逐条 register（冲突/越权跳过 + 告警不整体回滚）→ 有实际变化才 on_change 恰一次；clear_source_started cancel 回退不触发；回调锁内取克隆、锁外调用
   - **unregister_namespace 签名以现状为准**（Phase 2 已交付，可能是 (domain, namespace) 两参——保持已交付签名不改动，内部支持前缀批量注销即可；mark_source_* 的 prefix 为 'mcp:demo' 形态字符串）
3. 测试（command_registry_test.rs）：register_all 部分成功矩阵（合法 + 冲突 + 越权混合）；project_sources 移除触发注销 + removed_any 门控；mark_source_started 覆盖矩阵（含 Discovered→Started 撤旧）；mark_source_completed ptr_eq 拒绝旧 handle 回写（无 ABA）+ 成功回写 + on_change 恰一次；clear_source_started 可重试。

验证：cargo check -p peri-acp-types && cargo test -p peri-acp-types && cargo clippy -p peri-acp-types --all-targets -- -D warnings。
完成后报告：A1 核对结论 + A2 API 清单 + 测试数 + 验证结果。`, { label: 'impl:registry-lifetime' }),
  () => agent(`你在 Perihelion 实施 Phase 6 B1【插件命令词法迁移（plugin 域三层形态）】。依据：${PLAN6}（步骤 B1，完整读）+ ${DESIGN}（权威词法章节）。

任务：
1. ${MW}/plugin/loader.rs process_command_file（:218 附近）：CommandEntry.name 从 '{plugin}:{cmd}' 二层形态改为 'plugin:{plugin}:{cmd}' 三层形态（与 CommandSource::Plugin 语义对齐；namespace = 插件名；原二层形态对第二等级非法）
2. merge_plugin_mcp_servers（:541 附近）保持不变（config 层唯一键 'plugin:{plugin}:{server}'），注释同步更新说明其不进命令命名空间
3. ${MW}/plugin/loader_test.rs（:723-790 断言）同步更新为新形态
4. 影响面确认：CommandEntry.name 形态变化；现状生产零消费（PluginCommandProvider 仅测试使用）——grep 确认无其他消费者

验证：cargo test -p peri-middlewares plugin::loader && cargo clippy -p peri-middlewares --all-targets -- -D warnings。
完成后报告：文件 + 词法形态变化 + 影响面确认 + 验证结果。`, { label: 'impl:plugin-lexical' }),
])

const c1Status = c1.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`并行实施: ${JSON.stringify(c1Status)}`)

// ── 阶段 2：A3 MCP 发现接线 ──────────────────────────────────────
phase('MCP 发现接线（A3）')

const c2 = await agent(`你在 Perihelion 实施 Phase 6 A3【MCP 发现接线（双注册表：元数据面 + 命令面）】。依据：${PLAN6}（步骤 A3，完整读）。前置：A2 已交付 project_sources / mark_source_started / mark_source_completed / HandleToken。

任务（5 文件）：
1. ${MW}/mcp/middleware.rs：McpMiddleware 增字段 command_registry: Option<Arc<CommandRegistry>> + builder pub fn with_command_registry(self, reg) -> Self；before_agent（:174-216 附近）元数据面投影保持，追加命令面投影——connected = self.pool.get_all_clients() 映射 (format!('mcp:{}', h.name), h.clone() as HandleToken)；project_sources(&connected) → to_discover 逐个 mark_source_started + spawn run_discovery 时追加 command_registry 实参
2. ${MW}/mcp/skill_discovery.rs：run_discovery 签名增 command_registry: Option<Arc<CommandRegistry>> 参数；新增 pub(crate) fn mcp_route_entries(server_name: &str, skills: &[...]) -> Vec<RouteEntry> 转换（fullname = 'mcp:{server}:{skill}' 小写、kind = CommandEntryKind::McpSkill、aliases 空、description 取 skill 描述、args_schema None、provenance = CommandSource::Mcp + CommandLifecycle::Discovered→注册时 Connected、handler = 占位——**先核对 peri-acp-types 是否有 McpProxy/AgentPassthrough 之类现成 handler，无则用最小占位 handler**（返回 CommandOutcome::Inject 或 Done + UI-only 反馈，注释标注 Phase 6 占位）；发现完成后 command_registry.mark_source_completed(&format!('mcp:{}', name), handle_token, mcp_route_entries(...))；错误路径 clear_source_started
3. ${ACP}/host/stage_builder.rs（:134 附近）+ ${ACP}/host/assembly.rs（:481 构造链）：把 session 级 CommandRegistry Arc 经 with_command_registry 注入 middleware
4. ${ACP}/session/mod.rs（:112 附近 / :248 / :285 / :558）：McpMiddleware 构造处（assembly 或 session 创建）传入 command_registry
5. 冲突/越权条目：注册失败走注册表纯拒绝（warn 即可，不整体回滚）

验证：cargo check --workspace && cargo test -p peri-middlewares mcp && cargo test -p peri-acp host::stage && cargo clippy -p peri-middlewares --all-targets -- -D warnings。
完成后报告：文件清单 + 接线形态 + 占位 handler 选择 + 验证结果。`, { label: 'impl:mcp-wiring' })

// ── 阶段 3：B2+C1 插件注册与本地 skills ──────────────────────────
phase('插件注册接线 + 本地 skills 归 core 域（B2+C1）')

const c3 = await agent(`你在 Perihelion 实施 Phase 6 B2+C1【插件命令注册接线 + 会话创建注册本地 skills】。依据：${PLAN6}（步骤 B2/C1，完整读）。前置：B1 词法迁移（plugin:{plugin}:{cmd} 三层形态）已完成；A3 已把 command_registry 注入会话与 middleware；A2 register 校验（MalformedName / ProvenanceMismatch / Conflict 纯拒绝）就位。

【B2 插件注册】：
1. ${MW}/plugin/loader.rs 新增 pub fn plugin_route_entries(entries: &[CommandEntry]) -> Vec<RouteEntry>：fullname = e.name（'plugin:{plugin}:{cmd}'）、kind = CommandEntryKind::Command、provenance = CommandSource::Plugin { name: 剥离 'plugin:' 前缀 } + CommandLifecycle::Connected、handler = Arc::new(PluginCommandHandler)（**核对现状有无此 handler；无则实现占位**——Outcome::Inject 占位 + UI-only 反馈「插件命令执行待后续版本」，注释标注）
2. ${ACP}/host/assemble.rs（:306 附近）：plugin_data.all_commands 预转 Vec<RouteEntry> 存入 host cfg（cfg.plugin_command_entries）
3. ${ACP}/session/mod.rs（:248 / :285 会话创建）：注册顺序 = 内置（register_builtins）→ **本地 skills（C1）** → 插件（本步，register_all(plugin_command_entries)）→ 动态注入（发现管线异步，A3 已接）

【C1 本地 skills 归 core 域】：
4. ${ACP}/session/mod.rs 会话创建（内置注册之后、插件之前）：skills_port.available_skills(cwd, plugin_skill_roots) 扫描（现状调用点收敛为本处）→ RouteEntry { fullname: 'core:{name}' 小写、kind: CommandEntryKind::Skill、aliases 空、description、args_schema None、handler: Arc::new(AgentPassthrough)（**核对 peri-acp-types/contract 层有无此 handler；无则实现**——skill 注入语义：注入指令文本进 agent 管线或最小占位，注释标注）、provenance: CommandSource::Core + Connected } → register_all
   - 冲突裁决：本地 skill 与内置同名 → Conflict 拒绝 + 告警，保持内置（不覆盖、不静默）
   - skill 名含 ':' → MalformedName 拒绝 + 告警跳过
5. 越权测试矩阵（${TYPES}/command_registry_test.rs 或合适位置新增）：Plugin provenance 注册 'mcp:foo:bar' → ProvenanceMismatch；任意来源注册 'mcp:hello'（单层）→ MalformedName；'plugin:ecc:deploy' 合法通过；同插件两 skill 同名 → Conflict
6. 会话创建 core 域冲突矩阵用例（peri-acp 测试）：内置 compact 先注册 → skill 同名 compact 被拒 + 告警，注册表保持内置条目

验证：cargo check --workspace && cargo test -p peri-acp-types -p peri-acp -p peri-middlewares && cargo clippy -p peri-acp -p peri-acp-types --all-targets -- -D warnings。
完成后报告：文件清单 + 注册顺序 + handler 现状/实现 + 测试矩阵 + 验证结果。`, { label: 'impl:plugin-local' })

// ── 阶段 4：A4+B3+C2 挂点迁移与收尾 ──────────────────────────────
phase('挂点迁移 + 插件动态刷新 + 扫描收尾（A4+B3+C2）')

const c4 = await agent(`你在 Perihelion 实施 Phase 6 A4+B3+C2【on_change 挂点迁移 + 插件动态刷新 + 发送侧扫描收尾】。依据：${PLAN6}（步骤 A4/B3/C2，完整读）。前置：C1 已落地（本地 skills / 插件条目已在注册表，切换瞬间投影不缺条目）；A3 已接发现管线。

【A4 挂点迁移（行为切换点）】：
1. ${ACP}/host/stdio/commands.rs：删除 available_skills 扫描（:28 附近）与 all_skills 合并（:36-39 附近），投影 = build_available_commands_update(&registry.snapshot(), caps)（Phase 3 投影函数）；set_on_change 挂 CommandRegistry（Weak 防环 + 同步重发保留）
2. ${ACP}/host/notify.rs（:199-263）：同构迁移（tokio::spawn 异步重发保留）
3. ${ACP}/host/stdio/session/create.rs（:114 / :197 调用点）+ ${ACP}/host/requests.rs（:383 附近调用点）：新签名适配
4. **McpSkillRegistry::set_on_change 的 stdio/notify 挂点删除**（该回调只服务命令列表重发，命令面已迁；McpSkillRegistry on_change API 本体保留供元数据面未来消费方）——删除前 grep 确认无其他消费者

【B3 插件 install/uninstall 动态刷新】：
5. ${ACP}/host/requests.rs plugin/install（:797 附近）与 plugin/uninstall（:868 附近）成功回包前：command_registry.unregister_namespace('plugin:') → plugin_manager.reload()（或 load_enabled_plugins_aggregated 直调）→ register_all(plugin_route_entries(&fresh.all_commands))；重载失败 → 保留空 plugin 域 + 日志告警，不阻塞 RPC 回包

【C2 发送侧收尾】：
6. send_available_commands / send_available_commands_update 签名删除 skills_port / cwd / plugin_skill_roots 参数（A4 已切 snapshot）；SkillsPort::available_skills 生产调用点收敛为会话创建一处（C1）——grep 确认零残留调用点

验证：cargo check --workspace && cargo test -p peri-acp host::requests host::stdio && cargo clippy -p peri-acp --all-targets -- -D warnings。
完成后报告：文件清单 + 挂点迁移形态 + 插件刷新链路 + 参数清理 grep 证据 + 验证结果。`, { label: 'impl:host-switch' })

// ── 阶段 5：并行（A5 MCP 测试面 / D1 去重简化） ──────────────────
phase('并行：MCP 测试面更新 + 去重逻辑简化')

const c5 = await parallel([
  () => agent(`你在 Perihelion 实施 Phase 6 A5【MCP 测试面更新（含重连顺序性）】。依据：${PLAN6}（步骤 A5，完整读）。前置：A3/A4 已交付（middleware 命令面投影 + 挂点迁移）。

任务（全部修改/新增测试，不写生产代码）：
1. ${MW}/mcp/middleware_test.rs（:431-458 附近）：McpSkillRegistry 断连清理断言**保留**；新增 CommandRegistry 等价断言——断连 → 'mcp:{server}:' 前缀条目从 snapshot 消失 + on_change 恰一次
2. **重连顺序性测试**（验收标准核心）：连接 → 发现 → 注册（投影含 'mcp:demo:hello'）→ 断连（投影收缩）→ 重连（新 handle）→ 重扫完成前投影**不含**新条目（Started → Discovered 不占位）→ 完成回写（投影复现 + resolve('mcp:demo:hello') 路由一致）；旧任务回写（旧 handle）被 ptr_eq 拒绝（无 ABA）
3. ${ACP}/host/requests_test.rs（:1359+）：on_change 重发链路挂点换 CommandRegistry（重发恰一次断言不变）
4. ${TYPES}/mcp_skills_test.rs（:156-352 去抖矩阵）：**断言零改动**，仅核对——若 A4 后测试引用已删挂点，随 A4 同步修正

验证：cargo test -p peri-middlewares -p peri-acp -p peri-acp-types && cargo clippy -p peri-middlewares --all-targets -- -D warnings。
完成后报告：测试清单 + 重连顺序性用例形态 + 验证结果。`, { label: 'impl:mcp-tests' }),
  () => agent(`你在 Perihelion 实施 Phase 6 D1【build 函数收窄 + 去重逻辑删除 + Hub 适配】。依据：${PLAN6}（步骤 D1，完整读）。前置：A4 已切 snapshot 驱动；注册表键唯一性已保证（A2）。

任务：
1. ${ACP}/dispatch/commands.rs：删除「本地优先按名去重」合并逻辑（原 :59-68 附近，现状以实际为准——词法统一后本地 core:{name} / MCP mcp:{server}:{skill} / 插件 plugin:{plugin}:{cmd}，键空间两两不相交，去重退化为注册表键唯一性）；删除 mcpSkillNames meta 写入（kind 已入投影条目，该 key 退役）；build_available_commands_update 保持 Phase 3 投影形态（entries = snapshot）
2. ${ACP}/dispatch/commands_test.rs 断言重写：投影 = snapshot 全量（内置 + 本地 + MCP + 插件条目）；'core:hello' 与 'mcp:demo:hello' **共存**（键唯一性而非按名去重）；本地 skill 与内置同名 → 仅内置条目存在；无 cap 门控语义保持
3. ${HUB}/acp_channel.rs（:724-731 消费点）：mcp_skill_names / skill_names 消费按 Phase 3 条目级 kind 适配（若 Hub 无法同日适配，保留 mcpSkillNames 一周兼容窗口并注释——优先同日适配）；Hub 名称级过滤（:1386-1416）对全名冒号形态核对（冒号已可通过，无需改动）
4. grep 确认：合并语义消费者仅 commands_test.rs（删除前核对无其他消费者依赖）

验证：cargo test -p peri-acp dispatch::commands && cargo check -p acp-hub && cargo test -p acp-hub（如可编译）&& cargo clippy -p peri-acp --all-targets -- -D warnings。
完成后报告：文件清单 + 去重删除证据 + Hub 适配形态 + 验证结果。`, { label: 'impl:dedupe' }),
])

const c5Status = c5.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`并行实施: ${JSON.stringify(c5Status)}`)

// ── 阶段 6：整合编译 ─────────────────────────────────────────────
phase('整合与编译验证')

const integration = await agent(`你是 Phase 6 整合 agent。并行/串行实施已完成：注册表生命周期（A1 核对 + A2 状态机）、插件词法（B1）、MCP 发现接线（A3）、插件注册 + 本地 skills（B2+C1）、挂点迁移 + 动态刷新 + 扫描收尾（A4+B3+C2）、MCP 测试面（A5）、去重简化（D1）。

任务：
1. 检查文件改动完整性（git status / git diff --stat 查看改动面）
2. 编译修复：cargo check --workspace 全绿；cargo test -p peri-acp-types -p peri-acp -p peri-agent -p peri-middlewares 全绿；cargo clippy --workspace --all-targets -- -D warnings 全绿
3. 修复任务间接口漂移（register_all 签名、mark_source_* 参数形态、send_available_commands 新签名、plugin_route_entries 可见性、middleware builder 链）
4. 语义抽查：session 创建后注册表含 内置 + 本地 skills + 插件条目；投影 = snapshot 全量且无去重；断连投影收缩链路（测试覆盖）；mcp__ 零出现
5. 写实施状态到 ${IMPL}/phase6-integration.md

完成后报告：三绿状态 + 修复问题清单 + 遗留确认。`, { label: 'integrate' })

// ── 阶段 7：并行 review ─────────────────────────────────────────
phase('并行 code review')

const REVIEW_BASE = `你是 Phase 6 的 code reviewer。权威依据：${DESIGN}（Routing 层 / 权威词法 / 正交维度 / 注入链路）与 ${PLAN6}（验收标准 :412-417）。目标：对照设计与验收标准审查实施质量，输出可执行问题清单。只读不改代码。

输出：问题清单写入 ${IMPL}/phase6-review-<主题>.md：
- 每条：严重度（P0 阻断编译或语义错误 / P1 违背设计或验收标准 / P2 建议）+ 文件:行号 + 问题描述 + 修复建议
- 末尾：无问题项确认列表`

const reviews = await parallel([
  () => agent(`${REVIEW_BASE}

主题：registry。审查 ${TYPES}/command_registry.rs 生命周期状态机（A2）。
核对点：
1. register_all 部分成功矩阵（合法 + 冲突 + 越权混合，返回 (usize, Vec<RegisterError>)）
2. project_sources：retain 保留 connected、被移除来源按前缀批量注销、removed_any 才 on_change 恰一次
3. mark_source_started 覆盖矩阵（Started 不触发；Discovered→Started 撤旧 + on_change 一次）
4. mark_source_completed：ptr_eq 拒绝旧 handle（无 ABA）、清旧→逐条注册（冲突跳过不整体回滚）、有变化才 on_change 恰一次
5. clear_source_started 可重试不触发
6. 回调锁内取克隆锁外调用（防死锁）
7. A1 核对结论（注册表唯一在契约层，mod.rs re-export）`, { label: 'review:registry' }),
  () => agent(`${REVIEW_BASE}

主题：mcp。审查 A3/A4/A5（${MW}/mcp/middleware.rs + skill_discovery.rs + ${ACP}/host/stdio/commands.rs + notify.rs + create.rs + middleware_test.rs + requests_test.rs）。
核对点：
1. before_agent 命令面投影：connected 映射 'mcp:{name}' + HandleToken；to_discover 逐个 mark_source_started + run_discovery 传 command_registry
2. mcp_route_entries：fullname 小写 'mcp:{server}:{skill}'、kind=McpSkill、provenance=Mcp+Connected、handler 占位语义明确
3. 发现完成 mark_source_completed / 错误路径 clear_source_started
4. A4 切换：投影 = snapshot、set_on_change 挂 CommandRegistry（Weak 防环）、McpSkillRegistry stdio/notify 挂点删除（grep 零残留）
5. 重连顺序性测试：Started→Discovered 不占位、断连收缩、重连复现、旧回写被 ptr_eq 拒绝
6. 断连 → 投影自动收缩（展示与路由同源不漂移）`, { label: 'review:mcp' }),
  () => agent(`${REVIEW_BASE}

主题：plugin-local。审查 B1/B2/B3/C1/C2/D1（${MW}/plugin/loader.rs + ${ACP}/host/assemble.rs + requests.rs + session/mod.rs + dispatch/commands.rs + ${HUB}/acp_channel.rs）。
核对点：
1. B1 词法：'plugin:{plugin}:{cmd}' 三层形态落地，merge_plugin_mcp_servers 不进命令命名空间
2. B2 plugin_route_entries：provenance = Plugin { name 剥离前缀 }；PluginCommandHandler 占位语义明确
3. C1 本地 skills：'core:{name}' + kind=Skill + AgentPassthrough（或等价占位）；注册顺序 内置→本地→插件→动态；同名冲突纯拒绝保持内置；skill 名含 ':' 被拒
4. B3：install/uninstall 后 unregister_namespace('plugin:') + reload + register_all；失败不阻塞回包
5. C2：send_available_commands 签名删 skills_port/cwd/plugin_skill_roots，available_skills 调用点收敛一处
6. D1：本地优先按名去重删除（键唯一性取代）；mcpSkillNames 退役；Hub 消费点适配；commands_test 断言重写（core:hello 与 mcp:demo:hello 共存）`, { label: 'review:plugin-local' }),
])

const reviewStatus = reviews.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`Review 完成: ${JSON.stringify(reviewStatus)}`)

// ── 阶段 8：并行 fix ────────────────────────────────────────────
phase('并行 fix')

const fixes = await parallel([
  () => agent(`你是 Phase 6 fix agent（registry）。读取 ${IMPL}/phase6-review-registry.md 问题清单，逐一修复 ${TYPES}/command_registry.rs 及测试。P0/P1 必修，P2 视质量修。修复后 cargo check -p peri-acp-types && cargo test -p peri-acp-types 验证。完成后报告：修复条目 + 验证结果`, { label: 'fix:registry' }),
  () => agent(`你是 Phase 6 fix agent（mcp）。读取 ${IMPL}/phase6-review-mcp.md 问题清单，逐一修复 middleware/skill_discovery/host 挂点文件及测试。P0/P1 必修。修复后 cargo check --workspace && cargo test -p peri-middlewares -p peri-acp 验证。完成后报告：修复条目 + 验证结果`, { label: 'fix:mcp' }),
  () => agent(`你是 Phase 6 fix agent（plugin-local）。读取 ${IMPL}/phase6-review-plugin-local.md 问题清单，逐一修复 loader/assemble/requests/session/commands/Hub 文件及测试。P0/P1 必修。修复后 cargo check --workspace && cargo test -p peri-acp -p peri-middlewares -p acp-hub 验证。完成后报告：修复条目 + 验证结果`, { label: 'fix:plugin-local' }),
])

const fixStatus = fixes.map((r, i) => (r ? 'ok' : 'FAILED'))
log(`Fix 完成: ${JSON.stringify(fixStatus)}`)

// ── 阶段 9：最终验证（含 D2 端到端验收） ─────────────────────────
phase('最终验证')

const final = await agent(`你是 Phase 6 最终验证 agent。

任务：
1. 全量验证：cargo check --workspace && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings，三绿
2. 验收核对：读 ${PLAN6} 验收标准小节（:412-417，5 条）逐条 通过/不通过 + 证据：
   - MCP skill 发现完成（Discovered）后才注册，连接中（Started）不占位
   - 断连按 'mcp:{server}:*' 前缀批量注销，投影自动收缩（展示与路由同源不漂移）
   - 重连 = 注销→重发现→重注册，无 ABA（含顺序性测试）
   - provenance 校验：插件只能注册 plugin:*、MCP server 只能注册 mcp:*，越权被拒 + 告警
   - 动态注册/注销统一经注册表 on_change → available_commands_update，TUI 协议无直接动态注册通道
3. D2 端到端核对（环境可用则手工冒烟，否则代码级核对）：MCP 连接/断连/重连投影链路；插件 install/uninstall 刷新；本地 skill 裸名可解析 + 同名被拒
4. 边界核对：mcp__ 在命令面（注册表/投影/补全）零出现；mcp_skills.rs 的 mcp_skill_name 仅供 tool 面消费（注释标注，Phase 6 范围外）
5. 写验证报告到 ${IMPL}/phase6-final.md

完成后报告：三绿状态 + 验收通过数/总条数 + 遗留问题`, { label: 'verify:final' })

return {
  parallel: c1Status,
  mcpWiring: c2 ? 'ok' : 'FAILED',
  pluginLocal: c3 ? 'ok' : 'FAILED',
  hostSwitch: c4 ? 'ok' : 'FAILED',
  parallel2: c5Status,
  integration: integration ? 'ok' : 'FAILED',
  review: reviewStatus,
  fix: fixStatus,
  final: final ? 'ok' : 'FAILED',
}
