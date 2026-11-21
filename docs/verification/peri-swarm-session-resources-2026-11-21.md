# Peri Swarm 官方会话资源面验收（list / update / delete / turns）

- 状态：代码与契约测试证据记录；**真实 `openai@7.60.0` SDK 客户端已对本进程内网关执行
  create/retrieve/update/turns.list 探针**；无活体 Loom/Rivet/Bun engine
- 日期：2026-11-21
- 基线：`35819dfdd`（P1-6 初始实现）+ `0383c0dd9`（缺陷修复）+ 本记录同一提交中的
  update 空 body 兼容与准入措辞修正
- 范围：`openai@7.60.0` `client.beta.agents.sessions` 资源面。已能映射的
  `sessions.list/update/delete` 与 `sessions.turns.list/retrieve` 实现在网关；
  `turns.items`、`traces`、`artifacts`、`subagents` 及其子资源注册路由并返回网关
  JSON `404 unsupported_contract`。create/get/events/items 的既有语义不变。
- 参考源码（只读，未修改）：
  `/Users/mino/code/peri/.claude/dogfood/node_modules/openai/resources/beta/agents/sessions/`

## 1. 官方 SDK 事实依据

| 事实 | 源码位置 |
| --- | --- |
| `sessions.update` 使用 **`POST /agents/sessions/{sessionID}`** | `sessions/sessions.js` `update()` |
| `AgentSession.prompt_cache_options` 是 snake_case 可选字段 | `agents.d.ts` `AgentSession` |
| `Turn.status` 取值含 `cancelled` | `sessions/turns/turns.d.ts` `Turn` |
| `agent.session.turn.completed` 帧携带完整 `Turn` | `agents.d.ts` `AgentSessionTurnCompletedEvent` |
| `sessions.list` 使用 `CursorPage`，下一页 `after = data[last].id` | `core/pagination.js` `CursorPage.nextPageRequestOptions` |
| `SessionListParams extends CursorPageParams`（`after`、`limit`），另有 `order`（默认 `desc`）、`agent_id`、`metadata` | `sessions/sessions.d.ts` `SessionListParams` |
| `SessionUpdateParams`：`agent.model`、`agent.reasoning`、`agent.service_tier`、`agent.tools`、`metadata`、`prompt_cache_options`、`review` | `sessions/sessions.d.ts` `SessionUpdateParams` |
| `sessions.delete` 返回 `{id, object:"agent.session.deleted", deleted:true}` | `agents.d.ts` `AgentSessionDeleted` |
| 子资源路径：`/subagents/{id}/items`、`/subagents/{id}/turns`、`/subagents/{id}/turns/{turnID}`、`/subagents/{id}/turns/{turnID}/items` | `sessions/subagents/{items,turns/*}.js` |
| `metadata` 限制：最多 16 对、key ≤64、value ≤512 字符；`prompt_cache_options.ttl ∈ {"30m","12h"}` | `sessions/sessions.d.ts` |
| 无法映射的官方资源：`turns.items`、`traces`、`artifacts`、`subagents` 及其子资源 | `sessions/{turns/items,traces,artifacts,subagents}.d.ts` |

## 2. 本轮修复

| 缺陷 | 修复 |
| --- | --- |
| D1 update 动词错误（PATCH → 真实 SDK 405） | 路由改为官方 `POST /v1/agents/sessions/{logicalId}`；不保留 PATCH，测试断言 PATCH 为 `405`；真实 SDK 探针记录 update 调用是 POST |
| D2 `prompt_cache_options` 被序列化为 `promptCacheOptions` | `SessionView` 对该字段显式 `#[serde(rename = "prompt_cache_options")]`；create/get/list 共用同一投影，真实 SDK 能读到；文档写明其余 Swarm 扩展字段仍 camelCase |
| D3 遗漏官方子资源 404 | 注册 `subagents/{id}/items`、`subagents/{id}/turns`、`subagents/{id}/turns/{turnId}`、`subagents/{id}/turns/{turnId}/items`；Bearer 门禁后返回 `404 unsupported_contract`，`details.resource` 分别为 `sessions.subagents.items`、`sessions.subagents.turns`、`sessions.subagents.turns`、`sessions.subagents.turns.items` |
| D4 持久 `running`/开放 turn 行可被 PATCH/DELETE | 抽出单一判定 `service::persisted_turn_resume_required`（`state=running` 或 `swarm_has_open_turn`），`ensure_live` 与 update/delete 共用；无 live runtime 时返回 `502`、`details.operation="session_resume_required"`，且先判定后写库；live 在途仍 `409 turn_in_progress`。新增 store 端口 `swarm_has_open_turn`（内存 + Loom SQL） |
| D5 并发启动重复 `ALTER TABLE` | `apply_ddl` 首次失败后重读 `PRAGMA table_info`；仅当错误含 `duplicate column` 且迁移列已齐全时重试一次（幂等，并处理 version pinning）；其他错误或列不齐传播原错误。判定纯函数 `migration_retry_decision` 有单测 |
| D6 `agent.model: null` 被当作省略 | create 与 update 的 `agent.model` 改为三态读取；显式 `null` 返回 `400 invalid_request`、`details.field="agent.model"`；`agent.tools:null` 仍表示清空 |
| D7 create attach 的 metadata 语义与文档不一致 | `CreateSessionCommand` 的 `metadata`/`prompt_cache_options` 改为 `UpdateField` 三态；省略保持行内值且不冲突，显式值（含 `{}`/`null`）必须与行内一致，否则 `409 identity_conflict`；actor action 对 `Keep` 省略序列化，跨进程不把“省略”误读为 `null` |
| D8 取消 turn 持久化为 `completed` | `finish_turn` 在 `stop_reason == "cancelled"` 时持久 `TurnStatus::Cancelled`，同一帧的官方 `turn.completed.turn.status` 也是 `cancelled`；`turns.list`/`turns.retrieve` 断言同一状态 |
| D9 文档与证据 | 设计文档的动词、准入、404 清单、attach 语义、取消状态与字段拼写同步；本记录执行结果只写实际执行项 |
| D10 官方 `update(id, null)` 被判 `415` | `update(id)` 原本已发送 `{}` 并返回 `200`；只有显式 `update(id, null)`（无 body、无 content-type）被判 `415`。update handler 改读原始 body 并区分三态：空 body（含无 content-type、`Content-Length: 0`）与 JSON `null` 是空更新（`200` 当前投影，不写行）；`{}` 同样不写行；非空 body 才校验 JSON content-type 与语法，非法 JSON 仍 `400 invalid_request`，不降级为 no-op |

## 3. 行为矩阵

### 3.1 `sessions.list`

| 请求 | 结果 |
| --- | --- |
| 无参数 | `order=desc`，按 `logicalId` 倒序；`object:"list"`、`data`、`has_more`、`hasMore`、`nextCursor` |
| `limit=1` | 只返回 1 个会话；还有剩余时 `has_more=true`、`nextCursor=<页尾 id>` |
| `after=<上一页页尾 id>` | 在该顺序上排他，不重复 |
| `order=asc` | `logicalId` 升序 |
| `metadata[k]=v` | 所有给定 pair 精确匹配；`has_more` 基于过滤后结果 |
| `agent_id=root` / 其他 | 网关唯一根 agent 是 `root`：`root` 返回全部，其他值返回空页 |
| `cursor=...`、`limit=0`、`limit=501`、`order=...` 非法、未知参数 | `400 invalid_query` |
| metadata 越界（>16 对、key 空/超 64、value 超 512） | `400 invalid_query` |

### 3.2 create / update 的字段往返

| 请求 | 结果 |
| --- | --- |
| create `metadata` / `prompt_cache_options` | 持久化并在 create/get/list 投影一致；响应字段名为官方 `prompt_cache_options`，不出现 `promptCacheOptions` |
| create `metadata:null` / 未给 | 新行为空对象；`prompt_cache_options:null` |
| create `prompt_cache_options:{}` | 视为未配置（`null`） |
| create attach 省略 metadata / prompt cache | 保持行内值，成功（`200 created:false`） |
| create attach 显式相同值 | 成功 |
| create attach 显式不同值，或行内非空时显式 `{}`/`null` | `409 identity_conflict` |
| create/update `agent.model:null` | `400 invalid_request`，`details.field="agent.model"` |
| PATCH `/v1/agents/sessions/{id}` | `405 Method Not Allowed`（官方动词只有 POST） |
| POST update 无 body（无 content-type 或 `Content-Length: 0`）/ JSON `null` | `200` 当前 `SessionView`；不写行（含 `last_activity_at`），不做在途/恢复准入判定。官方 `update(id, null)` 走此形态；`update(id)` 省略 body 时 SDK 发送 `{}`，同样不写行 |
| POST update 非空 body + 非 JSON content-type | `415 invalid_content_type` |
| POST update 非空 body + JSON 语法错误 | `400 invalid_request`，不会被当成 no-op |
| POST update `metadata:{...}` | 整体替换 |
| POST update `metadata:null` 或 `{}` | 清空为 `{}` |
| POST update `prompt_cache_options:{ttl}` / `null` | 整体替换 / 清空 |
| POST update 省略字段 | 保持原值 |
| POST update `agent.tools:[]` / `null` | 清空 MCP 声明；非 MCP tool 仍 `422 unsupported_tool` |
| POST update `review` / `agent.reasoning` / `agent.service_tier` | `400 invalid_request`，`details.field` 指向官方字段，`details.supported=false` |
| POST update 未知顶层字段 | `400 unknown_field` |

### 3.3 update / delete 的准入

| 状态 | 结果 |
| --- | --- |
| live runtime 且 turn 运行中/有未关闭 turn | `409 turn_in_progress`，行不变 |
| 无 live runtime、持久行 `state=running` | `502`，`details.operation="session_resume_required"`，行与 turn 都不变 |
| 无 live runtime、持久行 `idle` 但存在开放 turn 行 | 同上 `502 session_resume_required` |
| 正常 idle 行 | update/delete 正常执行 |

### 3.4 `sessions.delete`

| 步骤 | 结果 |
| --- | --- |
| 删除空闲会话 | `200 {id, object:"agent.session.deleted", deleted:true}` |
| 行级联 | `swarm_sessions`、`swarm_items`、`swarm_inputs`、`swarm_turns`、`swarm_bindings` 全部删除；Turso 一次受管事务 |
| 删除后 get/items/turns/events | `404 unknown_agent` |
| 同一 `logicalId` 重新 create | `201 created:true`，新 `rootSessionId`，items 为空，turns 为空 |
| 持久未完成 turn，无 live runtime | `502 session_resume_required`，不删除任何行 |

### 3.5 turns

| 请求 | 结果 |
| --- | --- |
| `GET .../turns` | 默认 `desc`；按 `(created_at, turn_id)` 倒序；`object:"list"`、`data`、`has_more`/`hasMore`、`nextCursor` |
| `order=asc` | 创建顺序 |
| `limit=2` + `after=<页尾 turn id>` | 排他，下一页不含该 turn |
| `after=<未知 turn id>` | `400 invalid_query` |
| `cursor=...` | `400 invalid_query` |
| `GET .../turns/{turnId}` | `agent.session.turn`；不属于该会话 `404 unknown_agent`（`details.turnId`） |
| `GET .../turns/{turnId}/items` | `404 unsupported_contract`，`details.resource="sessions.turns.items"` |
| 成功 turn | `status:"completed"`、`started_at`/`completed_at` 为 Unix 秒、`usage:null`、`subagent_id:null`、`agent_id:"root"` |
| 取消 turn | 持久 `status:"cancelled"`；live `turn.completed.turn.status="cancelled"`；`turns.list/retrieve` 同为 `cancelled` |
| 失败 turn | `status:"failed"`、`error:{code:"internal_error", message}` 与 live 失败帧同源 |
| actor 崩溃遗留 `running` 行 | `ensure_live` 仍 `502 session_resume_required`；开放 turn 行被关闭为 `failed`（`completed_at` + error）；读路径执行同一关闭 |

### 3.6 显式 404 的官方资源

`GET/POST .../traces`、`GET/POST .../artifacts`、`GET/DELETE .../artifacts/{id}`、
`GET .../artifacts/{id}/content`、`GET .../subagents`、`GET .../subagents/{id}`、
`GET .../subagents/{id}/items`、`GET .../subagents/{id}/turns`、
`GET .../subagents/{id}/turns/{turnId}`、`GET .../subagents/{id}/turns/{turnId}/items`
都注册了路由：

- 未鉴权仍是 `401`（Bearer 门禁先执行）；
- 已鉴权返回网关 JSON `404`，`error.code="unsupported_contract"`，
  `details.resource` 是官方资源名，消息指名该资源；
- 不是 axum 默认纯文本 404。

## 4. 执行命令与结果

所有命令在 `/Users/mino/code/peri/.worktrees/swarm-official-p1b` 执行：

```sh
export PATH="$HOME/.local/share/uv/python/cpython-3.12.14-macos-aarch64-none/bin:/Users/mino/.cargo/bin:$PATH"
export CARGO_HOME=/tmp/cargo-home-rsproxy

cargo test -p peri-swarm
cargo clippy -p peri-swarm --all-targets -- -D warnings
cargo fmt -p peri-swarm -- --check
npx lefthook@1.13.6 run pre-commit
```

结果（2026-11-21，本记录提交的实际执行）：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-swarm` | **326 passed, 0 failed, 6 ignored** |
| `cargo clippy -p peri-swarm --all-targets -- -D warnings` | 通过（0 error） |
| `cargo fmt -p peri-swarm -- --check` | 通过 |
| `lefthook run pre-commit`（npx 1.13.6） | 第一次运行：`check`/`clippy`/`fmt` 在 mise 安装 `rust@1.99.0` 时因无网络失败（`static.rust-lang.org` DNS 不可达），不是命令本身失败；`feature-catalog`/`layer-imports`/`tui-components` 通过。以 `MISE_DISABLE_TOOLS=rust` 复用已激活的 rustc 1.99.0 对**最终暂存内容**重跑同一 hook：**全部 6 个命令通过**（`check` 3.54s、`clippy` 10.68s、`fmt` 4.12s、`feature-catalog` 0.18s、`layer-imports` 1.21s、`tui-components` 0.11s） |

分项计数：

| 目标 | passed | failed | ignored |
| --- | --- | --- | --- |
| lib 单测 | 236 | 0 | 0 |
| `actor_runtime_test` | 16 | 0 | 1 |
| `http_contract_test` | 25 | 0 | 0 |
| `items_pagination_test` | 12 | 0 | 0 |
| `loom_store_test` | 0 | 0 | 3 |
| `mcp_injection_test` | 4 | 0 | 0 |
| `openai_hosted_test` | 7 | 0 | 0 |
| `rivet_actor_test` | 0 | 0 | 2 |
| `session_resources_test` | 12 | 0 | 0 |
| `streaming_test` | 14 | 0 | 0 |

已知 flake：`openai_hosted_test::an_anonymous_hosted_session_reserves_a_bun_workspace`
在默认并行下偶发失败、串行通过；本轮全量运行未复现，不作为回归。

## 5. 真实官方 SDK 探针

环境没有 Loom/Rivet/Bun engine，但 HTTP 契约可以用真实 `openai@7.60.0` 客户端验证。
本轮用一个**临时**（未提交）`peri-swarm/examples/memory_gateway.rs` 在本机
`127.0.0.1:18081` 启动真实 axum 路由 + `InMemorySwarmService`，再用
`/Users/mino/code/peri/.claude/dogfood/node_modules/openai` 执行探针。

探针步骤：`sessions.create`（`metadata={team:alpha}`、
`prompt_cache_options={ttl:"12h"}`）→ `retrieve` → `update`（metadata + ttl 12h→30m）
→ `turns.list`（初始应为空）→ 官方 `events.create` 提交一条
`agent.session.input.message` → 轮询 `turns.list` → `turns.retrieve`。

实际输出摘要：

```json
{
  "create": { "metadata": {"team":"alpha"}, "prompt_cache_options": {"ttl":"12h"} },
  "retrieve": { "metadata": {"team":"alpha"}, "prompt_cache_options": {"ttl":"12h"} },
  "update": {
    "metadata": {"team":"beta"},
    "prompt_cache_options": {"ttl":"30m"},
    "call": { "method": "POST", "url": "http://127.0.0.1:18081/v1/agents/sessions/sdk-probe" },
    "patch_calls": 0
  },
  "turns": {
    "initial_count": 0,
    "object": "list",
    "turn": {
      "object": "agent.session.turn",
      "status": "completed",
      "agent_id": "root",
      "created_at_type": "number",
      "completed_at_type": "number"
    },
    "retrieve_status": "completed"
  }
}
```

探针结论：D1（真实 SDK 的 update 走 POST，且没有任何 PATCH）与 D2（create/retrieve
读到 `prompt_cache_options`；`promptCacheOptions` 为 undefined）都成立；`turns.list`
的 `object/data/has_more` 与 `Turn` 字段可被官方 SDK 解析。该探针不覆盖 Loom/Rivet/
Bun，也不覆盖取消路径（in-memory fake 没有 ACP host）。

## 6. 覆盖点与测试名

- 路由与显式 404：`session_resources_test::unmappable_official_resources_return_gateway_json_404`
  （14 条路径 × 已鉴权/未鉴权，含 subagents 子资源）。
- update 动词与 405：`session_resources_test::update_replaces_and_clears_official_fields`。
- list 分页/过滤/校验：`create_get_and_list_round_trip_metadata_and_prompt_cache`、
  `session_list_query_and_create_validation_are_explicit`、内存 store
  `session_list_orders_by_id_and_matches_metadata_exactly`。
- 官方字段拼写：`create_get_and_list_round_trip_metadata_and_prompt_cache`
  （create/get/list 均断言 `prompt_cache_options` 且无 `promptCacheOptions`）。
- 官方 `agent.session.idle` 的 metadata/prompt cache 投影（reader 读取真实持久行）：
  `reader::tests::idle_frame_carries_the_persisted_metadata_and_prompt_cache_options`；
  `domain::official::tests::official_session_carries_persisted_metadata_and_prompt_cache_options`
  仍只覆盖类型序列化，不再单独作为端到端证据。
- metadata / prompt cache 往返与整体替换/清空：
  `update_replaces_and_clears_official_fields`、`session_patch_replaces_and_clears_official_fields`。
- create attach 三态：`session_resources_test::create_attach_distinguishes_omitted_and_explicit_identity_fields`、
  `service::tests::create_attach_only_conflicts_on_an_explicit_different_value`、
  `service::tests::create_action_keeps_omission_across_the_actor_boundary`。
- `agent.model:null`：`session_resources_test::a_null_agent_model_is_rejected_but_null_tools_clear`
  （`agent.tools:null` 清空 MCP 声明由同一测试断言 store 行 `mcp_tools` 为空、`model`
  保持）。
- update 空 body / JSON `null`：`session_resources_test::update_accepts_an_absent_or_null_body_as_a_no_op`
  （无 body 无 content-type、空 body + `text/plain`、JSON `null` 三种都为 `200`，
  且 store 行的 model、MCP tools、metadata、prompt cache、`last_activity_at` 均未变；
  非空 `text/plain` 为 `415 invalid_content_type`，非法 JSON 为 `400 invalid_request`，
  正常 body 仍整体替换 metadata/model）。
- 持久未完成 turn 的 update/delete 准入：
  `actor_runtime_test::a_persisted_unfinished_turn_refuses_update_and_delete_without_a_runtime`
  （running 与 idle+开放 turn 两种持久行，update（官方 POST）/delete 均 502 且行未变；
  PATCH 不是官方动词，由 `update_replaces_and_clears_official_fields` 单独断言 `405`）；
  live 在途 409 由 `session_resources_test::update_and_delete_are_refused_while_a_turn_is_open` 覆盖。
- 取消 turn：`reader::tests::agent_event_done_terminal_is_the_active_turn`
  （持久 `cancelled`、官方帧 `status=cancelled`、`turns.list/retrieve` 同为 `cancelled`）。
- 删除级联、404、重新 create 干净：`delete_cascades_rows_and_recreate_starts_clean`、
  `delete_session_removes_every_scoped_row`、
  `turns_update_and_delete_round_trip_through_the_actor_service`（真实进程内 ACP host）。
- turns 写入/排序/`after` 排他/retrieve/失败 error：
  `turns_list_is_desc_by_default_and_after_is_exclusive`、`turn_retrieve_is_session_scoped`、
  `a_failed_turn_persists_its_customer_safe_error`、
  `turn_page_is_ordered_and_after_is_exclusive_in_both_orders`。
- 崩溃语义：`an_interrupted_turn_row_is_closed_on_the_next_live_attempt`、
  `close_open_turns_only_touches_non_terminal_rows`。
- 迁移竞态与 SQL 形状：`a_duplicate_column_race_is_retried_only_when_every_migrated_column_exists`、
  `migration_adds_only_the_missing_session_columns`、
  `session_schema_carries_metadata_and_prompt_cache_columns`、
  `turns_schema_is_keyed_for_exclusive_paging`、
  `session_list_sql_adds_one_json_each_predicate_per_metadata_pair`、
  `turn_statements_bind_their_placeholders_and_keyset_columns`、
  `turn_roundtrip_preserves_status_times_error_and_usage`。

## 7. 未验证项

1. **真实 Loom/Turso**：`swarm_turns` 建表、`swarm_sessions` 加列 `ALTER`、
   `json_each(metadata_json)` 过滤、五表单事务删除、`swarm_has_open_turn` 的
   `SELECT EXISTS` 都只有 SQL/参数形状单测；`loom_store_test` 的 3 个测试保持
   ignored。未在真实 Loom 库上执行迁移、分页或两副本并发启动（D5 的竞态重试只有
   决策函数单测，没有真实并发复现）。
2. **真实 Rivet engine**：actor `delete` 后的 `ctx.destroy()`、跨 actor 重建与
   `update`/`delete`/`turns` actions、create attach 三态跨 action 的真实网络往返
   未在 engine 上运行；`rivet_actor_test` 2 项保持 ignored。跨 action 的 `Keep`
   序列化有单测，不是引擎级证据。
3. **生产崩溃注入**：未注入进程崩溃；持久 `running`/开放 turn 的 502 由预置行
   单测覆盖，不是真实崩溃复现。update/delete 与并发受理的交错未做压力验证。
4. **取消路径的端到端**：`cancelled` 持久化由 `ItemLog` 单测覆盖（真实 reader +
   内存 store），没有通过 ACP host 触发一次完整取消后读 `turns.retrieve` 的进程内
   集成测试；SSE 回放层仍不读 `swarm_turns`，更早的取消 turn 在回放中显示为
   `completed`（既有边界，见设计 §4.2）。
5. **`sessions.list` 规模**：没有 1000 会话或大 metadata 集合的吞吐/延迟证据；
   过滤在 SQL 中完成，但没有索引/执行计划验证。
6. **官方 `prompt_cache_options` 的下游效果**：网关只持久化并回显该值；本地模型
   调用链是否使用该 TTL 未验证。
7. **`agent_id` 语义**：网关只有根 agent `root`；没有 saved agent id 的映射，
   其他值按空页处理（已在设计文档写明）。
8. **Lefthook 完整执行**：第一次运行因 mise 联网安装 `rust@1.99.0` 失败而被
   `check`/`clippy`/`fmt` 阻断；第二次用 `MISE_DISABLE_TOOLS=rust`（复用已激活的
   rustc 1.99.0）重跑同一 hook 后全部命令通过。没有使用 `--no-verify`。
9. **update 空 body 语义的证据边界**：只有内存 service + 真实 axum 路由的 HTTP
   契约测试；没有用 `openai@7.60.0` SDK 重跑 `update(id)`/`update(id, null)` 探针，
   也没有在 Loom/Rivet 路径上验证空更新的准入跳过。
