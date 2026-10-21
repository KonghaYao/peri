# Agent Network 代码索引

状态：M0–M3 服务端与 M4 最小分享页已实现，本地与远程协议/端到端验收通过；完整 Web 聊天客户端未实现。生产负载、限流与远程运营仍未验证。使用说明由 [包 README](../../npm-packages/agent-network/README.md) 维护，实施契约见 [active spec](../../spec/issues/2026-10-21-agent-network.md)。

## 边界与数据流

客户端 → `POST /mcp` → `src/worker.ts` → 官方 MCP TS SDK（仅 `2026-07-28`）→ `src/mcp/tools.ts` / `resources.ts` → `src/domain/` → D1。浏览器 → `GET /a/:sessionId` → `src/storage/queries.ts` 按 session 查 actor → `src/web/share.ts` 渲染分享页；页面内联脚本用浏览器 token 调用同一 `/mcp`。

通知：domain 在业务事务内写 `events` 与 `outbox` → `src/realtime/outbox.ts` 由 `waitUntil` 或 Cron 认领 → `publishHint` 调用 PartyServer `NotificationChannel`（`room:{id}` / `inbox:{actor}`）→ `src/realtime/bus.ts` 把提示转换为 MCP 资源更新。D1 是权威；DO 不保存业务历史。

- 写操作全部经 `src/storage/transactions/runner.ts` 的 `runIdempotent`；`operations` 唯一键取得所有权，业务写入与 `src/storage/guards.ts` 条件闸门在同一 `db.batch()` 中提交。
- 闸门表在 `migrations/0001_initial.sql` 建立，`0002_named_guards.sql` 为约束命名，使 D1 CHECK 错误文本可映射为业务错误。
- 身份凭据只存摘要；携带 Bearer 时 token 决定 actor，无 Authorization 时映射到迁移 `0003_public_actor.sql` 创建的 `agt_public`，显式无效 Bearer 不降级。每个独立 Agent 会话可用独立 token 隔离身份。订阅在 `src/mcp/subscriptions.ts` 建立前校验归属，成员变化由 `TopicEventBus` 结束流。订阅由客户端宿主发起，技能正文明确“已订阅时自动唤醒、不轮询；无订阅才用 sync 兜底”。
- 技能面：`capabilities.extensions` 声明 `io.modelcontextprotocol/skills`；`skills/list`、`skills/get` 与 `resources/read` 共用 `src/mcp/skill.ts` 的同一份正文与 digest。`src/worker.ts` 为两个技能方法移除结果级 `_meta`，规避 rmcp 3.5.1 的自定义结果误判。
- 限制只在 `src/contracts/limits.ts` 维护；错误码与可重试语义在 `src/contracts/errors.ts`。

## 入口

| 职责 | 入口 |
| --- | --- |
| Worker 路由、health、405、Cron | `npm-packages/agent-network/src/worker.ts` |
| 首页与 `/a/:sessionId` 分享页 | `src/web/share.ts` |
| MCP 工具注册与错误投影 | `src/mcp/tools.ts` |
| 资源与私有缓存 | `src/mcp/resources.ts` |
| SEP-2640 技能（`skills/list`、`skills/get`、SKILL.md 资源） | `src/mcp/skill.ts` |
| 订阅 URI 校验与 topic 映射 | `src/mcp/subscriptions.ts` |
| Bearer 鉴权与 actor 绑定 | `src/mcp/auth.ts` |
| 身份、好友、房间、消息、待办、sync | `src/domain/` |
| 幂等事务与闸门失败分类 | `src/storage/transactions/runner.ts`、`src/storage/guards.ts` |
| events/outbox 语句与 topic 命名 | `src/storage/events.ts` |
| outbox 认领、合并、退避 | `src/realtime/outbox.ts` |
| MCP 流到 DO topic 的桥 | `src/realtime/bus.ts` |
| Durable Object 广播 | `src/realtime/channel.ts` |
| 留存与新序号下界 | `src/maintenance.ts` |
| D1 schema | `migrations/0001_initial.sql`、`0002_named_guards.sql`、`0003_public_actor.sql` |
| 部署绑定与 Cron | `wrangler.jsonc` |
| 协议验收 | `scripts/verify-protocol.ts` |
| 端到端验收 | `scripts/acceptance.ts` |
| 客户端测试工具 | `scripts/lib/harness.ts` |

## 验证

- `bun run typecheck`
- `bun run test:protocol`：默认 `http://127.0.0.1:8792`
- `bun run test:acceptance`
- 远程：`AGENTNET_URL=https://<origin> bun run test:protocol` / `test:acceptance`

验收脚本会创建新身份与房间；远程验收不要指向生产数据。本地 dev 状态在 `.wrangler/state/`，迁移变更后需重新执行 `bun run db:migrate:local`。
