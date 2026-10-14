export const meta = {
  name: 's7d-anchor-and-integration-case',
  description: 'Close the last S7 frontend gaps: exact anchor compensation for prepends below the viewport (A20) and register the TUI integration case for scroll-loaded earlier history',
}

const WORKTREE = '/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history'
const CD = 'cd ' + WORKTREE + ' && '
const CARGO = './scripts/cargo-rmcp-patched.sh'

const BRIEF = [
  'worktree（唯一工作目录，绝对路径）：' + WORKTREE,
  '所有 bash 命令必须以 `cd ' + WORKTREE + ' && ` 开头（bash 默认 cwd 是主仓库，不是 worktree）。Cargo 一律用 `./scripts/cargo-rmcp-patched.sh`（禁止裸 cargo、禁止 --workspace）。',
  '禁止：git 写操作（工作区保持未提交）、DDL、新增依赖、改服务端语义（peri-acp / peri-acp-types / peri-resources）、改 SDK（npm-packages）。',
  '**迭代预算纪律**：上一轮 TUI 修复 agent 在 200 轮迭代上限被中断（没落报告、末次编辑还使 lib test 不可编译）。你必须：先读全再动手、改动收敛、用定向测试而不是反复全量、尽早把关键结论写进报告文件（不要留到最后一次性写）。',
].join('\n')

const CONTEXT_S7C = [
  '## 现状（来自 ' + WORKTREE + '/.recon/S7c-tui-verification.md，权威记录）',
  '- D1 滚动触发、D2 rewind 失效、D3 区间登记、D4 工具终点限定、D5 页 RPC 超时、D8 断言强度：**已 PASS**，每条都有「退回修复点即 FAIL」的证伪对照。不要动它们。',
  '- D6 锚点：`history_paging::prepend_scroll_delta(follow_bottom, epoch_changed, prev_total_visual, total_visual)`（`peri-tui/src/kit/scroll/auto_follow.rs:247-269`）只在浏览态且 `HISTORY_PREPEND_EPOCH` 变化时按**总行数增量**补偿 `offset`。',
  '  - 场景 A（插入点在视口上方，向上翻页常见形态）：同一可见单元屏幕行不变 → A20 成立。',
  '  - 场景 B（插入点在视口下方，中间缺口填补）：补偿仍按总增量 → 视口被整体推动 = 插入高度 → A20 不成立（复核者实测 −4 行）。',
  '- D7：`docs/code-index/peri-tui.md` 已同步；`docs/verification/tui-integration-cases/` **零改动**，而该目录 README 的约定（:9「用例编号是全目录 P0 执行顺序」、:51「新增场景在可用时链接 active issue」）要求登记新场景。当前最大编号 `P0-76`，新增应为 `P0-77`。',
  '- e2e/ 无翻页用例（已 grep 确认）。',
].join('\n')

const PROMPT_FIX = [
  '你是实现者。本轮只做两件事，都要做到可被独立证伪。',
  '',
  BRIEF,
  '',
  CONTEXT_S7C,
  '',
  '## 任务 1（主）：把 D6 从「按总增量近似」改成「只补偿视口上方的插入高度」——目标是 A20 在场景 A 与场景 B 都成立',
  '要求：',
  '1. 先读代码把事实读全：`auto_follow.rs` 的 `prepend_scroll_delta`/`run_auto_follow`/滚动事实、`history_paging.rs` 的区间索引（`HISTORY_SPANS` / `insert_slot_for` / 页合并）与 `HISTORY_PREPEND_EPOCH` 的写入点、页并入时「插入了哪些视觉行」在何处可知。',
  '2. 让补偿量依赖**插入块相对视口的位置**：插入完全在视口上方 → 补偿 = 插入高度（保锚点）；插入完全在视口下方 → 补偿 = 0（视口不动）；跨视口或跨界 → 取一个明确定义且写进注释的规则（并给出理由）。视口事实（offset / vis_height / follow_bottom）与插入行的视觉区间必须来自**生产路径已有的同一份事实**，不要另造第二份索引。',
  '3. 保留原有的「跟随底部」语义（`follow_bottom` 为真时不得因为补偿把视图从底部拉开）。',
  '4. 测试（放在 `peri-tui` 既有测试文件风格里，用真实的 `SlotIndex`/`slot_visual_start` 与生产函数，不要只测纯算术）：',
  '   - 场景 A：插入在视口上方 → 同一可见单元屏幕行**不变**；',
  '   - 场景 B：插入在视口下方（中间缺口填补）→ 同一可见单元屏幕行**不变**（这是本轮的核心增量）；',
  '   - 跟随底部时行为不变；',
  '   - 非浏览态（用户没在看历史）不做补偿的既有语义不变。',
  '5. 注释同步：把 `prepend_scroll_delta` 上方那段「已知局限」文档改成本轮事实（保留仍未覆盖的边界与移除条件，不要删成乐观表述）。',
  '',
  '## 任务 2：登记 TUI 集成用例（D7 未完成项）',
  '1. 先读 ' + WORKTREE + '/docs/verification/tui-integration-cases/README.md 的格式与编号约定，以及 `06-transcript-ui.md` / `11-transcript-display.md`（或 README 指向的合适文件）里既有用例的写法。',
  '2. 按约定新增一条编号 `P0-77` 的用例，覆盖「滚动到已载入窗口顶端 → 加载更早历史 → 合并与锚点表现 + 切会话/rewind 丢弃迟到页」这一人工回归场景；按 :51 的约定链接 active issue（本工作分支对应的 issue 文档：`spec/issues/2026-10-13-transcript-shrink-history-pagination.md`），不要发明新格式或新编号体系。',
  '3. 若 README 要求更新执行顺序表/索引，一并更新。',
  '',
  '## 交付与验证',
  '- 定向测试：' + CD + CARGO + ' test --locked -p peri-tui --lib -- history_paging（以及你新增用例所在的过滤词），记录结果。',
  '- 全量 1 次：' + CD + CARGO + ' test --locked -p peri-tui --lib（必须绿；若 flake，单跑对照并如实记录）。',
  '- 编译：' + CD + CARGO + ' check --locked -p peri-tui --all-targets（**必须过**——上一轮就是这里漏了）。',
  '- 报告写到 ' + WORKTREE + '/.recon/S7d-tui-anchor-and-cases.md：改动清单（绝对路径）、补偿规则的最终语义（含跨界与跟随底部的定义）、测试命令与结果原文、已知边界与移除条件、未验证项。**先写报告骨架与已完成的结论，再继续做剩下的**，避免再次中断后什么都没留下。',
].join('\n')

const PROMPT_VERIFY = [
  '你是独立复核者（verification），默认只读、不采信实现者报告。',
  '',
  BRIEF,
  '',
  CONTEXT_S7C,
  '',
  '## 复核对象',
  '本轮（S7d）对 D6 锚点补偿规则的修改与 D7 集成用例登记。实现者报告在 ' + WORKTREE + '/.recon/S7d-tui-anchor-and-cases.md（当参考，不当代替）。上轮复核报告 ' + WORKTREE + '/.recon/S7c-tui-verification.md 里有场景 A/B 的原始实测方法，可以复用方法但必须重跑。',
  '',
  '## 逐条判据（每条 PASS / FAIL / PARTIAL + 依据类型 + 命令与结果原文）',
  '1. **场景 A**：插入点在视口上方 → 同一可见单元屏幕行不变（用真实 `SlotIndex`/`slot_visual_start` + 生产函数实测，不要只看算术）。',
  '2. **场景 B**：插入点在视口下方（中间缺口填补）→ 同一可见单元屏幕行不变（本轮目标）。',
  '3. **证伪**：把新补偿规则退回旧规则（或等价退化），场景 B 的用例必须 FAIL；恢复后 green。给出两种状态的原文输出。',
  '4. **跟随底部**：`follow_bottom` 为真时视图仍在底部（实测）；非浏览态不补偿的语义未被顺带改动。',
  '5. **无第二份事实源**：插入行的视觉区间与视口事实是否来自生产既有结构（静态核对 + 指出 file:line）；有没有新增跨模块共享可变状态或重复索引。',
  '6. **D7**：`docs/verification/tui-integration-cases/` 的新用例是否符合 README 约定（编号、章节位置、链接、索引更新）；`docs/code-index/peri-tui.md` 与本轮代码是否仍一致。',
  '7. **回归**：' + CD + CARGO + ' test --locked -p peri-tui --lib 全量（记录 passed/failed/ignored）+ ' + CD + CARGO + ' check --locked -p peri-tui --all-targets；只读 `git diff` 逐条核对既有断言有没有被放宽或删除。',
  '8. 明确回答：**A20「阅读锚点不跳动」现在是否成立**；若仍有不可达的插入形态，给最小复现与量化偏差。',
  '注意：cargo 会争抢 target 目录锁，命令串行执行。',
  '',
  '## 报告',
  '写到 ' + WORKTREE + '/.recon/S7d-verification.md：判据表（PASS/FAIL/PARTIAL + 依据类型）、证伪对照、未验证项、结论。',
].join('\n')

phase('S7d 锚点补偿与集成用例')
const fix = await agent(PROMPT_FIX, { label: 's7d-anchor-and-cases' })

phase('S7d 独立复核')
const verify = await agent(PROMPT_VERIFY, { label: 's7d-verify' })

return { fix, verify }
