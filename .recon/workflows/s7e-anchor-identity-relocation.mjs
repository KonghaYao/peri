export const meta = {
  name: 's7e-anchor-identity-relocation',
  description: 'Replace the cross-layer slot-envelope anchor compensation with identity-based re-localization on the render side, so A20 holds for any insertion position and with folded tool groups',
}

const WORKTREE = '/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history'
const CD = 'cd ' + WORKTREE + ' && '
const CARGO = './scripts/cargo-rmcp-patched.sh'

const BRIEF = [
  'worktree（唯一工作目录，绝对路径）：' + WORKTREE,
  '所有 bash 命令必须以 `cd ' + WORKTREE + ' && ` 开头（bash 默认 cwd 是主仓库）。Cargo 一律用 `./scripts/cargo-rmcp-patched.sh`（禁止裸 cargo、禁止 --workspace）。',
  '禁止：git 写操作（工作区保持未提交）、DDL、新增依赖、改服务端（peri-acp / peri-acp-types / peri-resources）与 SDK（npm-packages）。',
  '**迭代预算纪律**：本任务的前一个 agent 在 200 轮迭代上限被中断（没落报告）。你必须先读全、改动收敛、用定向测试而非反复全量，并且**先把报告骨架与已得结论写盘**再继续。',
].join('\n')

const ROOT_CAUSE = [
  '## 根因（独立复核已定位到 file:line，见 ' + WORKTREE + '/.recon/S7d-verification.md 第 5.1 节；不要重新发明）',
  '上一轮（S7d）把锚点补偿从「按总行数增量」改成「只补偿视口上方的插入高度」，几何判定本身正确（复核已用证伪证明其有效），但它**把两套槽位坐标系当成了一套**：',
  '1. `HISTORY_PREPEND_MARK` 写入的是 `MergeOutcome::inserted_slot_range`（`peri-tui/src/kit/acp_events/history_page.rs:78`），是桥接侧 `state.committed` 的下标（`peri-tui/src/kit/history_paging.rs:290-306`）。',
  '2. 渲染侧 `SlotIndex` 的槽位是**发布后** `snapshot.items` 的下标（`peri-tui/src/kit/message_area/transcript.rs:117`）。',
  '3. 发布前会跑「相邻成功工具卡压成一张 `TuiCollapsedGroup`」（`peri-tui/src/kit/acp_events/render.rs:140` / `:461` / `:658`）→ **槽位数会变少**。复核者实测：桥接 `state.committed` 4 槽 vs 渲染 `VIEW_MODELS.items` 3 槽（`["bubble", "GROUP(2)", "bubble"]`），mark = (1,3)。',
  '4. 消费方直接用**桥接包络**去索引**渲染槽位**：`peri-tui/src/kit/scroll/auto_follow.rs:12-18` 的 `index.slots_visual_range(slot_range.0..slot_range.1)`。',
  '后果两个方向都有：**过补偿**（包络终点恰好等于渲染槽数时，`transcript_index.rs:282-297` 的 `slots.end == self.count` 分支取整窗视觉总行数，`scroll.rs:64` 的 `set_offset` 又不夹取 → 视口被推到窗口底部）；**不补偿**（终点越界 → `None` → delta 0 → 可见内容仍被整块推下）。',
  '复核者留了可复用的探针：' + WORKTREE + '/.recon/S7d-verify-probe.rs.txt（端到端发布路径 + 生产派生函数，含桥接/渲染槽位数对照）与输出 ' + WORKTREE + '/.recon/S7d-verify-probe-output.txt。',
].join('\n')

const PROMPT_FIX = [
  '你是实现者。本轮目标是让 A20「阅读锚点不跳动」在**任意插入形态**下成立，并彻底去掉跨层槽位坐标。',
  '',
  BRIEF,
  '',
  ROOT_CAUSE,
  '',
  '## 要求：按「身份重定位」实现（这正是该函数上方注释里原本写下的字面计划，现在把它做实）',
  '1. 在**渲染侧**（拥有发布后 `snapshot.items` 与 `SlotIndex` 的那一层）做锚点重定位：',
  '   - 发布新快照**之前**记录当前视口顶部可见单元的**稳定身份**（条目身份 + 子项序号，按仓库既有身份类型取，不要新造身份体系）与「视口偏移落在该单元视觉起点的哪个位移」（within-unit offset）；',
  '   - 发布之后用同一身份在**新**索引里重查它的视觉起点，令 `new_offset = new_visual_start - within_unit_offset`；',
  '   - 身份在新快照里找不到（被 rewind 替换、被移除等）→ **不补偿**（安全方向），并保留可观测的日志/注释说明；',
  '   - 跟随底部（`follow_bottom`）与非浏览态的既有语义不变。',
  '2. 让 `HISTORY_PREPEND_MARK` 不再承载「桥接槽位包络」这个跨层语义：可以只保留「发生过并合」这一事实（epoch 变化已经承担该职责），也可以整体删除该 atom —— 由你按最小改动决定，但**不得**再出现「桥接槽位号直接索引渲染槽位」的路径（这是复核判定 FAIL 的那一条）。',
  '3. 处理跨层重排的两种已知形态：插入把原有相邻工具卡**合并成折叠组**（原有单元的视觉起点会整体变化）；插入把折叠组**拆开**。身份重定位对两者都应成立——这也是本轮验收用例必须覆盖的形态。',
  '4. 注释与文档同步：把 `prepend_scroll_delta` / 相关函数的文档改成本轮事实（身份重定位、找不到身份时的行为、仍未覆盖的边界与移除条件）；`docs/code-index/peri-tui.md` 里沿用旧前提的措辞一并更正（复核报告第 5.3 节点名了这处）。',
  '',
  '## 测试（必须能失败；用真实发布路径而不是手工坐标）',
  '1. **折叠组形态（复核者的反例）**：按 ' + WORKTREE + '/.recon/S7d-verify-probe.rs.txt 的构造法端到端跑真实发布路径（`handle_history_page_loaded` → `push_view_models`），使桥接槽位数 ≠ 渲染槽位数；断言同一可见单元的屏幕行**不变**。',
  '2. 场景 A（插入在视口上方）与场景 B（插入在视口下方 / 中间缺口填补）：同一可见单元屏幕行不变。',
  '3. **证伪**：把补偿退回旧规则（或把身份重定位短路），上面至少两条用例必须 FAIL；给出两种状态的原文输出。',
  '4. 跟随底部为真时视图仍在底部；非浏览态不补偿。',
  '5. 匿名/身份缺失的边界：身份在新快照中不存在时不补偿（用例或明确论证）。',
  '',
  '## 交付与验证（报告里给命令原文 + 结果原文）',
  '- ' + CD + CARGO + ' test --locked -p peri-tui --lib -- history_paging（及你新增用例的过滤词）',
  '- ' + CD + CARGO + ' test --locked -p peri-tui --lib（全量必须绿；flake 要单跑对照）',
  '- ' + CD + CARGO + ' check --locked -p peri-tui --all-targets（**必须过**）',
  '- 报告写到 ' + WORKTREE + '/.recon/S7e-anchor-relocation.md：最终语义（含 within-unit offset 与找不到身份时的行为）、改动清单（绝对路径）、测试命令与结果原文、已知边界与移除条件、未验证项。**先写骨架与已得结论再继续**。',
].join('\n')

const PROMPT_VERIFY = [
  '你是独立复核者（verification），默认只读、不采信实现者报告。',
  '',
  BRIEF,
  '',
  ROOT_CAUSE,
  '',
  '## 复核对象',
  '本轮（S7e）用「渲染侧身份重定位」替换跨层槽位包络补偿后的真实状态。实现者报告在 ' + WORKTREE + '/.recon/S7e-anchor-relocation.md（当参考，不当代替）。上一轮你的同僚留下的判定在 ' + WORKTREE + '/.recon/S7d-verification.md（第 5.1 节是根因，判据 5 = FAIL、判据 8 = FAIL）。',
  '',
  '## 逐条判据（PASS / FAIL / PARTIAL + 依据类型 + 命令与结果原文）',
  '1. **折叠组反例（本轮核心）**：端到端真实发布路径下，桥接槽位数 ≠ 渲染槽位数时，同一可见单元的屏幕行**不变**。必须自己构造（可以复用 ' + WORKTREE + '/.recon/S7d-verify-probe.rs.txt 的方法，但要重跑）——不要只接受实现者的用例结论。',
  '2. **场景 A / B**：插入在视口上方 / 下方时锚点都不跳动。',
  '3. **证伪**：把补偿短路（或退回旧规则）→ 判据 1/2 的用例 FAIL；恢复后 green。给两种状态原文。',
  '4. **判据 5 重验（上轮 FAIL 项）**：确认工作区不再存在「桥接槽位号索引渲染槽位」的路径（`grep` 全部 `HISTORY_PREPEND_MARK` / `inserted_slot_range` / `slots_visual_range` 的调用点，逐个判定坐标系来源）；确认没有新增第二份索引或跨层共享可变状态。',
  '5. **跟随底部 / 非浏览态 / 身份缺失**：三条边界的行为符合实现者声明（实测或明确论证）。',
  '6. **回归**：' + CD + CARGO + ' test --locked -p peri-tui --lib 全量（记录 passed/failed/ignored）+ ' + CD + CARGO + ' check --locked -p peri-tui --all-targets；只读 `git diff` 逐条核对既有断言没有被放宽或删除。',
  '7. 文档：`history_paging`/`auto_follow` 的注释与 `docs/code-index/peri-tui.md` 是否与本轮代码一致（上一轮点名的旧前提措辞是否已更正）。',
  '8. 明确回答：**A20「阅读锚点不跳动」现在是否成立**（含折叠组形态）；若仍有反例，给最小复现与量化偏差。',
  '注意：cargo 会争抢 target 目录锁，命令串行执行。',
  '',
  '## 报告',
  '写到 ' + WORKTREE + '/.recon/S7e-verification.md：判据表、证伪对照、未验证项、结论。',
].join('\n')

phase('S7e 锚点身份重定位')
const fix = await agent(PROMPT_FIX, { label: 's7e-anchor-relocation' })

phase('S7e 独立复核')
const verify = await agent(PROMPT_VERIFY, { label: 's7e-verify' })

return { fix, verify }
