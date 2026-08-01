# Model 面板左侧 profile 行鼠标点击完全无反应

**状态**：Fixed
**优先级**：中
**创建日期**：2026-08-01

## 问题描述

打开 Model 面板后，鼠标点击左侧 profile 卡片行（fable / opus / sonnet / haiku）完全无反应——不切换 active profile、光标不跟随、面板不关闭。用户感知为"消息区域遮蔽了模型选择弹窗的鼠标点击"（点击被吃掉）。

## 症状详情

- 环境：cargo run 最新代码；开着 Model 面板；消息区有长内容
- 操作：`/model` 打开 Model 面板 → 鼠标点击左侧任一 profile 行
- 实际：无任何反馈（状态栏模型不变、面板光标不动、面板不关闭）
- 期望：点击行 = 选中并切换 active profile（与键盘 ↑/↓ 选择即切换语义一致，即 a8d0ff79 "click as enter" 统一模式）
- 对比：同批 click-as-enter 覆盖的其它 8 个面板（config/theme/thread_browser/memory/cron/login/ask_user/plugin）点击均有效，唯独 Model 面板无效

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. `/model` 打开 Model 面板
  2. 鼠标左键点击左侧任意 profile 卡片行（如 opus 行的任意一行）
  3. 无任何响应
- **环境**：macOS，120x40 终端，cargo run 最新代码

## 根因分析

1. `peri-tui/src/kit/panels/model.rs` 只注册了键盘 `use_event_handler`（`model.rs:75`，Current + Normal），**没有任何鼠标处理**。
2. `a8d0ff79`（2026-08-01 "所有 Enter 语义面板/弹窗支持 click as enter"）覆盖了 8 个面板 + 7 个弹窗，**唯独遗漏了 model.rs**——Model 面板的 Enter 语义（↑/↓ 选择即切换 profile）明显属于该模式范围。
3. 点击被"吃掉"的机制：面板打开时 `ACTIVE_PANEL` 非空 → 消息区/输入区/状态栏三个背景组件均通过 `mouse_router::is_occluded()` 让路（Ignored）→ 事件进入 Phase 2 → Model 面板 Current handler 不处理鼠标（Ignored）→ 无任何消费者 → 点击落空。用户感知为"被消息区域遮蔽"。

## 涉及文件

- `peri-tui/src/kit/panels/model.rs` —— 缺鼠标点击处理（本次修复）
- `peri-tui/src/kit/panel_mouse.rs` —— 共享命中工具（AreaTracker / ListLayout / hit_item，本次复用）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-01 | — | Open | agent | 创建；诊断确认根因：model.rs 无鼠标处理，a8d0ff79 遗漏 |
| 2026-08-01 | Open | Fixed | agent | 修复：按 click-as-enter 模式补 model.rs 鼠标处理 |

## 修复记录

### 修复 #1（2026-08-01）

- **操作人**：agent
- **用户原意**：点击 Model 面板（"状态栏的模型选择弹窗"）里的行应该能切换模型，而不是点击无反应
- **修复内容**：
  - `model.rs`：将键盘 handler 改为 `use_event_handler_with_options(Current, Normal, EventOptions { hit_test: true })`，新增鼠标分支——左侧栏（主区宽 45%，排除滚动条最右列）内左键点击按 `ListLayout { header_rows: 2, item_rows: 3, footer_rows: 1, visible_items: 4 }` 反推 profile 索引，命中则 `cursor = idx + switch_active_alias(idx)`（与键盘 ↑/↓ 语义一致）；未命中 Down(Left) 消费防穿透（与 config/theme 等面板一致）。
  - 左侧 ScrollView 改为外部受控 `state: Some(left_scroll)`（`use_state(ScrollViewState::default)`），鼠标 handler 读 `offset().y` 作为 `scroll_start`——列表滚动后点击命中不漂移。
  - 右侧 K/V 区点击不实现编辑（本次最小修复），Down(Left) 消费防穿透。
- **涉及 commit**：未提交（工作区改动）
- **验证状态**：已验证（e2e tui-tester 实测：点击 opus 行 → 状态栏 active profile 从 fable 切换为 opus；`cargo test -p peri-tui --lib` 658 通过；`cargo clippy -p peri-tui --all-targets` 零警告）
