# TUI 与 ACP 数据流

> 状态：现行设计
> 范围：`peri-tui` 的 ACP 请求、通知解码、状态归约、交互响应与渲染发布
> 代码定位：[`peri-tui` 代码索引](../code-index/peri-tui.md)

Peri TUI 是 ACP client 前端。用户交互和 Agent 执行都经 ACP transport；TUI 不直接驱动
ReAct loop 或 middleware。Agent 产出 canonical 事件，ACP 负责协议映射，TUI 只把通知归约为
本地视图状态。跨层执行归属以 `ARC-BOUNDARY-001`，事件链完整性以 `ARC-EVENT-001` 为准。

## 分层与所有权

```text
peri-tui                 peri-acp                    peri-agent / middlewares
────────                 ────────                    ─────────────────────────
输入与交互请求 ────────→ ACP client / transport ──→ session 执行入口
组件 ← atoms/ViewModel  ← notification / mapper    ← canonical Agent events
         ↑
  acp_notifier → acp_bridge / BridgeState
```

| 层 | 稳定职责 | 不拥有 |
| --- | --- | --- |
| TUI | 输入、焦点、组件、通知解码、本地状态与渲染 | ReAct、工具策略、middleware 顺序 |
| ACP | session lifecycle、transport、请求转发、事件协议化 | TUI ViewModel、业务执行语义 |
| Agent | session runtime、RCRA loop、canonical 事件与终态 | TUI atom、布局与渲染队列 |
| Middleware | prompt contribution、工具、审批、MCP/plugin 等能力 | ACP/TUI 协议投影 |
| `peri-acp-types` | 跨 crate identity、事件、命令和 DTO | TUI 私有渲染类型 |

TUI 可以在 deployment composition root 复用配置和初始化类型，但 prompt、cancel、session、
interaction response 与详情请求等运行时动作仍必须经 ACP client/transport。

## 核心链路

### 用户提交

```text
InputArea
  → SteerCommand / STEER_TX
  → steer_consumer
  → AcpTuiClient::ensure_session + session/input/enqueue
  → Agent session runtime
```

上述普通输入路径在协商 `peri.userInputQueue` 后启用；未协商时仍经
`SubmitRequest / SUBMIT_TX → submit_consumer → session/prompt`，已注册 slash 命令也保留原路由。
新路径的用户气泡由 canonical `UserInputDelivered` 经 bridge 使用既有渲染器生成；
旧路径仍通过 TUI local event 进入 bridge。组件不直接改写 transcript。
待发送投影、完整取回与运行身份见[用户待发送队列](user-input-queue.md)。
加载、取消、rewind、AskUser 和 permission response 各有独立 consumer/channel，但都经 ACP
执行，不建立旁路。

### Agent 输出

真实执行与交互许可由宿主开始/结束身份驱动，不由首 token 或 loading 反推。外部实际 attempt（包括内部 continuation）经 `ExecutionStarted` 提供 session mailbox generation 与非空 request ID；managed 用户输入保留已有开始事件，共用客户端执行身份规则。定时执行前审批先建立独立身份并在审批结束后关闭，实际执行使用另一身份。陈旧终态或提交失败只能结算自己的身份，不能直接写全局 loading。输入回滚记录独立绑定提交请求，external execution 开始不能借用待提交文本的回滚许可，其结束或取消也不能丢弃另一个请求的回滚记录。挂起信号与恢复后的渲染输出共享 FIFO，不能从另一条状态通道越过新输出。实施验证见 `spec/issues/2026-10-08-async-execution-chain-fixes.md`。

```text
Agent canonical event
  → ACP mapper / session notification
  → AcpTuiClient notification pump
  → acp_notifier（wire DTO → AcpEventData）
  → acp_bridge（session/owner 检查 + 状态归约）
  → VIEW_MODELS / domain atoms
  → components
```

`acp_notifier` 只解码和转发，不写 UI。`BridgeState` 是消息事件到 transcript/atoms 的状态
边界；组件只订阅状态并渲染。新增或变更事件必须同时覆盖 producer、协议类型、ACP mapper、
capability gate（如适用）、TUI decoder、reducer 和终态测试。

### 渲染发布

```text
BridgeState.committed + CurrentTurn
  → TuiRenderUnit projection
  → ViewModelsSnapshot { items, generation }
  → message_area incremental cache
  → wrap/slot index + viewport clipping
  → ratatui render
```

`VIEW_MODELS` 是消息区单一发布面；`BridgeState` 保存 committed 历史与当前回合，终态时将
当前回合归档。流式 mutation 与 ViewModel projection 分离，bridge publication scheduler 可在
固定帧预算内合并连续主 Agent 更新，但首块边界、终态和 reset 必须及时发布。Markdown、wrap
和 viewport 性能契约见 [TUI 流式 Markdown 性能设计](tui-streaming-markdown-performance.md)。

## 历史窗口与段生命周期（服务端能力门控）

`peri.historyWindowV1` 复用 `session/load` 的 `history` 字段（`open`/`page`/`seek`），正文分块
走 `session/history/body`；提交事实经私有通知 `peri/history_committed` 送达。该路径由服务端能力
门控：`initialize` 响应未明确回显 `peri.historyWindowV1: true` 时一律 fail-closed，客户端请求值
不能替代服务端能力事实。TUI 本地投影开关未开启时，旧 `HISTORY_PAGING` 分页与渲染路径保持不变，
所有已加载历史继续固定、不淘汰。开关只决定是否采用窗口模型，不改变协议协商结果。
`HISTORY_WINDOW_V1` 由 settings beta `history-window-v1` 在启动时读取一次并写入 atom；未配置视为
开启（发布启用绑定 S7），只有显式配置 `false` 才关闭。设置变更不热重载，重启后生效。仍要求
服务端明确回显 `peri.historyWindowV1: true`。Beta 设置面板仍只列
`peri-acp-types::beta_flags::BETA_FLAGS`，该键需手动写入 settings。

声明 `peri.historyWindowV1` 的连接，其 `session/load` **不回放历史**；TUI 在 load 响应后先完成
用户输入快照等本地初始化，成功后才 `enqueue_window_open`，初始化失败不得 open。首屏因此完全
由随后的 `history.open` 提供；第三方客户端声明能力却不 open 会看到空首屏。

### 请求与通知时序

1. `initialize` 协商 `peri.historyWindowV1`（响应未回显一律按不支持处理）；TUI 启动时已从 beta
   `history-window-v1` 写入 `HISTORY_WINDOW_V1`（只写一次，无热重载），它（未配置默认开启、
   显式 false 关闭）与服务端协商结果同时为真才走窗口路径。
2. `history_page_consumer/window.rs` 在发请求前登记 `PageReservation`（目标 512 KiB + 解码开销），
   受 `in_flight + pending_merge` 上限约束；预算不足先尝试释放，仍不足则不发送请求。
3. `session/load` 的 `history.open/page/seek` 经 ACP transport 到达 Host；Host 在生命周期路由
   之前分流，短锁校验 session/ticket 后在锁外读取存储，返回前再次校验并投影为 `items[]`。
4. consumer 只在响应自带的 `sessionId`、`sessionGeneration`、`snapshotToken`、
   `windowGeneration`、`requestId` 全部匹配时交给 bridge；bridge 再校验当前浏览身份。
   迟到页整页丢弃并归还预留，不合并、不留下半页。
5. bridge 通过一次释放/接纳事务把新段并入 `committed`，登记 `DisplayLedger`、`ResidentIndex`、
   段边界游标与 `WINDOW_UNIT_KEYS`，随后只发布一次快照。
6. `peri/history_committed` 到达后按 `checkpointId` 幂等封印 durable 段；通知丢失时下一次
   `history.open` 的 `committedTail` 是唯一重新对齐入口。

### 窗口模型、预算与释放

1. `BridgeState.committed: im::Vector<TuiRenderUnit>` 仍是展示正文的唯一强持有者。
   `HistoryWindow` 只保存持久身份（`source + entryId + subItem`）、`SegmentKey`、条目/单元计数、
   字节账目、`durable/sealed/pins`、gap 与锚点，不复制正文。
2. 段来源区分 `Replay`、`Page`、`LiveSeal`；`SegmentKey` 是本地生命周期句柄，不进入 wire、
   不参与锚点或工具配对。窗口换代后旧键全部作废。
3. 初始预算（实施初值，不是性能结论）：历史展示正文 16 MiB、页预留上限 2 MiB、
   普通页目标 512 KiB、页条目 50/最多 100、未配对工具结果 1 MiB/256 条、详情窗口 4 MiB、
   释放水位 75%、预取门槛 8 MiB、渲染缓存 8 MiB 且超限降到 6 MiB。live、运行中、未持久化
   与继承段计入 `pinned_bytes`（不可淘汰，但仍是 body 占用）；已按真实 assistant 消息归属的
   `LiveSeal` 工具卡在 `peri/history_committed` 解除 `Unpersisted` pin 后转为可淘汰。
   `unattributed_live_bytes` 只承载仍无可核实身份的工具/子 Agent 正文，工具卡每次归属都
   按窗口实例+代次扣减，不再只增不减。全部固定时接纳显式失败，
   不能把归档正文长期计为固定。
4. 接纳先按字节判断再构造展示单元；超预算时按“距锚点最远 → 最久未访问 → 最早创建”生成
   `ReleasePlan`，跳过所有 pin。全固定时拒绝接纳并记录 `evict_blocked_pinned`，不释放未持久化内容。
5. 统一释放事务只允许在 bridge 串行链调用。顺序是：移除 `committed` 对应槽位范围 → 清
   `DisplayLedger`/`HistorySpans`/`ResidentIndex` → 失效折叠历史、装饰历史与分组缓存 →
   清未配对工具结果、选择/焦点与 `WINDOW_UNIT_KEYS` → 递增 `structural_epoch` 并发布局部快照。
   正文与派生缓存释放后先记入 `pending_release`；渲染侧观察到新 `STRUCTURAL_EPOCH`、清空
   受影响 slot 并写回 `RENDER_ACHIEVED_EPOCH` 后，预算才归还。
6. 释放段变成 `HistoryGap`，保存前后稳定身份与内侧邻居游标：`beforeCursor` 取更晚驻留段的
   `beforeCursor`（page(Before) 从缺口上端回填），`afterCursor` 取更早驻留段的 `afterCursor`
   （page(After) 从缺口下端回填）；相邻 gap 合并。翻到 gap 时按滚动方向选择 page，锚点在
   中间时用 `seek` 按稳定 `entryId` 重新定位；锚点缺失不跳底。
7. 窗口边界游标由**驻留布局两端段**派生（最早段的 `beforeCursor`、最晚段的 `afterCursor`），
   在每次 admission/release 后重算并写入 consumer；单页响应只推进它负责的那一端。淘汰端段后，
   新的边缘段游标天然指向被淘汰区间，`request_window_newer_page_if_due` /
   `request_window_older_page_if_due` 据此继续双向翻页。
8. 视口所在段与其相邻段在 compact 后保留；其余已 durable/sealed 段才可释放。释放不写回
   存储，不调用工具完成、Todo、审批、workflow 或输入队列 handler，不改变 phase/request/turn 身份。

### 提交、失效与降级

1. `CompactCompleted` 仍只显示提示，不触发段释放，也不作为核心提交依据。`TurnDone` 只把
   `current_turn` 归档并标记未确认；没有持久身份回执的内容不淘汰。
2. `peri/history_committed` 只发给当前持票连接，按 `checkpointId` 幂等；它封印位置
   `<= committedTail` 的段，并把未持久化 pin 升级为 durable。缺口由下一次 `open` 对齐。
3. 每个 session 只有一个活动 ticket，且绑定发起 open 的连接。新 open 成功才原子替换旧 ticket；
   open 失败保留旧 ticket/snapshot/generation 继续可用。
4. `page`/`seek`/`body` 在存储读取前和返回前校验 `sessionGeneration`、`structureVersion`、
   ticket epoch、`snapshotToken` 与 `committedTail`。rewind durable 成功恰好推进
   `structureVersion + 1` 并撤销 ticket；失败不推进；持久化不确定撤销票据并进入
   `ReloadRequired`：旧 ticket 不再续用，但只读的 `open`/`page`/`seek`/`body` 仍按 Store
   事实服务（`HandedOff` 同样不拒绝只读）。
5. rewind、关闭重开、session 切换或身份不匹配时递增 `window_generation`，取消在途请求，
   丢弃迟到页；旧页不得合入新窗口。能力缺失、游标失效或通知丢失进入 `Degraded`：停止淘汰、
   保留可读内容与重试入口，禁止自动回退完整 `session/load`。

### 超大条目与详情（S7.3 已交付）

服务端定义 `items[].body` 内联描述符（`totalBytes`/`chunkSize`/`readTicket`/`snapshotToken`）
与 `session/history/body` 有界分块（默认 64 KiB、最大 256 KiB，`bytes` 为 JSON 数字数组），
并受当前 snapshot tail 与 descriptor 白名单约束；非 oversize 条目或越过 tail 的读取返回 `-32016`。

TUI 侧由 `PanelKind::HistoryDetail` + `HistoryDetailPanel`（`kit/panels/history_detail.rs`）承接，
复用现有 `PanelOverlay`：

1. 占位 note 按 `content_hash` 命中 descriptor，Enter 打开；Esc/q 关闭；Up/Down/PageUp/PageDown
   滚动，滚动到未加载区间时发 `session/history/body` 请求；`e` 按现有剪贴板惯例复制已加载正文。
2. `load_history_body` 请求带 `sessionId/entryId/source/snapshotToken/readTicket/offset/maxBytes`，
   响应 `{entryId, offset, bytes, nextOffset, complete}`；同一 descriptor 单在途请求。
3. 跨块字节按连续前缀重组，UTF-8 在整体解码时校验；详情窗口预算 4 MiB，超预算淘汰离视口最远的块；
   关闭面板/session 边界/失败路径清空 chunks，`Weak` 证据见 `detail_close_releases_chunk_allocations`。
4. inherited 来源使用 `HistorySource::Inherited { frozen_snapshot_id, ancestor_thread_id }`；
   open 响应 `inherited` 对象、条目 `position:null`；inherited 段固定不释放，详情按冻结 snapshot
   读取。`inheritedUnsupported=true` 在详情面板与通知中显式提示，不回退父会话当前历史。
5. 限制：鼠标单击只设焦点，需再 Enter 打开；`e` 是复制已加载正文，文件导出/超窗流式导出尚未接
   入 UI（模型 `export_stream` 已支持分块）；未做 JSON 字段级展示；inherited 段内部不插入独立
   占位行，使用全局 inherited notice。TUI 解码仍接受旧顶层 `bodyDescriptors` 兼容输入，服务端已
   不再发送该形状。

### 对象级证据与已知限制

进度与验收记录见 active spec；本节只固定当前实现边界，避免把目标写成现状。

释放验收不以条目数或 RSS 为唯一证据。`release_removes_committed_and_invalidates_holders_before_ack`
在释放 + 渲染 ack 后断言 `Weak<TuiAssistantBubble>` 无法升级、各 holder 指纹清空；
`structural_epoch_drops_old_slot_lines_and_uses_8mib_to_6mib_budget` 与
`structural_epoch_with_empty_snapshot_reaches_zero_retained_bytes` 断言旧 slot 行缓存被丢弃且
`retained_bytes() == 0`；`pending_release_keeps_budget_conservative_until_ack` 与
`release_ack_is_idempotent_and_late_ack_does_not_double_credit` 断言预算结算；页链路由
`bidirectional_page_requests_keep_placement_and_cursors`、
`stale_or_late_response_is_dropped_and_reservation_released`、
`one_in_flight_and_one_pending_merge_are_enforced` 覆盖。

已知未完成项：

- 折叠组/子 Agent 组尚未真正惰性化：构造时 `im::Vector` 结构共享、不深复制正文，但释放只
  整体失效 `FoldedHistory`/分组缓存，没有按段惰性物化与精准清理。
- `structural_epoch` 目前清空全部渲染 slot，未按段精准清理。
- 旧分页路径仍使用轻量静态 `PENDING_TOOL_ENDS`，未完全迁移到 `ResidentIndex`。
- 详情交互限制：鼠标单击只设焦点，需再 Enter 打开；`e` 复制已加载正文，文件导出/超窗流式
  导出尚未接入 UI（模型 `export_stream` 已支持分块）；未做 JSON 字段级展示；inherited 段内部
  不插入独立占位行。
- beta 设置面板仍只列 `BETA_FLAGS`；`history-window-v1` 未配置默认开启，如需关闭须手动写入 settings `false`，且 TUI 只在启动读取一次、无热重载。
- remote/emscripten adapter 未实现窗口/继承读取，默认 `Unsupported`；readTicket 无密码学签名；
  rewind 不确定态仍依赖集中文本 marker。

## 状态模型

### Transcript

`TuiRenderUnit` 表达 user、assistant、reasoning/tool、system、SubAgent、group、divider 和
interaction 等可渲染语义；具体枚举与字段以源码为准。动态 inventory 不在本文复制。

`CurrentTurn` 按到达顺序保存 assistant text/reasoning、tool 和 SubAgent segment。新的
message identity 或 tool/SubAgent 边界会冻结前一文本段；chunk 只增长当前段，不为每个 chunk
创建 entry。projection 必须维持 segment 顺序和独立 reasoning slice。

`ViewModelsSnapshot` 使用持久化容器与单调 generation，使组件能够比较 revision 并复用稳定
渲染缓存。只有 bridge 的 canonical publish 路径可以替换该 atom；session reset、rewind 和
replay 也必须通过同一所有权边界。

### UI domain atoms

Transcript 之外的状态按 domain atom 分离，例如 session/loading、context usage、notification、
interaction、panel/popup、background task、plugin 与 workflow snapshot。atom 只是 TUI 投影，
不得成为服务端事实的第二份持久化；刷新、重连和 session transition 后应由 owner-aware
snapshot/event 重建。

组件 render body 不写 atom。所有写入发生在 consumer、bridge event、effect 或明确用户 action
边界；相关约束见 [TUI 规则](../standards/tui.md)。

## 事件语义

| 事件族 | TUI 归约 |
| --- | --- |
| assistant text / thought chunk | 追加到当前 message segment；维持 message 与 source identity |
| tool start/update | 按调用 identity 原地更新 tool activity；历史 replay 走同一语义归约。live `ToolCall` 经 ACP 扩展位 `ToolCall._meta.peri.originMessageId` 携带真实所属 assistant 消息（纯工具轮次唯一归属来源）；replay 路径不写该字段 |
| plan / usage / context metadata | 更新计划或资源投影，不伪装成普通消息 |
| turn terminal / suspended / execution failure | 归档、回滚或保留当前回合，并退出 loading |
| compact / rewind | `CompactCompleted` 只显示提示；`peri/history_committed` 按提交水位封印 durable 段并驱动预算释放；rewind 以服务端 snapshot 重建 transcript 并失效旧窗口与游标 |
| 历史窗口 | `open/page/seek` 响应按稳定身份并入段窗口；gap/seek 恢复被释放区间；`session/history/body` 只读详情分块 |
| SubAgent | 按 source/instance 归组并更新生命周期 |
| background task / workflow / plugin | 更新独立 atom、通知或管理面板 snapshot |
| AskUser / permission / OAuth | 注册 durable interaction owner，再发布对应交互 surface |

`session/update` 承载标准 ACP 流式内容、工具、计划和 usage；Peri 扩展只用于 ACP 标准面不能
表达的低频语义。扩展不得建立第二条语义不一致的 tool/turn 终态通道。

### Live 工具卡归属、回填与释放

工具卡在 wire 上没有独立持久身份：ACP `ToolCall.toolCallId` 是调用身份，持久归属是所属
assistant 消息的 `messageId`。live 路径的归属链路是：

1. Agent 侧三个发射点写入真实消息身份：流式提前 `ToolStarted`（`model_bridge`）、正式
   dispatch 的审批通过/拒绝补发 `ToolStarted`（`tool_dispatch/execution.rs`）、结算路径
   `emit_settled_tool_render`，并标 `origin_readable=Some(true)`。嵌套执行入口
   （`StageEffectiveToolDispatcher`）没有独立 assistant 消息，显式传 `origin_message_id=None`
   与 `origin_readable=Some(false)`，不得用 turn/shell 消息 id 冒充。
2. v2 `ExecutorEvent::ToolStart.origin_message_id` 与 `origin_readable` 为独立可选字段，
   v1 映射原样透传；`message_id` 仍由 `turn_id` 派生，两套语义不互相替代。
3. `peri-acp` mapper 只对 live 工具事件写 `ToolCall._meta.peri.originMessageId` 与
   `originReadable`；历史 replay 由持久 transcript 重建工具投影，不写这些 key
   （否则会把回放条目误挂到实时消息）。
4. TUI notifier 解析 `_meta` 写入 `TuiToolStarted.origin_message_id / origin_readable`；
   `live_tool_links` 记录 `tool_id → assistant message`。`Some(false)` 表示发射点确认
   无可回读持久身份：不使用 origin，也不回退流式身份，卡片保持未归属、不可淘汰；
   字段缺失（旧服务端）才回退到同一流式 assistant 消息身份；两者都没有则保持未归属，
   按 `unattributed_live_bytes` 计入总预算。
5. 归档时同身份的文本与工具卡共用一个 `LiveSeal` 条目（`extend_live_entry`），归属成功
   立即把工具卡字节从 `unattributed_live_bytes` 移出；预算重置按窗口实例+代次隔离旧记录。
6. open 页按 `tool_id` 给出条目身份时，`RetainedLiveEntries` 绑定 `tool_id → 页条目/位置`。
   adopt 判据是 `committed` 非空，与本次 open 是否切换窗口身份无关；同身份重试也按现有
   live 卡片去重，不重置窗口、不重复入段。**回填必须发生在页正文与展示账本落定之后**：
   `DisplayLedger::insert_existing_slot` 原地登记已存在槽位，不移动其他区间；新段插入到
   「首个更晚展示区间所属段」之前，使 `layout`/`index` 顺序与 `committed` 一致。使用会
   右移区间的 `insert_block` 会让释放事务按错误区间删除正文（预算已归还但正文残留）。
   同一条目只有部分工具子项被 live 持有时，只跳过这些子项，兄弟正文仍随页插入；若该
   条目已作为页段驻留，`LiveSeal` 不能并入同一身份，工具卡按既有保守路径继续计入
   `unattributed_live_bytes`，不得因此丢弃兄弟正文。
7. 释放/rewind：统一释放事务按展示区间删除 `committed` 中的工具卡并清理 live 归属；
   rewind 替换整段 transcript 时整体丢弃会话归属（`forget_session`）。replay 只清启发式
   消息身份（`note_turn_boundary`），不清 `tool_id` 映射。`live_tool_links` 单会话两份
   映射的并集上限 1024，超出淘汰最旧关联，长进程不会无界累积。

回归：`history_window_page_test/open_page_backfill_keeps_trailing_text_slot_and_layout_order`
（布局/索引/释放一致性，释放计划为手构造 `ReleasePlan`）、
`bridge_event_attribution_then_budget_pressure_admits_page`（真实事件驱动的归属 → 封印 →
预算压力 → 页接纳，commit 由测试直接调用 `apply_commit`）、
`same_identity_open_retry_does_not_duplicate_tool_cards`（同身份 open 两次：一张卡、
`unattributed=0`、真实 `plan_eviction` 只释放一次）、`live_tool_links::tests`（含
`tool_links_per_session_are_bounded`）、`rewind_forgets_live_tool_links`、
`open_removing_displayed_tool_card_forgets_live_link`。

Root cache coverage 只使用当前 root turn 最近一次有效 observation；auxiliary agent usage 不得
覆盖父 turn。missing、zero 或不一致 observation 要显式清除旧样本。低覆盖提示在回合终态前
至多生成一次，不能由每个 model step 重复告警。

## 回合与 session 生命周期

Bridge 的 session phase 至少区分 idle、prompt running 与 history replay；渲染 variant 不等于
执行 phase。正常终态、取消、中断、挂起和执行失败必须有明确处理：

- 正常结束：归档 `CurrentTurn`，停止 loading，再按队列策略继续；窗口路径只有拿到持久提交回执
  的段才可封印/淘汰；
- 中断：按 request/session identity 拒绝 stale terminal；有输出时保留可审计内容，无输出时
  才恢复对应输入，不影响更新一代提交；
- 挂起：归档可见内容并停止 loading，但不把仍存活 Agent 误判为完成，也不提前 drain 输入；
- 失败：生成可见错误语义，清理与本回合绑定的 transient state；
- reset/load/rewind：递增 generation，失效旧 session 的 pending publication、interaction、
  selection、focus 与 hover 状态。

普通 session load 在 producer 入队边界取得 reservation，并持有到 load 提交或放弃；prompt
入口在选择 stable session 前等待 reservation 清零。该线性化要求防止延迟 load 替换首个 prompt
所需 session，详见 `ARC-SESSION-LOAD-001`。

Turn terminal 是唯一结束信号。每条终止路径都必须离开 loading；重复或 stale terminal 幂等，
不得删除新一代 turn 的内容。tool、SubAgent 和 interaction 的未终结状态应在对应 terminal
语义下收敛，而不是永久 spinner 或默认标记成功。

## Durable interaction

AskUser 和 permission request 走标准 ACP 交互通道。notification pump 在 forward 前从 request
提取非空 session identity，并由 interaction lifecycle 分配 semantic owner；bridge 发布前再次
验证 owner、session 和 projection。invalid、stale、已删除 session 或 transport terminal 必须
形成 typed cancel/terminal，而不是留下可提交 UI。

用户响应、cancel、session transition、prompt terminal 和 transport EOF 竞争同一 owner；每个
request 最多一次 response attempt 和一次 owner-aware local terminal。response 失败时保留可重试
surface；成功后 compare-and-clear exact owner，不能按相似 payload 或最新 popup 猜测。

Panel/Popup/Inline block 是同一 durable interaction 的不同投影，不是不同请求。headless client
可以 claim/respond，但不得触碰 TUI atom。审批的交互安全与目标 Workbench 见
[TUI Chat Workbench](tui-chat-workbench.md)。

## 焦点、后台与管理面

Panel、Popup、transcript 和 composer 的 focus/事件优先级由 TUI router 管理。组件只执行已有
semantic action；请求和副作用进入对应 consumer。Modal 处于前景时背景 inert；非模态 panel
是否允许 transcript 操作由区域 owner 决定。

SubAgent、后台 task、Cron、Workflow 和 plugin 数据不应混入 `CurrentTurn` 的文本语义：

- 与主时间轴有稳定 identity 的 Agent/tool activity 可投影为 transcript entry；
- service snapshot 或列表状态进入独立 atom/panel；
- 短暂完成提示进入 notification/background display；
- 需要长期审计的结果由 transcript 或服务端持久化提供，不能依赖会自动消失的 toast；
- polling 与 event 若同时存在，必须声明单一权威 reducer，避免两套状态互相覆盖。

## 稳定不变量

1. **ACP 边界**：TUI 不直接驱动 Agent runtime；所有运行时请求经 ACP。
2. **单一归约入口**：notification 先解码，再由 bridge 验证 session/owner 并写状态。
3. **单一 transcript 发布面**：消息区只消费 `VIEW_MODELS`；组件不旁路追加消息。
4. **身份优先**：session、request、message、tool 和 source identity 用于去重与 stale filtering；
   arrival order 或显示标题不是身份。
5. **终态完整**：完成、失败、取消、挂起和 transport terminal 均清理 loading 与 transient owner。
6. **live/replay 等价**：同一语义经相同 reducer 收敛；重放不恢复失效的可执行 interaction。
7. **前端隔离**：Agent/ACP 不依赖 `TuiRenderUnit`、atom、layout 或渲染缓存。
8. **安全显示**：协议 payload 按不可信输入处理，不把任意成功业务数据直接 dump 到
   transcript；运行时诊断按 ARC-SECRET-001 保留实际内容，不承诺界面无凭据数据。
   认证、受众授权、限长和终端控制字符处理独立有效。
9. **增量渲染**：流式更新只失效变化 slot；viewport 外内容不做无界重复 wrap/clone。
10. **可验证变更**：跨层事件修改同步更新协议、mapper、TUI decoder/reducer、caps 和测试。
11. **服务端能力门控**：服务端未明确回显 `peri.historyWindowV1: true` 或 TUI 本地开关显式关闭时，
    旧分页/渲染路径不变；
    窗口失败进入 `Degraded`，不以完整 `session/load` 兜底。
12. **释放有界**：只有 durable/sealed 且无 pin 的段可释放；正文唯一强持有者是 `committed`，
    释放必须走统一事务并等待渲染 ack，存储历史不因 UI 淘汰而改变。

## 事实源

- TUI 入口与符号：[`docs/code-index/peri-tui.md`](../code-index/peri-tui.md)
- ACP host/session/event：[`docs/code-index/peri-acp.md`](../code-index/peri-acp.md)
- Agent loop/session：[`docs/code-index/peri-agent.md`](../code-index/peri-agent.md)
- ACP wire：[peri-acp-protocol.md](peri-acp-protocol.md)
- 历史窗口/段生命周期：本文「历史窗口与段生命周期」与 [消息存储](message-transcript.md) §2.8
- transcript 持久化：[message-transcript.md](message-transcript.md)
- TUI 工具与交互目标：[tui-chat-workbench.md](tui-chat-workbench.md)
- SubAgent 目标展示：[tui-subagent-activity.md](tui-subagent-activity.md)
- 跨模块契约：[architecture-contracts.md](../standards/architecture-contracts.md)
