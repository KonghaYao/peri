# S7e 实现报告：A20 阅读锚点身份重定位（渲染侧）

- 工作区：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`（保持未提交；本会话无任何 git 写操作）
- 上游：`.recon/S7d-verification.md`（独立复核，判据 5/8 = FAIL）、`.recon/S7d-verify-probe.rs.txt`（复核者探针，构造法复用）
- 目标：A20「阅读锚点不跳动」在**任意插入形态**下成立，且**彻底去掉跨层槽位坐标**（桥接槽位号不得再索引渲染槽位）

## 0. 最终状态与结论

全部交付项完成，证据均在 §4–§6；关键结论三条：

1. **跨层槽位路径已不存在**：`HISTORY_PREPEND_MARK`、`prepend_scroll_delta`、`SlotIndex::slots_visual_range`、`prepend_visual_range` 在 `peri-tui/src` 与 `docs/` 中 grep 无任何引用（唯一提及是 code-index 的历史说明行，写明「已删除」）；`MergeOutcome::inserted_slot_range` 只剩桥接层内部用途（诊断日志 + 同层 merge 用例断言），**无任何渲染侧消费点**。
2. **A20 由渲染侧身份重定位承担**：`Transcript::prepare` 发布前捕获「视口顶行所在单元的身份 + 单元内位移」，发布后在新索引里重查该身份；身份找不到 / 不唯一 → 不补偿（安全方向）；跟随态与底部哨兵不换算。
3. **复核者反例形状被当轮代码覆盖**：端到端用例实测「桥接 `committed` 4 槽 vs 渲染 `VIEW_MODELS` 3 槽（槽 1 是 `TuiCollapsedGroup`）」时，锚点单元屏幕行不变；拆组形态同样成立。

## 1. 根因（采信复核 S7d §5.1；未重新发明）

`HISTORY_PREPEND_MARK` 存的是**桥接侧 `state.committed` 的槽位包络**（`MergeOutcome::inserted_slot_range`，写入点 `peri-tui/src/kit/acp_events/history_page.rs:78`——本轮已删除该写入），消费方却用它索引**发布后 `snapshot.items` 的下标**（渲染槽位）：

1. 桥接槽位：`peri-tui/src/kit/history_paging.rs`（`insert_slot_for` → `slot` 注册进 `HistorySpans`）。
2. 发布前会把相邻成功工具卡压成一张 `TuiCollapsedGroup`：`peri-tui/src/kit/acp_events/render.rs:140`（`group_successful_tools_from`）、`:461`（`is_mergeable`）、`:658`（`build_collapsed_group`）→ **槽位数变少**。
3. 渲染 `SlotIndex` 的槽位 = 发布后 `snapshot.items` 下标：`peri-tui/src/kit/message_area/transcript_index.rs`（`set_source` 传入的 items）。
4. 消费方（已删除）：`auto_follow.rs` 的 `index.slots_visual_range(slot_range.0..slot_range.1)`。

后果两方向：过补偿（包络终点命中 `slots.end == count` 分支取整窗行数 → 视口被推到窗口底部）与不补偿（终点越界 → `None` → delta 0）。

## 2. 本轮语义（最终实现，按代码事实写）

### 2.1 身份重定位（渲染侧，`Transcript::prepare`）

1. **发布前**（旧 `SlotIndex` 仍在位）：`ViewportAnchor::capture(&self.index, scroll_y)`（`viewport_anchor.rs:107`）
   - 视口顶行落在哪个槽位：`SlotIndex::visual_lookup(scroll_y)?.slot_index`；
   - 该单元的**稳定身份**（见 2.2）；
   - `offset_within_unit = scroll_y - slot_visual_start(slot)`（≥ 0，表示「视口顶行在该单元起点之后第几行」）。
2. **发布后**（新索引重建完成）：`anchor.relocate(&self.index)`（`viewport_anchor.rs:122`）
   - 在新 `items` 里按同一身份查单元 → 命中**唯一**槽位 → `target_offset = 新视觉起点 + offset_within_unit`
     （等价的另一种写法是任务书里的 `新起点 − within_unit_offset`，差别只在符号约定：本实现的
     `offset_within_unit ≥ 0`；两条等价式都使「锚点单元屏幕行 = `视觉起点 − 视口偏移`」不变）；
   - 命中 **0 个**（被替换 / 被移除 / 被并入折叠组而自身消失）→ `None` = **不补偿**，
     `tracing::trace!(target: "msg_scroll_diag", "prepend anchor: identity not found in the new window → no compensation")`（`:139`）；
   - 命中 **≥2 个**（内容完全相同的单元，身份按内容派生的那些）→ `None` = **不补偿**（歧义不猜），
     trace `"identity is ambiguous in the new window → no compensation"`（`:130`）。
3. **配对信号**：桥接侧 `HISTORY_PREPEND_EPOCH`（`acp_events/history_page.rs:79`，在发布前 +1）仍是唯一的跨层事实；
   `Transcript.merge_epoch_seen`（`transcript.rs:36`）只在**观测到发布变化**时推进（`transcript.rs:159-171`），
   因此 epoch 先于/晚于发布被读到都不会漏配；`published` 为假时**不产出**重定位（同一发布的多帧不重复补偿）。
4. **应用点**：`run_auto_follow` 并入分支（`scroll/auto_follow.rs:251-263`）——`PrependRelocation::target_for`
   （`viewport_anchor.rs:162`）在 `follow_bottom == true` 或 `scroll_y == usize::MAX`（跟随到底哨兵）时返回 `None`；
   否则按**相对量** `scroll_y + (target − from)` 落到 `set_offset`。跟随分支与非浏览态后续路径未改动。

### 2.2 锚点身份（既有身份类型，不新造体系）

1. 可折叠单元 → 既有 `FoldKey`（`tui_render_unit/fold.rs:40`，经既有 `message_area/entry_nav.rs:39` 的 `fold_key_of` 派生）：
   `Tool(tool_id)` / `Group(成员 tool_id 序列)` / `SubAgent(instance_id)` / `Interaction(request_id)` /
   `SystemReminder(id)` / `Reasoning(message_id)`。
2. 其余单元 → 既有内容身份：`content_hash`（assistant 正文走既有 `TuiAssistantBubble::stable_identity_hash`，
   `bubble.rs:170`，与 `hits.rs:140` 的点击陈旧校验同口径、排除时变时长）＋单元种类判别位
   （`content_kind`，防跨种类同 hash 互认，如所有 `TuiDivider` 的 `content_hash` 都是 0）。

### 2.3 删除跨层槽位路径

1. `HISTORY_PREPEND_MARK` **整体删除**（该 atom 只存在于本分支未提交的 S7d 工作里；epoch 已承载「发生过并合」）。
2. 连带删除只服务于它的 `prepend_visual_range`（`auto_follow.rs`）、`prepend_scroll_delta`（`history_paging.rs`）、
   `SlotIndex::slots_visual_range`（`transcript_index.rs`，删除后该文件与 HEAD 无 diff）与其 S7d 用例。
3. `MergeOutcome::inserted_slot_range` 保留（同层 merge 事实：诊断日志 + `history_paging_test.rs` 断言），无跨层消费点。

### 2.4 读序不变量（新语义成立的前提）

`message_area/mod.rs`：`read_render_snapshot()` 必须早于 `HISTORY_PREPEND_EPOCH.get()`（`:242-247`）。
桥接侧先推代数再发布，所以「快照里看得到并入 = 代数也看得到」；反序会漏配一整次并入。代码注释与模块文档 4. 都写明了这条。

## 3. 改动清单（绝对路径）

| # | 文件 | 说明 |
| - | ---- | ---- |
| 1 | `…/peri-tui/src/kit/message_area/viewport_anchor.rs`（新增 214 行） | `UnitIdentity` / `ViewportAnchor::{capture,relocate}` / `PrependRelocation::target_for` + 模块文档（语义、安全方向、边界、移除条件） |
| 2 | `…/peri-tui/src/kit/message_area/viewport_anchor_test.rs`（新增 277 行） | 7 条单测：身份+单元内位移、下方插入零补偿、折叠组压组、锚点被吞、身份缺失/歧义、位移不夹取、跟随态/哨兵不换算 |
| 3 | `…/peri-tui/src/kit/message_area/viewport_anchor_e2e_test.rs`（新增 473 行） | 4 条端到端：真实 `handle_history_page_loaded` → `push_view_models` → `Transcript::prepare`（场景 A、中间缺口、折叠组反例、拆组） |
| 4 | `…/peri-tui/src/kit/message_area/transcript.rs`（+49） | 捕获/重定位两半 + epoch 配对（`merge_epoch_seen`）+ 一次性 `take_prepend_relocation` |
| 5 | `…/peri-tui/src/kit/message_area/mod.rs`（+21/−1） | 读序注释与 epoch 读取点；把重定位结果注入 `AutoFollowCtx` |
| 6 | `…/peri-tui/src/kit/message_area/scroll.rs`（+8/−1） | `AutoFollowCtx.prepend_relocation` 字段 |
| 7 | `…/peri-tui/src/kit/message_area/scroll/auto_follow.rs`（+41） | 浏览态应用重定位；向前边界请求注释（行为未改） |
| 8 | `…/peri-tui/src/kit/atoms.rs`（+33/−7） | `HISTORY_PREPEND_EPOCH` 文档改写为「唯一的并合事实，不带槽位号」 |
| 9 | `…/peri-tui/src/kit/history_paging.rs`（未跟踪文件，618 行） | 删除 `prepend_scroll_delta`；`inserted_slot_range` 只留桥接层 |
| 10 | `…/peri-tui/src/kit/acp_events/history_page.rs`（未跟踪文件） | 删除 MARK 写入（`:78`），只留 epoch 推进（`:79`）与诊断日志 |
| 11 | `…/peri-tui/src/kit/history_paging_test.rs` | 顶注指针更正（A20 用例现在在 `viewport_anchor*`） |
| 12 | `…/peri-tui/src/kit/message_area/transcript_index.rs` | `slots_visual_range` 删除后与 HEAD 无 diff（净零） |
| 13 | `…/docs/code-index/peri-tui.md` | 速查表分页行与更新行改写为身份重定位口径（符号表只列现存符号） |

（`scroll/event.rs`、`throttle.rs`、`scroll_test.rs` 的其余 diff 属上一轮触发判据工作；S7d 曾追加的 4 条 `test_prepend_*` 已随旧口径删除，`grep -c "fn test_prepend" scroll_test.rs` = 0。）

注：工作区未提交，表内 `+n/−n` 是该文件 vs HEAD 的**累计** diff（含前几轮工作），描述列只写与本轮（S7e）相关的部分。

## 4. 测试（命令 + 结果原文）

### 4.1 新增用例（定向）

```
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history && \
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- viewport_anchor
```

```
running 11 tests
test kit::message_area::viewport_anchor::e2e_tests::a20_e2e_group_split_keeps_the_anchored_unit_screen_row ... ok
test kit::message_area::viewport_anchor::e2e_tests::a20_e2e_collapsed_group_shape_keeps_the_anchored_unit_screen_row ... ok
test kit::message_area::viewport_anchor::e2e_tests::a20_e2e_middle_gap_fill_does_not_move_the_viewport ... ok
test kit::message_area::viewport_anchor::e2e_tests::a20_e2e_insert_above_viewport_keeps_the_anchored_unit_screen_row ... ok
test kit::message_area::viewport_anchor::tests::capture_and_relocate_keep_the_anchored_unit_on_its_screen_row ... ok
test kit::message_area::viewport_anchor::tests::relocate_is_zero_when_the_inserted_block_is_below_the_viewport ... ok
test kit::message_area::viewport_anchor::tests::relocate_survives_neighbouring_tool_cards_collapsing_into_one_group ... ok
test kit::message_area::viewport_anchor::tests::relocate_refuses_when_the_anchored_unit_was_absorbed_into_a_group ... ok
test kit::message_area::viewport_anchor::tests::relocate_refuses_when_identity_is_missing_or_ambiguous ... ok
test kit::message_area::viewport_anchor::tests::relocate_does_not_clamp_a_within_unit_offset_that_outgrew_the_unit ... ok
test kit::message_area::viewport_anchor::tests::target_for_skips_follow_bottom_and_bottom_sentinel ... ok

test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 1812 filtered out; finished in 0.10s
```

（完整日志含编译行：`.recon/S7e-green-viewport-anchor.txt`。）

**用例与要求的对应**：

1. 折叠组形态（复核者反例）：`a20_e2e_collapsed_group_shape_...`——同一份发布实测 `state.committed.len() == 4`
   而 `probe.after.slot_count() == 3`、槽 1 是 `TuiCollapsedGroup`；断言锚点（自有气泡）屏幕行不变，
   且重定位量 = 折叠组在渲染布局里的真实高度（1 行）而非桥接槽位数。
2. 场景 A（插入在视口上方）：`a20_e2e_insert_above_viewport_...`；场景 B（中间缺口填补）：
   `a20_e2e_middle_gap_fill_...`（断言 `target == scroll_y`，即视口不动）。
3. 拆组形态：`a20_e2e_group_split_...`（发布前 2 张相邻成功卡已压组，插入把它们拆开 → 槽位变多，锚点屏幕行不变）。
4. 跟随底部 / 底部哨兵：`target_for_skips_follow_bottom_and_bottom_sentinel`（`target_for(true, …) == None`、
   `scroll_y == usize::MAX → None`）；生产调用点 `auto_follow.rs:251` 在这些情形下不调 `set_offset`，跟随分支原样。
5. 身份缺失 / 匿名边界：`relocate_refuses_when_identity_is_missing_or_ambiguous`（身份被替换、身份歧义、空索引都返回 `None`）
   ＋ `relocate_refuses_when_the_anchored_unit_was_absorbed_into_a_group`（被并入折叠组 → `None`）。

### 4.2 要求的过滤词 `history_paging`

```
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history && \
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- history_paging
```

```
running 7 tests
test kit::history_paging::tests::test_decode_page_response_rejects_unknown_shape ... ok
test kit::history_paging::tests::test_decode_page_response_keeps_sub_item_kind ... ok
test kit::history_paging::tests::test_merge_page_inserts_into_middle_interval ... ok
test kit::history_paging::tests::test_merge_page_orders_by_position_and_dedupes ... ok
test kit::history_paging::tests::test_settle_request_drops_response_for_stale_identity ... ok
test kit::history_paging::tests::test_request_older_page_if_due_merges_concurrent_triggers ... ok
test kit::history_paging::tests::test_invalidate_browsing_stops_paging_for_session ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 1816 filtered out; finished in 0.01s
```

（完整日志：`.recon/S7e-history-paging.txt`。）

## 5. 证伪（两种状态原文；退化已还原并字节校验）

### 5.1 证伪 1：短路身份重定位（`transcript.rs` 里 `self.prepend = None`）

退化点：`Transcript::prepare` 的配对分支内不再产出重定位，其余全部不动。原文（`.recon/S7e-falsify1-short-circuit.txt`）：

```
test kit::message_area::viewport_anchor::e2e_tests::a20_e2e_collapsed_group_shape_keeps_the_anchored_unit_screen_row ... FAILED
test kit::message_area::viewport_anchor::e2e_tests::a20_e2e_middle_gap_fill_does_not_move_the_viewport ... FAILED
test kit::message_area::viewport_anchor::e2e_tests::a20_e2e_insert_above_viewport_keeps_the_anchored_unit_screen_row ... FAILED
test kit::message_area::viewport_anchor::e2e_tests::a20_e2e_group_split_keeps_the_anchored_unit_screen_row ... FAILED
...
thread '...a20_e2e_insert_above_viewport_keeps_the_anchored_unit_screen_row' panicked at peri-tui/src/kit/message_area/viewport_anchor_e2e_test.rs:256:10:
并入帧必须产出重定位（身份认得出来）

test result: FAILED. 7 passed; 4 failed; 0 ignored; 0 measured; 1812 filtered out; finished in 0.10s
```

### 5.2 证伪 2：把重定位退回「槽位几何」口径（旧跨层坐标类的等价退化）

退化点：`ViewportAnchor` 记下旧槽位号，`relocate` 直接用 `index.slot_visual_start(旧槽位号)`——正是复核判 FAIL 的
「桥接槽位号索引渲染槽位」这一类的等价物（不再按身份重查）。原文（`.recon/S7e-falsify2-slot-geometry.txt`）：

```
---- kit::message_area::viewport_anchor::e2e_tests::a20_e2e_collapsed_group_shape_keeps_the_anchored_unit_screen_row stdout ----
thread '...' panicked at peri-tui/src/kit/message_area/viewport_anchor_e2e_test.rs:379:5:
assertion `left == right` failed: 折叠组形态：同一可见单元屏幕行不变（A20）
  left: 0
 right: -1

---- ...a20_e2e_insert_above_viewport_keeps_the_anchored_unit_screen_row stdout ----
assertion `left == right` failed: 场景 A：同一可见单元屏幕行不变（A20）
  left: 2
 right: -1

---- ...a20_e2e_group_split_keeps_the_anchored_unit_screen_row stdout ----
assertion `left == right` failed: 拆组形态：同一可见单元屏幕行不变（A20）
  left: 3
 right: -1

test result: FAILED. 4 passed; 7 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s
```

（另有 4 条单测同轮 FAIL，含「身份不在新窗口 → 不补偿」「锚点被并入折叠组后身份消失 → 不补偿」——
说明退化同时破坏了安全方向。注意：场景 B（中间缺口）在此退化下**仍然通过**，与复核 S7d §2 判据 1/2 的
「口径内 PASS」一致——正是这两套坐标在无折叠组窗口里偶然相等。）

### 5.3 还原与正向对照

1. `cp` 备份还原两个文件；`diff -q` 均输出 `RESTORED-BYTES-IDENTICAL`；`shasum -a 256` 与备份一致
   （`transcript.rs` = `d79b2c32…61ba2`，`viewport_anchor.rs` = `0ffe83d4…592b1c`）；`grep -c "证伪临时"` 两个文件均为 0。
2. 还原后复跑 §4.1 → `11 passed; 0 failed`（`.recon/S7e-green-viewport-anchor.txt`）。

## 6. 回归

### 6.1 全量 lib

```
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history && \
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib
```

```
running 1823 tests
...
test result: ok. 1817 passed; 0 failed; 6 ignored; 0 measured; 0 filtered out; finished in 28.17s
```

完整日志：`.recon/S7e-full-lib-final.txt`。本轮共跑两次全量（文档修订前后），均 0 failed（24.39s / 28.17s），
未观察到 S7d 提到的 `steer_consumer` flake。

### 6.2 编译检查

```
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history && \
./scripts/cargo-rmcp-patched.sh check --locked -p peri-tui --all-targets
```

```
    Checking peri-tui v0.2.0 (/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history/peri-tui)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 34.20s
CHECK_EXIT=0
```

日志：`.recon/S7e-check-all-targets-final.txt`（0 warning）。

### 6.3 跨层路径复核（静态）

`grep -rn "HISTORY_PREPEND_MARK|prepend_scroll_delta|slots_visual_range|prepend_visual_range|inserted_visual_range" peri-tui/src docs e2e npm-packages`
→ 仅 `docs/code-index/peri-tui.md:15` 的历史说明行（写明已删除）；`inserted_slot_range` 仅出现在
`history_paging.rs`（定义/合并）、`acp_events/history_page.rs:85`（日志）、`history_paging_test.rs`（同层断言）。

## 7. 文档同步

1. `docs/code-index/peri-tui.md:15`（更新行）与 `:80`（分页速查行）：改为「渲染侧身份重定位、桥接槽位号不跨层、
   `MergeOutcome::inserted_slot_range` 仅桥接层」口径，并登记 A20 用例位置（这正是复核 §5.3 点名的旧前提处）。
2. `viewport_anchor.rs` 模块文档：语义两条（capture/relocate）、身份缺失三种来源与不补偿选择、四条仍未覆盖的边界、
   模块移除条件。
3. `atoms.rs` 的 `HISTORY_PREPEND_EPOCH` 文档：明确「只表达发生过一次并合，不带槽位号」并指向 S7d 复核 §5.1。
4. `history_page.rs:75-78`、`auto_follow.rs:238-248`、`transcript.rs:152-157`、`message_area/mod.rs:242-246`：
   调用点注释与实现同口径。
5. `history_paging_test.rs` 顶注指针更正（旧引用 `scroll_test::test_prepend_*` 已不存在）。

## 8. 已知边界与移除条件

1. **锚点粒度 = 顶层单元**：`SlotIndex` 只给整槽视觉区间，锚点落在展开的折叠组 / 子 agent 组内部时，
   钉住的是**组位置**，组内那一行不保证。移除条件：`SlotIndex` 能给出嵌套子项视觉起点后按子项身份细化；
   模块存在理由（跨层槽位不可互换）不因此消失。
2. **单元内位移越界**：锚点单元在新布局里变矮（内容被折叠/缩短）时 `target_offset` 可能落到该单元之外；
   有意**不夹取、不额外补偿**（夹取会悄悄改掉位移量，引入第二种语义）。用例
   `relocate_does_not_clamp_a_within_unit_offset_that_outgrew_the_unit` 固化了这一选择。
3. **身份歧义**：非折叠单元的身份按内容 hash 派生，两条完全相同的用户气泡 / 多个 `TuiDivider`
   会把重定位判为歧义 → 不补偿（宁可少补一次，也不猜错单元）。
4. **同帧多次并入**：一帧落多页时只按最新快照重定位一次（身份重定位对「插入了多少块」不敏感，结果仍正确；
   被折叠的是中间诊断粒度）。
5. **晚一帧无害、读序必须保持**：`message_area/mod.rs` 的快照读必须早于 epoch 读（§2.4）；违反会漏配一整次并入。
6. **`MergeOutcome::inserted_slot_range` 的保留理由**：同为桥接层诊断与 merge 单测的断言对象；一旦这两个用途消失即可删除。

## 9. 未验证项（如实列出）

1. **无渲染帧 / 整组件树**：e2e 走真实桥接事件 + 真实发布 + 生产 `Transcript::prepare`，但没有 `ratatui` 布局帧，
   也没有 `handle_event` 闭包；跟随态行为在决策层验证（`target_for` 返回 `None` + 后续跟随分支未改动），
   与 S7c/S7d 口径一致。
2. **P0-77 仍是手工用例**：真实终端里滚到顶端不跳的最终确认未做（`e2e/` 无翻页自动化）。
3. **帧序竞态未构造复现**：`merge_epoch_seen` 的「只增且只在观测到发布变化时推进」是对 S7d §7.2 竞态窗口的
   设计性闭合，只有静态推演 + 4 条 e2e 同帧路径，无并发帧交错实验。
4. **S7d §6.1 的 `build_entries` 同条起止配对 panic 未修**（超出本轮范围）：`acp_events/history_page.rs:192`
   在同一条目内「起点 + 终点」形状下 `index out of bounds`；当前服务端投影不会产出该形状，既有用例一律
   把起止放在不同条目。本轮未改该函数，也未加回归用例。
5. **未跑 `lefthook run pre-commit`**：本分支 fmt 门整体不干净（历史文件偏差已知，S7d §4.3.5 曾逐文件核对）。
