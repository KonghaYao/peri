# P1：TUI live 工具卡无法进入历史窗口淘汰（同步子 Agent 正文为遗留项）

**状态**：已修复（本 worktree，含真实消息身份 wire），待 PR 合并。
**优先级**：P1。
**类型**：性能 / 内存。
**创建日期**：2026-10-24。
**来源**：代码审计。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `e598d178`）。

## 确认证据（基线 `e598d178`）

- `peri-tui/src/kit/acp_events/mod.rs:843-849`：`live_message_id` 只认
  `TuiUserBubble/TuiAssistantBubble.message_id`；工具卡（`tui_render_unit/tool_card.rs:78-81`
  只有 `tool_id`）与同步子 Agent 组（只有 `instance_id/agent_id`）返回 `None`。
- `peri-tui/src/kit/acp_events/mod.rs:272-305`：归档时无持久身份的单元只调用
  `note_unattributed_live_bytes`，不登记 `SegmentDraft`。
- `peri-tui/src/kit/history_window/model.rs:608-615`：`unattributed_live_bytes` 只有
  `saturating_add`，没有回落入口；`budget.rs:105-109` 把它计入总驻留量，但
  `admission.rs:301-335` / `commit.rs:140-172` 的淘汰候选限定
  `durable && pins.is_empty()` 的段，未归属字节永远不是候选。
- 淘汰只在页接纳预算压力（`admission.rs:301-335`）与 commit 后保留计划
  （`commit.rs:140-172`）触发；`TurnDone` / 折叠 / `CompactCompleted` 都不释放
  这些正文（`acp_events/turn.rs` 与 `compact.rs` 只 flush/提示）。
- 页加载出来的工具卡在 `PersistentEntryKey` 段内，可随页段淘汰
  （`acp_events/history_window.rs:808-874`）；live 工具卡不同。
- 协议侧：`peri-acp-types/src/event.rs:906-913` 的 `ExecutorEvent::ToolStart`
  虽然带 `message_id`，但 `event_v2/executor_mapping.rs:48-60` 用 `turn_id`
  填充；`peri-acp/src/event/mapper.rs:90-103` 映射 ACP `ToolCall` 时丢弃该字段。
  因此 TUI 收到的 live `tool_call` 没有可用的 assistant `messageId`。
- 存储投影确认工具调用是 assistant 消息条目的子项：
  `peri-acp/src/dispatch/session_replay.rs:485-559`（AI 消息的 `ToolUse`/`tool_calls`
  作为同一 payload 的 `tool_call` 子项），正确归属就是该 assistant 消息。

## 根因链

1. live 工具卡归档时没有 `PersistentEntryKey`，只能进 `unattributed_live_bytes`。
2. 该计数只增不减，且不在任何 `ResidentSegment` 里，无法进入 `ReleasePlan`。
3. 会话切换 / rewind / reset 只能整段清空 `committed`；正常折叠、TurnDone、
   CompactCompleted 都不释放。
4. 16 MiB 正文预算持续被未归属正文占用，超限后历史页接纳返回
   `EvictionBlocked`，页面不合并，长会话内存与交互同时退化。
5. 工具卡在存储里明明是 assistant 消息条目的子项，只是 live 事件丢掉了归属，
   属于「有持久身份但 TUI 未建立关联」，不是协议缺条目。

## 影响面

- 用户影响：长会话中每轮工具卡正文常驻；预算耗尽后无法继续加载更早历史页，
  折叠工具卡也不释放内存。
- 受影响模块：`peri-tui` 的 `kit/acp_events/`（归档与窗口桥接）、
  `kit/history_window/`（账目与淘汰）、`kit/tui_render_unit/`（工具卡身份）。
- 不受影响：卡片渲染样式、页协议语义、数据库 schema、会话切换/rewind/reset 语义。
- 同步子 Agent 正文：**本 issue 不修**。它没有 child-thread 补读通道，
  释放后无法按页取回；不得为此新增 ACP 子会话历史协议。

## 修复方案（本 PR）

1. **live 归属**：新增 bridge 内部 `live_tool_links`（按 `active_session_id`
   分区）。工具事件携带真实所属 assistant 消息时（wire `originMessageId`）
   直接记录 `tool_id -> message_id`，纯工具响应轮次（无文本/推理 chunk）同样
   可归属；旧服务端缺该字段时回退到流式 chunk 记录的最后 assistant 身份。
   turn 边界只清空启发式身份，按 `tool_id` 的归属保留到卡片释放。
2. **页回填**：open 页 adopt 阶段把页条目的工具子项与 `committed` 中 live 卡片按
   `tool_id` 匹配，扩展 `RetainedLiveEntries` 保存 `tool_id -> 条目/位置`；
   接纳成功后补登记为可淘汰段。没有可核实关联的卡片仍按未归属记账。
3. **并入现有生命周期**：沿用 `register_segment` / `promote_live_seal_position` /
   `seal_through_position` / `retention_plan_after_commit` /
   `release_window_plans`。同一消息的文本与工具卡共用一个 `LiveSeal` 条目：
   条目已驻留时用 `HistoryWindow::extend_live_entry` 并入（驻留索引要求条目身份
   唯一），不新造淘汰器、不重写 entry 模型、不动页协议。
4. **记账**：工具卡正文沿用 `retained_unit_bytes` 既有口径；归属后从
   `unattributed_live_bytes` 移出（`take_unattributed` +
   `release_unattributed_live_bytes`），未归属时按窗口实例+代次只记增量，
   避免重复计数。保留 `presentation` 字节的现有范围。
5. **迟到更新**：取消/挂起会把运行中工具卡归档；迟到的 `ToolEnded` 现在更新
   `committed` 中的卡片，并按条目是否驻留调整段字节或未归属账目，不产生悬挂引用。
   `ToolEnded` 只在该工具确实属于当前流式消息时才清除启发式身份，迟到的上一轮
   终点不会擦掉下一轮归属。
6. **wire（本轮补齐）**：`RenderEvent::ToolStarted` 与
   `ExecutorEvent::ToolStart` 新增可选 `origin_message_id`；既有
   `message_id` 仍由 `turn_id` 派生（v1 兼容语义不变）。`peri-acp` mapper 经
   ACP 标准扩展位 `ToolCall._meta.peri.originMessageId` 透传；TUI notifier 解析
   为 `TuiToolStarted.origin_message_id`。旧消息缺字段按 `None` 解析并回退启发式。
7. **明确不做**：不新增页协议、不释放同步子 Agent 正文、不改折叠/reset 语义、
   不改数据库 schema、不改 TUI 渲染样式。

## 复审修复：open 页回填不得破坏展示账本与布局顺序

**缺陷**：open 页回填「已在 `committed`、但尚未记账」的工具卡时复用了
`mark_display_block`，其 `DisplayLedger::insert_block` 会把所有 `start >= at` 的既有
区间右移；同时新段一律 `SegmentPlacement::Tail`，导致 `layout`/`index` 顺序与
`committed` 不一致。触发条件是工具轮先于文本轮归档（旧服务端常见）：回填后文本
展示区间被右移，释放事务按错误区间 `split_off(committed)`，预算已归还但正文残留。

**修复**：

1. `DisplayLedger::insert_existing_slot` / `HistoryWindow::mark_existing_display_slot`
   原地登记已存在槽位，不移动其他区间；冲突时不写入。
2. `HistoryWindow::segment_after_display_slot` 给出「首个更晚展示区间所属段」，
   回填段插到它之前（`SegmentPlacement::BeforeSegment`），无更晚区间才追加尾部。
3. `handle_window_page_loaded` 把绑定/回填推迟到 `merge_window_units` 之后：页正文与
   展示账本先落定，槽位、段顺序和预算都按最终 `committed` 计算；随后重发 spans 并
   补齐在途工具终点。
4. 回归覆盖区间不变、layout/index 顺序、工具/相邻段释放与预算回落，以及真实
   bridge 事件驱动的「归属 → 封印 → 预算压力 → 页接纳」链路。

## wire 链路（本轮补齐）

- **真实所属消息只在 Agent 发射点可见**：
  - 流式提前发射：`peri-agent/src/agent/model_bridge.rs:391` 生成本次 LLM 调用的
    `message_id`，`:492` 写入 `RenderEvent::ToolStarted.origin_message_id`；该 id
    在 `:504` 对齐到最终 `source_message`，与文本/推理 chunk 同身份。
  - 正式 dispatch：`peri-agent/src/agent/stages/tool_dispatch.rs:112` 取
    `ai_msg.id()`；`:226` 传给 `collect_tool_results`；结算路径
    `:66-86` 写入同一 id。`execution.rs:185/312/336` 在两类 `ToolStarted`
    （审批通过、拒绝补发）携带该 id。
  - 嵌套执行入口（`effective_dispatcher.rs:89`）没有独立 assistant 消息，显式传
    `None`，不用合成外壳消息 id 冒充持久归属。
- **v2 → v1 映射**：`peri-acp-types/src/event_v2/types.rs:97` 新增可选字段；
  `executor_mapping.rs:54-60` 原样透传，`message_id` 仍为 `turn_id` 派生。
  `peri-acp-types/src/event.rs:915` 的 `ExecutorEvent::ToolStart.origin_message_id`
  使用 `#[serde(default, skip_serializing_if)]`，旧载荷可解析为 `None`。
- **ACP 投影**：`peri-acp/src/event/mapper.rs:95-110` 把 origin 写入
  `ToolCall._meta.peri.originMessageId`（ACP 扩展位，不改标准字段；共享
  `project_tool_start` 不变，历史回放路径不受影响）。
- **TUI 解析与归属**：`peri-tui/src/kit/acp_notifier/session_update.rs:222-243`
  解析 `_meta.peri.originMessageId` 到 `TuiToolStarted.origin_message_id`；
  `acp_events/tool.rs:82-87` 交给 `live_tool_links::note_root_tool_started`
  （`live_tool_links.rs:109-123`）：显式 origin 优先，缺失才回退流式身份；
  空串按缺失处理。`note_root_tool_ended`（`:130`）只在结束工具属于当前消息时
  清身份，避免迟到 `ToolEnded` 擦掉下一轮；`note_turn_boundary`（`:149`）
  只清启发式，保留 `tool_id` 归属到卡片释放。

## 验收标准

1. 有 assistant `messageId`（含纯工具轮次的 `originMessageId`）的 live 工具卡
   归档后与正文同属一个 `LiveSeal` 条目，`unattributed_live_bytes` 为 0；
   提交封印后该段可被释放，`committed` 中的工具卡与正文一起移除。
2. 没有可用身份的 live 工具卡仍计入未归属预算，不伪造持久身份。
3. open 页按 `tool_id` 给出条目身份后，未归属工具卡可补登记为带位置的段，
   未归属账目不再只增。
4. 预算压力下，可淘汰的工具段能被 `plan_eviction` 选中；纯未归属字节仍拒绝接纳。
5. 同步子 Agent 正文继续不可淘汰（回归测试明确保留）。
6. 纯工具响应轮次（只有 `ToolStart`/`ToolEnded`、无文本/推理 chunk）在携带
   `originMessageId` 时可进入可淘汰段，释放后可由历史页重新补读。
7. 定向测试、`peri-tui` lib 全量测试（单线程）通过；`cargo fmt --check`、
   `git diff --check` 无输出。

## 验证记录（本 worktree，基线 `e598d178`）

第一轮（工具卡归属 + 窗口生命周期）：

- `history_window` → `115 passed; 0 failed`；其中 `history_window_commit_test`
  `19 passed`、`history_window_release_test` `7 passed`、
  `history_window_page_test` `36 passed`、`admission_test` `9 passed`。
- `test_live_tool_card_with_persistent_position_defers_result_with_source`
  → `1 passed`（live 卡片带持久位置时可进入超预算占位的补读来源）。
- `peri-tui` lib 全量（单线程）→ `2340 passed; 0 failed; 9 ignored`。
- 并行全量曾两次出现既有的全局 atom/时序用例偶发失败（每次用例不同，单独运行
  均通过，与本改动路径无关）；单线程全量作为确定性口径。

第二轮（真实消息身份 wire，本轮执行）：

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- tool`
  → `180 passed; 0 failed; 1111 filtered out`。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- tool`
  → `53 passed; 0 failed; 1014 filtered out`。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- history_window`
  → `117 passed; 0 failed; 2241 filtered out`（新增
  `tool_only_turn_origin_message_is_evictable_and_page_refetchable` 与
  `cross_turn_tool_ownership_does_not_bleed`）。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- --test-threads=1`
  → `2349 passed; 0 failed; 9 ignored`。
- 新增定向用例：
  - `test_tool_started_origin_message_identity_survives_mapping_and_legacy_payload`
    → `1 passed`（v2→v1 不丢 origin、`message_id` 仍为 turn 派生、旧载荷解析为 None）。
  - `test_tool_start_origin_message_id_is_carried_in_acp_meta` → `1 passed`
    （`_meta` wire key 正确；无 origin 不写 meta）。
  - `live_tool_links::tests` → `6 passed`（origin 优先/回退/迟到终点/边界/释放清理）。
  - `test_session_update_tool_call_reads_origin_message_id_from_meta` 与旧消息
    `test_session_update_tool_call_to_tool_started` → `4 passed`（含 update 变体）。
  - `bridge_emits_tool_started_on_first_tool_call_delta` →
    `origin_message_id == chunk message_id == source_message.id()`。
  - `test_resolution_error_emits_tool_started_and_ended` → 结算路径携带真实
    `ai_msg.id()`。
- `./scripts/cargo-rmcp-patched.sh check --locked -p peri-agent -p peri-acp -p peri-tui`
  → 成功。
- `./scripts/cargo-rmcp-patched.sh check --locked --tests -p peri-controller
  -p peri-middlewares` → 成功（事件新增字段的机械构造点同步）。
- `./scripts/cargo-rmcp-patched.sh fmt --all -- --check` → 退出码 0；
  `git diff --check` → 无输出。

第三轮（复审修复：open 页回填账本/layout 顺序）：

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- history_window`
  → `121 passed; 0 failed`（新增 `open_page_backfill_keeps_trailing_text_slot_and_layout_order`、
  `bridge_event_attribution_then_budget_pressure_admits_page`、
  `rewind_forgets_live_tool_links`、`open_removing_displayed_tool_card_forgets_live_link`）。
- `... -- live_tool_links` → `7 passed; 0 failed`。
- `... -- origin` → `15 passed; 0 failed`。
- `... -p peri-agent --lib -- test_approval_rejection_started_carries_origin_message_id
  test_dispatch_preserves_dual_path_schema_after_approval` → `2 passed`（审批通过/拒绝
  补发的 `origin_message_id` 专门断言）。
- `... -p peri-tui --lib -- --test-threads=1` → `2353 passed; 0 failed; 9 ignored`。
- `... check --locked -p peri-tui` → 成功。
- `fmt --all -- --check` → 退出码 0；`git diff --check` → 无输出。
- 测试口径说明：`bridge_event_attribution_then_budget_pressure_admits_page` 的 commit
  由测试直接调用 `apply_commit`（桥接事件负责归属/接纳，提交回执是构造的）；
  `open_page_backfill_keeps_trailing_text_slot_and_layout_order` 的释放计划是手构造
  `ReleasePlan`，不是 `plan_eviction` 产出。真实 `plan_eviction` 路径由第四轮
  `same_identity_open_retry_does_not_duplicate_tool_cards` 覆盖。

第四轮（复审修复：同身份 open 重试重复插入工具卡 + 单会话映射有界）：

- **根因**：`prepare_open_display` 只在 `identity_changed` 分支设置
  `adopt_enabled`。同身份重试（degraded 恢复 / 重复 open 响应）跳过
  `RetainedLiveEntries` / `bind_page_entries`，open 页的工具子项按新正文插入，
  live 工具卡又没有展示槽位，最终 `committed` 出现两张同一 `tool_id` 卡片，
  `unattributed_live_bytes` 保持 321。
- **修复**：adopt 判据改为「`committed` 非空」，在所有 `committed` 变更之后求值
  （`peri-tui/src/kit/acp_events/history_window.rs:767`）。同身份重试按现有 live
  卡片的 `tool_id` 去重并回填展示槽位，不重置窗口、不重复入段。
- **映射有界**：`live_tool_links` 单会话 `tool_messages` / `page_entries` 并集上限
  1024，超出淘汰最旧关联（`live_tool_links.rs:89-93`、`:75-83`）。已归属段的
  预算/释放只依赖段与展示账目，不查询这两份映射，释放语义不变。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- history_window`
  → `123 passed; 0 failed`（新增 `same_identity_open_retry_does_not_duplicate_tool_cards`、
  `same_identity_open_keeps_page_text_around_adopted_tool_card`）。
- `... -- live_tool_links` → `8 passed; 0 failed`（新增
  `tool_links_per_session_are_bounded`）。
- `... -- same_identity` → `3 passed; 0 failed`。
- `... -p peri-tui --lib -- --test-threads=1` → `2356 passed; 0 failed; 9 ignored`。
- `/tmp` 副本探针（同身份 open 两次，副本 `peri-tui` 源码分别换回修复前/后）：
  修复前 `FAILED`，`left: 321 / right: 0`（未归属字节未回落），EXIT 101；
  换回修复后源码 `1 passed; 0 failed`，EXIT 0。
- `... check --locked -p peri-tui` → 成功；`fmt --all -- --check` → 退出码 0；
  `git diff --check` → 无输出。

## 遗留 / 风险

- **同一条目混合文本 + 未归属工具卡**：open 页条目同时含兄弟文本与 live 工具卡时，
  adopt 只跳过被 live 持有的工具子项，兄弟正文仍随页插入；但该条目已作为页段驻留，
  `LiveSeal` 不能并入同一身份，工具卡继续留在 `unattributed_live_bytes`（保守记账，
  不伪造归属，也不会重复插入）。该卡片没有展示区间/可释放段，清理依赖直接
  `forget_tool`、rewind/reset 的 `forget_session` 或 1024 上限淘汰；
  `release_window_plans` 不会覆盖它。回归：
  `same_identity_open_keeps_page_text_around_adopted_tool_card`。该状态在
  `originMessageId` 可用时很少见：带 origin 的工具卡会先建立自己的 LiveSeal。
- **P1 遗留：同步子 Agent 正文仍不可淘汰。** 缺少 child-thread 补读通道，
  本 PR 明确不释放；需要单独的 ACP 子会话历史能力，不能靠本 issue 的页协议硬补。
- **旧服务端/旧事件回退**：没有 `originMessageId` 时，纯工具轮次仍只能依赖流式
  启发式，未发生 open 页回填前保持未归属（不伪造身份）。这是兼容路径，不是缺失。
- **嵌套执行入口**：`StageEffectiveToolDispatcher` 没有独立 assistant 消息，
  显式传 `None`；其工具卡不建立持久归属。外层工具仍由真实消息归属。
- **自测覆盖边界**：纯工具轮次用例通过桥接事件（带 origin）覆盖了 TUI 归属、
  淘汰与补读；wire 往返分别由 `peri-acp-types`/`peri-acp`/notifier 单测覆盖。
  没有启动真实模型/ACP 进程做端到端验证。
- **范围外机械同步**：为保持 workspace 可编译，事件新增字段需要给
  `peri-controller` 的 4 个测试文件、`peri-acp` 的 2 个测试文件及 `peri-agent`
  的测试构造点补 `origin_message_id: None`，均为纯字段补齐、无行为变化。
  `peri-middlewares` 只有匹配模式（`..`），未改动。
- **中断轮次的 origin 可能指向未持久化 assistant 消息**：`originMessageId` 在模型
  响应开始时即分配，若该轮在持久化前被中断/失败，工具卡仍会按该 id 建立 LiveSeal
  归属并参与淘汰；释放后历史页没有对应条目可补读。当前只保证归属来自真实消息身份，
  不保证消息已落库；需服务端在提交回执中提供更严格的「可释放」证据或补读兜底。
- 文档已同步：`docs/code-index/{peri-tui,peri-acp,peri-agent,peri-acp-types}.md`、
  `docs/design/tui-acp-data-flow.md`，以及 `history_window/budget.rs` 的过时注释。
- `live_tool_links` 清理按实际代码：rewind（`handle_rewind_completed`）调
  `forget_session`；replay 的 `handle_session_replay_started` 只调
  `note_turn_boundary`（只清启发式消息身份，不清 `tool_id` 映射；该事件当前是
  dead path）；没有独立的会话 reset 清理入口；工具卡释放/摘除走 `forget_tool`。
  单会话映射另有 1024 上限兜底，长进程不会无界累积。
