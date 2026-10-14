export const meta = {
  name: 's7-frontend-paging-and-compact',
  description: 'REQ-C01~C03: TUI scroll paging with stable-position merge and SDK incremental history projection; remove forced full reload after Compact',
}

const WORKTREE = '/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history'

const RULES = [
  '=== 环境与命令纪律（必须遵守） ===',
  '1. 所有 bash 命令必须以 cd ' + WORKTREE + ' && 开头：workflow 里 bash 的默认 cwd 是主仓库 /Users/konghayao/code/ai/peri-v4p3，不是 worktree。Read/Grep/Edit/Write 一律使用 worktree 绝对路径。',
  '2. Rust 面：构建与测试只能通过 ./scripts/cargo-rmcp-patched.sh（例：./scripts/cargo-rmcp-patched.sh check --locked -p peri-tui）。禁止 --workspace 级命令；一次只跑一个 cargo 进程。',
  '3. SDK 面：在 ' + WORKTREE + '/npm-packages/@peri-sdk 下用 bun（bun run typecheck、bun run build、bun test）。不要用 npm/yarn/pnpm 安装依赖，不要改 package.json 的依赖。',
  '4. 禁止任何 git 写操作：不 commit、不 stash、不 checkout、不 reset、不 clean、不建分支。只读 git（status/diff/log/show）允许。',
  '5. 禁止数据库表结构变更（DDL）；禁止新增依赖。只改本切片范围内的文件，不要顺手重构或删除既有死代码（例外：本任务明确要求删除的依赖路径，且你先确认了它的全部调用者）。',
  '6. ACP 既有 wire 字段名/错误码是契约：客户端只消费，不改服务端形状（服务端形状由本需求前序切片定稿）。',
  '',
  '=== 证据纪律 ===',
  '7. 报告必须区分「实测」（命令 + 输出片段）与「静态判断」（读代码得出）；不得把计划或推测写成结果。',
  '8. 没跑的验证必须写「未验证」。测试失败先判断是否本切片引入。',
  '9. 行为改变要写清「改动前 vs 改动后」，并指出对应需求条目（REQ-C01/C02/C03）与验收编号（A11/A12/A20/A22/A23/A24）。',
  '',
  '=== 预算 ===',
  '10. 预算约 200 次工具调用。按优先级交付：P1 必做、P2 尽力、P3 记录；不要为了赶进度写没有验证的断言。',
  '11. 用中文写报告，结构：范围 / 改动文件清单 / 改动前 vs 改动后 / 需求与验收映射 / 实测证据 / 未完成与未验证项 / 风险。报告写入指定文件，最终回复给出同一份摘要。',
].join('\n')

const RECON_POINTER = [
  '已有侦察底稿（**只作为线索，不作为权威**，每一项都要自己读代码核对）：' + WORKTREE + '/.recon/E-tui-sdk.md',
  '需求文档（只读）：/Users/konghayao/Desktop/peri-transcript-history-requirements.md（重点 §5.1 两种模式、§5.2 扩展字段与合并契约、§7 REQ-C01~C03、§9 验收 A11/A12/A20/A22/A23/A24）。',
  'ACP 侧分页契约的权威来源（已实现，不要改服务端）：peri-acp/src/host/history_page.rs 的模块文档（入口仍是 session/load；有 cursor 字段 = 历史页；响应形状 {sessionId, snapshotToken, nextCursor, items[{entryId, position, oversize, subItems[]}]}；-32011 跨会话、-32012 范围失效）。',
  '协商能力名：peri.historyPagination（PeriCaps::history_pagination）。未协商的连接带 cursor 会被服务端拒绝。',
].join('\n')

// ── S7a：TUI ─────────────────────────────────────────────────────────────────

const PROMPT_TUI = [
  '你在一个 git worktree 内实现 TUI 侧的前端切片（S7a）。目标：REQ-C01 滚动分页、REQ-C02 去掉「Compact 后强制完整 reload」、REQ-C03 不扩大架构改造。',
  '',
  'Worktree: ' + WORKTREE,
  '分支: feat/transcript-shrink-history-pagination（服务端切片 S1-S9 已在同一 worktree 落地且未提交，不要提交、不要改服务端）。',
  '',
  RECON_POINTER,
  '',
  '=== 任务（按优先级） ===',
  'P1. 删除「手动 Compact 完成后必须再触发完整 session/load」的依赖（REQ-C02 / A12）：',
  '   - 现状：peri-tui/src/kit/acp_events/turn.rs 的 handle_turn_done 在 compact_just_completed 置位时向 THREAD_LOAD_TX 发 session_id，consumer 随后调 client.load_session（整段 reset + 重放，会把已加载窗口整体替换、把视口拉到底）。',
  '   - 改为局部更新：Compact 完成不再触发 session/load；已显示的旧消息继续保留（模型上下文收缩 != 界面删除历史）；Compact 提示（PENDING_COMPACT_NOTE 及其展示语义）与有效上下文信息按既有文案在**当前视图**落地，行为与现状一致（手动 Compact 的提示必须仍然可见；自动 Compact 沿用现状展示，不新增展示面）。',
  '   - 先确认 THREAD_LOAD_TX / reserve_session_load / thread_load_consumer 的**全部调用者**：只有确实仅服务这条路径的部分才可以删除，其余保留（例如会话切换仍在用）。把调用者清单与删除范围写进报告。',
  '   - 若当前窗口已经由翻页扩大，Compact 完成不得把它整体替换成摘要，也不得把下一 cursor 跳回已加载区间（REQ-C02）。',
  '',
  'P2. 滚动分页（REQ-C01 / A11、A20、A23、A24）：',
  '   - 客户端入口：session/load + { sessionId, cursor, limit }（与初始 load 同一方法）。初始 load 响应里带 nextCursor / snapshotToken（协商连接才有），要接住并存起来。',
  '   - 向上滚动接近当前历史区间的向前边界（含稀疏首屏中的空缺边界）时发起一页；相同 cursor + 相同 limit 的并发请求必须合并；重试同一页不得产生重复消息。',
  '   - 合并：按 entryId 去重、按 position（同一次浏览内可比较的不透明存储序键）合并进正确区间；只有整页都早于当前最早条目才整体头插；阅读锚点用「稳定子项 ID + 子项内偏移」保持，不得跳动。',
  '   - 翻页 loading 与 Agent 执行状态分离（不得复用 turn 的 loading/停止按钮状态）；翻页失败保留当前画面、输入与运行状态并允许重试。',
  '   - 历史投影隔离：历史页不得重启工具动画、不得发起审批、不得改变当前 Todo/plan、任务状态、输入队列或当前 turn（旧 TodoWrite 的到达不得覆盖当前基线）。',
  '   - 失效丢弃：切换会话后拒绝上一会话的迟到页；同一会话 rewind 后拒绝旧范围的响应（服务端会对失效 cursor 返回 -32012 / 跨会话 -32011，客户端也要按会话身份与快照身份自行丢弃）。',
  '',
  'P3. 身份补充（只在必要时，最小改动）：',
  '   - TUI 重放/分页条目当前缺身份：CommittedAssistantText 不带 id（turn.rs 里恒 None）、TuiUserBubble 结构体没有 id 字段（bubble.rs）。去重与区间合并需要稳定身份时才补，且身份必须来自 wire（session_update.rs 已经从通知里读到 messageId）。不要发明新的服务端字段。',
  '',
  'P4. 测试（peri-tui 既有 kit/acp_events 测试风格；先看 peri-tui/src/kit 下现有测试怎么写）：',
  '   - 分页合并：乱序/重叠页、整页更早（头插）与中间填补、相同 cursor 并发合并、重试去重。',
  '   - 历史投影隔离：旧页到达不改变 Todo/plan、不建 turn、不触发审批。',
  '   - compact 局部更新：不再触发 load；提示仍在；已加载窗口保留。',
  '   - 迟到页丢弃：切换会话后旧页不入画面。',
  '',
  '硬约束（REQ-C03）：不引入 Yjs 分文档/轮换、不做全历史虚拟化、不新建前端持久化缓存；复用既有 BridgeState / ViewModel / 渲染组件；不得让历史投影走会推进执行、Todo 或审批状态的实时 handler。',
  '不要改服务端（peri-acp*、peri-resources、peri-agent 一律不动）；若发现服务端缺陷，写进报告，不要自行修改。',
  '',
  '报告文件：' + WORKTREE + '/.recon/S7a-tui.md',
  '必须实际跑并给出输出：',
  '- cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh check --locked -p peri-tui',
  '- cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- <本切片用例过滤串>',
  '',
  RULES,
].join('\n')

// ── S7b：SDK ─────────────────────────────────────────────────────────────────

const PROMPT_SDK = [
  '你在一个 git worktree 内实现 SDK 侧的前端切片（S7b，TypeScript / Yjs）。目标：REQ-C01 分页合并、REQ-C02 Compact 不再整表替换、REQ-C03 不扩大架构改造。',
  '',
  'Worktree: ' + WORKTREE,
  'SDK 包: ' + WORKTREE + '/npm-packages/@peri-sdk',
  '分支: feat/transcript-shrink-history-pagination（服务端切片已在同一 worktree 落地且未提交，不要提交、不要改服务端）。',
  '',
  RECON_POINTER,
  '',
  '=== 任务（按优先级） ===',
  'P1. 声明分页能力：@peri-sdk/src/agent/session.ts 的硬编码 capabilities 对象里补 peri.historyPagination: true（少声明一项就等于放弃该通道；服务端会拒绝未协商连接的 cursor 请求）。',
  '',
  'P2. Compact 不再整表替换（REQ-C02 / A12）：',
  '   - 现状：peri/agent_event 的 compact_completed.messages_json -> ChatProjection.replaceHistory（state/chat-projection.ts 的 replaceHistory 全量 replayEpoch++ / resetChat / 重建）。',
  '   - 改为局部：Compact 完成只更新 Compact 提示与有效上下文元数据、保留已加载窗口与 cursor 进度；不得清空消息区、不得调用完整 history replacement、不得批量中止交互。',
  '   - 自动 Compact 必须保留 active request/turn、流式输出与未决交互；Compact 失败只更新失败展示。',
  '   - rewind 与 Compact 分开处理：rewind 是范围真的变了（服务端推进结构版本、删除尾部行），客户端可以让受影响窗口失效并刷新，但同样不得清空实时执行状态或未决交互。写清两者的差异。',
  '   - 摘要按既有 Compact 展示语义处理，不伪装成用户新输入。',
  '',
  'P3. 初始 load 的代次暂存与丢弃（A22）：',
  '   - 现状：Session.startOnce 订阅后对每条通知立即 docs.accept(event)，load 响应到达后 completeTurn()。',
  '   - 改为：按本次 load 的 loadGeneration（响应 _meta.periLoadGeneration 与通知上的同名标记，见 peri-acp/src/host/requests/session_restore.rs 与 dispatch/session_replay.rs 的写入点）识别回放批次：暂存该代次的历史投影，收到成功响应且回放完整后一次安装；失败/取消/被新代次取代时丢弃暂存并保留原画面与实时状态；迟到回放不得污染新快照。',
  '   - 不允许先完成查询、再无保护地开始订阅而漏消息（订阅与代次边界要写清顺序）。',
  '',
  'P4. 分页合并（REQ-C01 / A11、A20、A23、A24）：',
  '   - 入口：session/load + { sessionId, cursor, limit }（同一方法）；响应 { sessionId, snapshotToken, nextCursor, items[{entryId, position, oversize, subItems[]}] }，其中 subItems 是回放路径同一份投影（sessionUpdate 等）。',
  '   - 合并：按 entryId + 子项位置幂等合并（不能只用 messageId 去重：一条 assistant 多 block 共享 messageId）；按 position 合并进正确区间；只有整页都早于当前最早条目才整体头插；页内保持正序。',
  '   - 历史投影模式：无需 Human 前导即可展示摘要/assistant/tool；不建立 active turn、不伪造用户输入、不推进 Todo/plan/任务状态/输入队列（旧 TodoWrite 到达不得覆盖当前任务基线）。',
  '   - 相同 cursor + 相同 limit 并发合并、重试去重；翻页失败保留画面与运行状态并可重试；切换会话后拒绝上一会话的迟到页；同一会话 rewind 后拒绝旧范围响应（服务端 -32011/-32012，客户端也要按会话与快照身份丢弃）。',
  '',
  'P5. 测试（bun；沿用 tests/ 下既有风格，例如 tests/session-docs-wire.test.ts、tests/session-view-incremental.test.ts）：',
  '   - 合并：重叠页、中间填补、整页更早头插、同 messageId 多 block 不互相覆盖、重复页幂等。',
  '   - Compact：不再触发整表替换；窗口与 cursor 进度保留；active turn / 未决交互保留。',
  '   - 代次：回放中途失败/被取代时暂存丢弃、旧画面保留；成功响应后一次安装、无重复。',
  '   - 迟到丢弃：切换会话/rewind 后旧页与旧代次通知不污染当前视图。',
  '',
  '硬约束（REQ-C03）：不引入 Yjs 分文档或轮换、不做全历史虚拟化、不新建前端持久化缓存；复用现有 ChatProjection / DocModel / view 投影。不改服务端。',
  '',
  '报告文件：' + WORKTREE + '/.recon/S7b-sdk.md',
  '必须实际跑并给出输出：',
  '- cd ' + WORKTREE + '/npm-packages/@peri-sdk && bun run typecheck',
  '- cd ' + WORKTREE + '/npm-packages/@peri-sdk && bun run build && bun test（给全量结果摘要；若某些用例本来就失败，先证明基线状态再报告）',
  '',
  RULES,
].join('\n')

// ── 验证 ─────────────────────────────────────────────────────────────────────

const PROMPT_VERIFY_TUI = [
  '你是独立复核者（verification），只审 TUI 侧（peri-tui）。目标：判定 REQ-C02 与 REQ-C01 的 TUI 侧是否真的达成、REQ-C03 是否被违反、既有行为是否回归。',
  '不要相信实现者报告，从 git diff 与实测出发。默认只读；发现缺陷写最小复现与影响面，不要自行修（除非是一行内笔误，修完必须跑测试并在报告标注）。',
  '',
  'Worktree: ' + WORKTREE,
  '需求文档：/Users/konghayao/Desktop/peri-transcript-history-requirements.md（§5.1、§5.2、§7 REQ-C01~C03、§9 的 A11/A12/A20/A23/A24）。',
  '侦察底稿（只作线索）：' + WORKTREE + '/.recon/E-tui-sdk.md；实现者报告：' + WORKTREE + '/.recon/S7a-tui.md。',
  '',
  '必做：',
  '1. 从 git diff 出发读 peri-tui 的改动，写清「改动前 vs 改动后」；指出哪些需求条目只是部分完成（PARTIAL）。',
  '2. REQ-C02：确认「手动 Compact 后强制完整 session/load」这条依赖真的不再存在（grep 全部调用者，给出证据）；确认已显示历史、已加载窗口、cursor 进度在 Compact 完成/失败后保留；确认 Compact 提示仍然可见（手动场景实测或读代码给出路径证据）。',
  '3. REQ-C01：核对分页触发边界、并发合并、重试去重、按 position 的区间合并与头插条件、锚点保持、翻页状态与执行状态分离、迟到页丢弃、历史投影隔离（旧页不改 Todo/plan/审批/turn）。能实测的写用例实测（可临时加 #[cfg(test)] 用例，报告标注是否保留）。',
  '4. REQ-C03：确认没有引入 Yjs 分文档/轮换、全历史虚拟化或新的前端持久化缓存；确认历史投影没有走会推进执行/Todo/审批状态的实时 handler。',
  '5. 回归：跑 peri-tui 的 lib 测试（给出命令与结果摘要）；核对既有测试断言是否被放宽或删除（逐条列出）。',
  '',
  '报告文件：' + WORKTREE + '/.recon/S7-tui-verification.md（逐条 PASS/PARTIAL/FAIL + 证据 + 未覆盖项；区分「实测」与「静态判断」）。',
  '命令：cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh check --locked -p peri-tui；cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib',
  '',
  RULES,
].join('\n')

const PROMPT_VERIFY_SDK = [
  '你是独立复核者（verification），只审 SDK 侧（npm-packages/@peri-sdk，TypeScript/Yjs）。目标：判定 REQ-C01/C02 的 SDK 侧是否真的达成、REQ-C03 是否被违反、bun 测试是否真绿。',
  '不要相信实现者报告，从 git diff 与实测出发。默认只读；发现缺陷写最小复现与影响面，不要自行修（除非是一行内笔误，修完必须跑测试并在报告标注）。',
  '',
  'Worktree: ' + WORKTREE,
  '需求文档：/Users/konghayao/Desktop/peri-transcript-history-requirements.md（§5.1、§5.2、§7 REQ-C01~C03、§9 的 A11/A12/A20/A22/A23/A24）。',
  '侦察底稿（只作线索）：' + WORKTREE + '/.recon/E-tui-sdk.md；实现者报告：' + WORKTREE + '/.recon/S7b-sdk.md。',
  '',
  '必做：',
  '1. 从 git diff 出发读 SDK 改动，写清「改动前 vs 改动后」。',
  '2. 能力声明：确认 peri.historyPagination 已声明（给出文件与行）；确认没有顺手删除其它既有能力项。',
  '3. REQ-C02：确认 Compact 完成不再调用完整 history replacement / resetChat（给出调用者证据）；确认自动 Compact 保留 active request/turn、流式输出与未决交互；确认 rewind 与 Compact 分流，且 rewind 也不清空实时执行状态；确认 Compact 失败只更新失败展示。',
  '4. A22 代次边界：确认回放按 loadGeneration 暂存、成功响应后一次安装、失败/取消/被取代时丢弃；构造或复用一个"回放中途失败/迟到代次"的用例实测（bun test），并确认订阅顺序不会漏消息。',
  '5. REQ-C01 合并：核对 entryId + 子项位置去重（同 messageId 多 block 不互相覆盖）、position 区间合并、头插条件、并发合并、迟到页与旧范围丢弃；能实测的写用例实测。',
  '6. REQ-C03：确认没有引入 Yjs 分文档/轮换、全历史虚拟化或新的持久化缓存。',
  '7. 回归：cd ' + WORKTREE + '/npm-packages/@peri-sdk && bun run typecheck && bun run build && bun test（给出结果摘要；若有用例失败，判断是否本切片引入，并给基线证据）。',
  '',
  '报告文件：' + WORKTREE + '/.recon/S7-sdk-verification.md（逐条 PASS/PARTIAL/FAIL + 证据 + 未覆盖项；区分「实测」与「静态判断」）。',
  '',
  RULES,
].join('\n')

// ── 编排 ─────────────────────────────────────────────────────────────────────

phase('S7 前端实现（TUI ∥ SDK）')
log('S7a TUI: remove forced reload after Compact, scroll paging with stable-position merge')
log('S7b SDK: caps + incremental history projection, compact/rewind split, loadGeneration staging')
const [tui, sdk] = await parallel([
  () => agent(PROMPT_TUI, { label: 's7a-tui' }),
  () => agent(PROMPT_SDK, { label: 's7b-sdk' }),
])

phase('S7 独立验证')
log('verification: TUI and SDK in parallel')
const [verifyTui, verifySdk] = await parallel([
  () => agent(PROMPT_VERIFY_TUI, { label: 'verify-s7-tui' }),
  () => agent(PROMPT_VERIFY_SDK, { label: 'verify-s7-sdk' }),
])

return {
  s7a_tui: tui,
  s7b_sdk: sdk,
  verification_tui: verifyTui,
  verification_sdk: verifySdk,
}
