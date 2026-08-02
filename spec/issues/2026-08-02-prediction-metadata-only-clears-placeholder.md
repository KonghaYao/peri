# 仅元数据（SetTitle/AddTag）的 prediction 会以空文本覆盖输入区占位内容

**状态**：Open
**优先级**：低
**创建日期**：2026-08-02

## 问题描述

ACP server 在 agent 完成后发起 prediction 请求，客户端收到 `peri/prediction_ready` 后用 `text` 字段覆盖输入区占位。当 prediction 只产出元数据动作（`SetTitle`/`AddTag`，无 `Placeholder`）时，服务端 `text` 为空字符串，客户端 `handle_prediction` 用空文本覆盖已有的占位内容（如 `!` 或上次输入），占位被清空。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- `peri-tui/src/acp_server/mod.rs`（prediction 完成分支）：`text` 取首个 `Placeholder` 动作，取不到时 `unwrap_or_default()` 得空字符串，随通知发送。
- `peri-tui/src/kit/acp_events/system.rs` `handle_prediction`：`PredictionState.text` 直接取 `p.text`，`*PREDICTION.state().write() = ...` 无条件覆盖。
- 结果：仅 SetTitle/AddTag 的 prediction 把输入区占位（含上次预测内容）擦为空。
- 服务端（mod.rs 约 237-330 行）与客户端（system.rs 约 62 行）均无空文本保护。

## 复现条件

- **复现频率**：prediction 只返回 SetTitle/AddTag 时必现
- **触发步骤**：
  1. 输入区已有预测占位内容（或非空输入）
  2. 完成一轮对话，prediction 仅产出元数据动作（如模型只给标题不给占位）
  3. 观察输入区占位被清空
- **环境**：prediction caps 开启的会话

## 期望改进方向

- 客户端：`handle_prediction` 中 `text` 为空时保留现有占位（不覆盖）。
- 服务端：无 Placeholder 时也可发送空 actions 通知或跳过 text 字段，避免误导。

## 涉及文件

- `peri-tui/src/acp_server/mod.rs` —— prediction 通知组装（text 取首个 Placeholder）
- `peri-tui/src/kit/acp_events/system.rs` —— `handle_prediction` 覆盖 `PREDICTION` 状态

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: 客户端 handle_prediction 空 text 时保留现有占位，不再清空输入区 |

## 修复记录

- `peri-tui/src/kit/acp_events/system.rs` `handle_prediction`：组装 `PredictionState` 后若 `text` 为空（prediction 仅含 SetTitle/AddTag 等元数据动作），从现有 `PREDICTION` 状态读回 `text` 再写入——占位不被空文本覆盖；`received_at`/`summary` 语义不变。
- 服务端（`mod.rs` prediction 分支）未改动（issue 标注为可选；空 text 通知现在对客户端无副作用）。
- 验证：`cargo check -p peri-tui --all-targets` 通过。
