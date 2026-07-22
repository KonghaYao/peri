# 消息流偶发渲染不全，下一事件后才恢复


> 归档于 2026-07-20，原路径 spec/issues/2026-07-07-message-stream-partial-render-missing-until-next-event.md
**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-07

## 问题描述

TUI 消息流有时会出现部分内容渲染不全：消息已经处在输出过程中，但界面上少显示一段或几段内容。该现象不固定发生在单一阶段，文本 streaming、工具更新、SubAgent 输出等消息流阶段都可能出现。用户观察到缺失内容通常会在下一次事件后恢复，因此希望增加一个强制刷新命令，并调查是否存在瞬时变化被节流忽略的问题。

## 症状详情

| 维度 | 观察到的现象 |
|------|--------------|
| 表现 | TUI 上部分消息内容缺失，像是某次渲染没有完整反映最新消息流状态 |
| 触发阶段 | 不固定；文本 streaming、工具调用更新、SubAgent 输出等阶段都可能出现 |
| 恢复方式 | 下一次事件后恢复，例如后续输入、工具事件、窗口 resize 或滚动触发后内容补出来 |
| 用户期望 | 提供一个手动强制刷新 command；同时调查渲染不全是否由瞬间状态变化被节流忽略导致 |

## 功能需求

新增一个调试刷新命令：`/debug-refresh`。

期望行为：

1. 用户在 TUI 中输入 `/debug-refresh`。
2. TUI 触发一次强制刷新/重绘。
3. 如果当前存在因渲染事件漏发、缓存未更新或节流导致的显示不全，命令执行后应尽可能让界面恢复到当前真实消息状态。

新增一个调试导出命令：`/debug-export-text [all|screen]`。

期望行为：

1. 用户在 TUI 中输入 `/debug-export-text` 或 `/debug-export-text all` 时，导出当前全量消息文本。
2. 用户在 TUI 中输入 `/debug-export-text screen` 时，只导出当前消息区可见范围文本。
3. `all` 是默认模式；未知参数暂按 `all` 处理。
4. 导出文件默认写入当前工作目录。
5. 文件名以时间命名，格式为 `peri-debug-export-YYYYMMDD-HHMMSS.txt`。
6. 导出完成后，TUI 状态栏显示导出文件路径；失败时显示错误信息。

该命令用于辅助排查“屏幕上渲染不全但数据是否已经存在”的问题：

- `all` 可确认 `RENDER_CACHE` 中全量文本是否完整。
- `screen` 可确认当前 viewport 裁剪后的可见文本是否与屏幕显示一致。

## 调查方向

需要调查消息流渲染不全是否与以下现象有关：

- 消息状态发生瞬时变化，但变化被节流逻辑忽略。
- render bridge / render cache 没有在某些消息流更新后及时刷新。
- TUI 已经有新数据，但缺少一次重绘触发，直到下一事件到来才补齐。

## 复现条件

- **复现频率**：偶发
- **触发步骤**：
  1. 在 TUI 中进行一次会产生消息流更新的操作。
  2. 观察文本 streaming、工具调用更新或 SubAgent 输出过程。
  3. 偶尔会看到部分内容没有完整显示。
  4. 等待下一次事件（例如继续输入、工具事件、resize、滚动）后，缺失内容可能恢复。
- **环境**：本地 Peri TUI；具体模型和配置待补充。

## 涉及文件

- `peri-tui/src/kit/render_bridge.rs` —— TUI 渲染预计算与 `RENDER_CACHE` 更新链路。
- `peri-tui/src/kit/message_area.rs` —— 消息区基于 `RENDER_CACHE` 的视口裁剪与展示；记录当前可见行范围供 `/debug-export-text screen` 使用。
- `peri-tui/src/kit/acp_bridge.rs` —— ACP 事件进入 TUI atom 状态的桥接层。
- `peri-tui/src/kit/entry.rs` —— TUI 主入口与全屏运行路径，可能涉及命令事件分发。
- `peri-tui/src/kit/submit_consumer.rs` —— 视图层命令拦截；实现 `/debug-refresh`、`/debug-export-text` 等不经过 agent 的调试命令。
- `peri-tui/src/kit/atoms.rs` —— TUI 全局状态；可保存消息区 viewport 快照等调试状态。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-07 | — | Open | agent | 创建 |
| 2026-07-07 | Open | Open | agent | 追加 `/debug-export-text [all|screen]` 调试导出命令需求 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
