export const meta = {
  name: 's13-revision-and-size-governance',
  description: 'S13 修订轮：锁回归测试确定化、wire 测试拆分 + 拒绝分支持久化、WASM 运行时闭环、STD-SIZE-001 规模治理 → 独立复核',
}

const WT = '/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history'

const PRE = [
  '工作目录（git worktree，绝对路径）：' + WT,
  '你的 bash 默认 cwd 是主仓库 /Users/konghayao/code/ai/peri-v4p3，**每条 bash 命令都必须以如下前缀开头**：',
  'cd ' + WT + ' && ',
  'Rust 构建与测试一律用 ./scripts/cargo-rmcp-patched.sh（禁止裸 cargo；除 -p <crate> 外不要加 --workspace）。',
  '分支 feat/transcript-shrink-history-pagination 的 HEAD 是 3ccf75864，工作树里有上一轮（S12）未提交的改动与报告（.recon/S12-*.md 是权威记录，先读再动手）。',
  '禁止：git add、git commit、git checkout、git stash、git rebase；禁止任何数据库 DDL；禁止改动本任务指定范围之外的文件。',
  '报告必须写到指定路径（Markdown），含：结论、证据（命令 + 关键原始输出片段）、未验证项与局限、最后单独一行 VERDICT。',
].join('\n')

const SPLIT_RULES = [
  '规模治理硬规则（STD-SIZE-001，见 docs/standards/index.md）：',
  '- 拆分后**每个**文件（含新增模块文件）≤1000 行（含空行与注释）；用 wc -l 逐个证明。',
  '- 拆分必须**按职责与模块边界**（例如“分页契约”“rewind 与过期游标”“拒绝路径”），禁止按行数机械切断、禁止压缩排版规避。',
  '- **语义零改动**：只搬移与重新挂载；生产代码若必须新建子模块，必须用 pub use 重导出保持既有公开路径可用；不改行为、不改断言。',
  '- 挂载必须真实成立：拆完要跑过滤命令证明用例被真实执行（不是 0 filtered / 0 passed 的空跑），并给出 test result 数字。',
  '- 拆分后必须跑该 crate 的全量 lib 测试：./scripts/cargo-rmcp-patched.sh test --locked -p <crate> --lib，全绿且数量与拆分前一致（基线：peri-acp 890、peri-resources 499、peri-agent 见报告、peri-tui 1859、peri-acp-types 见报告）。',
].join('\n')

phase('修订')

const [lockRework, wireSplit, wasmClosure] = await parallel([
  () => agent(
    [
      PRE,
      '',
      '任务 A1：按 .recon/S12-verification.md 的 §4.1–§4.3 与 §7.2 第 1 条，重做「冷开锁」回归测试的确定性（可改 peri-resources 的测试与必要 helper，范围限 peri-resources/src/sessions/sqlite_store/connection_open_test.rs 及其 helper）。',
      '',
      '复核已实测三点：① 把预算常量 LOCK_RELEASE_PROBE_BUDGET 退回 Duration::ZERO，3 次里仍有 1 次绿（child 可能在父探测前释放，顺序不确定）；',
      '② 把那四处真实调用点的预算退回原样，新用例 3/3 绿（新用例只测共享常量，守不住真实调用点）；③ 外部 holder 持锁 350ms 时用例仍红（250ms 只是容忍窗口）。',
      '',
      '要求：',
      '1) 用显式握手建立**确定顺序**：必须能判定「父的第一次探测发生时 child 仍持锁」（例如 child 持锁后写 ready 文件，父读到 ready 才发起探测，并让 child 在收到父的放行信号前不释放）。不要用 sleep 猜时序。',
      '2) 回归用例必须护住真实调用点：把那四处调用（或它们共用的 helper）临时退回原样时用例必须红。**实际做这个证伪**并记录原始输出（临时改 → 跑 → 红 → 字节级还原，给 sha256 或完整 diff）。',
      '3) 如实记录边界：该预算能被多长的合法持锁窗口击穿、它修的是根因还是症状；禁止「必红」「彻底消除」这类表述。',
      '4) connection_open_test.rs 拆分后必须 ≤1000 行（当前 629 行，注意别越界；越界就按职责拆文件）。',
      '5) 验证：./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- <相关 filter> 连续 ≥5 轮（记录 exit 与 test result），并跑一轮 -p peri-resources --lib 全量（基线 499 passed / 26 ignored）。',
      '',
      '报告路径：.recon/S13-lock-test.md。',
      'VERDICT：VERDICT: LOCK-TEST-DETERMINISTIC / VERDICT: LOCK-TEST-STILL-WEAK。',
    ].join('\n'),
    { label: 'A1-lock-test', model: 'sonnet' },
  ),
  () => agent(
    [
      PRE,
      '',
      '任务 A2：拆分 peri-acp 的 wire 测试文件，并把三条未被持久覆盖的拒绝分支转成回归用例（只动 peri-acp 的测试文件与必要挂载）。',
      '',
      '背景：peri-acp/src/host/history_page_wire_test.rs 现在 1935 行，违反 STD-SIZE-001（单文件 ≤1000 行）。',
      '挂载点：peri-acp/src/host/requests_test.rs:566 的 #[path = "history_page_wire_test.rs"] mod history_page_wire_tests;。',
      '上一轮独立复核（.recon/S12-verification.md §4.7）用临时探针验证过三条当前正确但无回归保护的拒绝分支：',
      'R6 合法 cursor 但缺 sessionId → -32602、零回放通知、原会话事实不变；',
      'R9 从热态集合移除会话后用原 cursor → -32602、零通知、不得创建会话；',
      'R10 会话处于 closing → -32010、零通知。这三类本来就没有 data.reason，不要机械套用「data 只含 reason」的既有 helper 规则。',
      '',
      SPLIT_RULES,
      '',
      '要求：',
      '1) 按职责拆分 history_page_wire_test.rs（建议：首屏/初始游标契约、页链与合并、拒绝路径、rewind 与过期游标），每文件 ≤1000 行；保持挂载成立，-- history_page 与各文件代表用例名都要能命中。',
      '2) 把 R6/R9/R10 写成持久用例（放进职责匹配的文件），断言错误码、零回放通知、存储事实不变/不创建会话；走真实 dispatch/ServerLoop 路径。',
      '3) 证伪：至少对 R6/R9/R10 各做一次注入（例如让拒绝路径先执行一次 initial load、或放宽某处校验）证明断言能红，再字节级还原（记录 sha256 或 diff）。',
      '4) 跑 -p peri-acp --lib 全量全绿（基线 890 passed），并用 wc -l 与 bash scripts/check-file-size.sh 证明你改动的文件全部 ≤1000 行。',
      '5) 不要改生产实现；发现生产缺陷只在报告里记录。',
      '',
      '报告路径：.recon/S13-wire-split.md。',
      'VERDICT：VERDICT: WIRE-SPLIT-DONE / VERDICT: WIRE-SPLIT-BLOCKED。',
    ].join('\n'),
    { label: 'A2-wire-split', model: 'sonnet' },
  ),
  () => agent(
    [
      PRE,
      '',
      '任务 A3：闭环验证 langfuse-client 的 WASM 修复（要运行时证据，不只是编译通过）。',
      '',
      '背景：上一轮 B3（报告 .recon/S12-baseline-fix.md）把 langfuse-client/src/batcher/recovery.rs 的 encode_batch 改为 wasm32/emscripten 就地同步执行、native 保留 tokio spawn_blocking。',
      '独立复核（.recon/S12-verification.md §4.8）只做到 emscripten 交叉编译检查；把修复退回后 native lib 292 条仍全绿（native 侧本就等价）。',
      '本任务豁免「只用 cargo」的限制：可以运行仓库既有的 SDK/WASM 构建与 bun 测试入口；允许执行 mise trust（此前 worktree 的 mise.toml 未受信任导致 sqld 起不来）并按脚本启动本地 sqld。',
      '',
      '要求：',
      '1) 先看 npm-packages/@peri-sdk 与 scripts/ 下的既有构建/测试入口，按仓库既有方式重建 WASM 产物并运行目标集成测试；不要自创入口、不要改生产代码（测试夹具可改，范围限 npm-packages/@peri-sdk）。',
      '2) 运行时**红→绿**证据：把修复临时退回原实现 → 重建 → 跑目标测试（应失败，贴真实输出与退出码）；字节级还原修复（给 sha256/diff）→ 重建 → 再跑（应通过，贴真实输出）。',
      '3) 若 SDK 夹具本身有阻塞（例如 gzip 请求夹具与默认压缩不一致），先判定是夹具问题还是产品问题；能改夹具就改，不能就如实记录并给最小修复方案。',
      '4) 不要长时间驻留服务；测试结束清理你自己启动的进程与临时目录（日志内容保留在报告里）。',
      '',
      '报告路径：.recon/S13-wasm-closure.md，写清实际命令、耗时、真实输出片段、哪些没有验证。',
      'VERDICT：VERDICT: WASM-VERIFIED / VERDICT: WASM-NOT-VERIFIED（后者必须说明卡点，并给出「保留修复」还是「建议回退」的判断与理由）。',
    ].join('\n'),
    { label: 'A3-wasm-closure', model: 'sonnet' },
  ),
])

phase('规模治理')

const [sizeAcp, sizeAgent, sizeTuiRes] = await parallel([
  () => agent(
    [
      PRE,
      '',
      '任务 B1：拆分 peri-acp 两个超限测试文件（只动测试文件与必要挂载）。',
      '',
      '目标文件（含与你的 A2 无关，若 A2 也在动 peri-acp 测试请注意不要互相覆盖：你只处理下面两个文件，history_page_wire_test.rs 归 A2）：',
      '- peri-acp/src/host/requests_rewind_cases_test.rs：1722 行（origin/main 基线 311 行，本交付新增 1416 行）。',
      '- peri-acp/src/host/requests_unnegotiated_replay_test.rs：1223 行（本交付新建）。',
      '',
      SPLIT_RULES,
      '',
      '要求：找清这两个文件的挂载点（grep 模块名），按职责拆成若干模块文件（例如 rewind 请求分支 / 过期游标 / 未协商回放契约 / 通知形状），保持既有 canonical 过滤命令能命中；',
      '拆完跑 -p peri-acp --lib 全量全绿（基线 890 passed）与 bash scripts/check-file-size.sh 对照（本轮改动文件逐个 wc -l ≤1000）。',
      '',
      '报告路径：.recon/S13-size-acp-tests.md。',
      'VERDICT：VERDICT: SIZE-SPLIT-DONE / VERDICT: SIZE-SPLIT-BLOCKED（说明卡点）。',
    ].join('\n'),
    { label: 'B1-size-acp-tests', model: 'sonnet' },
  ),
  () => agent(
    [
      PRE,
      '',
      '任务 B2：拆分 peri-acp-types 的生产模块 peri-acp-types/src/session_resources.rs（1034 行，origin/main 基线 807 行，本交付新增 230 行）。',
      '',
      SPLIT_RULES,
      '',
      '要求：按职责把本交付新增的历史分页 / rewind 相关类型与既有条目分开（例如把分页游标与页边界相关类型移入子模块），用 pub use 保持既有公开路径（crate::session_resources::X）不变；',
      '不要改任何行为、签名或序列化形状（这些是已冻结契约）。拆完跑 -p peri-acp-types --lib 全量全绿 + -p peri-acp --lib 全量全绿（下游消费者必须仍能编译）。',
      '报告里列出：拆出的文件、公开路径保持方式、测试数字。',
      '',
      '报告路径：.recon/S13-size-acp-types.md。',
      'VERDICT：VERDICT: SIZE-SPLIT-DONE / VERDICT: SIZE-SPLIT-BLOCKED。',
    ].join('\n'),
    { label: 'B2-size-acp-types', model: 'sonnet' },
  ),
  () => agent(
    [
      PRE,
      '',
      '任务 B3：拆分 peri-agent 的 transcript 生产模块与其测试（两者都被本交付改到，且都超过 1000 行）。',
      '',
      '目标文件：peri-agent/src/session/transcript.rs（1064 行，基线 990 行，本交付 +79/-5）、peri-agent/src/session/transcript_test.rs（1101 行，基线 961 行，本交付 +140）。',
      '',
      SPLIT_RULES,
      '',
      '要求：先判断 transcript.rs 的职责边界（例如持久化/重排/查询接口），用子模块 + pub use 保持既有路径；测试文件按职责拆（例如按接口分组）。',
      '拆完跑 -p peri-agent --lib 全量全绿（记录拆分前后的 test result 数字，必须一致）。',
      '',
      '报告路径：.recon/S13-size-agent.md。',
      'VERDICT：VERDICT: SIZE-SPLIT-DONE / VERDICT: SIZE-SPLIT-BLOCKED。',
    ].join('\n'),
    { label: 'B3-size-agent', model: 'sonnet' },
  ),
  () => agent(
    [
      PRE,
      '',
      '任务 B4：拆分 peri-tui 与 peri-resources 的超限文件（4 个）。',
      '',
      '目标文件（全部被本交付改到且超过 1000 行）：',
      '- peri-tui/src/kit/acp_events/system.rs（1013 行，基线 1007）：本交付 +13/-7。',
      '- peri-tui/src/kit/acp_events_test/session_events_test.rs（1021 行，基线 984）：本交付 +53/-16。',
      '- peri-resources/src/sessions/sqlite_store/session_history_perf_test.rs（1361 行，本交付新建）。',
      '- peri-resources/src/sessions/remote/full_remote_test.rs（1029 行，基线 588）：本交付 +441。',
      '',
      SPLIT_RULES,
      '',
      '要求：system.rs 是生产文件，按职责拆（例如事件分派 vs 具体事件形状）并用 pub use 保持既有路径；其余为测试文件，按职责分组（例如 perf 基准按场景、远端测试按契约面）。',
      '拆完分别跑 -p peri-tui --lib（基线 1859 passed / 6 ignored）与 -p peri-resources --lib（基线 499 passed / 26 ignored）全量全绿。',
      '',
      '报告路径：.recon/S13-size-tui-resources.md。',
      'VERDICT：VERDICT: SIZE-SPLIT-DONE / VERDICT: SIZE-SPLIT-BLOCKED。',
    ].join('\n'),
    { label: 'B4-size-tui-res', model: 'sonnet' },
  ),
])

// B2 单独串行：它要跑 `-p peri-acp --lib` 验证下游仍可编译，必须等 B1 停止改动 peri-acp 测试文件。
const sizeTypes = await agent(
  [
    PRE,
    '',
    '任务 B2：拆分 peri-acp-types 的生产模块 peri-acp-types/src/session_resources.rs（1034 行，origin/main 基线 807 行，本交付新增 230 行）。',
    '',
    SPLIT_RULES,
    '',
    '要求：按职责把本交付新增的历史分页 / rewind 相关类型与既有条目分开（例如把分页游标与页边界相关类型移入子模块），用 pub use 保持既有公开路径（crate::session_resources::X）不变；',
    '不要改任何行为、签名或序列化形状（这些是已冻结契约）。拆完跑 -p peri-acp-types --lib 全量全绿 + -p peri-acp --lib 全量全绿（下游消费者必须仍能编译）。',
    '报告里列出：拆出的文件、公开路径保持方式、测试数字。',
    '',
    '报告路径：.recon/S13-size-acp-types.md。',
    'VERDICT：VERDICT: SIZE-SPLIT-DONE / VERDICT: SIZE-SPLIT-BLOCKED。',
  ].join('\n'),
  { label: 'B2-size-acp-types', model: 'sonnet' },
)

phase('复核')

const verdictSize = await agent(
  [
    PRE,
    '',
    '任务 C：独立复核「规模治理」轮（B1~B4）。不采信实施者自述。',
    '',
    '要求：',
    '1) 用 wc -l 逐个核实本交付改动过的**所有**文件是否 ≤1000 行：先算 `git diff --name-only origin/main...HEAD` ∪ 当前未跟踪新增文件，再逐个 wc -l，列出仍超限者（这是本轮的核心判据）。',
    '2) 核实每次拆分是**按职责**而非机械切割：抽查 3 处，确认新模块的文件名/模块文档与被搬移内容职责一致，且没有为了凑行数而压缩排版（例如把多行结构压成一行）。',
    '3) 核实**语义零改动**：对每个被拆的生产模块，确认公开路径仍可用（下游 crate 能编译）且没有行为改动；抽查 diff 中是否有断言/逻辑被顺带修改（有则列为越界）。',
    '4) 核实挂载真实成立：对每个拆分后的测试文件，用其代表用例名跑过滤命令，确认不是空跑（0 filtered / 0 passed 即空跑），给出 test result 数字。',
    '5) 独立跑受影响 crate 的 lib 全量（至少 -p peri-acp --lib、-p peri-agent --lib、-p peri-acp-types --lib、-p peri-tui --lib、-p peri-resources --lib），给出真实 pass/fail 数并与基线核对（peri-acp 890、peri-resources 499、peri-tui 1859）。',
    '6) 若发现拆分引入的行为风险或未还原的临时改动，明确标出。',
    '',
    '报告路径：.recon/S13-verification-size.md。',
    'VERDICT：VERDICT: SIZE-GOVERNANCE-PASS / VERDICT: SIZE-GOVERNANCE-FAIL（逐条给判据级结论）。',
  ].join('\n'),
  { label: 'C-verify-size', model: 'opus' },
)

const verdictBehavior = await agent(
  [
    PRE,
    '',
    '任务 D：独立复核「行为修订」轮（A1 锁回归测试、A2 wire 拆分与拒绝分支、A3 WASM 闭环）。不采信实施者自述。',
    '',
    '要求：',
    '1) 逐条核对 .recon/S12-verification.md §7.2 的修订清单落地情况（锁测试确定性、拒绝分支持久化、WASM 运行时闭环、A14 口径、A05 处理、范围记录），标出未落地项。',
    '2) 对锁回归测试做证伪：把真实调用点退回原样确认用例变红，再字节级还原（sha256 证据）；若仍绿，明确判 FAIL 并给反例。',
    '3) 对 A2 新增的 R6/R9/R10 用例做证伪：注入等价异常（例如先执行一次 initial load）确认断言变红，再字节级还原。',
    '4) A3：核实运行时红→绿证据是否真实执行（命令、退出码、原始输出），指出任何把静态推断写成实测的地方；若判定 NOT-VERIFIED，给出「保留修复 / 建议回退」的独立判断。',
    '5) 独立跑 -p peri-acp --lib 与 -p peri-resources --lib 全量，给出真实数字；范围审计：是否有越界改动、新 DDL、未跟踪残留、临时改动是否全部还原。',
    '',
    '报告路径：.recon/S13-verification-behavior.md。',
    'VERDICT：VERDICT: BEHAVIOR-PASS / VERDICT: BEHAVIOR-NEEDS-REVISION（逐条给判据级结论）。',
  ].join('\n'),
  { label: 'D-verify-behavior', model: 'opus' },
)

return {
  lock: String(lockRework).slice(-1200),
  wire: String(wireSplit).slice(-1200),
  wasm: String(wasmClosure).slice(-1200),
  size: {
    acp: String(sizeAcp).slice(-800),
    types: String(sizeTypes).slice(-800),
    agent: String(sizeAgent).slice(-800),
    tuiRes: String(sizeTuiRes).slice(-800),
  },
  verification: {
    size: String(verdictSize).slice(-1500),
    behavior: String(verdictBehavior).slice(-1500),
  },
}
