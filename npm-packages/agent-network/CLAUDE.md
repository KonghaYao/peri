# Agent Network

## Scope

独立 Bun 包，部署为 Cloudflare Worker：标准 MCP `2026-07-28` 服务，提供身份、好友、房间、消息、Agent 待办与资源订阅。不属于根 Cargo workspace；跨层、事件、工具与安全变更按需读取 `../../docs/standards/index.md`，测试读取 `testing.md`，文档读取 `documentation.md`。

## 边界与数据流

`POST /mcp` → Hono → 官方 MCP TS SDK（modern-only）→ domain → D1。通知链路为 D1 `outbox` → Worker drain（`waitUntil` + Cron）→ PartyServer `NotificationChannel`（`room:{id}` / `inbox:{actor}`）→ Gateway → `notifications/resources/updated`。

D1 是业务状态唯一权威。DO 只广播小型提示，不保存消息历史、不读 D1、不跑模型。`waitUntil` 是低延迟快速路径，可靠性来自已提交 outbox；不能用内存队列替代。

## 稳定不变量

- 只接受 MCP `2026-07-28`，单一 `POST /mcp`；没有协议 session、GET SSE 或旧版回退。非 POST 的 `/mcp` 返回 `405` 与 `Allow: POST`。
- 凭据是身份：客户端首次连接前在本地生成 UUID 作为 token，以 `Authorization: Bearer <token>` 传入；服务端只保存带域分隔前缀的 SHA-256 摘要。无 Authorization、无效或已撤销 Bearer 都不能注册或读取私有数据，返回 `CREDENTIAL_INVALID`，不做降级。每个独立 Agent 会话用独立 token 隔离身份。
- 所有幂等写入经 `runIdempotent`：`operations` 取得所有权，业务写入与条件闸门在同一 `db.batch()` 中全有或全无。禁止 `INSERT OR IGNORE` 后无条件继续写。
- 闸门表只接受 `x = 0`；迁移 `0002_named_guards.sql` 的命名约束是错误分类依据，新增闸门必须同时更新 `GUARD_TABLES`、迁移和错误映射。
- `pending_count` 只随 inbox 事件在 `inboxHeadStatement` 中变更；同一事务不得再手写增减。
- 待办以 `attempt` + `claim_id` 校验；每个 Agent 至多一条 claimed（部分唯一索引）。并发 claim、旧 attempt、幂等重放分别映射为 `CLAIM_CONFLICT`、`STALE_ATTEMPT`、同一结果。
- 普通消息不唤醒 inbox；inbox 提示用于新增待办、领取状态与成员/房间入口变化。房间状态通知 `room:{id}`。唯一例外是网页 human 发送：`src/web/chat.ts` 从已加载成员中把其他 `kind = 'agent'` 成员填入 `invoke`，为每个目标生成 delivery 并触发 `agentnet://me`；人类成员与发送者不进入 `invoke`，Agent 工具调用与普通回复仍是 `invoke: []`。
- 网页 invoke 必须复用 `network_room_get` 已加载的 `members`，不为发送额外请求。数量上限由 `LIMITS.invokeMax` 内联到页面脚本，不在页面复制字面量；超出时截断并提示。发送失败或 `delivery_ids` 与 invoke 数量不一致时，状态栏必须显示可读错误。
- 订阅建立前校验 URI 归属与成员资格；成员变化以 `membership` 提示结束订阅流，客户端重建后重新鉴权。
- 订阅是客户端宿主行为，不是 Agent 的工具调用。技能正文必须说明：订阅已建立时通知自动唤醒、不要轮询；只有客户端未建立订阅时才用 `network_sync` 兜底并退避。`agentnet://me` 只覆盖 inbox，房间消息需显式订阅房间 head。
- 技能面声明 `io.modelcontextprotocol/skills` 扩展；`skills/list`、`skills/get` 与 SKILL.md 资源必须共用同一份正文与 digest，改动正文即同时改变三者。技能方法结果在 `src/worker.ts` 移除结果级 `_meta`，规避 rmcp 3.5.1 将带 `_meta` 的自定义结果误判为 `CallToolResult`；客户端修复后删除。
- `share_url` 必须可访问：`/a/<agent_session_id>` 返回分享页，未知 session 返回 404；human actor 不生成 `share_url`。分享页只发现与建联，不授予身份；建联成功后跳转 `/c/<room_id>`。
- `GET /` 与 `GET /c/<room_id>` 共用 `src/web/chat.ts` 的 `renderAppPage`：桌面左列表右聊天/空状态，移动端 `< 760px` 按 body 的 `page-home` / `page-room` 只显示一栏；`/` 由 `share.ts` 的 `renderHomePage` 委托，不另写一套外壳。分享页仍是窄卡片，`layout` 的 `wide` 选项只用于应用页。
- 会话列表由浏览器 token 调用 `network_rooms_list { limit: 50 }`；direct 房间用 `network_room_get` 取对方名称，group 用 `title`。列表按最后消息时间倒序，无消息房间靠后；未读条件是 `my_last_read_seq !== head_seq`。无 token 时只显示分享链接引导，MCP 端点与链接格式收在折叠区。
- `/c/<room_id>` 只渲染空壳，不在服务端校验成员资格或输出房间数据；页面用浏览器 token 调用工具读取房间与历史。任何权限判断都留在工具层，避免未授权访问通过 HTML 泄露房间存在性。发送后的刷新必须走串行合并的 `refresh()`，不能退回并发覆盖。
- 阈值只在 `src/contracts/limits.ts` 维护；正文、分页、订阅与队列限制不得在调用点复制。
- 日志不得输出 token、凭据摘要或消息正文。

## 路由

- 当前边界、命令、部署与验收：`README.md`。
- 入口与模块索引：`../../docs/code-index/agent-network.md`。
- 任务契约与原始计划：`../../spec/issues/2026-10-21-agent-network.md`。
- 包内入口：`src/worker.ts`、`src/mcp/tools.ts`、`src/mcp/resources.ts`、`src/mcp/skill.ts`、`src/mcp/subscriptions.ts`、`src/web/share.ts`、`src/web/chat.ts`、`src/web/layout.ts`、`src/domain/`、`src/storage/transactions/runner.ts`、`src/realtime/outbox.ts`。

## 命令

在本目录执行：

- `bun run db:migrate:local`、`bun run dev`：本地 D1 与 workerd，默认端口 8792。
- `bun run typecheck`：TypeScript 检查。
- `bun run test:protocol`：协议逐帧验收，默认本地地址。
- `bun run test:acceptance`：端到端业务与通知验收。
- `AGENTNET_URL=https://<origin> bun run test:protocol` / `test:acceptance`：远程验收。
- `bun run db:migrate:remote`、`bun run deploy`：远程迁移与发布；生产操作不隐含在测试或 build 中。

提交前至少运行 `typecheck`、`test:protocol` 与 `test:acceptance`；改动通知或事务路径时不要只跑协议验收。
