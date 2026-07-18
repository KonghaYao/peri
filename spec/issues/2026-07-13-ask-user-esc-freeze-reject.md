# AskUserQuestion 面板 ESC 退出后 TUI 界面卡死，需改为确认拒绝流程

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-13

## 问题描述

当 agent 调用 `AskUserQuestion` 工具弹出问答面板后，用户按 ESC 关闭面板，再输入文字时 TUI 界面整体卡死，无法继续交互。

此外，当前 ESC 直接取消的行为不够友好——应在 ESC 时弹出确认提示，让用户选择"拒绝回答"还是"返回继续作答"。拒绝回答时应向 agent 核心发送拒绝信号，结束工具调用。

## 症状详情

### 现象 1：ESC 关闭面板后 TUI 界面无响应（主 bug）

| 步骤 | 期望 | 实际 |
|------|------|------|
| 1. agent 调用 AskUserQuestion，弹出问答面板 | — | — |
| 2. 按 ESC 关闭问答面板 | 面板关闭，键盘焦点回到输入区 | 面板关闭 |
| 3. 输入文字 | 输入正常显示，回车发送 | **TUI 界面卡死无响应** |

### 现象 2：ESC 缺少确认提示（UX 改进）

当前 ESC 静默取消问答，无任何提示。用户可能误触 ESC 导致 agent 收到空答案后继续执行不符合预期的操作。

**期望行为**：
- 按 ESC 弹出确认框："是否拒绝回答？"（选项：确认拒绝 / 返回继续作答）
- 确认拒绝 → 向 agent 发送拒绝响应，结束工具调用
- 返回继续 → 回到问答面板，恢复到按 ESC 前的选择状态

## 根因分析（经对抗验证确认）

### 卡死的直接根因

**`cancel_ask_user()` 在 ESC 路径中从未被调用。**

原因在于 ratatui-kit 的事件分发规则（`input/mod.rs:168-223`）：

- Phase 2 中，同一层（root）、同一优先级（Normal）的 handler 按注册顺序执行
- `register_root_handlers`（`event_handlers.rs:141`）在 AppShell 初始化时注册
- `AskUserPanel::use_event_handler`（`ask_user.rs:70`）在组件渲染时才注册 → **注册顺序晚于全局 handler**
- 任意 handler 返回 `EventResult::Consumed` 后会**截断同一阶段内所有后续 handler**

### 实际事件流

```
用户按 ESC
  │
  ▼
register_root_handlers (EventScope::Current, EventPriority::Normal)
  │  active_layer() == FocusLayer::Panel
  │  → close_active_panel()  // 关闭面板，清空 Panel 状态
  │  → EventResult::Consumed  // ★ 截断所有同阶段后续 handler
  │
  ▼  [AskUserPanel 的 handler 被跳过——从未执行]
  │
  ✗  cancel_ask_user() 从未被调用
  ✗  ASK_USER_RESPONSE_TX 没有任何消息
  ✗  spawn_ask_user_consumer 无限期阻塞在 rx.recv()
  ✗  Agent 永远收不到 Cancel 响应
  ✗  AskUserTool::invoke 的 broker.request() 永远挂起
  ✗  TUI 在 agent 层面被阻塞（不是渲染循环卡死）
```

### 为什么 HITL Popup 不受影响

| | HITL Popup | AskUserPanel |
|---|---|---|
| 事件优先级 | **`EventPriority::High`** | `EventPriority::Normal` |
| 全局 handler 优先级 | `Normal`（低于 High） | **`Normal`（同级，但注册更早）** |
| ESC 时谁先运行 | **HITL handler 先运行** → 发送 Reject → Consumed | **全局 handler 先运行** → close_panel → Consumed → 面板 handler 被跳过 |
| Cancel/Reject 是否发送 | ✅ 发送 | ❌ 从未发送 |

### 关键文件与行号

| 文件 | 行号 | 代码 | 角色 |
|------|------|------|------|
| `peri-tui/src/kit/event_handlers.rs` | 186-188 | `close_active_panel()` + `Consumed` | **卡死的直接执行者**：关闭面板后截断所有后续 handler |
| `peri-tui/src/kit/event_handlers.rs` | 141 | `EventPriority::Normal` | 与 AskUserPanel 同级，但注册更早 |
| `peri-tui/src/kit/panels/ask_user.rs` | 70 | `EventPriority::Normal` | 因优先级相同且注册更晚，ESC 时被跳过 |
| `peri-tui/src/kit/panels/ask_user.rs` | 55-66 | `cancel_ask_user()` | 本应发送 Cancel，但从未被调用 |
| `peri-tui/src/kit/ask_user_action.rs` | 44-73 | `spawn_ask_user_consumer` | 后台消费者，无限期阻塞在 `rx.recv()` |
| `peri-tui/src/kit/popups/hitl_popup.rs` | 43 | `EventPriority::High` | **正确的处理模式**——参考实现 |
| `peri-middlewares/src/tools/ask_user_tool.rs` | 109 | `self.broker.request(ctx).await` | Agent 侧挂起点 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 与 agent 对话，让 agent 调用 `AskUserQuestion` 工具
  2. AskUser 面板出现后，按 ESC
  3. 尝试在输入框输入任意文字
  4. 观察：TUI 卡死，无法交互
- **环境**：所有环境

## 修复方案

### 方案 A（最小改动）：提升面板 handler 优先级

将 `AskUserPanel::use_event_handler` 的优先级从 `EventPriority::Normal` 改为 `EventPriority::High`，与 HITL Popup 对齐。

```diff
// peri-tui/src/kit/panels/ask_user.rs:70
- hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
+ hooks.use_event_handler(EventScope::Current, EventPriority::High, move |event| {
```

**效果**：AskUserPanel 的 handler 在全局 handler 之前运行 → ESC 触发 `cancel_ask_user()` 发送 Cancel 响应 → 返回 Consumed 阻止全局 handler → agent 正常收到响应继续。

**风险**：此修改只修复了 bug（ESC 不卡死），**未包含用户期望的确认弹窗功能**。

### 方案 B（完整修复）：ESC 弹出确认弹窗 + 拒绝语义

1. **优先级提升**：`EventPriority::Normal` → `EventPriority::High`（同方案 A）
2. **ESC 拦截**：面板 handler 捕获 ESC 后，**不直接调用 `cancel_ask_user()`**，而是：
   - 记录当前 answers 状态
   - 打开确认弹窗 → 用户选择"确认拒绝" or "返回继续"
3. **确认拒绝**：通过 `ASK_USER_RESPONSE_TX` 发送 `AskUserResponseAction::Reject`
4. **返回继续**：关闭弹窗，恢复面板焦点和之前的选择状态
5. **全局 handler 加固**（防御性）：在 `event_handlers.rs:186-188` 的 `FocusLayer::Panel` 分支中，若检测到当前活跃面板为 `AskUser`，则也尝试发送 Cancel（双重保险，防止未来其他面板出现同类问题）

### 拒绝语义设计

```rust
// peri-tui/src/kit/ask_user_action.rs - 新增变体
pub enum AskUserResponseAction {
    Submit { request_id_str: String, answers: Vec<Option<usize>> },
    Cancel { request_id_str: String },   // 现有
    Reject { request_id_str: String },   // 新增：明确的拒绝
}
```

| 操作 | ACP 发送 | Agent 侧翻译 |
|------|---------|-------------|
| 正常提交 | `{"action":"submit", "answers":[...]}` | `InteractionResponse::Answers(...)` |
| ESC→确认拒绝 | `{"action":"reject"}` | `InteractionResponse::Rejected` → `Err(AgentError::ToolRejected)` |
| 全局 handler 兜底 Cancel | `{"action":"cancel"}` | `InteractionResponse::Answers(vec![])` |

## 涉及文件

| 文件 | 改动类型 | 说明 |
|------|---------|------|
| `peri-tui/src/kit/panels/ask_user.rs:70` | **优先级修正** | `Normal` → `High` |
| `peri-tui/src/kit/panels/ask_user.rs:181-184` | **ESC 逻辑重写** | 直接 cancel → 打开确认弹窗 |
| `peri-tui/src/kit/popups/` | **新增确认弹窗** | "是否拒绝回答？" 二选一组件 |
| `peri-tui/src/kit/ask_user_action.rs` | **新增 Reject 变体** | `AskUserResponseAction::Reject` + `handle_reject()` |
| `peri-tui/src/kit/event_handlers.rs:186-188` | **防御性加固** | Panel 分支检测 AskUser 时发送 Cancel |
| `peri-middlewares/src/tools/ask_user_tool.rs` | **区分 Cancel/Reject** | Cancel → 空 Answers，Reject → ToolRejected |
| `peri-agent/src/interaction/mod.rs` | **新增 Rejected 变体** | `InteractionResponse::Rejected` |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-13 | — | Open | agent | 创建 |
| 2026-07-13 | — | — | agent | 对抗验证修正根因：cancel_ask_user() 从未被调用，是事件优先级问题而非竞态 |

## 修复记录

（待 fix-issue 追加）
