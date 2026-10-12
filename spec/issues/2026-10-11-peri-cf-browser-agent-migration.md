# 2026-10-11 peri-cf 浏览器宿主 Agent 迁移

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

## MCP 接入（2026-10-11 用户裁决，追加）

用户裁决：新增插件页维护多个 MCP 服务器；MCP 流量经 Worker relay 转发；**只接协议版本 2026-07-28，
且该限制仅作用于 peri-cf/浏览器部署**；不改审批逻辑（只开 `McpMiddleware`，工具调用不弹审批）。

1. **合同**
   - relay 新增 `mcp` 通道：`/api/relay/mcp/<encodeURIComponent(上游基址)>/<上游路径>`，JSON / SSE
     响应与 `mcp-session-id` 原样透传、不做 `base_url` 改写；基址校验与 400 语义同 storage/model。
   - 浏览器配置新增 `mcpServers: [{ name, url, headers }]`（同一 localStorage 单键）：名称即宿主内
     server 名（工具名 `mcp__<name>__<tool>` 前缀），`headers` 是每行一条 `Key: Value` 的请求头文本；
     条目一旦存在就必须合法（名称字符集与唯一性、URL 为 http(s)、请求头可解析），否则设置门禁不通过。
   - 宿主启动时把地址改写为同源 relay 前缀后随 `settings.config.mcpServers` 注入，
     `meta_harness = { ...BareHarnessConfig, McpMiddleware: true }`（`PermissionMiddleware` 保持关闭）。
   - 版本约束：peri-middlewares 的 HTTP 通道在 emscripten 目标只走 `server/discover` 且不回落
     （`serve_client_http` + `client/fetch_http.rs` 的宿主 fetch 实现）；TUI/本机与 stdio/ACP 路径
     仍走 `serve_client_auto`（Auto + legacy 回落）。
2. **改动面**：`worker/api/relay.ts`、`web/settings/config.ts`、`web/plugins/PluginsView.tsx`（新增）、
   `web/{App.tsx,styles.css}`、`web/host/config.ts`；`peri-middlewares/src/mcp/client/{fetch_http.rs（新增）,
   transport.rs}`、`initialize.rs`、`reconnect.rs`、`client.rs`、`Cargo.toml`（新增 sse-stream/bytes 与
   emscripten 目标的 wasm-bindgen 依赖）。
3. **状态**：实现完成；`peri-middlewares` 全量 lib 测试（1792 通过）、emscripten 目标编译与
   `bun run build:wasm`（provenance 已刷新）、peri-cf `typecheck` 与 `bun test tests web`（99 通过）
   均通过。**浏览器端到端未实测**：接入真实 MCP server 的链路验收需浏览器手工验证。
4. **风险与限制**：MCP 自定义请求头（可能含令牌）与模型/存储凭据同样存 localStorage 并经 relay 明文过网；
   插件页可把任意 `http(s)` 目标配成 MCP 上游，relay 不做 host 白名单（与 storage/model 同姿态）；
   浏览器部署不启用 `PermissionMiddleware` ⇒ MCP 工具调用无审批拦截，工具面授权由 MCP server 自身与
   部署入口控制承担；鉴权只支持请求头方式（不提供 MCP OAuth 授权流——回调服务器是本地 TCP 端口，
   属原生路径）。

## 浏览器 VFS 持久化（2026-10-11，追加）

用户裁决（2026-10-11）：引入浏览器虚拟文件系统，**全部配置装进一个文件**，文件持久化在 IndexedDB。
本节为追加记录，不改写上文历史条目（上文各节记述的「localStorage 单键」自此被取代）。

1. **决策与合同**
   - 库选型：`@isomorphic-git/lightning-fs@4.10.3`（**MIT**）。peri 自身是 Apache-2.0，
     `@zenfs/*` 全系为 LGPL-3.0-or-later，故排除。数据落在 IndexedDB 库 `peri-vfs` 的
     `peri-vfs_files` 商店：键 `!root` 是目录树，其余键（inode）是文件内容。
   - `web/vfs/index.ts` 是薄门面，只暴露 `readText` / `writeText` / `deleteFile` / `list`，
     路径必须是绝对路径；不把底层 Node 风格 API 外溢。写入与删除后显式 `flush()`：底层默认靠
     500ms 去抖或空闲提交，不 flush 时「写完立刻关页面」会丢目录树（实测）。
   - 配置文件 = `/config.json`，一个文件装全部（machineId + model + storage + mcpServers）；
     `web/settings/config.ts` 的读写改为异步，localStorage 键 `peri-cf.browser-config.v1` 删除。
   - **VFS 仅服务 JS 侧**：wasm 的 Emscripten FS 保持现状，不挂 IDBFS、不做持久化；Rust 侧不读这个
     文件，二者不打通。旧 localStorage 数据不迁移，VFS 从空开始。
   - 配置改为**页面启动时读一次并显式下传**：`App` → `useChat(config)` → `createApplication({ config })`
     → 宿主启动（`BrowserHostOptions.config`，宿主不再自行读盘）与 `TursoStorage`。同一库名的第二个
     VFS 实例要等前一个空闲 500ms 释放跨标签页锁（实测），实例因此在 `web/main.tsx` 建一次。
2. **改动面**：`web/vfs/{index.ts（新增）, vfs.test.ts（新增）}`、`web/settings/config.{ts,test.ts}`、
   `web/{App.tsx,main.tsx}`、`web/settings/SettingsView.tsx`、`web/plugins/PluginsView.tsx`、
   `web/host/index.ts`、`web/runtime/{application,repository}.ts`、`web/chat/useChat.ts`、
   `package.json`（新增 `@isomorphic-git/lightning-fs`，dev 增 `fake-indexeddb`）。
3. **状态**：实现完成；`typecheck`、`bun test tests web`（110 通过）、`bun run build` 均通过。
   **真实浏览器端到端已实测**（独立临时 profile 的 headless Chrome + 运行中的 8791 预览 + 新构建产物，
   经 CDP 驱动）：空 VFS 首启动生成 machineId 并落盘（IDB 库 `peri-vfs` / 商店 `peri-vfs_files` /
   键 `[1,"!root"]`）→ 刷新后 machineId 不变 → 填完整配置保存后刷新回显逐字段一致 → 插件页新增 MCP
   条目保存后刷新回显一致。
4. **影响与限制**：清除浏览器数据（IndexedDB）或换浏览器即从零开始，旧身份与旧配置一起失效；
   配置读写不再同步，首屏多一个异步门；IndexedDB 不可用时页面停在明确的读取失败提示，不按「未配置」
   放行。凭据位置由 localStorage 变为 IndexedDB，安全姿态不变（仍在浏览器，经 relay 明文过网）。
