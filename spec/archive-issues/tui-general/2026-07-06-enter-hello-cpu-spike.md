> 归档于 2026-07-06，原路径 spec/issues/2026-07-06-enter-hello-cpu-spike.md

# TUI 输入 hello 并 Enter 后 CPU 100%

**状态**：Verified
**优先级**：高
**创建日期**：2026-07-06

## 问题描述

在 TUI 输入区输入 `hello` 并按 Enter 提交后，消息区会出现用户输入的 `hello`，随后 CPU 使用率升到 100%。这是基础提交路径中的异常行为，影响正常使用。

## 症状详情

| 维度 | 观察 |
|------|------|
| 触发输入 | `hello` |
| 触发操作 | 按 Enter 提交 |
| 可见 UI 表现 | 消息区出现 `hello` |
| CPU 行为 | 随后 CPU 达到 100% |
| 与预期差异 | 提交后应进入正常 agent 响应/loading 流程，不应持续占满 CPU |

## 复现条件

- **复现频率**：用户当前反馈可复现，具体是否必现待验证
- **触发步骤**：
  1. 启动 Peri TUI
  2. 在输入区输入 `hello`
  3. 按 Enter 提交
  4. 观察消息区出现 `hello`
  5. 观察 CPU 使用率升至 100%
- **环境**：Peri TUI，当前工作区；具体模型/终端/运行命令待补充

## 相关历史 Issue

以下历史 issue 与「TUI 交互后 CPU 飙高/提交后渲染状态异常」相关，但触发条件不同，当前现象先作为新 issue 记录：

- `spec/issues/2026-07-03-tui-double-slash-cpu-spike.md` —— 输入 `//` 后 CPU 持续高负载，已 Verified；当前触发条件是普通文本 `hello` + Enter。
- `spec/issues/2026-07-05-mouse-move-cpu-spike.md` —— 鼠标移动导致 CPU 暴涨；当前触发条件是键盘提交。
- `spec/issues/2026-07-05-message-flow-render-sync-freeze.md` —— 提交后消息显示/loading/history 异常；当前新增现象是消息出现后 CPU 100%。

## 涉及文件

- `peri-tui/src/kit/input_area.rs` —— 输入区 Enter 提交路径和本地用户消息回显相关。
- `peri-tui/src/kit/submit_consumer.rs` —— 提交后的 prompt 发送/loading 生命周期相关。
- `peri-tui/src/kit/render_bridge.rs` —— 消息显示缓存刷新相关。
- `peri-tui/src/kit/message_area.rs` —— 消息区渲染与可见内容显示相关。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-06 | — | Open | agent | 创建 |
| 2026-07-06 | Open | Fixed Pending Verification | agent | 已修复两段问题：loading spinner 自激导致 CPU 100%；输出结束后 AgentDone 未转 TurnDone 导致 spinner 残留。等待用户在 TUI 中回归验证。 |
| 2026-07-06 | Fixed Pending Verification | Verified | user | 用户反馈已验证完毕。CPU 100% 与输出结束后 spinner 残留问题已通过手动回归。 |

## 修复记录

### 2026-07-06：修复 loading spinner 自激循环

**根因**：`peri-tui/src/kit/message_area.rs` 的 `build_footer_lines` 在 loading 稳态下每次 render 都写 hook state（`was_loading` / `load_start` / `spinner_state`），形成 `render → state write → render` 自激循环。用户输入 `hello` 并 Enter 后进入 loading，spinner footer 持续触发重渲染，CPU 升至 100%。

**修复**：

- `was_loading` / `load_start` 只在 loading 状态变化时写入。
- spinner catch-up 只在 `delta > 0` 时写 `spinner_state`。
- 增加回归测试：
  - `test_footer_loading_steady_state_has_no_control_state_transition`
  - `test_spinner_catchup_skips_state_write_when_no_delta`

**验证**：

- `cargo test -p peri-tui --lib message_area -- --nocapture`：通过，9 passed。
- `cargo check -p peri-tui`：通过。
- `cargo fmt --check`：通过。
- `code-reviewer` 审查：未发现 hook 顺序违规；确认修复命中该自激路径。

### 2026-07-06：修复输出结束后 spinner 残留

**用户追加反馈**：输出结束后 spinner 还在。

**根因**：`peri-tui/src/kit/acp_notifier.rs` 中 `AcpNotification::AgentDone` 被当作未处理通知丢弃，没有转成 `AcpEventData::TurnDone` 发给 `acp_bridge` / `render_bridge`。因此结束后 `ACP_STATE.is_loading` 或 `RENDER_CACHE` 的 `CurrentTurn` 仍可能残留，`MessageArea` 继续认为处于 loading。

**修复**：

- `AgentDone` 现在转换为 `AcpEventData::TurnDone`。
- 同时发送到 `bridge_tx` 和 `render_bridge_tx`，确保状态层和渲染缓存都收到结束边界事件。
- 增加回归测试：`test_agent_done_forwards_turn_done_to_bridges`。

**验证**：

- `cargo test -p peri-tui --lib acp_notifier -- --nocapture`：通过，8 passed。
- `cargo test -p peri-tui --lib message_area -- --nocapture`：通过，9 passed。
- `cargo check -p peri-tui`：通过。
- `cargo fmt --check`：通过。

**验证结果**：用户已在 TUI 中手动回归 `hello` + Enter，确认 CPU 不再 100%，且输出结束后 spinner 消失。

