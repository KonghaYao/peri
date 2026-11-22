# Peri Swarm 官方 `events` 信封兼容验收（P0）

- 状态：代码与契约测试证据记录；无活体 Loom/engine，无真实官方 SDK 进程调用
- 日期：2026-11-21
- 基线：`32642434f`（当前 HEAD）+ 工作树中未提交的 M1 改动（`peri-swarm/src/bootstrap.rs`、
  `config.rs`、`bootstrap_test.rs`、`config_test.rs`、`tests/rivet_actor_test.rs`，以及
  M1 文档与未跟踪的 `peri-swarm-multi-replica-2026-11-21.md`）。本记录的改动叠加在该
  工作树上，未回退、未修改 M1 语义。
- 范围：`POST /v1/agents/sessions/{logicalId}/events` 额外接受官方 OpenAI Agents API
  Node SDK（`openai@7.60/7.62`）`client.beta.agents.sessions.events.create` 提交的
  `{events:[...]}` 信封；原生 body 行为保持不变。
- 明确不在本记录内：真实 SDK 进程、真实 Loom/Rivet engine、SSE `sessions.stream()`
  的内部 submit 链路、批量事件原子性。

## 1. 改动

| 位置 | 改动 |
| --- | --- |
| `peri-swarm/src/http/request.rs` | `SubmitEventBody` 增加可选 `events`；`attachments`/`pluginSelections` 改为 `Option` 以区分“缺失”与 `[]`（`present_vec` 仍拒绝显式 `null`）；`resolve_submission` 在 `events` 出现时分派到官方信封解析器；新增事件/message/content 三层显式校验与字段路径诊断 |
| `peri-swarm/src/http/request_test.rs` | 新增 11 个单测（成功映射、key 复用、无 key 生成 UUIDv7、原生路径仍要求 inputId、多事件、多 message、`input_image` 下标、未知事件类型/字段、原生字段混用、空白输入） |
| `peri-swarm/tests/http_contract_test.rs` | 新增 3 个集成测试（带 key → `202` 且 items 文本正确；无 key → `202`；未知事件类型 → `400`）；现有原生 events 断言未改 |
| `docs/design/peri-swarm.md` | §4.1 路由表、约定、§4.2、§4.4 补官方信封与幂等 carve-out |
| `docs/reference/feature-catalog.md` | `F-SWARM-004` 描述补充官方信封单条 message |
| `docs/verification/feature-acceptance.md` | `F-SWARM-004` 证据列追加本记录 |

## 2. 行为矩阵

| 请求形状 | 结果 |
| --- | --- |
| 原生 body（无 `events`），有 `inputId` 或 `Idempotency-Key` | 与改动前完全一致 |
| 原生 body，无 `inputId` 且无 key | `400 invalid_input`（服务端不代生成） |
| `{events:[一条 agent.session.input.message]}`, 一条 `role:"user"` message, 纯 `input_text` | `202`，受理为 `kind=user`、`mode=turn`，多段文本按换行拼接 |
| 官方信封 + `Idempotency-Key: <uuid>` | `202`，`inputId` 等于该头 |
| 官方信封 + 非 UUID `Idempotency-Key` | `400 invalid_input`（`inputId must be a UUID`） |
| 官方信封，无 key | `202`，服务端生成一次性 `UUIDv7` |
| 官方信封 + 任一原生字段（含 `attachments: []`） | `400 invalid_input`，`details.field` 指向该原生字段 |
| `events` 非数组、空数组或多于一条 | `400 invalid_input`，`details.field="events"` |
| 事件类型不是 `agent.session.input.message`（含 `agent.session.input.cancel`、`mcp_approval_request_result`） | `400 invalid_input`，`details.field="events[0].type"` |
| 事件对象含未知字段 | `400 invalid_input`，`details.field="events[0].<字段>"` |
| `input` 为空或多于一条 message | `400 invalid_input`，`details.field="events[0].input"` |
| message 非 `role:"user"` 或缺 role | `400 invalid_input`，`details.field="events[0].input[0].role"` |
| content 含 `input_image` 或未知类型 | `400 invalid_input`，`details.field="events[0].input[0].content[<下标>]"` |
| 拼接后文本为空白 | `400 empty_input`（复用现有检查） |
| 拼接后文本超过 1 MiB | `400 invalid_input`（复用现有 `check_len`） |

响应形状保持 `202` + 现有 outcome JSON；官方 SDK 忽略 body，未改状态码或响应结构。

## 3. 实际执行的命令与结果

所有 cargo 命令均带 rsproxy 镜像参数并使用 `PATH=/Users/mino/.cargo/bin:$PATH`：

`--config 'source.crates-io.replace-with="rsproxy"' --config 'source.rsproxy.registry="sparse+https://rsproxy.cn/index/"'`

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `./scripts/cargo-rmcp-patched.sh ... check --locked -p peri-swarm --all-targets` | 通过，exit 0，`Finished dev profile`（7.85s） |
| 2 | `./scripts/cargo-rmcp-patched.sh ... test --locked -p peri-swarm` | 通过，0 failed：lib 185、`actor_runtime_test` 7（1 ignored）、`http_contract_test` 23、`items_pagination_test` 6、`loom_store_test` 0（3 ignored）、`mcp_injection_test` 4、`openai_hosted_test` 7、`rivet_actor_test` 0（2 ignored）、`streaming_test` 11、doc-tests 0 |
| 3 | `python3 scripts/check-feature-catalog.py` | 通过：`Documentation: PASS (structure and destinations; not runtime acceptance)` |
| 4 | `git diff --check` | 通过，exit 0，无输出 |
| 5 | `cargo fmt -p peri-swarm -- --check`（仅 check，未运行 `cargo fmt`） | 通过，exit 0，无输出 |

## 4. 未验证项

1. **没有活体 Loom/Rivet engine。** 集成测试使用 `InMemorySwarmService`；本记录不证明
   真实 actor 路由、持久化 `swarm_inputs` 或重启恢复。
2. **没有真实官方 SDK 进程调用。** 请求体取自任务给出的 Node SDK 形状；测试用等价
   JSON 直接打网关，未启动 `openai@7.60/7.62` 客户端。因此不证明 SDK 版本级序列化
   差异（例如额外字段、`type: "message"` 的实际出现）。
3. **SSE `sessions.stream()` 的内部 submit 链路未端到端运行。** 只验证它调用的同一
   POST 契约。
4. **批量事件不提供原子性。** 官方信封本轮只接受单条事件；多事件明确 `400`，不存在
   “部分受理”。
5. **1 MiB 边界未单独构造接近上限的请求**；只验证了增量文本走同一 `check_len` 与
   空白检查。
6. **无 key 重试不保证幂等。** 每次无 key 官方信封都会生成新 UUIDv7，这是刻意的
   carve-out；未把它写成重试承诺。

## 5. 证据边界

- 单测证明解析、字段路径与错误码；契约测试证明 HTTP 状态、outcome 与 items 中的
  user message 文本。
- 均不证明真实 SDK 兼容性、真实存储受理或跨进程行为。
- 现有原生 events 测试的断言未修改，全部通过；这支持“原生路径行为不变”，但不覆盖
  所有未写测试的原生输入组合。

## 6. 第二批：官方事件覆盖补齐（同一文件的独立批次记录）

- 状态：代码与契约测试证据记录；**无真实 `openai@7.60.0` SDK 进程调用**，无活体
  Loom/Rivet engine
- 日期：2026-11-21
- 基线：`22e44e0b9`（工作树 `feat/swarm-official-events`）
- 说明：§1-§5 记录同日的 `POST /events` 官方信封 P0；本批是独立改动，覆盖
  `agent.session.*` 事件，未回退或修改 §1-§5 的语义。因任务指定的路径已被上一批
  占用，本记录追加在原文件而不是覆盖旧证据。
- 范围：`peri-swarm` 的 live（actor reader）与 replay（`OfficialReplay::render`）
  官方帧、流式 create 首帧、失败 `error` 帧，以及 cookbook 文本收集所需的
  `content_part.*`/`output_text.done`。
- 参考源码（只读）：`/Users/mino/code/peri/.claude/dogfood/node_modules/openai/resources/beta/agents/agents.d.ts`

### 6.1 本批新增覆盖

| 官方事件 | live 发出点 | replay 发出点 |
| --- | --- | --- |
| `agent.session.created` | 仅在 `POST /v1/agents/sessions?stream=true` 的初始帧，先于自定义 `session` | 不发 |
| `agent.session.in_progress` | `begin_turn`，先于 `turn.created` | 回放窗口开启新 turn 时 |
| `agent.session.turn.in_progress` | `begin_turn`，紧跟 `turn.created` | 同上 |
| `agent.session.turn.item.done` | assistant 完成行落库后 | 每个 `complete:true` assistant 完成行 |
| `agent.session.turn.content_part.added` | 同上 | 同上 |
| `agent.session.turn.content_part.done` | 同上 | 同上 |
| `agent.session.turn.output_text.done` | 同上 | 同上 |
| `error` | 失败 turn：`turn.failed` → `error` → `session.failed` | 最后失败 turn 同样三帧 |

`event_id` 全部由持久事实派生，不含时钟或随机源：

| 事件 | `event_id` |
| --- | --- |
| `agent.session.created` | `evt_<sessionId>_session_created` |
| `agent.session.in_progress` | `evt_<turnId>_session_in_progress` |
| `agent.session.turn.in_progress` | `evt_<turnId>_in_progress` |
| `turn.item.done` | `evt_item_<seq>_done` |
| `turn.content_part.added` | `evt_item_<seq>_part_added` |
| `turn.content_part.done` | `evt_item_<seq>_part_done` |
| `turn.output_text.done` | `evt_item_<seq>_text_done` |
| `error` | `evt_<turnId>_error` |

### 6.2 明确缺省

- 文本 delta（`output_text.delta`）、四种 `reasoning_summary.*`、八种
  `environment.*`、三种 `subagent.*`、`requires_action`、`turn.cancelled`、
  `function_call` item 映射均不新增。取消仍折叠为
  `turn.completed(status="cancelled")`。
- `AgentSession.environment` 仍不在 `OfficialSession` 投影中；这是既有缺口，本批
  没有新增该字段。
- 未引入“等待审批”状态，未打开 token/usage 记账；`usage` 仍为 `null`。

### 6.3 关键取舍

- assistant `phase`：`finish_turn` 按消息首块持久化 `seq` 排序，最后一条非空完成
  消息为 `final_answer`，其余为 `commentary`；`phase` 与 `outputIndex` 写入该完成
  item 的 payload。live 与 replay 都从 payload 读取，避免两套推导。
- 旧 payload 没有 `phase`/`outputIndex`：`phase` 回放为 `final_answer`，
  `outputIndex` 按该 turn 内完成行顺序推导。旧记录仍可回放。
- 回放失败 `error` 不读 `swarm_turns`：`code`/`type` 固定 `internal_error`（网关
  只写这一分类），`message` 在无持久文本时为空字符串，而不是编造失败原因。
- 回放不会因新事件读取 turn 表；新增的 `session.in_progress` 使用回放已有的会话行
  时间字段，`session.created` 完全不参与 replay。
- `error` 的 SSE 事件名没有 `agent.session.` 前缀；actor→gateway hop 已显式识别
  `event: error`，否则该帧会在 hop 上被丢弃。

### 6.4 验收项与测试

| 验收项 | 测试 | 关键断言 |
| --- | --- | --- |
| create 流首帧是 `agent.session.created`，且 session id 与后续 turn 一致 | `http::sse::tests::create_stream_starts_with_the_official_session_frame`；`streaming_test::stream_create_subscribes_before_the_first_turn` | 首个 SSE frame 为 `agent.session.created`，`session.id == "agent-01"`；后续 `turn.created.session_id` 相同；events 流不出现该帧 |
| turn 开始顺序 `session.in_progress` → `turn.created` → `turn.in_progress` | `actor::reader::tests::turn_start_announces_progress_before_the_user_item`；`streaming_test::official_frames_stream_in_sdk_order_and_replay_identically` | 官方 `(type, id)` 顺序为 `evt_<turn>_session_in_progress`、`evt_<turn>_created`、`evt_<turn>_in_progress`、`evt_item_1_added`；自定义 `item` 在前、`turn: started` 最后 |
| 单条 assistant 消息 | `actor::reader::tests::assistant_completion_reports_item_parts_and_final_phase` | `item.done` phase=`final_answer`、output_index=0、status=`completed`、content 为 `output_text`；同时有 `part_added`/`text_done`/`part_done` |
| 同一 turn 两条 assistant 消息 | `actor::reader::tests::interleaved_assistant_messages_share_the_turn_by_first_chunk_seq` | 按首块 seq 排序：`commentary`/0 与 `final_answer`/1；消息 id 逆序也不影响顺序 |
| 失败 turn 的顺序与 code | `actor::reader::tests::failed_turn_reports_error_between_turn_and_session_failure`；`service::in_memory::tests::replay_marks_a_failed_last_turn_failed_after_partial_output` | `turn.failed` → `error` → `session.failed`；`error.error.code == "internal_error"`，`type == "internal_error"`，`param == null` |
| live/replay `(type,event_id)` 一致 | `service::in_memory::tests::live_and_replay_agree_on_the_official_type_and_event_id_sequence`；`streaming_test::official_frames_stream_in_sdk_order_and_replay_identically` | live 与 `replay_after(START)` 的官方序列完全相等 |
| 旧 payload 兼容 | `service::in_memory::tests::replay_keeps_every_assistant_message_of_one_turn`；`replay_from_inside_a_turn_seeds_the_turn_identity` | 无 `phase`/`outputIndex` 时回放为 `final_answer`，index 依次为 0/1；旧 assistant 帧仍挂在同一 `turn_id` |
| 回放读取持久 `phase`/`outputIndex` | `service::in_memory::tests::replay_reads_the_persisted_phase_and_output_index` | `commentary`/0 与 `final_answer`/1 原样回放，不重新推导 |
| hop 不丢 `error` | `event_stream::tests::error_frames_cross_the_hop_under_their_official_name` | 编码为 `event: error`，解码回同一 `OfficialEvent` |
| `session.created` 与 idle 共用投影 | `domain::official::tests::completion_frames_match_the_local_sdk_shape`；`official_session_carries_persisted_metadata_and_prompt_cache_options` | `session.object == "agent.session"`、`event_id` 稳定；无 environment 字段 |

### 6.5 实际执行的命令与结果

所有 cargo 命令使用 `CARGO_HOME=/tmp/cargo-home-rsproxy`，并通过
`PATH=$HOME/.local/share/uv/python/cpython-3.12.14-macos-aarch64-none/bin:$HOME/.cargo/bin:$PATH`
调用。

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `cargo test -p peri-swarm` | **通过，exit 0**：lib 247、`actor_runtime_test` 16（1 ignored）、`http_contract_test` 25、`items_pagination_test` 13、`loom_store_test` 0（3 ignored）、`mcp_injection_test` 4、`openai_hosted_test` 7、`rivet_actor_test` 0（2 ignored）、`session_resources_test` 17、`streaming_test` 15、doc-tests 0；合计 344 passed / 0 failed / 6 ignored |
| 2 | `cargo clippy -p peri-swarm --all-targets -- -D warnings` | **通过，exit 0**，`Finished dev profile` |
| 3 | `cargo fmt -p peri-swarm -- --check` | **通过，exit 0**，无输出 |
| 4 | `MISE_DISABLE_TOOLS=rust npx -y lefthook@1.13.6 run pre-commit --all-files` | **手动运行通过，但不能作为提交本身全绿的证据**：`check`、`clippy`、`feature-catalog`、`fmt`、`layer-imports`、`tui-components` 全绿（summary 7.11s）。该通过依赖先对基线已存在的 `mcp-packages/peri-browser-gateway/{cdp_test,ownership,ownership_test}.rs` 临时执行 `cargo fmt`（**不 stage**），提交后恢复；干净 worktree 直接运行会在 `fmt` 失败。详见 §6.7 |
| 5 | `git diff --check` | 通过，exit 0，无空白错误 |

### 6.6 证据局限

1. **未跑真实 SDK 进程。** 没有启动 `openai@7.60.0` 客户端消费这些帧；证据是本地
   类型对照（`agents.d.ts`）与 JSON/SSE 契约测试，不证明 SDK 运行时行为。
2. **未跑活体 Loom 或 Rivet。** `cargo test -p peri-swarm` 使用内存 store / fake
   service；`loom_store_test` 与 `rivet_actor_test` 的 live 用例默认 ignored。本记录
   不证明真实 actor 路由、Loom 持久化或跨进程 hop。
3. **回放失败 `error` 的 `message` 是空字符串。** 因为 `swarm_turns.error` 在回放中
   不可读，且持久 item 行没有失败文本；`code`/`type` 只证明网关唯一的分类，不证明
   真实失败原因。live 帧的 `message` 是真实文本。
4. **旧数据的 `outputIndex` 只按窗口内行序推导。** 若重连游标落在 turn 中间且旧
   payload 没有 `outputIndex`，序号从窗口内第一条 assistant 完成行重新从 0 开始；
   新 payload 不受影响。这是旧数据回放的已知边界。
5. **`turn.item.added.output_index` 仍为 `null`。** 本批只让 `item.done` 携带
   `output_index`；`item.added` 的既有载荷未改。
6. **`docs/reference/feature-catalog.md` 未改。** 该表有既有行宽约束；本批事件
   覆盖属于 `F-SWARM-004` 的同一契约族，若要登记需协调者统一处理，避免破坏表格
   检查。
7. **全量 `fmt` 的基线格式债未进入提交。** `mcp-packages/peri-browser-gateway/`
   的 3 个文件在基线 `22e44e0b9` 就未通过 `cargo fmt --check`，且不是本任务范围。
   为让未跳过的 pre-commit 通过，本次对其临时执行 `cargo fmt`（不 stage），并在
   提交后恢复；`git diff --quiet 22e44e0b9 -- mcp-packages/peri-browser-gateway`
   在最终工作树上通过。因此本记录不声称“该目录在基线工作树上原样通过全量 fmt”。

### 6.7 复验修复：混合流、root 过滤与 session payload

- 复验结论：上一提交 `4a3d0e5b` 判 FAIL。原因是真实 Peri 路径把 reasoning 与
  answer 写入同一 `messageId`，而流键只有 `(agentId, messageId)`；`Thought` 先到会
  固化流 kind，answer 被并入 Thought，最终没有 root assistant `item.done`。
- 日期：2026-11-21
- 基线：`4a3d0e5b`（本记录 §6 的上一提交）+ 本节的修复提交

#### 6.7.1 修复内容

| # | 问题 | 修复 |
| --- | --- | --- |
| B1 | 同一 `messageId` 的 Thought/Message 混合流 | `StreamKey` 改为 `(agentId, messageId, kind.as_str())`；`Message` 与 `Thought` 独立累积、独立 `itemId`、各自持久化 delta 与完成行 |
| B1 | Thought 抢 `final_answer` | `finish_turn` 只为 root `Message` 计算 `phase`/`outputIndex`；Thought/child 完成行不带这两个键，也不参与“最后一条”判定 |
| H1 | child 消息进入 root 官方 item | `official_frames` 先要求 `source.is_root()`；`finish_turn` 只统计 root `Message`；回放 `classify_row` 只处理 root `Message`；内存 fake 的 live 投影同样只处理 root `Message` |
| H2 | Thought 首块更晚抢 final | 与 B1 同源：输出序号只由 root `Message` 序列产生 |
| M1 | live/replay `session.in_progress` payload 不一致 | `OfficialReplay` 从会话行补齐 `metadata`/`prompt_cache_options`，`in_progress`/`idle`/`failed` 共用一个 `official_session` 构造 |
| M2 | `created=false` 也发 `agent.session.created` | `create_response` 仅在 `result.created` 时加入该官方帧；attach 只发自定义 `session` 帧 |

实现位置：`peri-swarm/src/actor/reader.rs`、`peri-swarm/src/service/mod.rs`、
`peri-swarm/src/service/in_memory.rs`、`peri-swarm/src/domain/item.rs`、
`peri-swarm/src/http/sse.rs`。未改 store schema、迁移或 README，未新增 reasoning
官方事件、delta、environment、subagent、`requires_action` 或 `turn.cancelled`。

#### 6.7.2 新增测试

| 验收点 | 测试 | 关键断言 |
| --- | --- | --- |
| 同一 messageId 先 thought 后 text | `actor::reader::tests::message_and_thought_sharing_a_message_id_stay_separate` | Message 完成行 text 恰为 `answer`，`phase=final_answer`、`outputIndex=0`；Thought 行 text 为 `reasoning`，无 `phase`/`outputIndex`；两者 `item_id` 不同 |
| 同一 messageId 先 text 后 thought | 同上（case `text then thought`） | answer 不被 reasoning 污染；官方只有一个 `item.done` |
| 只有 thought | 同上（case `only thought`） | 没有官方 `item.added`/`item.done`；Thought 完成行仍持久化 |
| 只有 text | 同上（case `only text`） | 只有 Message 完成行与 `final_answer`/0 |
| Thought 首块更晚 | `actor::reader::tests::thought_first_chunk_later_does_not_steal_final_answer` | 即使 Thought 有独立 messageId 且 `seq` 更晚，Message 仍是 `final_answer`/0 |
| child 不影响官方 item 与序号 | `actor::reader::tests::child_messages_never_become_official_items_or_output_indices` | child 先到也不占用 output 0；两条 root Message 得到 `commentary/0` 与 `final_answer/1`；child 完成行无 `phase`/`outputIndex`，不出现在官方 item.added/done |
| 回放 child 过滤 | `service::in_memory::tests::replay_ignores_child_messages_for_official_items_and_output_index` | 回放只有一个 root `item.done`（`final_answer`/0）；child 行只保留自定义 `item` 帧 |
| 回放 lookback 的 child 过滤 | `service::in_memory::tests::replay_lookback_ignores_child_output_when_deciding_steer` | 只有 child 输出时，后续 root user item 仍是 `turn_input_x` 的 steer，不会被 child 输出误判为已闭合的新 turn |
| 回放 session payload | `service::in_memory::tests::replay_session_frames_carry_persisted_metadata_and_prompt_cache` | `session.in_progress` 与 `session.idle` 都带持久 `metadata` 与 `prompt_cache_options` |
| attach 不发 created | `http::sse::tests::attach_stream_does_not_emit_session_created` | `created=false` 时首帧是自定义 `session`，无 `agent.session.created`，后续 turn 帧正常 |

#### 6.7.3 门禁结果

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `cargo test -p peri-swarm` | **通过，exit 0**：lib 254、`actor_runtime_test` 16（1 ignored）、`http_contract_test` 25、`items_pagination_test` 13、`loom_store_test` 0（3 ignored）、`mcp_injection_test` 4、`openai_hosted_test` 7、`rivet_actor_test` 0（2 ignored）、`session_resources_test` 17、`streaming_test` 15、doc-tests 0；合计 351 passed / 0 failed / 6 ignored |
| 2 | `cargo clippy -p peri-swarm --all-targets -- -D warnings` | **通过，exit 0**，`Finished dev profile` |
| 3 | `cargo fmt -p peri-swarm -- --check` | **通过，exit 0**，无输出 |
| 4 | `MISE_DISABLE_TOOLS=rust npx -y lefthook@1.13.6 run pre-commit --all-files` | **手动运行 exit 0**：`check`（2.51s）、`clippy`、`feature-catalog`、`fmt`、`layer-imports`、`tui-components` 全绿（summary 7.82s）。该结果依赖对基线 `peri-browser-gateway` 3 个文件临时 `cargo fmt`（不 stage，运行后恢复）；**干净 worktree 直接运行会在 `fmt` 失败，所以它不是提交本身全绿的证据** |

#### 6.7.4 仍未覆盖的证据局限

1. **未跑真实 `openai@7.60.0` SDK 进程。** 修复以本地 `agents.d.ts` 形状和 SSE/JSON
   契约测试为证据，不证明 SDK 运行时对混合流或 attach 流的实际消费。
2. **未跑活体 Loom/Rivet。** 测试使用内存 store 与 in-process fake/actor 测试；
   `loom_store_test`、`rivet_actor_test` 的 live 用例仍 ignored。未证明真实
   `_meta.peri.sourceAgentId` 在跨进程 hop 上的 child 过滤，只证明投影与持久行路径。
3. **全量 lefthook 的 fmt 通过依赖临时处理。** 基线 `mcp-packages/peri-browser-gateway`
   的 3 个文件本身未格式化；本次未把它们纳入提交，也未修改它们的内容。干净
   worktree 的 `fmt` 仍会失败；`cargo fmt -p peri-swarm -- --check` 才是本提交
   范围内的格式证据。
4. **回放失败 `error` 仍无持久失败文本。** `message` 为空字符串，`code`/`type` 固定
   `internal_error`；这与 §6.3 的取舍一致，未因本轮修复改变。
5. **child/Thought 完成行在 root turn 结束时 flush。** 它们会持久化并在自定义 item
   面可见，但不参与官方输出；真实子代理长生命周期是否跨越多个 root turn 未单独
   验证。
6. **修复前的混合行不会自动还原。** 若旧数据已经把 answer 文本写进 `Thought`
   kind 的完成行，回放仍按 Thought 过滤，不会凭空生成 assistant `item.done`。本修复
   只保证新流不再把两种 kind 合并；没有做数据迁移（也不在本任务范围）。
