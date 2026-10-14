export const meta = {
  name: 's12-remaining-items',
  description: 'S12 收尾：冷开锁抖动、A05 无 DDL 可行性、验收缺口、冒烟与基线失败 → 修复 → 独立复核',
}

const WT = '/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history'

const PRE = [
  '工作目录（git worktree，绝对路径）：' + WT,
  '你的 bash 默认 cwd 是主仓库 /Users/konghayao/code/ai/peri-v4p3，**每条 bash 命令都必须以如下前缀开头**：',
  'cd ' + WT + ' && ',
  '构建与测试一律用 ./scripts/cargo-rmcp-patched.sh（禁止裸 cargo；除 -p <crate> 外不要加 --workspace）。',
  '分支 feat/transcript-shrink-history-pagination 已有三个 commit（95c1e07d7 核心 / 9b96f456a 前端 / 3ccf75864 文档），工作树干净。',
  '禁止：git add、git commit、git checkout、git stash、git rebase；禁止任何数据库 DDL（新增列/表/索引）；禁止改动本任务指定范围之外的文件。',
  '报告必须写到指定路径（Markdown），含：结论、证据（命令 + 关键原始输出片段，不要只给结论）、未验证项与局限、以及最后单独一行 VERDICT。',
].join('\n')

phase('侦察')

const [lockRecon, a05Recon, gapRecon, smokeRecon] = await parallel([
  () => agent(
    [
      PRE,
      '',
      '任务 A1：定位并给出 peri-resources「冷开锁 5s 抖动」的最小修复方案（只做侦察，不改代码）。',
      '',
      '现象：./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib 偶发失败，报错 session database initialization is busy；',
      '此前独立复核者连跑 6 次命中 2 次。报错来自 peri-resources 的 schema open 锁（搜索 SCHEMA_OPEN_LOCK_TIMEOUT，约 5s）。',
      '',
      '要求：',
      '1) 读代码定位该锁的作用域与超时路径（connection.rs 等），判断「谁持锁、为何超过 5s、失败的是 acquire 超时还是别的原因」。',
      '2) 尽量稳定复现：优先用便宜的入口（例如只跑 connection_open 相关测试模块、用 --test-threads 复现并发、或连续多次运行），',
      '   给出你实际用的命令与命中率（跑了 N 次，红 M 次）。若无法复现，说明你尝试过什么、并给出静态证据支撑的根因假设（区分观察与假设）。',
      '3) 给出最小修复方案（可以贴 diff 到报告，但不要落到代码里），并比较候选方案的代价与风险，例如：超时上调、失败重试、锁按 db 路径分桶、',
      '   测试内串行化、把锁与 schema 版本检查解耦。指出每个方案是否只是掩盖症状。',
      '4) 明确该抖动是否与本分支改动有因果关系（peri-resources 是否依赖本分支新增的接口；用 git log/git diff 证据说话）。',
      '',
      '报告路径：.recon/S12-cold-open-lock.md（相对工作目录）。',
      'VERDICT 取值：VERDICT: MINIMAL-FIX-CLEAR（方案明确且低风险）/ VERDICT: NEEDS-DESIGN（需更大改造）/ VERDICT: NOT-REPRODUCIBLE（无法复现，只有假设）。',
    ].join('\n'),
    { label: 'A1-cold-open-lock', model: 'sonnet' },
  ),
  () => agent(
    [
      PRE,
      '',
      '任务 A2：判定「恢复路径 O(线程行数)」（已知偏差 D10 / 验收 A05 FAIL）能否在不新增索引或列（即无 DDL）的前提下收敛，并复跑性能基线（只做侦察与度量，不改代码）。',
      '',
      '已知事实（可复核，不要照抄当作结论）：',
      '- 恢复热路径的三条 SQL（ACTIVE_PAYLOADS_SQL / ACTIVE_FLAGS_SQL / ACTIVE_ENTRY_POSITIONS_SQL，见 peri-resources/src/sessions/sqlite_store/）',
      '  在 perf 记录里的计划是 SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)，逐行回表判定 excluded/content。',
      '- D10 实测（10k/100k/1M 归档行）恢复 cold p50 = 11.109 / 98.182 / 1081.927 ms（debug），解码正文恒为 196971 B。',
      '- 结论是：内存与解码字节有界，但触达行数随线程行数线性增长，修复需 (thread_id, excluded) 索引 = DDL 未授权。',
      '',
      '要求：',
      '1) 先给决定性证据：用 EXPLAIN QUERY PLAN（或等价手段）实测那三条 SQL 当前的计划与触达行数探针，写清你用的命令。',
      '2) 逐条评估无 DDL 的收窄手段是否真的成立：',
      '   a. 能否用存储序（rowid / 主键）范围把读取收窄到「最后一次 Full 之后」——检查 full_watermark 之类信息的持久化位置与可用性；',
      '   b. 能否让 SQL 只在索引内完成过滤（覆盖索引）而不回表；',
      '   c. 能否把「有效后缀起点」在写入时记在已有列里（不新增列）。',
      '   对每个手段给出：可行性、预期触达行数上界、语义等价性风险（尤其 rewind / 精确删除 / 复用 rowid 之后）。',
      '3) 复跑性能用例：peri-resources/src/sessions/sqlite_store/session_history_perf_test.rs 里被 #[ignore] 的用例。',
      '   给出你实际跑的命令与真实输出（若 1M 规模受限于时间或内存，跑更小规模并明确说明跳过了哪一条、为什么）。',
      '   注意：机器上可能有其他 agent 在编译，毫秒数只作参考，必须注明负载情况，不要用绝对毫秒下结论。',
      '4) 最后给出裁决：无 DDL 方案存在 / 不存在（必须 DDL）。若必须 DDL，给出建议的索引定义与预期收益，供用户批准时参考。',
      '',
      '报告路径：.recon/S12-a05-recovery-scan.md（相对工作目录）。',
      'VERDICT 取值：VERDICT: NO-DDL-FIX-AVAILABLE（并给出最小实现草案）/ VERDICT: DDL-REQUIRED（并给出索引定义），二者都必须附证据。',
    ].join('\n'),
    { label: 'A2-a05-scan', model: 'sonnet' },
  ),
  () => agent(
    [
      PRE,
      '',
      '任务 A3：核对三处验收缺口（只做侦察与判定，不改代码、不补测试）。',
      '',
      '需求文档（328 行，需求方原文）：/Users/konghayao/Desktop/peri-transcript-history-requirements.md',
      '验收清单是 §9 的 A01~A25。本分支的验收报告在 .recon/A-acceptance-report.md（A14 目前记为「未验证」）。',
      '',
      '1) A14：读需求 §9 找到 A14 的原文判据，逐字抄录到报告；再在代码与测试中核对是否达成。',
      '   若测试已覆盖：给出测试文件 + 用例名 + 你实际跑过的命令与结果。',
      '   若未覆盖或与判据有差距：写清缺口在哪一层（实现缺失 / 只有部分断言 / 只有间接覆盖），以及具体可执行的验证方式。不要伪造结论。',
      '2) 门控来源：定位 peri.sessionWorkspaceV1 的判定来源（哪个文件、按连接还是按会话注册表、与 peri.historyPagination / peri.replay 的关系），',
      '   给出文件:行号，并判断它是否存在与本分支同类「会话级注册表 vs 当前连接」的偏差（参考 .recon/ 里 D11/D12 的记录）。',
      '   若发现同类偏差，说明可达场景（怎样构造出一个连接 A 写过之后连接 B 拿到不该拿的字段）。',
      '3) 「拒绝不得触发 initial load」：在 peri-acp 里找到分页参数被拒绝的路径（坏游标 / 跨会话 / 已失效 / cursor 与 setup 参数同现等），',
      '   核对测试是否断言了「拒绝时不得附带 initial load 的全量通知与响应字段」。若缺，列出需要补的断言清单（精确到测试文件、用例名建议、断言内容）。',
      '',
      '报告路径：.recon/S12-acceptance-gaps.md（相对工作目录）。',
      'VERDICT 取值：VERDICT: A14-COVERED 或 VERDICT: A14-GAP（并给出最小补齐方案），另在报告里分别标出 2) 与 3) 的判定。',
    ].join('\n'),
    { label: 'A3-acceptance-gaps', model: 'sonnet' },
  ),
  () => agent(
    [
      PRE,
      '',
      '任务 A4：基线失败定位 + 可离线冒烟清单（只做侦察；除非明确要求，不要改代码）。',
      '',
      '1) 基线失败（与本分支无关，属交付前遗留）：',
      '   npm-packages/@peri-sdk 跑 bun test 时，历史上恒有 1 条失败，位置 langfuse-client/src/batcher/recovery.rs:256，',
      '   现象是 WASM telemetry 相关 panic（mise 未信任 worktree 的 mise.toml 时会再多一条 sqld 起不来导致的环境失败）。',
      '   要求：给出实际复现命令与真实输出；读代码定位根因（是产品缺陷、测试夹具缺陷，还是 WASM 运行时限制）；',
      '   给出最小修复方案与风险评估（说明它属于 langfuse-client 这个独立包，是否值得在本交付内修）。',
      '   复现前先确认 sqld 是否可用（若不可用，先说明你如何让环境可用或为何放弃，不要伪造测试结果）。',
      '2) 可离线冒烟清单：读 e2e/ 下的指引（e2e/CLAUDE.md 等，以仓库实际内容为准），列出当前可运行的端到端或 stdio 冒烟方式：',
      '   每条给出命令、前置依赖（venv / sqld / 网络 / 环境变量）、预计耗时。',
      '   至少实际尝试一条最小的 stdio / ACP 冒烟（若前置齐备），把真实输出与退出码写进报告；前置不齐备的逐条说明原因。',
      '   注意：不要启动长期驻留服务；不要改仓库文件（除你的报告）。',
      '',
      '报告路径：.recon/S12-smoke-and-baseline.md（相对工作目录）。',
      'VERDICT 取值：VERDICT: BASELINE-FIX-SMALL（一行级/低风险，可修）/ VERDICT: BASELINE-FIX-LARGE（需单独立项）/ VERDICT: BASELINE-ENV-ONLY（只是环境问题）。',
    ].join('\n'),
    { label: 'A4-smoke-baseline', model: 'sonnet' },
  ),
])

log('侦察完成：lock=' + String(lockRecon).slice(-400) + ' | a05=' + String(a05Recon).slice(-400))

const fixLock = /VERDICT:\s*MINIMAL-FIX-CLEAR/.test(String(lockRecon))
const fixA05 = /VERDICT:\s*NO-DDL-FIX-AVAILABLE/.test(String(a05Recon))
const fixBaseline = /VERDICT:\s*BASELINE-FIX-SMALL/.test(String(smokeRecon))
log('门控：lock=' + fixLock + ' a05=' + fixA05 + ' baseline=' + fixBaseline)

phase('实施')

const lockFix = fixLock
  ? await agent(
      [
        PRE,
        '',
        '任务 B1：按 .recon/S12-cold-open-lock.md 的结论实施「冷开锁抖动」的最小修复（允许改代码 + 补测试）。',
        '',
        '硬要求：',
        '1) 只做该报告认定的最小修复；不要顺手重构。修复必须解释清楚「修的是根因还是症状」，若只是把超时调大，说明为什么可接受。',
        '2) 必须补一条能失败的测试（或复现脚本）证明修复有效：把修复退回去该测试必须红。把证伪过程（临时改动 → 红 → 还原）写进报告。',
        '3) 跑受影响范围：至少 ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib 全绿，并说明跑了多少次、有没有再命中抖动。',
        '4) 不改 DDL、不动 public 契约。',
        '',
        '报告路径：.recon/S12-cold-open-lock-fix.md（相对工作目录），写清改了什么文件、为什么、验证命令与结果、残留风险。',
      ].join('\n'),
      { label: 'B1-lock-fix', model: 'sonnet' },
    )
  : 'SKIPPED: 侦察判定不做最小修复'

const gapFix = await agent(
  [
    PRE,
    '',
    '任务 B2：按 .recon/S12-acceptance-gaps.md 的结论补齐验收缺口（允许改代码 + 补测试）。',
    '',
    '范围（严格按报告结论，做多少算多少，不要扩范围）：',
    '1) A14：若报告判定为缺口，按报告给出的最小补齐方案实现；若已覆盖，只补一条把判据钉死的断言（若确实没有必要，就明确写「无需改动」并说明理由）。',
    '2) 「拒绝不得触发 initial load」：按报告清单补断言（走真实的 dispatch 路径，不要只测工具函数）。',
    '3) 门控来源那条：若报告发现同类「会话级注册表 vs 当前连接」偏差，**不要直接改**，只写清可达场景与最小修复草案，留给后续裁决。',
    '',
    '硬要求：每条新断言都要能失败（把实现/断言退回去必须红），证伪过程写进报告；改动范围限定在 peri-acp / peri-acp-types 及其测试。',
    '跑法：./scripts/cargo-rmcp-patched.sh test --locked -p <crate> --lib -- <filter>（新用例必须全绿；至少再跑一次 -p peri-acp --lib 的对应范围）。',
    '',
    '报告路径：.recon/S12-gap-tests.md（相对工作目录）。',
  ].join('\n'),
  { label: 'B2-gap-tests', model: 'sonnet' },
)

const baselineFix = fixBaseline
  ? await agent(
      [
        PRE,
        '',
        '任务 B3：按 .recon/S12-smoke-and-baseline.md 的结论修基线失败（langfuse-client 的 WASM telemetry panic）。',
        '',
        '硬要求：',
        '1) 只做报告认定的最小修复；若实施中发现风险超出报告预期，停下来只写报告并说明（不要硬上）。',
        '2) 必须给出复现命令与修复后的真实输出（bun test 的目标用例全绿，或说明为何无法在本机验证）。',
        '3) 不改 langfuse-client 的公开导出契约；不引入新依赖。',
        '',
        '报告路径：.recon/S12-baseline-fix.md（相对工作目录）。',
      ].join('\n'),
      { label: 'B3-baseline-fix', model: 'sonnet' },
    )
  : 'SKIPPED: 侦察判定不在本交付内修'

const a05Fix = fixA05
  ? await agent(
      [
        PRE,
        '',
        '任务 B4：按 .recon/S12-a05-recovery-scan.md 的结论实施「无 DDL」的恢复路径收窄（允许改代码 + 补测试）。',
        '',
        '硬要求：',
        '1) 严格按报告的方案实施，不得新增列/表/索引（DDL 未授权）；若实施中发现必须 DDL 才能等价，立即停止并只写报告。',
        '2) 语义等价性优先：rewind、精确删除、rowid 复用、继承条目（继承前缀在本线程没有行）都必须有测试或显式论证。',
        '3) 用行数探针/EXPLAIN 证明触达行数真的降下来了，把证据（命令 + 输出片段）写进报告。',
        '4) 跑 ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib 全绿，并跑相关分页/rewind 用例。',
        '',
        '报告路径：.recon/S12-a05-fix.md（相对工作目录）。',
      ].join('\n'),
      { label: 'B4-a05-fix', model: 'sonnet' },
    )
  : 'SKIPPED: 侦察判定必须 DDL（保持已知偏差）'

phase('复核')

const verdict = await agent(
  [
    PRE,
    '',
    '任务 C：独立复核本轮（S12）的全部改动，判定能否交付。你是独立复核者，不要相信实施者的自述。',
    '',
    '输入：',
    '- 侦察报告：.recon/S12-cold-open-lock.md、.recon/S12-a05-recovery-scan.md、.recon/S12-acceptance-gaps.md、.recon/S12-smoke-and-baseline.md',
    '- 实施报告：.recon/S12-cold-open-lock-fix.md、.recon/S12-gap-tests.md、.recon/S12-baseline-fix.md、.recon/S12-a05-fix.md（按实际存在者为准）',
    '- 改动范围：git status --short 与 git diff（相对 HEAD 3ccf75864）。',
    '',
    '要求：',
    '1) 逐条列出本轮实际改动（文件 + 目的），指出任何越出任务范围、或与报告不符的改动。',
    '2) 对每条修复做证伪：把修复点临时退回去，确认对应的新测试确实变红，再还原（必须用 sha256 或 git diff 证据证明已字节级还原，最后 git status 必须回到复核前的状态）。',
    '3) 独立跑关键验证：受影响的 lib 测试范围（至少 -p peri-acp --lib 与本轮改动的其他 crate）与新增用例，给出真实 pass/fail 数。',
    '4) 找反例：对每条修复尝试构造至少一个「失败场景」或边界（例如锁修复在真并发下是否只是把窗口拉长；A05 收窄在 rewind 之后是否仍等价）。',
    '5) 判定：可以交付 / 需修订 / 不可交付，并给出判据级结论（PASS / PARTIAL / FAIL，逐条）。',
    '',
    '报告路径：.recon/S12-verification.md（相对工作目录）。',
    'VERDICT 取值：VERDICT: DELIVERABLE / VERDICT: NEEDS-REVISION / VERDICT: NOT-DELIVERABLE。',
    '注意：你的报告是权威记录，请写清你实际跑过的命令与观察到的输出；无法验证的项如实标注，不要用推断填充。',
  ].join('\n'),
  { label: 'C-verify', model: 'opus' },
)

return {
  recon: {
    lock: String(lockRecon).slice(-1200),
    a05: String(a05Recon).slice(-1200),
    gaps: String(gapRecon).slice(-1200),
    smoke: String(smokeRecon).slice(-1200),
  },
  gates: { fixLock, fixA05, fixBaseline },
  fixes: {
    lock: String(lockFix).slice(-1200),
    gaps: String(gapFix).slice(-1200),
    baseline: String(baselineFix).slice(-1200),
    a05: String(a05Fix).slice(-1200),
  },
  verification: String(verdict).slice(-2500),
}
