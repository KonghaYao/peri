# Peri Swarm 对齐官方 Agents SDK 验收（分页、状态、事件流）

- 状态：代码与契约测试证据记录；无活体 Loom/Rivet engine，**无真实官方 SDK 进程调用**
- 日期：2026-11-21
- 基线：`05d93fe1e`（`feat/swarm-official-p1a` 分支基点）+ 本记录同一提交中的工作树改动；
  第二轮复核基线为 `d06e1a124`（判 FAIL 的上一轮实现），对应改动见 §7；第三轮复核
  基线为 `155441f59`，对应改动见 §8
- 范围：按 `openai@7.60.0` Agents API 的实际读取行为，补齐 items 分页、会话状态、
  官方 `agent.session.*` 事件（实时与 backlog 重放）以及 `POST /events` 的 `202`
  边界。既有自定义 `item`/`turn` 帧、错误码与门禁不变。
- 参考源码（只读，未修改）：`/Users/mino/code/peri/.claude/dogfood/node_modules/openai`

## 1. 官方 SDK 事实依据

| 事实 | 源码位置 |
| --- | --- |
| `CursorPage` 只读 `body.data` / `body.has_more`；下一页 `after = data[last].id` | `src/core/pagination.ts:154-200` |
| `ItemListParams extends CursorPageParams`（`after?: string`、`limit?`）并增加 `order` | `src/resources/beta/agents/sessions/items.ts:259-266` |
| `sessions.stream()` 先 `sessions.retrieve`，`status !== 'idle'` 直接抛错；随后 `events.stream` 再 `events.create` | `src/lib/agents/agent-session-stream.ts:168-192` |
| `TurnState` 以 `agent.session.turn.created`（`turn.subagent_id === null`）选择 `turn_id`；按 `event_id` 去重（1024 窗口）；只在 `turn.completed/failed/cancelled` 后把 `agent.session.idle` 视为终态，`agent.session.failed` 本身即终态 | `src/lib/agents/turn-state.ts:13-52` |
| `events.create` 返回 `APIPromise<void>`；文档写明 “HTTP 202 confirms acceptance, not durable completion” | `src/resources/beta/agents/sessions/events.ts:24-59` |
| `turn.created` / `turn.item.added` / `turn.completed` / `idle` 帧字段（`event_id`、`session_id`、`turn_id`、`turn`、`item`、`output_index`、`session`） | `src/resources/beta/agents/agents.ts:3076-3120`、`3186-3235`、`2936-2975`、`2188-2215` |
| `Turn` 结构（`id`、`object`、`agent_id`、`created_at`、`started_at`、`completed_at`、`status`、`subagent_id`、`error`、`usage`） | `src/resources/beta/agents/sessions/turns/turns.ts:294-365` |

`turn.item.added` 只承载 item；`TurnState.call()` 只消费 `item.type === 'function_call'`。
本地没有对应事实，因此官方层不投影 `function_call`、`requires_action` 或子代理 turn。

## 2. 改动

| 位置 | 改动 |
| --- | --- |
| `peri-swarm/src/domain/official.rs`（新增） | 官方 `Turn`/`AgentSession`/message item/事件类型；`turn_id_for_item` 与六个 `event_id` 派生函数；RFC3339→Unix 秒 |
| `peri-swarm/src/domain/official_test.rs`（新增） | id 派生、序列化往返、content block、`SessionStatus::project`、时间转换 |
| `peri-swarm/src/domain/session.rs` | `SessionStatus{Idle,InProgress,Failed}` 与 `project(state, turn_active)`；`SessionView.status` |
| `peri-swarm/src/domain/item.rs` | `ItemPage` 增加 `data`；手写 `Serialize` 同值输出 `hasMore` 与 `has_more`；`ItemsWindow.after_id` |
| `peri-swarm/src/domain/event.rs` | `TurnEvent` 携带 `turn_id`；`StreamEvent::Official` |
| `peri-swarm/src/store/{mod,memory,turso}.rs` | `swarm_get_item`：按 `(logical_id, item_id)` 取最大 `seq` 的行 |
| `peri-swarm/src/http/request.rs` | `after` 接受十进制或 `itim_...`；畸形游标 `400 invalid_query`；与 `afterSeq` 不一致 `400 conflicting_cursor` |
| `peri-swarm/src/http/handlers.rs`、`sse.rs` | `202` 受理后由独立任务执行 turn；官方帧按 `event_type()` 输出 SSE 事件名 |
| `peri-swarm/src/service/mod.rs` | `AcceptedInput`/`PendingTurn`；`resolve_window_after`；`replay_backlog` 与 `OfficialReplay::render`（按行重建 turn 边界、多 assistant 帧、失败终态、中段游标播种） |
| `peri-swarm/src/service/in_memory.rs` | `TurnPolicy::{Immediate,Manual,Deferred}`；状态投影；官方帧发出；回放走同一 `replay_backlog` |
| `peri-swarm/src/actor/reader.rs` | 受理只落 user item；“turn 真正开始”时才发 `turn.created` + 该 item 的 `turn.item.added`；终态帧紧接自定义 `turn` 帧 |
| `peri-swarm/src/actor/{service,swarm}.rs` | `accept_input`/`run_pending_turn`；`Submit` 用 `ctx.wait_until` 在 202 后执行；`status` 投影；回放用 `replay_backlog` 并传入实时是否仍有在途 turn |
| `peri-swarm/src/service/in_memory_test.rs` | 回放契约：多 assistant 帧同 turn、失败 turn 不伪装成功、中段游标播种、实时 turn 不补终态 |
| `peri-swarm/src/event_stream.rs` | actor→gateway hop 透传 `agent.session.*` 事件名 |
| `peri-swarm/Cargo.toml`、`Cargo.lock` | 新增 `chrono`（仅用于 RFC3339 解析）；lockfile 只增加 peri-swarm→chrono 依赖边 |

## 3. 行为矩阵

### 3.1 items 分页

| 请求 | 结果 |
| --- | --- |
| `GET /items?limit=2` | `object:"list"`；`limit` 计 item 数，`items` 是本页 2 个 item 的全部原始行，`data` 是这 2 个 item 各自的最后一行；`hasMore` 与 `has_more` 同值；`nextCursor` 是本页最后一个 item 的最大 `seq`（仅 `has_more=true` 时非 `null`） |
| `?limit=2&after=<上一页最后一项的 id>` | 解析该 `item_id` 的最大 `seq`，只返回最大 `seq` 严格大于它的 item |
| `?after=itim_...&afterSeq=<同值 seq>` | 一致，正常返回 |
| `?after=itim_...&afterSeq=<不同值>` | `400 conflicting_cursor` |
| `?after=not-a-cursor` | `400 invalid_query` |

`id`/`itemId` 相同的多行（assistant delta 与 complete）属于同一个 item：`data`
只取最大 `seq` 的那一行，`items` 保留全部行，因此 `data.len() <= limit` 而
`items.len()` 可以更大。分页归属按 item 的最大 `seq` 判定（升序 `>`、降序 `<`），
`after=<itemId>` 因此严格越过整个 item，不会在同一 item 上循环。

### 3.2 会话状态

| 会话事实 | `status` |
| --- | --- |
| 进程内无在途 turn，行状态 `idle`/`creating` | `idle` |
| 有在途 turn（含已受理、尚未开始执行） | `in_progress` |
| 行状态 `failed` 且无在途 turn | `failed` |
| 任何情况 | 不发送 `requires_action` |

### 3.3 官方事件映射

| 官方事件 | 发出时机 | `event_id` |
| --- | --- | --- |
| `agent.session.turn.created` | turn 真正开始（`begin_turn`），受理时**不发** | `evt_<turnId>_created` |
| `agent.session.turn.item.added` | user item 开启/steer；assistant `complete:true` 落库后 | `evt_item_<seq>_added` |
| `agent.session.turn.completed` | 成功终态 | `evt_<turnId>_completed` |
| `agent.session.turn.failed` | 失败终态 | `evt_<turnId>_failed` |
| `agent.session.idle` | 紧接 `turn.completed` | `evt_<turnId>_idle` |
| `agent.session.failed` | 紧接 `turn.failed` | `evt_<turnId>_session_failed` |

`turnId = turn_<打开 turn 的 user itemId 去掉 itim_ 前缀>`；它不取自 ACP
`messageId`，也不含时钟或随机源。`event_id` 只由该 `turnId` 或持久化 `seq` 派生，
所以实时与 backlog 重放给出完全相同的 identity。`receipt`、thought、tool 帧只留在
网关自定义面，不进入 `turn.item.added`。

回放按行重建 turn 边界（`OfficialReplay::render`）：

- user item 在「没有打开的 turn」或「打开的 turn 已落 assistant 完成帧」时开启新
  turn；否则是该 turn 的 steer。
- 一个 turn 的所有 `complete:true` assistant 帧都挂同一 `turn_id`；终态在下一 turn
  的 `turn.created` 之前或回放末尾发出。最后一个 turn 仅在会话行仍为 `failed` 时
  补 `turn.failed` + `agent.session.failed`（重放的 `error`、`completed_at` 为
  `null`）。
- 回放时该 turn 仍在实时执行（存在在途 turn）则不补终态，避免 SDK 提前结束。
- 重连游标落在 turn 中间时，lookback 向游标之前回看并跳过 steer，找到仍打开该 turn
  的 opener，`turn.item.added` 仍带同一 `turn_id`，不重复 `turn.created`。
- 窗口为空（游标在末尾）时，会话行 `idle`/`failed` 仍补最后 turn 的终态与
  `idle`/`failed`；重复终态由 `event_id` 去重。
- 无游标订阅不回放（见 §7.1）。

实现差异：内存 fake 在 `accept_input` 内同步执行 turn，因此 `turn.created` 紧跟
user item（actor 路径推迟到 `begin_turn`，避免已受理但从未执行的 turn 留下无终态的
`turn.created`）；两条路径最终序列一致（`created → item.added → ... →
completed/failed → idle/failed`）。`TurnPolicy::Immediate` 下 202 的 `lastItemSeq`
已包含 fake 的 assistant 帧，与生产路径「受理时刻的 MAX(seq)」不同，仅测试替身如此。

### 3.4 `POST /events` 的 `202`

| 阶段 | 行为 |
| --- | --- |
| 受理前校验失败 | 同步 `400`（空输入/畸形请求） |
| 同 key 不同内容 | 同步 `409 input_conflict`，不入队、不追加 item |
| 需要 `session/resume` | 同步 `502 session_resume_required` |
| 受理成功 | receipt、user item、turn 预留持久化后返回 `202`；turn 在响应后执行 |
| turn 执行失败 | 流上 `turn.failed` + `agent.session.failed`；已返回的 `202` 不追认更改 |

响应中的 `lastItemSeq` 是受理时刻的 `MAX(seq)`（通常只含本条 user item），
不是终态游标。

## 4. 实际执行的命令与结果

命令均在 worktree 内执行，环境为
`PATH=$HOME/.local/share/uv/python/cpython-3.12.14-macos-aarch64-none/bin:$HOME/.cargo/bin:$PATH`、
`CARGO_HOME=/tmp/cargo-home-rsproxy`（rsproxy 镜像与补丁 crate 配置）。

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `cargo check -p peri-swarm --all-targets --locked` | 通过，exit 0，`Finished dev profile` |
| 2 | `cargo test -p peri-swarm --locked` | 通过，0 failed：lib 196、`actor_runtime_test` 7（1 ignored）、`http_contract_test` 25、`items_pagination_test` 7、`loom_store_test` 0（3 ignored）、`mcp_injection_test` 4、`openai_hosted_test` 7、`rivet_actor_test` 0（2 ignored）、`streaming_test` 12、doc-tests 0 |
| 3 | `cargo clippy -p peri-swarm --all-targets --locked -- -D warnings` | 通过，exit 0 |
| 4 | `python3 scripts/check-feature-catalog.py` | `Documentation: PASS (structure and destinations; not runtime acceptance)` |
| 5 | `git diff --check` | 通过，exit 0，无输出 |

新增/调整的断言：

- `tests/items_pagination_test.rs`：`data == items`、`has_more`、`after=itim_...`
  第二页、与 `afterSeq` 的一致/冲突、畸形游标。
- `tests/http_contract_test.rs`：`GET /sessions/{id}` 的 `idle`/`in_progress`/`failed`
  三态；`TurnPolicy::Deferred` 下 `202` 只含 user item，assistant item 在响应之后出现。
- `tests/streaming_test.rs`：实时
  `turn.created → item.added(user) → item.added(assistant) → turn.completed →
  agent.session.idle`；`event_id` 与 backlog 重放逐项相等。
- `src/actor/reader_test.rs`、`src/service/in_memory_test.rs`、
  `src/domain/item_test.rs`、`src/event_stream.rs`：单元级顺序、`event_id`、
  双拼写与 hop 透传。
- `src/service/in_memory_test.rs` 的回放契约：同一 turn 多条 assistant 帧、失败 turn
  带已落库 assistant 帧仍为 `turn.failed`、中段游标播种后 `event_id` 不变、实时
  turn 不补终态。

## 5. 未验证项与残余风险

1. **没有真实官方 SDK 进程。** 断言形状取自上述源码与类型定义；未启动
   `openai@7.60.0` 客户端消费本服务。因此不证明 SDK 运行时的额外校验（例如
   `sessions.retrieve` 的其它必填字段、事件联合类型的运行时分支）。
2. **没有活体 Loom/Rivet engine。** 集成测试使用 `InMemorySwarmService`；actor 路径
   由 `actor_runtime_test`/`rivet_actor_test` 在无 engine 条件下覆盖，未验证跨副本
   唤醒后的实时/重放一致性。
3. **backlog 的 `turn.failed` 只能按会话行状态重建。** 会话行只保留最新状态：
   **只有最近一个 turn 的失败能被如实回放，更早的失败会显示为 `completed`**。本轮
   未新增表或列区分历史成败（需单独批准），这是明确保留的限制，不是已修复项。
   失败原因未持久化，重放的 `agent.session.failed.session.error`、
   `turn.error` 与 `turn.completed_at` 为 `null`；实时失败帧的 `turn.error` 已由
   失败消息填充（`{code:"internal_error",message}`）。
4. **回放无法区分 steer 与「上一 turn 已输出后开启的新 turn」。** 行里没有 turn
   边界标记：打开的 turn 已落 assistant 完成帧时，后续 user item 按新 turn 处理；
   否则按 steer 处理。真实 steer 若发生在已有输出之后，回放会多出一个 turn 边界
   （实时流不受影响）。lookback 只修复了游标落在 steer 之前、且该 turn 尚无
   assistant 完成帧的情况。
5. **宿主崩溃遗留的 `running` 行。** 行状态为 `running` 且此刻没有在途 turn 时，回放
   仍按「已闭合 turn → completed」处理；它无法证明该 turn 曾正常结束。`GET` 的
   `status` 同时报 `in_progress`，两者都只是从持久化事实的推断。
6. **`agent.session.*` 的 `session` 载荷是子集。** 已含 `id`/`object`/`status`/
   `created_at`/`last_active_at`/`error`/`agent{id,model,instructions}`/`metadata`/
   `required_actions`/`usage`/`vault_ids`，缺官方必填的 `environment`；`agent.model`
   在部署未记录时为 `null`（官方类型为 `string`）。SDK 的 `TurnState` 只读
   `session.status`/`session.id`，运行时不校验该形状。
7. **`usage` 恒为 `null`。** 本地没有 token 计量事实，未做假值。
8. **`sandbox.state` 恒为 `null`。** `swarm_bindings` 不持久化 SRT 状态；`runId` 在
   绑定时填充。实时状态仍以 Bun 的 `GET /v1/sandboxes/{logicalEnvId}` 为准。
9. **未实现** `turn.cancelled`、budget、subagent turn 与官方 `function_call` item；
   `requires_action` 永不发送。官方 SDK 若严格依赖这些帧，会停在等待态。
10. **慢订阅者 `gap`。** 本轮改为发一帧 `gap` 后关闭订阅（见 §7.5），单测覆盖
    「Lagged → gap → EOF」。多订阅者容量与真实慢客户端仍未压测。
11. **分页 id 语义依赖 item 分组。** 分组键是 `(logical_id, item_id)`：若同一
    `item_id` 跨 turn 复用（assistant 身份按 `messageId` 聚合，跨 turn 不保证
    唯一），这些行会被视为同一个 item，`data` 只保留最大 `seq` 的一行。当前
    网关生成的 identity 每 turn 唯一；出现复用需重新定义分组键。
12. **内存 fake 的 `202` 时序**：`TurnPolicy::Immediate` 在 `accept_input` 内同步跑完
    turn，响应里的 `lastItemSeq` 已含 assistant 帧；`202`-before-terminal 由
    `TurnPolicy::Deferred` 与 HTTP 契约测试覆盖，actor 路径由 `ctx.wait_until` 覆盖，
    但未在真实 model 延迟下计时。
13. **无游标语义是行为变更。** 依赖「不带游标即从头回放」的旧客户端必须显式传
    `afterSeq=0`（或 `after=0`/`Last-Event-ID: 0`）。另外，十进制 `after`/`afterSeq`
    现在也按 item 边界解释：游标落在多帧 item 中间时，该 item 整条进入结果页。
    `peri-swarm/README.md` 本轮未改，其 `after` 等于/超过末尾「只有心跳」一行只
    描述 `item`/`turn` 帧；官方 `agent.session.*` 终态补发是附加帧，见 §7.3。
14. **`after=<itemId>` 不会返回请求之后追加的帧。** 该游标在请求时解析为 item 当时的
    最大 `seq`；翻页期间同一 item 继续落帧时，新帧位于游标之后，不会被下一页返回
    （其他最大 `seq` 低于该值的 item 也会被越过）。这是 item 级游标的既定语义，
    不做「增长中的 item 在下一页重新给出」的承诺；需要从最后已渲染行续读时用
    `afterSeq=<seq>`，它按 item 边界判定并整条重给包含该行的 item。见 §8.2。

## 6. 证据边界

- 单测证明 id 派生、序列化、状态投影与事件编码；契约测试证明 HTTP 状态码、
  响应字段、SSE 顺序与重放 identity。
- 均不证明真实 SDK 兼容性、真实 Loom/Rivet 行为或跨进程恢复。
- `Cargo.lock` 的唯一改动是 `peri-swarm` 依赖列表新增 `chrono`；`chrono` 本身早已在
  lockfile 中。

## 7. 第二轮复核：游标与回放边界修正

独立验证对 `d06e1a124` 判 FAIL。以下缺陷均已复现并以本轮改动修复，证据是本节的契约
测试与 §7.7 的命令结果。

### 7.1 无游标订阅不再回放

缺陷：`GET /events` 不带任何游标时按 `Cursor(0)` 回放全部。官方 `sessions.stream`
先订阅再 POST，旧 turn 的 `turn.completed` + `agent.session.idle` 进入新流的
`TurnState`，SDK 在第一个历史 turn 的 idle 结束，看不到新 turn。

修复：`EventCursor::{Tail, After}`（`domain/cursor.rs`）。没有任何游标
（`afterSeq`/`after`/`Last-Event-ID` 全缺）是 `Tail`：backlog 为空，只送实时事件。
显式 `afterSeq=0` 仍是 `After(0)`，回放全部。actor hop 在 `Tail` 时不发
`afterSeq`，网关 `open_stream`（`POST /sessions?stream=true` 前打开）也明确使用
`Tail`，两条路径都是「不回放」。依赖「无游标即回放」的测试改为 `?afterSeq=0`。

### 7.2 items 按 item 分组分页

缺陷：`after=<itemId>` 解析为该 id 的最大 `seq` 后仍按行分页；同一 assistant
消息的 delta 与 complete 共享 `itemId`，升序会跳过中间帧，降序 `limit=1` 会在同一
item 上反复命中，`has_more` 恒真。

修复：`SwarmStore::swarm_item_page`（memory 与 Turso 都实现）以 item 为单位选页：
`GROUP BY logical_id, item_id` + `MAX(seq)`，升序 `MAX(seq) > after`、降序
`MAX(seq) < after`，再 join 回这些 item 的全部行；`limit` 计 item 数，`has_more`
按 item 计算，`nextCursor` 是页内最后一个 item 的最大 `seq`。`data` 是每 item
最大 `seq` 的一行，`items` 是全部原始行，二者数量可以不同（长消息 3 行、`limit=1`
时 `items` 3 行、`data` 1 行）。分组在存储层完成，不存在「先取 `limit` 行再前端
去重」导致的空页。`afterSeq` 与 `after` 不一致仍走
`400 conflicting_cursor`。第三轮为整页增加聚合行预算（`limit` 同时限制总行数），
见 §8.3；`after=<itemId>` 的增长语义见 §8.2。

### 7.3 lookback 播种与末尾终态

缺陷与修复：

| 场景 | 修复 |
| --- | --- |
| 窗口首行是 steer user item | 向游标之前回看，跳过 steer，找到仍打开该 turn 的 opener，用它的 `turn_id` 播种；该 user item 作为 steer 发出 `turn.item.added`，与实时一致，不再派生 `turn_input_<steer>` |
| 只有 tool 帧、无 assistant 完成帧的 turn | 同一 lookback 找到 opener 并播种；会话行 `idle` 时按「已闭合且无输出 → `completed`」补 `turn.completed`+`idle`，`failed` 时补失败终态 |
| `afterSeq` 等于尾部 seq（窗口为空） | 会话行 `idle`/`failed` 且无在途 turn 时，从最后一批 item 推导最后 turn，补发终态与 `idle`/`failed`；`event_id` 不变，客户端去重 |

判定依据：回放的行边界规则是「user item 在无打开 turn、或打开 turn 已落
assistant 完成帧时开启新 turn」。lookback 用同一规则反向走：从游标（含）向前，
只有当一个 assistant 完成帧把候选 user item 与更早的 user item 分开时，候选才是
opener。`turn_id` 始终由 opener 的 item id 派生，不用随机或时间戳。播种不重发
`turn.created`：新订阅的 `TurnState` 会选中第一个 `turn.created`，重发旧 turn
会让它在旧 turn 的终态提前结束。空窗口补发的终态因此不带 `turn.created`，只对
已经历该 turn 的客户端构成幂等重放。

### 7.4 历史失败 turn（中 4）

未新增表或列：会话行只保留最新状态，**只有最近一个 turn 的失败能被如实回放，
更早的失败会显示为 `completed`**。这是验收限制（§5-3），不是已修复项。

### 7.5 慢订阅者 Lagged（中 5）

官方流无法区分客户端，因此两条 SSE 写路径统一处理：检测到
`broadcast::RecvError::Lagged` 时发一帧 `gap` 后关闭订阅。丢失的事件可能包含
终态；官方 SDK 以「EOF before idle/failed」抛错识别中断，不再静默挂起。原生
客户端用最后一个已渲染 item 的 `seq` 重连恢复。单测覆盖
`Lagged → gap → EOF`。

### 7.6 其余记录项

- 低 6：`OfficialSession`/`OfficialSessionAgent` 仍是官方载荷的子集（缺
  `environment` 等字段，`agent.model` 可为 `null`）。本轮不扩字段；`TurnState`
  只读 `session.status`/`id`，不校验该形状。记录为限制。
- 低 7：items 默认 `order=asc` 与官方默认 `desc` 不同；保持既有客户端行为，
  不改默认值。记录为限制。
- 低 8：实时 `turn.failed` 的 `turn.error` 现在填
  `{code:"internal_error", message}`（消息来自已有失败文本）；重放没有持久化文本，
  保持 `null`。`agent.session.failed.session.error` 的实时文本不变。

### 7.7 实际执行的命令与结果

环境同 §4（`PATH` 含 CPython/`cargo`，`CARGO_HOME=/tmp/cargo-home-rsproxy`）。
默认并行与 `--test-threads=1` 各跑一次全量。

| # | 命令 | 默认并行 | 单线程 |
| --- | --- | --- | --- |
| 1 | `cargo check -p peri-swarm --all-targets --locked` | 通过，exit 0 | — |
| 2 | `cargo test -p peri-swarm --locked` | 通过：lib 207、`actor_runtime_test` 7（1 ignored）、`http_contract_test` 25、`items_pagination_test` 10、`loom_store_test` 0（3 ignored）、`mcp_injection_test` 4、`openai_hosted_test` 7、`rivet_actor_test` 0（2 ignored）、`streaming_test` 14、doc-tests 0；合计 274 passed，0 failed | 相同计数，0 failed |
| 3 | `cargo clippy -p peri-swarm --all-targets --locked -- -D warnings` | 通过，exit 0 | — |
| 4 | `python3 scripts/check-feature-catalog.py` | `Documentation: PASS (structure and destinations; not runtime acceptance)` | — |
| 5 | `git diff --check` | 通过，exit 0，无输出 | — |

`openai_hosted_test::an_anonymous_hosted_session_reserves_a_bun_workspace` 是已知
并行 flaky（不在本 diff）；本轮两次全量运行均通过，未为它改无关代码。

新增/调整的断言：

- `tests/streaming_test.rs`：无游标订阅只收实时、不因历史 idle 终止；
  `afterSeq=0` 显式回放；`afterSeq=<tail>` 补发最后 turn 的
  `turn.completed`+`idle`（无 `turn.created`）；旧的「无游标回放」测试改为显式
  `afterSeq=0`；「先订阅再 POST」用无游标订阅。
- `tests/items_pagination_test.rs`：`data` 按 item 去重且短于 `items`；
  升序/降序 `limit=1` 逐页遍历，`nextCursor` 严格前进并终止；长消息跨 3 行仍只
  占 1 个 item。
- `src/service/in_memory_test.rs`：无游标无 backlog；steer 中段游标沿用 opener
  的 `turn_id`；tool-only turn 补终态；空窗口补发终态；已输出后下一个 user item
  开启新 turn；重放失败 `error` 为 `null`。
- `src/actor/reader_test.rs`：实时 steer 帧带打开 turn 的 `turn_id`；实时
  `turn.failed` 的 `turn.error` 含失败文本。
- `src/store/memory_test.rs`、`src/store/turso_test.rs`：item 分组分页
  （`items`/`data` 数量、item 边界、无游标降序）与分组 SQL 形状。
- `src/http/sse_test.rs`：`Lagged` 后只发一帧 `gap` 并结束流。
- `src/domain/cursor_test.rs`：无游标 → `Tail`；显式 `0` → `After(0)`。

### 7.8 本轮未验证

1. 仍未用真实 `openai@7.60.0` 客户端消费本服务；官方 `sessions.stream` 的行为
   依据仍是源码阅读。
2. writer 侧的 Loom 实现只做了 SQL 形状与离线单测；无活体 Loom 的 item 级分页
   往返。
3. 多订阅者容量、真实慢客户端与重连风暴未压测。

## 8. 第三轮复核：create-stream 首轮与 item 页容量

独立复验在 `155441f59` 上发现三个问题。**A 是既有缺陷（`05d93fe1e` 起），本轮修复，
不归因于本分支**；B/C 是本分支上一轮引入的语义/容量问题。

### 8.1 A：真实 actor 路径的 create-stream 丢掉首轮事件（既有，本轮修复）

复现路径（验证者给出并复核）：

1. `POST /v1/agents/sessions`，`stream:true`。
2. `handlers::create_session` 先 `open_stream`，真实 actor 收到 `GET /events`（无
   `afterSeq`，即 `EventCursor::Tail`）。
3. actor `events_response` → `AcpSwarmService::replay_after` → 会话行尚不存在，
   `require_row` 返回 `unknown_agent` → actor 回 `503`。
4. gateway 的 `open_stream` 把失败映射成已关闭的 `broadcast`。
5. `create_session` 随后写行并跑首轮；SSE 只剩初始 `session` 事件，首轮
   `item`/`turn`/官方帧全丢。

内存 fake 的 `open_stream` 不读行，因此既有测试没有暴露该顺序。根因自
`05d93fe1e` 的 actor `replay_after` 就存在。

修复：`AcpSwarmService::replay_after` 在 `EventCursor::Tail` 时先返回空 backlog，
不读会话行；行检查只保留给显式游标（未知会话仍是 `unknown_agent`）。内存 fake 的
`replay_after` 采用同一规则，避免两个实现再次分叉。gateway 的 `open_stream` 因此
拿到 actor 的 `200` SSE 并转发真实活跃订阅，不再退回已关闭广播。

测试：`tests/actor_runtime_test.rs::tail_subscription_before_the_row_keeps_the_first_turn`
（非 ignored）。它按 actor 端点的顺序执行 `open_stream` → `replay_after(Tail)` →
带初始输入 `create_session`，断言 tail 回放为空、显式游标在无行时仍报
`unknown_agent`，并收到首轮 user item、assistant 完成帧与 `completed` 终态（一次
真实 host+stub 模型调用）。

后续修复（`feat/swarm-official-p1a`）：上述修复只保证订阅在首轮开始前建立；
`create_session` 仍在返回 SSE 响应之前同步跑完首轮。receiver 在响应到达客户端前
没有消费者，首轮事件数超过广播容量时首次读取得到 `Lagged`，SSE 发一帧 `gap` 后
关闭；未溢出时也只是首轮结束后一次性投递，不是实时流。现在流式 create 在 service
内持久化初始 user item 与 `turn_id` 后立即返回：actor 用 `ctx.wait_until` 调度该
turn，in-process fake 由 handler `tokio::spawn`；网关先构造并返回 SSE 响应
（headers 与初始 `session` 帧），首轮随后在同一订阅上执行。首轮事件在广播容量内
实时送达；客户端读取过慢或首轮突发超过容量时，按既有 `gap` + EOF 语义显式失败，
可按最后 item `seq` 重连。措辞边界：actor 路径的 action reply 与 gateway 响应构造
之间仍有一个短窗口（`wait_until` 在 reply 后即可开始），所以不承诺「首轮一定晚于
客户端读到响应」，只承诺订阅先于首轮建立、响应构造不等待首轮完成（§9.2）。覆盖：

- `tests/streaming_test.rs::stream_create_subscribes_before_the_first_turn` 按顺序
  断言 `agent.session.turn.created`、user 与 assistant 的 `turn.item.added`、
  `agent.session.turn.completed`、`agent.session.idle`，且 SSE name 与 JSON `type`
  一致、没有 `gap`。
- `service::in_memory::tests::deferred_create_returns_the_first_turn_without_running_it`
  与 `deferred_first_turn_streams_the_complete_turn_when_run`：defer 返回时只有
  user item，`run_pending_turn` 后出现完整首轮。
- `tests/actor_runtime_test.rs::deferred_create_returns_before_the_first_turn_executes`
  覆盖 actor 新行路径；`creating_row_with_a_persisted_peri_session_reattaches_without_replaying`
  以 defer 重试恢复窗口，断言已有 item 时不重复提交。

证据边界：没有 Rivet engine，`SwarmActor::events_response` 与 gateway 的
`RivetSwarmService::open_stream` 之间的真实 HTTP 回调未在本轮运行；测试覆盖的是
该回调调用的同一 service 序列。新增的 HTTP 级 create-stream 断言使用
`InMemorySwarmService`；actor 侧的 defer 语义由 `tests/actor_runtime_test.rs` 直接
驱动 service 覆盖，不含 gateway 的 HTTP 回调。engine 下的 create-stream 场景仍标记
未验证。

### 8.2 B：`after=<itemId>` 与增长中的 item

上一轮的文档与测试声称「增长中的消息会在下一页以同一 `itemId` 再给出」，与实现
矛盾：`after=<itemId>` 在请求时解析为该 item 的当前最大 `seq`，之后追加的帧都在
游标之后，下一页不会返回它们；最大 `seq` 低于该值的其他 item 也会被越过。

决策：不改游标语义（item 级游标就是「越过整个 item」）。已改写
`docs/design/peri-swarm.md` §4.2.2 与本记录 §5-14，明确：

- `after=<itemId>` 解析为请求时的最大 `seq`，之后追加的帧不再由该游标返回；
- 从最后已渲染行续读用 `afterSeq=<seq>`；它同样按 item 边界判定，会把包含该行的
  item 整条重给（含新帧），因此能看到最终帧。

测试：`tests/items_pagination_test.rs::item_id_cursor_does_not_cover_frames_appended_later`
固化两侧行为（`after=<itemId>` 的下一页为空；`afterSeq=2` 的下一页包含后续追加的
最终帧）。

### 8.3 C：官方 `data` 让页失去行数上限（容量回归）

上一轮按 item 分组后返回页内所有 item 的全部原始行：`limit=1` 遇长消息可返回数千
行，且 `items`/`data` 同时序列化，构成容量回归。

修复：`limit` 同时是 item 数上限与整页聚合行预算。`grouped_item_page` 按 item 顺序
累加原始行数，达到或超过 `limit` 行后不再纳入下一个 item；`has_more` 仍按 item
计算，`nextCursor` 不变。**单 item 例外**：第一个 item 总是纳入，因此它自身行数
超过预算时整条返回，避免丢掉最终帧；这是已知边界，`items.list` 是 item 取向接口，
delta 帧应走 SSE。

测试：

- `tests/items_pagination_test.rs::row_budget_stops_before_the_next_item`：`limit=2`、
  第一个 item 3 行、第二个 item 1 行 → 第一页只含第一个 item（`data` 1、`items`
  3、`has_more=true`），第二页返回第二个 item。
- `tests/items_pagination_test.rs::item_grouped_pages_keep_data_shorter_than_items`
  按行预算更新：首页只含 3 行的长消息，第二页才给出后两个 item。
- `src/store/memory_test.rs::item_page_spends_limit_as_an_aggregate_row_budget`：
  存储层行预算、`nextCursor` 与单 item 例外。

Turso 侧复用同一 `grouped_item_page`，SQL 选页仍取 `limit+1` 个候选 item；行预算
只在已取到的候选行上生效，不需要额外查询。

### 8.4 实际执行的命令与结果

环境同 §4。默认并行与 `--test-threads=1` 各跑一次全量。

| # | 命令 | 默认并行 | 单线程 |
| --- | --- | --- | --- |
| 1 | `cargo check -p peri-swarm --all-targets --locked` | 通过，exit 0 | — |
| 2 | `cargo test -p peri-swarm --locked` | 通过：lib 208、`actor_runtime_test` 8（1 ignored）、`http_contract_test` 25、`items_pagination_test` 12、`loom_store_test` 0（3 ignored）、`mcp_injection_test` 4、`openai_hosted_test` 7、`rivet_actor_test` 0（2 ignored）、`streaming_test` 14、doc-tests 0；合计 278 passed，0 failed | 相同计数，0 failed |
| 3 | `cargo clippy -p peri-swarm --all-targets --locked -- -D warnings` | 通过，exit 0 | — |
| 4 | `python3 scripts/check-feature-catalog.py` | `Documentation: PASS (structure and destinations; not runtime acceptance)` | — |
| 5 | `git diff --check` | 通过，exit 0，无输出 | — |

`openai_hosted_test::an_anonymous_hosted_session_reserves_a_bun_workspace` 的已知并行
flake 在本轮两次全量中均未出现；未为它改无关代码。

### 8.5 仍存限制与未验证项

1. **历史失败 turn**：会话行只保留最新状态，只有最近一个 turn 的失败能如实回放，
   更早的失败显示为 `completed`（未新增表/列）。
2. **官方载荷子集**：`OfficialSession`/`OfficialSessionAgent` 缺官方字段，
   `agent.model` 可为 `null`，`usage` 恒 `null`；`TurnState` 只读少量字段。
3. **items 默认 `order=asc`** 与官方默认 `desc` 不同，不改默认值。
4. **Lagged 后不可恢复**：检测到丢事件时发 `gap` 并关闭订阅；官方 SDK 以 EOF
   中断，同一连接无法续读中间 turn，重订阅从 tail 或显式游标开始，服务端不会重发
   在途 turn 的 `turn.created`。
5. **lookback 扫描成本**：正常会话只回看到上一 turn 的完成帧；若当前 turn 从未产生
   assistant 完成帧，游标之后的订阅可能反向扫描到会话开头，成本随该 turn 的行数
   线性增长，未做行数上限。
6. **`data` 仍是事件级 `ItemView`**：每项是我们的事件级行（含 `payload.delta` 等），
   不是官方高层 item 联合类型；官方 SDK 的 `CursorPage` 只读 `id`，类型化消费者
   不能把 `data[]` 当官方 item。
7. **真实 engine/Turso 未运行**：create-stream 场景只在 service 序列上覆盖；Turso
   的 item 分组/行预算只有 SQL 形状与离线单测，无活体 Loom 往返；真实官方 SDK
   进程仍未启动。
8. **steer 在已有输出之后**：回放仍可能多一个 turn 边界（设计 §4.2 已记录）。

## 9. 第四轮复核：运行中会话的 create 与首轮措辞

复验基线 `d5cae0008`（`fix(swarm): 流式创建先返回 SSE 再执行首轮`）。本轮修复一个
中高缺陷与一处文档措辞，均不改变正常 idle 会话的 create 语义、`created` 标志或
item/turn 身份规则。

### 9.1 缺陷 1：actor 既有行的 streamed create 静默失败并留下 stray item

复验证据（源码确定）：`AcpSwarmService::create_session` 的既有行路径在
`ensure_live` 成功后直接调用 `accept_initial_input`；该函数先
`record_user_input(None, text)`，没有检查 `runtime.is_running()` 或
`runtime.log.has_open_turn()`。`defer=true` 时它返回 `PendingTurn`，actor
`Handler<Create>` 用 `wait_until` 调 `run_pending_turn` → `run_turn`，admission
发现 `is_running()` 后返回 `turn_in_progress`，调用处只记 warning。结果：HTTP 已
返回 `200` SSE，会话日志多一条永不执行的 user item，既无 `turn.failed` 也无其他
终态，客户端一直等待。in-memory 的既有行路径在写 item 之前检查行状态并返回 `409`，
两个实现因此分叉。

修复（`peri-swarm/src/actor/service.rs::accept_initial_input`）：在
`runtime.admission.lock().await` 之下，先检查 `runtime.is_running()`，再检查
`runtime.log.has_open_turn()`（已受理但尚未开始的 turn），任一成立就在写 user item
**之前**返回 `SwarmError::turn_in_progress()`。规则与同文件 `accept_input` 的
admission 段一致，没有第二套判定。检查对 `defer=true`/`defer=false` 同样生效，
因此**非流式 create 在同样场景由原先的「写入 item 后执行失败」收紧为 `409` 且不写
item**；这是本轮有意的行为收紧。`recover_creating_row` 保留 `last_seq == 0` 条件，
窗口内遇到运行中/已预占 turn 时由同一函数返回 `turn_in_progress`，不静默跳过。

判定依据是进程内在途 turn，不是持久化行状态：崩溃后残留的 `running` 行在重新
attach 后没有在途 turn，create 仍可继续接收初始输入。

测试：

- `tests/actor_runtime_test.rs::create_on_a_reserved_turn_is_refused_before_the_user_item`
  （真实 host，无模型调用）：既有 idle 行先 `accept_input` 预占 turn（不执行），
  `defer=false` 与 `defer=true` 的 create 都返回 `TurnInProgress`，item 列表与
  `suggest=steer` 不变，`stub.main_calls()==0`。
- `tests/actor_runtime_test.rs::create_on_a_running_turn_is_refused_before_the_user_item`
  （stub 挂起 3 s）：turn 已进入模型请求后，两种 defer 的 create 都返回
  `TurnInProgress`；turn 结束后会话只有一条 user item（文本为原输入），证明被拒的
  create 没有写入。
- `service::in_memory::tests::create_on_a_running_session_is_refused_before_recording_the_input`：
  in-memory 的 `409` 行为与「被拒后 item seq 不变」。
- `creating_row` 的 `last_seq == 0` 窗口无法确定性构造「零 item + 在途 turn」
  （任何公开受理路径都先写 item）；两个 defer 模式的同一函数测试覆盖该检查本身，
  恢复路径只透传其错误，未为不可复现窗口造时序测试。

### 9.2 缺陷 2：「先返回 SSE，再执行首轮」的措辞过绝对

actor 的 `ctx.wait_until` 在 action reply 后即可开始首轮，gateway 的响应随后才
构造；因此「响应一定先于首轮」不成立。这是有界广播 + `gap` 的既有语义，不是无限
挂起，但文档必须写准。已改写：

- `docs/design/peri-swarm.md` §4.1 方法表、§4.2 慢订阅者、§4.3 创建与首轮、
  §4.4 的恢复与 `202`：先建立订阅并构造/返回响应，再调度首轮；首轮事件在容量内
  实时送达，超容量按 `gap` + EOF 显式失败并可按最后 item `seq` 重连；明确 actor
  路径 action reply 与响应构造之间的短窗口，不承诺首轮晚于客户端读到响应。
- 本文 §8.1 的同一措辞。
- 代码注释：`src/http/handlers.rs`、`src/http/sse.rs`、`src/actor/swarm.rs`
  的 `Handler<Create>`。

### 9.3 新增证据

- **actor 官方帧顺序**：`tests/actor_runtime_test.rs::deferred_first_turn_emits_the_official_frames_in_sdk_order`
  不复用忽略 `StreamEvent::Official` 的 `collect_until_terminal`，在真实
  host + stub 模型上按 defer 首轮收集官方帧，断言顺序为
  `turn.created` → user `turn.item.added` → assistant `turn.item.added` →
  `turn.completed` → `agent.session.idle`；`turn.created.turn.subagent_id`
  为 `null`，user/assistant 的 `turn_id` 与 created 一致，`turn.completed`
  属于同一 turn。
- **`pending_initial` 不跨进程**：`service::in_memory::tests::pending_initial_does_not_cross_the_actor_boundary`
  对一个带 pending 的真实 `CreateSessionResult` 序列化，断言 JSON 不含
  `pendingInitial`，再反序列化得到 `None`（`#[serde(skip)]`），避免 gateway
  误调 `run_pending_turn`。
- **既有 idle 行 defer**：`tests/actor_runtime_test.rs::existing_idle_row_defers_the_first_turn_exactly_once`
  断言复用的 idle 行只写一条 user item、返回 `pending_initial`、`created=false`、
  执行前无 assistant/终态；`run_pending_turn` 后 assistant 完成且 user item 仍为
  一条。

### 9.4 实际执行的命令与结果

环境同 §4（`CARGO_HOME=/tmp/cargo-home-rsproxy`）。

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `cargo test -p peri-swarm --locked --test streaming_test` | 通过：14 passed |
| 2 | `cargo test -p peri-swarm --locked --test http_contract_test` | 通过：25 passed |
| 3 | `cargo test -p peri-swarm --locked --test actor_runtime_test` | 通过：13 passed，1 ignored |
| 4 | `cargo test -p peri-swarm --locked --test items_pagination_test` | 通过：12 passed |
| 5 | `cargo test -p peri-swarm --locked --lib` | 通过：212 passed |
| 6 | `cargo clippy -p peri-swarm --all-targets --locked -- -D warnings` | 通过，exit 0 |
| 7 | `cargo check -p peri-swarm --all-targets --locked` | 通过，exit 0 |
| 8 | `cargo fmt --check` | 通过，exit 0 |
| 9 | `git diff --check` | 通过，exit 0，无输出 |

本轮只跑了上述定向命令，未重跑 `openai_hosted_test` 与全量 `cargo test`；
`openai_hosted_test::an_anonymous_hosted_session_reserves_a_bun_workspace` 的已知
并行 flake 与本次改动无关。

### 9.5 证据边界与残余风险

1. **仍无真实 engine**：没有 Rivet engine，`SwarmActor::events_response` 与 gateway
   `RivetSwarmService::open_stream` 之间的真实 HTTP 回调未运行。actor 官方帧测试
   直接驱动 `AcpSwarmService`（与回调调用同一 service），但不覆盖 engine 的
   action/stream 编解码与定时器。
2. **admission 与 turn 启动的窄竞态**：`accept_initial_input` 在锁内检查并写 item，
   但 `run_turn` 在释放锁后再次取锁、设置 turn slot。窗口内若有另一个输入抢到
   slot，`run_turn` 仍会返回 `turn_in_progress`（此时 item 已写）；正常 create 的
   初始输入紧随其后，实际不可达，但形式上不是「检查即预留」。彻底消除需要把
   slot 预留放在写 item 的同一临界区，本轮未扩大设计。
3. **defer 后进程崩溃**：初始 user item 已持久化；若首轮在 `wait_until` 调度前
   进程退出，`PendingTurn` 随进程消失，没有恢复路径补跑该 turn（重试 create 带
   `input` 时会作为新输入、新 item 再跑一次；不带 `input` 时该 turn 永不执行）。
   这是 `d5cae0008` 已存在的边界，本轮不改。
4. **超容量首轮**：首轮事件超过广播容量时仍以 `gap` + EOF 显式失败，客户端需按
   最后 item `seq` 重连；掉线窗口内的实时官方帧不可恢复（已知限制，§8.5-4）。
