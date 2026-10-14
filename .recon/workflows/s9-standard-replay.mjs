export const meta = {
  name: 's9-standard-client-full-replay',
  description: 'D7 / REQ 5.3: streamed canonical full replay for standard (unnegotiated) ACP clients over a forward bounded segment read',
}

const WORKTREE = '/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history'

const RULES = [
  '=== 环境与命令纪律（必须遵守） ===',
  '1. 所有 bash 命令必须以 cd ' + WORKTREE + ' && 开头：workflow 里 bash 的默认 cwd 是主仓库 /Users/konghayao/code/ai/peri-v4p3，不是 worktree。Read/Grep/Edit/Write 一律使用 worktree 绝对路径。',
  '2. 构建与测试只能通过 ./scripts/cargo-rmcp-patched.sh（例：./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp-types）。禁止 --workspace 级命令；一次只跑一个 cargo 进程；cargo 用前台执行并给足超时（编译可能数分钟）。',
  '3. 禁止任何 git 写操作：不 commit、不 stash、不 checkout、不 reset、不 clean、不建分支、不改 git config。只读 git（status/diff/log/show）允许。',
  '4. 禁止数据库表结构变更（DDL）：不加表、不加列、不加索引、不改 schema。禁止新增依赖（不动任何 Cargo.toml 的依赖项）。',
  '5. 只改本切片范围内的文件。发现无关问题写进报告，不要顺手重构、不要"改进"邻近代码、不要删除既有死代码。',
  '6. ACP 响应与通知的既有字段名/错误码是契约，只能按本任务明确要求改动。',
  '',
  '=== 证据纪律 ===',
  '7. 报告必须区分「实测」（给出命令与输出片段）与「静态判断」（读代码得出）；不得把计划、推测或"应该会"写成结果。',
  '8. 任何"通过"的结论必须附命令与关键输出；没跑的验证必须写"未验证"。',
  '9. 测试失败先判断是不是本切片引入，不要为了过测试改语义或放宽断言。',
  '',
  '=== 预算 ===',
  '10. 预算约 200 次工具调用。先跑最快定向验证（单 crate check + 目标单测），再扩大范围；不要在一次 bash 里串多条 cargo。',
  '11. 用中文写报告，结构：范围 / 改动文件清单（绝对路径 + 一句话）/ 契约与不变量 / 实测证据（命令 + 结果）/ 未验证与不确定项 / 风险。报告写入指定文件，最终回复给出同一份摘要。',
].join('\n')

const DELIVERABLE_NOTE = [
  '',
  '交付物必须以文件形式落盘（报告路径见任务）。不要修改 .recon/ 之外的报告文件。',
  '不要修改 docs/standards、docs/code-index 之外的文档；若确实需要更新 code-index 或协议文档，只做与本改动直接对应的最小补充，并在报告中列出。',
].join('\n')

// ── S9a：存储与契约 ───────────────────────────────────────────────────────────

const PROMPT_S9A = [
  '你在一个 git worktree 内实现一个已经设计定稿的存储与契约切片（S9a）。不要重新设计整体方案，只落地下面写明的契约。',
  '',
  '=== 任务背景（已核实的事实，不必重新考证） ===',
  'Worktree: ' + WORKTREE,
  '分支: feat/transcript-shrink-history-pagination（有大量未提交改动，属于本需求的前序切片 S1-S8；不要提交）。',
  '',
  '需求矛盾点：Full Compact 之后核心只保留"有效集合"。当前 session/load 对所有连接只回放 SessionState.history_payloads（有效集合：继承区 + 未排除自有 payload），于是**未协商 peri.historyPagination 的标准 ACP 客户端**会拿到被截断的历史却收到成功响应。',
  '需求 §5.3 原文要求：未协商的通用 ACP 客户端仍使用标准完整回放投影，不得收到分页私有语义并被误告知已完成标准全量回放；标准路径可以通过有界读取流式输出，不要求核心重新持有完整历史。',
  '',
  'canonical 完整历史的既有事实源是 SessionResources::load_session_history：继承区在前（threads.inherited_context 单行 JSON 快照，见 peri-resources/src/sessions/sqlite_store/context.rs 的 load_inherited_context_on）+ 自有行按 rowid 升序（同文件 load_payloads_on，**不过滤 excluded**）。旧行为就是这条读取（可用 git show HEAD:peri-acp/src/dispatch/session_load.rs 核对）。它的问题是"一次性物化全历史"，本切片要补一条与之等价的**向前有界读取**。',
  '',
  '范本（必须同构，不要另创造抽象）：',
  '- 契约：peri-acp-types/src/session_resources.rs 的 load_history_page / history_page_bounds / load_entry_positions（含默认实现与其 doc comment 的写法）。',
  '- 类型：peri-acp-types/src/history_page.rs（HistoryPage / HistoryPageRow / HistoryPagePlan / materialize）。',
  '- SQLite 实现：peri-resources/src/sessions/sqlite_store/session_data/history.rs（两阶段读取：阶段一只读 id + rowid + length，阶段二只取本段正文；SQL 常量与 GUARD 风格）。',
  '- 映射层：peri-resources/src/sessions/sqlite_store/session_data.rs 与 sessions/data.rs、sessions/resources.rs 的转发。',
  '- Remote 实现：peri-resources/src/sessions/remote/session_read.rs 与 remote/session_sql.rs、remote/session_data.rs。',
  '',
  '=== 要实现的内容 ===',
  'A. 契约类型（peri-acp-types/src/history_page.rs，命名与文档按下面写死；只允许极小命名微调）',
  '1) HistoryForwardCursor { after: MessageId, tail: MessageId }：两者都是存储序键的字符串形式。tail 是"本次完整回放的固定上界"，由首段读取确定，随后续游标透传。',
  '2) HistorySegment { payloads: Vec<PersistedPayload>, next_cursor: Option<HistoryForwardCursor> }。',
  '3) 构造/解码函数与校验风格沿用既有（不要新造错误类型；复用 SessionResourceResult 与既有错误映射；HistoryForwardCursor 需要可编码/可解码，形式与 HistoryCursor 同风格但不得与它混用）。',
  '',
  'B. trait 方法（peri-acp-types/src/session_resources.rs，紧跟 load_history_page 之后）',
  'async fn load_history_segment(&self, id: &ThreadId, cursor: Option<&HistoryForwardCursor>, limit: usize, byte_budget: usize) -> SessionResourceResult<HistorySegment>',
  '默认实现返回 SessionResourceErrorKind::Unsupported（与 load_history_page 一致）。doc comment 必须写全以下语义：',
  '- cursor = None：本次回放的首段。返回**全部继承区 payload**（来自单行 inherited_context 快照；继承区不在本切片做分页）+ 自有行中 rowid 最小的若干行；同一时刻把 tail 定为该线程自有行当前最大 rowid。',
  '- cursor = Some：从 after 之后继续（rowid > after），且**不得越过 tail**（判定用 rowid <= tail，不要求 tail 那行仍存在）。',
  '- 不过滤 excluded、不过滤 System / model-only reminder：这是 canonical 完整回放，可展示性过滤属于调用方（ACP 投影）。',
  '- 有界：自有行一次最多 limit 行，且段内正文编码字节之和不超过 byte_budget；单条超预算时独占一段并完整交付正文（完整回放不允许静默丢内容）。两阶段读取：阶段一最多取 limit + 1 行（多读的那一条只用于判断"还有没有更多"），阶段二只取本段那些行的正文。',
  '- 锚点消失失败关闭：after 指定的行已不在存储里 → 返回失败（不得回退成从头开始，不得用空段冒充结束；与 load_history_page 的游标过期同一处置）。',
  '- next_cursor = None 只表示"没有更多自有行了"（已到 tail 或本来就没有行）。',
  '- 首段必须是同一次一致读取：tail 与返回的行必须同源（同一事务/同一连接作用域读取）。',
  '',
  'C. 两个生产后端',
  '1) SQLite：在 session_data/history.rs 增加 read_history_segment 与 SQL 常量（与既有常量同风格，导出 pub(in crate::sessions) 供计划证据使用）。谓词形状：',
  '   - 非首段：SELECT ... FROM messages WHERE thread_id = ?1 AND rowid > ?2 AND rowid <= ?3 ORDER BY rowid ASC LIMIT ?4',
  '   - 首段：同形但去掉 rowid > ?2，且 ?3 由同一次事务内的 MAX(rowid)（WHERE thread_id = ?1）子查询给出。',
  '   阶段二只取本段 id 的正文（复用既有 phase-2 语句风格）。不得新增索引、不得改 schema。',
  '2) Remote：在 remote/session_read.rs + remote/session_sql.rs 按既有分页语句的批量/守卫风格实现同一语义；远端批内的边界守卫复用既有做法（例如 GUARD_BOUNDARY_ROWID_SQL 风格），不要另造一套。',
  '3) 转发：sessions/data.rs、sessions/resources.rs 以及 sqlite_store/session_data.rs、remote/session_data.rs 的 trait 实现各加一处转发，与 load_history_page 的转发位置一一对应。',
  '4) 若某后端确实无法在既有结构下表达 tail 上界，不要降级语义（不得用"读到没有为止"冒充）；记录下来并在报告中说明，先实现能正确表达的那部分（宁可该后端返回 Unsupported）。',
  '',
  'D. 测试（放在各自 crate 既有的 #[cfg(test)] 模块里，沿用既有命名与夹具风格）',
  '1) SQLite 语义：空线程（无自有行）；只有继承区；多段遍历把全部自有行按 canonical 顺序读齐（含 excluded 行与 System 行）；遍历期间插入新行不进入本次遍历（tail 上界生效）；after 锚点行被删除 → 失败；单条超 byte_budget → 独占一段且正文完整；byte_budget 使段长由字节而非 limit 决定；trait 默认实现（测试替身）返回 Unsupported。',
  '2) Remote：与 SQLite 同语义的最小契约测试（至少：多段遍历、不过滤 excluded、锚点消失失败），并断言两后端同形。',
  '3) 计划证据：把新增语句纳入既有 EXPLAIN QUERY PLAN 证据路径（session_history_perf_test.rs 的 assert_bounded_plan / assert_located_by_index 或同风格的小 helper），断言按索引定位、无 SCAN messages。不要跑 --ignored 的大夹具 perf 用例。',
  '',
  'E. 硬约束：不新增依赖；不做 DDL；不改既有 wire 形状；不修改 load_history_page / load_session_history 的既有语义（只能新增）。',
  DELIVERABLE_NOTE,
  '',
  '报告文件：' + WORKTREE + '/.recon/S9a-storage-contract.md',
  '',
  '必须实际跑并给出输出的验证命令：',
  '- cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp-types',
  '- cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh check --locked -p peri-resources',
  '- cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib -- history_segment（按实际用例名调整过滤串）',
  '- cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- history_segment',
  '',
  RULES,
].join('\n')

// ── S9b：ACP 接线 ─────────────────────────────────────────────────────────────

const PROMPT_S9B = [
  '你在一个 git worktree 内实现切片 S9b：把 session/load 的**未协商连接**分支改成 canonical 完整回放的流式投递。存储契约由 S9a 提供（同一 worktree，已落盘）。',
  '',
  'Worktree: ' + WORKTREE,
  '分支: feat/transcript-shrink-history-pagination（大量未提交改动属于本需求前序切片；不要提交）。',
  '',
  '=== 先确认 S9a 的产物（读代码，不要假设） ===',
  'peri-acp-types/src/session_resources.rs 的 trait 方法 load_history_segment(id, cursor, limit, byte_budget) 与 peri-acp-types/src/history_page.rs 的 HistoryForwardCursor / HistorySegment：首段返回全部继承区 payload + 最旧若干自有行并固定 tail 上界（canonical 完整历史，含 Full 归档原文）；后续段按 rowid 升序推进且不越过 tail；anchor 消失 → 失败；后端不支持 → Unsupported。',
  '若 S9a 的签名与此有出入，以工作区实际代码为准，并把差异写进报告（不要改 S9a 的契约）。',
  '',
  '=== 任务 ===',
  '1. 分流（peri-acp/src/host/requests/session_restore.rs 的 handle_load，判据是连接级事实 history_pagination）：',
  '   - 协商（true）：行为**完全不变**——有效集合回放 + ReplayMarks 的 entryId/position + 响应 _meta.periLoadGeneration + 首游标。',
  '   - 未协商（false）：不再用 state.history_payloads（它只是有效集合）回放；改为循环调用向前段读取、逐段投影成标准 session/update 通知。通知与响应里不得出现任何 Peri 私有分页字段（periEntryId / position / periLoadGeneration / nextCursor / snapshotToken），响应体形状与改动前一致。',
  '   - 片段大小取命名常量（与 history_page.rs 的 DEFAULT_PAGE_LIMIT / PAGE_BYTE_BUDGET 同一量级，例如 REPLAY_SEGMENT_ROWS = 50、REPLAY_SEGMENT_BYTE_BUDGET = 512 * 1024），常量位置与命名注释写明来源与理由。',
  '',
  '2. 流式回放入口（peri-acp/src/dispatch/session_replay.rs，与 replay_persisted_session_history 并列）：',
  '   - 签名大致 async fn replay_canonical_history_streamed(controller, session_id, sender, caps) -> Result<(), ReplayError>；',
  '   - 循环：load_history_segment(id, cursor, rows, bytes) → project_payload_updates 逐条投影发送 → cursor = seg.next_cursor；循环只在内存里持有当前一段（不得先取全量再切段）。',
  '   - Unsupported（后端不具备该读取）：显式回退到 load_session_history 全量读取后按同一投影回放，并写一条明确日志（区分"能力缺失的回退"与"正常流式"）。不得把 Unsupported 静默当成"没有更多历史"。',
  '   - 其他错误（Corrupt / Unavailable / 锚点消失 / 读失败）：本次 load 必须失败（-32603 + 日志），不得回一个成功响应、不得留下"半份首屏 + 成功"的组合（与既有回放失败语义一致）。',
  '   - ACP 侧不直操存储：一切存储访问经 Controller::sessions()（与 dispatch::session_load 现有函数同风格）。',
  '',
  '3. 幂等与语义边界：',
  '   - AlreadyLive（会话已在内存）分支：未协商连接的 canonical 回放同样从存储流式读取；不得回退成只重放 history_payloads。',
  '   - tail 上界语义：本次交付的是「load 开始时刻的持久前缀」，回放期间新落盘的行不进入本次交付（避免与实时通知重复）。请核对写屏障路径（peri-acp/src/host/prompt.rs 里 result.persisted_payloads 的发布时机）确认这不会丢掉"已发给客户端但未落盘"的内容，并把结论写进 doc comment。',
  '   - 继承区在本切片不分页（S9a 首段整体返回）；在 doc comment 里写明这是已知边界，不是遗漏。',
  '',
  '4. 测试（peri-acp 侧，沿用既有 host/dispatch 测试风格；参考 requests_history_lifecycle_test.rs、requests_test.rs、session_replay_test.rs 的夹具构造方式）：',
  '   - 未协商连接 + 含 Full 归档（excluded 行）的会话：回放通知**包含**归档原文，且逐条内容与顺序等于 load_session_history 的 canonical 序列（正文一并断言）。',
  '   - 通知与响应中不含 periEntryId / position / periLoadGeneration（wire 级断言）。',
  '   - 有界证据：用记录调用的测试替身断言"段读取被多次调用、没有任何一次返回全量"；并断言未协商路径上**不调用** load_session_history（生产后端路径）。',
  '   - 协商连接回归：既有断言（有效集合 + marks + 首游标 + periLoadGeneration）继续通过且语义不变。',
  '   - 失败关闭：段读取失败（含锚点消失）→ load 返回错误，且不产生半份首屏 + 成功响应。',
  '   - 边界：空会话、只有继承区、单条超大正文独占一段仍被完整交付。',
  '   - 回退路径：替身返回 Unsupported 时仍能完整回放（经全量读取），并有日志。',
  '',
  '5. 硬约束：不改既有 wire 字段名与错误码；不改协商路径语义；不新增依赖；不 DDL；不改 S9a 的契约（需要扩展就停手并在报告里说明）。',
  DELIVERABLE_NOTE,
  '',
  '报告文件：' + WORKTREE + '/.recon/S9b-acp-streaming-replay.md',
  '',
  '必须实际跑并给出输出的验证命令：',
  '- cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp',
  '- cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- <本切片用例过滤串>',
  '- cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- replay（回放相关面回归）',
  '',
  RULES,
].join('\n')

// ── 验证 ─────────────────────────────────────────────────────────────────────

const PROMPT_V1 = [
  '你是独立复核者（verification）。目标：判定需求 §5.3（D7）是否真的达成——**未协商（标准）ACP 客户端**在 session/load 上拿到完整 canonical 历史、没有被误告知、没有收到分页私有语义；协商路径行为完全不变。',
  '不要相信实现者的报告，从代码与实测出发。默认只读；只有在发现一行内的明显笔误且你已确认时才修，修完必须跑测试并在报告标注。',
  '',
  'Worktree: ' + WORKTREE,
  '分支: feat/transcript-shrink-history-pagination（前序切片 + S9a/S9b 均未提交，git diff 可见）。',
  '需求文档（只读）: /Users/konghayao/Desktop/peri-transcript-history-requirements.md（重点 §5.3、§9 与 A25）。',
  '',
  '必做：',
  '1. 从 git diff 出发独立读代码（peri-acp、peri-acp-types、peri-resources）。先写清"改动前行为"与"改动后行为"的对照（改动前可用 git show HEAD:<path> 读旧版本）。',
  '2. 「完整」判定：构造/复用一个含 Full 归档（excluded 行）的会话夹具，独立断言未协商连接的回放序列 == load_session_history 的 canonical 输出（条数、顺序、正文）。需要临时用例就写（保留在 #[cfg(test)] 内并在报告标注）。',
  '3. 「无误告知」判定：只有真的交付完整历史才出现成功响应；段读取失败/锚点消失必须让 load 失败。构造失败路径实测，不要只看代码。',
  '4. 「无私有语义」判定：未协商连接的通知与响应体里没有 periEntryId / position / periLoadGeneration / nextCursor / snapshotToken；作为对照，协商连接仍然带这些字段。',
  '5. 协商路径回归：核对测试文件 diff 是否被放宽或删除断言（逐条列出），并实跑相关面。',
  '6. 无 DDL / 无新依赖：核对 schema 文件与所有 Cargo.toml 无改动（给出命令与输出）。',
  '7. 若发现反例（比如某条路径仍回放有效集合、某处仍会误报成功、某后端未实现却静默降级），给出最小复现与影响面；不要自行修（除非是一行内笔误）。',
  '',
  '报告文件：' + WORKTREE + '/.recon/S9-verification.md（逐条 PASS / PARTIAL / FAIL + 命令与输出片段 + 未覆盖项；区分「实测」与「静态判断」）。',
  '',
  '验证命令基线（串行跑，不要并发多个 cargo）：',
  '- cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp-types',
  '- cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp',
  '- cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh check --locked -p peri-resources',
  '- cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- <相关过滤串>',
  '',
  RULES,
].join('\n')

const PROMPT_V2 = [
  '你是独立审计者（verification）。目标：证明或反驳"标准客户端完整回放是**有界读取**、不物化全历史"，并独立复核新增存储读取的实现与计划证据。',
  '默认只读；不要为了让结论好看而放宽断言。发现缺陷写最小复现与影响面，不要自行修（除非是一行内笔误）。',
  '',
  'Worktree: ' + WORKTREE,
  '分支: feat/transcript-shrink-history-pagination（S9a 存储契约 + S9b ACP 接线均未提交）。',
  '新增读取的契约在 peri-acp-types/src/session_resources.rs 的 load_history_segment（含 doc comment 的语义），类型在 peri-acp-types/src/history_page.rs；两后端实现在 peri-resources/src/sessions/{sqlite_store/session_data/history.rs, remote/session_read.rs}。',
  '',
  '必做：',
  '1. 实跑三 crate 的 check 与相关 lib 单测（给出命令与结果摘要，串行执行，不要并发多个 cargo）。',
  '2. 计划证据：对新增的向前读取语句取 EXPLAIN QUERY PLAN 原文（沿用 session_history_perf_test.rs 的 helper 风格写一条 #[ignore] 用例 + 小夹具跑一次，或用既有 helper 直接在测试内打印），断言按索引定位、无 SCAN messages；把语句原文与计划原文贴进报告。若计划显示触达行数与索引覆盖不足，直接写出来。',
  '3. 有界性实测：段长与字节预算真的生效（构造多条大行，实测每段行数与字节）；tail 上界在"回放期间插入新行"时不越过；after 锚点消失失败关闭；阶段二只取本段正文（给出代码路径证据 + 如果有办法，实测多读那一条正文不进内存）。',
  '4. 反例审计：空线程 / 只有继承区 / 单条超大正文 / excluded 行与 System 行都在段内 / Remote 与 SQLite 语义一致（对照两实现 SQL 与契约测试）。',
  '5. 诚实性审计：区分「内存与解码字节有界」与「存储触达行数有界」。在 messages 只有 idx_messages_thread_id 索引（thread_id 一列）时，WHERE thread_id = ? AND excluded = 0 之类的谓词仍需逐行回表判 excluded —— 触达行数与该线程总行数同阶；恢复路径同理（与既有 .recon/perf-evidence.md 的发现一致，可读该文件对照）。请给出本次新增读取的**触达行数量级**结论，不要用计划形状冒充行数有界。',
  '6. 与需求 §9 的对齐：报告里必须明确"初始恢复成本降低"与"持续浏览后累计缓存"两类成本各自的证据与缺口，不得写未经基准支持的固定毫秒承诺。',
  '',
  '报告文件：' + WORKTREE + '/.recon/S9-boundedness.md（结论 + 命令 + 输出片段 + 未验证项 + 已知偏差）。',
  '',
  RULES,
].join('\n')

// ── 编排 ─────────────────────────────────────────────────────────────────────

phase('S9a 存储与契约')
log('S9a: forward bounded segment read (contract + SQLite + Remote + tests)')
const storage = await agent(PROMPT_S9A, { label: 's9a-storage-contract' })

phase('S9b ACP 接线')
log('S9b: unnegotiated session/load streams canonical history')
const acp = await agent(PROMPT_S9B, { label: 's9b-acp-streaming-replay' })

phase('独立验证')
log('verification: semantics/wire and boundedness in parallel')
const [v1, v2] = await parallel([
  () => agent(PROMPT_V1, { label: 'verify-section-5-3' }),
  () => agent(PROMPT_V2, { label: 'verify-boundedness' }),
])

return {
  s9a_storage_contract: storage,
  s9b_acp_streaming_replay: acp,
  verification_section_5_3: v1,
  verification_boundedness: v2,
}
