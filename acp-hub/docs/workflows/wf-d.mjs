export const meta = {
  name: 'acp-hub-wf-d',
  description: 'acp-hub M1 阶段D：F7 server 端口集成测试（真实二进制进程全链路），test→fix→test 循环'
}

const COMMON = [
  '背景：/Users/konghayao/code/ai/perihelion/acp-hub 是独立 workspace（members: proto/server/machine）。',
  '全部功能已实现：proto crate（38 tests）、acp-hub-server（252 tests：config/auth/persist/state/protocol/channel/control 全模块）、acp-machine（48 tests：child/transport/buffer/auth）、clippy 零警告。',
  '架构权威文档：docs/architecture.md；设计文档：docs/plans/（f1-f6 六个）。',
  '关键二进制：server bin = acp-hub-server（监听 127.0.0.1:PORT，默认 8456，CLI 有 token 子命令与 run 参数，见 server/src/main.rs）；machine bin = acp-machine（outbound ws 连 server，见 machine/src/main.rs）；假 ACP 进程 bin = test-child（machine/bin/test_child.rs，stdio 协议，集成测试用它充当 ACP agent 子进程）。',
  'token 体系（§9.2）：machine token（每机器一个）+ client token（full/read-only）；server 首次启动自动生成或 CLI token generate；token 文件 0600（见 server/src/auth/mod.rs TokenStore 文件格式）。',
  '',
  '协作纪律：',
  '1. 集成测试只新增 server/tests/ 与 machine/tests/ 下文件（或必要的最小被测代码修复）；不改 lib.rs/Cargo.toml；',
  '2. 不修改 docs/architecture.md 与 docs/plans/；',
  '3. 每个测试独立 temp 数据目录与随机端口，可并行重跑；',
  '4. 测试超时控制：每个用例总超时 ≤ 60s（防挂起）；',
  '5. 进程清理：测试结束必须 kill 所有子进程（server/machine/test-child），防残留；',
  '6. 日志用 tracing/test 打印，token 不落日志。',
  ''
].join('\n')

phase('F7 集成测试实现')

const it_code = await agent(`你是 acp-hub 集成测试 agent，负责 Feature F7：编写 server 端口集成测试（真实进程全链路）。\n\n${COMMON}\n\n任务：\n1. 先 Read docs/architecture.md §4.8（M1 测试向量 1-12 全表）、§6.2（连接建立时序）、§7（状态机）、§9.2（认证）、§12（集成测试清单）；再读 server/src/main.rs（CLI 参数与 token 子命令）、machine/src/main.rs（machine 启动参数）、machine/src/bin/test_child.rs（假 ACP 进程协议行为）、proto/src/（帧类型与帧格式，测试用 serde_json 构造帧）。\n2. 在 server/tests/ 下编写：\n   - common/mod.rs：测试基建——编译好的二进制路径解析（env!("CARGO_BIN_EXE_acp-hub-server") 与 CARGO_BIN_EXE_acp-machine、CARGO_BIN_EXE_test-child）、随机端口（0 端口监听后读出）、temp data dir、token 文件生成（调用 server 二进制 token generate 或直接构造 TokenStore 文件格式）、子进程 spawn/清理（Drop guard，确保 kill 进程组）、ws 客户端 helper（tokio-tungstenite 连接 + 认证握手 + 读帧/发帧）。\n   - integration_tests.rs（或按场景拆多个文件）：\n     a. 启动 → machine 连接注册（hello 双向认证）→ machine 状态 ONLINE（§4.5）\n     b. client ws 连接 → 认证 → ready + 快照（§6.2 时序）\n     c. create session（§6.2 全时序：spawn → initialize → session/new → binding → committed）→ 用 test-child 作为 ACP 进程\n     d. prompt → test-child 输出 delta/工具事件 → server 聚合 → 客户端收到广播与 committed ack（§4.3/§4.4）\n     e. 同 commandId 二次提交 → duplicate ack（§4.4）\n     f. 坏 token / 未知帧类型 → UNAUTHENTICATED / UNSUPPORTED_FRAME（§4.8 向量 6/7）\n     g. 非回环 peer 拒绝（§9.5，用 config 开关验证或跳过标注）\n     h. machine 断线（kill machine 进程）→ 活 session 置 interrupted + gap；machine 重启重连 → 心跳恢复 → buffer_sync 补推（§8.2/§8.5）\n     i. cancel → 终态；close → kill ACP 进程 + session closed\n     j. keep_alive 超时 → 4501 关闭（§4.7，用短超时配置）\n     k. 契约层：§4.8 向量 1-5、8-11 中适合进程级验证的（协议版本拒绝、无 token 连接拒绝、action 方法面白名单外拒绝）\n3. 每个测试开头输出第一行固定格式「T-<name>: START」，结束输出「T-<name>: PASS/FAIL <原因>」；测试总超时 60s。\n4. 验证：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo build --workspace && cargo test -p acp-hub-server --test integration_tests 2>&1 | tail -30（先跑通再交付；若某用例因实现缺陷失败，记录「实现缺陷：<文件>:<原因>」到输出，不要在集成测试里绕过）。\n\n输出：测试文件清单、每个用例的结果（PASS/FAIL + 原因）、发现的实现缺陷清单（文件+现象+建议修复点）、遗留问题。`, { label: 'it-code', model: 'sonnet', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

phase('F7 test→fix→test 循环')

let rounds = 0
const MAX_ROUNDS = 4
let outcome = null

while (rounds < MAX_ROUNDS) {
  rounds += 1
  const fix = await agent(`你是 acp-hub 集成测试执行/修复 agent（第 ${rounds} 轮）。\n\n${COMMON}\n\n背景：server/tests/ 下已有集成测试（common/mod.rs + 场景用例）。上一轮遗留的实现缺陷与失败用例见你的探索。\n\n任务：\n1. 运行：cd /Users/konghayao/code/ai/perihelion/acp-hub && cargo test -p acp-hub-server --test integration_tests 2>&1 | tail -40。\n2. 若有失败：定位根因——区分「集成测试自身问题」（时序/端口/token/进程清理，改测试）与「实现缺陷」（server/machine/proto 代码 bug，改对应实现代码——此时允许修改 server/src、machine/src 下的被测代码，但必须最小修改并说明）。\n3. 修复后重跑全部集成测试，再跑 cargo test -p acp-hub-server && cargo test -p acp-machine && cargo clippy --workspace --all-targets -- -D warnings 确认无回归。\n4. 输出第一行必须是：「RESULT: PASS」或「RESULT: FAIL <剩余失败数>」；随后是：本轮修复清单（文件+修复内容）、剩余失败详情、遗留问题。`, { label: `fix-round-${rounds}`, model: 'sonnet', allowedTools: ['Read','Write','Edit','Bash','Glob','Grep','folder_operations'] })

  if (fix.trimStart().startsWith('RESULT: PASS')) {
    outcome = `PASSED after ${rounds} round(s)`
    break
  }
  outcome = `round ${rounds} still failing`
}

return {
  it_code,
  final: outcome,
  rounds_run: rounds,
}
