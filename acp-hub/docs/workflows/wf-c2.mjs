export const meta = {
  name: 'acp-hub-wf-c2',
  description: 'acp-hub M1 阶段C 补单：F5 拆分为 F5a（protocol+channel）与 F5b（control+装配）串行实现（code→review，sonnet）'
}

const COMMON = [
  '背景：/Users/konghayao/code/ai/perihelion/acp-hub 是独立 workspace（members: proto/server/machine）。',
  '已完成：proto crate；server 的 config/、auth/、persist/、state/（140 tests 全绿）；machine（48 tests 全绿）。',
  '设计文档：docs/plans/f5-channel-control.md（732 行，模块划分与全部 API 签名）。',
  '架构权威文档：docs/architecture.md。',
  '',
  '协作纪律（必须遵守）：',
  '1. 只操作任务指定的文件；严禁修改 lib.rs、Cargo.toml（依赖已预填，如需新增依赖记录到输出由主管处理）、其他模块文件。',
  '2. 占位单文件（protocol.rs/channel.rs/control.rs）按设计转为目录 protocol//channel//control/（git rm 原单文件）。',
  '3. 公共项写 doc 注释；不修改 docs/architecture.md 与 docs/plans/。',
  '4. 日志用 tracing，遵守 §9.3 脱敏。',
  '5. 设计文档与架构冲突时以架构文档为准，无法裁决的记录到输出。',
  '',
  '【迭代纪律——防再次超 200 次迭代】：',
  'a. 每个源文件用 Write 一次性写入完整内容（不要先写骨架再多次 Edit 修补）；',
  'b. 测试文件同样一次 Write；',
  'c. 编译/测试错误先集中收集（一次 cargo build 拿全部错误），再统一修复；',
  'd. 若某文件超 400 行，拆成同目录多个文件（如 gateway 可拆 gateway_auth.rs/gateway_loop.rs），保持 lib 内 mod 组织；',
  'e. 严禁用「试错循环」调 API——不确定的 API 先 Read 源文件确认签名。',
  ''
].join('\n')

phase('F5a: protocol + channel')

const f5a_code = await agent(`你是 acp-hub 实现 agent，负责 Feature F5a：实现 server 的 protocol/ 与 channel/ 模块（设计稿 §3-§10）。\n\n${COMMON}\n\n任务：\n1. 先 Read docs/plans/f5-channel-control.md 全文（§1-§10），再按需核对 architecture.md §4.2/§4.3/§4.4/§6.1/§6.2/§7.4/§8.6/§9.2/§9.5 与已有模块 API（grep pub fn：server/src/config/mod.rs、server/src/auth/mod.rs、server/src/state/doc_manager.rs、server/src/state/normalized.rs、server/src/persist/mod.rs、proto/src/conn.rs、proto/src/action.rs、proto/src/machine.rs、proto/src/whitelist.rs、proto/src/hmac.rs）。\n2. 实现（占位单文件 protocol.rs/channel.rs 转为目录）：\n   - protocol/acp_channel.rs：双格式 sessionId 提取（原始 {type,payload} 与 JSON-RPC 包裹）、事件映射表（§6.1 表 14 变体 → NormalizedEvent::EventBody）、未知帧 → UNSUPPORTED_FRAME、出站翻译边界。\n   - protocol/translator.rs：出站 action → ACP JSON-RPC（cwd/rpcId 注入、方法面映射）。\n   - channel/connection_registry.rs：配额 200、连接上下文（ConnectionCtx：token_id/role/binding/本连接订阅）。\n   - channel/session_channel.rs：客户端连接归一化（action 入站 → CommandCoordinator、快照/ready 时序辅助）。\n   - channel/command_coordinator.rs：每 session 串行队列（64 上限）、commandId 去重（与 persist outbox 去重索引协作）、入队临界区、两阶段 Ack（accepted 立即 / committed 投影后）。\n   - channel/relay_event_handler.rs：machine 入站事件消费（校验 epoch/seq、写 outbox 意图记录、投 DocManager）、断链清理钩子。\n   - channel/broadcaster.rs：fan-out（每连接每 doc 订阅）、背压 64KB/128KB、merge_updates、慢连接断开。\n   - channel/gateway.rs：ws accept（tokio-tungstenite）、连接级认证（machine token 双向 HMAC / client token 校验，复用 auth 模块）、快照时序（§6.2）、keep_alive/pong（§4.7）、关闭码 4500/4501/4502、非回环拒绝（config.allow_peer）。\n3. 写测试（按设计稿 §16 对应部分）：acp_channel 纯函数映射逐条、translator 映射、connection_registry 配额、command_coordinator 去重/队列满/两阶段 Ack、broadcaster 背压与 merge、gateway 用 fake ws client（tokio-tungstenite 起真 ws 或内存 loopback）验证 auth/ready/keep_alive 4501。\n4. 验证：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo test -p acp-hub-server protocol channel && cargo clippy -p acp-hub-server --all-targets -- -D warnings。\n\n注意：本阶段只实现 protocol/ 与 channel/；control/ 与 main.rs 由下一个 agent 做（你不得创建 control/ 文件）。channel 需要调用 control 侧类型时（如 machine_registry），先按设计稿签名定义接口并在输出中列出「待 F5b 提供的类型」，或者用 trait/闭包注入解耦。\n\n输出：模块文件清单、核心类型/函数签名摘要、测试结果（测试名列表）、clippy 结果、「待 F5b 提供的类型/接口」清单、遗留问题。`, { label: 'f5a-code', model: 'sonnet', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

const f5a_review = await agent(`你是 acp-hub 代码审查 agent，负责审查 Feature F5a（server protocol/ + channel/）。\n\n${COMMON}\n\n任务：\n1. 对照 architecture.md §4.2/§4.3/§4.4/§6.1/§6.2/§7.4/§8.6/§9.2/§9.5 与 docs/plans/f5-channel-control.md（§3-§10），审查 server/src/protocol/ 与 server/src/channel/：\n   - 事件映射表逐条核对 §6.1（14 变体映射完整、字段正确）\n   - 双格式 sessionId 提取正确性\n   - command_coordinator 去重与两阶段 Ack 顺序（accepted 立即、committed 投影后）\n   - gateway 认证（HMAC 双向、token 校验）、快照时序、keep_alive/关闭码（4500/4501/4502）、allow_peer\n   - broadcaster 背压（64KB/128KB）、merge_updates\n   - relay_event_handler 的 outbox 交互与断链清理钩子\n   - 安全：token 不落日志\n2. 运行：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo test -p acp-hub-server && cargo clippy -p acp-hub-server --all-targets -- -D warnings。失败/警告直接修复（只改 protocol//channel/）。\n3. 输出：结论（通过/不通过）、问题清单按严重级、已修复项、「待 F5b 提供的类型/接口」清单核实、未修复项及理由。`, { label: 'f5a-review', model: 'sonnet', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

phase('F5b: control + 装配')

const f5b_code = await agent(`你是 acp-hub 实现 agent，负责 Feature F5b：实现 server 的 control/ 模块与 main.rs 装配（设计稿 §11-§15）。\n\n${COMMON}\n\n任务：\n1. 先 Read docs/plans/f5-channel-control.md（§11-§15），再 Read server/src/protocol/、server/src/channel/ 已实现代码（F5a 已完成，API 以代码为准）与 server/src/main.rs（F2 的 CLI/token 命令与 run_with 骨架）、server/src/state/registry.rs、server/src/persist/mod.rs、server/src/auth/mod.rs。\n2. 实现 control/ 模块（占位单文件 control.rs 转目录）：\n   - control/machine_registry.rs：机器注册表（REGISTERED/ONLINE/OFFLINE）、心跳 30s 判定离线、hello 幂等 fencing（同 machine 重连踢旧连接）、spawn/kill 指令下发与 ack 跟踪。\n   - control/session_registry.rs：会话状态机（accepting/ended/closed/crashed/gap/pending_close）、Turn 状态机（accepting/running/awaiting_permission/cancelling/completed/failed/cancelled/interrupted）、绑定（session/new → binding → committed）、断链语义（machine 离线 → 活 session 置 interrupted + 权限批量 expired + gap + pending_close 补发）。\n   - control/heartbeat.rs + close_codes.rs：心跳定时器与关闭码常量接线（§4.7）。\n   - control/hub.rs：装配（run_with 扩展）：监听 127.0.0.1:8456 默认（config 优先）、装配 auth/persist/state/protocol/channel/control 全部组件、优雅关闭（SIGINT/SIGTERM）、Degraded 判定入口、恢复对账钩子（§15）。\n3. 扩展 server/src/main.rs：run_with 调用 control/hub 的装配入口；保持 CLI/token 子命令兼容。\n4. 写测试（按设计稿 §16）：machine_registry 心跳离线（短超时配置/tokio::time::pause）、hello fencing、session_registry 状态机迁移（全路径）、断链语义（interrupted + 权限 expired + gap + pending_close）、补推协调（epoch 校验、from_seq=last_seq+1）、control/hub 装配 smoke test（起真 server 于随机端口 + fake client 连接验证 ready 时序）。\n5. 验证：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo test -p acp-hub-server && cargo clippy -p acp-hub-server --all-targets -- -D warnings。\n\n输出：模块文件清单、核心类型/函数签名摘要、测试结果（测试名列表）、clippy 结果、遗留问题。`, { label: 'f5b-code', model: 'sonnet', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

const f5b_review = await agent(`你是 acp-hub 代码审查 agent，负责审查 Feature F5b（server control/ 与装配）。\n\n${COMMON}\n\n任务：\n1. 对照 architecture.md §3/§4.5/§4.7/§6.2/§7.1/§7.2/§7.3/§7.6/§8.2/§8.3/§8.5/§17 与 docs/plans/f5-channel-control.md（§11-§15），审查 server/src/control/ 与 main.rs：\n   - machine_registry：状态迁移、心跳离线判定、hello fencing（踢旧连接）、spawn/kill ack 跟踪\n   - session_registry：状态机全路径（含 pending_close、crashed、gap）、Turn 状态机（含 interrupted 校准）、绑定时序\n   - 断链语义：machine 离线 → 活 session interrupted + 权限批量 expired + gap + pending_close 补发\n   - 补推协调：epoch 校验、from_seq=last_seq+1、排空后恢复实时\n   - 装配：监听配置、优雅关闭、degraded 判定、恢复对账\n   - 提交点纪律：outbox 落盘 → 下发 → L1/L2 → 投影 → committed Ack（跨 channel+control 核对）\n2. 运行：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo test -p acp-hub-server && cargo clippy -p acp-hub-server --all-targets -- -D warnings。失败/警告直接修复（只改 control//main.rs；若需修 protocol//channel/ 的小接口问题可改，但须在输出说明）。\n3. 输出：结论（通过/不通过）、问题清单按严重级、已修复项、未修复项及理由。`, { label: 'f5b-review', model: 'sonnet', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

return { f5a: { code: f5a_code, review: f5a_review }, f5b: { code: f5b_code, review: f5b_review } }
