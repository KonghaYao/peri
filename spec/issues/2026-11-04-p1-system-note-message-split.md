# P1：系统通知在流式输出中途切断 assistant 消息

**状态**：已确认，待修复。
**优先级**：P1。
**类型**：错误。
**创建日期**：2026-11-04。
**来源**：用户报告（TUI 截图，2026-11-04）。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `19f19e221`）。

## 确认证据

- 用户截图：一条 assistant 消息「…消除流式回弹；3. 面板滚轮…」被两段 `MCP: xxx connected (N tools)` 通知与一行 `Thought for 0s · 0 lines` 从中间打断，显示为「消除」→ 通知 →「流式回」→ 通知 →「弹； 3. 面板…」；`3.`/`4.` 未形成有序列表，退化为普通文本。未运行时复现；证据为截图 + 以下代码链路。
- `push_system_note` 无条件冻结当前文本段：`peri-tui/src/kit/acp_types/current_turn/streaming.rs:166-175` → `flush_text_segment`（`:12-47`）。
- 投影层每个 `AssistantText` 段独立构建 `TuiAssistantBubble`：`peri-tui/src/kit/acp_types/current_turn/projection.rs:171-213`；`SystemNote` 独立构建：`:276-289`。同一 ACP message 因此变成多个气泡，后续片段从零开始解析 Markdown。
- MCP 状态通知生产：`peri-middlewares/src/mcp/client/status.rs:345-366`（`status_change_text` + notifier）；中间件发布 `SystemNotification`：`peri-middlewares/src/mcp/middleware.rs:357-395`。
- TUI 消费：`peri-tui/src/kit/acp_events/system.rs:250-259` → `inject_system_note`（`peri-tui/src/kit/acp_events/mod.rs:595-601`）。
- 通知由独立后台事件泵投递（`peri-agent/src/session/executor_helpers/v2_execute.rs:290-317`），与文本 chunk 无同步屏障，可在单条消息中间到达。

## 根因链

MCP 连接在后台异步完成 → `SystemNotification` 与流式 `TextChunk` 在同一 TUI 事件队列中按到达顺序交错 → `handle_system_notification` 立即注入 → `push_system_note` 把当前未完成文本 flush 成独立段，再插入 `SystemNote` 段 → 投影层为每个 `AssistantText` 段单独生成气泡 → 后续文本片段丢失 Markdown 上下文（有序列表断裂、句子被截断）。

## 影响面

- 用户影响：一条完整回复被系统通知切成多段，列表/代码块/段落连续性丢失；无需用户操作即可触发（后台 MCP 连接与首次回复并发）。无数据丢失。
- 受影响调用点：所有流式期间到达的 `SystemNote`（MCP 状态、budget 警告、LLM 重试、cache 覆盖、命令反馈、compact 完成等，均经 `inject_system_note`）。根级 `SystemReminder` 的 `flush_current_turn` 路径为同类问题，但不在本次范围。
- 是否可规避：无可靠规避；等待 MCP 连接完成再输入仅降低概率。

## 修复方案

- 方向：`SystemNote` 不得插入正在增长的 assistant 消息内部。流式期间到达的 note 进入 `CurrentTurn` 待落段队列；在当前消息边界（message_id 变化、tool/subagent 开始、user bubble、reminder、turn 结束）冻结文本段后再落为 `SystemNote` 段。无流式文本时保持现有立即注入行为。
- turn 结束不得为落 note 而 flush trailing 文本（会丢失正文时长）：`archive_current_turn_fully` 先取包含 trailing 气泡的 `view_models()` 追加到 committed，再依序追加待落 note，最后 reset；`register_live_seal_segments` 覆盖新增范围。
- 待落 note 计入 `is_empty` 与 `preview_memory_bytes`；`reset` 清空；缓存尾部在 drain 时必须先随文本段冻结一起截断，保持 `segments[i] ↔ cached_view_models[i]` 对齐。
- 范围：`peri-tui/src/kit/acp_types/current_turn.rs`、`current_turn/streaming.rs`、`peri-tui/src/kit/acp_events/mod.rs`、回归测试、`docs/code-index/peri-tui.md`。
- 明确不做：不修改 MCP notifier/ACP 事件层；不做渲染层相邻段合并；不处理根级 `SystemReminder` 的整轮切割（另记）。
- 回归测试：流式文本 + 两条 note 后仍只有一个气泡；note 在 message_id 边界与工具前按 FIFO 落位；TurnDone / TurnSuspended / TurnInterrupted 归档后 note 可见且顺序正确；空 turn 立即注入行为不变；缓存与时长契约不回归。

## 验收标准

1. 流式文本中连续注入两条 `SystemNotification` 后继续文本：`current_turn.view_models()` 只产生一个 `TuiAssistantBubble`；TurnDone 后 committed 顺序为完整气泡 → note1 → note2，且气泡文本连续。
2. 目标测试与 `cargo check` 通过；正文 `duration_ms` 冻结行为不变。

## 验证记录

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- kit::acp_events::acp_events_test::streaming_test` → 19 passed，0 failed；新增 7 条 SystemNote 用例全部通过。
- CodeRabbit 意见（成立）：`AgentExecutionFailed` 是终态，排队中的错误 note 不能依赖后续 `TurnDone` 才可见；`agent.rs` 在注入后立即归档，并新增回归用例。该路径同时暴露 `is_empty()` 未计入尚未投影的 `segments`，已补齐。
- `... -- kit::acp_events::acp_events_test` → 291 passed，0 failed，4 ignored。首跑出现一次 `history_window_page_test::accepted_page_with_commit_receipt_seals_immediately` 失败；该用例隔离运行通过，随后连续 3 次全模块运行均通过，且不涉及本改动路径。
- `./scripts/check-changed.sh` → `cargo check --locked -p peri-tui` Finished，无错误。
- `cargo fmt -p peri-tui -- --check` → 通过。
- `git diff --check` → 通过。
- 未做真实终端复现：触发依赖后台 MCP 连接与首次回复并发；已用 VM 级确定性回归测试覆盖同一事件序列。
