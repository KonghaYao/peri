# Rewind v2：历史驱动回溯 — 设计文档

**日期**：2026-08-01
**状态**：待评审
**范围**：`peri-acp`（服务端）+ `peri-tui`（TUI）

## 1. 背景与问题

现有 rewind（双击 Esc 回退）整条链路存在多处偏差：

1. **候选依赖服务端推送**：`"rewind-preview"` 事件由服务端每个 turn 完成后推送（2026-08-01 修复加入），TUI `REWIND_PREVIEW` atom 被动接收。理念偏差——候选应**直接由历史判断**，打开面板时实时查询，而非等待推送。
2. **候选内容偏差**：推送包含全部消息（user + assistant），而回退节点应只针对 **user 消息**（排除 system note / `<system-reminder>` 系统注入）。
3. **无输入框回填**：回退后目标 user 消息的文本应返回输入框（用户修改后重发），现状不回填。
4. **无文件回退预算**：Enter 确认直接执行，用户不知道将撤销哪些 Write/Edit 文件改动；应**先展示预算**，无文件改动时直接回退。
5. **确认 RPC 断链**（探索发现）：`session/execute-command` 在 TUI 进程内 ACP server（`peri-tui/src/acp_server/requests.rs` 的 `handle_request` match）中**未注册**，`rewind_consumer` 发送的确认 RPC 会命中 `_ => Err("Method not found")`——即使弹窗有候选，确认也无法执行。新设计用专用 RPC 绕开。

## 2. 设计决策（已与用户确认）

| # | 决策 | 结论 |
|---|---|---|
| D1 | 回退后目标消息去留 | **删除并回填输入框**——目标及之后全部移除，目标文本回填输入框（替换草稿），用户修改重发 |
| D2 | 文件复原精确度 | **最佳努力（现状）**——Edit 反替换（`replacen` new→old）、Write 删文件 + `git checkout HEAD`；不引入执行前备份 |
| D3 | 弹窗候选展示 | **只列 user 消息**（截断文本），去掉 files 双视图 |
| D4 | 候选获取方式 | **打开面板时查询一次**（RPC），不做每轮推送同步 |
| D5 | 回填交互 | **回填并自动聚焦输入框**（复用 `INPUT_RESTORE_TEXT` + `RENDER_HEARTBEAT` 通道） |
| D6 | 预算流程 | Enter 候选后**先查询文件回退预算**；预算为空直接执行，非空则展示（弹窗切换文件列表 + Enter 确认 / Esc 取消），确认后执行 |
| D7 | 持久化删除 | `ThreadStore` 无软删除接口（仅 `delete_messages` / `delete_messages_since`，均为硬删除）→ **维持硬删除**，文档注明 |

## 3. 协议设计（3 个新 RPC）

替代"每轮推送 rewind-preview"。均注册于 `peri-tui/src/acp_server/requests.rs` 的 `handle_request` match，响应为同步 JSON-RPC 结果。

### 3.1 `session/rewind-candidates` — 候选查询

打开面板时调用一次（双击 Esc 触发）。

```json
// 请求
{ "sessionId": "<sid>" }
// 响应
{
  "messages": [
    { "id": "<message-uuid>", "preview": "<前 200 字符>" },
    ...
  ]
}
```

**服务端规则**：从 session history 提取 **user 消息**（`BaseMessage::Human`），排除文本包含 `<system-reminder>` 的系统注入消息（与 TUI `ReminderInfo` 检测口径一致）。按历史顺序返回，id 为服务端权威 `MessageId`。

### 3.2 `session/rewind-preview` — 文件回退预算（只读）

Enter 候选后调用。

```json
// 请求
{ "sessionId": "<sid>", "target_message_id": "<id>" }
// 响应（预算非空）
{
  "file_changes": [
    { "path": "src/main.rs", "kind": "edit" },
    { "path": "new_file.txt", "kind": "write" }
  ]
}
```

**服务端规则**：定位目标消息 → 提取目标**之后**（含目标）被移除消息中的 Write/Edit 工具调用（兼容 OpenAI `tool_calls` 与 Anthropic `ContentBlock::ToolUse` 两种格式，复用 `RewindCommand::extract_file_changes` 逻辑）→ 按时间逆序返回。**只读，不修改任何状态**。目标不存在 → 返回错误（复用 `emit_rewind_not_found` 的 CompactError 事件 + 错误响应）。

### 3.3 `session/rewind` — 执行回退

```json
// 请求
{ "sessionId": "<sid>", "target_message_id": "<id>" }
// 响应
{ "status": "executed" }
```

**服务端规则**：复用 `RewindCommand::execute` 现有逻辑——定位 → 截断（目标及之后移除）→ Write/Edit 最佳努力反向复原 → ToolUse/ToolResult 配对校验 → 持久化硬删除（`thread_store.delete_messages`）→ 发送 `RewindCompleted { summary, messages }` 事件（TUI 刷新链路不变）→ 响应 `{ status: "executed" }`。

## 4. 数据流

```
用户双击 Esc
  → event_handlers: open_popup(Rewind) + spawn 查询任务
  → RPC session/rewind-candidates
  → 响应写 REWIND_PREVIEW atom → 弹窗渲染
     · 查询未返回: 显示"加载中…"
     · 空列表:    显示"无可回退"
     · 非空:      显示 user 消息列表（截断文本）

Enter 选择候选
  → 目标文本暂存 REWIND_TARGET_TEXT atom
  → RPC session/rewind-preview（预算）
  → 预算为空: 直接 RPC session/rewind → 执行
  → 预算非空: 弹窗切换为文件列表视图（path + kind + "确认回退？"）
              · Enter → RPC session/rewind → 执行
              · Esc   → 回到候选列表（并清 REWIND_TARGET_TEXT）

执行完成
  → RewindCompleted 事件 → handle_rewind_completed:
      1. 重建 committed（现有逻辑）
      2. 重建候选（现有逻辑，从 messages_json 提取）
      3. 消费 REWIND_TARGET_TEXT → INPUT_RESTORE_TEXT + RENDER_HEARTBEAT++
         （复用 TurnInterrupted 回填通道，InputArea use_effect 写入编辑态）
      4. 焦点回输入框（close_popup 已关弹窗）
```

## 5. 组件改动清单

### 服务端（peri-acp）

| 文件 | 改动 |
|---|---|
| `peri-acp/src/dispatch/rewind_candidates.rs`（新） | 候选查询：从 history 提取 user 消息（排除系统注入）→ `{ messages: [{id, preview}] }` |
| `peri-acp/src/dispatch/rewind.rs`（新） | 预算（只读提取 Write/Edit）+ 执行（调用 `RewindCommand` 或复用其内部逻辑） |
| `peri-acp/src/session/command/rewind.rs` | `extract_file_changes` / `parse_tool_call` 提取为 `pub(crate)` 供 dispatch 复用（预算阶段只读调用） |
| `peri-tui/src/acp_server/requests.rs` | 注册 `session/rewind-candidates` / `session/rewind-preview` / `session/rewind` 三个分支 |
| `peri-acp/src/session/executor_helpers.rs` | **删除** Phase 8.7 推送块 + `build_rewind_preview_payload`（2026-08-01 修复的推送逻辑整体退役） |
| `peri-acp/src/session/executor.rs` | 移除 `build_rewind_preview_payload` re-export |
| `peri-acp/src/session/executor_test.rs` | 删除 `build_rewind_preview_payload` 相关测试 |

### TUI（peri-tui）

| 文件 | 改动 |
|---|---|
| `peri-tui/src/kit/rewind_candidates.rs`（新） | 候选查询 RPC 发送 + 响应解析为 `Vec<RewindCandidate{id, preview}>`（event_handlers 双击 Esc 时 spawn 调用，不经过 RewindAction channel） |
| `peri-tui/src/kit/rewind_action.rs` | `RewindAction` 扩展：`Preview { target_message_id, target_text }`（候选 Enter，触发预算）/ `Confirm { target_message_id }`（预算确认，执行）；consumer 处理两段式流程 |
| `peri-tui/src/kit/atoms.rs` | 新增 `REWIND_TARGET_TEXT: AtomStatic<Option<String>>` |
| `peri-tui/src/kit/popups/rewind_popup.rs` | 候选列表（只 user 消息，截断）+ 加载态 + 空态；预算确认视图（文件列表 + Enter/Esc）；Enter 触发预览流程 |
| `peri-tui/src/kit/acp_events/system.rs` | `handle_rewind_preview` 退役（无生产者）；`handle_rewind_completed` 增加输入框回填（消费 REWIND_TARGET_TEXT → INPUT_RESTORE_TEXT + heartbeat），候选重建逻辑保留（现有，从 messages_json 提取） |
| `peri-tui/src/kit/event_handlers.rs` | 双击 Esc：open_popup + spawn 候选查询 |
| `peri-tui/src/kit/submit_consumer.rs` / `thread_load_consumer.rs` | 会话边界（/clear、thread 切换）同时清 `REWIND_TARGET_TEXT` |

## 6. 错误处理

| 场景 | 行为 |
|---|---|
| 候选查询 RPC 失败 | 弹窗显示错误文案，Esc 关闭；不阻塞其他功能 |
| 预算查询目标不存在 | 服务端发 CompactError 事件（现有 `emit_rewind_not_found`）+ 错误响应 → TUI 通知 + 关闭弹窗，清 REWIND_TARGET_TEXT |
| 执行 RPC 失败 | TUI 通知；REWIND_TARGET_TEXT 清理（rewind_consumer 失败路径） |
| 持久化删除失败 | 现有 `revert_warnings` → summary 警告（不改） |
| 弹窗 Esc（候选列表 / 预算视图） | 清 REWIND_TARGET_TEXT（若已暂存） |

## 7. 边界情况

- **无 user 消息**（首轮未完成 / `/clear` 后）：候选空 → "无可回退"
- **compact 压缩后**：候选 = 当前可见 history（服务端权威提取，与 TUI 消息区一致）；id 精确匹配，无序号漂移问题
- **重复文本**：按消息 id 定位，无歧义
- **弹窗打开期间消息流继续**：候选为查询时刻快照；用户可关闭重开刷新（不做实时刷新）
- **`<system-reminder>` 注入消息**（bg 结果等）：user 消息但非用户输入，不进入候选（TUI `ReminderInfo` 与服务端文本检测双口径一致）

## 8. 测试计划

### 服务端
- `dispatch/rewind_candidates`：user-only 提取、系统注入排除、空历史 → 空列表、preview 截断 200 字符
- `dispatch/rewind`（预算）：Write/Edit 提取（两种消息格式）、目标定位失败、预算逆序
- `dispatch/rewind`（执行）：截断边界、持久化删除调用、RewindCompleted 事件（复用 `rewind_test.rs` 模式）
- `requests.rs`：三个新方法路由分支

### TUI
- `rewind_popup`：加载态 / 空态 / 候选列表（只 user）/ 预算视图切换 / Enter-Esc 流程
- `rewind_action`（consumer）：三个 RPC 的参数序列化、失败路径清 REWIND_TARGET_TEXT
- `acp_events`：`handle_rewind_completed` 回填输入框（INPUT_RESTORE_TEXT + heartbeat）；删除 `handle_rewind_preview` 相关旧测试
- `executor_test`：删除 payload 测试（随服务端退役）

## 9. 非目标（本设计不包含）

- 修复 `session/execute-command` 未注册问题（HITL/AskUser 是否受影响需另行核查——`docs/design/peri-tui-architecture.md` 记载其走 execute-command，当前 requests.rs 无对应分支，为观察点）
- 精确文件复原（执行前备份机制）——D2 已否决
- 软删除支持——ThreadStore 无此能力，D7 维持硬删除
- 弹窗实时刷新、多选回退、回退到 system note

## 10. 验证方式

- `cargo test -p peri-acp --lib`（rewind 相关 + 全量）
- `cargo test -p peri-tui --lib`（rewind/acp_events 相关）
- `cargo clippy --workspace --all-targets -- -D warnings`
- 手动：真实 TUI 对话数轮 → 双击 Esc → 候选为 user 消息 → Enter → 有 Write/Edit 时见预算 → 确认 → 消息回退 + 文本回填输入框 + 焦点在输入框
