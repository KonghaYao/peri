> 归档于 2026-08-11，原路径 spec/issues/2026-08-05-bg-command-expect-panic-via-rpc.md

# BgCommand 双 expect 可经公开 RPC session/execute-command 触发 panic

**状态**：Fixed
**优先级**：中
**创建日期**：2026-08-05
**关联计划**：`2026-08-05-core-flow-bugfix-plan.md` S1.2

## 问题描述

`BgCommand` 对 `bg_event_sender` 与 `bg_registry` 两个字段使用 `expect("bg_event_sender 总是 Some")`。内部 `intercept_immediate_command`（`executor_helpers.rs:79-80`）确实总是传 `Some`，但 `session/execute-command` 与 `session/rewind` 是面向 IDE/stdio 的**公开 ACP 方法**，参数为 `Option`——任何外部调用方传 `None` 后执行 `/bg`，expect 直接 panic。panic 在 async context 传播到 RPC handler，若无 catch_unwind 会崩掉整个 server task（比单请求 panic 严重）。

## 症状详情

- 第一个 expect：`bg.rs:91-93`（`bg_event_sender`）
- **第二个 expect 同文件 `:94-96`（`bg_registry`），易被遗漏**
- `dispatch/execute_command.rs:93-110` 两个字段均为 Option 直传，无校验

## 复现条件

- **复现频率**：仅在外部客户端调用时（当前无 in-tree 调用者，属潜在路径）
- **触发步骤**：
  1. 外部客户端经 `session/execute-command` 调用 `/bg`
  2. 参数中 `bg_event_tx`/`bg_registry` 传 `None`
- **环境**：stdio/IDE 客户端

## 涉及文件

- `peri-acp/src/session/command/bg.rs:91-96` —— 两个 expect
- `peri-acp/src/session/dispatch/execute_command.rs:35-53` —— Option 参数入口
- `peri-acp/src/session/dispatch/rewind.rs:109-117` —— 同构入口

## 修复方向（对抗 review 已确认）

- 两个 expect 均改为优雅降级：`bg_event_sender` 是 `spawn_background_fork` 的必需项，降级 = `.ok_or(...)` 返回 RPC 错误（合理语义）
- 或把 `CommandContext.bg_event_sender/bg_registry` 改为非 Option，在入口强制校验并返回 RPC 错误
- 同类隐患：`workflow_agent.rs:214-256` 的 std `Mutex::lock().unwrap()`（低概率，可顺带评估）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（peri-acp 审查发现，对抗 review 补充第二个 expect） |
| 2026-08-05 | Open | Fixed | agent | 修复：bg.rs 两个 expect 改为优雅降级（emit 错误提示 + EndTurn 返回，不 panic）；新增 RPC 直调缺省上下文测试 |

## 修复记录

### 修复 #1（2026-08-05）

- **操作人**：agent（Slice 1 编码切片，auto-devflow）
- **用户原意**：`session/execute-command` / `session/rewind` 是公开 ACP 方法，参数为 Option，外部调用方传 None 后执行 `/bg` 不应 panic 崩掉 server task
- **修复内容**：
  - **文件**：`peri-acp/src/session/command/bg.rs`（:91-96 区域）
    - 两个 `expect("...总是 Some")` 改为 `let-else` 优雅降级：缺失时经 `emit_bg_spawn_error` 推送明确错误提示（分别指明缺失字段 `bg_event_sender` / `bg_registry`），返回 `CommandResult { stop_reason: EndTurn }`，不 panic
    - 错误信息说明 RPC 直调缺少后台事件通道/注册中心、/bg 需经 executor 内部路径执行
    - `bg_event_sender` / `bg_registry` 是 `spawn_background_fork` 的必需项，无合理 fallback，报错返回是唯一正确语义（issue 修复方向确认）
  - **测试**：`peri-acp/src/session/command/bg_test.rs` 新增 `test_bg_command_missing_bg_context_gracefully_fails`（有效 provider 越过 LLM 构造检查 + 两字段 None，断言不 panic、EndTurn、emit 含 "bg_event_sender" 的错误提示）
  - **顺带评估**（未改动）：`workflow_agent.rs:214-256` 的 `Mutex::lock().unwrap()` 低概率同类隐患，不在本 slice 范围
- **验证状态**：已验证（L1 复验 2026-08-05：`cargo test -p peri-acp --lib bg` 12 通过，含 test_bg_command_missing_bg_context_gracefully_fails——两字段 None 时 emit 错误提示 + EndTurn 返回，不 panic）
