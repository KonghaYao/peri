> 归档于 2026-08-11，原路径 spec/issues/2026-08-02-rewind-preview-test-uses-ai-message-target.md

# rewind-preview 单测以 AI 消息作为回滚目标，与生产口径不一致

**状态**：Fixed
**优先级**：低
**创建日期**：2026-08-02

## 问题描述

`requests_test.rs` 的 rewind-preview 测试用 `register_session_with_history` 构造三段历史 `[human, ai, human]`，测试断言以 `history[1]`（AI 消息）为目标。而生产 `rewind-candidates` 只返回 Human 消息，AI 消息永远不可能成为回滚目标。测试覆盖的是生产不存在的场景，真实路径（目标为 `history[2]` 的用户消息）未被覆盖。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- `peri-tui/src/acp_server/requests_test.rs` 约 366-383 行：断言目标为 `history[1]`（AI 消息）。
- `register_session_with_history`（约 292 行）构造 `[0]=human, [1]=ai, [2]=human`。
- 生产 `rewind-candidates` 仅返回 Human 消息（与 `system.rs` `handle_rewind_completed` 重建 preview 的口径一致：只保留 user 消息）。
- 后果：测试通过并不代表生产 rewind-preview 路径正确；AI 消息被回滚的回归无法被该测试捕获。

## 复现条件

- **复现频率**：测试始终通过（因为断言目标固定）
- **触发步骤**：运行 `requests_test` 中 rewind-preview 相关用例
- **环境**：单测环境

## 期望改进方向

- 测试目标改为生产实际会返回的 Human 消息（`history[2]`），并补充"目标不存在/仅 AI 消息时返回 not found"的用例。

## 涉及文件

- `peri-tui/src/acp_server/requests_test.rs` —— rewind-preview 测试（约 366-383 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: rewind-preview 测试目标改为 history[2]（Human），补充目标不存在 not found 用例 |
| 2026-08-11 | Fixed | Fixed | agent | 终态确认归档：单测目标对齐生产口径（Human 消息），修复记录见正文 |

## 修复记录

- `peri-tui/src/acp_server/requests_test.rs` `test_rewind_preview_routes_to_dispatch`：目标由 `history[1]`（AI 消息）改为 `history[2]`（Human 消息），与生产口径一致（`rewind-candidates` 只返回 user 消息）；测试注释同步说明。
- 新增 `test_rewind_preview_missing_target_returns_not_found`：目标 id 不存在于 history 时 `handle_request` 返回 Err，错误消息含「未找到目标消息」（复用现有 `err.message.contains(...)` 断言风格）。
- 说明：「仅 AI 消息」目标在生产 `rewind_preview` 中按 id 定位、不区分 role（AI 消息 id 实际可命中），UI 侧由候选层 user-only 保证不可达；本用例以不存在的 id 覆盖 not found 异常路径。未改 `peri-acp/src/dispatch/rewind.rs`（与另一 issue rewind-missing-target-emits-compact-error 范围重叠，避免冲突）。
- 验证：`cargo check -p peri-tui --all-targets` 通过；`cargo test -p peri-tui --lib rewind_preview` 3 个用例全过（含修改与新增），`--lib rewind` 36 个用例全过。
