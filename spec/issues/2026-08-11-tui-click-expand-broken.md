# TUI 点击展开失效 + 点击后焦点回输入框 Enter 失效

**状态**：Fixed（2026-08-11 按高层设计 `2026-08-11-tui-click-expand-design.md` 完成落地：手势状态机 + 焦点单一事实源；1086 tests 全绿、独立 verification PASS——**待用户实测确认**后归档）
**优先级**：高（鼠标主路径完全失效）
**创建日期**：2026-08-11

## 问题描述

用户报告两个相互关联的现象：

1. **点击消息区条目（tool / reasoning 折叠卡）完全没反应**——不展开也不折叠；`Alt+↑/↓` 聚焦后按 Enter 同样无法切换折叠。
2. 点击展开后焦点落入消息区（entry 导航模式），**点击输入框把光标放回后，Enter 依然无法提交**（被消息区消费为折叠切换），必须先按 Esc 退出导航。

## 根因分析

三个独立缺陷叠加，全部由 commit `84d7cf2e`（chat 消息流 redesign——折叠状态机与鼠标等价交互）引入：

### 1. 单击判定的坐标空间不一致（展开永不命中）

- **Down 分支**（`peri-tui/src/kit/message_area/scroll.rs` Down 处理）：按下锚点以**视觉坐标**写入 `selection_down_pos`：`(row − area.y + scroll_y, col − area.x)`（与拖拽选区语义一致）。
- **Up 分支**（`mod.rs` 单击展开 handler）：修复前用**屏幕坐标** `(mouse.row, mouse.column)` 构造 Up 与锚点比较。
- `is_click` 容差仅 ±1 行 / ±2 列 → 只要 `scroll_y > 0`（长会话必然）或 `area.x > 0`（网格前缀必然），原地单击即被判为"拖拽意图"→ `Ignored`。`entry_click_target` / `wrap_map` 换算本身正确，既有测试因同一坐标系而恒过，未抓住。

### 2. 手抖 Drag 事件使容差形同虚设（点击完全没反应的直接原因）

- 终端鼠标协议下，**按下左键后任何微移（哪怕 1 列）都会产生 `Drag` 事件**，无阈值过滤。
- `scroll.rs` Drag 分支对任何 Drag 都执行 `start_drag`——`TextSelection::start_drag`（`peri-tui/src/kit/text_selection.rs`）**无条件置 `dragging = true`**，并把 `selection_down_pos` **清空**（scroll.rs 原 `write_no_update() = None`）。
- Up 时单击展开 handler 先检查 `text_sel.read().dragging`（true → 放行给选区逻辑）；即使通过，锚点已被清空（`let Some(down) = ... else { Ignored }`）。**两条路都断**——手抖一次，点击展开永不触发。`is_click` 容差设计上就是为容忍手抖，但 dragging 检查让它失效。

### 3. 点击输入框不回退 entry 焦点（Enter 提交失效）

- 点击展开 / `Alt+↑/↓` 后，消息区进入 entry 导航模式：局部 `entry_focus = Some(slot)` + 共享 `FOCUSED_ENTRY_KEY = Some(key)`（双轨）。
- `focus_router::message_nav_accepts` 在 `entry_focused && 无修饰符` 时把 **Enter 仲裁给消息区**（折叠切换语义）。
- `input_area` 的鼠标 Down handler 命中 composer 时**只设置光标，从不清除消息区焦点** → 焦点回输入框后 Enter 仍被消息区消费。

## 修复草案（工作区未提交，未经用户实测；接手人需验证或重做）

> **2026-08-11 更新**：高层设计已另立文档
> `2026-08-11-tui-click-expand-design.md`（借鉴 grok-build 消息区域鼠标事件架构：手势状态机 + 焦点单一事实源，从结构上消灭三类根因）。本草案为其实现前身，落地以设计文档为准。

1. **坐标空间统一**（`mod.rs` 单击展开 handler）：Up 点改为与 Down 锚点一致的视觉坐标 `(row − area.y + scroll_y, col − area.x)`，并直接复用作 `visual_row`（消除二次换算）。Down 写入不动，拖拽选区语义不受影响。
2. **单击判定只认距离，不认 dragging**：
   - `scroll.rs` Drag 分支**不再清空 `selection_down_pos`**——保留锚点供 Up 时判定；`start_drag` 幂等（start 恒为 down 位置，重复调用只重置 end 再 update），选区拖拽行为不变。
   - `mod.rs` 单击展开 handler **删除 `text_sel.dragging` 检查**——单击/拖拽由 Down-Up 距离（`is_click` 容差）判定；真实拖拽（距离超容差）仍放行给 scroll 的 Up 分支做选区复制；单击命中则 Consumed 并 `text_sel.clear()`（既有取消选区语义）。
3. **点击输入框 = 焦点回到输入态**（双轨清除）：
   - `input_area.rs` 鼠标 Down handler 命中 composer 时写 `FOCUSED_ENTRY_KEY = None`（共享 atom 立即失效）。
   - `message_area/mod.rs` 新增 use_effect 块：`use_atom(&FOCUSED_ENTRY_KEY)` 订阅 + Some→None 边沿收敛清除局部 `entry_focus` 与 `interaction_option`（prev 值在 effect 内回写，保证边沿检测生效；写入在 effect 边界，符合 TUI-RENDER-001）。

## 验证（仅代码层，待用户实测）

- `cargo test -p peri-tui --lib`：1073 passed，含新增回归测试：
  - `scroll_test::test_is_click_same_screen_pos_with_scroll_offset`（滚动/网格前缀下原地单击必须判定为单击；并锁定"错误屏幕坐标比较必须判为拖拽意图"，防止回退）。
- `cargo clippy -p peri-tui --lib -- -D warnings`：通过。
- `git diff --check`：通过。
- **待办**：用户实测点击展开 / 焦点回输入框 Enter 提交；若验证通过，将状态改回 Fixed。

## 实施记录（2026-08-11，按高层设计落地）

设计文档：`spec/issues/2026-08-11-tui-click-expand-design.md`；开发流程记录：`.peri/plans/tui-click-expand-gesture-focus/`（00-context / 01-explore / 02-plan / 02-plan-review / 03-code / 04-review / 05-verification）。

- **S1 手势状态机**：`GesturePending { screen, visual, entry_hit }`（Down 冻结）取代 `selection_down_pos`；纯函数层 `freeze_down` / `drag_step` / `settle_up`；`is_click` 改屏幕坐标；**升级判定先于节流**（修复计划评审 High 1）；单击 Up 只消费冻结结果（缺陷 1/2 根除）。
- **S2 焦点单一事实源**：`FOCUSED_ENTRY: Atom<Option<FocusedEntry { slot, key }>>` 取代 `FOCUSED_ENTRY_KEY` + 局部 `entry_focus` 双轨；写点收口 `set_entry_focus`（VIEW_MODELS 写锁内派生 key）；删除 effect 收敛块；`acp_events/render.rs` 指纹与 §7 免疫读者迁移（计划评审 High 2）；reset 路径清焦点；input_area 事件边界同步清除（缺陷 3 根除，无窗口期）。
- **S3 测试基建**：`entry_click_decision` 纯判定函数 + 7 测试；S1 已有 5 状态机测试（含节流窗口内升级锁定）。
- **S4 收尾**：区域外 Up 清 gesture；注释修正；已知限制标注。
- **验证**：`cargo test -p peri-tui --lib -- --test-threads=1` → **1086 passed / 0 failed / 1 ignored**；clippy `-D warnings` 通过；`git diff --check` 通过；独立 verification **PASS**（成功标准 1-5 全过，设计不变量 I1-I4 满足）。
- **已知限制**：dispatch 层事件链与遮挡分支不可注入测试（ratatui-kit `dispatch` pub(crate)，`State` 无公开构造）；既有 flaky `test_subagent_stopped_freezes_child_trailing_bubble`（VIEW_MODELS 全局 atom 竞争，与本任务无逻辑关联）。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-11 | — | Open | user | 创建 |
| 2026-08-11 | Open | Fixed | agent | 按高层设计 `2026-08-11-tui-click-expand-design.md` 落地（S1-S4）；1086 tests 全绿、独立 verification PASS；待用户实测后归档 |

## 遗留 / 待办

- **键盘路径的终端配置问题（非代码 bug）**：macOS Terminal.app 默认把 Option 键当作"发送特殊字符"，`Alt+↑/↓` 发送的是合成 Unicode 而非 Up/Down 键码 → 键盘聚焦无法激活（focus_router.rs 已注释 macOS Option 合成字符路径，仅覆盖 `Alt+字母`，不含方向键）。用户在 iTerm2（Option 默认 ESC+）无此问题。可选改进：为 entry 焦点导航增加不依赖 Alt 方向键的备选键位（需产品确认）。
- **点击后立即按 Enter 的窗口期**：输入框点击后 `FOCUSED_ENTRY_KEY` 立即失效，但局部 `entry_focus` 经 effect 在下一渲染帧收敛——人类按键间隔远大于渲染帧，实际不可达；若未来要消除窗口期，可在键盘仲裁处增加 `entry_focus` 与 `FOCUSED_ENTRY_KEY` 的一致性检查（注意：无折叠能力 entry / `request_id` 缺失的 interaction 存在 `entry_focus=Some` 而 `FOCUSED_ENTRY_KEY=None` 的合法场景，直接 `&&` 会让 interaction 的 Enter 提交回归，需按场景细化）。
- **鼠标事件链集成测试缺失**：ratatui-kit 的 `dispatch` 为 pub(crate)，组件级 Down→Drag→Up 事件链无自动测试。建议后续评估在 ratatui-kit 暴露测试用事件注入 API，或在 peri-tui 内建模拟事件链的测试基建。
