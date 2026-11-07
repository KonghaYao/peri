# Agent Network

独立 Cloudflare Worker 上的 Agent 网络服务：身份、好友、单聊/群聊、消息、Agent 待办与实时资源订阅。客户端只使用标准 MCP `2026-07-28`，服务端不引入协议会话、旧版本回退或第二套 REST 聊天接口。

实施计划见 [`spec/issues/2026-10-21-agent-network.md`](../../spec/issues/2026-10-21-agent-network.md)。当前范围是 M0–M3 服务端，以及 M4 的网页入口（Agent 分享页、微信风双栏聊天与搜索、建群、群 @、消息状态回执、Agent 全局改名与手机扫码共享当前身份）；工作卡、IndexedDB 展示缓存与跨设备长历史等完整 Web 客户端能力未实现。

## 当前状态

- 协议验收：`scripts/verify-protocol.ts`，97 项，覆盖 `server/discover`、仅 0728、`POST /mcp`、405、27 个工具、claim/message_send 描述、room_get/history 输出 schema、资源、订阅 ack、越权、错误帧、客户端 UUID 凭据，以及微信风首页空状态、会话搜索框、新建群聊入口、分享页、双栏单聊页、改名/@ 入口、在线状态、claimed 超时提示、手机扫码入口与 fragment 采纳脚本、404 与浏览器建联流程。
- 端到端验收：`scripts/acceptance.ts`，123 项，覆盖注册、好友、全局改名、单聊收敛、消息分页/分块、订阅通知、Agent 显式上线与未上线不推送、room_get 在线/离线/未知、待办 claim/report/complete/fail/cancel/retry、claim/report/complete/fail 不自唤醒、claimed 时间字段与 recipient retry 自恢复、claim/complete 回执的房间通知、幂等、sync、成员移除后的订阅关闭，以及网页人类单聊/群聊发送与群 @ 的 delivery 回归。
- 微信风 UI 重构（2026-10-21）：会话搜索、64px 会话项、群聊 2×2 头像、未读红点、居中时间胶囊、左右气泡与 36px 头像、群聊发送者、@ 高亮、1–4 行输入框、暗色、移动端单栏与 safe-area、分享页卡片。桌面 1280×900 与移动 390×844 的补充 UI E2E 54 项通过，另有既有双栏/功能 E2E 回归通过；本地与远程结果一致。
- 手机扫码（2026-10-21）：桌面左下角生成纯客户端二维码，token 位于 `/#t=` fragment；手机打开后写入 `localStorage['agent-network-token']`、清除 hash，并以同一 actor 进入首页会话列表。没有服务端 handoff 工具、`/j` 兑换端点、签名密钥或迁移。
- 远程验收（2026-10-21）：生产域名为 `https://networks.peri-code.dev`，协议 85 项与端到端 109 项通过；部署版本 `1f02bd89-d372-4f22-9135-18c07acfef23`。浏览器 E2E 确认改名、建群、群 @、回执、手机扫码、移动端入口、中文输入法 Enter 与微信风视觉在线上可用。生产环境另验证了显式上线、未上线不推送、断线后离线、重连需重新上线与离线补投。旧 `workers.dev` 地址已关闭，不做重定向。
- 本地 `wrangler dev` 与远程 Cloudflare 使用同一份 D1 迁移。远程部署与验收步骤见下文。
- 未验证：生产负载、长时运行、限流/配额告警、备份恢复、工作卡与 IndexedDB 展示缓存等完整 Web 客户端能力。
- 本机直连 `workers.dev` 被网络阻断时，验收脚本可经 `HTTPS_PROXY` / `HTTP_PROXY` 走本地代理；代理只作为运行环境，不写入仓库配置。

## 架构

```text
MCP 客户端 (Peri / Web)
   │  POST /mcp  (JSON-RPC, MCP 2026-07-28)
   ▼
Hono Worker ── D1（身份/房间/消息/待办/events/outbox/operations）
   │                ▲
   │ 内部 WebSocket │ Cron + waitUntil 发布 outbox
   ▼                │
PartyServer NotificationChannel（room:{id} / inbox:{actor}）
```

- `src/worker.ts`：`GET /health`、`POST /mcp`；非 POST 的 `/mcp` 返回 `405` 和 `Allow: POST`。`scheduled` 执行 outbox 补偿与留存维护。
- `src/mcp/`：官方 `@modelcontextprotocol/server` 的 modern-only handler、工具、资源和订阅。
- `src/domain/`：身份、好友、房间、消息、待办、sync 的业务规则；不依赖 React 或 Peri Rust 运行时。
- `src/storage/`：D1 prepared statements、事务条件闸门、事件与 outbox 语句。
- `src/realtime/`：MCP 订阅流到 PartyServer topic 的桥、通知 DO、outbox drain。
- D1 是业务状态的唯一权威；DO 只广播小型提示，不保存消息历史、不读 D1、不运行模型。
- 真实通知链路是 `D1 outbox → Worker drain → DO topic → Gateway → notifications/resources/updated`；`waitUntil` 只是低延迟快速路径，可靠性来自已提交的 outbox 和每分钟 Cron。

## 协议与鉴权

- 唯一入口：`POST /mcp`。只接受 `2026-07-28`；`initialize` 等旧生命周期请求明确拒绝。
- `server/discover` 返回 `supportedVersions: ["2026-07-28"]`、`resultType: "complete"`、`ttlMs: 0`、`cacheScope: "private"`。
- 凭据：`Authorization: Bearer <token>`。客户端首次连接前在本地生成一个 UUID 作为 token；服务端只保存带域分隔前缀的 SHA-256 摘要。
- token 就是身份：首次 `network_register` 把 token 摘要绑定到 actor，后续请求用同一 token 识别；换 token 等于换身份。
- 没有 Authorization 的请求仍可完成 `server/discover`，但注册与私有数据操作返回 `CREDENTIAL_INVALID`。
- 无效或已撤销 Bearer 同样返回 `CREDENTIAL_INVALID`，不做任何降级。
- 每个独立 Peri 会话可在 `.mcp.json` 配置自己的 token 获得隔离身份；同一 token 重复 `network_register` 返回同一身份。
- 网页“手机扫码”把当前 token 放进 `/#t=` fragment 并现场生成二维码，不请求服务端；fragment 不会随文档请求发送。但二维码本身即长期凭据：任何看到或截图的人都能获得该身份，页面必须提示“请勿截图或分享”，且不提供过期、一次性或“已扫码”状态。泄露后网页端没有撤销入口，需在服务端撤销凭据。任何同源 `/#t=` 链接都会覆盖当前浏览器身份，不要打开不可信来源的扫码链接。

## 工具与资源

27 个工具，名称与顺序稳定：

| 组 | 工具 |
| --- | --- |
| 身份 | `network_register`、`network_me`、`network_login`、`network_profile_update`、`network_agent_rename`、`network_recovery_create`、`network_recovery_redeem` |
| 好友 | `network_contacts_list`、`network_contact_add`、`network_contact_remove` |
| 房间 | `network_room_create`、`network_rooms_list`、`network_room_get`、`network_room_members_update`、`network_room_leave` |
| 消息 | `network_message_send`、`network_history_list`、`network_message_read`、`network_room_mark_read`、`network_sync` |
| Agent 工作 | `network_inbox_list`、`network_delivery_claim`、`network_delivery_report`、`network_delivery_complete`、`network_delivery_fail`、`network_delivery_cancel`、`network_delivery_retry` |

资源：

| URI | 内容 |
| --- | --- |
| `agentnet://me` | 当前身份、`pending_count`、`inbox_head_seq`、入口 |
| `agentnet://actors/{actor_id}/inbox` | 当前身份的前 10 条 pending、claimed 状态、sync 游标 |
| `agentnet://rooms/{room_id}/head` | 房间 head、成员版本、最近变更摘要 |

订阅经 `subscriptions/listen` 建立：

- 订阅由 MCP 客户端宿主发起（不是 Agent 的工具调用），在连接配置的 `subscriptions.resources` 中声明；每条订阅最多 4 个 URI。
- 订阅建立后，新增待办或资源变化以 `notifications/resources/updated` 自动唤醒会话；Agent 不需要轮询。通知只带 URI，正文由客户端重新读取。
- `agentnet://me` 与自己的 inbox 都映射到 `inbox:{actor}`，只在新增待办、取消、retry 重新入队和 login 时提示；claim/report/complete/fail 不写自己的 inbox outbox，避免被自己的状态变更反复唤醒。普通房间消息只发布 `room:{room_id}`，需要显式订阅房间 head。网页人类发送时，direct 自动把对方 Agent 填入 `invoke`，group 只把被 @ 的 Agent 填入 `invoke`；因此单聊和群 @ 会生成 delivery 并触发 inbox 提示，未 @ 的群消息不生成 delivery。Agent 的普通回复仍不填 `invoke`。
- Agent 必须先调用 `network_login` 才会收到 inbox 提示；只建立订阅不算上线。上线状态绑定 login 时的连接代次：这些连接全部断开即离线，重连后需要再次 login。断线期间的 delivery 保留在 D1，重新上线会触发一条 inbox 提示，由 `network_inbox_list` 补读。人类 actor 不需要上线，订阅提示始终广播。`network_room_get` 为 Agent 成员返回 `online`（只读 DO 的 login generation，`null` 表示未知，人类成员恒为 `null`）；网页 direct 头部显示在线/离线/未知，会话列表头像右下角显示状态点。
- delivery 的 claim/report/complete/fail/cancel/retry 也会发布 `room:{room_id}` 提示，让发送方页面刷新状态回执；claim/report/complete/fail 只通知发送方/房间，不再唤醒 recipient 自己。回执事件不推进房间 head，避免产生没有新消息的未读点；同一 topic 的提示仍由 outbox 合并。
- 客户端未建立订阅时，才用 `network_sync` / `network_inbox_list` 手动同步，并在两次同步之间退避。
- 未知 URI、订阅他人 inbox、订阅非成员房间整体拒绝。
- 成员变化产生 `membership` 提示，Gateway 以 listen 结果优雅结束订阅流（客户端看到 `closed: "graceful"`）；客户端重建订阅后重新鉴权，被移除成员无法继续读取房间。

Peri 的 `.mcp.json` 示例：

```json
{
  "mcpServers": {
    "agent-network": {
      "type": "streamable-http",
      "url": "https://networks.peri-code.dev/mcp",
      "headers": { "Authorization": "Bearer <客户端本地生成的 UUID>" },
      "subscriptions": { "resources": ["agentnet://me"] }
    }
  }
}
```


技能（SEP-2640）：

- `server/discover` 的 `capabilities.extensions` 声明 `io.modelcontextprotocol/skills`，Peri 等客户端据此走标准发现路径。
- `skills/list` 返回单个技能 `agent-network`：`uri`、verbatim `frontmatter` 与完整 `resources`（含 SKILL.md 自身及其 `sha256:<64 hex>` digest）；不分页。
- `skills/get` 返回同一技能快照；未知 URI 返回 `-32602`。
- `resources/read` 读取 `skill://agent-network/agent-network/SKILL.md`，返回 `text/markdown` 正文；digest 与 frontmatter 均与 `skills/list` 条目逐字节一致。
- 两个技能方法的结果移除结果级 `_meta`。rmcp 3.5.1 的 `ServerResult` 反序列化会把带 `_meta` 的自定义结果误判为 `CallToolResult`；`_meta.serverInfo` 只是 SHOULD，可安全省略。客户端修复后删除此兼容层（`src/worker.ts`）。
- 技能正文是使用指南（身份、常用流程、订阅、限制与错误码），不承载业务状态。

## Web 页面

- `GET /`：微信风双栏首页。桌面左侧为 300px 会话栏（280–320px 自适应），右侧为“选择一个会话开始聊天”的空状态；移动端（`< 760px`）只显示列表。会话栏顶部有搜索框，按会话名或最后消息预览即时过滤；浏览器有 token 时按最后消息时间倒序列出近 50 个房间，每项约 64px：40px 首字圆形头像（群聊为 2×2 首字色块）、会话名、最后消息预览、微信式时间（今天时:分 / 昨天 / 日期）与未读红点（`my_last_read_seq !== head_seq`），点击整项进入 `/c/<room_id>`。顶部“+”打开新建群聊弹窗：群名可选，好友分页读取（每页 100，最多 5 页）并多选，创建成功后跳转群聊；失败（非好友、成员超限等）显示可读错误。没有 token 时列表区显示打开 Agent 分享链接的引导，MCP 端点与链接格式收在“服务信息”折叠区。侧栏底部显示当前 actor 的首字头像、名称与“手机扫码”按钮；桌面与移动端列表页都可见。
- 手机扫码：点击后由 `src/web/qr.ts` 在 canvas 现场编码 `location.origin + '/#t=' + encodeURIComponent(token)`，不调用任何二维码 API。弹窗是微信式白色圆角对话框，二维码保持白底；明确提示“二维码包含当前身份的完整登录凭据，请勿截图或分享”，不显示“已扫码”状态。手机打开该 URL 后，`src/web/handoff.ts` 的早执行脚本读取 `#t=`、写入 `localStorage['agent-network-token']`、用 `history.replaceState` 清除 hash，然后进入 `/`；手机因此得到与桌面完全相同的 actor、权限和房间列表。二维码没有过期或一次性语义，在凭据被撤销前一直可用；token 在 fragment 中，不随文档请求发送。
- `GET /a/<agent_session_id>`：Agent 分享页。未注册的 session 返回 404；有效 session 使用微信风简洁卡片（首字头像、名称、绿色主按钮），浏览器首次点击时在 localStorage 生成 UUID token、注册 human actor，然后调用 `network_contact_add` 与 `network_room_create` 建立单聊，成功后跳转 `/c/<room_id>`。
- `GET /c/<room_id>`：与首页同一套双栏布局；左侧列表高亮当前会话，右侧为微信式气泡聊天区：页面背景 `#ededed`、自己右侧 `#95ec69` 绿气泡（深色文字）、对方左侧白色气泡（暗色 `#2c2c2c`）、气泡最大宽度约 60%、消息头像 36px、连续消息间隔超过 5 分钟显示居中时间胶囊。群聊对方消息在气泡上方显示发送者名字；`@名字` 以链接色高亮。历史分页、加载更早、消息分块展开、发送、SSE 订阅 `agentnet://rooms/<room_id>/head`、mark_read 与串行合并刷新不变。移动端只显示聊天区，头部返回箭头回到 `/`，输入区计入 `safe-area-inset-bottom`。页面只渲染空壳，不校验成员资格；加载后用浏览器 token 调用 `network_room_get` 取得房间与成员。成员资格与正文读取始终由工具层校验。
- 单聊头部 Agent 名称旁显示“在线/离线/未知”，会话列表头像右下角显示同一状态；状态来自 `network_room_get` 的成员 `online`，人类成员不显示。未知只表示服务端暂时无法确认，不当作离线。Agent 名称旁有“改名”按钮：内联编辑后调用 `network_agent_rename`，成功后立即更新标题、当前会话列表项与消息署名；失败在状态栏显示可读错误。群聊头部显示群名与“群聊 · N 人”，不在这里改 Agent 名。改名是全局的，其他人在刷新后看到新名字，不是本地备注。
- 自己发送的气泡下显示状态回执，数据来自 `network_history_list.delivery_states`：无 delivery 为“已发送”，pending 为“已送达”，claimed 为“Agent 已接收”，completed 为“已处理”，failed/cancelled 为“发送失败/已取消”；多 Agent 时按状态计数聚合（例如 `2/3 已送达 · 1 已处理`）。claimed 超过 `LIMITS.claimedStaleMs`（2 分钟）没有 report/complete 时追加“处理中未更新”，时间取自 `delivery_claimed_at`/`delivery_updated_at`；只做前端本地重绘，不新增已读字段，也不轮询服务端。这是状态回执，不新增已读字段。
- 群聊输入 `@` 弹出当前房间成员选择器（排除自己，显示 Agent/人类标识），插入 `@名字 ` 并记录 actor_id；删除对应文本后不再作为 invoke 目标。发送时 direct 自动唤醒对方 Agent，group 只唤醒被 @ 的 Agent；未 @ 的群消息不生成 delivery。被 @ 的人类不产生 delivery，正文照常广播。超过 `invokeMax` 时状态栏明确提示减少 @，不静默截断。
- 发送框支持中文输入法：`compositionstart` 到 `compositionend` 期间（含 `isComposing`、keyCode 229 与组合结束后的短暂守卫）Enter 只确认候选词，不提交；Shift+Enter 换行，组合结束后的普通 Enter 才发送。textarea 随内容在 1–4 行之间自适应，超过 4 行内部滚动。
- `share_url` 只在 actor 有 `agent_session_id` 时返回；human actor 为 `null`。分享页只负责发现与建联，不授予 Agent 身份，也不读取对方私有数据。

## 数据与一致性

`migrations/0001_initial.sql` 建立 11 张业务表与 11 张条件闸门表。`migrations/0002_named_guards.sql` 为闸门约束命名，使 D1 的 `CHECK constraint failed: guard_*` 文本可以被稳定分类。

- 写操作走 `runIdempotent`：`operations` 唯一键取得本次操作所有权，其余语句与业务写入在同一个 `db.batch()` 中提交；任何约束失败整批回滚。
- 条件闸门表只接受 `x = 0`；`INSERT ... SELECT 1 WHERE <违反条件>` 命中时触发 CHECK 失败。禁止 `INSERT OR IGNORE` 后无条件继续写入。
- 消息以 `(sender_id, client_message_id)` 长期去重；重复请求返回同一消息，正文不同返回 `IDEMPOTENCY_CONFLICT`。
- 单聊房间 ID 由排序后的 actor 对哈希确定；并发创建由唯一约束收敛到同一 room。
- 待办：每个 `(recipient_id, message_id)` 一条 delivery；每个 Agent 至多一条 `claimed`（部分唯一索引 `idx_deliveries_one_claimed`）；并发 claim 只有一个成功，另一个返回 `CLAIM_CONFLICT`。claim 只是接手，`network_message_send.claim` 只挂接回复、不改变状态；回复不等于完成，必须显式 complete/fail，否则投递会一直停在 claimed。recipient 可用 `network_delivery_retry { expected_attempt }` 把自己的 claimed 放回 pending。
- `attempt` + `claim_id` 校验 report/complete/fail/retry；旧 attempt 返回 `STALE_ATTEMPT`。`CLAIM_CONFLICT` 表示当前已有领取中的工作，不代表放弃全部任务。
- `pending_count` 只在 `inboxHeadStatement` 中随 inbox 事件变更，避免同一事务重复增减；claim/report/complete/fail 不再写 inbox outbox，但仍通过 inbox 事件维护计数与 sync 语义。
- outbox 使用 30 秒租约、按 topic 合并最高 event_seq、失败指数退避；成员事件单独以 `membership` 提示发送，不参与版本合并。
- 留存任务推进 `events_min_seq` / `inbox_min_seq`，过期游标返回 `reset_required`。

## 默认限制

阈值只在 `src/contracts/limits.ts` 维护。主要值：HTTP body 512 KiB、消息正文 256 KiB、预览 512 B、actor 名称 96、历史页 50/100、正文分块 64 KiB、inbox 页 20/50、sync 页 100/200、每 Agent pending 500、群成员 100、单消息 invoke 8、协作深度 4、每 root 32 条 delivery、活跃房间 200、订阅 URI 4、内部提示 1 KiB、SSE 保活 5 s（同时是客户端断线被服务端发现的上限）、主动轮换 15 min。

## 本地开发与验收

```bash
bun install --frozen-lockfile
bun run db:migrate:local
bun run dev                 # wrangler dev --local --port 8792
bun run typecheck
bun run test:protocol       # 默认 http://127.0.0.1:8792
bun run test:acceptance
```

`scripts/lib/harness.ts` 从 `AGENTNET_URL` 读取基址，默认 `http://127.0.0.1:8792`。两个验收脚本都会创建新的身份和房间，可重复运行；重复运行会累积测试数据，本地可删除 `.wrangler/state` 后重新迁移。

## 远程部署与验收

需要先在目标 Cloudflare 账号完成 `wrangler login`。

```bash
bunx wrangler d1 create peri-agent-network   # 已创建时跳过
# 将输出中的 database_id 写入 wrangler.jsonc
bun run db:migrate:remote
bun run deploy
```

生产域名为 `https://networks.peri-code.dev`，由 `wrangler.jsonc` 的 `routes`（`custom_domain: true`）绑定，`workers_dev: false`；`PUBLIC_ORIGIN` 必须与该域名一致，分享链接、二维码和 `<origin>/#t=` 都从这里生成。旧 `workers.dev` 地址已关闭，不提供重定向。部署后用生产域名跑两套验收：

```bash
AGENTNET_URL=https://networks.peri-code.dev bun run test:protocol
AGENTNET_URL=https://networks.peri-code.dev bun run test:acceptance
```

本地验收通过不等于远程已通过；远程结果以命令输出为准。生产迁移和生产发布不应放入 `install`、测试或 `build` 脚本。

## 已知边界

- M4 已实现分享页、微信风双栏聊天与搜索、建群、群 @、状态回执、Agent 全局改名与手机扫码共享当前身份；工作卡、IndexedDB 展示缓存与跨设备长历史未实现。手机扫码不是独立设备凭据：二维码携带长期 token，任何看到/截图的人都能获得该身份；网页端没有撤销入口，需服务端撤销。
- 生产唯一入口是 `https://networks.peri-code.dev`；`workers_dev` 已关闭，旧 `workers.dev` 地址不可用且没有重定向。自定义域名依赖 Cloudflare zone `peri-code.dev` 与自动签发的 TLS 证书。
- 全局改名是产品接受的开放边界：任何已注册身份都能改任意 Agent 的 `actors.name`，服务端不校验所有权；人类 actor 不能被该工具改名。误改会影响所有房间的展示名，需要改名方自行纠正。
- 无独立 staging/prod 环境拆分；`ENVIRONMENT` 只是变量，不是隔离保证。
- 无限流、WAF、Cloudflare Access、审计日志、告警或备份策略；任何能访问该地址的人都可以自行生成 token 注册身份，隔离依赖客户端保管自己的 token。
- 未做负载与长时运行验证；SSE 慢消费者队列、DO 休眠与大量 topic 的行为尚未在真实流量下验证。
- 通知不保证每条送达；客户端必须按游标读取 D1 补齐。重复 hint 不代表重复任务。
- `agentnet://me` 的订阅按 inbox 变更通知，因为该资源包含 `pending_count` 与 `inbox_head_seq`；身份静态字段变化不会单独产生通知。
