export const meta = {
  name: 's10-final-verification-and-acceptance-report',
  description: 'Final test matrix, adversarial audit and the A01-A25 acceptance report for the transcript shrink + history pagination requirement',
}

const WORKTREE = '/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history'
const REQUIREMENTS = '/Users/konghayao/Desktop/peri-transcript-history-requirements.md'

const RULES = [
  '=== 环境与命令纪律 ===',
  '1. 所有 bash 命令必须以 cd ' + WORKTREE + ' && 开头（默认 cwd 是主仓库，不是 worktree）。Read/Grep/Edit/Write 用 worktree 绝对路径。',
  '2. Rust：只用 ./scripts/cargo-rmcp-patched.sh；禁止 --workspace 级命令；**串行**跑 cargo（一次一个进程，跑完再起下一个），每个命令给足超时（编译可能数分钟）。',
  '3. SDK：cd ' + WORKTREE + '/npm-packages/@peri-sdk && bun run typecheck / bun run build / bun test（不要改依赖）。',
  '4. 禁止 git 写操作（不 commit/stash/checkout/reset/clean）；只读 git 允许。禁止 DDL、禁止新增依赖。',
  '5. 证据纪律：区分「实测」（命令 + 输出片段）与「静态判断」；没跑的必须写「未验证」；不得把计划或推测写成结果。',
  '6. 报告用中文，落盘到指定文件，最终回复给同一份摘要。',
].join('\n')

const CONTEXT = [
  'Worktree: ' + WORKTREE,
  '分支: feat/transcript-shrink-history-pagination（全部改动未提交，用 git status / git diff 看）。',
  '需求文档: ' + REQUIREMENTS + '（§9 是验收表 A01~A25；§5.3 是 ACP 标准边界；§10 是不在本任务内）。',
  '实现蓝图与决策记录: ' + WORKTREE + '/spec/issues/2026-10-13-transcript-shrink-history-pagination.md（§7 有 D1~D14 决策、切片状态与编排脚本表）。',
  '证据目录: ' + WORKTREE + '/.recon/（R4/S6/R4b 复核、perf-evidence、S9*/S7* 等）。',
].join('\n')

// ── 1. 终局测试矩阵 ───────────────────────────────────────────────────────────

const PROMPT_MATRIX = [
  '你是终局验证者。任务：在 worktree 里跑完整测试矩阵并如实记录结果，不做任何生产代码修改（只允许新增/调整测试文件，且必须在报告中列出）。',
  '',
  CONTEXT,
  '',
  '必须跑（串行，逐条给命令 + 结果摘要 + 失败用例名）：',
  '1. cd ' + WORKTREE + ' && ./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp-types --all-targets',
  '2. 同上 -p peri-acp、-p peri-agent、-p peri-resources、-p peri-tui（各 --all-targets）',
  '3. 各 crate 的 lib 测试：cargo test --locked -p <crate> --lib（peri-acp-types、peri-acp、peri-agent、peri-resources、peri-tui）',
  '4. cd ' + WORKTREE + '/npm-packages/@peri-sdk && bun run typecheck && bun run build && bun test',
  '',
  '已知基线（用于判断是否本切片引入，不要直接采信，要自己跑）：peri-acp-types 568 passed、peri-acp 863 passed、peri-agent 1089 passed、'
    + 'peri-resources 481~484（该 crate 有既有 flake：connection_open_test 的锁探针与 workspace git 探针在并发负载下会失败，单跑通过）。'
    + '若出现失败：先判断是否本切片引入（对照 git diff 与失败用例的语义），单跑一次确认是否 flake，把「失败用例 + 单跑结果 + 判断」写清；不要为了让套件变绿而改断言。',
  '注意：性能用例（history_perf_tests）在 #[ignore] 下，默认不会跑，也不要跑（夹具构建 36 分钟）。',
  '',
  '报告文件：' + WORKTREE + '/.recon/final-test-matrix.md',
  '结构：逐条命令与结果 / 失败与 flake 分析 / 与基线差异 / 未验证项。',
  '',
  RULES,
].join('\n')

// ── 2. 对抗审计 ───────────────────────────────────────────────────────────────

const PROMPT_ADVERSARIAL = [
  '你是独立对抗审计者。任务：不去复述既有复核结论，而是主动找反例，判定本需求「最容易出错、也最容易自证清白」的几条断言是否真的成立。',
  '默认只读；发现缺陷写最小复现与影响面，不要自行修（除非一行内笔误，修完必须跑测试并在报告标注）。',
  '',
  CONTEXT,
  '',
  '必查（每条都要给证据；能实测就写临时 #[cfg(test)]/#[test] 用例实测，报告标注保留与否）：',
  '1. 无 DDL / 无新依赖：核对 schema 与所有 Cargo.toml、package.json 无改动（给命令与输出）；核对没有任何迁移/双写/兜底代码。',
  '2. 失败关闭：分页游标不可用（形式错误/跨会话/锚点被删/结构版本过期）、分页请求带 setup 参数、未协商连接带 cursor —— 各自是否**明确拒绝**且不降级为初始 load（A25）。',
  '3. 标准客户端完整回放（§5.3 / A25）：未协商连接的初始 load 是否真的交付完整 canonical 历史（含 Full 归档原文），且不含任何 Peri 私有字段——**不只是分页四项**：`periReplay` / `periLoadGeneration` 也在内，必须覆盖「会话级 caps 注册表已被先前的 peri 连接写过」这个场景（D12 的成因）；协商连接是否仍带 entryId/position/periLoadGeneration；未 initialize 的进程内路径（TUI/MPSC）是否仍是全能力语义。',
  '4. 一致性：同一夹具下 SQLite 与 Remote 两后端的初始有效视图、分页顺序、游标失效语义是否一致（A13）——静态对照 + 能跑的契约测试。',
  '5. 收缩不变量：提交失败/取消/后续 turn 取消时不得留下「原文已退出但摘要不可恢复」的状态（A02）；Full 期间到达的用户输入/后台结果不被旧边界移除（A03）。',
  '6. 重复/幂等：相同 cursor+limit 重试等价、追加消息并再次 Full 后旧页仍等价且成员不按新 flags 变化（A08/A09）；稳定 delivery id 在提交确认丢失/Full 前后/冷恢复后重投只有一份（A18）。',
  '7. 前端：Compact 不再强制完整 reload、历史窗口与 cursor 进度保留、历史投影不推进实时状态（A12/A23/A24）——以 S7 的 report 与代码为准，指出未达成项。',
  '8. 逐条列出你认为**未验证或证据不足**的验收项（宁多勿少），不要用「应该没问题」收尾。',
  '',
  '报告文件：' + WORKTREE + '/.recon/final-adversarial-audit.md',
  '结构：断言 → 结论（成立/不成立/证据不足）→ 证据（命令与输出或 file:line）→ 反例/最小复现 → 影响面。',
  '',
  RULES,
].join('\n')

// ── 3. 验收报告 ───────────────────────────────────────────────────────────────

const PROMPT_REPORT = [
  '你是验收报告的作者。任务：产出面向评审者的验收报告，逐条回答需求 §9 的 A01~A25，并严格区分两类成本（需求 §9 原文：验收报告必须区分"初始恢复成本降低"和"持续浏览后累计缓存大小"；查询计划和实际扫描工作量共同作为有界读取证据，不能只看返回行数；不得新增未经基准支持的固定毫秒承诺）。',
  '',
  CONTEXT,
  '',
  '输入（全部要读）：需求 §9 验收表、蓝图 §7（D1~D14 决策与已知偏差）、'
    + '.recon/ 下的 R4-verification.md、S6-verification.md、R4b-verification.md、perf-evidence.md、'
    + 'S9*.md（含 S9-verification.md / S9-boundedness.md / S9c-*.md）、S7*.md（含 S7-tui-verification.md / S7-sdk-verification.md / S7c-* / S7d-* / S7e-*）、final-test-matrix.md、final-adversarial-audit.md。',
  '',
  '报告要求：',
  '1. 表格：A01~A25 逐条给 结论（PASS / PARTIAL / FAIL / 未验证）+ 证据指针（命令、文件、章节或测试名）+ 缺口一句话。**不得**把没证据的项写成 PASS；PARTIAL 要写清哪一半成立、哪一半不成立。',
  '2. 两类成本分开写：初始恢复成本（含 perf-evidence §4/§4.1 的三规模数字与机制说明）与持续浏览后累计缓存（现状与缺口，不得宣称分页已保证累计内存恒定）。',
  '3. 有界读取证据必须同时给查询计划与实际扫描工作量（行数探针/触达上界），并明确指出恢复路径仍是 O(线程行数)（D10 / perf-evidence §8），以及修复所需的索引属 DDL、未获授权。',
  '4. 已知偏差单列：D8（两次以上 Full 后 rewind 到排除段内部时的保守整段清零，A19 字面未达）、D10（恢复路径触达行数线性，需索引 = DDL 未授权）、'
    + 'S9-boundedness.md 的 R1（回放中途 rewind 删 `tail` 行 → SQLite 复用 rowid，新行进入本次回放、被删行静默消失；「行集合同源」不成立）、'
    + 'R3（分页反向读取的索引项触达随深度线性，表行仍有界）、D12/D14（回放标记门控与锚点坐标，均已修并复核）、'
    + 'A20 的锚点边界（锚点单元**自身**被折叠或拆组时不补偿：实测下方位移 0 行 / +4 行；身份歧义形状仅为静态判定，未构造反例）、'
    + 'SDK 残留 P3（`compact_started` 之前的真实 Compact 失败仍无展示，非本轮引入）、'
    + '前端覆盖缺口（`docs/verification/tui-integration-cases/` 已登记人工用例 P0-77，`e2e/` 仍无翻页自动化用例），以及任何「未验证」项。',
  '5. 交付面清单：改了哪些 crate/包、哪些对外行为变化（wire 形状、错误码、新能力名 peri.historyPagination）、哪些不变。',
  '6. 不做的事（§10）要显式声明未被引入（旧格式兼容/迁移/双写、未 Compact 超长历史专项、物理删除/TTL/GC、Checkpoint/event-sourcing、Yjs 架构替换）。',
  '7. 所有数字必须来自 .recon 下的实测记录，禁止自造；debug profile 与冷热口径要照抄 perf-evidence §1 的声明。',
  '',
  '报告文件：' + WORKTREE + '/.recon/A-acceptance-report.md',
  '',
  RULES,
].join('\n')

// ── 4. 报告复核 ───────────────────────────────────────────────────────────────

const PROMPT_REPORT_AUDIT = [
  '你是验收报告的独立复核者。任务：审计 ' + WORKTREE + '/.recon/A-acceptance-report.md，判定它是否**诚实且可核对**。',
  '不要改写报告本身（除非是明显笔误）；把审计结论写成独立文件。',
  '',
  '必做：',
  '1. 逐条抽查：报告里每个 PASS 是否能追到具体证据（命令、文件、测试名、章节）？把「无证据支撑」「证据与结论不符」「把静态判断写成实测」的项全部列出。',
  '2. 数字一致性：报告引用的数字与 .recon/perf-evidence.md、final-test-matrix.md、final-adversarial-audit.md 是否一致？逐条对账。',
  '3. 遗漏检查：是否有验收项在报告里被合并、淡化或跳过（尤其 A02/A03/A08/A09/A10/A13/A14/A16/A18/A22/A23/A24）？是否遗漏「未验证项」清单？是否遗漏 §10（不在本任务内）的显式声明？',
  '4. 口径检查：两类成本是否真的分开写？有界读取证据是否同时给了计划与扫描量？有没有出现未经基准支持的固定毫秒承诺（例如「X ms 内完成」）？',
  '5. 结论：报告能否作为交付验收依据（可以 / 需修订，列出必须修订项）。',
  '',
  '报告文件：' + WORKTREE + '/.recon/A-acceptance-report-audit.md',
  '',
  RULES,
].join('\n')

// ── 编排 ─────────────────────────────────────────────────────────────────────

phase('终局测试矩阵')
log('final: full test matrix across 5 crates + SDK')
const matrix = await agent(PROMPT_MATRIX, { label: 'final-test-matrix' })

phase('对抗审计')
log('adversarial: invariants, failure-closed paths, SQLite/Remote parity, uncovered items')
const audit = await agent(PROMPT_ADVERSARIAL, { label: 'final-adversarial-audit' })

phase('验收报告')
log('acceptance report: A01-A25 mapping, two cost classes, known deviations')
const report = await agent(PROMPT_REPORT, { label: 'acceptance-report' })

phase('报告复核')
log('report audit: honesty and traceability of every PASS')
const reportAudit = await agent(PROMPT_REPORT_AUDIT, { label: 'acceptance-report-audit' })

return {
  final_test_matrix: matrix,
  adversarial_audit: audit,
  acceptance_report: report,
  report_audit: reportAudit,
}
