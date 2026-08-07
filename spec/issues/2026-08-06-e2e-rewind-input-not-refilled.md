# E2E: rewind 回退执行后输入框未回填问题文本

**状态**：Open
**优先级**：高
**类型**：缺陷
**创建日期**：2026-08-06
**来源**：E2E 全量运行（2026-08-06，`e2e/e2e-results-2026-08-06.md` 问题 2）

## 问题描述

`e2e/tests/scenarios/rewind-v2.test.ts` 失败（55s，未重试）。测试链路为「候选 → 预算 → 执行 → 文件删除+回填 → 二次候选更新」，失败点：

- 位置：`tests/scenarios/rewind-v2.test.ts:207`，`expect(r.pass).toBe(true)`（Judge `rewind-after-exec`）
- Judge 反馈：**输入框未回填用户刚发送的问题文本**——"屏幕底部的输入框区域显示的是用户指令文本（'请用 Write 工具创建文件...'）和状态信息（'Bypass · perihelion...'），但未发现回填了用户刚发送的问题文本（如 'hello rewind' 或类似内容）"

## 现状

回退执行后，输入框应回填用户刚发送的问题文本（可见其开头部分）；当前 Judge 判定输入框显示的是指令文本与状态信息，未出现回填内容。可能原因：回填逻辑未触发、回填内容被覆盖、或回填与渲染时序问题。

## 期望改进方向

- 回退执行完成后输入框回填用户刚发送的问题文本，且屏幕可见。
- 与「二次双击 Esc → 候选已更新」链路（P1 #1 history 写回）不冲突。

## 验收标准

- [ ] `npm test -- tests/scenarios/rewind-v2.test.ts` 通过（从 `e2e/` 执行）。
- [ ] 输入框回填文本在 rewind 执行后可见，且后续 rewind 操作不破坏回填。

## 涉及文件

- `peri-tui/src/kit/rewind_candidates.rs` —— 候选管理
- `peri-tui/src/kit/rewind_action.rs` —— rewind 动作
- `peri-tui/src/kit/input_area.rs` —— 输入框回填
- `peri-acp/src/session/command/rewind.rs`、`peri-acp/src/dispatch/rewind.rs` —— rewind 执行与事件
- `e2e/tests/scenarios/rewind-v2.test.ts` —— 场景测试

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-06 | — | Open | agent | E2E 全量运行失败，创建 issue |

## 修复记录

### 2026-08-06 — 修复完成（agent）

**根因**

回填链路本身正常（`RewindCompleted` → 消费 `REWIND_TARGET_TEXT` → `INPUT_RESTORE_TEXT` → InputArea use_effect 写入编辑态），但**回填文本被 `<system-reminder>` 注入块污染**：

1. Bypass 模式下首轮 user 消息会被服务端追加 `<system-reminder>Current permission mode: ...</system-reminder>` 权限通知（`peri-agent/src/session/exec/executor.rs` 的 `permission_mode_notice_if_changed` + agent_input 组装）。
2. `session/rewind-candidates`（`peri-acp/src/dispatch/rewind_candidates.rs`）只排除**纯**系统提醒消息，preview 直接取 content 前 200 字符——带尾部注入的用户消息 preview 含 reminder 文本。
3. TUI 候选 Enter 暂存该 preview 为 `REWIND_TARGET_TEXT`，RewindCompleted 后原样回填 → 输入框显示「用户问题 + \<system-reminder\>…\</system-reminder\>」，Judge 判定这不是"用户刚发送的问题文本"。

**修复内容**

- `peri-acp-types` 新增共享纯函数 `strip_system_reminders`（剥离所有 `<system-reminder>…</system-reminder>` 块，未闭合标签防御性保留剩余文本），作为两端剥离的单一事实源。
- 服务端 `rewind_candidates`：preview 生成前剥离 reminder，剥离后为空的注入消息不进候选（弹窗候选与回填文本均干净）。
- TUI `handle_rewind_completed` 重建 `REWIND_PREVIEW`：原「`contains` 即整条丢弃」改为「剥离后为空才丢弃」——带尾部注入的用户消息剥离后保留，与 `rewind-candidates` 口径统一（避免多轮场景候选不一致）。

**验证结果**

- `cargo build -p peri-acp-types -p peri-acp -p peri-tui` 通过；`cargo clippy -p peri-acp-types -p peri-acp -p peri-tui --all-targets -- -D warnings` 通过。
- 单元测试：`peri-acp-types --lib` 86 通过（含 strip 5 例）；`peri-acp --lib` 307 通过（rewind_candidates 7 例）；`peri-tui --lib` 869 通过（含新增 `test_rewind_completed_rebuild_preview_strips_reminder`）。
- E2E：`npm test -- tests/scenarios/rewind-v2.test.ts` **通过**，耗时约 57.7s（Judge 三项 criteria 全过，输入框回填可见且无系统注入文本）。

**修改文件**

- `peri-acp-types/src/messages/content.rs`（新增 `strip_system_reminders`）
- `peri-acp-types/src/messages/mod.rs`（导出）
- `peri-acp-types/src/messages/content_test.rs`（新增 5 例测试）
- `peri-acp/src/dispatch/rewind_candidates.rs`（preview 剥离 reminder）
- `peri-acp/src/dispatch/rewind_candidates_test.rs`（更新/新增断言）
- `peri-tui/src/kit/acp_events/system.rs`（重建 preview 口径统一）
- `peri-tui/src/kit/acp_events_test.rs`（新增剥离重建测试）
