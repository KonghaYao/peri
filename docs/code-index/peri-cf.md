# Peri CF 代码索引

状态：独立的前后端分离示例，**Bun 长驻服务是唯一计算实例**。服务端在进程内启动 peri-wasm ACP Host，持有会话运行期、投影、命令、交互与 Turso/libSQL 仓储；浏览器只做展示、命令转发与交互回答，经 HTTP/SSE 消费服务端。凭据与远程 MCP 由服务端环境变量管理。生产部署、真实模型/Store 与平台资源仍需环境验收；不属于根 Cargo workspace。使用说明与命令由 [包 README](../../npm-packages/peri-cf/README.md) 维护，架构基线见 [服务端 Agent 设计](../design/peri-cf-server-agent.md)。

## 边界与数据流

- **共享内核**：`npm-packages/peri-cf/core/agent/`（运行期端口与错误、投影、交互、ACP 初始化）与 `core/shared/`（Zod 契约、`SyncState`、构建常量）平台无关，不依赖 React/DOM/localStorage。
- **服务端**：`server/index.ts` 是 Bun 入口；`server/agent/manager.ts` 是组合根，装配 `TursoChatSessionStore`、WASM 宿主启动器与每实例环境工厂，并向下传给 `ChatService`；`server/agent/service.ts` 持有 `PeriInstance` 映射与驻留 LRU；`server/agent/peri-instance.ts` 是服务端实现；`server/store.ts` 只读 Store；`server/api/` 提供会话、命令与 SSE。
- **前端**：`frontend/remote/` 提供 HTTP 仓储、`RemoteChatRuntime`（SSE + POST 命令）与 `createRemoteAgent()`；`frontend/chat/` 是 assistant-ui 适配与展示；`frontend/app/` 是 SPA 外壳。没有本地宿主、localStorage 凭据或首次运行门禁。
- **命令**：HTTP `POST /api/chats/:id/commands`，契约由 `core/shared/chat.ts` 的 `chatCommandSchema` 校验；失败以稳定码与 `source:"server"` 返回。`ChatConnection` 仍是视图层的窄命令通道。
- **投影**：SSE `GET /api/chats/:id/events` 首帧与每次变化发送完整 `SyncState`，50 ms 合并，15 s 心跳；重连后重发完整快照，客户端无需增量状态。
- **复原**：`PeriInstance.restore` 先订阅、再 `session/load`，重放通知进入 SDK 投影；`initialize` 声明 `peri.historyPagination`、`peri.replay`、`peri.unstableEvent` 与 `peri.userInputQueue`。
- **驻留**：`ChatService` 按 ACP Session ID 惰性驻留 `PeriInstance`，空闲 LRU 上限 8；执行中的实例不回收。客户端断开不终止执行。
- **凭据**：`server/config.ts` 从 `PERI_MODEL_*`、`PERI_STORAGE_*`、`PERI_MACHINE_ID`、`PERI_MCP_SERVERS` 读取；任何解析问题都拒绝启动。`/api/info` 只返回去敏摘要。
- **工具面**：`server/agent/manager.ts` 的 `SERVER_HARNESS_CONFIG` 开启 Todo/HITL/SubAgent/Mcp/ToolSearch/Goal；不启动 local-fs、模拟 Bash、stdio 或进程内 builtin；文件/终端等能力只能由远程 MCP 提供。每次执行/复原启动独立 WASM 实例，关闭后由实例释放。
- **静态资源**：Bun 入口对非 `/api/*` 路径提供 `dist/client` 与 `index.html` 回退；Vite 开发服务器把 `/api` 代理到 `127.0.0.1:8787`。
- **模块边界**：文件夹是模块，目录根 `index.ts` 是唯一公共出口；`core/agent/boundary.test.ts` 扫描生产代码，并强制 `frontend/` 与 `server/` 互不引用。
- **没有访问控制**：服务不提供认证、限流或多租户隔离；部署入口鉴权是必要条件。

## 入口

| 职责 | 入口 |
| --- | --- |
| 包命令、依赖 | `npm-packages/peri-cf/package.json` |
| Bun 服务入口（API + 静态资源 + 信号处理） | `npm-packages/peri-cf/server/index.ts` |
| 环境变量解析与启动校验 | `npm-packages/peri-cf/server/config.ts` |
| 服务端组合根（Store、宿主启动器、环境工厂的装配与委托） | `npm-packages/peri-cf/server/agent/manager.ts` |
| 会话服务（`PeriInstance` 驻留、LRU、创建、命令、订阅） | `npm-packages/peri-cf/server/agent/service.ts` |
| 新会话系统指令 | `npm-packages/peri-cf/server/system-instructions.ts` |
| Store 列表/读取/缺表容忍 | `npm-packages/peri-cf/server/store.ts` |
| HTTP 路由、命令校验、SSE | `npm-packages/peri-cf/server/api/index.ts` |
| 服务端 `PeriInstance` 实现 | `npm-packages/peri-cf/server/agent/peri-instance.ts` |
| 共享运行期端口与错误 | `npm-packages/peri-cf/core/agent/session/contracts.ts` |
| 共享 wire 契约 | `npm-packages/peri-cf/core/shared/chat.ts`、`sync.ts`、`sync-state.ts` |
| 前端远程客户端 | `npm-packages/peri-cf/frontend/remote/runtime.ts`、`frontend/remote/repository.ts`、`frontend/remote/application.ts` |
| 前端会话装配与查询缓存 | `npm-packages/peri-cf/frontend/chat/runtime/useChat.ts`、`queries.ts`、`adapter.ts` |
| SPA 外壳与只读服务端信息 | `npm-packages/peri-cf/frontend/app/App.tsx`、`frontend/settings/ServerInfoDialog.tsx` |
| Vite 构建与 `/api` 代理 | `npm-packages/peri-cf/vite.config.ts` |
| 开发启动（Bun watch + Vite） | `npm-packages/peri-cf/scripts/dev.ts` |
| WASM 构建与 provenance | `npm-packages/peri-cf/scripts/{build-wasm,prepare-wasm,wasm-provenance,wasm-profile}.ts` |

## 验证入口

- `bun run typecheck`：TypeScript 全量检查。
- `bun run test`：`core`、`frontend`、`server`、`scripts` 下的 Bun 测试；服务端配置、API 与 SSE 首帧在 `server/*.test.ts`。
- `bun run build`：`prepare:wasm` + Vite 构建到 `dist/client`；`bun run start` 启动服务。
- 服务端冒烟：`GET /api/info`、`GET /api/chats`、`GET /api/chats/:id/events`；真实执行需可用的模型端点与 Store。
