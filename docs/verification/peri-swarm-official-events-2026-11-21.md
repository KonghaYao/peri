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
