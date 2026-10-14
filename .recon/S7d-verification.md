# S7d TUI 复核报告（独立复核者）

- 复核工作区：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`（保持未提交；复核者无任何 git 写操作）
- 复核对象：本轮（S7d）D6 锚点补偿规则改写 + D7 集成用例登记
- 参考（未采信）：`.recon/S7d-tui-anchor-and-cases.md`（实现者报告）、`.recon/S7c-tui-verification.md`（上轮权威记录，D1–D5、D8 已 PASS，本轮未动）
- 复核日期：2026-10-14；全部数值来自本工作区当前代码的实跑输出

## 1. 结论摘要

1. **A20「阅读锚点不跳动」仍不成立（判据 8 = FAIL）**，但**失败原因与上轮不同**。新规则的几何判定本身是对的（判据 1/2 在其口径内 PASS，判据 3 证伪证明它真的在起作用），坏在**跨层槽位坐标**：`HISTORY_PREPEND_MARK` 里存的是**桥接侧 `state.committed` 的槽位**，消费它的渲染侧 `SlotIndex` 用的是**发布后 `VIEW_MODELS.items` 的下标**——只要窗口里出现「被压成折叠组的相邻成功工具卡」，两套坐标就不再相等（本轮实测：桥接 4 槽 vs 渲染 3 槽）。
2. **判据 5「无第二份事实源」= FAIL**。没有新造索引，但**把桥接槽位当渲染槽位用**了；这正是实现者报告 §6.2 请复核者独立核对的那一条，它不成立。
3. **判据 1/2 的 PASS 是「口径内 PASS」**：4 条新用例确实走真实 `merge_page` + 真实 `SlotIndex` + 生产派生函数；但它们的索引与包络**由测试手工构造成同一坐标**，所以**结构上不可能**发现第 1 条的错位。
4. **额外发现（本轮范围之外、同文件）**：`acp_events/history_page.rs:182-195` 的 `complete_in_place` 在「同一条目内起点 + 终点」形状下**panic**（`index out of bounds: the len is 0 but the index is 0`，`:192`），实测稳定复现。当前服务端投影不会产出该形状（一个 payload 只发射起点或终点），属**潜在**缺陷；但该函数文档注释正宣称支持这种配对（「起点先到就原地升级成终态」）。
5. **D7（判据 6）= PASS**：`06-transcript-ui.md` 的 P0-77 与 `README.md` 的执行顺序第 20 项都符合该目录约定（编号 = 全目录最大 +1、区域文件内有序列项、链接 active issue、README 索引同步）；`docs/code-index/peri-tui.md` 的符号与文件路径均存在、描述与代码文字一致（但描述沿用了判据 5 里那个错误前提，见 §5.3）。
6. **判据 7 = PASS**：全量 lib `1810 passed; 0 failed; 6 ignored`；`check --all-targets` 干净；逐行审计 `git diff` 未见任何断言被放宽/删除（唯一一条删除断言行的替换仍是 S7c 已核过的 REQ-C02 强化组）。

## 2. 判据表

| # | 判据 | 判定 | 依据类型 |
| --- | --- | --- | --- |
| 1 | 场景 A：插入点在视口上方 → 同一可见单元屏幕行不变 | **PASS（口径内）** | 实测（真实 `merge_page` + 真实 `SlotIndex` + 生产派生函数） |
| 2 | 场景 B：插入点在视口下方（中间缺口填补）→ 同一可见单元屏幕行不变 | **PASS（口径内）** | 实测（同上） |
| 3 | 证伪：退回旧规则 → 场景 B 用例 FAIL；恢复后 green | **PASS** | 实测（两种状态关键行逐字节摘录见 §4） |
| 4 | 跟随底部不补偿、非浏览态语义未改 | **PASS**（决策层） | 实测（`delta == 0`）+ 静态（调用点与后续 follow 分支） |
| 5 | 无第二份事实源：插入行视觉区间来自生产既有结构 | **FAIL** | 实测（端到端发布探针）+ 静态（`transcript.rs:117`） |
| 6 | D7 用例登记与 code-index 一致性 | **PASS**（一处措辞见 §5.3） | 静态 |
| 7 | 回归：全量 lib + `check --all-targets` + 断言审计 | **PASS** | 实测 |
| 8 | A20 现在是否成立 | **FAIL（有实测反例）** | 实测（最小复现 + 量化） |

## 3. 判据 1 / 2：A20 场景 A、B（口径内）

### 3.1 命令与结果（原文）

```
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history && \
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- prepend
```

```
running 6 tests
test kit::message_area::scroll::tests::test_prepend_anchor_holds_when_insert_straddles_viewport_top ... ok
test kit::message_area::scroll::tests::test_prepend_no_compensation_for_follow_and_unknown_blocks ... ok
test kit::message_area::scroll::tests::test_prepend_anchor_holds_when_insert_lands_below_viewport ... ok
test kit::message_area::scroll::tests::test_prepend_compensates_when_insert_above_viewport ... ok
test kit::acp_events::acp_events_test::history_page_test::test_history_page_prepends_older_page_after_inherited_prefix ... ok
test kit::acp_events::acp_events_test::history_page_test::test_history_page_prepends_after_prefix_without_replay_done ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
```

### 3.2 用例测的是什么（逐条核对，非采信报告）

1. **场景 A**（`scroll_test.rs:815`）：窗口 `[继承(1 行), 自有(5 行)]`，更早一页插到继承前缀之后（桥接槽位 `(1,2)`），当帧视觉区间 `(1,5)`；`delta = 4`；屏幕行 = `slot_visual_start − delta`：自有条目 `1 → 5−4 = 1`，**不变**。
2. **场景 B**（`:850`）：视口 `[0,4)` 只看得到 A；插入块（`position 30`）落在 B(20) 与 D(40) 之间 → 插入块视觉区间 `(8,12)`，`block_start(8) >= scroll_y + vis_height(4)` → `delta = 0`；A 的屏幕行 `0 → 0`，**不变**。
3. 另外两条覆盖「完全在视口上方（`delta` = 块高，整屏保位）」与「跟随态 / 非翻代帧 / 区间取不到（一律 0）」。
4. 这三条的**坐标前提是测试自己构造的**：`index_of(&[1,4,5])` 这类手工索引与 `HISTORY_PREPEND_MARK` 的槽位号被当成同一套坐标系。真实发布路径下这个前提不成立（§5.1），因此本项按「给定口径内 PASS」记录，而不是「产品行为 PASS」。

## 4. 判据 3 / 4 / 7

### 4.1 判据 3：证伪（退回旧规则 → 场景 B 必须 FAIL）

1. 退化写法（`history_paging.rs::prepend_scroll_delta`，只改判定分支，等价的旧「一律按插入高度补偿」口径）：
   ```rust
   let _ = (scroll_y, vis_height);
   let Some((_block_start, block_end)) = inserted_visual_range else { return 0; };
   if true { block_end.saturating_sub(_block_start) } else { 0 }
   ```
2. 退化后 `test --lib -- prepend_anchor` 输出（关键行摘录，逐字节一致；完整日志路径见 §4.1.4 归档文件）：
   ```
   test kit::message_area::scroll::tests::test_prepend_anchor_holds_when_insert_straddles_viewport_top ... ok
   test kit::message_area::scroll::tests::test_prepend_anchor_holds_when_insert_lands_below_viewport ... FAILED
   assertion `left == right` failed: 插入块整体在视口下沿之下 → 视口不动
     left: 4
    right: 0
   test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 1817 filtered out; finished in 0.00s
   ```
3. 恢复：`cp` 回备份后 `diff -q` 与备份**字节一致**（`RESTORED-BYTES-IDENTICAL`）、`grep -c 临时证伪` = 0；复跑 `-- prepend` → `6 passed; 0 failed`。
4. 结论：**场景 B 的新判据是载荷**（不是恒真断言），证伪成立。关键行逐字节摘录留档 `.recon/S7d-verify-falsify-scenario-b.txt`，该文件同时记录原始日志完整路径与省略的行。

### 4.2 判据 4：跟随底部

1. **实测（决策层）**：`test_prepend_no_compensation_for_follow_and_unknown_blocks` 断言 `prepend_scroll_delta(true, true, 0, 4, Some((1,5))) == 0`，本轮复跑 ok。
2. **调用点静态**（`auto_follow.rs:268-285`）：`prepend_scroll_delta(!browsing, true, scroll_y, ctx.vis_height, ctx.prepend_visual_range)`；`delta == 0` 时**不调用** `set_offset`，即跟随态下翻页不动 offset。
3. **非浏览态语义未改**：翻页块的后续路径未动——`if !*ctx.follow_bottom.read() { return; }`（浏览态直接返回）之后仍是 `loading → scroll_to_bottom` / `items_len < prev → scroll_to_bottom` / `total_visual_rows > prev_lsa → scroll_to_bottom`（`auto_follow.rs:302-341`），跟随态在内容增长时仍吸底。
4. 局限：未跑整组件树/渲染帧（`AutoFollowCtx` 需要 hook 级 `State`，本仓无既有测试构造先例），与 S7c §13.1 同口径。

### 4.3 判据 7：回归

1. **全量 lib**（探针已全部删除后，最终状态）：
   ```
   ./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib
   test result: ok. 1810 passed; 0 failed; 6 ignored; 0 measured; 0 filtered out; finished in 24.20s
   ```
   与实现者报告一致（1810 / 6）。
2. **首次全量的 1 条失败与本轮无关**：第一次全量跑出 `1809 passed; 1 failed; 6 ignored`，失败项 `kit::steer_consumer::tests::test_slow_session_preparation_is_not_capped_by_receipt_deadline`（断言 `NOTIFICATION` atom 为空，实际是别的并行用例写进去的 `"Exported messages to: …peri-debug-export-…"`）。
   - `steer_consumer.rs` / `steer_consumer_test.rs` **不在本分支改动清单里**（`git status` 无记录）；
   - 失败断言读的是**进程级共享 atom** `atoms::NOTIFICATION`（`steer_consumer_test.rs:627-633`），无 `#[serial]`；
   - 复跑通过（§4.3.1）→ 判定为跨用例共享全局状态的既有 flake，不归因本轮。
3. **编译检查**：
   ```
   ./scripts/cargo-rmcp-patched.sh check --locked -p peri-tui --all-targets
   Finished `dev` profile [unoptimized + debuginfo] target(s) in 33.76s    CHECK_EXIT=0
   ```
4. **逐行断言审计**（`git diff -U0 -- peri-tui`，与 S7c 同一命令）：
   - 删除行中含 `assert` 的 **1 行**：`- assert!(!state.compact_just_completed, "场景 A: flag 应清除");`——该处已被 REQ-C02 的强化组替换（`session_events_test.rs:851-860`：窗口保留 `committed.len() == 2` + `rx_a.try_recv().is_err()` + 提示归位），非单纯删除；
   - 新增 `#[ignore]`：0；删除 `#[ignore]`：0；新增 `should_panic`：0；测试文件被删：0；
   - **口径限制**：工作区未提交，无法用 git 隔离 S7c→S7d 的增量，以上是整分支 diff；实现者报告 §1.9 自述「删除 1 条旧的纯算术 `prepend_scroll_delta` 用例」，该文件 `history_paging_test.rs` 是**未跟踪新文件**、原文无法从 git 取回，只能确认替代覆盖存在（`scroll_test.rs` 4 条新用例 + `history_paging_test.rs` 3 处包络断言）。
5. **fmt（实现者 §5.5 自述的核对）**：`rustfmt --edition 2024 --check` 逐文件实测——本轮新增/改动的 `history_paging.rs`、`atoms.rs`、`scroll/auto_follow.rs`、`transcript_index.rs`、`history_paging_test.rs` = 0 偏差；`scroll.rs`、`message_area/mod.rs`、`acp_events/history_page.rs` 的偏差全部落在 `scroll/event.rs:42`、`scroll_test.rs:672/701/719/737`、`history_page.rs:38/146`（前几轮代码），与实现者披露的清单一致。**结论：fmt 自述属实。**

## 5. 判据 5 / 6 / 8

### 5.1 判据 5 = FAIL：桥接槽位坐标被当成渲染槽位坐标

**探针 1（端到端发布路径，真实协议形状）**：桥接窗口 `[继承前缀, 自有条目]`；更早一页含 2 张成功工具卡，起点/终点各占一个条目（与 `peri-acp/src/dispatch/session_replay.rs:449-578` 的真实投影形状一致：一个 payload 只发射 `tool_call` 或 `tool_call_update`）；然后走生产 `handle_history_page_loaded`（`merge_page` → 写 MARK → 推 EPOCH）与生产 `push_view_models`。

逐字节摘录（`.recon/S7d-verify-probe-output.txt`；原文是**一整行**，此处按分号断行便于阅读）：

```
PROBE: 桥接 committed 槽位数 = 4; mark（桥接槽位坐标）= Some((1, 3));
渲染侧 VIEW_MODELS 槽位数 = 3; 渲染侧槽位序列 = ["bubble", "GROUP(2)", "bubble"]
```

**探针 2（生产派生 + 生产判定）**：把上一步的渲染侧现实（3 槽：继承 1 行 / 折叠组 3 行 / 自有 2 行）喂给生产函数：

```
PROBE: 派生视觉区间 = (1, 6)（插入块真实只占 1 槽 = 3 行）；补偿量 delta = 5；正确补偿 = 3（GROUP 高度）→ 过补偿 2 行
```

**链条（每一环都有 file:line）**：

1. `HISTORY_PREPEND_MARK` 写入的是 `MergeOutcome::inserted_slot_range`（`acp_events/history_page.rs:78`），它是 `state.committed` 的下标（`history_paging.rs:290-306`）。
2. 但 `push_view_models` 在发布前会跑「相邻成功工具卡压成一张 `TuiCollapsedGroup`」：`render.rs:140 group_successful_tools_from(&mut items, 0, confirmed_prefix)`，判据 `render.rs:461 is_mergeable`，压组动作 `render.rs:658 grouped.push_back(build_collapsed_group(&run, …))`——**槽位数会变少**。（实现者报告 §6.2 声称「后处理不改变槽位数量」：`apply_fold_pass`（`fold.rs:55`）确实只用 `set`、长度不变——`fold.rs:255-256` 收集完 `(i, vm)` 后原位写回；divider（`render.rs:91-97`）与 todo 摘要（`render.rs:206-270`）也都不动 `committed` 段的槽位号，**但分组不同**。以上 `render.rs` 均指 `peri-tui/src/kit/acp_events/render.rs`。）
3. 渲染 `SlotIndex` 的槽位就是发布后 `snapshot.items` 的下标：`message_area/transcript.rs:117 let vm = &snapshot.items[slot];`。
4. 消费方直接用桥接包络去索引渲染槽位：`auto_follow.rs:12-18 index.slots_visual_range(slot_range.0..slot_range.1)`。

**后果（两种方向都有）**：

1. **过补偿**：包络终点落在一个「仍然存在」的渲染槽位上（例如恰好等于 `count` → `transcript_index.rs:282-297` 的 `slots.end == self.count` 分支取**整窗视觉总行数**）→ 补偿量 = 折叠组 + 之后自有条目的高度，甚至整窗剩余；`set_offset(scroll_y + delta)`（`scroll.rs:64` 不夹取）把视口推到窗口底部。
2. **不补偿**：包络终点越界（`slots.end > count`）→ `None` → `delta = 0`；此时插入仍会把可见内容整块推下，锚点跳动方向与上轮报告的 −4 行相反。

**同时确认的两条**：

1. 没有第二份**索引**（`prepend_visual_range` 只用当帧索引换算），但存在**两套槽位坐标系**——这才是问题；本轮新增的跨模块共享可变状态正是这对 atom（`HISTORY_PREPEND_MARK` 为本轮新增，`atoms.rs:523`；`HISTORY_PREPEND_EPOCH` 上轮已有），清空点挂在会话边界（`history_paging.rs:487`）是正确做法。
2. 未受影响的对照：D4 的 `HistorySpans::slots()`（定义 `history_paging.rs:235`，调用点 `history_page.rs:213` 的 `apply_pending_tool_ends`）在**同一层**（`state.committed`）内使用槽位（`state.committed.get(*index)`），不受本问题影响。

### 5.2 判据 6 = PASS：D7 登记

1. **编号**：全目录原最大 `P0-76` → 新增 `P0-77`，与 README :9「用例编号是全目录 P0 执行顺序」一致；`grep -rn "P0-77" docs/verification/tui-integration-cases/` 只有两处（README 索引 + 用例本体），无重号。
2. **章节位置**：`06-transcript-ui.md` 用有序列表现有 4 条（P0-28～P0-31），新增为第 5 项——符合 :9「区域文件内也使用有序列表」。
3. **链接**：`[issue](../../../spec/issues/2026-10-13-transcript-shrink-history-pagination.md)` 路径可达（文件存在，`状态：实现中` = active issue），符合 README :51「新增场景在可用时链接 active issue」。
4. **索引更新**：README「P0 执行顺序」新增第 20 项（`README.md:32`）。
5. **内容自洽**：用例第 3 步同时覆盖「阅读位置不跳（含缺口在视口下方填补）」「迟到页不出现/不改写游标」，与本轮两条修复点对得上。
6. `e2e/` 侧仍无翻页自动化用例（本轮未承诺自动化）。

### 5.3 code-index 一致性（判据 6 的后半）

1. `docs/code-index/peri-tui.md` 的分页行符号 `HISTORY_PREPEND_MARK` / `SlotIndex::slots_visual_range` / `prepend_scroll_delta` 全部存在（`atoms.rs:523`、`transcript_index.rs:282`、`history_paging.rs:352`），表头更新行记到了 2026-10-14 并写明「P0-77 登记」。
2. **但该行同时写进了「渲染 body 用当帧同一份视觉索引换算成视觉行区间」这一因果**——这句话在**坐标系层级**是判据 5 的反例（同一份索引不等于同一套槽位坐标）。文字与代码现状吻合，属「忠实记录了一个错误前提」，建议随修复一起改写。

### 5.4 判据 8 = FAIL：A20 的最小复现与量化偏差

1. **最小复现**（探针源码留档 `.recon/S7d-verify-probe.rs.txt`）：
   1. 建桥接状态：`committed = [继承前缀(user 气泡), 自有条目(s1:own, 一条 user 气泡)]`，登记浏览身份（`s1` / `tok`）；
   2. 构造一页响应：`s1:old-1a [tool_call t1]`、`s1:old-1b [tool_call_update t1]`、`s1:old-2a [tool_call t2]`、`s1:old-2b [tool_call_update t2]`（`position` 10/11/20/21，均早于 100）；
   3. `dispatch_and_notify(HistoryPageLoaded)` → 生产并入 + 生产发布；
   4. 读 `HISTORY_PREPEND_MARK` 与 `VIEW_MODELS.items`。
2. **量化偏差**：包络 `(1,3)`（桥接坐标）对渲染索引 `[bubble, GROUP(2), bubble]` 命中 `(1, total_visual)`：
   - 本探针窗口（1+3+2 = 6 行）→ `delta = 5`，正确值 3 → **过补偿 2 行 = 折叠组之后那一条自有条目的高度**；
   - 一般化：过补偿 ≥ 插入块之后第一个已载入条目的视觉高；当包络终点**恰好等于**渲染槽位数时终点取整窗总行数（`transcript_index.rs:290-292`；若终点 `>` 槽位数则整段判 `None`，见 `:286-288`）→ 过补偿 = 插入块之后的**全部**可见内容，`set_offset(scroll_y+delta)` 直接落到窗口底部（渲染侧 clamp 到 `max_scroll`，`scroll.rs:80-83`）。
3. **触发条件（不是罕见形状）**：只要**本次并入的页内含 ≥2 张相邻的成功通用工具卡**（`is_mergeable`：非 running、非 error、无 diff、`Generic`、未被用户改过、非焦点），或窗口里包络**之前**已有被折叠的相邻工具卡，坐标即错位。agent 历史里连续工具调用（Read/Grep…）是常态；本探针没有构造任何极端条件。
4. **结论**：A20 在「无折叠组错位」的窗口下成立（判据 1/2），在含折叠组的窗口下**不成立且方向更糟**（旧规则只在下沿缺口时多补插入高度，新规则在这类窗口里把视口推向底部）。**不能按 PASS 结项。**
5. **可选的闭合方式（供实现者参考，不构成本轮判定）**：让桥接侧写「渲染槽位」而不是 committed 槽位（在 `push_view_models` 里把 `inserted_slot_range` 按分组结果重映射后再写 atom），或让包络带上发布代数 + 由发布侧给出换算系数；或退回 `prev_total_visual/total_visual` 口径再单独修缺口形态。

## 6. 额外发现（超出判据、但会阻断用例）

### 6.1 `build_entries` 同条起止配对 panic（潜在，实测稳定复现）

1. 复现：一页里**同一条目**含 `[tool_call t1, tool_call_update t1]` → `dispatch_and_notify(HistoryPageLoaded)`。
2. 输出：
   ```
   panicked at peri-tui/src/kit/acp_events/history_page.rs:192:16:
   index out of bounds: the len is 0 but the index is 0
   ```
3. 根因（读代码）：`history_page.rs:141` 记录的是 `tool_slots.insert(tool_id, (entries.len(), units.len()))`——`entries.len()` 是该条目**将要**占的下标，但条目要等本轮循环结束才 `entries.push`（`:172`）；同条目的终点在 push 之前到达，`complete_in_place`（`:182-195`，越界点在 `:192` 的 `entries[entry_index]`）就会去索引 `entries[k]`，而此时 `entries` 只有 k 个元素。
4. 影响面：**当前服务端投影形状不会触发**（一个 payload 只发射起点或终点，§5.1 探针 1 用的正是这种形状），所以不是线上 panic；但 `:103` 的函数文档正宣称「起点先到就原地升级成终态」，一旦服务端把同一 payload 的两个阶段合并、或未来加字段，就会 panic 掉整个桥接任务。建议要么让配对在 push 之后结算，要么对同条目情形早退。
5. 说明：该缺陷不是本轮（S7d）引入的，上轮（S7c）也未覆盖这一形状——既有用例一律把起点/终点放在**不同条目**（`history_page_test.rs:363/412/423/613`）。

## 7. 未验证项与局限（如实列出）

1. **无渲染帧/整组件树**：判据 1/2/4 都在状态/决策层完成（真实合并 + 真实索引 + 生产函数，但没有 `ratatui` 布局帧、没有 `handle_event` 闭包），与 S7c §13.1 同口径。
2. **帧序竞态未构造复现**（实现者 §4.2 自述）：`handle_history_page_loaded` 并入后立即写 MARK（`:78`）再推 EPOCH（`:79`），随后调 `push_acp_state`（`:89`）；渲染侧的 `push_view_models`（`render.rs:26`）不在该函数里，而由桥接发布屏障在合帧时触发（`publish_barrier`，`mod.rs:195`；生产入口 `acp_bridge.rs:608` 的 `dispatch_for_bridge` 返回 `Deferred` 后交 scheduler）。静态推演：同一帧内 `prepend_visual_range`（`mod.rs:391`）与 effect deps 同源，读到新代数的帧必然用当帧索引（该帧索引已含并入内容）或该帧 deps 未变而不执行 effect；但若 `is_loading` / `vis_height` 等无关 deps 恰在窗口内变化的帧里执行 effect，仍可能用旧索引算新包络——未实测。
3. **P0-77 仍是手工用例**：`e2e/` 与集成层没有翻页自动化，用户可观察的最终确认（真实终端里滚到顶端不跳）未做。
4. **`steer_consumer` flake**：只观察到「首跑失败、复跑通过」，未做多次重复统计（判定为既有共享 atom 干扰，与本轮无关）。
5. **S7c→S7d 增量diff不可得**：工作区未提交、关键文件未跟踪，§4.3.4 的断言审计是整分支口径；被删用例原文不可恢复。
6. **未跑 `lefthook run pre-commit`**（fmt 门在本分支整体不干净，§4.3.5 只做了逐文件核对）。

## 8. 复核者对工作区的改动（全部已还原，无残留）

1. **新增留档（仅 `.recon/`，不参与构建）**：`S7d-verification.md`（本报告）、`S7d-verify-probe.rs.txt`（探针源码）、`S7d-verify-probe-output.txt`（探针输出）、`S7d-verify-falsify-scenario-b.txt`（证伪对照）。
2. **临时探针（已删净）**：`peri-tui/src/kit/acp_events_test/zz_verify_s7d_test.rs`（删）、`acp_events_test.rs` 的 `mod zz_verify_s7d_test;` 声明（已还原，该文件 diff 仍只有本轮的 `history_page_test` 模块声明 2 行）、`scroll_test.rs` 末尾 27 行探针（已裁回 950 行，`grep zz_verify` = 0）。
3. **临时退化（已还原并字节校验）**：`history_paging.rs::prepend_scroll_delta` 判定分支（`cp` 恢复后 `diff -q` = 一致，`grep -c 临时证伪` = 0）。
4. 未做任何 git 写操作；未改服务端 / SDK / 依赖 / DDL。

## 9. 结论

1. **本轮修好了上轮报告的那个失败形态（判据 1/2/3 PASS），但换进来一个更常见、更严重的失败形态**：桥接槽位坐标与渲染槽位坐标在「工具卡折叠组」存在时错位，导致 A20 在真实形状下失效（判据 5 FAIL、判据 8 FAIL）。
2. **D7 登记合格（判据 6 PASS）**，回归与编译干净（判据 7 PASS），断言未被放宽。
3. **给出可执行的下一步**：先修 §5.1 的坐标口径（或把包络改写成渲染槽位、或退回 `total_visual` 增量并单独处理缺口形态），再补一条「页内含相邻成功工具卡」的 A20 用例（现有 4 条结构上覆盖不到该形状）；同时修 §6.1 的同条配对索引越界（潜在 panic）。
