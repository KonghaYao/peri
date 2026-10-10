# peri-cf 架构转向：无 Durable Object、零表、零持久化、按需 ACP 重放复原

状态：**已实施（本地范围）/ 部署端未验收**（2026-10-10）。用户已裁决转向；代码由代码 owner 在同一分支落地（工作树复核：无 DO 入口与注册表已就位、`worker/chat/record.ts` 已删除、`wrangler.jsonc` 无 DO 绑定/迁移、命令端点 426、WS 控制帧与 `ChatCommandPort` 已接线）。本文只记录决策、证据、受影响面与风险；本地测试与部署端验收由代码 owner 记录。**生产切换未执行**。

取代关系：

- 取代 [P3 批准包](2026-10-10-peri-cf-chat-objects-p3-approval.md) 的 §1（DO `recordVersion:2` 记录结构）、§2（DO 分段持久化 payload）、§3（DO 容量/保留/GC 数值）、§4（平台资源清单）与 §6 逐项性质标注中与 DO 持久化相关的部分；该文件的证据（官方文档抓取、探针实测）仍有效并作为本文对照。
- 取代 [实施 issue](2026-10-10-peri-cf-chat-objects-implementation.md) 的「P3 交付记录」与「P5 交付记录」中把 DO 记录/DO socket attachment 当作展示持久化与连接恢复基线的部分；P0–P2、P4 的对象契约与 view 结论仍有效。
- 取代 [设计](../../docs/design/peri-cf-chat-objects.md) 原有 5.3.4 平台结构裁决与四张表草案；设计 5.3 已改写为新基线。

## 1. 决策

用户 2026-10-10 裁决（要点，原话摘录）：

> 不允许使用 DO / CF 专有原语；「所有表删除，无需依赖，所有的数据都可以通过 peri wasm + turso session 存储复原出来，故不需要你的四张表，可以说 yjs 的数据无需持久化」。

落成的架构基线：

1. **无 Durable Object**：不注册 DO class、不配置 `durable_objects` 绑定与 `migrations`，`wrangler.jsonc` 只有 Worker + 静态资源。
2. **零表、零持久化**：删除 DO 记录适配器（`worker/chat/record.ts`，含 `recordVersion:2` 索引、`payload:<id>:<n>` 分段、`record:v1-backup` 回滚窗口与孤儿清扫）；不引入 R2/KV/D1 等替代持久层。
3. **展示态按需复原**：会话展示由 ACP `session/load` 的历史重放重建；执行驻留在接受连接/请求的 isolate 内。
4. **会话命令走 WS 文本控制帧**：prompt / cancel / interaction 回答 / payload 全文读取经同一条 WS 连接下发（HTTP 没有会话亲和性）；二进制帧仍是 SDK 文档同步帧。
5. **HTTP 只保留无状态面与 peek**：`GET/POST /api/v2/chats`（Turso）、`GET /api/v2/chats/:id`（触发复原读）、`GET /api/v2/chats/:id/resources`（peek，不触发复原）、`GET /api/instances`（仅 peek）、`GET /api/v2/chats/:id/sync`（WS 升级）；原命令/载荷端点返回 `426`。
6. **持久权威唯一**：Turso/libSQL 的 Rust Store 是唯一跨 isolate 持久事实源；Yjs 展示态、运行元数据、连接状态都只在 isolate 内存。

### 1.1 理由

1. 展示态可从权威历史重建，持久化它是第二份权威，带来迁移、回滚、容量与 GC 复杂度（P3 批准包 §1–§3 全部由此而来）。
2. DO 记录只能保存应用侧投影，重建覆盖范围依赖 SDK checkpoint 版本与分段完整性；而 ACP 重放直接来自 Rust Store 的 canonical 历史，链路更短。
3. 避免 CF 专有原语（DO 存储、Hibernation socket attachment、`acceptWebSocket` 事件入口）成为应用正确性的前提。
4. 命令在 HTTP 上无法路由到持有执行状态的实例：HTTP 无会话亲和性，因此命令必须与应用连接同路（WS）。

## 2. 证据

### 2.1 官方文档（2026-10-10 抓取；来源见链接）

| 事实 | 数值 | 来源 |
| --- | --- | --- |
| Workers 免费档 CPU/请求 | **10 ms**（Paid 5 min，默认 30 s） | <https://developers.cloudflare.com/workers/platform/limits/>（CPU time 一节） |
| Workers 内存/isolate | 128 MB（含 JS heap 与 WASM 分配） | 同上（Memory 一节） |
| Workers subrequest/请求 | 免费 50 / Paid 10,000 | 同上（Subrequests 一节） |
| Worker 体积 / 启动 | 未压缩 64 MiB / 启动 1 s | 同上（Worker size、Worker startup time） |
| 超 CPU 的错误形态 | 1102 `Worker exceeded resource limits` | 同上（Error: exceeded CPU time limit） |
| wall time（HTTP） | 无硬上限（客户端保持连接）；`waitUntil` 在响应/断连后最多延长 30 s | 同上（Duration、Wall time limits 表） |
| DO（SQLite 后端）CPU/请求 | 30 s（默认，可配置 5 min）；免费档 DO 可用 | <https://developers.cloudflare.com/durable-objects/platform/limits/> |
| DO 单值 / WebSocket 消息 | key+value ≤ 2 MB / 接收 32 MiB | 同上 |

DO 行只作对照：本设计不使用该原语，数值不再构成容量约束。

### 2.2 复原机制（代码事实，`session/load` 在响应前重放）

| 事实 | 证据 |
| --- | --- |
| ACP `session/load` 在返回响应**之前**用 `session/update` 重放持久历史 | `peri-acp/src/host/requests/session_restore.rs:467`（`handle_load`）与 `:489`（注释 "replay history via session/update BEFORE responding"），重放调度自 `:493` |
| 改造前时序错误：先 `await session/load`、后订阅 → 重放通知全部丢弃（行号为改造前 HEAD `d0842728` 版本，用 `git show d0842728:npm-packages/peri-cf/worker/chat/session.ts` 复核） | `worker/chat/session.ts:261`（`session/load`）先于 `:270`（`transport.subscribe`）；DO 记录因此成为唯一复原来源 |
| 新基线：先订阅/建 runtime，再触发重放；重放直接进入 SDK 投影 | `npm-packages/peri-cf/worker/chat/session.ts:197`（"关键顺序：先订阅再 session/load"）与 `hydrate()`；`worker/chat/sync.ts` 的 `SocketRuntimeBridge.hydrate()`、`worker/chat/registry.ts`（isolate 内 runtime 注册表） |

### 2.3 本机 WASM 冷启动实测

| 口径 | 结果 | 说明 |
| --- | --- | --- |
| 用户报告 | 11.8 MB `peri_wasm.wasm` 编译一次 ≈ **66.6 ms**（Bun / ARM Mac） | 2026-10-10，用户提供 |
| Owner 复测 `WebAssembly.compile` | 9.5 / 10.1 / 10.7 / 10.9 / 16.9 ms（Bun 1.4.2，macOS arm64） | 产物 `npm-packages/peri-cf/worker/wasm/generated/peri_wasm.wasm` = 11,823,967 字节 |
| Owner 复测 `WebAssembly.instantiate`（stub imports） | 18.9 / 19.8 / 24.4 / 55.3 ms | 与用户数值同量级；口径不同（compile vs instantiate vs 端到端宿主启动） |
| 实际代码路径 | 每个 WASM Host 启动都走 `WebAssembly.instantiate(wasmModule, imports)` | `npm-packages/peri-cf/worker/wasm/host.ts` 的 `instantiateModule`；宿主启动预算 20 s（`worker/chat/session.ts` `startHost`） |

结论：**无论按哪个口径，冷启动单次开销都 ≥ 免费档 10 ms/请求 CPU 预算**；免费档部署很可能返回 1102。该结论与用户已知情的要求一致，按文档结论推进、部署端复验。

### 2.4 其他代码事实（转向时的改动面）

| 事实 | 证据（工作树，2026-10-10 复核） |
| --- | --- |
| 控制帧协议：文本帧 = 应用命令/结果，二进制帧 = SDK 文档同步 | `npm-packages/peri-cf/shared/sync.ts` 的 `chatCommandSchema` / `chatCommandResultSchema` / `parseControlFrame`（payload 命令带可选 `bytes` 做完整性断言） |
| 重放标记：客户端声明 `peri.replay`，宿主给重放的 `session/update` 打 `_meta.periReplay`；应用据此区分历史与实时并关闭重放 turn | `npm-packages/peri-cf/worker/chat/bootstrap.ts`（`CLIENT_CAPABILITIES`）；`peri-acp-types/src/peri_caps.rs:17-19,84,129`；`peri-acp/src/dispatch/session_replay.rs:222,232,243`；`worker/chat/session.ts` 的 `isReplayChunk` + `projection.closeReplayedTurn()` |
| 连接层平台 seam：升级、保活、时钟可注入；控制帧命令经 runtime 执行 | `worker/chat/sync.ts` 的 `SocketHost`（`cfSocketHost` 用**升级请求**的 `ExecutionContext.waitUntil`）、`SocketRuntimeBridge.run/inFlight` |
| isolate 内运行期注册表（`runtimeFor`/`peek`/LRU 回收，上限 8） | `worker/chat/registry.ts`（`MAX_RESIDENT_RUNTIMES = 8`；`peek` 不创建、不 hydrate） |
| 单会话运行期：`hydrate()` 重放复原、执行、取消、交互、载荷 | `worker/chat/session.ts`（`ChatRuntime`；`hydrate` 在 `:132`，先订阅再 `session/load` 在 `:197-210`） |
| 命令失败码（控制帧没有 HTTP 状态行） | `worker/chat/session.ts:24-33`：`busy`（原 409）、`blocked`（原 503）、`cancel_timeout`/`cancel_failed`、`settled`/`invalid_answer`、`unavailable`/`integrity_error`/`too_large`、`not_found`、`internal_error` |
| 无 DO 入口 | `worker/index.ts`（`createApi({ repository, startTransport, runtimes })`） |
| 绑定与迁移删除 | `wrangler.jsonc` 已无 `durable_objects` 与 `migrations` |
| 命令/载荷端点 426；读面 `GET /api/v2/chats/:id` 触发复原、`/resources` peek、`/api/instances` peek | `worker/chat/routes.ts`、`worker/api/router.ts` |
| DO 记录适配器删除 | `worker/chat/record.ts` 已删除（355 行）；`tests/record-store.test.ts`、`tests/p3-migration-drill.test.ts` 同步删除 |
| 同步连接状态不再持久化 | `worker/chat/ws-delivery.ts` 删除 `SYNC_ATTACHMENT_VERSION` / `attachmentSchema` / `SocketAttachment`；连接上限 `MAX_SYNC_CONNECTIONS = 16` |
| 浏览器命令面 | `web/api/sync.ts` 的 `ChatCommandPort`（结果按帧 id 匹配，断线/超时整体作废、不重放）；`web/api/client.ts` 只读，426 提示刷新 |

## 3. 受影响面

### 3.1 命令通道：HTTP → WS

| 旧（P3 已批准入口） | 新基线 |
| --- | --- |
| `POST /api/v2/chats/:id/messages` | 文本控制帧 `{type:"command", op:"prompt"}` |
| `POST /api/v2/chats/:id/cancel` | 文本控制帧 `op:"cancel"` |
| `POST /api/v2/chats/:id/interactions/:interactionId` | 文本控制帧 `op:"interaction"` |
| `GET /api/v2/chats/:id/payloads/:refId` | 文本控制帧 `op:"payload"`（结果 `payload` 带 `{id,bytes,json}`） |

- 旧 HTTP 命令/载荷路径保留为 **426 Upgrade Required**（`{error, upgrade:true}`），不做长期双写。
- 命令结果的确定性：`command_result.ok=false` 是服务端明确答否（执行状态确定）；未获确认（断线/超时/未同步）时执行状态不确定，调用方必须阻断重跑（`web/api/sync.ts` 的 `CommandFailure.source`）。

### 3.2 读取面语义

| 端点 | 语义 |
| --- | --- |
| `GET /api/v2/chats`、`POST /api/v2/chats` | 无状态：Turso Rust Store 列表/创建，不触发复原 |
| `GET /api/v2/chats/:id` | **触发按需复原**（重放 `session/load` 后返回同一 SyncState DTO） |
| `GET /api/v2/chats/:id/resources` | peek：只读本 isolate 当前内存态，不触发复原、不启动 Host |
| `GET /api/instances` | peek：本 isolate 无该会话内存态即无实例观测；不再"轮询唤醒 DO" |
| `GET /api/v2/chats/:id/sync` | WS 升级；鉴权成功后 hydrate（必要时触发复原）再推送文档 |

### 3.3 其他

- **执行归属**：执行驻留在接受连接的 isolate；isolate 回收 = 在途执行丢失，**不复原**（与既有"不恢复旧 Agent 执行"一致）。
- **唯一持久事实**：Turso Rust Store；`PERI_MACHINE_ID` 仍是部署持久 UUID（`worker/wasm/config.ts` 校验）。
- **前端**：命令全部经 `ChatCommandPort`；HTTP 客户端只读。`web/chat/payloadReader.ts` 改为 WS 载荷读取。

## 4. 风险与未验证项

> 本节要求原文保真，不得淡化。以下 6 条同时写进[设计 5.3 与 10.2](../../docs/design/peri-cf-chat-objects.md)。

1. **免费档 10 ms/请求 CPU vs 本机实测 WASM 冷启动 66.6 ms**（§2.1、§2.3）：部署到免费档很可能 1102。用户已知情并要求按文档结论推进；这是部署端验收的第一项。
2. **无 DO 后长时执行的 isolate 保活没有平台保证**：WS 消息处理里的 `await` 与升级请求的 `waitUntil` 只是尽力而为（`worker/chat/sync.ts` 的 `keepAlive` 注释已标注）；只有在部署端才能验证长时执行是否被回收。
3. **HTTP 无会话亲和性**：因此命令必须走 WS；多 isolate 下同一会话可能出现多份内存态（各自持有不同连接），客户端以自己的连接为准，跨连接不保证一致。
4. **每个 isolate 只有一份 64 MB WASM 预算**（`shared/runtime-limits.ts` 的 `CF_WASM_ISOLATE_RESERVED_BYTES`、`worker/wasm/budget.ts` 的 `WasmCapacityError`）：同一 isolate 内并发执行会被容量预留拒绝，命令失败进入 `command_result` 失败码路径（非 `CommandError` 时当前映射为 `internal_error`），并按 `startHost` 失败路径置本 isolate 的执行阻断（`blocked`）。容量拒绝不是可重试即可的资源错误，必须有显式失败与用户可见原因。
5. **未决**：远程 Turso DB 地址与令牌尚未提供；SDK `checkpoint` API（`SessionDocs.fromCheckpoint/exportCheckpoint`）已无生产消费方，去留待用户裁决。
6. **事实核查纪律**：向用户报告时区分「已核对（文档/代码/实测）」与「推断」；本文 §2.2/§2.3 的实测口径已标注，§5 列出无法与最终代码对齐的项。

## 5. 待核实清单（本文写作时无法与最终代码对齐）

1. `worker/chat/routes.ts`、`worker/api/router.ts` 的最终路由表逐条语义（哪些 426、哪些保留 peek）以最终源码为准；本文按 2026-10-10 工作树复核。
2. payload 全文在重放后是否完整可重建：取决于 ACP 历史重放是否携带完整工具参数/结果（不被截断）；未验证。`worker/chat/session.ts` 复原路径对重放 chunk 的判定（`isReplayChunk`）亦以最终源码为准。
3. `GET /api/v2/chats/:id/resources` 与 `GET /api/instances` 的 peek 实现细节（是否完全无副作用、注册表 LRU 是否会被 peek 触碰）以最终源码为准。
4. 代码 owner 的测试与 smoke 结果（`tests/hydrate-replay.test.ts`、`tests/real-workerd-ws.test.ts`、`scripts/smoke.mjs`）不在本文结论内，待其记录。

## 6. 验证记录（文档侧，本 owner）

| 检查 | 结果 |
| --- | --- |
| 本地 Markdown 链接检查（改动文件集） | 见交付报告；目标文件缺失 0 |
| `git diff --check` | 无空白错误 |
| 文件行数（`docs/standards/STD-SIZE-001`） | 本文与设计文档均为 Markdown，脚本不适用；源码文件未由本 owner 改动 |

代码侧测试（`bun run test`、`real-workerd-ws.test.ts`、smoke）与部署端验收（免费档 CPU、长时保活、真实浏览器、远程 Turso）由代码 owner 记录，不属本文结论。

## 7. 明确不做

1. 不用 R2/KV/D1/Queue 等替代持久层保存展示态或连接状态。
2. 不实现跨 isolate 的会话接管、在途执行恢复或"另一实例看到同一内存态"。
3. 不为旧 DO 记录保留迁移/回滚路径（无旧记录可迁移：转向时未部署）。
4. 不恢复 `peri/execution/*` 准入或 `session/control*` 精确停止协议。
