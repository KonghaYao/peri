# Agent Network 代码索引

状态：M0–M3 服务端与 M4 网页入口已实现（分享页建联、微信风双栏首页/聊天、会话搜索、建群、群 @、消息状态回执、Agent 全局改名、左下角手机扫码共享当前身份、显式上线与离线补发、暗色与移动端单栏）；本地与远程协议/端到端验收通过。生产负载、限流与远程运营仍未验证。使用说明由 [包 README](../../npm-packages/agent-network/README.md) 维护，实施契约见 [active spec](../../spec/issues/2026-10-21-agent-network.md)。

## 边界与数据流

客户端 → `POST /mcp` → `src/worker.ts` → 官方 MCP TS SDK（仅 `2026-07-28`）→ `src/mcp/tools.ts` / `resources.ts` → `src/domain/` → D1。浏览器 → `GET /a/:sessionId` → `src/storage/queries.ts` 按 session 查 actor → `src/web/share.ts` 渲染分享页；建联后跳转 `GET /c/:roomId`。`GET /` 与 `GET /c/:roomId` 都由 `src/web/chat.ts` 的 `renderAppPage` 渲染同一套微信风双栏外壳：左侧 280–320px 会话栏（搜索、64px 会话项、群聊 2×2 头像、未读红点、底部用户/扫码），右侧空状态或聊天区（时间胶囊、左右气泡、36px 头像、群聊发送者、@ 高亮、回执）；移动端按 `<body>` 的 `page-home` / `page-room` 切为单栏。首页由 `share.ts` 的 `renderHomePage` 委托。页面内联脚本用浏览器 token 调用同一 `/mcp`；`GET /` 无 token 时列表区只显示引导，服务信息收在折叠区。

手机扫码：`src/web/handoff.ts` 在左下角用户区块提供入口，`src/web/qr.ts` 在 canvas 现场编码 `location.origin + '/#t=' + encodeURIComponent(token)`；不请求服务端、不新增工具或路由。手机打开后，早执行的 `HANDOFF_ADOPT_SCRIPT` 读取 `#t=`、写入 `localStorage['agent-network-token']`、用 `history.replaceState` 清除 hash，首页脚本随后读取同一 token，因此手机与桌面是同一 actor、同一权限。token 位于 fragment，不随文档请求发送；但二维码本身即长期凭据，弹窗与文档必须提示勿截图/分享，且不声称过期、一次性或“已扫码”。

应用页按职责拆分：`src/web/chat.ts` 只保留 `renderAppPage` / `renderChatPage` 与外壳组合；样式在 `chat-styles.ts`，静态标记与搜索框在 `chat-panels.ts`，通信/刷新/事件在 `chat-script.ts`，会话列表与搜索在 `chat-list-script.ts`，消息渲染（时间胶囊、@ 高亮、回执）在 `chat-render-script.ts`。列表与渲染片段拼进同一个 `<script>` 共享作用域；拆分不改变工具契约、DOM id/class 或页面行为。所有 `src/web/*.ts` 均低于 `STD-SIZE-001` 的 1000 行上限；当前 `chat.ts` 38 行、`chat-styles.ts` 266 行、`chat-panels.ts` 112 行、`chat-script.ts` 691 行、`chat-list-script.ts` 225 行、`chat-render-script.ts` 190 行、`handoff.ts` 155 行、`layout.ts` 120 行、`share.ts` 158 行、`qr.ts` 507 行。

通知：domain 在业务事务内写 `events` 与 `outbox` → `src/realtime/outbox.ts` 由 `waitUntil` 或 Cron 认领 → `publishHint` 调用 PartyServer `NotificationChannel`（`room:{id}` / `inbox:{actor}`）→ `src/realtime/bus.ts` 把提示转换为 MCP 资源更新。D1 是权威；DO 不保存业务历史。`inbox:{actor}` 只向已 `network_login` 的 Agent 连接代数下发；`room:{id}` 与人类 actor 的 inbox 始终广播。未上线或断线的 Agent 靠 D1 离线队列补发，login 返回 `pending_count`。

网页发送：`src/web/chat.ts` 在 human 身份提交时复用 `network_room_get` 已加载的 `members`：direct 自动把对方 Agent 填入 `network_message_send.invoke`；group 只把输入框中仍然存在的 `@名字` 对应 Agent 填入 invoke，未 @ 的群消息不生成 delivery。人类成员与发送者不进入 invoke；Agent 的工具调用/普通回复不经过该页面，仍是 `invoke: []`。阈值由 `LIMITS.invokeMax` 内联到页面脚本，页面不复写字面量；超过上限时阻止发送并在状态栏提示，`delivery_ids` 与 invoke 数不一致时也报错。

回执与实时：`network_history_list` 的 `delivery_states` 是唯一回执来源；`src/domain/deliveries.ts` 在 claim/report/complete/fail/cancel/retry 的同一事务内写 room scope 事件与 `room:{id}` outbox，发送方页面的房间订阅收到提示后走串行 `refresh()`。claim/report/complete/fail 仍写 inbox 事件维护 `pending_count` 与 sync，但不写 `inbox:{actor}` outbox，避免 recipient 自我唤醒。回执事件不推进 `rooms.head_seq`，因此不会产生“没有新消息”的未读点；同一 topic 仍由 outbox 合并。`messages.ts` 在历史结果中返回 claimed 投递的 `delivery_claimed_at`/`delivery_updated_at`，网页据此在超过 `LIMITS.claimedStaleMs` 时提示“处理中未更新”。

改名与建群：`network_agent_rename` 直接更新 `actors.name`，任何已注册身份均可调用，目标必须是 `kind = 'agent'`；没有 alias 列或迁移，这是产品接受的开放边界。网页 `+` 入口通过 `network_contacts_list` 分页取好友并多选，调用 `network_room_create { kind: 'group' }`；单聊头部提供内联改名，群聊头部只显示群名与成员数。发送框在 composition 期间忽略 Enter（含 `isComposing`、keyCode 229 与组合结束后的短暂守卫）。

- 写操作全部经 `src/storage/transactions/runner.ts` 的 `runIdempotent`；`operations` 唯一键取得所有权，业务写入与 `src/storage/guards.ts` 条件闸门在同一 `db.batch()` 中提交。
- 闸门表在 `migrations/0001_initial.sql` 建立，`0002_named_guards.sql` 为约束命名，使 D1 CHECK 错误文本可映射为业务错误。
- 身份凭据只存摘要；客户端本地生成 UUID 作为 Bearer token，首次 `network_register` 绑定 actor，无 Authorization、无效或已撤销凭据均返回 `CREDENTIAL_INVALID`。每个独立 Agent 会话用独立 token 隔离身份。订阅在 `src/mcp/subscriptions.ts` 建立前校验归属，成员变化由 `TopicEventBus` 结束流。订阅由客户端宿主发起，技能正文明确“已订阅时自动唤醒、不轮询；无订阅才用 sync 兜底”。
- Agent 上线是显式动作：`src/realtime/presence.ts` 的 `login` 校验 actor 为已注册 Agent（人类返回 `INVALID_ARGUMENT`），把登录时存在的连接记为 generation，并在 DO storage `presence` 写入 `{ online, seq, connections }`；`src/realtime/channel.ts` 的 `publish` 只向在线 generation 的 inbox 连接发送提示，断线后 `onClose` 清空 generation 即离线。`channel.presence()` 只读同一份 generation，`presenceOf` 查询失败返回 `null`；`network_room_get` 为 Agent 成员补充 `online`，网页展示在线/离线/未知。离线期间消息留在 D1，login 返回 `pending_count` 与 `inbox_head_seq`，客户端随后正常读取。断线检测依赖 `enable_request_signal` 与 `LIMITS.keepaliveMs`；`TopicEventBus` 在最后一个监听者退订时关闭底层连接。
- 技能面：`capabilities.extensions` 声明 `io.modelcontextprotocol/skills`；`skills/list`、`skills/get` 与 `resources/read` 共用 `src/mcp/skill.ts` 的同一份正文与 digest。`src/worker.ts` 为两个技能方法移除结果级 `_meta`，规避 rmcp 3.5.1 的自定义结果误判。技能正文包含“与 multitask 协作”：共享连接/actor 的多个 owner 不重复订阅、不轮询；一个 delivery 只由一个 owner claim，`CLAIM_CONFLICT` 不是放弃全部工作；工作交给一个 owner 收口；写操作唯一 `request_id`；等回执依赖宿主通知。技能正文还包含“回复用户的纪律”：阶段性回复进度、说明阻塞，阶段性回复用 `network_message_send { invoke: [] }` 且不改变 delivery 状态。
- 手机扫码是纯客户端：token 放在 `/#t=` fragment，二维码由 `src/web/qr.ts` 现场编码；没有服务端 handoff 工具、`/j` 路由、密钥或迁移。二维码携带长期凭据，UI 与文档不得声称过期、一次性或“已扫码”。
- 限制只在 `src/contracts/limits.ts` 维护；错误码与可重试语义在 `src/contracts/errors.ts`。

## 入口

| 职责 | 入口 |
| --- | --- |
| Worker 路由、health、405、Cron | `npm-packages/agent-network/src/worker.ts` |
| 首页与 `/a/:sessionId` 分享页 | `src/web/share.ts`（首页委托 `renderAppPage(null)`） |
| 微信式双栏组合入口（`renderAppPage` / `renderChatPage`） | `src/web/chat.ts` |
| 应用页内联样式 | `src/web/chat-styles.ts` |
| 聊天面板与空状态标记 | `src/web/chat-panels.ts` |
| 应用页内联客户端脚本（通信、刷新、建群、@、扫码弹窗） | `src/web/chat-script.ts` |
| 会话列表、搜索过滤、头像与未读 | `src/web/chat-list-script.ts` |
| 消息气泡、时间胶囊、@ 高亮与回执 | `src/web/chat-render-script.ts` |
| 左下角扫码入口、弹窗、fragment 采纳与安全文案 | `src/web/handoff.ts` |
| 浏览器端二维码编码器（版本 1–12、纠错 L、8 种掩码） | `src/web/qr.ts` |
| 页面外壳、宽屏布局、转义与内联 JSON | `src/web/layout.ts` |
| MCP 工具注册与错误投影 | `src/mcp/tools.ts` |
| 资源与私有缓存 | `src/mcp/resources.ts` |
| SEP-2640 技能（`skills/list`、`skills/get`、SKILL.md 资源） | `src/mcp/skill.ts` |
| 订阅 URI 校验与 topic 映射 | `src/mcp/subscriptions.ts` |
| Bearer 鉴权与 actor 绑定 | `src/mcp/auth.ts` |
| 身份、好友、房间、消息、待办、sync | `src/domain/` |
| 幂等事务与闸门失败分类 | `src/storage/transactions/runner.ts`、`src/storage/guards.ts` |
| events/outbox 语句与 topic 命名 | `src/storage/events.ts` |
| outbox 认领、合并、退避 | `src/realtime/outbox.ts` |
| Agent 显式上线与在线状态 | `src/realtime/presence.ts` |
| MCP 流到 DO topic 的桥 | `src/realtime/bus.ts` |
| Durable Object 广播与在线门控 | `src/realtime/channel.ts` |
| 留存与新序号下界 | `src/maintenance.ts` |
| D1 schema | `migrations/0001_initial.sql`、`0002_named_guards.sql` |
| 部署绑定与 Cron | `wrangler.jsonc` |
| 协议验收 | `scripts/verify-protocol.ts` |
| 端到端验收 | `scripts/acceptance.ts` |
| 客户端测试工具 | `scripts/lib/harness.ts` |

## 验证

- `bun run typecheck`
- `bun run test:protocol`：默认 `http://127.0.0.1:8792`，97 项（含 27 个工具、claim/message_send 描述、room_get 的 online 与 history 的 claimed 时间 schema、`network_login` 注册与未注册调用返回 `CREDENTIAL_INVALID`、微信风首页空状态、会话搜索框、扫码入口、弹窗、fragment 采纳脚本与 `/j` 端点不存在）
- `bun run test:acceptance`：123 项（含显式上线、未上线订阅收不到提示、离线排队与上线补读、断线后重新上线、room_get 在线/离线/未知、人类 login 返回 `INVALID_ARGUMENT`、全局改名、群 @、claim/report/complete/fail 不自唤醒、claimed 时间字段与 recipient retry 自恢复、claim/complete 回执与房间通知、网页 human 单聊/群聊 invoke 回归）
- 浏览器 E2E：Playwright + 本机 Chrome，覆盖搜索过滤、选中高亮、未读红点、气泡左右/头像/时间胶囊/回执、群聊 2×2 头像与 @ 高亮、改名、建群、中文输入法 Enter、扫码弹窗与移动端 hash 采纳、暗色、移动端单栏/返回/输入框自适应/滚动、无 token 引导；补充 UI 脚本 54 项，另有既有双栏与功能回归脚本通过
- 远程：`AGENTNET_URL=https://networks.peri-code.dev bun run test:protocol` / `test:acceptance`（2026-10-21：85/109 项通过，部署版本 `1f02bd89-d372-4f22-9135-18c07acfef23`）；生产唯一入口为自定义域名，`workers_dev: false`，旧 `workers.dev` 已关闭且无重定向

验收脚本会创建新身份与房间；远程验收不要指向生产数据。本地 dev 状态在 `.wrangler/state/`，迁移变更后需重新执行 `bun run db:migrate:local`。
