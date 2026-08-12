> 归档于 2026-08-11，原路径 spec/issues/2026-08-02-rewind-missing-target-emits-compact-error.md

# rewind 目标缺失时复用 CompactError 事件，TUI 显示压缩错误提示

**状态**：Fixed
**优先级**：低
**创建日期**：2026-08-02

## 问题描述

rewind 相关失败（目标消息不存在、参数解析失败）通过 `ExecutorEvent::CompactError` 上报，TUI 走 `handle_compact_error` 渲染压缩（compact）专属的系统提示。用户看到的是与回退无关的错误文案，误导排障。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- `peri-acp/src/dispatch/rewind.rs` 约 58-66 行：目标消息未找到时 `push_event(..., ExecutorEvent::CompactError { message })`。
- 约 118 行注释：`RewindCommand 内部解析失败只发 CompactError 事件`——两条路径均复用。
- TUI 侧 `peri-tui/src/kit/acp_events/compact.rs` 约 98 行：CompactError 进入 compact 专属处理，注入压缩语境提示。

## 复现条件

- **复现频率**：异常路径（目标 id 失效/解析失败）
- **触发步骤**：
  1. 调用 `session/rewind-preview` 或 `session/rewind`，目标 message id 不存在
  2. 观察 TUI 出现压缩（Compact）相关错误提示
- **环境**：任意回退失败场景

## 期望改进方向

- 新增 rewind 专属错误事件（或使用已有 rewind 错误文案路径），两个失败点均改用它，保留 warning 日志与 AcpError 响应。

## 涉及文件

- `peri-acp/src/dispatch/rewind.rs` —— 两处 CompactError 上报（约 58-66、118 行）
- `peri-tui/src/kit/acp_events/compact.rs` —— CompactError 处理（约 98 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: 新增 ExecutorEvent::RewindError + AcpEvent::RewindError 全链路事件，rewind 两个失败点改用它，TUI 渲染 rewind 专属提示 |
| 2026-08-11 | Fixed | Fixed | agent | 终态确认归档：rewind 失败改用独立错误事件/文案，修复记录见正文 |

## 修复记录

- **方案**：新增 rewind 专属错误事件（issue 期望方向第一条），全链路覆盖（ARC-EVENT-001）：
  - `peri-agent/src/agent/events.rs`：新增 `ExecutorEvent::RewindError { message }` 变体（RewriteCompleted 旁）；
  - `peri-acp/src/session/command/rewind/events.rs`：`emit_rewind_parse_error` / `emit_rewind_not_found` 改发 `RewindError`；
  - `peri-acp/src/dispatch/rewind.rs:61`：目标未找到改发 `RewindError`；第 118 行注释同步更新；
  - `peri-acp/src/event/mod.rs`：AcpEvent DTO 新增 `RewindError { message }`（serde tag `rewind_error`）；
  - `peri-acp/src/session/event_sink.rs`：ExecutorEvent → AcpEvent 映射加分支；
  - `peri-acp/src/langfuse/bridge.rs`：`RewindError` 归入 `=> None`（不再误报为 CompactEnded is_error）；
  - TUI：`acp_types.rs`（AcpEventData 变体）、`acp_notifier.rs`（convert_agent_event 分支）、`acp_events/mod.rs`（dispatch 分支）、`acp_events/system.rs`（`handle_rewind_error`，Warning 级 SystemNote「回退失败: …」）、`acp_bridge.rs`（event_name 分支）；
  - i18n：`en/main.ftl` + `zh-CN/main.ftl` 新增 `app-note-rewind-error`。
- **测试**：`rewind_test.rs` 两个用例改名并断言 `rewind_error`；`events_test.rs` 加 serde round-trip；`mapper_test.rs` 加 no-session-update 断言。
- **验证**：`cargo check -p peri-agent --all-targets` 与 `cargo check -p peri-tui --all-targets` 通过；`cargo test -p peri-agent --lib -- rewind_error`（1 passed）、`cargo test -p peri-acp --lib -- rewind`（49 passed）、`cargo test -p peri-tui --lib -- acp_`（95 passed）全部通过。
- **理由与风险**：CompactError 语义被 compact 路径（`compact_test.rs` 断言）与 langfuse 遥测依赖，rewind 复用会使 langfuse 将 rewind 失败记为「压缩失败」；新增变体不改任何既有语义。风险：AcpEvent 序列化增加变体，旧版 TUI 反序列化 `rewind_error` 会落到 `Unknown` fallback（`acp_types.rs` decode 有 forward-compat），同仓库同步发布风险可控。

