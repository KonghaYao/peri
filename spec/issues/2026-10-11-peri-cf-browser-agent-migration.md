# 2026-10-11 peri-cf 浏览器宿主 Agent 迁移

> **状态：superseded（2026-10-25）。** 浏览器宿主/Worker relay 路径已不在工作树；当前实现是
> Bun 服务端 + SDK ChatManager/ChatService + HTTP Action + WebSocket/Yjs，见
> [2026-10-24 SDK Chat 重构](2026-10-24-peri-sdk-chat-refactor.md) 与
> [服务端 Agent 设计](../../docs/design/peri-cf-server-agent.md)。本文只保留迁移历史，不得作为
> 当前入口或验收依据。

目标设计：[docs/design/peri-cf-browser-agent.md](../../docs/design/peri-cf-browser-agent.md)。
范围：仅 `npm-packages/peri-cf`（含其依赖的 peri-wasm / peri-resources / peri-model 接线）。

## 分步

1. **宿主可行性**：浏览器加载 peri-wasm、启动 ACP Host、经 fetch 传输完成一次
   `initialize → session/new → session/load` 与一次真实模型调用。
2. **store 传输 seam**：`turso_serverless` 的 `HttpExecutor` seam（仓库补丁 +
   `wasm-fetch` feature）；浏览器构建经 `/api/relay/storage/*` 走宿主 `fetch`；CF/native 路径
   行为不变。
3. **浏览器构建 profile**：peri-cf 产出浏览器 wasm 产物与胶水适配（不含 socket/DNS 依赖）。
4. **无状态后端**：Worker 收敛为静态资源 + `/api/relay/model/*` + `/api/relay/storage/*` +
   `/api/config`；删除 `worker/chat/*`、`worker/wasm/*`、`/api/v2/*` 会话面与实例观测端点。
5. **前端迁移**：会话运行时、投影、命令、交互、载荷读取从 `worker/chat/*` 前移到 `web/`；
   UI 改为消费本地宿主；删除 WS 同步客户端与 replica 传输用途。
6. **文档**：README、CLAUDE.md、code-index 与设计基线同步。

## 验收（用户执行）

- 浏览器打开应用，无需任何凭据即可列出/创建会话并完成一轮真实模型对话。
- 刷新页面后会话从远端 Store 重建；模型密钥与存储 token 不出现在前端产物或网络明文。
- Worker 不持有任何跨请求状态：同一请求在任意实例上结果一致。

## 实施状态（2026-10-11，代码级）

1. 宿主可行性：**已实测通过**（见下「浏览器实测记录」）。
2. store 传输 seam：已落地（`patches/turso-serverless-0.1.3-wasm-fetch.patch` + `peri-resources` 的
   `wasm-fetch` feature + `scripts/cargo-rmcp-patched.sh` 校验值）；**改动过补丁，必须先
   `bun run build:wasm` 重建**。
3. 浏览器构建 profile：`prepare:wasm` 把带 glue 替换的产物发布到 `web/wasm/generated`，
   `web/host/module.ts` 经 Vite `?url` 资源 + 显式 `instantiateWasm` 实例化；只注入调度器端口。
   SDK `/portable` 的准入面引用 `crypto`，浏览器构建以显式失败的替身替换
   （`web/shims/nodeCrypto.ts` + Vite alias）；peri-cf 不消费该面，调用即报错。
4. 无状态后端：已删除 `worker/chat/*`、`worker/wasm/*`、`worker/instances/*`、`worker/sdk/*`、
   `worker/api/json.ts` 与 `/api/v2/*`、`/api/instances`、426 旧面；`worker/index.ts` +
   `worker/api/{router,app,http,relay}.ts` 只剩静态资源、`GET /api/config` 与 relay。
   （注：该句已被下文「配置与凭据迁移」追加节取代——`GET /api/config` 已删除。）
5. 前端迁移：新增 `web/host/`、`web/runtime/`（application/registry/session/projection/connection/
   creation/interactions/repository/logging）；删除 `web/api/sync.ts`、`web/instances/`、
   `shared/{resources,instances}.ts` 与控制帧 schema；UI 消费本地 `ChatApi`/`ChatConnection`。
   投影不再实例化 `SessionDocSync`（无跨网络同步），`shared/sync-state.ts` 只保留展示物化
   （`readSyncState`/`UnsupportedSchemaError` 随服务端投影删除；unsupported pair 由 `web/api/client.ts` 拒绝）。
   实例观测端点随 Worker 观测面一并删除（浏览器内可观测范围只有本标签页运行期，未提供替代 UI）。
6. 文档：README、CLAUDE.md、code-index、设计索引已同步；`shared/runtime-limits.ts` 属于 wasm
   构建输入，本次未改动以免额外作废产物缓存。

**附带删除**：`scripts/{smoke.mjs,e2e.mjs,e2e-config.mjs,local-dns.ts}` 与对应 package scripts
（它们驱动的是已删除的 WS/HTTP 面）；浏览器端行为验收目前以手工验证为准。

**用户侧执行顺序**：`npm-packages/@peri-sdk` 先 `bun run build` 刷新 dist（新 `/wasm` 导出与
`createSchedulerPort` 端口拆分），再在 peri-cf 执行 `bun run build:wasm` → `bun run typecheck` →
`bun run build` → 浏览器实测（需要本地 sqld 与 `.dev.vars`）。

## 浏览器实测记录（2026-10-11）

环境：真实 sqld（独占库）+ 本地 workerd（`bun run preview`，8791）+ Playwright Chrome，模型为真实 `deepseek-v4-flash`（经 `/api/relay/model/*`）。

1. **端到端通过**：页面创建新会话 → 宿主在浏览器内启动 → 发送 prompt → 真实模型调用 200（一次主调用 + 一次「预测下一步输入」调用）→ 回复渲染；`peri_store_meta` 落 `contract=peri.session.store/v5`、`schema_version=11`，`threads` 1 行、`messages` 用户/助手各 1 行。
2. **刷新重建通过**：整页重新加载后侧栏列出该会话（store 查询），点击后经 `session/load` 重放重建完整转录。
3. **前端不含密钥**：`dist/client` 中 `.dev.vars` 的模型 key 与存储 token 命中 0；仅占位 `relay-injected`。
4. **实测中修掉的宿主缺陷**：
   - Emscripten 胶水的 `NODERAWSOCKETS` Node-only 断言在浏览器必抛（宿主启动直接失败）；`scripts/worker-glue.ts` 该适配移除断言（改动后必须 `bun run build:wasm` 重建）。
   - 流被主动中止时 `reader.cancel()` 以 AbortError 拒绝，被记成 console 错误；`peri-model` 的 fetch 胶水按取消语义处理（非 AbortError 仍上报）。
5. **dev store 前提（重要）**：本地必须用**独占**的 libSQL 库。原先指向共享库（本机 8088 的 1Panel libsql 默认命名空间）时宿主启动即失败：那里既有 peri 的旧代 store（schema 19 / 契约 v4，压缩前开发代），又有 Langfuse 的 `projects` 表与 peri canonical 表名撞名；构建按 fail-closed 拒绝，报 `Peri WASM host startup failed` + `behavior is unsupported here (NotApplied)`。当前用 `sqld --http-listen-addr 127.0.0.1:8090 --db-path /tmp/peri-cf-dev-store`，`.dev.vars` 的 `PERI_STORAGE_URL` 指向它（README「本地启动与构建」已写明）。
6. **未覆盖**：Cloudflare 托管部署与真实 Turso 云库、多标签页并发、刷新丢失在途执行的行为（设计已定，未实测）。

## 已知风险

- 第 1 步已在本地 workerd + Chrome 实测成立（见上）；托管环境（Cloudflare 上的 Emscripten/Tokio
  事件循环、实例资源预算）仍以部署实测结论为准。
- 首屏成本：wasm 产物约 11.8 MB（gzip 约 5 MB），缓存策略未评估。
- `config.ts` 的 relay 凭据是占位值（`relay-injected`），依赖 relay 端剥离与替换；部署入口
  控制是必要条件。

## 配置与凭据迁移（2026-10-11，BYOK 零配置后端，追加）

用户裁决（2026-10-11）：放弃「模型 key / 存储 token 不进浏览器、只在 Worker 注入」，改为浏览器
BYOK 零配置后端。本节为追加记录，不改写上文历史条目。

1. **决策与合同**
   - Worker 零配置、零状态、纯透传：删除 `GET /api/config`；env 全部移除（`worker/types.ts` 仅剩
     `ASSETS`，`wrangler.jsonc` 无 vars，`.dev.vars.example` 删除；本机 `.dev.vars` 保留但已不被读取）。
   - relay 上游由请求路径第一段携带：`/api/relay/<kind>/<encodeURIComponent(上游绝对基址)>/<上游路径>?query`，
     kind = `storage` | `model`；基址必须 http/https，否则 400。凭据（`authorization` / `x-api-key` /
     `api-key`）不再剥离或注入，按客户端真实值透传；存储 JSON 响应的 `base_url` 改写为
     `/api/relay/storage/<原编码段><pathname>`。
   - 浏览器配置：新增 `web/settings/config.ts`（localStorage 单键 `peri-cf.browser-config.v1`，
     形状 `{ machineId, model: { provider, id, baseUrl, apiKey }, storage: { url, token } }`）与
     `web/settings/SettingsView.tsx`（首次运行门禁 + 标题栏齿轮入口）；machineId 改为客户端
     `crypto.randomUUID()` 生成并长期持久化。
2. **改动面**：`worker/{index,types}.ts`、`worker/api/{router,relay,app,http}.ts`、`wrangler.jsonc`、
   `.dev.vars.example`（删除）、`web/host/{index,config}.ts`、`web/settings/*`（新增）、
   `web/runtime/repository.ts`、`web/App.tsx`、`web/api/client.ts`。
3. **状态**：实现已完成（双端合同互验：worker 侧与浏览器侧各一 owner；`typecheck` 与 `vite build`
   通过）；**用户手工验收未进行**。
4. **安全姿态（用户裁决）**：密钥现存 localStorage 并经 relay 透传，DevTools 网络可见；部署语义
   仍是「拿 URL 即访问」，入口控制由 Cloudflare Access 或等效设施承担；relay 现为按请求目标转发，
   未挂入口控制时等价开放转发面（已知风险，无代码限制）。
5. **被取代的条目**：上文「验收（用户执行）」第 2 条「模型密钥与存储 token 不出现在前端产物或网络
   明文」，以及「已知风险」最后一条（`relay-injected` 占位凭据依赖 relay 剥离与替换）随本裁决失效：
   凭据改由浏览器持有并原样透传。
6. **旧会话与 machineId**：dev store 中的旧会话归旧 machineId；新身份下这些会话的机器归属与当前
   身份不一致（执行归属不匹配），换身份后的实际表现未验收。
7. **本地验证流程**：`bun run build` → 重启 `bun scripts/local-preview.ts` → 首次打开在设置页填值
   （存储 URL 须 http(s)，如 `http://127.0.0.1:8090`；模型取值可参考本机 `.dev.vars`）。
8. **dev store 重置建议**：需要与旧身份彻底分离时，用新的 `--db-path` 重建本地 sqld 库，再在设置页
   重新填写配置。
