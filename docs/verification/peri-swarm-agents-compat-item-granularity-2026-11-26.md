# Peri Swarm Agents API 兼容与 item 粒度验收（2026-11-26）

> 状态：**部分验收**。代码基线 `6f935edf6`（`feat/deploy-compose`）。本文记录三批
> 改动在本地 Compose 部署与 crate 测试中的证据，并逐项标出**未验证**范围；未验证项
> 不构成验收。证据来自实施/部署报告与其中引用的 `/tmp` 回执，本轮未重跑部署。
>
> 相关契约：[peri-swarm.md §4.2 / §4.4 / §5.1 / §9](../design/peri-swarm.md)；
> [active spec](../../spec/issues/2026-11-26-swarm-event-log-granularity.md)。

## 1. 基线与范围

合并链（均在 `feat/deploy-compose` 上）：

| 批次 | 合入提交 | 范围内行为 |
| --- | --- | --- |
| transport authorization | `3a3d5f4b8`（`ab9036d64`、`c7947adb9`） | 接受官方 Agents API 的 MCP `transport` 形状；`transport.authorization` 的 Bearer 前缀幂等 |
| item 粒度与取消语义 | `72e832c58`（`877a638e2`、`530c137ea`、`56897e6ff`） | turn 终止按 item 合并落库；恢复行不归属旧 turn；取消 turn 落库失败保持 `cancelled` |
| Agents API 兼容 D/B/E 与修复 | `a5b2a44a5`（`0de1aff23`、`eb25f446c`、`83f2171bc`、`50f088a0a`、`dd74864c5`）、`ed70b1cd0`（`937e0b629`）、`6f935edf6` | 官方 items 投影（D）、官方 cancel（B）、非 UUID 幂等键（E）、M-2/M-4 修复 |

- 验收对象只有 `peri-swarm` 与两份契约文档（`docs/design/peri-swarm.md`、
  `peri-swarm/README.md`）；未改 `deploy/**`、状态库 schema、`peri-swarm` 之外的运行时。
- `6f935edf6` 相对 `ed70b1cd0` 只改 `peri-swarm/src/service/official_items_test.rs`
  （+49/−65），不改变运行时字节。
- 部署沿用既有 Compose 配置与既有 provider，没有为新验收修改配置或覆写模型。

## 2. 证据

> 来源均为本地实施/部署报告：`.peri/plans/swarm-compat-merge-deploy.md`、
> `swarm-fix2-merge-deploy.md`、`swarm-agents-compat-implement.md`、
> `swarm-agents-compat-fix.md`、`swarm-agents-compat-fix2.md`、
> `swarm-item-granularity-cancelled-fix.md`。这些路径在 Git 忽略目录内，不随仓库发布；
> 原始日志落在 `/tmp`，可能已清理。

### 2.1 测试与 PostgreSQL 契约

| 项 | 观测值 | 来源 |
| --- | --- | --- |
| `cargo fmt -p peri-swarm -- --check` | 退出 0 | fix2 合并部署报告 |
| clippy（`-p peri-swarm --all-targets -- -D warnings`） | 退出 0，0 warning | fix2 合并部署报告 |
| crate 测试（合并基线） | lib **417 passed / 0 failed / 20 ignored**；集成 **115 passed / 0 failed / 7 ignored**；doctest 0；合计 **532 passed / 0 failed / 27 ignored** | fix2 合并部署报告，`/tmp/peri-swarm-fix2-gates2/test.log` |
| PostgreSQL 契约（显式 ignored） | **20 passed / 0 failed**，PostgreSQL **16.16**；使用 `postgres` 维护库的私有 schema，运行后残留 schema 为 0 | 同上，`/tmp/peri-swarm-fix2-gates2/pg.log` |

27 个 ignored 中已显式运行 20 个 PG 契约；其余 7 个未运行。

### 2.2 镜像与容器

| 项 | 观测值 | 来源 |
| --- | --- | --- |
| 镜像 | `peri-swarm:local` = `sha256:c6e6ea7ce229501f827d89e4c690b2498a2ae7f7681982f51a211c69fe3f1b96`（210 MB） | fix2 合并部署报告，`/tmp/peri-swarm-fix2-rebuild/image.id` |
| 上一版镜像 | `sha256:3976e8f833cd95891abbbca29c2b70b4a5b178082a1f85a1c35d48f8b363993f` | 同上 |
| 副本 1 | 容器 `72bf397ccb0e`，running / healthy | 同上，`docker compose ps` |
| 副本 2 | 容器 `28743ff6ad5c`，running / healthy | 同上 |
| PostgreSQL / nginx / Rivet engine | `2d5f30f24660` healthy、`9cfd4bafda39` running、`b809ed4dc415` running | 同上 |

`docker compose up -d --no-build` 退出 0，只重建两个 `peri-swarm` 副本；轮询 6 秒后
所有带 healthcheck 的容器 healthy。镜像构建在 `6f935edf6` 之后完成，构建上下文由
`.dockerignore` 排除本地 `.env` 与 provider 配置。

### 2.3 探针与真实 turn

| 项 | 观测值 | 来源 |
| --- | --- | --- |
| `/healthz` | HTTP 200，`{"status":"ok"}` | fix2 合并部署报告 |
| `/health` | HTTP **401**，`error.code=unauthorized`（与部署前一致，未改路由） | 同上 |
| 真实 turn | 经 nginx、既有 provider 发起；收到 `turn.completed`（`turn_01a21abd-043c-78c3-9514-98b58446c0d3`），耗时 1.75s | 同上，`/tmp/peri-swarm-fix2-rebuild/smoke.log` |
| SSE 事件序列 | `agent.session.created` → `agent.session.turn.created` → `agent.session.turn.in_progress` → `agent.session.turn.item.done` → `agent.session.turn.completed` → `agent.session.idle`；item 帧 121 | 同上 |
| `/items` | HTTP 200，`raw=3 official=3`；`items` 含 `seq`，`data` 含官方条目 | 同上 |
| 落库 | 该 turn `swarm_items` **3 行 = 2 `message` + 1 `thought`**，3 个唯一 `item_id`，**0 行持久 delta** | 同上，删除前只读事务按 `logical_id` 统计 |
| 删除 | `DELETE` 200 → `GET` 404 → 会话/turn/item 行均为 0 | 同上 |

### 2.4 粒度对比

同一部署在改动前测得约 **930 行/turn**（2 会话、4 turn，逐 chunk 落库；见
[active spec](../../spec/issues/2026-11-26-swarm-event-log-granularity.md) §2）。该数字是
**先前测量**，prompt、模型与会话数与本次不同，不是受控对照；本次单 turn 的 3 行也不能
外推到其他 prompt 或模型。可比较的只有同一事实：新写入不再为每个 chunk 分配 `seq`。

## 3. 逐项验收映射

状态用语：**用例覆盖**指仓内测试在本次基线上通过；**部署观察**指 2.3 的单 turn
冒烟；两者都不覆盖未列出的路径。

### 3.1 ILOG-1..7

| 项 | 目标行为 | 证据 | 状态 |
| --- | --- | --- | --- |
| ILOG-1 | 单 turn item 行数 ≤ item 数 + 常量 | `reader_test::chunks_broadcast_live_and_persist_once`（实时逐 chunk、落库 0 行）、`reader_test::one_row_per_stream_per_turn`（文本/推理/工具/审批交错，每 item 一行）；部署观察 3 行 | 用例覆盖 + 纯文本单 turn 实测；含工具/审批 turn 未在部署中观察 |
| ILOG-2 | `afterSeq=0` 回放顺序与实时一致 | `actor_runtime_test::replay_matches_live_frames`（逐条比对 `item` 帧与官方 `(type,event_id)`） | 进程内用例覆盖；线上未实测 |
| ILOG-3 | 断线重连按游标补齐，不重复、不丢终态 | `actor_runtime_test::reconnect_mid_turn_gets_final_row_once`（turn 中重放无半写行，终态后同一游标只返回合并行与终态一次） | 进程内用例覆盖；真实 `Last-Event-ID`/`afterSeq` 断线重连未实测 |
| ILOG-4 | 审批折叠：已结算不复活，过期先结算 | `service_test::an_expired_pending_approval_is_settled_before_the_replay_window`、`service_test::interleaved_approval_and_merged_rows_replay_without_a_pending_action`（含二次答复被拒、无新行） | 进程内用例覆盖；崩溃注入/真实宿主未跑 |
| ILOG-5 | 崩溃恢复不重复执行 initial input | `actor_runtime/interrupted::crash_after_user_item_keeps_initial_input_once`（从持久半态复现 create 只写 user item） | 进程内持久半态覆盖；真实进程 kill 未在本轮运行 |
| ILOG-6 | 官方事件序列不变 | `reader_test::a_single_turn_keeps_the_official_sequence`；`replay_matches_live_frames` 的官方 `event_id` 一致性；部署观察 6 事件序列 | 纯文本 turn 覆盖；工具/审批 turn 的真实序列未观察，未与录制基线逐帧比对 |
| ILOG-7 | 溢出 ring 后仍发 `gap` 并可续读 | `reader_test::a_lagged_subscriber_can_resume_from_the_last_item`（容量 2、慢消费者、按最后持久 `seq` 续读） | 进程内用例覆盖；线上慢消费者未实测 |

### 3.2 D：官方 items 投影

| 项 | 目标行为 | 证据 | 状态 |
| --- | --- | --- | --- |
| 类型与字段 | `data` 发 `message` / `reasoning` / `function_call` / `function_call_output`，`items` 保留原始行 | `domain::official_item_test`（四类序列化、工具字段、排除项）、`service::official_items_test`（页内缺口、steer、恢复行、跨页）、`items_pagination_test`；部署观察 `raw=3 official=3` | 用例覆盖 + 纯文本单 turn 实测；真实 SDK 只消费过消息项 |
| M-1 悬挂调用 | 调用行无输出时按 turn 终态给 `incomplete` / `in_progress` | `a_dangling_call_in_a_terminal_turn_is_incomplete`、`dangling_call_status_follows_the_turn_terminal` | 用例覆盖 |
| M-2 历史多代折叠 | 首条 complete 的 `text` + 其后 **delta** 帧；后续 complete 不追加 | `multi_generation_streams_project_the_first_completion_plus_late_deltas`、`assistant_appends_rows_after_the_first_complete_frame` | **只在 store 层构造历史行验证**（见 §4.2） |
| M-3 SDK 游标 | `data[last].id` 为 `MAX(seq)`；未映射条目的跳过/重读按契约 | `items_pagination_test::sdk_data_cursor_skips_*` | 用例覆盖 |
| M-5 回扫上限 | 每请求最多 8 次回扫；超限组只留 `items` 并 warn | `many_gaps_stop_at_the_lookback_cap_and_degrade_to_items_only` | 用例覆盖；常量不可配置 |
| M-6 升级边界 | `ItemPage` 是 actor/gateway 共享 wire 类型；混版 `/items` → `502 upstream_unavailable`（`details.operation=swarm_actor`） | `docs/design/peri-swarm.md` §4.2；部署要求整体重启 | 文档契约；未做滚动混版实测 |

### 3.3 B：官方 cancel 与管理冲突

| 项 | 目标行为 | 证据 | 状态 |
| --- | --- | --- | --- |
| 事件解析 | 接受单条 `agent.session.input.cancel`；额外字段、批量、混用按字段拒绝 | `http::request` 单测、`http_contract/official_cancel` | 用例覆盖 |
| 受理语义 | 202 收据，不建 turn、不写 item；同键重放 `existing`，不影响后续 turn | `cancel_closes_the_active_turn_without_creating_an_item_and_retry_spares_the_next_turn`、`idle_cancel_is_a_receipted_no_op` | 用例覆盖 |
| 无宿主 | 持久 turn 未完成且无 live runtime → `502 session_resume_required`，写收据前返回 | `actor_cancel_without_a_host_refuses_a_persisted_open_turn_before_receipting`、`in_memory_cancel_without_a_live_turn_refuses_a_persisted_open_turn` | 用例覆盖 |
| 管理冲突 | update/delete 在 turn 运行时 `409` 且给 cancel 入口；无 slot 时 `retry_later` | `service_test::an_open_turn_without_a_slot_asks_the_caller_to_retry_instead_of_cancel` | 用例覆盖 |

### 3.4 E：非 UUID `Idempotency-Key`

| 项 | 目标行为 | 证据 | 状态 |
| --- | --- | --- | --- |
| 派生 | 任意非空 ASCII 键 → `UUIDv5`（namespace `76663edb-72b5-58df-9b4c-b1e3c8f8d418`，名称 `logicalId + NUL + key`） | `domain::input_id_test::namespace_and_name_encoding_are_stable`、`derived_ids_are_scoped_to_the_session_and_preserve_key_bytes` | 用例覆盖 |
| 请求路径 | header/body 一致校验；原键不落库、不回显；同键同文 `existing`、异文 `409` | `http::request` 单测、`http_contract/idempotency_keys`（官方 message 与原生 header-only 两条路径） | 用例覆盖 |
| 实机探针 | 官方 SDK 同键重试非 UUID 消息：派生 UUIDv5、`202 existing`、原始行不变 | compat 分支 `83f2171bc` 的内联探针（`11/11 assertions, raw rows 6 -> 6`） | compat 分支实测；合并基线未重跑 |

### 3.5 transport 形状

| 项 | 目标行为 | 证据 | 状态 |
| --- | --- | --- | --- |
| 官方形状 | `transport.type="http"` 的 `server_url`/`headers` 映射为内部声明；平铺 `server_url` 保留为 alias | `http_contract_test::create_accepts_the_official_sdk_transport_shape`、`official_transport_and_flat_alias_normalize_to_one_declaration`；`request_test::official_transport_shape_maps_onto_the_flat_spec` | 用例覆盖 |
| authorization | 无 auth scheme 时补 `Bearer`；已带 `Bearer`/`Basic` 原样；空值 `400 invalid_input` | `transport_authorization_is_idempotent_and_empty_is_rejected`、`request_test::transport_authorization_only_gains_a_missing_bearer_scheme` | 用例覆盖 |
| 冲突与不支持 | 平铺与 `transport` URL 不一致 → `400`；`stdio`/未知类型 → `422 unsupported_tool` | `disagreeing_flat_and_transport_urls_are_rejected_at_the_transport_path`、`stdio_and_unknown_transports_are_rejected_at_the_transport_type` | 用例覆盖；本部署没有 stdio 运行时 |
| update 路径 | `sessions.update` 的 `tools` 同样接受官方形状 | `request_test::sessions_update_tools_accept_the_official_transport_shape` | 用例覆盖 |

`allowed_tools` 只记录在 ACP `_meta`，未在调用侧强制过滤；`require_approval` 经宿主审批
通道产生 `requires_action`。这不是本轮的实现缺口，而是已文档化的边界。

### 3.6 取消语义与落库失败

| 项 | 目标行为 | 证据 | 状态 |
| --- | --- | --- | --- |
| 首次下发失败 | `503 queue_unavailable`，无收据，文案“未写收据”，同键可重试 | `a_cancel_that_never_reaches_the_host_writes_no_receipt` | 用例覆盖 |
| 重发失败（M-4） | 收据已存在但重发未送达 → `503 cancel_redelivery_failed`，`details.receipt=accepted` | `a_failed_cancel_redelivery_reports_the_existing_receipt` | 用例覆盖 |
| 在途取消 | 已送达但宿主未退出 slot → 202；同键重放再下发 `session/cancel`，`lastSeq` 不变 | `a_timed_out_cancel_is_delivered_again_on_the_same_key_without_new_items` | 用例覆盖，等待窗口缩短为 50ms |
| 取消 turn 落库失败 | 完成行 flush 失败只记 warn；终态仍 `cancelled`，持久行 `Cancelled`，会话 `idle` | `a_cancelled_turn_stays_cancelled_when_its_completion_flush_fails`；反向验证见实施报告 | 用例覆盖 |
| 部分失败顺序 | 已提交批次先广播，再发失败终态 | `a_partially_failed_completion_flush_broadcasts_the_committed_row` | 用例覆盖 |
| 非取消 turn 落库失败 | 未持久化完成组 → `turn.failed` | `a_completion_flush_that_persists_nothing_fails_the_turn` | 用例覆盖 |

已知代价：取消 turn 的完成文本若落库失败，`items.list`/`turns.retrieve` 会显示
“cancelled 但无对应 assistant 文本”。这是“不静默改成 failed”的代价，契约已按此写明。

## 4. 未验证与降级

### 4.1 真实部署未实测

- **历史多行回放**：只在既有数据上核对过一条 10 行 thought（9 delta + 1 complete）的
  `/items` 折叠结果（37 字符，与规则重算逐字节一致）；未走真实 SSE 历史回放。
- **`afterSeq` 断线重连**：没有在真实部署中断开订阅再带游标重连；只有进程内中段重放用例。
- **审批折叠**：`pending`/`settled`、过期先结算、已结算不复活只有进程内用例；没有真实
  崩溃注入或真实宿主等待审批后重连。
- **真实宿主 30s 取消超时**：单测把等待窗口缩短为 50ms；真实宿主下“已送达但未退出 slot”
  的 30s 路径未跑，真实 provider 的停止时效未验证。

### 4.2 历史形状只在 store 层构造

- 库内现有数据中**没有任何条目含 ≥2 个 complete 帧**；M-2 的“两代 complete 拼成
  `hellolo`”无法用既有历史复现。
- 合并后的写入器在 turn 终态 flush 后**有意丢弃**迟到 chunk（`actor/reader.rs` 的
  `closed` 集合），不再产生该形状。因此 `6f935edf6` 把用例改为在 store 层直接写入
  两代历史行；断言未变。这是该场景的**唯一证据**，不是线上复现。

### 4.3 未运行的 e2e 与混版

- **Node e2e 未在合并基线运行**：`sdk_sessions_e2e.mjs` 的 322/322 来自 compat 分支
  `83f2171bc`（真实官方 SDK + 本地确定性 stub，未声明 tools）；`sdk_reconnect_idempotency_e2e.mjs`
  依赖 Loom/Hrana、Bun 与 SRT，本轮验证栈从未运行。
- **滚动混版未验证**：`ItemPage` 同时是 HTTP body 与 actor `ListItems` 的 Rivet wire 类型，
  新旧 `data` 形状不能互解。升级要求 actor + gateway **整体重启**；混版 `/items` 预期
  `502 upstream_unavailable`，未实测滚动过程。
- 真实官方 SDK 的工具声明（`transport` + `tools`）未跑；transport 只有单测/HTTP 契约证据。

### 4.4 范围外与不可外推

- `swarm_items` 3 行只是本次单 turn、单一 prompt 与模型的值；约 930 行/turn 是改动前的
  另一组测量，两者不是受控对照，不能据此给出普遍降幅。
- 未做两副本负载、故障切换、真实 CI、发版矩阵；1000 Agent 规模无吞吐证据。
- 未用真实 provider 验证多代重复、cancel 重发失败与工具调用时序。
- 上述实施/部署轮次未改 `deploy/**` 配置语义或状态库 schema；本次记录提交只改文档与
  注释，不改运行时语义。

## 5. 证据局限

- 测试计数、镜像 ID、容器状态与单 turn 数字均引自本地实施/部署报告；**本轮没有重跑**。
  `/tmp` 日志可能已清理，`.peri/plans/` 不随仓库发布。
- 「已部署」只覆盖单机 Compose、两个副本、一个真实 provider 的一个纯文本 turn；不是
  发版验收，也不代表长会话或高并发。
- PostgreSQL 契约使用 `postgres` 维护库的私有 schema，不接触部署库
  `peri_swarm_state`/`peri_session`；运行后残留 schema 为 0。
- 27 个 ignored 中只显式跑了 20 个 PG 契约；其余 7 个默认 ignored 用例未运行，未据此
  声称 Rivet、两库部署或 ACP 测量覆盖。
- 容器/镜像 ID 是 2026-11-26 的时点值；重新构建会得到不同 ID。

## 6. 结论

三批改动已合入 `6f935edf6`，在所列测试与单 turn 冒烟范围内行为符合契约：turn 终止按
item 合并、`swarm_items` 从逐 chunk 行降为 3 行/turn、官方 items 投影可读、官方 cancel
与两类下发失败可区分、非 UUID 幂等键可派生、取消 turn 落库失败保持 `cancelled`。
本文不声称完整验收；§4 列出的未验证项仍需真实断线重连、崩溃注入、真实宿主取消超时、
Node e2e 与滚动混版证据。
