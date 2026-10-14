# E — TUI + SDK 现状：session/load 重放与 Compact

范围：`peri-tui/src/kit/**`、`peri-tui/src/acp_client/**`、`npm-packages/@peri-sdk/src/**`，
以及为坐实数据流而读的 `peri-acp/src/dispatch/session_replay.rs`、`peri-acp/src/host/requests/session_restore.rs`、
`peri-acp-types/src/peri_caps.rs`。全部为当前检出（worktree `peri-transcript-history`）实读代码。

## Summary

1. **TUI 的“compact 后强制重放”是唯一存在的强制 session/load 路径**：`handle_turn_done` 中
   `compact_just_completed` 置位时向 `THREAD_LOAD_TX` 发 session_id（`turn.rs:43-53`），
   consumer 同步取 reservation 后调 `AcpTuiClient::load_session`（`thread_load_consumer.rs:60-66,163-182`）。
2. **TUI 安装重放历史 = “先整段清空再逐条追加”**（reset + append），不存在合并逻辑：
   `project_session_boundary` 递增 `BRIDGE_RESET_COUNTER`（`session_boundary.rs:15`）→
   `apply_bridge_reset` 把 `committed` 置空（`acp_bridge.rs:405`）+ `push_view_models_for_reset()`
   把 `VIEW_MODELS` 置空快照（`render.rs:763-785`）→ 重放事件用 `committed.push_back` 追加
   （`turn.rs:451`、`tool.rs:186`、`acp_events/mod.rs:249-254`）。
3. **TUI 重放通道丢弃消息身份**：assistant/thought replay 合并成无 id 的 `CommittedAssistantText`
   （`session_update.rs:130-142,155-167`；`turn.rs:442` 显式 `message_id: None`），
   user bubble 结构体本身没有 id 字段（`tui_render_unit/bubble.rs:5-13`）；只有 tool 的 `toolCallId` 被保留
   （`session_update.rs:169-202`）。
4. **SDK 不消费 compact 触发的 session/load**；同一语义走 `peri/agent_event` 的
   `compact_completed.messages_json` → `ChatProjection.replaceHistory`（`session-docs.ts:132-137`、
   `chat-projection.ts:134-176`），是真正的“整表替换 + 重建”。
5. **SDK 在 replay 通知里依赖两项身份**：`update.messageId`（条目/轮身份）与 `update._meta.periReplay`
   （replay 语义开关），并由自己声明 `peri.replay: true` 让服务端打这个标记
   （`session.ts:117-128`、`peri_caps.rs:84`、`session_replay.rs:220-224`）。

---

## Findings

### 1. 触发链：手动 Compact → TurnDone → 强制 session/load 重放

前置：手动 compact 的事件置位。

- `pub(super) fn handle_compact_completed(state, summary, trigger, strategy, ...)`
  `peri-tui/src/kit/acp_events/compact.rs:15-24`；`compact.rs:35-37`：
  ```rust
  if trigger == "manual" {
      state.compact_just_completed = true;
  }
  ```
  同时把展示文案写入 `PENDING_COMPACT_NOTE`（`compact.rs:81-83`，atom 定义 `atoms.rs:625`）。
- `trigger` 的来源：`CompactTrigger::{Auto,Manual}`（`peri-acp-types/src/event.rs:410-414`），
  v2 手动 pipeline 发 `CompactTrigger::Manual`（`peri-agent/src/session/exec/compact_pipeline.rs:255-268`）。
- 内部 auto-compact 后紧跟流事件，`dispatch_for_bridge` 入口用流事件清单清 flag
  （`acp_events/mod.rs:328-348`，含 `TextChunk|ReasoningChunk|ToolStarted|ToolEnded|SubagentStarted|ToolCount|Progress`）。

触发点（唯一）：`pub(super) fn handle_turn_done(state: &mut BridgeState)`
`peri-tui/src/kit/acp_events/turn.rs:10`；`turn.rs:43-53`：
```rust
if state.compact_just_completed {
    state.compact_just_completed = false;
    if let Some(tx) = THREAD_LOAD_TX.get() {
        let session_id = state.active_session_id.clone();
        let _ = tx.send(session_id);
    }
}
```
（`turn.rs:38-42` 注释说明为何删掉旧的 `current_turn.is_empty()` 判定。）

队列侧：`ThreadLoadDispatcher::send(&self, thread_id: String) -> Result<(), ThreadLoadDispatchError>`
在同步边界先取 `self.client.reserve_session_load()` 再入队（`thread_load_consumer.rs:55-67`；
`reserve_session_load(&self) -> SessionLoadReservation` 见 `acp_client/client/session.rs:266-278`，
`SessionLoadReservation` 的 `Drop` 递减 pending 并 bump epoch，`session.rs:76-90`）。
这条 reservation 存在的理由写在 `turn.rs:56-57`：避免 drain 出来的首条 prompt 抢在异步 load consumer 前绑定旧 session。

执行：`spawn_thread_load_consumer`（`thread_load_consumer.rs:70-157`）→ `handle_load`（`:163-182`）：
```rust
acp_client.load_session(&thread_id, cwd, None).await?;   // thread_load_consumer.rs:180
```
`pub async fn load_session(&self, session_id: &str, cwd: &str, model: Option<&str>) -> Result<String, AcpError>`
（`acp_client/client/session.rs:368-376`）→ `load_session_under_gate`（`:378-458`）里
**先投影会话边界再发 RPC**（`:412-414`）：
```rust
if self.projection_mode == ClientProjectionMode::Interactive {
    crate::kit::session_boundary::project_session_boundary(Some(session_id));
}
```
随后 `send_request("session/load", params)`（`:432-436`）、`commit_stable`、`finish_session_initialization`。
`thread_load_consumer.rs:173-174` 的注释明确：「Client-owned session boundary projects the target before the load RPC
so pre-response replay is accepted」——因为 host 侧 replay 在响应之前完成
（`peri-acp/src/host/requests/session_restore.rs:506-527`，注释 “replay history via session/update BEFORE responding”）。

对比参照（非 TUI 路径）：`peri/agent_event` 的 `AcpEvent::CompactCompleted` 同样带
`messages_json: String`（`peri-acp/src/event/mod.rs:139-162`），TUI 收到后**忽略**该字段
（`acp_events/mod.rs:452-470` 以 `..` 解构；`TurnCommitted` 同样 `messages_json: _`，`mod.rs:447-450`）。

### 2. 重放历史如何装入 TUI：整段替换（reset + append）

- 复位信号：`pub fn project_session_boundary(target_session_id: Option<&str>)`（`session_boundary.rs:10`），
  `session_boundary.rs:15`：
  ```rust
  atoms::BRIDGE_RESET_COUNTER.set(atoms::BRIDGE_RESET_COUNTER.get().wrapping_add(1));
  ```
  同函数还调 `acp_events::push_view_models_for_reset()`（`session_boundary.rs:20`）。
- bridge 侧：`fn apply_bridge_reset(state: &mut BridgeState, last_reset_counter: &mut u64, counter: u64) -> u64`
  （`acp_bridge.rs:388-440`），关键行 `acp_bridge.rs:405`：
  ```rust
  state.committed = im::Vector::new();
  ```
  并清 `current_turn` / `folded_history` / 代际 / `INPUT_BUFFER`（`:406-423`），末行 `push_view_models_for_reset()`（`:435`）。
  **同 session replay 特判**：`:394-402` 用 `replay_session_id == active_session_id` 保留
  `PENDING_COMPACT_NOTE`，并在 `:424-429` 重新塞回 `current_turn` 作为 system note；
  非 same-session 时丢弃该 note（`:430-434`）。
- atom 侧：`pub fn push_view_models_for_reset()`（`render.rs:763-785`）清
  `FOLD_OVERRIDES`、`FOCUSED_ENTRY`、`TRANSCRIPT_HISTORY`，并写入空
  `ViewModelsSnapshot { items: im::Vector::new(), generation: 0 }`（`render.rs:777-784`）。
- 追加侧（无合并）：重放事件逐条 push：
  - `handle_committed_assistant_text(state, text: &str, reasoning: &Option<String>)`
    → `state.committed.push_back(vm)`（`turn.rs:417-454`，push 在 `:451`）。
  - `handle_replay_tool_started(...)` → `state.committed.push_back(TuiRenderUnit::TuiToolCard(card))`（`tool.rs:153-189`，push 在 `:186`）。
  - `handle_replay_tool_ended(...)` → `update_committed_tool_card` 按 `tool_id` 改已存在的卡片
    （`tool.rs:191-202`；查找逻辑 `tool.rs:209-219`，注释：「如果找不到对应 tool_id，静默忽略」）。
  - user：`user_message_chunk` → `AcpEventData::ReplayedUserBubble{input_id,text}` 或
    `LocalUserBubble{text}`（`session_update.rs:287-302`）→ `handle_local_user_bubble`
    → `BridgeState::push_user_bubble`（`turn.rs:336-348`；`acp_events/mod.rs:237-255`）。
- 快照发布：重放三类事件返回 `PublicationIntent::Deferred`（`acp_events/mod.rs:578-585`），
  每条只 `push_acp_state`，避免逐条全量快照；`push_view_models` 的组装逻辑见 `render.rs:26-…`
  （fold pass `:66`、divider 插入 `:83-95`、`join_into(&mut items, active)` `:116`）。
- **没有 merge 逻辑**：`peri-tui/src/kit/` 内 `merge` 只出现在工具卡片分组（`render.rs:367-634`），
  与历史合并无关。

### 3. SDK：session/load 响应处理、写入 Yjs、消息身份

宿主侧 replay 内容（形状来源）：`pub async fn replay_session_history(session_id, history: &[BaseMessage], sender: &dyn ReplaySender, caps: &PeriCaps) -> Result<(), ReplayError>`
（`peri-acp/src/dispatch/session_replay.rs:58-63`）：
- `BaseMessage::Human` → `SessionUpdate::UserMessageChunk`，用 `msg.id()` 作 chunk messageId（`:76-83`）；
- `Ai` 的 Reasoning/Text/ToolUse → `AgentThoughtChunk` / `AgentMessageChunk` / `ToolCall`（`:122-161`）；
- `BaseMessage::Tool` → `ToolCallUpdate`（`:176-201`）；
- `is_system()` 消息被过滤（`:64`），compact reminder 转成 system reminder 通知（`:66-75`）。
- `fn replay_chunk(content: ContentBlock, caps: &PeriCaps, message_id: peri_acp_types::messages::MessageId) -> ContentChunk`
  （`:208-226`）显式保留身份：
  ```rust
  chunk.message_id = Some(MessageId::from(message_id.as_uuid().to_string()));   // :217-219
  if caps.replay { meta.insert("periReplay", Bool(true)); chunk.meta = Some(meta); }  // :220-224
  ```
  注释（`:214-216`）：「replay 时携带消息真实 ID，与流式路径一致」。

SDK 侧读取会话能力 + 发起 load：`Session.startOnce`（`npm-packages/@peri-sdk/src/agent/session.ts:75-177`）：
- 订阅（`:91-116`）：`transport.subscribe((event) => { ... this.docs.accept(event) ... })`；
  对 `session/update` 先判定 `replayTurnPending = update._meta?.periReplay === true`
  （`:99-103`），`peri/agent_event_done` 则清 `false`（`:104-106`）。
  任何通知都先 `this.docs.accept(event)`（`:108`），失败只 `console.error`（`:110-112`）。
- load 请求（`:143-148`）：
  ```ts
  const loaded = await transport.request<SessionSetup>("session/load", { ...params, sessionId: requestedSessionId });
  this.docs.seedConfig(loaded);
  if (replayTurnPending) this.docs.completeTurn();   // :146
  ```
  注释（`:145`）：「ACP 在 load 响应前完成历史回放，且没有独立的回放 turn-done 通知」。
- 之后 `session/input/snapshot` → `seedInputQueue`，置 `state="active"`（`:149-157`）。

写入 Yjs：
- `SessionDocs.accept(notification)`（`state/session-docs.ts:91-123`）→
  `chatProjection.accept(update, params)`（`:104`）→ `ChatProjection`。
- `ChatProjection.accept(update, params)`（`state/chat-projection.ts:19-28`）分派
  `user_message_chunk` / `agent_message_chunk|agent_thought_chunk` / `tool_call|tool_call_update`；
  带 `_meta.peri.sourceAgentId` 的 chunk 直接丢弃（`:22-23`）。
- `turn-machine.ts:33-50` `start(userText?, replayId?, replay=false)` 决定轮与条目 id：
  ```ts
  const turnId = replayId ? `turn:${replayId}` : `turn:${++this.nextTurn}`;   // :36
  const user = this.docs.ensureEntry(`${turnId}:user`, turnId, "user");       // :44
  this.docs.ensureEntry(`${turnId}:assistant`, turnId, "assistant");          // :47
  ```
- 文档 schema/写入原语由 `DocModel` 独占：`entries()`/`tools()`（`state/doc-model.ts:36-37`）、
  `ensureEntry(id, turnId, role)`（`:66-79`）、`appendText(entry, type, value)`（`:82-99`，文本进 `Y.Text`、
  block 顺序进 `Y.Array`）、`addToolBlock(entry, id)`（`:101-112`）、`ensureTool(id, turnId, name)`（`:114-123`）、
  `transactBoth`（`:49-51`）、`resetChat()`（`:53-64`）。

**messageId 穿透链（结论：整条链完整保留到 SDK 侧）**：
wire `messageId`（`session_replay.rs:217-219`）→ `JsonRpcTransport.acceptFrame` 只做 JSON.parse + 原样
`this.publish({ jsonrpc, method, params })`（`transport/json-rpc-transport.ts:110-123,138-143`，不裁剪字段）→
`SessionDocs.accept` → `chat-projection.ts:43` `const replayId = string(update.messageId);`（user）与
`:66` `const messageId = string(update.messageId);`（assistant），并落进 entry：
`:84` `entry.set("messageId", messageId)`、`:85` `entry.set("replayMessageId", messageId)`、
`:77` 轮内多消息时新条目 id 变成 `${turnId}:assistant:${messageId}`。

### 4. SDK：直播流与历史的合并、现有按 id 去重

- **合并点是“当前可写轮”**：`acceptAssistant`（`chat-projection.ts:61-91`）里
  `if (!this.turns.activeTurn() || !this.turns.writable()) return false;`（`:68`，注释：“A terminal turn can only
  be followed by a user turn. Late chunks must not create a ghost turn.”）；写入前用
  `info.activeAssistantEntryId` 定位条目（`:72-73`），messageId 变化时封旧条目并换新条目（`:74-80`）。
- **replay 覆盖同一 entry 的机制**：`replayText(entry, type, value)`（`:30-37`）在
  `entry.get("replayEpoch") !== this.replayEpoch` 时重置 `blockOrder`/`blocks` 再 append；
  assistant 侧在 `:82` 判断 `replay && messageId && entry.get("messageId") === messageId` 才走 `replayText`，
  否则 append，并在 `:86` 落 `replayEpoch`。
- **按 id 去重（现存全部）**：
  1. 通知级 `eventId`：`session-docs.ts:96-97` 读 `params._meta.peri.eventId`，
     `:122` `if (accepted && eventId) this.seenEvents.add(eventId)`。
     **注意：全仓库 Rust 侧检索不到该字段的生产者**（`peri-acp`、`peri-tui/src`、`peri-acp-types` 均无
     `eventId`/`event_id` 命中；`peri-acp` 的 `_meta.peri` 目前只写 `sourceAgentId`，见
     `peri-acp/src/session/event_sink.rs:281`）——该去重键当前事实上为空。
  2. user 条目存在性：`chat-projection.ts:45` `const existing = replayId ? this.docs.entries().get(`turn:${replayId}:user`) : undefined;`
     命中则 `:50-52` 追加/覆盖，不新建轮。
  3. 本地投递输入：`acceptDeliveredUserInput(inputId, text)`（`:13-17`）以
     `turn:${inputId}:user` 是否存在做幂等（`:14`）。
  4. 工具：`acceptTool`（`:93-132`）按 `toolCallId`（`:94`）取已有工具；终态不可回退：
     `if ((oldStatus === "completed" || oldStatus === "error" || oldStatus === "cancelled") && oldStatus !== status) return false;`（`:104`）；
     已挂到当前轮则不重复 `addToolBlock`（`:109-114`）。
  注：user 侧两条身份空间不同（`inputId` vs `messageId`），代码里没有任何把二者对齐的去重。

### 5. SDK 现有 ViewModel / 投影类型

`npm-packages/@peri-sdk/src/view/session-view.ts`：
- `EntryView`（`:19-27`）：`{ entryId; turnId; role: "user"|"assistant"; status; messageId?; tokenUsage?; blocks: EntryBlockView[] }`。
- `EntryBlockView`（`:16-18`）：`{ blockId; type: "text"|"reasoning"; text; visibility? } | { blockId; type: "tool_call"; toolCallId; tool: ToolView | null }`。
- `ToolView`（`:5-15`）：`{ toolCallId; turnId; name; status; kind?; arguments?; result?; argumentsRef?; resultRef? }`。
- `SessionView`（`:76-87`）：`{ schemaVersion; entries; activeTurnId; activeTurnStatus; session: SessionInfoView; tasks; plan; plansByTurn; pendingInteractions; inputQueue }`。
- 读路径：`export function readSessionView(chat: Y.Doc, session: Y.Doc): SessionView`（`:268-270`）；
  `class SessionViewStore`（`:305-…`）在 Y 事务上失效缓存并调度快照。
- 条目投影：`function entryView(value, tools, cache): EntryView | null`（`:160-178`），
  `:172-173` 从 entry 读 `messageId` 输出到 view。
- 导出面：`view/index.ts:1-8`（含 `Y`、`readSessionView`、`SessionViewStore`、view 类型 re-export）。

### 6. SDK 在哪里读/声明 client capabilities

- 声明点（唯一）：`npm-packages/@peri-sdk/src/agent/session.ts:117-128`：
  ```ts
  await initializeAcpClient(transport, {
      capabilities: {
          "peri.userInputQueue": true, "peri.agentEvent": true, "peri.sessionWorkspaceV1": true,
          "peri.agentEventDone": true, "peri.replay": true, "peri.tokenStats": true,
          "peri.planEntryActiveForm": true, "peri.unstableEvent": true,
      },
  });
  ```
  该对象是**硬编码常量**，不从 `AgentOptions`/配置读取（`grep capabilit` 在 `@peri-sdk/src` 只命中
  `session.ts:118`、`acp-handshake.ts:9-10,15,20`、以及无关的 `config/bare-harness-config.ts:3`）。
- 传输：`initializeAcpClient(transport, options: AcpInitializeOptions): Promise<void>`
  （`agent/acp-handshake.ts:17-27`）：
  `transport.request("initialize", { protocolVersion: ACP_PROTOCOL_VERSION, clientCapabilities: { _meta: { ...options.capabilities } }, ... })`（`:20`）。
- 服务端解析与使用：`PeriCaps::from_client_meta(meta)`（`peri-acp-types/src/peri_caps.rs:76-95`，
  `replay: meta_bool(meta, "peri.replay")` 在 `:84`）；caps 在 `initialize` 时暂存
  （`peri-acp/src/host/requests/session_lifecycle.rs:140-144`，`set_pending_caps`），
  `session/load` 时按 session 取（`session_restore.rs:505` `ensure_session_caps`）；
  只有 `caps.replay` 为真才写 `periReplay` 标记（`session_replay.rs:220-224,229-246`）。
- 对照（TUI 客户端）：以 `PeriCaps::all_enabled()` 为基座、关闭 `oauth`、替换 `ui_commands`
  后随 `initialize` 上送（`peri-tui/src/acp_client/client/requests.rs:70-80`；
  `all_enabled` 含 `replay: true`，`peri_caps.rs:171-189`）。

### 7. TUI 消息身份 + 滚动锚点现状

身份：
- `TuiUserBubble` 只有 `text / content_hash / reminder / source`，**无 messageId**
  （`peri-tui/src/kit/tui_render_unit/bubble.rs:5-13`），`TuiUserBubble::new` 也不接收 id（`:19-29`）。
- `TuiAssistantBubble.message_id: Option<String>`（`bubble.rs:48-50`，注释：身份字段、不进 content_hash、
  进 partial_eq），但重放/commit 路径恒 `None`（`turn.rs:439-442`）；直播路径才有值
  （`acp_events/streaming.rs:34,76,94,131,165` 用 `tc.message_id.as_deref()`）。
- 工具卡片身份 `tool_id`（replay 与 live 共用，`tool.rs:169,186,209-219`）。
- 折叠/焦点身份 = 从 VM 派生的 `FoldKey`：`Reasoning(message_id) / Tool(tool_id) / SubAgent(instance_id)
  / Interaction(request_id) / SystemReminder(reminder_id) / Group(tool_ids)`
  （`message_area/entry_nav.rs:39-69`，`fold_key_of(vm) -> Option<(FoldKey, FoldState)>`）。
- 焦点单一事实源是 **slot 索引 + key**：`set_entry_focus(snapshot, slot)` 写
  `FOCUSED_ENTRY = Some(FocusedEntry { slot, key })`（`entry_nav.rs:78-85`），并在 reset 时清空
  （`render.rs:773-775`）——即身份在跨 reset 后不可迁移。

滚动：
- 状态是纯行偏移 `struct ScrollPos { offset_y: usize }`（`message_area/scroll.rs:38-49`），`scroll_to_bottom()`
  设最大偏移、渲染每帧 clamp（`scroll.rs:45-49`）；**没有按条目身份记录的滚动锚点**。
- `Transcript` 里的 `anchors: BTreeSet<usize>`（`message_area/transcript.rs:22`）只是渲染缓存 pin
  （`anchor() -> Option<usize>` 取最小 slot，`:277-279`；在 generation 变化时 `split_off(&changed_from)`，`:103`），
  与滚动位置无关。
- 唯一的“锚定某个 entry”逻辑：pending interaction block → `anchor_scroll_target(scroll_y, vis_height, anchor_end, max_scroll)`（`scroll/auto_follow.rs:53-64`），
  仅在 follow 态生效（`auto_follow.rs:215-243`）。
- reset 后行为：`BRIDGE_RESET_COUNTER` 变化 → 把 `prev_items_len`、`last_scrolled_at` 归零作哨兵
  （`auto_follow.rs:158-171`），`consume_reset_force_bottom(prev_items_len, items_len)`（`:70-78`）
  在首个非空快照强制 `scroll_to_bottom()` + `follow_bottom = true`（`:186-201`）。
  即**历史重放一律滚到底**，浏览位置不保留。
- 粘性 follow：`should_follow_after_user_scroll(max_scroll, offset_y)`（`:17-21`，扣 `SCROLL_PADDING`）、
  `update_follow_on_scroll(...)`（`:23-31`）、`new_output_indicator_active(...)`（`:39-51`）；
  effect 依赖含 `loading_epoch / bridge_reset_counter / items_len / vm_generation`（`message_area/mod.rs:383-415`）。
- `LOADING_EPOCH` 变化 = submit 强制滚底（`auto_follow.rs:134-150`）。

---

## Constraints & risks

1. **TUI 重放的整段替换依赖 `BRIDGE_RESET_COUNTER` 这一个全局信号**（`session_boundary.rs:15` →
   `acp_bridge.rs:450-451,546-561,586-593` 三处检测点）。任何绕过 `project_session_boundary` 的 load
   都会变成“向旧 committed 追加历史”的脏状态。
2. **跨 reset 只保留一个 UI note**：`PENDING_COMPACT_NOTE` 的 same-session 特判
   （`acp_bridge.rs:394-402,424-434`）+ `system.rs:170-211` 的第二条兜底写入；
   其余 UI 覆盖态（`FOLD_OVERRIDES`、`FOCUSED_ENTRY`、`TRANSCRIPT_HISTORY`）在 reset 时清空
   （`render.rs:770-776`）。
3. **TUI 重放后强制滚底**（`auto_follow.rs:158-201`）：长历史 compact replay 会把用户视口拉到末尾，
   当前没有任何按条目标识恢复位置的机制。
4. **TUI 重放条目无消息身份**：`CommittedAssistantText` 不带 id（`event_data.rs:148-152`、
   `turn.rs:439-442`），user bubble 无 id 字段（`bubble.rs:5-13`）——去重、按 id 更新、跨 replay 折叠键
   都只能依赖顺序与 `tool_id`；`update_committed_tool_card` 找不到 id 时静默忽略（`tool.rs:206-219`）。
5. **消息顺序契约**：replay 是“逐条 RPC 通知”，且 user chunk 必须先建立轮，否则 assistant chunk 被丢
   （`chat-projection.ts:68`）；SDK 侧 load 响应前必须收完 replay 才 `completeTurn()`（`session.ts:143-148`）。
6. **SDK 的 `eventId` 去重目前无生产者**（`session-docs.ts:96-97`；Rust 侧全仓库无 `eventId` 写入）。
7. **SDK 硬编码 caps**（`session.ts:118-127`）：少声明一项就等于放弃对应事件通道
   （注释 `acp-handshake.ts:13-16`）；SDK 未声明 `peri.rewind` / `peri.systemReminder` 等，
   replay 的 reminder 会走 fallback 通道（`session_restore.rs:714-733`）。
8. **SDK `replaceHistory` 是全量清空重建**（`chat-projection.ts:137-139` `this.replayEpoch++; ... this.docs.resetChat()`），
   与 TUI 的“reset + 通道重放”是两套机制；SDK 侧 compact 不触发 session/load，也没有增量 merge。

## Open questions

1. TUI 是否要保留用户滚动位置/条目锚点（当前 replay 强制滚底），需要什么身份才能跨 reset 复原？
2. TUI 是否应改为消费已存在的 `messages_json`（`CompactCompleted`/`TurnCommitted` 已带到 `acp_events/mod.rs:452-470,447-450` 被忽略），
   以替代“compact 后 session/load 重放”？两者语义差异（是否含 system/reminder、工具结果缺失态）仍需核对。
3. 若要给 TUI 重放条目补 id：是否扩展 `AcpEventData::CommittedAssistantText` / `LocalUserBubble`，
   并在 `TuiUserBubble` 增字段？`session_update.rs:126-129,294` 已经从 wire 读到 messageId，只在 replay 分支丢弃。
4. `params._meta.peri.eventId` 是预留协议还是遗留字段？Rust 无生产者，SDK 的 `seenEvents` 目前不起作用。
5. SDK `replayEpoch` / `replayMessageId` 的“覆盖 vs 追加”边界是否有意为之（`chat-projection.ts:30-37,82-86`），
   以及 live 与 replay 对同一 messageId 是否可能出现双条目（当前无跨路径 messageId 去重）。
