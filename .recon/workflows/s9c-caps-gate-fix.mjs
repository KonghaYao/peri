export const meta = {
  name: 's9c-connection-caps-gate-fix',
  description: 'REQ 5.3 / A25: gate replay markers (periReplay/periLoadGeneration) on the current connection caps instead of the session-level caps registry, plus the doc/flake follow-ups from the two S9 verification reports',
}

const WORKTREE = '/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history'
const CD = 'cd ' + WORKTREE + ' && '
const CARGO = './scripts/cargo-rmcp-patched.sh'

const BRIEF = [
  'worktree（唯一工作目录，绝对路径）：' + WORKTREE,
  '所有 bash 命令必须以 `cd ' + WORKTREE + ' && ` 开头（默认 cwd 不是这里）；Cargo 一律用 `./scripts/cargo-rmcp-patched.sh`（禁止裸 cargo、禁止 --workspace、禁止 --no-verify 类绕过）。',
  '禁止：git 写操作（工作区保持未提交）、DDL、新增依赖、顺手重构无关代码。',
].join('\n')

const PROMPT_FIX = [
  '你是实现者。先读后改，动手前把证据读全。',
  '',
  BRIEF,
  '',
  '## 背景：独立验证发现的需求违背（报告 .recon/S9-verification.md 第 7.1 / 8 / 10 节）',
  '需求 §5.3 原文（外部权威，逐字）：能力版本以**当前连接**的协商结果为准，不能继承会话内保存的旧客户端能力。未协商成功却提供分页字段时明确拒绝；重连后重新按本连接能力分流。',
  '现状：peri-acp/src/host/requests/session_restore.rs 的 handle_load 里 `let caps = cfg.session_manager.ensure_session_caps(req_session_id);`（约 527 行）读的是**会话级** caps 注册表（peri-acp/src/session/caps.rs:82：命中旧值直接返回）。',
  '这个 caps 被传给回放入口（同文件约 566 / 573 行的 dispatch::replay_persisted_session_history 与 dispatch::replay_canonical_history_streamed）、响应戳（约 640 行 `if caps.replay { response[_meta][periLoadGeneration] }`），以及 dispatch/session_replay.rs:596 的 replay_meta（门控 periReplay）。',
  '后果（已实测复现，见被 #[ignore] 的反例探针）：同一进程内 peri 客户端（TUI/MPSC，未 initialize → all_enabled）先加载过某会话后，注册表里存下 replay=true；随后一个**未协商**的标准 ACP 连接（stdio，initialize 不带 peri 扩展）session/load 同一会话时，收到 periReplay（4 条通知）与 periLoadGeneration（响应 + 通知）。分页四项字段（nextCursor/snapshotToken/periEntryId/periPosition）没有泄漏。',
  '这与 peri-acp/src/dispatch/session_replay.rs:25 的模块文档（判据必须来自**当前连接**的协商事实，不得回读进程 / 会话级 caps 注册表）自相矛盾——本切片新增的 periLoadGeneration 挂在错误来源的同一个门上。',
  '',
  '## 任务 1：把回放标记的协商判据改成当前连接（核心）',
  '1. 先读：peri-acp/src/host/connection.rs（ConnectionContext::caps / history_pagination / ConnectionFacts，以及文件顶部关于 negotiated_caps 的注释）、peri-acp/src/host/requests.rs（session/load 分派点，约 118-128 行）、peri-acp/src/host/requests/session_restore.rs（handle_load 全函数 + TuiReplaySender）、peri-acp/src/host/history_page.rs（模式分流与 attach_initial_cursor）。',
  '2. ConnectionFacts 增加本连接的 caps 字段，取值来自 ConnectionContext::caps()。注意该方法的既有语义：未 initialize 的进程内路径（MpscTransport / TUI）返回 all_enabled —— 这是 TUI 的历史全能力语义，必须保留；history_pagination 的失败关闭语义（必须成功 initialize 且声明该键）不得改动、不得改用 ConnectionContext::caps() 推导。',
  '3. session/load 分派点把该值传进 handle_load。',
  '4. handle_load 里**只把回放相关的 caps** 换成连接 caps：两个 replay_* 调用、响应 _meta 的 periLoadGeneration 戳。',
  '5. send_available_commands_update(...) 那处**保持现状**（会话级命令注册表语义，属另一功能面、不在本需求范围）：请加一行注释说明此处刻意仍是会话级来源、与回放标记不同，避免后来者误改。',
  '6. 变量命名自解释（例如 session_caps / connection_caps），不要留裸 caps 让人猜。',
  '7. 修一类而不是修一个：grep 生产代码里的 periReplay / periLoadGeneration 全部发射点，确认没有别的路径用会话注册表 caps 门控回放标记；有就一并修，并在报告里列出你排查过的每一处（含判定理由）。',
  '',
  '## 任务 2：把反例探针变成回归用例',
  'peri-acp/src/host/history_page_wire_test.rs 约 911-940 行的 s9_recheck_session_caps_do_not_leak_peri_private_fields_to_an_unnegotiated_connection：去掉 #[ignore]（连同它的 ignore 说明），让它作为回归用例断言修复后的行为（未协商连接不得收到 periReplay / periLoadGeneration / 分页四项；正文仍是完整 canonical）。',
  '同文件补一条正向用例：同一场景下 peri 协商连接（或未 initialize 的进程内路径）**仍然**收到 periReplay 与 periLoadGeneration —— 证明门控没被改死。',
  '',
  '## 任务 3：四处理论上独立的次要修正（逐条对应验证报告的条目号，都是小改）',
  '1. R2（doc 收紧）：peri-acp-types/src/session_resources.rs 里 load_history_segment 的 doc 把「阶段一不取正文」改成事实：length(CAST(content AS BLOB)) 会关掉 SQLite 的 length-arg 优化，引擎仍可能把候选行正文读进寄存器，但不越过 SQL 边界、不进解码器（依据 .recon/S9-boundedness.md 第 3.4 节第 3 点与第 6 节 R2）。',
  '2. R1（doc 收紧，不改语义）：.recon/S9-boundedness.md 第 6 节 R1 实测反驳了「回放期间行集合同源」——回放中途 rewind 删掉 tail 行会让 SQLite 复用其 rowid，新行落进 (after, tail] 并进入本次回放，被删行静默消失。把 peri-acp-types/src/session_resources.rs 与 history_page.rs 中相关措辞改成事实：追加行不越过 tail 成立；「tail 行被删后 rowid 复用」是已知边界。**不要**加校验、不要改语义（那是设计变更，另外评估）。',
  '3. R9（指针修正）：peri-resources/src/sessions/sqlite_store/session_history_segment_test.rs 第 4 行指向不存在的 remote/session_history_segment_test.rs，改成实际存在的 remote/session_history_page_test.rs。',
  '4. R8（测试 flake）：peri-acp/src/host/requests_unnegotiated_replay_test.rs 的 streamed_canonical_replay_falls_back_to_the_full_read_when_the_segment_read_is_unsupported 依赖日志捕获（LogBuffer），默认并发下会 flake（.recon/S9-boundedness.md 第 6 节 R8）。保留产品断言（segment_calls / full_reads）为主判据，把日志断言改成不依赖全局 callsite 缓存的稳定形式（或去掉日志文本断言并用调用记录断言替代）。目标：同一条命令连跑 3 次稳定通过。',
  '',
  '## 任务 4：实测验证（每条都要在报告里给命令原文 + 结果原文）',
  '1. ' + CD + CARGO + ' check --locked -p peri-acp',
  '2. ' + CD + CARGO + ' test --locked -p peri-acp --lib -- history_page_wire（含改后的探针与新增正向用例）',
  '3. ' + CD + CARGO + ' test --locked -p peri-acp --lib -- replay',
  '4. 任务 3 第 4 条那条命令连跑 3 次（记录每次结果）',
  '5. 全量 1 次：' + CD + CARGO + ' test --locked -p peri-acp --lib（若出现 flake，单跑对照并如实记录，不要只报最终那次）',
  '注意：cargo 会争抢 target 目录锁，命令串行执行、不要并发起多个 cargo。',
  '',
  '## 任务 5：报告',
  '写到 ' + WORKTREE + '/.recon/S9c-caps-gate-fix.md：',
  '1. 改动清单（绝对路径 + 一行说明）',
  '2. 契约与不变量（回放标记判据、分页准入、TUI/MPSC 语义各自的门与理由）',
  '3. 实测证据（命令 + 结果原文；flake 要写清第几次失败、单跑结果）',
  '4. 未验证与不确定项',
  '5. 风险',
  '报告要能让独立复核者仅按命令复现，不要写没有命令支撑的结论。',
].join('\n')

const PROMPT_VERIFY = [
  '你是独立复核者（verification），默认只读、不采信实现者报告，结论必须从代码与实测重新得出。',
  '',
  BRIEF,
  '',
  '## 复核对象',
  'S9c：把回放标记（periReplay / periLoadGeneration）的协商判据从**会话级 caps 注册表**改为**当前连接**的协商结果，以满足需求 §5.3（能力版本以当前连接的协商结果为准，不能继承会话内保存的旧客户端能力）与验收 A25。实现者报告在 .recon/S9c-caps-gate-fix.md（当参考，不当代替）。',
  '你要读生产代码自己判定：peri-acp/src/host/requests/session_restore.rs（handle_load）、peri-acp/src/host/connection.rs（ConnectionFacts / ConnectionContext::caps / history_pagination）、peri-acp/src/host/requests.rs（session/load 分派点）、peri-acp/src/dispatch/session_replay.rs（replay_meta 与投影）。',
  '',
  '## 逐条判据（每条给 PASS / FAIL / PARTIAL + 静态还是实测）',
  '1. 回放标记的门确为当前连接：读代码给出「本连接 caps 从哪来、如何流到 replay_meta 与响应戳」的完整链路；并说明未 initialize 的进程内路径（TUI/MPSC）与 stdio 未协商 / stdio 已协商三类连接各自拿到什么。',
  '2. 三类连接的 wire 字段集合实测（真实 ServerLoop + 真实存储，夹具要含 Full 归档原文）：',
  '   a) 会话注册表已被先前 peri 连接写过时，未协商连接（initialize 不带 peri 扩展）的 session/load：响应与全部 session/update 通知**不得**出现 periReplay / periLoadGeneration / nextCursor / snapshotToken / periEntryId / periPosition，且正文仍是完整 canonical（含归档原文）；',
  '   b) 同一场景下协商连接的 session/load：**仍必须**出现 periLoadGeneration（及分页字段），回放标记不得被改死；',
  '   c) 未 initialize 的进程内路径：保持历史全能力语义（periReplay / periLoadGeneration 照旧）。',
  '   逐条给用例名 + 命令 + 真实结果。若既有用例覆盖不全，自己补用例（在报告里标注为复核者新增，并说明它断言了什么、为什么不是重复）。',
  '3. 回归：协商路径的既有断言有没有被放宽或删除（用只读 git diff 对照工作区，禁止 git 写操作）；分页准入（history_pagination 的失败关闭）与 §5.1 模式分流有没有被顺带改动。',
  '4. 修一类是否成立：grep 生产代码的全部 periReplay / periLoadGeneration 发射点，逐一判定门控来源是否正确；列出被漏掉的点（若有）。',
  '5. 无 DDL、无新依赖：给出检查命令与输出。',
  '6. 全量 1 次：' + CD + CARGO + ' test --locked -p peri-acp --lib，记录 passed / failed；任何 flake 给单跑对照。',
  '注意：cargo 会争抢 target 目录锁，命令串行执行、不要并发起多个 cargo。',
  '',
  '## 报告',
  '写到 ' + WORKTREE + '/.recon/S9c-verification.md：判据表（PASS/FAIL/PARTIAL + 依据类型）、反例（若有，给最小复现命令）、未验证与不确定项、结论。结论要明确回答：§5.3 的「不能继承会话内保存的旧客户端能力」现在是否对**全部** Peri 私有回放字段成立，还是仍有残留。',
].join('\n')

phase('S9c 连接级 caps 门控修复')
const fix = await agent(PROMPT_FIX, { label: 's9c-caps-gate-fix' })

phase('S9c 独立复核')
const verify = await agent(PROMPT_VERIFY, { label: 's9c-verify' })

return { fix, verify }
