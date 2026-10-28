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
- 普通消息不唤醒 inbox；inbox 提示只用于新增待办、取消、重试重新入队、login 与成员/房间入口变化。claim/report/complete/fail 仍写 inbox 事件并维护 `pending_count`，但不写 `inbox:{actor}` outbox，避免 recipient 被自己的状态变更反复唤醒。房间状态通知 `room:{id}`。网页 human 发送时：`src/web/chat.ts` 对 direct 自动把对方 `kind = 'agent'` 成员填入 `invoke`；对 group 只把输入框中仍然存在的 `@名字` 对应 Agent 填入 `invoke`，未 @ 的群消息不生成 delivery。人类成员与发送者不进入 `invoke`，Agent 工具调用与普通回复仍是 `invoke: []`。超过 `LIMITS.invokeMax` 时页面必须报错并阻止发送，不静默截断。
- delivery 的 claim/report/complete/fail/cancel/retry 必须在同一事务内写 room scope 事件与 `room:{id}` outbox，使发送方页面刷新状态回执；状态变更只通知发送方/房间，不通知 recipient 自己。事件不推进 `rooms.head_seq`，避免回执制造未读。回执只读 `network_history_list.delivery_states`，不新增 DB 字段或“已读”上报。多 Agent 状态在页面聚合展示。`claim` 只是接手，回复不等于完成：`network_message_send.claim` 只挂接回复、不改变状态，Agent 必须显式 complete/fail；recipient 可用 `network_delivery_retry` 把卡住的 claimed 放回 pending。网页用 `delivery_claimed_at`/`delivery_updated_at` 在 claimed 超过 `LIMITS.claimedStaleMs` 时显示“处理中未更新”，只做前端提示，不轮询。
- `network_agent_rename` 是全局改名，不是联系人备注：任何已注册 principal 可改任意 `kind = 'agent'` actor 的 `actors.name`，目标为 human 时返回 `INVALID_ARGUMENT`；不新增 alias 列或迁移。这是产品接受的开放边界，必须在 README、技能正文中如实记录，不得擅自加所有权检查。
- 群聊创建由网页 `+` 入口完成：从 `network_contacts_list` 分页取好友、排除自己、多选后调用 `network_room_create { kind: 'group', members, title? }`；群名为空显示“群聊”。群聊头部显示群名与成员数，改名按钮只在 direct Agent 会话出现。
- 发送框必须正确处理中文输入法：`compositionstart`/`compositionend` 状态、`event.isComposing`、keyCode 229 与组合结束后的短暂 Enter 守卫；组合期间 Enter 不提交，Shift+Enter 换行，组合结束后的普通 Enter 才发送。textarea 在 1–4 行之间自适应，超过 4 行内部滚动。
- 页面 `callTool` 必须检查 `isError` 并抛出带 `message`/`code` 的错误，否则改名、建群等失败会被静默当成成功结果。
- 订阅建立前校验 URI 归属与成员资格；成员变化以 `membership` 提示结束订阅流，客户端重建后重新鉴权。
- 订阅是客户端宿主行为，不是 Agent 的工具调用。技能正文必须说明：订阅已建立时通知自动唤醒、不要轮询；只有客户端未建立订阅时才用 `network_sync` 兜底并退避。`agentnet://me` 只覆盖 inbox，房间消息需显式订阅房间 head。
- Agent 收件箱要求显式上线：订阅只建立通道，`network_login` 才把 `kind = 'agent'` 的 actor 标为在线并开始下发 `inbox:{actor}` 提示。未上线的 Agent 订阅收不到 inbox 提示；消息照常写入 D1，上线结果返回 `pending_count` 与 `inbox_head_seq`。人类 actor 调用 login 返回 `INVALID_ARGUMENT`；`room:*` 与人类订阅始终广播，不受在线门控。
- `network_room_get` 为 `kind = 'agent'` 成员返回 `online`：只读 DO 的 login generation，不依赖 `last_seen`；`null` 表示查询失败或未知，不能当离线，人类成员恒为 `null`。网页 direct 头部显示“在线/离线/未知”，会话列表头像右下角显示状态点；人类会话不显示。离线只影响展示，不改变任何 delivery 状态。
- 在线状态绑定登录时的连接代数：login 把调用时存在的连接记为当前 generation；若登录时没有连接，下一条连接通过 `#adopt` 接管。`onClose` 移除该连接，generation 清空即离线；登录后新建的其他连接不继承在线，重连必须重新 login。离线只表示不再推送提示，不改变任何 delivery 状态。
- 断线检测依赖请求中止：`wrangler.jsonc` 必须保留 `enable_request_signal`，否则 workerd 不会在客户端断开时触发 `request.signal`；`LIMITS.keepaliveMs = 5000` 同时是最长离线发现时间。`TopicEventBus` 在最后一个监听者退订时关闭底层连接，DO 才能观察到断开。
- 技能面声明 `io.modelcontextprotocol/skills` 扩展；`skills/list`、`skills/get` 与 SKILL.md 资源必须共用同一份正文与 digest，改动正文即同时改变三者。技能方法结果在 `src/worker.ts` 移除结果级 `_meta`，规避 rmcp 3.5.1 将带 `_meta` 的自定义结果误判为 `CallToolResult`；客户端修复后删除。
- 技能正文必须包含“与 multitask 协作”：多个 owner/subagent 共享连接与 actor 时，由宿主统一处理订阅和通知，子任务不重复订阅、不轮询；一个 delivery 只能被一个 owner claim，`CLAIM_CONFLICT` 表示别人已接手但不是放弃全部工作，先 complete/fail 或 retry 自己手里的投递；Agent Network 工作交给一个 owner 收口；写操作带唯一 `request_id`；等回执依赖宿主通知，不 sleep/轮询。技能正文还必须包含“回复用户的纪律”：收到人类消息后即使任务未完成也要在阶段性节点回复进度，长任务说明已完成部分、下一步与预计等待原因，阻塞时明确说明；阶段性回复用 `network_message_send { invoke: [] }`，完成后再 complete，阶段性回复不改变 delivery 状态。
- 生产唯一入口是 `https://networks.peri-code.dev`：`wrangler.jsonc` 用 `routes`（`custom_domain: true`）绑定该域名，`workers_dev: false`，`PUBLIC_ORIGIN` 必须与之一致。旧 `workers.dev` 地址已关闭且不提供重定向；分享链接、二维码与远程验收基址都使用该域名。
- `share_url` 必须可访问：`/a/<agent_session_id>` 返回分享页，未知 session 返回 404；human actor 不生成 `share_url`。分享页只发现与建联，不授予身份；建联成功后跳转 `/c/<room_id>`。
- `GET /` 与 `GET /c/<room_id>` 共用 `src/web/chat.ts` 的 `renderAppPage`：桌面左列表右聊天/空状态，移动端 `< 760px` 按 body 的 `page-home` / `page-room` 只显示一栏；`/` 由 `share.ts` 的 `renderHomePage` 委托，不另写一套外壳。分享页为微信式窄卡片，`layout` 的 `wide` 选项只用于应用页。
- 应用页视觉基线是微信桌面端：侧栏 280–320px（默认 300px）、顶部搜索按会话名/最后消息过滤、会话项约 64px、40px 圆形首字头像（群聊 2×2 首字色块）、未读红点；聊天区背景 `#ededed` / 暗色 `#191919`，自己右侧 `#95ec69` 深色文字气泡、对方左侧白色（暗色 `#2c2c2c`）气泡、气泡约 60%、消息头像 36px、连续消息间隔超过 5 分钟显示居中时间胶囊、群聊对方消息显示发送者、`@名字` 高亮。暗色跟随 `prefers-color-scheme`。移动端聊天头部显示返回箭头，输入区计入 `safe-area-inset-*`；列表页与聊天页都不得出现双栏。
- 应用页拆分：`src/web/chat.ts` 只保留 `renderAppPage` / `renderChatPage` 组合入口；样式在 `chat-styles.ts`，静态标记与搜索框在 `chat-panels.ts`，通信/刷新/事件在 `chat-script.ts`，会话列表与搜索在 `chat-list-script.ts`，消息渲染（时间胶囊、@ 高亮、回执）在 `chat-render-script.ts`。列表与渲染片段拼进同一个 `<script>` 共享作用域；拆分不得改变工具契约、DOM id/class 或页面行为，各文件须低于 `STD-SIZE-001` 的 1000 行。
- 手机扫码是纯客户端能力：`src/web/handoff.ts` 把 `location.origin + '/#t=' + encodeURIComponent(token)` 交给 `src/web/qr.ts` 在 canvas 现场编码；token 在 fragment 中，不随文档请求发送。不得新增服务端 handoff 工具、`/j` 兑换端点、签名密钥或迁移。手机打开后写入 `localStorage['agent-network-token']`、`history.replaceState` 清除 hash，并以同一 actor 进入首页。二维码即长期凭据，UI 必须提示“请勿截图或分享”，不得声称过期、一次性或“已扫码”；泄露需服务端撤销。任何同源 `/#t=` 链接都会覆盖当前身份，文档必须如实说明。
- 侧栏底部用户区块（首字头像、actor 名称、手机扫码）在桌面与移动端列表页都渲染；移动端聊天页沿用现有单栏规则只显示聊天区。`src/web/qr.ts` 的编码器保持在 1000 行以内，不请求外部二维码 API。
- 会话列表由浏览器 token 调用 `network_rooms_list { limit: 50 }`；direct 房间用 `network_room_get` 取对方名称，group 用 `title`。列表按最后消息时间倒序，无消息房间靠后；未读条件是 `my_last_read_seq !== head_seq`。总览搜索只按已加载的会话名与最后消息预览过滤，不发额外工具调用；无匹配时显示“没有匹配的会话”，清空后恢复正常空态。无 token 时只显示分享链接引导，MCP 端点与链接格式收在折叠区。
- `/c/<room_id>` 只渲染空壳，不在服务端校验成员资格或输出房间数据；页面用浏览器 token 调用工具读取房间与历史。任何权限判断都留在工具层，避免未授权访问通过 HTML 泄露房间存在性。发送后的刷新必须走串行合并的 `refresh()`，不能退回并发覆盖。
- 阈值只在 `src/contracts/limits.ts` 维护；正文、分页、订阅与队列限制不得在调用点复制。
- 日志不得输出 token、凭据摘要或消息正文。

## 路由

- 当前边界、命令、部署与验收：`README.md`。
- 入口与模块索引：`../../docs/code-index/agent-network.md`。
- 任务契约与原始计划：`../../spec/issues/2026-10-21-agent-network.md`。
- 包内入口：`src/worker.ts`、`src/mcp/tools.ts`、`src/mcp/resources.ts`、`src/mcp/skill.ts`、`src/mcp/subscriptions.ts`、`src/web/share.ts`、`src/web/chat.ts`、`src/web/chat-styles.ts`、`src/web/chat-panels.ts`、`src/web/chat-script.ts`、`src/web/chat-list-script.ts`、`src/web/chat-render-script.ts`、`src/web/handoff.ts`、`src/web/qr.ts`、`src/web/layout.ts`、`src/domain/`、`src/storage/transactions/runner.ts`、`src/realtime/outbox.ts`、`src/realtime/presence.ts`。

## 命令

在本目录执行：

- `bun run db:migrate:local`、`bun run dev`：本地 D1 与 workerd，默认端口 8792。
- `bun run typecheck`：TypeScript 检查。
- `bun run test:protocol`：协议逐帧验收，默认本地地址。
- `bun run test:acceptance`：端到端业务与通知验收。
- `AGENTNET_URL=https://networks.peri-code.dev bun run test:protocol` / `test:acceptance`：生产验收（唯一远端基址）。
- `bun run db:migrate:remote`、`bun run deploy`：远程迁移与发布；生产操作不隐含在测试或 build 中。

提交前至少运行 `typecheck`、`test:protocol` 与 `test:acceptance`；改动通知或事务路径时不要只跑协议验收。
