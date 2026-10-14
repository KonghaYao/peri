# S7c TUI 复核报告（独立复核者）

- 复核工作区：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`（未提交）
- 复核对象：本轮 TUI 修复切片。**实现者未留下 `.recon/S7c-tui-fix.md`**，本轮全部判定来自工作区代码、`git diff` 与实测，未采信任何实现者叙述。
- 上轮报告：`.recon/S7-tui-verification.md`（问题项 D1–D8，本报告逐项回应）。
- 复核日期：2026-10-14。

## 1. 结论摘要

1. **交付切片无法编译**（阻断项，复核者已打最小修复）。D8 的 `THREAD_LOAD_TX.set(...).expect(...)` 需要 `ThreadLoadDispatcher: Debug`，`peri-tui` 的 `test` 与 `check --all-targets` 都编译不过。复核者改这一处后可编译，此后全部命令才跑通。**由此可判定：实现者留下的证据文件不来自当前工作区状态**（§11.3）。
2. **REQ-C01「滚动触发翻页」现在成立**（实测 + 证伪）。滚动事件路径已接线；50 次高频滚动（滚动路径与渲染路径交替）实测恰好派发 1 次；把判据退化回修复前的「只看当帧事实」后，同样是这 50 次滚动实测派发 **0** 次（旧路径不触发）。
3. **A20 锚点 PARTIAL**：插入点落在视口上方（翻页常见形态）时同一可见单元屏幕行不变（实测 0 行变化）；插入点落在视口下方（中间缺口填补）时视口被整体推动插入高度（实测 −4 行）。局限已写入生产代码文档，含实装代价与移除条件，不再是「无代价的 PARTIAL」。

## 2. 判定表

| 项 | 上轮判定 | 本轮判定 | 判据类型 |
| ----------------------------------------------------- | ------------- | ------------------------------ | ------------------------------------------ |
| D1 滚动触发接线（REQ-C01-1/2） | FAIL（P1，静态） | **PASS** | 实测（状态/决策层）+ 证伪；未过整组件树 |
| D2 同会话 rewind 后迟到页丢弃 | FAIL（P2，静态） | **PASS** | 实测（事件层）+ 证伪 |
| D3 首屏自有 reminder 区间登记 | FAIL（P3，静态） | **PASS** | 实测（事件层）+ 证伪 |
| D4 未配对工具终点错配实时卡片 | FAIL（P3，静态） | **PASS** | 实测（事件层）+ 证伪 |
| D5 页 RPC 无超时 | FAIL（P3，静态） | **PASS** | 实测（虚拟时钟 `start_paused`） |
| D6 锚点用行数增量近似（REQ-C01-3） | FAIL（P3，机制） | **PARTIAL（已量化 + 已计价）** | 实测（真实 `SlotIndex` 视觉索引） |
| D7 注释与 code-index 同步 | FAIL（P3，文档） | **PARTIAL** | 静态（code-index 已同步；集成用例未登记） |
| D8「无 reload」断言真空 | FAIL（P3，测试强度） | **PASS**（在复核者编译修复之后） | 静态（改动可编译后断言指向自有通道） |
| 回归（全量 lib 测试） | 1798 passed / 6 ignored | **PASS**（1807 / 0 failed / 6 ignored） | 实测 |
| 判据 7：非翻页/未协商连接不受影响 | 未列 | **PASS** | 静态 + 实测（未协商不派发、通道为空） |

## 3. D1：滚动触发翻页（REQ-C01-1/2）

### 3.1 静态：触发口已从渲染路径扩到事件路径

1. `peri-tui/src/kit/message_area/scroll/event.rs:37` 新增纯决策层 `request_older_page_at_scroll_facts`：当帧事实（`follow_bottom`+`offset`）与「节流窗口内 pending 落地后的事实」（`project_viewport_after_pending`）**取并**，再交 `history_paging::request_older_page_if_due(true)` 认领。
2. 新增 `request_older_page_after_scroll`（:73，从 `State` 读事实），`git diff` 显示 6 个新调用点：滚动条 ▲ 端点（:180）、轨道点击（:223）、滑块拖拽（:266）、滚轮 ScrollDown（:485）、ScrollUp（:500）、键盘 Ctrl+Up/Home（:546）。上轮判 FAIL 的依据（事件处理里完全没有触发点）已被消除。
3. 去重仍是单点：读—判—写同锁（`in_flight` 非空 → 不再派发；`REQUEST_BACKOFF_MS = 250`，`history_paging.rs:42/564`）。
4. 渲染路径**没有**变成逐帧触发源：`use_effect` 的 deps 未新增滚动量，`flush_scroll_if_due`（render body）不调用触发口；`message_area/mod.rs` 的注释明确写了「不在 render body 触发」（TUI-RENDER-001）。

### 3.2 实测：单次认领 + 游标推进（复核者新增用例）

用例 `zz_verify_d1_scroll_trigger_claims_once_and_then_advances`（全文留档 `.recon/S7c-verifier-added-tests.rs.txt`，已从工作区删除）三段：

1. **未协商**（无 `snapshotToken`/`nextCursor`）：贴边也不派发，通道为空（同时是判据 7 的实测）。
2. **已协商**：50 个 tick，每 tick 交替调用滚动路径（`request_older_page_at_scroll_facts`，offset 停在边界外、pending 尚未落地）与渲染路径（`request_older_page_at_boundary`）：
   - `dispatched == 1`；通道里恰好一条请求，字段 `s1 / tok / c1 / PAGE_LIMIT`；`in_flight` 已认领；再无第二条。
3. **结算后推进**：`settle_request(Ok(Some("c2")))` 后立即重试为 no-op（250 ms 退避），sleep 300 ms 后下一次取页游标为 `c2`。

命令与结果：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- zz_verify_d1` → `1 passed; 0 failed; 1813 filtered out`。

### 3.3 证伪：旧路径（只看当帧事实）确实不触发

把 `request_older_page_at_scroll_facts` 的判据临时退化回只有当帧事实（删掉 `|| project_viewport_after_pending(...)` 分支），重跑：

```
test kit::message_area::scroll::tests::test_scroll_lands_at_top_dispatches_older_page_request ... FAILED
   panicked at scroll_test.rs:711: pending 落地后贴向前边界 → 派发取页
test kit::message_area::scroll::tests::zz_verify_d1_scroll_trigger_claims_once_and_then_advances ... FAILED
   assertion `left == right` failed: 同一页只允许被认领一次（并发合并）  left: 0  right: 1
test result: FAILED. 0 passed; 2 failed; 1813 filtered out
```

即：**同一条滚动在旧判据下 50 个 tick 派发 0 次，在当前判据下恰好 1 次**。恢复后两用例均 `ok`（`2 passed`）。

### 3.4 实现者的 D1 用例本身可证伪

`peri-tui/src/kit/message_area/scroll_test.rs:690` 的 `test_scroll_lands_at_top_dispatches_older_page_request` 在 §3.3 的退化下同样 FAIL，说明它不是恒真断言；因此判据 1 中「实现者是否缺少可失败用例」的条件不成立，复核者未新增常驻用例（新增的两个用例仅作测量，已删除，全文留档）。

## 4. D2：同会话 rewind 后迟到页丢弃

1. 修复点：`peri-tui/src/kit/acp_events/system.rs::handle_rewind_completed` 在重建 `committed` 前调用 `history_paging::reset_browsing_state()`。
2. 用例：`acp_events_test/history_page_test.rs:517` `test_history_page_is_dropped_after_same_session_rewind`，同时断言 `HISTORY_PREPEND_EPOCH` 未变、`next_cursor`/`snapshot_token`/`in_flight` 均为 `None`。
3. **证伪实测**：删掉那一行 `reset_browsing_state()` 后重跑 → FAIL：`left: ["late page", "kept"]  right: ["kept"]`（迟到的页被并入已重建的窗口，其中 `s1:old` 合并进同一条目）。恢复后 green。→ 修复载荷成立，用例可失败。

## 5. D3：首屏自有 reminder 条目登记区间

1. 修复点：`acp_events/mod.rs` 的槽位登记 `matches!` 新增 `SystemReminder | SystemReminderFallback`；`acp_notifier.rs` 对 unstable_event 路径在内容之前注入 `ReplayEntryMarked`；`history_page::handle_history_page_loaded` 在并入前收口区间。
2. 用例：`acp_notifier_test.rs:339` `test_unstable_event_reminder_entry_marks_precede_content`；`history_page_test.rs:466` `test_history_page_keeps_prefix_before_reminder_only_own_entry`。
3. **证伪实测**：从 `matches!` 中移除 `SystemReminder | SystemReminderFallback` → 后者 FAIL（「首屏自有 reminder 条目必须登记区间」）。恢复后 green。

## 6. D4：未配对工具终点错配实时卡片

1. 修复点：`HistorySpans::slots()` 新增，`acp_events/history_page.rs::apply_pending_tool_ends` 只扫描历史来源槽位（不再扫整窗）。
2. 用例：`history_page_test.rs:575` `test_pending_history_tool_end_never_completes_live_card`；新增 `clear_pending_tool_ends_for_tests()`（上限 256 未变）。
3. **证伪实测**：把 `let history_slots = spans.slots();` 换回整窗 `vec![(0, state.committed.len())]` → FAIL（「实时卡片不得带上历史输出」）。恢复后 green。

## 7. D5：页 RPC 超时

1. 修复点：`history_page_consumer.rs:83` `PAGE_FETCH_TIMEOUT = 10s` 包住 `fetch_page` 的 `load_history_page`；超时按普通失败结算（`invalidates:false`），释放 `in_flight`、保留游标与 `last_error`。
2. 用例：`history_page_consumer_test.rs:213` `test_fetch_page_times_out_on_silent_server`、`:237` `test_handle_request_releases_in_flight_on_timeout`（`#[tokio::test(start_paused = true)]`，外层 `PAGE_FETCH_TIMEOUT + 2s` 兜底）。
3. 局限：虚拟时钟下验证，未对真实静默 server 端到端跑。

## 8. D6：阅读锚点（REQ-C01-3 / A20）

### 8.1 机制现状

`history_paging::prepend_scroll_delta(follow_bottom, epoch_changed, prev_total_visual, total_visual)`（`auto_follow.rs:247-269`）：只在浏览态且 `HISTORY_PREPEND_EPOCH` 变化时，按**总行数增量**补偿 `offset`。文档注释带「已知局限」：插入点不在视口上方时会过补偿，并写明实装锚点重定位的字面计划（锚点身份 = entry_id + 子项序号 + 单元内视觉行偏移，并合后重查）、代价估计与移除条件。

### 8.2 实测：同一可见单元的屏幕行（复核者新增用例）

用例 `zz_verify_d6_anchor_screen_row_before_and_after_prepend` 用真实 `SlotIndex` + `slot_visual_start` + 生产函数 `prepend_scroll_delta`，屏幕行 = 视觉起始行 − 补偿后 offset：

1. **场景 A（插入点在视口上方，翻页常见形态）**：`[继承(1) 自有(5)]` → 插入 4 行 → `[继承(1) 更早页(4) 自有(5)]`；`delta = 4`，自有条目屏幕行 `1 → 5−4 = 1`，**不变 → A20 成立**。
2. **场景 B（插入点在视口下方 = 中间缺口填补）**：视口只看得到 A（`vis_height 4`、`offset 0`）；缺口 4 行插到 A 之后 → `delta = 4`、offset 变 4，A 的屏幕行 `0 → −4`，**可见内容被整体推走 → A20 不成立**。

命令与结果：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- zz_verify` → `2 passed; 0 failed; 1813 filtered out`。

### 8.3 判定

**PARTIAL（已量化 + 已计价）**：主路径（向上翻页，插入点在视口上方）成立；缺口填补路径有确定的、可复现的偏差（= 插入块高度），且该偏差与实装代价、移除条件一起写在生产代码注释里，不是口头兜底。**不采纳「仍是 PARTIAL 但无代价说明」的形态**——本轮已有代价说明。

## 9. D7：文档与维护上下文

1. **已同步**：`docs/code-index/peri-tui.md` 新增「改历史分页（滚动加载更早历史）」行，并重写 compact 行（REQ-C02）；表头日期与说明更新为 2026-10-13。新行引用的 16 个符号（`HISTORY_PAGING`、`HISTORY_SPANS`、`viewport_at_forward_boundary`、`request_older_page_if_due`、`request_older_page_at_scroll_facts`/`request_older_page_after_scroll`、`spawn_history_page_consumer`、`fetch_page`、`settle_request`、`invalidate_browsing`、`prepend_scroll_delta`、`HISTORY_PAGES_TX`、`LOCAL_EVENT_TX`、`PAGE_FETCH_TIMEOUT`、`MAX_AUTO_CONTINUE_PAGES`、`HISTORY_PREPEND_EPOCH`）逐一 grep 确认存在；行内声明的文件路径全部存在。旧注释同步：`acp_events/mod.rs`、`acp_bridge.rs`、`atoms.rs`（`PENDING_COMPACT_NOTE` 语义）、`thread_load_consumer.rs`。
2. **未完成（约定）**：`docs/verification/tui-integration-cases/` 在本轮**零改动**（`git status` 为空）。该目录 README 的约定（:9「用例编号是全目录 P0 执行顺序」、:51「新增场景在可用时链接 active issue」）要求新场景登记；当前最大编号 `P0-76`，新增应为 `P0-77`（`06-transcript-ui.md` 或 `11-transcript-display.md`）。「滚动加载更早历史 / rewind 丢弃迟到页」这条最需要人工回归的场景没有对应用例。
3. `e2e/` 侧同样没有翻页相关用例（按 `historyPage`/`cursor`/`更早历史` 等关键词 grep 无结果），因此 REQ-C01 没有端到端覆盖。

## 10. D8：测试强度（含编译修复）

1. 修复点：`thread_load_consumer.rs` 删除 `#[cfg(test)] ThreadLoadRequest::thread_id()` 访问器；用例改为断言 `rx_a.try_recv().is_err()` + 窗口/提示断言；`session_events_test.rs` 改名为 `test_compact_turndone_keeps_window_without_reload`，语义按 REQ-C02 重写。
2. **交付切片不能编译**：原写法 `THREAD_LOAD_TX.set(...).expect("...")` 对 `Result<(), ThreadLoadDispatcher>` 调用，而 `ThreadLoadDispatcher`（`thread_load_consumer.rs:41`）无 `Debug`。为给出可核对的原文，复核者把该处**临时还原**为交付原形后重跑，实测输出：

```
error[E0277]: `thread_load_consumer::ThreadLoadDispatcher` doesn't implement `Debug`
    --> peri-tui/src/kit/acp_events/../acp_events_test/session_events_test.rs:792:10
     |
 792 |         .expect("THREAD_LOAD_TX 装配失败");
     |          ^^^^^^ unsatisfied trait bound
help: the trait `Debug` is not implemented for `thread_load_consumer::ThreadLoadDispatcher`
    --> peri-tui/src/kit/thread_load_consumer.rs:41:1
error: could not compile `peri-tui` (lib test) due to 1 previous error
```

还原后重跑 `diff -q` 确认与已验证版本字节一致，再恢复最小修复（`test` 与 `check --all-targets` 在此错误下都跑不起来）。复核者最小修复（保留原意：装配失败必须显式失败，否则两条「通道为空」断言退化为真空）：

```rust
if THREAD_LOAD_TX.set(crate::kit::thread_load_consumer::ThreadLoadDispatcher::new(tx_a, client)).is_err() {
    panic!("THREAD_LOAD_TX 已被占用：本用例的「不触发 reload」断言会变成真空");
}
```

3. 修复后 `check --all-targets` 通过（见 §11.2）。**这是复核者对工作区唯一保留的改动**（§12）。

## 11. 回归（判据 6）

### 11.1 全量 lib 测试

1. 最终状态（复核者新增用例已删除后）：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib`
   → `test result: ok. 1807 passed; 0 failed; 6 ignored; 0 measured; 0 filtered out; finished in 24.23s`（退出码 0）。
2. 含复核者 2 个新增用例时：`1809 passed; 0 failed; 6 ignored`。
3. 上轮基线：`1798 passed; 6 ignored`（`.recon/S7-tui-verification.md`），本轮净增 9 个用例。

### 11.2 编译检查

`./scripts/cargo-rmcp-patched.sh check --locked -p peri-tui --all-targets` → `Finished dev profile [unoptimized + debuginfo] target(s) in 34.53s`，无 error / warning。**注意：这是在复核者修掉 §10.2 的编译错误之后；交付原样运行该命令会停在 `E0277`。**

### 11.3 与实现者证据文件不一致（重要）

`.recon/S7c-raw-tui-pass-tail.txt` 记录 `1801 passed; 0 failed; 6 ignored`（04:02:53，`TUI TEST EXIT=0`），而当前工作区实测 1807 passed；且该文件记录成功的一轮显然不是在当前文件内容下运行的——当前内容编译不过。同目录另一份 `.recon/S7c-raw-tui-first-attempt-stderr.log` 是更早的中间态（7 个编译错误：E0364 `project_viewport_after_pending` 私有重导出、E0061 参数个数不匹配），同样不对应交付状态。结论：**实现者的 TUI 证据文件与本轮交付状态不对应（过期或来自其他中间状态）**，不能作为本轮证据；本轮数值以 §11.1/§11.2 为准。

### 11.4 既有断言删除/弱化审计（逐行读 `git diff`）

1. `peri-tui` 侧删除行合计 149 行；其中含 `assert` 的删除行 **1 行**：
   `-    assert!(!state.compact_just_completed, "场景 A: flag 应清除");`
   该行按 REQ-C02 重写为「TurnDone 只清标志与桥接 note、不触发 replay」的更强制断言组（含 `rx_a.try_recv().is_err()`），非单纯删除。另一处命中关键词的删除行 `restore_error.lock().unwrap() = ...` 是 `match` 重构的**位移**（新版本中同一行存在于 `Err(error) =>` 分支），非删逻辑。
2. 新增 `#[ignore]`：0；删除 `#[ignore]`：0；新增 `should_panic`：0；`test.failing`：0。
3. 无测试文件被删除（`git status` 无 ` D` 项）。
4. 无法与 `main` 基线做完整对照（工作区禁止 checkout/写 git），以上为工作区内 `git diff HEAD` 审计。

## 12. 复核者对工作区的改动（全部为复核所需，需实现者确认）

1. **保留**：`peri-tui/src/kit/acp_events_test/session_events_test.rs`（`.expect(...)` → `if ...is_err() { panic!(...) }`）。理由：不修则整个 peri-tui 无法编译，任何验证都无法进行；语义与实现者原意一致。
2. **已回退**（三处证伪 + 一处 D1 证伪，全部验证过恢复后 green）：
   - `acp_events/system.rs`（删 `reset_browsing_state()` 调用）→ 已恢复；`history_page_test` 重跑 10 passed。
   - `acp_events/mod.rs`（`matches!` 去掉 reminder 两个分支）→ 已恢复。
   - `acp_events/history_page.rs`（整窗 slots）→ 已恢复。
   - `scroll/event.rs`（D1 判据退化）→ 已恢复；`git diff` 中无残留（grep「临时证伪」= 0），两用例重跑 `2 passed`。
3. **已删除**：`peri-tui/src/kit/message_area/scroll_test.rs` 末尾 145 行复核者新增用例（文件恢复到 751 行；`grep zz_verify|S7c 复核者` = 0）。全文留档 `.recon/S7c-verifier-added-tests.rs.txt`，运行输出见 §3.2/§3.3/§8.2。

## 13. 未验证项与局限（如实列出）

1. **D1 未过整组件树**：实测在状态/决策层（`request_older_page_at_scroll_facts` + `history_paging` 状态机）完成，没有驱动真实 `handle_event` 闭包、也没有渲染一帧；「50 tick → 1 请求」是直接调用两条触发口的等价模拟。真正的 scroll → fetch 端到端（含节流窗口、`apply_scroll` 落地）没有自动化覆盖。
2. **REQ-C01 无端到端用例**：`e2e/` 与 `docs/verification/tui-integration-cases/` 都没有翻页场景（也正是 D7 未完成的部分）。
3. **A20 只用视觉索引算术验证**：场景 A 的「屏幕行不变」由 `SlotIndex` + `prepend_scroll_delta` 计算得出，没有渲染帧对照；场景 B 的 −4 行同样。
4. **D5 超时在虚拟时钟下验证**：未对真实「不响应 `session/load`」的服务端做端到端测量；`transport 无内建超时` 的结论来自代码阅读。
5. **D2 的窗口窄性仍是估计**：rewind 恰好发生在页 RPC 在途窗口内的概率/影响面未实测（服务端时序相关）。
6. **性能**：`HistorySpans::contains` 线性扫描在长历史下的成本未压测（上轮遗留项，本轮未新增）。
7. **无 main 基线对照**：禁止 checkout，无法跑基线分支的同一批命令。

## 14. 两个明确结论

1. **REQ-C01「滚动触发翻页」：成立（PASS）**。事件路径（滚轮/滚动条端点/轨道/拖拽/键盘 Ctrl+Up/Home）已接线到 `request_older_page_if_due`，判据含「节流窗口内 pending 落地后」的事实；实测 50 次高频滚动只认领 1 次，退化回修复前判据时 0 次认领（旧路径不触发已被测量确认）；渲染路径未引入逐帧触发源。限制：见 §13.1/§13.2（无整组件与端到端覆盖）。
2. **A20 锚点：PARTIAL（已量化）**。插入点在**视口上方**（正常向上翻页）：同一可见单元屏幕行不变，A20 成立；插入点在**视口下方**（中间缺口填补）：可见内容被推动 −4 行（= 插入块高度），A20 不成立。该失败场景、实装代价与移除条件已写入生产代码文档（`prepend_scroll_delta`），因此本项按「部分满足 + 已计价」而非「未处理的残余」结项；后续若要字面满足 REQ-C01-3，需按文档中的锚点身份方案实装并补两组用例。
