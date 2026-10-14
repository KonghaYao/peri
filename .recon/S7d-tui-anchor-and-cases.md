# S7d TUI 锚点补偿 + 集成用例登记（实现者报告）

- 工作区：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`（保持未提交，无 git 写操作）
- 输入：`.recon/S7c-tui-verification.md`（独立复核，权威记录）——D1–D5、D8 已 PASS（未动）；D6 锚点 PARTIAL（场景 B 实测 −4 行）；D7 集成用例未登记。
- 状态：见 §7 进度。

## 1. 改动清单（绝对路径）

1. `peri-tui/src/kit/history_paging.rs`
   - `MergeOutcome::first_insert_slot: Option<usize>` → `inserted_slot_range: Option<(usize, usize)>`（插入条目槽位**包络** `[first, end)`，`merge_page` 内按 min 起点 / max 终点累计）。
   - `prepend_scroll_delta` 换口径（新签名：`(follow_bottom, epoch_changed, scroll_y, vis_height, inserted_visual_range)`），文档重写为最终语义 + 理由 + 仍未覆盖的边界 + 移除条件。
   - `reset_browsing_state()` 追加 `atoms::HISTORY_PREPEND_MARK.set(None)`（会话边界作废并入信号）。
2. `peri-tui/src/kit/atoms.rs`：新增 `HISTORY_PREPEND_MARK: AtomStatic<Option<(usize, usize)>>`；`HISTORY_PREPEND_EPOCH` 文档改成本轮口径（不再提「总行数增量」）。
3. `peri-tui/src/kit/acp_events/history_page.rs`：并入成功后**先写包络、再推代数**；日志字段 `first_insert_slot` → `inserted_slot_range`。
4. `peri-tui/src/kit/message_area/transcript_index.rs`：`SlotIndex::slots_visual_range(Range<usize>) -> Option<(usize, usize)>`（复用既有 `slot_visual_start` 与 `total_visual`，不新建索引；终点取包络后第一个槽位的视觉起始，包络贴窗口末尾时用整窗视觉总行数收口；空 / 越界 / 零高 → `None`）。
5. `peri-tui/src/kit/message_area/scroll/auto_follow.rs`：新增派生函数 `prepend_visual_range(&SlotIndex) -> Option<(usize, usize)>`（读 `HISTORY_PREPEND_MARK`，按当帧索引换算）；补偿调用点改传 `scroll_y` / `ctx.vis_height` / 该派生值。
6. `peri-tui/src/kit/message_area/scroll.rs`：`AutoFollowCtx` 新增字段 `prepend_visual_range`；`pub(super) use` 重导出 `prepend_visual_range` 供 `mod.rs` 调用。
7. `peri-tui/src/kit/message_area/mod.rs`：渲染 body 每帧由当帧 `slot_index` 调 `scroll::prepend_visual_range` 派生区间并传入 ctx。
8. `peri-tui/src/kit/message_area/scroll_test.rs`：新增 4 条 A20 用例（真实 `merge_page` + 真实 `SlotIndex`）。
9. `peri-tui/src/kit/history_paging_test.rs`：3 处 `first_insert_slot` 断言改包络；删除旧的纯算术 `prepend_scroll_delta` 用例（覆盖移到 8.）；模块文档注明 A20 用例位置。
10. `docs/code-index/peri-tui.md`：分页行符号与描述更新（`HISTORY_PREPEND_MARK`、`SlotIndex::slots_visual_range`、锚点新口径）；表头更新行加 2026-10-14 段。
11. `docs/verification/tui-integration-cases/06-transcript-ui.md`：新增 **P0-77**（链接 active issue）。
12. `docs/verification/tui-integration-cases/README.md`：P0 执行顺序新增第 20 项（P0-77）。

## 2. 补偿规则的最终语义

`history_paging::prepend_scroll_delta(follow_bottom, epoch_changed, scroll_y, vis_height, inserted_visual_range)`

1. **输入事实**：`inserted_visual_range` = 插入块在**并入后布局**里的视觉行区间 `[block_start, block_end)`，由「桥接侧槽位包络（`HISTORY_PREPEND_MARK`）+ 当帧同一份视觉索引（`SlotIndex::slots_visual_range`）」算出；`scroll_y` / `vis_height` / `follow_bottom` 取自当帧生产视口事实。**没有第二份索引**。
2. **判定**（`block_start < scroll_y + vis_height` 时补偿 `block_end - block_start`，否则 0）：
   - 插入块**完全在视口上方** → 补偿 = 插入块高度（视口内每一行被整块下推，补同高即原位不动）；
   - 插入块**整体在视口下沿之下** → 补偿 = 0（视口里没有任何内容被推动，动 offset 反而把画面推走）；
   - **跨视口**（块起点在视口内部）→ 同样补偿整块高度。理由：插入点上方与下方的可见内容不可能同时保位；固定「插入点下方」那部分，因为翻页紧跟「视口贴向前边界」发生，视口下沿方向是用户已载入、正在读的自有内容，新并入的更早历史紧贴其上方——固定它符合「往上补历史、阅读位置不回跳」；补 0 就是 A20 要禁止的「读到一半的内容跳走」。
   - **跟随底部**（`follow_bottom = true`）→ 0，既有语义不变（用户看的是尾部，插入在其上方，由吸底跟随处理）。
   - **非翻页帧**（`epoch_changed = false`）→ 0，既有语义不变。
   - **区间取不到**（`None`：越界槽位 / 空包络 / 会话边界清空）→ 0（位置未知不动视口）。

## 3. 测试命令与结果（原文）

全部在 worktree 根目录执行，`./scripts/cargo-rmcp-patched.sh`。证据文件：`.recon/S7d-raw-tui-final.txt`（三命令 + A20 定向，含 `pipefail` 下的真实退出码）。

1. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- history_paging`
   → `test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 1809 filtered out; finished in 0.00s`，`REAL_EXIT1=0`
2. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib`（全量）
   → `test result: ok. 1810 passed; 0 failed; 6 ignored; 0 measured; 0 filtered out; finished in 25.08s`，`EXIT2=0`
   （基线 S7c = `1807 passed; 6 ignored`；本轮净 +3 条：`scroll_test` 新增 4 条、`history_paging_test` 删 1 条旧的纯算术用例）
3. `./scripts/cargo-rmcp-patched.sh check --locked -p peri-tui --all-targets`
   → `Checking peri-tui v0.2.0 ...` / ``Finished `dev` profile [unoptimized + debuginfo] target(s) in 31.86s``，`REAL_EXIT3=0`，无 error / warning。
4. A20 定向（本轮新增用例）：`... test --locked -p peri-tui --lib -- prepend`
   → `test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 1810 filtered out`，`REAL_EXIT0=0`
   （4 条 `scroll::tests::test_prepend_*` + 2 条既有 `acp_events_test::history_page_test::*`）

**证伪实测**（证明新用例真的在检验新口径）：把 `prepend_scroll_delta` 的判定临时退化为旧「按插入总增量」口径（`if true { block_end - block_start }`），只跑 `-- prepend_anchor`：
- `test_prepend_anchor_holds_when_insert_straddles_viewport_top ... ok`（场景 A 在旧口径下也成立，符合预期——旧口径的问题不在这一侧）
- `test_prepend_anchor_holds_when_insert_lands_below_viewport ... FAILED`
  `panicked at peri-tui/src/kit/message_area/scroll_test.rs:867:5: assertion left == right failed: 插入块整体在视口下沿之下 → 视口不动  left: 4  right: 0`
  → `test result: FAILED. 1 passed; 1 failed; 1814 filtered out`，`error: test failed`
- 之后已恢复正确实现（`grep -c 'if true { block_end' = 0`），恢复后 CMD0 全绿。原文留档 `.recon/S7d-raw-tui-falsify-scenario-b.txt`。

**格式修正后复跑**（本轮只格式化自己新增的代码，见 §5 第 5 条）：`... test --locked -p peri-tui --lib -- prepend` → `6 passed; 0 failed`，`EXIT0B=0`；`... check --locked -p peri-tui --all-targets` → ``Finished `dev` profile … in 35.53s``，`EXIT3B=0`（同留档文件）。

## 4. 已知边界与移除条件

**仍未覆盖的边界**（同一份文字也已写进 `prepend_scroll_delta` 的生产文档注释）：

1. **包络口径的过补偿**：`HISTORY_PREPEND_MARK` 是「插入条目槽位包络」而非精确的连续插入区间。在**当前页契约**（一页整体更早，或整页落在同一个已登记空隙里）下，包络恰好等于插入行集合，故补偿准确；但 `merge_page` 本身按 `position` 逐条插槽，并不禁止页内条目被夹在既有条目两侧——一旦出现交错插入，包络内部夹带的既有行会被计入 → 过补偿（多滚）。要精确需要 `MergeOutcome` 记录逐条目的槽位区间列表。判据：是否出现「页与既有条目交错插入」。
2. **帧序竞态窗口**：`history_page.rs` 先写 `HISTORY_PREPEND_MARK` 再推 `HISTORY_PREPEND_EPOCH`，但并入内容要经 `VIEW_MODELS` 发布才进入渲染帧。若 mark 与 epoch 之间有别的渲染帧执行了 effect，该帧会用**旧索引**算区间并消费掉 epoch（`prepend_visual_range` 对越界槽位返回 `None` → 补偿 0），本页的锚点补偿就丢了（不会误补偿，只会少补一次）。闭合方式：包络带上发布代数，帧内与 `snapshot.generation` 比对后再消费。
3. **锚点身份未参与**：规则只按几何位置（插入块与视口的关系）判定，不识别「视口顶行是哪个条目」。因此插入点落在视口内部（跨视口）时，其上方可见行必然移动——本轮选择固定插入点下方（见 §2 理由），而非真正保位。
4. **`slots_visual_range` 放宽了零行约束**：单槽 `slot_visual_range` 要求 `start < end`（零行槽返回 `None`）。包络版只在整体零高时返回 `None`，包络内的零视觉行槽位（继承前缀、提示行等真实可为 0 高的单元）既不报错也不计入。理由：否则只要包络里有一个零行条目，整个插入块的定位就判为「未知」而不补偿。这是有意的口径差异，不是疏漏。

**移除条件**：实装「锚点身份重定位」（真正按条目身份而非几何补偿）时，`prepend_scroll_delta` 与 `HISTORY_PREPEND_EPOCH` / `HISTORY_PREPEND_MARK` 的整条应用路径一并替换，而不是在其上叠加。

## 5. 未验证项

1. **无真实渲染帧对照**。A20 用例用真实 `merge_page` + 真实 `SlotIndex` + 生产派生函数（`super::prepend_visual_range`）验证「插入后同一屏幕行仍是同一个条目」，但视口事实（`scroll_y` / `vis_height` / `follow_bottom`）是测试直接构造的，未经 ratatui 实际布局帧。滚动 offset 的实际生效路径（`scroll.rs` → 渲染）本轮未端到端验证。
2. **帧序竞态未实测**（§4 第 2 条）。窗口存在但未构造复现；断言只覆盖「同一帧内 mark + epoch + 当帧索引」的一致情形。
3. **e2e / 集成层无翻页用例**。S7c 已确认此现状，本轮只登记 P0-77（用例文档），未实现自动化执行；P0-77 的「合并与锚点表现」仍需人工或后续自动化跑通。
4. 未在真实 TUI 会话里滚到窗口顶端验证「视觉上无跳动」——即用户可观察行为的最终确认。
5. **`cargo fmt --check` 在本工作区整体不通过**（提交前状态）：`rustfmt --edition 2024 --check` 在 `git status` 里 peri-tui 的 34 个 `.rs` 文件上报告 13 个文件有偏差（`acp_bridge_test.rs`、`acp_events/history_page.rs`、`acp_events/mod.rs`、`acp_events_test/history_page_test.rs`、`acp_notifier.rs`、`history_page_consumer_test.rs`、`message_area/scroll/event.rs`、`kit/mod.rs` 等），属**本分支前几轮未提交工作**，不是本轮引入（`git show HEAD:…scroll_test.rs` 经同一命令为 0 偏差，可复核）。本轮**只**把自己新增/改动的代码格式化干净：`history_paging.rs`、`atoms.rs`、`message_area/scroll.rs`、`scroll/auto_follow.rs`、`message_area/mod.rs`、`transcript_index.rs` 逐文件零偏差；`scroll_test.rs` 本轮新增段落已干净（仅剩前几轮的 672/701/719/737）。未做全仓 `cargo fmt`，以免格式化其他轮次的文件、扩大 diff。因此 `lefthook run pre-commit` 的 fmt 门会因前几轮残留而失败。

## 6. 复核者请重点核对

1. **帧序竞态**（最高优先）：`peri-tui/src/kit/acp_events/history_page.rs` 中 mark/epoch 的写入顺序，与 `message_area/mod.rs` 里每帧派生 `prepend_visual_range` 的时机——是否可能某帧拿到 `Some(旧包络)` 配 `epoch_changed=true` 而错补一次。§4 第 2 条已如实标注为未闭合。
2. **包络口径**：`history_paging.rs::merge_page` 累计的 `inserted_slot_range` 是否与 `SlotIndex::slots_visual_range` 期望的输入一致（桥接侧槽位索引 ↔ 渲染帧槽位索引）。依据：`push_view_models` 的后处理（`TuiDivider` / `apply_fold_pass` / todo 摘要 / `group_successful_tools`）不改变槽位数量——请独立复核这一条，它是「跨层索引可混用」的全部理由。
3. **跨视口规则**（场景 A）：插入块起点落在视口内部时补**整块**高度。请核对 §2 的理由是否成立、是否与产品意图（「往上补历史，阅读位置不回跳」）一致；若产品意图是「插入点下方内容不许跳」，则规则正确；若是「视口顶行不许跳」，则应改为「补 `block_end - scroll_y`」之类。
4. **`slots_visual_range` 的零行槽语义**（§4 第 4 条）：确认放宽是有意的、且不掩盖真实错误。
5. 是否真的没有第二份索引：`HISTORY_PREPEND_MARK` 存的是**槽位**区间，视觉换算只用当帧 `SlotIndex`。请确认没有别处再算一遍视觉位置。

## 7. 进度

1. [x] 读全事实：旧口径（总行数增量）、`HISTORY_PREPEND_EPOCH` 唯一写入点、`merge_page` 的插入槽位、`SlotIndex` 视觉查询、视口事实来源（`AutoFollowCtx`）。
2. [x] 实现：包络信号（`HISTORY_PREPEND_MARK`）+ 视觉区间换算（`SlotIndex::slots_visual_range`）+ 规则函数 + 调用点。
3. [x] 测试：场景 A / 场景 B / 完全在上方 / 跟随态 + 非翻页帧 + 区间未知（4 条，真实索引 + 真实合并）。
4. [x] 注释与文档同步（生产代码 + code-index + 集成用例 P0-77 + README 执行顺序）。
5. [x] 定向 / 全量 / `check --all-targets` 三条命令 + 证伪实测（见 §3）。
6. [x] 本轮新增代码 `rustfmt --check` 干净（§5 第 5 条；前几轮残留的 fmt 偏差未动）。
7. [x] 报告 §3–§6 落盘。
