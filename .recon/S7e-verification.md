# S7e TUI 复核报告（独立复核者）

- 复核工作区：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`（保持未提交；复核者无任何 git 写操作）
- 复核对象：本轮（S7e）「渲染侧身份重定位」替换跨层槽位包络补偿后的真实状态
- 参考（未采信）：`.recon/S7e-anchor-relocation.md`（实现者报告）、`.recon/S7d-verification.md` §5.1（根因）、`.recon/S7e-verify-probe-output.txt`（本站探针原文）
- 复核日期：2026-10-14；全部数值来自本工作区当前代码的实跑输出

## 1. 结论摘要

1. **A20「阅读锚点不跳动」在锚点单元存活的所有已测形态下成立**（判据 1/2 = PASS）：折叠组反例（桥接 4 槽 vs 渲染 3 槽）、拆组（渲染槽位 2 → 4）、场景 A（插入在视口上方）、场景 B（中间缺口填补），锚点单元屏幕行逐一实测不变。
2. **跨层槽位路径已彻底消失**（判据 4 = PASS）：`HISTORY_PREPEND_MARK` / 包络版视觉区间换算 / `prepend_scroll_delta` 在工作区无任何代码引用；`inserted_slot_range` 只剩桥接层内部（定义 + 写入 + 日志 + 同层断言），无渲染侧消费点；渲染侧唯一的跨层事实是 `HISTORY_PREPEND_EPOCH`（纯计数，不带槽位号）。
3. **证伪成立**（判据 3 = PASS）：把 `Transcript::prepare` 的重定位短路后，折叠组反例、场景 A、拆组三条用例全部 FAIL（视口分别跳 1 / 3 / 4 行）；还原后字节一致、复跑全绿。
4. **残留边界（判据 8 的限定条件，实现者已声明）**：锚点单元**自身**被并入新折叠组、或锚点身份是折叠组而该组被拆开时，`relocate` 返回 `None`（不补偿，安全方向）。此时「锚点单元」已不存在，严格意义上的 A20 无法度量；实测一种形态下同屏下方内容位移 0 行（高度恰好相等），另一种形态下位移 +4 行（组展开的布局增量）。
5. 判据 5/6/7 见 §6–§8；未验证项见 §10。

## 2. 判据表

| # | 判据 | 判定 | 依据类型 |
| --- | --- | --- | --- |
| 1 | 折叠组反例：桥接槽位数 ≠ 渲染槽位数时同一可见单元屏幕行不变 | **PASS** | 实测（自造端到端探针：真实 `handle_history_page_loaded` → `push_view_models` → `Transcript::prepare`） |
| 2 | 场景 A / B：插入在视口上方 / 下方锚点都不跳 | **PASS** | 实测（同上） |
| 3 | 证伪：短路补偿 → 判据 1/2 用例 FAIL；恢复后 green | **PASS** | 实测（两种状态原文见 §4） |
| 4 | 判据 5 重验：无「桥接槽位索引渲染槽位」路径、无第二份索引 | **PASS** | 静态（全量 grep + 逐调用点判定） |
| 5 | 跟随底部 / 非浏览态 / 身份缺失三条边界符合声明 | **PASS** | 实测（决策层 + 两条真实身份缺失形态）+ 静态（调用点与后续分支） |
| 6 | 回归：全量 lib + `check --all-targets` + 断言审计 | **PASS** | 实测（见 §7） |
| 7 | 文档：注释与 `docs/code-index/peri-tui.md` 与代码一致 | **PASS** | 静态（逐条比对；上一轮点名的旧前提措辞已清除） |
| 8 | A20 现在是否成立（含折叠组形态） | **PASS（含一条已声明的边界，见 §9）** | 实测（判据 1/2 的四形态 + 两条身份消失形态） |

## 3. 判据 1 / 2：自造端到端探针

1. 探针**不是**实现者用例：自建桥接状态、真实派发 `HistoryPageLoaded`、真实 `push_view_models` 发布、真实 `Transcript::prepare` 两帧（发布前捕获 / 发布后重定位），锚点单元**按内容自行查找**（不采信任何槽位常量），视口偏移按生产 effect 口径 `target_for(false, scroll_y)`（`None` → 不动 offset）。
2. 命令（基线）：`cd …/peri-transcript-history && ./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- zz_s7e --nocapture`
3. 实测原文（`.recon/S7e-verify-probe-output.txt` 状态一，逐字节）：
   ```
   S7E-PROBE[collapsed]: 桥接槽位=4 渲染槽位=3 渲染序列=["bubble(inherited)", "GROUP(2)", "bubble(first-screen-own)"] before_rows=6 after_rows=7 scroll_y=4 relocation=Some((4, 5)) row_before=-1 row_after=-1
   S7E-PROBE[scenario-A]: 桥接槽位=3 渲染槽位=3 序列=["bubble(inherited)", "bubble(old one)", "bubble(first-screen-own)"] scroll_y=4 relocation=Some((4, 7)) row_before=-1 row_after=-1
   S7E-PROBE[scenario-B]: 桥接槽位=3 渲染槽位=3 序列=["bubble(own-a)", "bubble(between)", "bubble(own-b)"] scroll_y=1 relocation=Some((1, 1)) row_before=-1 row_after=-1
   S7E-PROBE[group-split]: 桥接槽位=4 渲染槽位=4 前索引槽位=2 序列=["card(t1)", "bubble(between)", "card(t2)", "bubble(below-own)"] scroll_y=2 relocation=Some((2, 6)) row_before=-1 row_after=-1
   test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 1823 filtered out; finished in 0.06s
   ```
4. 判读：
   1. **判据 1（S7d 反例本体）**：桥接 `committed` = 4 槽，渲染 `VIEW_MODELS.items` = 3 槽（槽 1 是 `TuiCollapsedGroup(2)`）——两把尺子长度实测不同；锚点屏幕行 `-1 → -1` 不变。重定位量 `4 → 5` = 1 行 = 折叠组在**渲染布局**里的真实高度（不是桥接槽位数 2）。S7d 在同一形状上的旧路径实测 `delta = 5`（过补偿 2 行），本轮为 1，方向与量都对。
   2. **场景 A**：重定位 `4 → 7`（+3 行），锚点屏幕行不变；断言 `target > scroll_y` 说明补偿真的发生（非恒真）。
   3. **场景 B**：重定位 `1 → 1`（视口不动），锚点屏幕行不变。
   4. **拆组（反向：渲染槽位变多）**：前索引 2 槽 → 后索引 4 槽，重定位 `2 → 6`，锚点屏幕行不变——两把尺子在两个方向上都实测不同，判据仍成立。

## 4. 判据 3：证伪（两种状态原文）

1. 退化点（最小、可字节还原）：`peri-tui/src/kit/message_area/transcript.rs` 的配对分支内，在原赋值之后加一行 `self.prepend = None;`（标记 `// [S7e 复核者证伪临时] 短路身份重定位`）。
2. 退化后命令与输出原文（`.recon/S7e-verify-probe-output.txt` 状态二）：
   ```
   S7E-PROBE[collapsed]: … relocation=None row_before=-1 row_after=0
   assertion `left == right` failed: 折叠组形态：同一可见单元屏幕行不变（A20）
     left: -1
    right: 0                                        ← 视口跳 1 行
   S7E-PROBE[scenario-A]: … relocation=None row_before=-1 row_after=2
   assertion `left == right` failed: 场景 A：同一可见单元屏幕行不变
     left: -1
    right: 2                                        ← 视口跳 3 行
   S7E-PROBE[group-split]: … relocation=None row_before=-1 row_after=3
   assertion `left == right` failed: 拆组形态：锚点屏幕行不变（A20）
   test result: FAILED. 4 passed; 3 failed; 0 ignored; 0 measured; 1823 filtered out; finished in 0.06s
   ```
3. 诚实标注：**场景 B 在退化下仍通过**——它的正确行为（不动视口）与「不补偿」重合，因此该用例在本证伪下不具备区分力；它的区分力在 S7d（旧「按插入高度一律补偿」口径实测 −4 行）已由上一轮证伪过。本轮为它补了一个量化事实：插入块落在视口下方时，视口上方行数变化为 0（`relocation = (1,1)`）而插入块本身占 ≥1 行 → 任何「按插入高度补偿」的规则都会把画面推走。
4. 还原：`cp` 回备份 + `diff -q` 无输出（= 字节一致）+ `shasum -a 256` 与备份一致
   （`transcript.rs` = `d79b2c328b72a93045739ce456aa6e6b400e16a819b5c91572f761728c361ba2`，与实现者 §5.3 自述的 `d79b2c32…61ba2` 相符）；`grep -c "证伪临时"` = 0。
5. 还原后复跑：见 §7 全量回归（含 7 条探针全绿）。

## 5. 判据 4：跨层槽位路径已消失（静态全量核对）

1. 每个符号逐个判定（`grep -rn` 全工作区，命令见下）：
   ```
   grep -rn "HISTORY_PREPEND_MARK|inserted_slot_range|slots_visual_range|prepend_visual_range|prepend_scroll_delta|HISTORY_PREPEND_EPOCH" peri-tui/src/ docs/
   ```
   1. `HISTORY_PREPEND_MARK`：**0 个代码引用**；唯一命中是 `docs/code-index/peri-tui.md:15` 的历史说明（写明「整体删除」）。
   2. `prepend_scroll_delta` / `prepend_visual_range` / 包络版 `SlotIndex::slots_visual_range`：**0 命中**（`transcript_index.rs` 与 HEAD 无 diff，说明该文件已还原到本轮之前的状态）。
   3. `inserted_slot_range`：只剩 4 处——`history_paging.rs:270`（定义）、`:309`（`merge_page` 内写入）、`acp_events/history_page.rs:85`（并入日志字段）、`history_paging_test.rs:94/105/118`（同层断言）。四处都在**桥接层**，消费的是 `state.committed` 下标；定义处注释明写「不得参与渲染侧计算」。
   4. `HISTORY_PREPEND_EPOCH`：桥接侧 `history_page.rs:79`（+1，在 `push_view_models` 之前）、渲染侧 `transcript.rs:160/170`（配对）与 `mod.rs:247`（读序）+ 用例。**只承载「发生过一次并合」，不带任何槽位号**。
2. 其余槽位 API 的调用点逐个判定：`SlotIndex::slot_visual_range`（保留的**单槽**版本）只被 `mod.rs:296`（interaction block 锚定，用当帧 `slot_index`）与测试调用；`slot_visual_start` / `visual_lookup` 的调用者全部持有**当帧** `SlotIndex`（`viewport_anchor.rs`、`mod.rs`、`selection.rs`、测试），没有跨层传递。
3. 沙箱内同层的对照：`apply_pending_tool_ends`（`history_page.rs:213-221`）用 `HistorySpans::slots()` 索引 `state.committed.get(index)`——**同一层**，不受影响（与 S7d §5.1 结论一致）。
4. 无第二份索引 / 无新增跨层共享可变状态：跨层仍只有 `HISTORY_PREPEND_EPOCH`（`atoms.rs:517`，纯 `u64`，会话边界清空）；渲染侧配对状态 `Transcript.merge_epoch_seen` 是渲染侧私有字段（`transcript.rs:36`），不跨层共享。

## 6. 判据 5：跟随底部 / 非浏览态 / 身份缺失

1. **跟随底部**（实测）：`target_for(follow_bottom = true, scroll_y) == None`；生产调用点 `auto_follow.rs:249-252` 传 `!browsing`（跟随态 = `true`）→ `None` → **不调用 `set_offset`**（`:262` 在 `if let Some(...)` 内）。跟随分支（`:286` 起的 `scroll_to_bottom` 路径）本轮未改。
2. **底部哨兵**（实测）：`target_for(false, usize::MAX) == None`，与 `scroll_to_bottom` 的哨兵语义一致。
3. **非浏览态**（实测 + 静态）：浏览态按**相对量** `scroll_y + (target − from)` 落 offset；实测 `from=5,target=42,scroll_y=7 → 44`（不覆盖期间发生的其它滚动）。
4. **身份缺失**（实测两条真实形态，端到端）：
   1. 锚点卡被并入新折叠组：`relocation = None`（下方内容位移 0 行，本 fixture 中卡高 = 组高 = 1 行）。
   2. 锚点身份是折叠组、组被拆开：`relocation = None`（下方内容位移 +4 行 = 组展开的布局增量）。
   两条都与实现者声明的「认不出 → 不补偿（安全方向）」一致；代码路径 `viewport_anchor.rs:138-144`。
5. **同一次发布不重复补偿**（实测）：同一快照连续两次 `prepare`（`published == false`）均返回 `None`。

## 7. 判据 6：回归

1. 全量 lib（**最终状态**：探针与临时注册行均已删除，`git status` 无新增文件）：
   ```
   cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history && \
   ./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib
   ```
   ```
   test result: ok. 1817 passed; 0 failed; 6 ignored; 0 measured; 0 filtered out; finished in 24.69s
   TEST_EXIT=0
   ```
   （与实现者报告 §6.1 的 1817 / 0 / 6 一致；未复现 S7d 提到的 `steer_consumer` flake。）
2. 探针在场时的全量：`1824 passed; 0 failed; 6 ignored`（= 1817 + 7 条探针；`.recon/S7e-verify-probe-output.txt` 状态一）。
3. 编译检查（最终状态）：
   ```
   ./scripts/cargo-rmcp-patched.sh check --locked -p peri-tui --all-targets
   ```
   ```
       Checking peri-tui v0.2.0 (…)
       Finished `dev` profile [unoptimized + debuginfo] target(s) in 36.26s
   CHECK_EXIT=0
   ```
4. **断言审计**（`git diff -U0 -- peri-tui`，只读）：删除行中含 `assert` 的 **1 行**、新增 `assert` **23 行**、无新增 `#[ignore]` / `should_panic`、无测试文件被删。唯一那条删除是 `session_events_test.rs` 里 `assert!(!state.compact_just_completed, "场景 A: flag 应清除");` 与其尾随注释被拆行——**该断言在同一处上方仍在**（现 `:856`），且同处新增 `rx_a.try_recv().is_err()` 等更强断言，非放宽。
5. 局限：工作区未提交，无法用 git 隔离「S7c→S7d→S7e」的增量，以上是整分支 diff（与上一轮同口径）。

## 8. 判据 7：文档一致性

1. `viewport_anchor.rs` 模块文档（`:1-61`）：与代码逐条对应——capture/relocate 语义、安全方向、身份来源、读序不变量、四条边界（含「锚点粒度 = 顶层单元」「单元内位移越界不夹取」「同帧多页只按最新快照重定位一次」），无夸大。
2. `history_paging.rs:262-270`（`inserted_slot_range` 定义）与 `acp_events/history_page.rs:75-79`（epoch 写入点）：都写明「桥接槽位与渲染槽位是两把尺子」「不得跨层」，与代码一致。
3. `atoms.rs:511-517`：`HISTORY_PREPEND_EPOCH` 文档改为「只表达发生过一次并合、不带槽位号」，并点名 S7d §5.1 的历史失败路径——与现状一致。
4. `auto_follow.rs:238-263`：并入分支注释描述身份重定位与「认不出 → 不补偿」，与代码一致。
5. `docs/code-index/peri-tui.md`：`:15` 更新行与 `:80` 速查行都改为「渲染侧身份重定位 + 桥接槽位号不跨层」；上一轮点名的旧前提措辞（「渲染 body 用当帧同一份视觉索引换算成视觉行区间」）**已清除**（`grep` 全 `docs/` 0 命中）。
6. `docs/verification/tui-integration-cases/06-transcript-ui.md` P0-77（`:21-25`）与 `README.md:32` 索引：描述与当前语义一致（不依赖槽位包络）。

## 9. 判据 8：A20 现在是否成立

1. **成立（实测四形态，锚点单元在新窗口里存活）**：折叠组反例（桥接 4 槽 vs 渲染 3 槽）、拆组（2 槽 → 4 槽）、场景 A、场景 B——锚点单元屏幕行逐一不变（§3）。
2. **已声明的边界（锚点单元自身消失，不补偿）**：
   1. 触发形状 A：锚点单元是「成功通用工具卡」，更早一页带来紧邻的同类卡 → 两卡被压成一张 `TuiCollapsedGroup`，锚点身份（`FoldKey::Tool(id)`）在新窗口不存在（组身份 = 成员序列）→ `None`。实测下方内容位移 0 行（本 fixture 卡高 = 组高 = 1 行）。
   2. 触发形状 B：锚点身份是折叠组本身，插入把组拆开 → 组身份不存在 → `None`。实测下方内容位移 **+4 行**（组 1 行 → 两张卡 + 插入气泡的展开布局）。
   3. 判定：这两种形态下「同一可见单元」已不存在（被压组 / 被拆开），A20 的度量对象本身消失；代码选择「不补偿」并在模块文档 §「身份缺失」第 2 条显式声明。**不是隐藏缺陷，但是真实边界**——若要求「锚点所在组的位置也不许动」，需要按组成员关系重定位；复核者判断这属另一条设计取舍（在「上方内容重排时保持下方内容不动」与「锚点内容被压组时保持其行号」之间，这两者不可同时满足）。
3. **残留风险（静态推断，未实测）**：非折叠单元的身份是「种类判别位 + 内容 hash」，窗口内出现**两枚内容完全相同**的同类单元（如两条相同文案的 user 气泡、`content_hash` 恒为 0 的 `TuiDivider` 出现两枚）时，`relocate` 判歧义 → `None` → 不补偿。实现者已在文档中声明该方向，代码路径 `viewport_anchor.rs:129-135`；未构造端到端反例（预算内未覆盖，见 §10）。

## 10. 未验证项与局限

1. **未跑渲染帧 / 整组件树**：`AutoFollowCtx` 的 `State<T>` 在仓库内无法在测试中构造（`scroll_test.rs:549-558` 既有说明），因此探针覆盖到「生产决策层 + 生产调用点语义」（`target_for` + `set_offset` 参数），未覆盖 `use_effect` 的真实调度。读序不变量（快照早于 epoch 读）为静态核对。
2. **未覆盖「同帧多页」与「迟到页 / rewind 竞争」**：属上一轮（S7d/REQ-C02）已覆盖范围，本轮未重复。
3. **未实测身份歧义反例**（§9.3），仅静态判定代码路径。
4. 探针为**临时文件**，已在收尾时删除并校验工作区无残留（见 §11）。
5. 未能用 git 隔离各轮增量（工作区未提交）。

## 11. 复核者自身的可还原性核验

1. `peri-tui/src/kit/message_area/viewport_anchor.rs`：临时追加的 `mod zz_s7e_probe;` 注册行已删除，`cp` 回备份后 `diff -q` 无输出 → `VIEWPORT_ANCHOR-RESTORED-BYTES-IDENTICAL`；`grep -c "zz_s7e_probe"` = 0。
2. `peri-tui/src/kit/message_area/zz_s7e_probe_test.rs`：已 `rm`；`git status --short peri-tui/src/kit/message_area/` 中无该文件（残留检查输出 `NO_PROBE_FILE_LEFT`）。
3. `peri-tui/src/kit/message_area/transcript.rs`：证伪改动已 `cp` 还原，`diff -q` 无输出 → `RESTORED-BYTES-IDENTICAL`、`shasum -a 256` 与备份相同（§4.4）。
4. 探针源码与输出留存 `.recon/S7e-verify-probe.{rs.txt,output.txt}`、静态核对留存 `.recon/S7e-verify-grep.txt`；复核者无任何 `git` 写操作，工作区保持未提交（`git status --short` 171 行，均为实现者既有改动 + 26 个未跟踪文件，无复核者新增文件）。
5. **哈希交叉核对（复核结束时实测）**：`transcript.rs` = `d79b2c328b72a93045739ce456aa6e6b400e16a819b5c91572f761728c361ba2`、`viewport_anchor.rs` = `0ffe83d4bc8b1ffaebfbd69c7d4497b8bd40c289c93d4032abc41572b1592b1c`，与实现者报告 §5.3 自述的两个还原哈希逐字节相同——本轮复核的代码状态就是实现者报告所描述的代码状态。

## 12. 结论

1. 判据 1/2/3/4/5/6/7 = **PASS**；判据 8 = **PASS（含一条实现者已声明、复核已量化的边界：锚点单元自身被折叠/拆组时按设计不补偿）**。
2. 上一轮（S7d）判 FAIL 的根因「桥接槽位号被当成渲染槽位用」在代码里已不存在（判据 4），且在折叠组反例形状上实测不再复现（判据 1）。
3. 交付建议：本轮修复可接受独立复核；建议把 §9.2 的两条边界写进 TUI 集成用例 P0-77 的备注（当前用例只描述「阅读位置不跳」，未限定「锚点单元存活」前提）。
