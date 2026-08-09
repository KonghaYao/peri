// acp-hub 返工 workflow：质疑 → 对抗 → plan → fix → 验收（JS 全流程脚本实测）
//
// 背景：acp-hub 全部模块已实现且单测全绿（proto 38 / server 252 / instance 48），
// 但 Web 面板（前端）→ server（后端）→ instance → ACP 进程 的每一条用户链路
// 都不可用，无法进入验收。本 workflow 先对抗性质疑每条链路，再计划修复，
// 最后以「模拟前端用户全流程的 JS 脚本真实拉起进程实测通过」为交付判据。
//
// 已知观测证据（由主管初步调查收集，质疑 agent 必须验证或推翻）：
//   E1. server 启动 OK；instance hello 双向认证 OK（bootstrap-instance 注册，
//       fenced=false）；alive_sessions reconciliation 正常（0/0/0/0）。
//   E2. client 认证大量 `UnknownToken`（token_id="<unknown>"，auth_failed_total
//       累计 20+）；但用正确 token 时认证成功（e38527e9 / b670fe80，conn.open ok）。
//   E3. 02:57:16 一次真实 create chat：`chat persist store created` →
//       `command.submit ok` → 7ms 内 `outbox record cleared for retry
//       (pre-dispatch retryable failure)` → `command.error`。
//       根因候选：instance spawn 失败。server 硬编码 DEFAULT_ACP_CMD = ["peri","acp"]
//       （server/src/channel/command_coordinator.rs:50），acp-hub 是独立 workspace，
//       PATH 中未必有 `peri`，child::spawn 立即失败 → spawn_ack(ok=false) →
//       fail_retryable（command_coordinator.rs:1281-1305）→ pre-dispatch 清除。
//   E4. server/src/web/（Web 面板 + 验证台静态资源）是 git 未跟踪的新代码，
//       未经任何端到端验证；yjs 依赖 unpkg CDN（panel.html），离线即无法渲染。
//   E5. scripts/ws-verify.mjs 是既有的协议闭环验证脚本（node ≥21 内置 WebSocket）。

export const meta = {
  name: 'acp-hub-return',
  description: 'acp-hub 返工：质疑→对抗→plan→fix→验收（JS 全流程脚本实测通过才算交付）'
}

const COMMON = [
  '背景：/Users/konghayao/code/ai/perihelion/acp-hub 是独立 workspace（members: proto/server/instance）。',
  '组件：acp-hub-server（后端控制面，监听 127.0.0.1:8456，web 静态资源内嵌 include_str，见 server/src/web/mod.rs）；acp-instance（每实例一个，outbound ws 连 server，spawn/kill ACP 进程，见 instance/src/hub.rs）；acp-hub-proto（帧/动作/机器协议）；test-child（instance/bin/test_child.rs，假 ACP 进程，stdio 协议，集成测试用它充当 ACP agent）。',
  '架构权威文档：docs/architecture.md（§2 产品语义 P1-P9、§4 协议、§6 链路时序、§7 状态机、§8 韧性、§9 认证安全）；设计文档：docs/plans/（f1-f6）。',
  '已知观测证据：E1 instance hello 认证/注册 OK；E2 client 认证大量 UnknownToken 但正确 token 可认证成功；E3 create chat 后 7ms 内 pre-dispatch retryable failure → command.error（候选根因：spawn 硬编码 ["peri","acp"] 不存在于 PATH，见 command_coordinator.rs:50）；E4 web/ 是未跟踪新代码未经端到端验证；E5 有 ws-verify.mjs 协议验证脚本（node ≥21）。',
  '',
  '协作纪律（所有 agent 必须遵守）：',
  '1. 不修改 docs/architecture.md 与 docs/plans/（设计权威，只读）；发现设计缺陷记录到输出，由主管裁决。',
  '2. 修复只允许改：proto/src/、server/src/、instance/src/、acp-hub/scripts/（验收脚本）、acp-hub/docs/workflows/（本 workflow 可补充说明文件）；禁止改根项目（/Users/konghayao/code/ai/perihelion）的其他 crate。',
  '3. 日志用 tracing，token/密钥不落日志、不进输出。',
  '4. 每个验证用独立 temp 数据目录与随机端口；进程结束必须 kill 全部子进程（server/instance/test-child），防残留。',
  '5. 编译/测试命令：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo build --workspace && cargo test -p acp-hub-server && cargo test -p acp-instance && cargo test -p acp-hub-proto && cargo clippy --workspace --all-targets -- -D warnings。',
  '6. 报告第一行必须是固定格式（见各 agent 任务说明），便于 workflow 自动判定。',
  ''
].join('\n')

// ============================================================================
// Phase 1：质疑 —— 五条用户链路并行对抗调查（只读，不改代码）
// ============================================================================
phase('质疑：五条用户链路对抗调查')

const grills = await parallel([
  // ---- 链路 1：连接与认证（Web 面板打开 → token 粘贴 → 认证 → ready → registry 快照）
  () => agent(`你是 acp-hub 质疑 agent（链路 1：连接与认证）。\n\n${COMMON}\n\n任务：调查「Web 面板用户输入 token → 连接 → 认证 → ready → 看到实例/对话列表」这条链路为何不可用。\n1. 通读：server/src/auth/（TokenStore 文件格式、token 生成/校验、nonce）、server/src/channel/gateway.rs（ws accept、首帧认证、快照时序 §6.2、关闭码）、proto/src/conn.rs（帧枚举）、proto/src/hmac.rs（HMAC 双向）、scripts/ws-verify.mjs（既有验证脚本的认证写法）、server/src/web/index.html 与 panel.html 的说明。\n2. 验证 E2：为什么大量 UnknownToken？token generate 写入 tokens.toml 的格式与校验端读取是否一致（hash？明文？revoked 标记？）；token 文件路径（config_dir）在 CLI/env 覆盖下的不一致可能性；ws-verify.mjs 传 token 的用法（参数格式）与 server 期望是否匹配；read-only/full 角色校验路径。\n3. 实测（只读方式）：可临时用 node 起一个最小 ws 客户端对本地已启动的 server 探测（如果本地 server/instance 在跑），或用 server 二进制 token 子命令生成/列出 token 走查格式。\n4. 输出「链路 1 报告」：链路步骤图（每步：输入→动作→期望→实际）、断点证据（文件:行号 + 日志佐证）、根因假设（每个带确信度 HIGH/MED/LOW 与验证方法）、该链路的最小可验证断言清单。`, { label: 'grill-auth', model: 'sonnet', allowedTools: ['Read', 'Grep', 'Glob', 'Bash', 'folder_operations'] }),

  // ---- 链路 2：建会话（新建 → 选 instance → create → 两阶段 ack）
  () => agent(`你是 acp-hub 质疑 agent（链路 2：新建会话与 instance 路由）。\n\n${COMMON}\n\n任务：调查「用户点击新建会话 → create action → server 向 instance 下发 spawn → instance 拉起 ACP 进程 → committed ack」链路为何不可用。\n1. 通读：server/src/channel/command_coordinator.rs（submit/dispatch/exec_create 全路径、fail_retryable 触发点、两阶段 ack §4.4/§6.2）、server/src/control/instance_registry.rs（instance 注册表、send_spawn、InstanceSpawnAck）、server/src/control/chat_registry.rs、server/src/channel/chat_channel.rs、instance/src/hub.rs（handle_spawn、spawn 幂等、cwd/env 校验）、instance/src/child.rs（spawn 实现、失败路径）。\n2. 验证 E3：create 提交后 7ms 内 pre-dispatch retryable failure。追根因：spawn 命令 DEFAULT_ACP_CMD=["peri","acp"]（command_coordinator.rs:50）在当前环境是否可执行（PATH 检查）；instance 侧 spawn 失败返回 spawn_ack(ok=false, "spawn_failed") 的时序是否与 7ms 吻合；还有哪些 pre-dispatch 失败路径（实例离线/未注册/队列满/配额）；instance_id 路由（默认 "local" vs instance 注册的 "bootstrap-instance" 不一致？§4.5 的 id 语义）。\n3. 输出「链路 2 报告」：create 全时序图（server→instance→ACP 进程→ack 回传，标注每个 hop 的帧与超时）、断点证据（文件:行号 + 日志佐证，包括 02:57:16 那次失败的完整推断）、根因假设（确信度 + 验证方法）、最小可验证断言清单（含「用什么命令代替 peri acp 做验收」的建议）。`, { label: 'grill-create', model: 'sonnet', allowedTools: ['Read', 'Grep', 'Glob', 'Bash', 'folder_operations'] }),

  // ---- 链路 3：对话（prompt → ACP 输出 → 广播 → 前端看到流式内容）
  () => agent(`你是 acp-hub 质疑 agent（链路 3：prompt 与事件回流）。\n\n${COMMON}\n\n任务：调查「用户输入消息 → prompt action → instance 转发给 ACP 进程 → ACP 输出事件 → server 规范化投影 → yjs update 广播 → 前端渲染」链路。\n1. 通读：server/src/channel/command_coordinator.rs（prompt 的 dispatch、L3 确认 §4.4 路径 B）、server/src/protocol/acp_channel.rs（ACP 事件 → NormalizedEvent 映射 §6.1 表）、server/src/protocol/translator.rs（action → ACP JSON-RPC 线格式）、server/src/state/aggregator.rs 与 normalized.rs（投影）、server/src/state/doc_manager.rs 与 doc_pair.rs（Y.Doc 双 Doc）、server/src/channel/broadcaster.rs（fan-out、merge_updates、背压）、instance/src/transport.rs（帧转发）、instance/src/buffer.rs（断线缓冲）、instance/src/child.rs（stdout 转发）。\n2. 关键质疑点：prompt 事件（agent 消息 delta、message/status、elicitation 权限请求、toolCall）从 ACP stdout → instance → server → 客户端完整路径是否闭环；yjs 编码（encode_state_as_update_v1 vs v2，panel.html 注释要求 v1）是否对齐；快照帧/增量帧的 projectionVersion 语义；permission/elicitation 事件（Web 面板的 permission bar 依赖它，panel.html 有 perm-allow/perm-deny 按钮）是否在广播路径上。\n3. 输出「链路 3 报告」：prompt 全时序图（含超时与失败分支）、断点证据、根因假设（确信度 + 验证方法）、最小可验证断言清单。`, { label: 'grill-prompt', model: 'sonnet', allowedTools: ['Read', 'Grep', 'Glob', 'Bash', 'folder_operations'] }),

  // ---- 链路 4：Web 前端（静态资源 + js 帧格式 + UI 状态机 + yjs 渲染）
  () => agent(`你是 acp-hub 质疑 agent（链路 4：Web 前端代码）。\n\n${COMMON}\n\n任务：审查 Web 面板与验证台前端代码本身能否工作（不假设后端 OK，但也找出前端侧必然断点）。\n1. 通读：server/src/web/ 全部文件（mod.rs 静态路由、index.html/app.js/ws.js/style.css 验证台、panel.html/protocol.js/ws-client.js/yjs-view.js/ui.js/main.js 面板）与 proto/src/（帧格式：conn.rs 帧枚举与 JSON 字段名、action.rs、instance.rs）。\n2. 逐项核对前端与后端协议的契约：帧 JSON 字段名/结构是否与 server 解析一致（t/type 字段、action 信封字段名、ack/ready/keep_alive/error 帧字段）；认证首帧格式（ws-client.js 的 HubProtocol.auth 与 gateway 期望）；订阅/取消订阅帧（ysync.subscribe/unsubscribe 的 payload 形状）；action 帧的 commandId/chatId 字段名。\n3. 检查静态资源挂载：mod.rs 路由表与 index.html/panel.html 引用的路径是否一一对应；404 风险；panel.html 依赖 unpkg CDN yjs（离线不可用）的降级行为；style.css 是否被两页共用。\n4. 检查 UI 状态机：ws-client.js 的 ready/reconnect/重放订阅逻辑与 yjs-view.js 的 applyUpdate 调用是否自洽；ui.js 的事件绑定与 main.js 装配是否缺项（如新建会话按钮 → create action 的参数 instance_id 从哪来）。\n5. 输出「链路 4 报告」：前端协议契约核对表（每项：前端写法 vs 后端期望 vs 结论 OK/BREAK，附文件:行号）、UI 断点清单、根因假设（确信度 + 验证方法，验证方法应为 node 可执行的最小复现或 jsdom 不可用时的手工走查）、最小可验证断言清单。`, { label: 'grill-web', model: 'sonnet', allowedTools: ['Read', 'Grep', 'Glob', 'Bash', 'folder_operations'] }),

  // ---- 链路 5：instance 侧（注册/心跳/转发/缓冲/补推/重连/进程管理）
  () => agent(`你是 acp-hub 质疑 agent（链路 5：instance 进程侧）。\n\n${COMMON}\n\n任务：调查 instance 侧全链路：hello 注册 → 心跳保活 → spawn/kill 执行 → ACP 事件转发 → 断线缓冲/补推 → 重连恢复。\n1. 通读：instance/src/（hub.rs 主循环与状态、transport.rs 帧收发与重连、buffer.rs 分桶缓冲、child.rs 进程树管理、auth.rs）、instance/src/bin/test_child.rs（假 ACP 进程的 stdio 协议：它期望什么输入、输出什么事件）、server/src/control/instance_registry.rs（server 侧 instance 状态机：offline/online/fenced/断链清理）。\n2. 关键质疑点：instance 启动参数（--token-file 等，main.rs）与 dev.sh 的流程是否一致；hello 帧内容（instance_id="bootstrap-instance" vs server 默认路由 "local" 的错配风险）；心跳/keep_alive 双向机制（server 5s keep_alive → instance 回 pong？还是 instance 主动 ping？协议里是谁在保活，§4.7）；断线重连的 backoff 与补推（resync）流程；spawn 的 cwd/env 白名单与进程组 kill 正确性；test-child 的输出是否与 server 期望的 ACP 事件 schema（§6.1 表）匹配（尤其 JSON-RPC 线格式：id/method/params 字段）。\n3. 输出「链路 5 报告」：instance 状态机时序图（正常 + 断线恢复）、断点证据、根因假设（确信度 + 验证方法）、最小可验证断言清单（含 test-child 能否当 ACP 验收替身的结论）。`, { label: 'grill-instance', model: 'sonnet', allowedTools: ['Read', 'Grep', 'Glob', 'Bash', 'folder_operations'] }),
])

// ============================================================================
// Phase 2：对抗 —— 汇总五份质疑报告，交叉验证，挑战未证实假设
// ============================================================================
phase('对抗：根因汇总与假设挑战')

const adversary = await agent(`你是 acp-hub 对抗裁决 agent。\n\n${COMMON}\n\n背景：五条链路的质疑报告已产出（见下方五份报告的全文）。\n\n任务：\n1. 通读五份报告，交叉验证：同一现象的重复根因（合并）；互相矛盾的结论（裁决：谁的证据更硬）；「假设但未实测」的结论（标记 UNVERIFIED，必须给出一个可执行的实测方案，能由后续 fix agent 或验收脚本验证）。\n2. 按「用户主链路依赖序」输出最终根因候选清单，格式：\n   [R1] <根因标题> ｜ 影响链路: <链路名> ｜ 证据: <文件:行号 + 日志> ｜ 确信度: HIGH/MED/LOW ｜ 建议修复: <最小改动描述> ｜ 验证: <命令或断言>\n   排序要求：先修认证链路（R-auth），再建会话（R-create），再对话（R-prompt），再前端（R-web），再韧性（R-instance/其他）。\n3. 明确「主链路恢复的充分条件」：列出验收脚本必须全 PASS 的断言集（参考 ws-verify.mjs 的 a-g 断言并扩展到完整用户流程：认证→ready→建会话→订阅→prompt→收到 ACP 输出→cancel→close→断开）。\n4. 输出首行必须是「ADVERSARY: DONE」，随后是：根因候选清单（按上述格式）、UNVERIFIED 假设清单、验收断言集、建议的修复执行顺序（依赖图：哪些修复必须先完成）。`, { label: 'adversary', model: 'sonnet', allowedTools: ['Read', 'Grep', 'Glob', 'Bash', 'folder_operations'] })

// ============================================================================
// Phase 3：plan —— 修复计划
// ============================================================================
phase('plan：修复计划')

const plan = await agent(`你是 acp-hub 修复计划 agent。\n\n${COMMON}\n\n背景：对抗裁决已给出根因候选清单（见下方 adversary 报告全文）。\n\n任务：\n1. 以根因候选清单为输入，产出可执行的修复计划。每阶段（P0/P1/P2…）含：\n   - 阶段名与目标（对应哪些根因 R#）\n   - 要修改的文件清单（精确到文件）与每处改动的描述\n   - 每处改动的验证方式（单元测试名 / cargo test 过滤 / 手工命令）\n   - 完成该阶段的判定标准（可执行检查）\n2. 特别要求：\n   a. 对 E3/spawn 根因给出确定方案：ACP 命令如何可配置（建议：server 配置或环境变量注入 acp_cmd，默认值需在当前环境可用——明确写出验收时用什么命令充当 ACP 进程，如 test-child 的路径与参数，或 peri 的真实路径）；方案必须让「create → spawn → 事件回流」在无 peri 的机器上也能端到端验收。\n   b. 对 E2/认证根因给出确定方案：修复 UnknownToken（若是格式不一致则统一 token 生成/校验；若是用法问题则在验收脚本中写明正确用法）。\n   c. 对 E4/前端：列出前端必须修的契约 BREAK 项；确认 yjs CDN 依赖是否可接受（不可接受则给出替代：vendored yjs 文件放入 web/ 或降级方案）。\n   d. 验收脚本方案：在 acp-hub/scripts/ 下新建 e2e-flow.mjs（node ≥21，内置 WebSocket，零 npm 依赖），自包含：随机端口/临时目录起 server → 提取 instance token 起 instance → 生成 client full token → 模拟前端全流程断言（含真实 spawn ACP 进程与事件回流）。写明脚本的步骤与每步断言。\n3. 输出首行必须是「PLAN: <阶段数> 阶段」，随后是完整计划；最后附「验收判据」（e2e-flow.mjs 全 PASS 的充分条件清单）。`, { label: 'plan', model: 'sonnet', allowedTools: ['Read', 'Grep', 'Glob', 'Bash', 'folder_operations'] })

// ============================================================================
// Phase 4：fix —— 修复执行循环（串行轮次，每轮全量回归）
// ============================================================================
phase('fix：修复执行')

let fixRounds = 0
const MAX_FIX = 4
let fixOutcome = 'UNRUN'

while (fixRounds < MAX_FIX) {
  fixRounds += 1
  const fix = await agent(`你是 acp-hub 修复 agent（第 ${fixRounds} 轮）。\n\n${COMMON}\n\n背景：修复计划见下方 plan 报告全文；对抗裁决根因清单见下方 adversary 报告全文。上一轮修复情况见你的探索（本轮 agent 无历史上下文，先读 plan 与 adversary 的输出，再看 git diff 了解已改内容）。\n\n任务（第 ${fixRounds} 轮范围）：\n1. 按 plan 的阶段顺序执行尚未完成的修复项（已完成项跳过；若上一轮改动引入回归，先修复回归）。\n2. 允许修改：proto/src/、server/src/、instance/src/、acp-hub/scripts/（本轮以 Rust 代码修复为主；web 静态资源属于 server/src/web/ 也在范围内）。\n3. 每个根因修复后立即验证对应测试；全部完成后跑全量：cargo build --workspace && cargo test -p acp-hub-server && cargo test -p acp-instance && cargo test -p acp-hub-proto && cargo clippy --workspace --all-targets -- -D warnings。\n4. 若可行（node ≥21 且本地能起 server+instance），运行 scripts/ws-verify.mjs 实测协议闭环；不可行则说明原因。\n5. 输出第一行必须是「RESULT: PASS」（全量测试+clippy 全绿且无未决根因）或「RESULT: FAIL <剩余问题摘要>」；随后：本轮改动清单（文件+改动+对应根因 R#）、验证结果摘要、仍未解决项及原因、下一轮建议。\n\n注意：不要为了 PASS 而删测试或跳过验证；每处改动必须能说明对应哪个根因。`, { label: `fix-r${fixRounds}`, model: 'sonnet', allowedTools: ['Read', 'Write', 'Edit', 'Bash', 'Glob', 'Grep', 'folder_operations'] })

  if (fix.trimStart().startsWith('RESULT: PASS')) {
    fixOutcome = `PASS after ${fixRounds} round(s)`
    break
  }
  fixOutcome = `round ${fixRounds} still failing`
}

// ============================================================================
// Phase 5：验收 —— JS 全流程脚本实测（交付判据）
// ============================================================================
phase('验收：JS 全流程脚本实测')

let accRounds = 0
const MAX_ACC = 3
let accOutcome = 'UNRUN'
let accReport = ''

while (accRounds < MAX_ACC) {
  accRounds += 1
  const acc = await agent(`你是 acp-hub 验收 agent（第 ${accRounds} 轮）。\n\n${COMMON}\n\n背景：修复已完成（见 fix 结果）；对抗裁决的验收断言集与 plan 的验收脚本方案见对应报告。\n\n任务：\n1. 编写 acp-hub/scripts/e2e-flow.mjs（node ≥21，内置 WebSocket，零 npm 依赖；如第 ${accRounds} 轮已有该文件则改为修复它）。脚本自包含、可重复执行，模拟 Web 面板用户全流程：\n   a. 启动：随机端口 + 临时 config/data 目录（环境变量 ACP_HUB_CONFIG_DIR/ACP_HUB_DATA_DIR 注入）启动 acp-hub-server 真实二进制（cargo build 产物，CARGO_BIN_EXE 或 target/debug/ 路径）；等待 tokens.toml 出现 instance token（bootstrap 自动生成）。\n   b. 启动 acp-instance（--token-file 指向提取的 instance token）→ 等待 server 日志出现 instance connected（或通过 ws 探测）。\n   c. 生成 client full token（server token generate 子命令，或直接构造 tokens.toml）。\n   d. ws 客户端全流程断言（每步 PASS/FAIL 打印）：\n      (1) 连接 → 首帧 auth → ready（projectionVersions 含 hub:registry）；\n      (2) 订阅 registry 或直接 chat/create → accepted → committed（chatId 非空）；\n      (3) 关键：create 必须真实 spawn 成功 ACP 进程（ACP 命令按修复方案注入，如 test-child 绝对路径）；\n      (4) subscribe chat:{cid} → 收到快照帧（含 projectionVersion）；\n      (5) chat/prompt → accepted → committed → 随后收到来自 ACP 进程的 delta 事件（test-child 收到 prompt 后会输出事件，断言收到 ≥1 个内容帧且含该 prompt 的回应内容）；\n      (6) cancel → 终态确认（interrupted/终态帧）；\n      (7) chat/close → server 日志出现 chat closed（或收到对应帧）；\n      (8) 断开连接（正常 close）。\n   e. 清理：kill server/instance/test-child 全部子进程与临时目录。\n2. 执行：cd /Users/konghayao/code/ai/perihelion/acp-hub && node scripts/e2e-flow.mjs（若脚本需先 cargo build 则在脚本外先构建；给 Bash 充足超时）。\n3. 若脚本失败：区分「脚本自身 bug」（修脚本）与「实现缺陷」（记录缺陷清单：文件+现象+建议修复点，留给下一轮 fix 或在本轮直接最小修复实现代码后重跑）。\n4. 输出第一行必须是「RESULT: PASS」（脚本全 PASS 且无未决实现缺陷）或「RESULT: FAIL <失败断言摘要>」；随后：每步断言结果明细、脚本路径与用法、遗留缺陷清单。`, { label: `accept-r${accRounds}`, model: 'sonnet', allowedTools: ['Read', 'Write', 'Edit', 'Bash', 'Glob', 'Grep', 'folder_operations'] })

  if (acc.trimStart().startsWith('RESULT: PASS')) {
    accOutcome = `PASS after ${accRounds} round(s)`
    accReport = acc
    break
  }
  accOutcome = `round ${accRounds} still failing`
  accReport = acc
}

return {
  grills: grills.map((g, i) => `[链路${i + 1}] ${(g || '').slice(0, 400)}`),
  adversary: (adversary || '').slice(0, 1200),
  plan: (plan || '').slice(0, 1200),
  fixOutcome,
  fixRounds,
  accOutcome,
  accRounds,
  accTail: (accReport || '').slice(-1500),
  verdict: (fixOutcome && fixOutcome.startsWith('PASS') && accOutcome && accOutcome.startsWith('PASS'))
    ? 'DELIVERED: 主链路端到端实测通过'
    : `NOT DELIVERED: fix=${fixOutcome} accept=${accOutcome}`,
}
