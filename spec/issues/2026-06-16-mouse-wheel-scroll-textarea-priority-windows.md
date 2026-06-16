# Windows 环境下鼠标滚轮消息区滚动时输入框优先截获滚动事件

**状态**：Fixed（Fix #2 待验证）
**优先级**：高
**创建日期**：2026-06-16

## 问题描述

在 Windows 环境下（Windows Terminal），在消息区使用鼠标滚轮滚动时，底部输入框（textarea）会优先消费滚动事件（上下滚动自己的内容）。只有当输入框内容滚到顶或到底无法继续滚动后，消息区才开始滚动。macOS/Linux 下无此问题。

## 症状详情

| 维度 | 现象 |
|------|------|
| **触发条件** | 鼠标在消息区（messages_area）内滚动滚轮 |
| **实际行为** | 每次滚轮 tick：输入框内容先上下滚动 → 输入框无法继续滚动后 → 消息区才滚动 |
| **期望行为** | 鼠标在消息区滚动时，应直接滚动消息区内容 |
| **复现频率** | 必现，每次滚轮 tick 都发生 |
| **环境** | Windows Terminal + Windows |
| **macOS/Linux** | 无此问题 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 在 Windows Terminal 中启动 peri-tui
  2. 输入内容使消息区和输入框都有足够内容可滚动（输入框多行文本、消息区有多条消息）
  3. 鼠标停留在消息区，滚动滚轮
  4. 观察：输入框内容先上下移动，输入框滚到头后消息区才开始滚动
- **环境**：Windows + Windows Terminal

### 现象 2（2026-06-16 优先级上调）

**严重性评估**：消息区是用户阅读 agent 回复的核心区域，鼠标滚轮是该区域的自然操作方式。当前行为导致用户在 Windows 上每次用滚轮浏览消息时都先触发输入框滚动，严重阻碍消息阅读流程。无 workaround（无法通过设置或快捷键绕过此行为），Windows 用户的消息阅读体验严重受损。

## 根因分析（2026-06-16）

`filter_mouse_wheel_keys()` 使用两阶段策略过滤 ConPTY 产生的 spurious Key(Up/Down) 事件：
- **Phase 1**（peek forward）：读到 Key(Up/Down) 后，用 `Duration::ZERO` 非阻塞 peek 队列中是否有紧随的 MouseScroll。有则丢弃 Key 返回 MouseScroll。
- **Phase 2**（look backward）：若 200ms 内处理过 MouseScroll，则丢弃当前孤立的 Key。

**泄漏场景**：当 ConPTY 先交付 Key(Up/Down)、MouseScroll 尚未存入 crossterm 事件缓冲区时，Phase 1 的检查无法找到配对的 MouseScroll。**Fix #1 残留问题**：Phase 1b 统一使用 3ms 等待，当这是批次中第一个滚轮事件时，Phase 2 的 lookback 窗口内无近期 MouseScroll（200ms），Key 仍会泄漏到 `handle_event` → `handle_up`/`handle_down` → `textarea.input(Key::Up/Down)` → textarea 先滚动。随后 MouseScroll 到达 → `handle_event` → `app.scroll_up()`/`app.scroll_down()` → 消息区再滚动。

## 修复（2026-06-16，Fix #1）

**方案**：在 Phase 1 增加两阶段 peek——`poll(ZERO)` 即时检查失败后，追加 `poll(3ms)` 短暂等待 ConPTY 交付 MouseScroll。

**改动**：`peri-tui/src/event/mod.rs` `filter_mouse_wheel_keys()` Phase 1 部分，`else if` 分支追加 3ms 等待重试。

**代价**：仅在 Windows 上、裸 Key(Up/Down) 且队列无即时事件时增加 ≤3ms 延迟，箭头键操作感知不到。不影响非 Windows 平台。

从 `if event::poll(Duration::ZERO)` 改为两阶段：
- Stage 1a: `poll(ZERO)` → 有 MouseScroll 直接返回
- Stage 1b: `poll(3ms)` → 等待 ConPTY 交付，有 MouseScroll 返回

## 现有修复尝试

`filter_mouse_wheel_keys()`（`peri-tui/src/event/mod.rs:104`）已在 Windows 构建中生效。该函数通过 peek 下一条事件来过滤 Windows Terminal 伴随 MouseScroll 产生的 spurious Key(Up/Down) 事件。但问题并未完全解决——消息区和输入框均内容充足时依然发生。

## 涉及文件

- `peri-tui/src/event/mod.rs:160-228` —— `filter_mouse_wheel_keys()`：两阶段 peek 过滤 ConPTY 产生的 spurious Key(Up/Down) 事件。Phase 1b **自适应等待**：首批次（无 Phase 2 后盾）10ms，后续批次 3ms。Phase 2 使用 200ms lookback 丢弃孤立 Key。
- `peri-tui/src/event/keyboard/normal_keys.rs:334-403` —— `handle_up()`/`handle_down()`：将裸 Up/Down key 传递给 textarea，是泄漏 Key 的最终消费点
- `peri-tui/src/app/thread_ops.rs` —— `scroll_up()`/`scroll_down()` 仅修改消息区 scroll_offset

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-06-16 | — | Open | agent | 创建 |
| 2026-06-16 | Open | Open | agent | 优先级上调：中 → 高（严重阻碍消息阅读操作，无 workaround） |
| 2026-06-16 | Open | Fixed | agent | Fix #1：filter_mouse_wheel_keys Phase 1 增加 3ms 等待重试 |
| 2026-06-16 | Fixed | Open | agent | Reopened：Fix #1 未完全解决，首批次滚轮事件仍偶尔泄漏 |
| 2026-06-16 | Open | Fixed | agent | Fix #2：Phase 1b 自适应等待时长——首批次 10ms，后续批次 3ms |

## 修复记录

### 修复 #1（2026-06-16）

- **操作人**：agent
- **用户原意**：消息区鼠标滚轮不应被输入框截获，Windows 下需要正常滚动
- **修复内容**：`peri-tui/src/event/mod.rs` `filter_mouse_wheel_keys()` Phase 1 增加两阶段 peek——`poll(ZERO)` 即时检查失败后追加 `poll(3ms)` 等待 ConPTY 交付 MouseScroll，捕获时序分离的 Key/MouseScroll 事件对
- **涉及 commit**：待提交
- **验证状态**：部分有效——多数场景修复，首批次滚轮事件仍偶尔泄漏（ConPTY 延迟 >3ms 时）

### 修复 #2（2026-06-16）

- **操作人**：agent
- **根因**：Fix #1 的 Phase 1b 统一使用 3ms 等待。当批次中第一个 Key(Up/Down) 到达时，Phase 2 lookback（200ms）内无近期 MouseScroll 作为后盾。若 ConPTY 交付 MouseScroll 延迟超过 3ms，Key 直接泄漏。
- **修复内容**：`peri-tui/src/event/mod.rs` `filter_mouse_wheel_keys()` Phase 1b 改为自适应等待——检查 Phase 2 后盾是否可用（是否有近期 MouseScroll）。无后盾时（首批次）等待 10ms，有后盾时维持 3ms。Phase 2 逻辑不变，仍使用 200ms lookback 丢弃孤立 Key。
- **涉及 commit**：待提交
- **验证状态**：待验证（需 Windows 实机测试）
