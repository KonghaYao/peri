# Peri Swarm 官方会话资源面验收（list / update / delete / turns）

- 状态：代码与契约测试证据记录；无活体 Loom/Rivet engine，**无真实官方 SDK 进程调用**
- 日期：2026-11-21
- 基线：`39fa6db2f13510f7981513407d9a0940c355f764`（`feat/swarm-official-p1b` 分支基点）
  + 本记录同一提交中的 worktree 改动
- 范围：`openai@7.60.0` `client.beta.agents.sessions` 资源面。已能映射的
  `sessions.list/update/delete` 与 `sessions.turns.list/retrieve` 实现在网关；
  `turns.items`、`traces`、`artifacts`、`subagents` 注册路由并返回网关 JSON
  `404 unsupported_contract`。create/get/events/items 的既有语义不变。
- 参考源码（只读，未修改）：
  `/Users/mino/code/peri/.claude/dogfood/node_modules/openai/resources/beta/agents/sessions/`

## 1. 官方 SDK 事实依据

| 事实 | 源码位置 |
| --- | --- |
| `sessions.list` 使用 `CursorPage`，下一页 `after = data[last].id` | `core/pagination.js` `CursorPage.nextPageRequestOptions` |
| `SessionListParams extends CursorPageParams`（`after`、`limit`），另有 `order`（默认 `desc`）、`agent_id`、`metadata` | `sessions/sessions.d.ts` `SessionListParams` |
| `SessionUpdateParams`：`agent.model`、`agent.reasoning`、`agent.service_tier`、`agent.tools`、`metadata`、`prompt_cache_options`、`review` | `sessions/sessions.d.ts` `SessionUpdateParams` |
| `sessions.delete` 返回 `{id, object:"agent.session.deleted", deleted:true}` | `agents.d.ts` `AgentSessionDeleted` |
| `Turn`：`id`、`object:"agent.session.turn"`、`agent_id`、`session_id`、`status`、`created_at`、`started_at`、`completed_at`、`subagent_id`、`error`、`usage`、可选 `prompt_cache_options` | `sessions/turns/turns.d.ts` `Turn` |
| `TurnListParams extends CursorPageParams`，`order` 默认 `desc`；`after` 在所选顺序上排他 | `sessions/turns/turns.d.ts` |
| `metadata` 限制：最多 16 对、key ≤64、value ≤512 字符；`prompt_cache_options.ttl ∈ {"30m","12h"}` | `sessions/sessions.d.ts` |
| 无法映射的官方资源：`turns.items`、`traces`、`artifacts`、`subagents` | `sessions/{turns/items,traces,artifacts,subagents}.d.ts` |

## 2. 改动

| 位置 | 改动 |
| --- | --- |
| `peri-swarm/src/domain/session.rs` | `PromptCacheOptions`/`PromptCacheTtl`、metadata 上限常量、`SessionsWindow`、`SessionPage`（`has_more` + `hasMore` + `nextCursor`）、`SessionDeleted`；`SessionView.metadata`/`promptCacheOptions` |
| `peri-swarm/src/domain/official.rs` | `TurnsWindow`、`TurnPage`、`TurnStatus::{parse,is_open}`；`OfficialSession.metadata` 真实投影、可选 `prompt_cache_options` |
| `peri-swarm/src/store/mod.rs` | `SwarmSessionRow` 两列、`SwarmSessionPatch`、`SwarmTurnRow`；`swarm_list_sessions`/`swarm_patch_session`/`swarm_delete_session`/`swarm_upsert_turn`/`swarm_get_turn`/`swarm_turn_page`/`swarm_close_open_turns` |
| `peri-swarm/src/store/memory.rs` | 上述端口的同规则内存实现；turn 按 `(created_at, turn_id)` 排序；删除级联 |
| `peri-swarm/src/store/turso.rs` | `swarm_sessions` 新增 `metadata_json`/`prompt_cache_options_json`；新表 `swarm_turns` 与 `(logical_id, created_at, turn_id)` 索引；`PRAGMA table_info` + `ALTER TABLE` 幂等迁移；`json_each` metadata 精确过滤；一次受管事务删除五表 |
| `peri-swarm/src/actor/reader.rs` | `begin_turn` 写 `in_progress` turn 行；`finish_turn` 写 `completed`/`failed`、`completed_at` 与 `error_json`；失败帧与持久错误同源 |
| `peri-swarm/src/actor/service.rs` | `list_sessions`/`update_session`/`delete_session`/`list_turns`/`get_turn`；更新与删除复用 admission 互斥；model/tools 变更重启 host；崩溃遗留 turn 行关闭为 `failed` |
| `peri-swarm/src/actor/swarm.rs` | 新增 actor actions `update`/`delete`/`list_turns`/`get_turn`；`delete` 成功后 `ctx.wait_until` 销毁 actor |
| `peri-swarm/src/gateway/rivet_service.rs` | 路由上述 actions；`list_sessions` 直接读共享 store（跨会话） |
| `peri-swarm/src/service/{mod,in_memory}.rs` | `SessionUpdateCommand`/`UpdateField` 三态；共享 `project_session`、`list_sessions_from_store`、`list_turns_from_store`、`get_turn_from_store`；fake 的 turn 持久化 |
| `peri-swarm/src/http/request.rs` | create 的 `metadata`/`prompt_cache_options` 校验；`sessions.list` 原始查询解析（动态 `metadata[key]`）；`PATCH` 三态解析与未映射字段显式 400 |
| `peri-swarm/src/http/{mod,handlers}.rs` | 新路由与显式 404 handler；`unsupported_resource` 网关 JSON 错误 |
| `peri-swarm/src/domain/error.rs` | `SwarmError::unsupported_resource`（`404` + `unsupported_contract`） |

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
| create `metadata` | 持久化到 `metadata_json`；create/get/list/update 的投影一致 |
| create `prompt_cache_options:{ttl}` | 持久化；`promptCacheOptions` 返回该对象 |
| create `metadata:null` / 未给 | 空对象；`promptCacheOptions:null` |
| create `prompt_cache_options:{}` | 视为未配置（`null`） |
| PATCH `metadata:{...}` | 整体替换 |
| PATCH `metadata:null` 或 `{}` | 清空为 `{}` |
| PATCH `prompt_cache_options:{ttl}` / `null` | 整体替换 / 清空 |
| PATCH 省略字段 | 保持原值 |
| PATCH `agent.tools:[]` / `null` | 清空 MCP 声明；非 MCP tool 仍 `422 unsupported_tool` |
| PATCH `review` / `agent.reasoning` / `agent.service_tier` | `400 invalid_request`，`details.field` 指向官方字段，`details.supported=false` |
| PATCH 未知顶层字段 | `400 unknown_field` |
| 运行中/已受理未开始的 turn | PATCH/DELETE 都是 `409 turn_in_progress` |

### 3.3 `sessions.delete`

| 步骤 | 结果 |
| --- | --- |
| 删除空闲会话 | `200 {id, object:"agent.session.deleted", deleted:true}` |
| 行级联 | `swarm_sessions`、`swarm_items`、`swarm_inputs`、`swarm_turns`、`swarm_bindings` 全部删除；Turso 一次受管事务 |
| 删除后 get/items/turns/events | `404 unknown_agent` |
| 同一 `logicalId` 重新 create | `201 created:true`，新 `rootSessionId`，items 为空，turns 为空 |
| 运行中删除 | `409 turn_in_progress`，不删除任何行 |

### 3.4 turns

| 请求 | 结果 |
| --- | --- |
| `GET .../turns` | 默认 `desc`；按 `(created_at, turn_id)` 倒序；`object:"list"`、`data`、`has_more`/`hasMore`、`nextCursor` |
| `order=asc` | 创建顺序 |
| `limit=2` + `after=<页尾 turn id>` | 排他，下一页不含该 turn |
| `after=<未知 turn id>` | `400 invalid_query` |
| `cursor=...` | `400 invalid_query` |
| `GET .../turns/{turnId}` | `agent.session.turn`；不属于该会话 `404 unknown_agent`（`details.turnId`） |
| `GET .../turns/{turnId}/items` | `404 unsupported_contract`，消息含 `sessions.turns.items` |
| 成功 turn | `status:"completed"`、`started_at`/`completed_at` 为 Unix 秒、`usage:null`、`subagent_id:null`、`agent_id:"root"` |
| 失败 turn | `status:"failed"`、`error:{code:"internal_error", message}` 与 live 失败帧同源 |
| actor 崩溃遗留 `running` 行 | `ensure_live` 仍 `502 session_resume_required`；开放 turn 行被关闭为 `failed`（`completed_at` + error）；读路径执行同一关闭 |

### 3.5 显式 404 的官方资源

`GET/POST .../traces`、`GET/POST .../artifacts`、`GET/DELETE .../artifacts/{id}`、
`GET .../artifacts/{id}/content`、`GET .../subagents`、
`GET .../subagents/{id}`、`GET .../turns/{turnId}/items` 都注册了路由：

- 未鉴权仍是 `401`（Bearer 门禁先执行）；
- 已鉴权返回网关 JSON `404`，`error.code="unsupported_contract"`，
  `details.resource` 是官方资源名，消息指名该资源；
- 不是 axum 默认纯文本 404。

## 4. 执行命令与结果

所有命令在 `/Users/mino/code/peri/.worktrees/swarm-official-p1b` 执行：

```sh
export PATH="$HOME/.local/share/uv/python/cpython-3.12.14-macos-aarch64-none/bin:/Users/mino/.cargo/bin:$PATH"
export CARGO_HOME=/tmp/cargo-home-rsproxy
export CARGO_TARGET_DIR=/Users/mino/code/peri/target

cargo test -p peri-swarm
cargo clippy -p peri-swarm --all-targets -- -D warnings
cargo fmt -p peri-swarm -- --check
npx --yes lefthook@1.13.6 run pre-commit   # 本机未安装 lefthook 二进制，用同版本 npm 包执行同一配置
```

`scripts/cargo-rmcp-patched.sh` 需要从 crates.io 拉取并打补丁的 rmcp/hyper-util/
turso_serverless 缓存；本轮无外网，缓存从主 worktree 的 `target/peri-*-patches`
复制到本 worktree 的 `target`（仅构建缓存，未改源码）。

结果（2026-11-21）：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-swarm` | **318 passed, 0 failed, 6 ignored** |
| `cargo clippy -p peri-swarm --all-targets -- -D warnings` | 通过（0 error） |
| `cargo fmt -p peri-swarm -- --check` | 通过 |
| `lefthook run pre-commit`（npx 1.13.6） | 通过：`check`、`clippy`、`feature-catalog`（22 单测 + 文档校验）、`fmt`、`layer-imports`（22 条规则、0 违规）、`tui-components` 全绿 |

分项计数：

| 目标 | passed | failed | ignored |
| --- | --- | --- | --- |
| lib 单测 | 232 | 0 | 0 |
| `actor_runtime_test` | 15 | 0 | 1 |
| `http_contract_test` | 25 | 0 | 0 |
| `items_pagination_test` | 12 | 0 | 0 |
| `loom_store_test` | 0 | 0 | 3 |
| `mcp_injection_test` | 4 | 0 | 0 |
| `openai_hosted_test` | 7 | 0 | 0 |
| `rivet_actor_test` | 0 | 0 | 2 |
| `session_resources_test` | 9 | 0 | 0 |
| `streaming_test` | 14 | 0 | 0 |

## 5. 覆盖点与测试名

- 路由与显式 404：`session_resources_test::unmappable_official_resources_return_gateway_json_404`
  （10 条路径 × 已鉴权/未鉴权）。
- list 分页/过滤/校验：`create_get_and_list_round_trip_metadata_and_prompt_cache`、
  `session_list_query_and_create_validation_are_explicit`、内存 store
  `session_list_orders_by_id_and_matches_metadata_exactly`。
- metadata / prompt cache 往返与整体替换/清空：
  `create_get_and_list_round_trip_metadata_and_prompt_cache`、
  `update_replaces_and_clears_official_fields`、`session_patch_replaces_and_clears_official_fields`、
  `official_session_carries_persisted_metadata_and_prompt_cache_options`、
  `update_field_distinguishes_absent_null_and_value`、
  `a_kept_update_field_is_omitted_and_a_cleared_one_is_null`。
- 校验边界（16 对、key 64、value 512、非法 ttl）：
  `session_list_query_and_create_validation_are_explicit`。
- 删除级联、404、重新 create 干净：`delete_cascades_rows_and_recreate_starts_clean`、
  `delete_session_removes_every_scoped_row`、
  `turns_update_and_delete_round_trip_through_the_actor_service`（真实进程内 ACP host）。
- turns 写入/排序/`after` 排他/retrieve/失败 error：
  `turns_list_is_desc_by_default_and_after_is_exclusive`、`turn_retrieve_is_session_scoped`、
  `a_failed_turn_persists_its_customer_safe_error`、
  `turn_page_is_ordered_and_after_is_exclusive_in_both_orders`、
  `turns_update_and_delete_round_trip_through_the_actor_service`。
- 崩溃语义：`an_interrupted_turn_row_is_closed_on_the_next_live_attempt`、
  `close_open_turns_only_touches_non_terminal_rows`。
- 迁移与 SQL 形状：`migration_adds_only_the_missing_session_columns`、
  `session_schema_carries_metadata_and_prompt_cache_columns`、
  `turns_schema_is_keyed_for_exclusive_paging`、
  `session_list_sql_adds_one_json_each_predicate_per_metadata_pair`、
  `turn_statements_bind_their_placeholders_and_keyset_columns`、
  `turn_roundtrip_preserves_status_times_error_and_usage`。

## 6. 未验证项

1. **真实 Loom/Turso**：`swarm_turns` 建表、`swarm_sessions` 加列 `ALTER`、
   `json_each(metadata_json)` 过滤、五表单事务删除都只有 SQL/参数形状单测；
   `loom_store_test` 的 3 个测试保持 ignored。未在真实 Loom 库上执行迁移或分页。
2. **真实官方 SDK 客户端**：没有运行官方 `openai@7.60.0` Node 客户端进程；
   `CursorPage` 自动翻页由源码契约（`data[last].id`）与响应形状推断，没有真实
   客户端消费记录。
3. **真实 Rivet engine**：actor `delete` 后的 `ctx.destroy()`、跨 actor 重建与
   `update`/`delete`/`turns` actions 未在 engine 上运行；`rivet_actor_test` 2 项保持
   ignored。
4. **生产并发/崩溃注入**：未注入进程崩溃；崩溃遗留 turn 的关闭由预置 `running`
   行 + turn 行的单测覆盖，不是真实崩溃复现。更新/删除与并发受理的交错也未做
   压力验证。
5. **`sessions.list` 规模**：没有 1000 会话或大 metadata 集合的吞吐/延迟证据；
   过滤在 SQL 中完成，但没有索引/执行计划验证。
6. **官方 `prompt_cache_options` 的下游效果**：网关只持久化并回显该值；本地模型
   调用链是否使用该 TTL 未验证。
7. **`agent_id` 语义**：网关只有根 agent `root`；没有 saved agent id 的映射，
   其他值按空页处理（已在设计文档写明）。
