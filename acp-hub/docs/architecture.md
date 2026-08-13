# acp-hub 架构设计（权威版）

> 状态：v2.5（补充 Web project session 与浏览器认证契约）
> 日期：2026-08-12
> 定位：acp-hub 独立项目的架构基准文档。与 peri 的唯一耦合点是 ACP 进程（协议线格式），本设计不依赖 peri 的任何 crate 与部署形态。
> 来源：三轮对抗面试（产品/用户角度）收敛裁决 + 参考实现 `@fenix/chat-channel`（`/Users/konghayao/code/pazhou/remote-control-server/packages/chat-channel`，实现基线 `docs/arch/19-yjs-chat-streaming.md`，ADR `spec/global/adr/2026-08-04-chat-channel-package-design.md`）+ 三视角对抗审查（架构师/高级开发工程师/高级运维工程师，2026-08-07）+ 三轮 advisor 成熟度审查（2026-08-07，opus，第三轮评级：**可开工**）。v2.1 修订项以「【审查】」标注；v2.2 以「【顾问】」；v2.3 以「【顾问2】」；v2.4 以「【顾问3】」。advisor 关于「删除 HMAC 双向认证」的删减建议**被否决**（§9.2 保留，v2.3 补齐协议级规范，v2.4 补齐线格式精度）。
> 约定：引用 chat-channel 处标注其文档章节号（如「chat §5.2」），实现时以该仓库为对照基线。

---

## 1. 背景与目标

### 1.1 现状

现有 `acp-hub` 是一个 stdio 桥接器：IDE stdin → JSON-RPC 解析 → 按 chat 分流到独立 ACP 子进程 → 子进程 stdout 转发回 IDE。单机、单连接、进程随 IDE 生命周期。

### 1.2 演进目标

将 acp-hub 升级为**中心服务器**形态：

1. **server / instance 两级实体**：server 是中心控制面；instance 是实际运行 ACP 进程的机器，与 server 通过 WebSocket 联通，接收 server 下发指令完成 ACP 进程的启动/停止。
2. **ws 通信取代 stdio**：为未来 CS 远程模式（本机 TUI 连远程 server、局域网多机）打基础。
3. **yjs 统一数据对象**：ACP 事件在 server 侧经**规范化边界 + 聚合器（agg）**投影为**视图对象**，以 yjs 标准数据结构承载（每 chat 双 Doc + 全局 Registry Doc），多端（多 TUI、未来 Web 面板）经 yjs 同步一致。
4. **TUI 纯视图层**：server 是独立常驻后台进程；TUI 只是视图层，与 server 是 client–server 关系，经 ws + yjs 同步状态。

### 1.3 非目标（明确不做）

- chat 跨 instance 迁移 / 自动恢复
- server 自动负载均衡调度
- 多用户、配额、审计（结构化操作日志保留，见 §9.4；连接级配额除外，见 §8.6）
- 公网部署（wss / 公网认证）——后置为 M4
- ACP 协议本身升级（wire format 保持既有兼容）
- 领域事件日志体系与写租约（chat Q5 评审决策，理由同源：YJS CRDT 保证收敛、进程内单写、`commandId` 去重承担防重复副作用；去重记录持久化见 §4.4）

---

## 2. 产品语义（用户可见行为契约）

以下为第一版必须成立的用户可见行为，作为验收基线：

| # | 语义 | 验收标准 |
|---|------|---------|
| P1 | TUI 崩溃/重启不影响正在运行的 agent | kill TUI 后 agent 继续跑完，重开 TUI 秒级恢复视图 |
| P2 | 多 TUI 可同时 attach 同一 server | 两个 TUI 看到一致状态；任一 TUI 可发控制指令 |
| P3 | server 崩溃/重启不中断 agent | 重启 server 后 instance 自动重连，agent 产出**在缓冲有界承诺内不丢**（缓冲上限内不丢；超限按 §8.5 丢弃策略丢弃并以 gap 呈现，不假装完整）【顾问：P0-3】 |
| P4 | instance 断线时活动 turn 明确中断，chat 可恢复 | 断线瞬间活动 turn 呈现 `interrupted`；补推完成后 chat 恢复可用、可开新 turn（见 §7.3 分区恢复裁决） |
| P5 | 新建 chat 显式指定 instance（默认本机） | 路由可预测、可调试 |
| P6 | TUI 操作（发消息/cancel/新建/关闭）有请求-响应确认 | 两阶段 Ack（accepted→committed），失败有稳定错误码，不静默 |
| P7 | 未知设备无法接入 | token 校验失败即断开，无任何数据可见 |
| P8 | 重试安全 | 客户端以同一 `commandId` 重发不产生重复副作用（去重记录持久化，跨 server 重启有效，见 §4.4） |
| P9 | instance daemon 崩溃不产生无人知晓的孤儿执行 | 重连后 server 对「已中断但 instance 声称存活」的 chat 默认下发 kill 清理，TUI 可见（见 §7.5） |

---

## 3. 系统拓扑与组件

### 3.0 Web project session 扩展

Web UI 使用四层身份，禁止互换：`project_id` 是左栏分组，`project_session_id` 是 SQLite 持久入口，ACP `session_id` 是 agent 的 durable thread，`chat_id` 是一次 server/ACP runtime。`last_chat_id` 只作运行期提示；重启后打开持久入口必须以精确 ACP session id 走 `session/load`，不得复活旧进程或根据标题猜测。

左栏 catalog 只展示来源为 `hub`（经 `session/create` 建立）或 `imported`（用户经 `session/import` 明确加入）的 project session。ACP `session/list` 的其余历史仅作为按 project cwd 分面的导入候选，不得在启动或轮询时自动进入侧边栏；旧版自动迁移记录标为 `legacy_hidden`，保留数据但不投影。

`<data_dir>/metadata.sqlite3` 是 project/project session 元数据与全局 metadata command 去重的唯一事实源；现有 per-chat update/outbox/watermark 保持原崩溃恢复语义。Registry v2 的 `projects`、`project_sessions` 是 SQLite 的只读广播投影。project/session mutation 的 committed Ack 必须跨过 SQLite 提交与 Registry 投影屏障；ACP 副作用结果不确定时进入 `reconciliation_required`，不得自动重试 `session/new`。`project/rename` 只更新展示名并保持 project id、cwd、instance binding 与所有 session identity 不变。project 的“删除”在用户界面中始终是可逆归档：`project/archive` 设置 `archived_at`，`project/restore` 清除该字段，三类 project mutation 都复用全局 commandId 去重与投影屏障；不会删除 project session、ACP thread 或工作目录文件。任何 project session 仍绑定非终态 runtime 时，归档必须拒绝；无法读取元数据或验证 runtime 状态时同样 fail-closed，避免把仍在工作的 agent 从导航中隐藏。

Registry 视图采用 `<data_dir>/registry.snapshot` + `<data_dir>/registry.log` 增量记录恢复。增量日志超过 8 MiB 后，server 把当前**可见值图**物化到一个新的 Yjs Doc，丢弃只影响历史合并而不影响当前投影的 tombstone/历史 item，再以 tmp → fsync → rename → 目录 fsync 发布快照；快照通过 blob CRC 与 Yjs decode 读回验证后才轮换日志。首次从旧的纯日志格式迁移时，原日志保留为权限 `0600` 的 `registry.log.legacy-v1`，供兼容窗口内人工回滚；后续压缩原子截断增量日志。启动时先应用快照再应用增量；若增量尾部是半写或 CRC 损坏，只截断到最后一条完整记录，损坏快照则 fail-fast，不静默重建。

浏览器认证通过同源 `POST/GET/DELETE /api/auth/session` 建立内存 opaque session，并只下发 `HttpOnly; SameSite=Strict; Path=/` Cookie。Web 不在 URL、Web Storage 或 WS 首帧保存/发送 bearer token。Cookie attach 与存量连接按心跳重新校验 token id、撤销状态和当前 role；loopback HTTP 校验 Host/Origin，instance HMAC 与旧 CLI wire-token 流程保持兼容。

### 3.1 拓扑

```
┌───────────────┐   ws(单连接, 多路复用)   ┌───────────────────┐
│  acp-hub-tui  │◄───────────────────────►│                   │
│  (视图层 ×N)  │   Action/Ack 控制帧      │                   │
└───────────────┘   + y-sync 状态帧        │                   │
┌───────────────┐                          │  acp-hub-server   │
│  Web 面板(M3) │◄───────────────────────►│  (常驻后台进程)    │
└───────────────┘                          │  - 认证/授权       │
                                          │  - 控制面          │
┌───────────────┐   ws(outbound, 主动连)  │  - ACPChannel     │
│ acp-instance   │◄───────────────────────►│  - 聚合器(agg)    │
│ (每台机器 1个)│   instance 协议          │  - DocManager     │
└──────┬────────┘                          │  - 广播器          │
       │ stdio (JSON-RPC 行协议)           └───────────────────┘
       ├───────────► [ACP 进程 session_1]
       ├───────────► [ACP 进程 session_2]
       └───────────► [...]
```

要点：

- **连接方向**：instance **主动 outbound** 连接 server（NAT 友好、server 零入站依赖）；TUI/Web 主动连 server。
- **单 ws 多路复用**：一条连接上按帧类型区分控制帧（Action/Ack）与状态帧（y-sync），见 §4。
- **instance 与 ACP 进程之间保持 stdio**：复用现有 `child.rs` 的 spawn/监控/转发能力（现有资产直接迁移），instance 对上层统一输出为**原始 ACP 帧流**。
- **规范化只发生在 server 侧**：`ACPChannel` 边界在 server（§6.1），instance 保持透明转发。

### 3.2 组件与二进制

三个独立二进制（共享一个协议 crate）：

| 二进制 | 职责 | 备注 |
|--------|------|------|
| `acp-hub-server` | 常驻后台：认证、控制面、ACPChannel 规范化、聚合器、DocManager、instance 注册表 | 无 TUI 依赖，可 launchd/systemd 托管 |
| `acp-instance` | 每台机器一个：outbound 连 server、收 spawn/kill 指令、管理 ACP 进程树、透明转发 + 断线缓冲 | 由现有 acp-hub 单机版演化 |
| `acp-hub-tui` | 纯视图层：yjs 渲染 + Action/Ack 操作 | 与 peri-tui 视觉风格一致（ratatui），独立实现 |

共享 crate：`acp-hub-proto`（帧定义、Action/Ack 信封、instance 协议类型、Y.Doc schema 的 Rust 类型镜像）。

> 裁决依据：用户明确要求 server 与 instance 为两个独立二进制。共享代码收敛在 `acp-hub-proto`，避免两处重复实现协议解析。

### 3.3 instance 职责边界【审查：架构 P1-2 + 运维 P0-2】

instance 是 **dumb pipe**，但「不做协议理解」需精确化——缓冲分桶与转发要求最小协议面：

**允许的最小协议面（instance 侧唯一允许的 ACP 帧解析）**：
1. 双格式 sessionId 提取（原始 `{type,payload}` 与 JSON-RPC 包裹格式，与 §6.1 ACPChannel 双格式兼容规则一致）；
2. 按 chat 分桶 + 分配单调 `seq`；
3. 进程管理（spawn/kill/退出监控）与本地缓冲。

**禁止项**：不解析事件语义（不区分 delta/终态/工具）、不聚合、不写任何状态、不生成业务事件。

**无法提取 sessionId 的帧**：丢弃并记本地缺口计数（随 `instance/hello` 上报）。

**instance 进程本身崩溃**【审查：运维 P0-2】：
- 正常退出由 `shutdown_all` + `kill_on_drop` 终止 ACP 进程树；daemon 被 `SIGKILL` 时 Drop 无法运行，ACP 进程组可能残留。instance data-dir 持有非阻塞独占 owner lock，watermark 同时记录 data-dir `(dev,ino)` 与进程组 leader 出生指纹；下次启动只在两者精确匹配时发 `SIGKILL`。旧 watermark、目录副本、PID/PGID 复用或指纹不可读都 fail closed 为不发信号，仍上报 `buffer_lost` 交给 server 对账；
- 内存缓冲与磁盘溢出缓冲**不跨重启保留**（重启后 `hello` 上报 `buffer_lost: true`）；
- 每 chat `seq` 计数器与 `stream_epoch` 绑定（daemon 重启后 epoch +1、seq 可重置，§4.5.1【顾问：P0-2】）。

---

## 4. 通信协议

### 4.1 传输与序列化

- WebSocket，文本帧，每条消息一个 JSON 对象。
- 序列化：控制帧（action/ack/error/instance.*/event/keep_alive/ready/pong/auth）用 serde_json；`ysync.*` 帧体为 y-sync 协议消息（`Y.encodeStateAsUpdate` / update diff），**base64 嵌入文本帧**（与 chat `broadcaster.ts` 的 `Buffer.toString("base64")` 一致）；固定 update 编码版本 v1。【审查：开发 P2→写为协议事实】
- 局域网默认 ws 明文；支持配置 TLS（wss）后置（M4）。**运维指引**：M1–M3 默认监听绑定 + 不可信网络禁用（见 §16 配置默认值）。

### 4.2 帧模型（单连接多路复用）

每条消息 `{ "t": <frame_type>, ... }`，按 `t` 分派：

| `t` | 方向 | 载荷 | 说明 |
|-----|------|------|------|
| `action` | C→S | Action envelope（§4.3） | 控制命令，必须 Ack |
| `action_ack` | S→C | 两阶段 Ack（§4.4） | 每个 action 至多一个最终 Ack |
| `action_error` | S→C | 稳定错误码（§4.4） | 失败即返回，不静默 |
| `ysync.sync` | 双向 | y-sync Step 1/2 消息 | 文档同步握手 |
| `ysync.update` | S→C（**单向**） | y-sync update（增量/快照，base64） | 状态变更传播；**客户端（TUI/Web）上行 update 一律拒绝**——server 是唯一写入者（§5.6），客户端无写权限、不持有写租约【顾问：P0-4】 |
| `ysync.subscribe` | C→S | `{ docs: ["chat:{cid}", ...] }` | 订阅指定 Doc 的更新（多 chat 视图必需）【审查：开发 P1】 |
| `ysync.unsubscribe` | C→S | `{ docs: [...] }` | 退订 |
| `ysync.awareness` | 双向 | y-protocol awareness | 在线状态（M3 启用） |
| `ready` | S→C | `{ projection_versions: {...} }` | 快照推送完成握手（§4.6）【审查：开发 P1】 |
| `pong` | C→S | — | keep_alive 回执（§4.7）【审查：开发 P1】 |
| `keep_alive` | S→C | ping | 心跳 |
| `event` | S→C | `{ chat_id, seq, frame }` | `events/subscribe` 推送（§4.3）【审查：开发 P1】 |
| `instance.*` | S↔M | instance 协议帧（§4.5） | server ↔ instance 专用 |
| `auth` | C→S | `{ token }` | 连接建立后的第一个帧；**角色由 token 解析，客户端不声明 role**【审查：开发 P2】 |

### 4.3 Action envelope 与方法面

参照 chat §7.1（Q9 修订）：客户端只发送 `commandId` 与 action 内容，信封其余字段由服务端按 chat 绑定补充与校验。

```jsonc
{
  "t": "action",
  "commandId": "uuid",          // 幂等键，同 chat 唯一；重试复用同一 ID（绝不可换 ID 猜测结果）
  "type": "chat/prompt",     // 见方法面
  "payload": { ... }            // 转发所需绑定字段（ACP session_id 等）由服务端注入，客户端字段不可覆盖 binding
}
```

Action 方法面（Server 对客户端）：

| type | payload | 说明 |
|------|---------|------|
| `chat/create` | `{ instance_id?, cwd?, title? }` | instance_id 缺省 = 本机（P5）；cwd 语义见下 |
| `chat/load` | `{ chat_id }` | 载入既有对话（转发 ACP `session/load`；转发前开启回放窗口，见 §8.6） |
| `chat/close` | `{ chat_id }` | 关闭并 kill 对应 ACP 进程（offline 时语义见 §7.6） |
| `chat/prompt` | `{ chat_id, message }` | 转发到目标 instance |
| `chat/cancel` | `{ chat_id }` | 转发 cancel（携带目标 sessionId，路由据此精确投递） |
| `permission/resolve` | `{ chat_id, permission_id, decision }` | 权限应答（CAS 校验通过后才下发，见 §7.4） |
| `events/subscribe` | `{ chat_id?, from_seq? }` | 原始 ACP 事件订阅（§4.3.1） |

**cwd 语义裁决**【审查：开发 P2】：客户端可指定 `cwd`，server 校验其合法性并注入默认值（未指定时用已认证上下文默认目录）；`Translator` 出站时始终由 server 按已认证上下文注入最终 `cwd`（§6.1 同源），客户端字段不可越权。

#### 4.3.1 events/subscribe 订阅契约【审查：开发 P1】

- `events/subscribe { chat_id?, from_seq? }`：`from_seq` 缺省 = 实时起（不重放历史）；带 `from_seq` 则从该序号起推。
- `events/unsubscribe { chat_id? }` 退订。
- 推送帧：`{ "t": "event", chat_id, seq, frame }`（frame 为规范化事件，chat_id 为 hub 侧 id，经 binding 翻译——不透传原始 ACP session_id）【审查：架构 P2-3】。
- 无权限 chat 的订阅 → `FORBIDDEN`。
- **双流顺序契约**：视图收敛以 yjs 为准，事件流尽力而为（背压时允许丢弃），双流之间无顺序契约。

### 4.4 Ack 与错误码

参照 chat §7.1：`accepted` 只表示进入有界处理队列，`committed` 才表示业务事实已持久化（**对应 update 已落盘**，见 §8.4）。

```jsonc
// action_ack
{ "t": "action_ack", "commandId": "uuid", "status": "accepted" | "committed" | "duplicate",
  "turnId?", "chatId?", "committedProjectionVersion?" }
//  - chatId：chat/create 的 committed 必须携带（server 生成 id 的唯一告知路径）【审查：开发 P1】
//  - committedProjectionVersion：字段预留（对齐 chat types.ts，乐观并发校验二期启用）【审查：开发 P1】

// action_error
{ "t": "action_error", "commandId": "uuid",
  "code": "UNAUTHENTICATED" | "FORBIDDEN" | "CHAT_NOT_FOUND" | "INSTANCE_OFFLINE"
        | "VERSION_CONFLICT" | "INVALID_STATE" | "RATE_LIMITED" | "AGENT_UNAVAILABLE"
        | "PAYLOAD_TOO_LARGE" | "UNSUPPORTED_FRAME",
  "message": "脱敏信息", "retryable": boolean, "retryAfterMs?" }
```

**commandId 去重与持久化（server command outbox）**【审查：架构 P0-1 + 开发 P0-4 + 顾问 P0-1】：

- **去重记录必须移出 Y.Doc，持久化到 server 的 command outbox**【顾问：P0-1】。理由：去重要防的是 **ACP 进程的外部副作用**，而 Y.Doc 是可丢弃的实时镜像（§8.1 原则 5）——视图重建 ≠ 去重事实重建；且 update 日志会被 compact 裁剪、按磁盘预算归档，去重记录随 Doc 生命周期存亡会失效。outbox 是与 update 日志**并列的独立持久化文件**（按 chat 分片，`commandId → {type, turnId, status, dispatched_at}`，与 §8.4 同一 fsync 纪律）。
- 去重表 = outbox 的内存索引：每 chat 进程内 Map<commandId, 记录>，启动时从 outbox 重放重建；**committed 记录删除的唯一时机 = 显式清理策略**（如 chat 关闭后保留 7 天、按磁盘预算淘汰，不随 Doc compact 消失）。P8 × P3 交集由 outbox 跨 server 重启成立。
- 已提交命令重发返回原 Ack（`duplicate`）与 `turnId`，不重复调用 Agent；执行失败（`AGENT_UNAVAILABLE` 等 retryable 错误）清除 outbox 记录，允许重发重新执行。
- **turnId 生成规则**：由 server 生成（uuid）；同 `commandId` 重试复用同一 `turnId`（从 outbox 读取），新 `commandId` 产生新 `turnId`。

**delivery_confirmed 三级定义**【顾问：P0-1】（`committed` 的强度依据，内部实现语义，不暴露给客户端）：

| 层级 | 含义 | 达成条件 |
|------|------|---------|
| L1 ws 传输确认 | 指令帧已送达 instance | instance 收到下行指令并返回对应 `instance/*_ack`（spawn_ack/kill_ack）或对 prompt/cancel/resolve 的转发确认 |
| L2 stdin 写入确认 | instance 已将指令完整写入 ACP 进程 stdin | instance 侧写成功（字节级确认；ACP 子进程退出写失败 → 上报失败） |
| L3 ACP 接收确认 | ACP 进程已受理（JSON-RPC 请求已发出且未被连接错误拒绝） | instance 转发 ACP 的响应/错误帧（含 JSON-RPC error 也算受理，业务失败走 `action_error`） |

M1 实现 L1+L2 合并（instance 在转发确认中隐含写成功），L3 作为 `chat/prompt` 的 committed 前置。

**outbox 记录状态机**【顾问2：P0-1】（每条 outbox 记录唯一、持久化的迁移路径；任意崩溃点不得产生默认重复投递）：

```
received → accepted → intent_durable → dispatched → delivery_confirmed
   │           │              │             │             │
   │           │              │             └─► delivery_unknown（L2 后 L3 不可得）【顾问2】
   │           │              └─► 失败清除（retryable 错误，允许重发）
   └───────────┴──► failed（终态）
delivery_confirmed → projection_committed → completed（终态）
                   └─► failed（终态，业务失败走 action_error）
```

**delivery_unknown 裁决**【顾问2：P0-1】（L3 依赖 peri ACP 关联 ID 能力，M1 前必须裁决，二选一）：

- **路径 A（peri ACP 支持关联 ID）**：ACP 请求携带/回传稳定关联 ID（commandId 或映射 ID），可查询处理状态 → 恢复时依据关联 ID 判定「未接收 / 已接收未完成 / 已完成」后决定重试；L3 定义查询路径并固定映射。
- **路径 B（不支持）**：L2 后未取得 L3 一律进入持久化 `delivery_unknown`——**非幂等命令（prompt/cancel/permission/resolve）禁止自动重试**，直至可观测状态对账（如 agent 状态查询）或人工裁决完成；仅幂等命令（create/close 按 chat_id）可安全重发。
- **M1 决策门禁（非开工门禁）**【顾问2 + 顾问3】：与 peri ACP 核实 prompt/cancel/permission 的关联 ID 能力；**开工不等待此确认，M1 默认按路径 B 实现**（路径 A 是强化项）；**M1 功能完成/发布前**必须给出结论——支持则启用路径 A，不支持则正式接受并验证路径 B（人工裁决运营成本达标）。【顾问3】
- **幂等性分类默认**【顾问3】：所有命令进入 outbox 前必须显式标记重试类别（可安全重发 / 不可自动重发）；**未分类命令默认禁止自动重发**；新增命令类型必须走同一分类流程，不得绕过。
- **路径 B runbook 要点**【顾问3】：`delivery_unknown` 必须可查询、可持久化、可展示（重启/归档不得静默丢弃）；人工裁决入口与权限定义：谁能裁决（server 操作员）、依据哪些可观测状态（agent 状态查询/进程存活/用户确认）、裁决结果迁移——「确认已送达」→ completed、「确认未送达」→ 清除记录允许重发、「仍未知」→ 保持 delivery_unknown；每次裁决留审计记录（§9.4 结构化日志）。

**崩溃点 × 持久化状态 × 重试行为**【顾问：P0-1】（每类 action 的执行语义）：

| action | server 崩溃点 | outbox 持久化状态 | 重启后重试行为 |
|--------|-------------|------------------|---------------|
| `chat/create` | outbox 落盘后、spawn 前 | `dispatched=false` | 重发重新走 spawn（§6.2 时序；chat_id 幂等，已存在返回现有句柄） |
| `chat/create` | spawn 后、binding 前 | `dispatched=true` | 重发 → server 与 instance 对账 binding，缺失则重走；不二次起进程 |
| `chat/prompt` | outbox 落盘后、投递前 | `dispatched=false` | 重发重新投递（客户端重试窗口内） |
| `chat/prompt` | 投递后（L1+L2 达成）、投影前 | `dispatched=true` | 重发返回 `duplicate` + turnId，**不重复调用 Agent**（outbox 兜底，提交顺序不可倒置）；恢复时该记录按 delivery_unknown 裁决：路径 A 以关联 ID 查询，路径 B 展示「结果未知」并走对账/人工【顾问2】 |
| `chat/cancel` | 同 prompt | 同 prompt | 同 prompt；`cancelling` 状态幂等迁移 |
| `permission/resolve` | 同 prompt | 同 prompt | 同 prompt；CAS 已迁移则返回 `duplicate`【审查：开发 P2】 |
| `chat/close` | outbox 落盘后 | 同 prompt | 重发；instance offline 时走 §7.6 `pending_close` |

**重试分类**：`AGENT_UNAVAILABLE` / `INSTANCE_OFFLINE` → `retryable=true`（可自动重试，须复用同一 commandId）；`INVALID_STATE` / `FORBIDDEN` / `SESSION_NOT_FOUND` → `retryable=false`（重试不会改变结果）。

- **提交点纪律**【审查：架构 P0-1】：user entry 在 ACP 投递确认**后**写（`committed` 返回前完成）。顺序不可倒置：**outbox 记录先行落盘 → 下发 ACP → L1+L2 投递确认 → 投影 user entry → committed Ack**。投递确认前 server 崩溃 → 该 commandId 无 dispatched 记录，客户端重发即重新执行，无幽灵 turn；投递成功但 entry 投影前崩溃 → 重发时 `duplicate` 由 outbox 兜底。
- 错误码 `VERSION_CONFLICT` 与开放问题 4 对齐：**保留字段语义，M1 不强制校验**（见 §14 开放问题 4）【审查：架构 P2-9】。

### 4.5 Server ↔ instance 协议

**Server 下发**（下行指令均携带 `commandId`；以 `chat_id` 为天然幂等键——server 可安全重发）【审查：架构 P1-4 + 开发 P0-3/P0-4】：

| 方法 | 参数 | 说明 |
|------|------|------|
| `instance/spawn` | `{ command_id, chat_id, cmd, cwd, env? }` | 启动 ACP 进程；**按 chat_id 幂等**（已存在返回现有句柄，不二次起进程）；`env` 受 server 白名单约束（§9.5）【顾问：P1-7】 |
| `instance/kill` | `{ command_id, chat_id, grace? }` | 停止 ACP 进程；**幂等**（已死成功返回） |

**Instance 上报**：

| 方法 | 参数 | 说明 |
|------|------|------|
| `instance/hello` | `{ token, hostname, caps, buffered?, buffer_lost?, stream_epochs?, nonce }` | 注册 + 重连握手；**幂等替换语义**——新 hello 到达即 fencing 旧连接（旧连接事件丢弃、关闭）【审查：架构 P1-4】；`buffer_lost` 上报 daemon 崩溃缓冲丢失（§7.5）【审查：运维 P0-2】；`stream_epochs` 为 per-chat 流纪元映射（§4.5.1）【顾问：P0-2】；`nonce` 用于 server 身份证明（§9.2） |
| `instance/heartbeat` | `{ load, alive_sessions }` | 周期心跳（默认 5s） |
| `instance/event` | `{ chat_id, epoch, seq, frame }` | 原始 ACP 帧转发（**带 instance 侧单调 seq 与流纪元**）【审查：开发 P0-3】【顾问：P0-2】 |
| `instance/buffer_sync` | `{ chat_id, epoch, from_seq, frames[] }` | 断线缓冲补推（frames 每帧带 seq；epoch 由 server 回传校验，见 §4.5.1） |
| `instance/spawn_ack` | `{ command_id, chat_id, ok, error? }` | spawn 结果（成功/失败+脱敏原因）【审查：开发 P0-3】 |
| `instance/kill_ack` | `{ command_id, chat_id, ok }` | kill 结果 |
| `instance/process_exit` | `{ chat_id, code }` | ACP 进程退出事件（含退出码；`crashed`/`ended` 状态由此驱动）【审查：开发 P0-3】 |

**buffer_sync 起点（from_seq）**【审查：开发 P1】：server 持久化 per-chat `last_seq`（随 update 日志，§8.4）；重连后 `from_seq = last_seq + 1`。**instance 保留环形滑窗**（最后 500 条，覆盖 server 崩溃前已收未落盘段）作为兜底：server 发现缺口时请求滑窗重发。

#### 4.5.1 stream_epoch（流纪元）【顾问：P0-2】

`stream_epoch` 是 instance 侧 per-chat 的**流代际标识**：instance 为每个 chat 的 ACP 输出流维护一个纪元号，**daemon 重启或 ACP 子进程重建时 +1**（chat 新开为 1）。它解决「补推边界无法区分旧流残余与新流开始」的歧义：

- **epoch 相同**（同一代际）：server 已持久化 `(epoch, last_seq)`，补推按 `from_seq = last_seq + 1` 连续追平；
- **epoch 变化**（daemon 崩溃重启 / 进程重建）：旧流 seq 空间作废，**server 判定该 chat 产生不可校准缺口**——chat 保持 `interrupted` + `gap`（uncalibratable），不尝试按 seq 补推旧流；若 chat 已终止（ended/closed）则无需处理；
- server 持久化 `(epoch, last_seq)` 对（随 outbox/update 日志同目录独立文件）；`instance/buffer_sync` 回传 epoch，与 server 记录不一致即拒绝该批（防旧纪元缓冲混入新纪元流）；
- `instance/event` 携带 epoch：epoch 与 server 记录不一致的帧直接丢弃并计数（防御性，正常路径下 hello 已对账）。

### 4.6 连接建立时序（快照先于操作）

参照 chat §4.1（Q13 实现差异：不采用 y-sync 增量握手，先推全量快照）：

1. 连接配额检查 → `auth`（token）→ 授权解析（角色/可访问 chat 集合）。
2. 按订阅清单（`ysync.subscribe`）打开/恢复 Chat Doc、Control Doc 与 Registry Doc（首个客户端注册广播监听）。
3. 推送各 Doc 的**全量快照**（`ysync.update` snapshot，携带各 Doc 的 `projection_version`）【审查：开发 P1】。
4. 发送 `ready` 握手（含 `projection_versions`，远端据此判断是否需要校准显示）→ 置 `relayReady = true` → flush 缓冲的 Action。

约束：`relayReady` 前到达的 Action 进入有界缓冲，不处理；`relayReady` 前 UI 可读本地缓存，但不得视为在线可写。建立失败用**终态关闭码**区分是否重连（§4.7），不得静默降级。

### 4.7 keep_alive 心跳与关闭码

参照 chat §11：服务端周期性下发 `keep_alive`，客户端以 `pong` 回执；超时未回以 4501 关闭（页面隐藏等场景不在后台自动重连）。

| 关闭码 | 触发条件 | 客户端行为 |
|--------|---------|-----------|
| 4500 | 实例离线（`INSTANCE_OFFLINE`） | 停止自动重连，展示手动重试 |
| 4502 | 配置性永久失败（spawn 配置错误等） | 停止自动重连 |
| 4501 | keep_alive 超时 | 不在后台自动重连 |
| 1011 / 1013 | 通用失败 / 连接配额超限 | 退避重连 |

> 4004 已删除【审查：架构 P2-5】：chat 的 4004 对应「environment 不存在」，acp-hub 无此概念；不可恢复场景统一归 4502。

**判定性时间戳权威**【审查：架构 P2-4】：expiresAt（权限 5min）、心跳 30s 判定、取消 10s 超时等判定性时间戳**统一由 server 单一权威时钟生成与判定**；instance 只上报相对时序（seq），不参与判定。

### 4.8 MVP-M1 帧集收窄（minimal IDL）【顾问：P1-6】

§4.2 的帧模型是完整面；**M1 只实现下列最小帧集**，其余帧（`ysync.awareness`、`events/subscribe` 的 `from_seq` 重放、Web 面板路径）在对应里程碑才进入 IDL。`acp-hub-proto` 以「帧集白名单」形式定义：未列入白名单的 `t` 一律返回稳定错误（`UNSUPPORTED_FRAME`）并计数，不静默。

| M1 帧集（client ↔ server） | 说明 |
|---------------------------|------|
| `action`（chat/create、chat/prompt、chat/cancel、chat/close、permission/resolve） | events/subscribe、chat/load 帧**不进 M1**（chat/load 由 M2 载入；events/subscribe 由 M3） |
| `action_ack` / `action_error` | 完整错误码面保留，帧本身即 M1 面 |
| `ysync.subscribe` / `ysync.unsubscribe` / `ysync.update`（S→C 单向） | 状态同步面 |
| `ready` / `keep_alive` / `pong` / `auth` | 连接生命周期面 |

| M1 帧集（server ↔ instance） | 说明 |
|---------------------------|------|
| `instance/hello` / `instance/heartbeat` / `instance/event` / `instance/buffer_sync` / `instance/spawn` / `instance/kill` / `instance/spawn_ack` / `instance/kill_ack` / `instance/process_exit` | 全量（M1 即完整 instance 面） |

**M1 测试向量清单**【顾问：P1-6】（固化进 proto crate 的契约测试，见 §12）：

1. 连接握手：`auth` 错误 token → 断开；`ready` 前 Action 缓冲、`ready` 后 flush；
2. 幂等：同 `commandId` 重发 prompt → `duplicate` + 原 turnId；重发 cancel → `duplicate`；permission 重复应答 → `duplicate`；
3. 重放：同一 `instance/event` 流补推两次 → 视图无重复 entry/toolCall（§6.3 幂等键）；
4. 终态守卫：cancelled 后晚到 delta 丢弃；interrupted 后带序依据终态事件恰一次校准；
5. 崩溃恢复：kill -9 server → instance 缓冲 → 重启 → buffer_sync 追平（含 epoch 相同/变化两分支）；
6. 帧集白名单：未知 `t` → `UNSUPPORTED_FRAME`；客户端上行 `ysync.update` → 拒绝；
7. delivery_unknown【顾问2】：L2 后崩溃且无 L3 → 非幂等命令禁止盲重试（路径 B）；路径 A 的关联 ID 查询分支；
8. 双向认证【顾问2】：重放旧握手报文 / 错误角色 / 过期 challenge / 未知 instance 身份 → 拒绝并关闭连接 + 审计计数；
9. 错误脱敏【顾问2】：错误回显与日志在截断前剔除命令参数/env 值/认证材料；
10. outbox 归档保留【顾问2】：归档触发后，存在未裁决 outbox 记录的 chat 不丢失去重与恢复数据；
11. delivery_unknown 重启保留【顾问3】：跨进程重启注入——L2 已确认、L3 未确认 → 重启后该记录保持 delivery_unknown、可查询、非幂等命令**不自动再次投递**；幂等命令仅按 chat 标识安全重发；
12. HMAC 字节级向量【顾问3】：给定 nonce/context/version/role 的期望 MAC 输出；旧 challenge 重放、跨连接重放、错误角色、错误版本、过期 challenge、未知身份 → 拒绝并关闭。

---

## 5. 数据模型（Y.Doc schema）

### 5.1 核心裁决：规范化聚合视图而非原始事件

**ACP 事件经 ACPChannel 规范化为统一事件后，由聚合器有损投影到 Y.Doc；原始事件不进 Y.Doc。**（用户裁决 + chat §2.1 原则 6：「流式增量可丢、最终状态不可丢」）

依据：

1. ACP 事件是海量高频追加流（token 流、工具调用、中间消息），进 CRDT 文档必然膨胀到不可用；聚合是有损、有界的。
2. 聚合视图 = 多端（TUI/Web）真正消费的形态；TUI 不关心原始事件序列，只关心「当前 chat 长什么样」。
3. 需要完整事件流的客户端（IDE 类）走 `events/subscribe`，不经 yjs，协议语义不被污染。
4. CRDT 的并发合并能力只对「小、低频、多端一致」的数据有意义——视图对象恰好是这个形状。

### 5.2 文档拆分（chat §5.1）

| Doc | 名称 | 内容 | 更新频率 |
|-----|------|------|---------|
| Chat Doc | `chat:{chat_id}` | 消息时间线、内容块、工具调用、turn 投影 | 高频（内容流） |
| Control Doc | `control:{chat_id}` | 对话元信息、Agent 状态、能力、活动 turn、权限请求、agent 磁盘历史会话列表 | 低频（控制状态） |
| Registry Doc | `hub:registry` | instance 列表 + **活跃 chat 摘要列表**（全局视图，acp-hub 特有） | 低频 |

拆分理由（chat §5.1 同源）：**隔离高频内容流与低频控制状态**，降低订阅与同步成本。三份 Doc 都是 ACP 进程运行态的实时镜像，不是持久化恢复源；跨文档更新按 ACP 会话内事件顺序应用，不依赖跨 Doc transaction。

**chats 投影位职责裁决**【审查：架构 P1-5】：

- **Registry Doc `chats`** = TUI 对话列表的**唯一权威源**：活跃 chat 摘要（id/instance_id/title/status/gap/updated_at），由 **server 状态源单写**（chat 生命周期事件驱动：create/binding/终态/close 时更新），不从 Control Doc 聚合。
- **Control Doc `sessions`** = 该 ACP 进程的**磁盘历史会话列表**（`session_list` 10s 轮询投影，chat §5.3 语义在本架构下的正确对应——每 chat 一进程，返回的是 agent 侧历史），供 `chat/load`/resume 历史浏览，与 Registry 的活跃 chat 摘要**语义不同、互不替代**。
- §15 映射表该行标注差异（非「同构」）。

### 5.3 Chat Doc schema（chat §5.2）

结构版本 `CHAT_DOC_SCHEMA_VERSION = 1`（真相来源以 `acp-hub-proto` 实现为准）：

```rust
struct ChatDocRoot {
    schema_version: u32,
    projection_version: u32,        // 每次成功投影 +1；与 schema_version 分离（chat §5.4）
    entry_order: Vec<String>,       // Y.Array<String>，与 entries 分离便于局部更新/未来分页
    entries: Map<String, ChatEntry>,
    tool_calls: Map<String, ToolCallProjection>,
    // 无 committed_commands：去重记录在 server command outbox（§4.4），不随 Doc 生命周期存亡【顾问：P0-1】
}

struct ChatEntry {
    entry_id: String,               // 派生规则：`{turnId}:user` / `{turnId}:assistant` / tool: 按 toolCallId
    turn_id: Option<String>,
    kind: Message | Tool | System,
    role: User | Assistant | System,
    status: Pending | Streaming | Completed | Cancelled | Error,
    author_user_id: Option<String>,
    created_at: String,
    completed_at: Option<String>,
    block_order: Vec<String>,       // Y.Array<String>
    blocks: Map<String, ContentBlock>,
    error: Option<PublicError>,     // 脱敏公开错误，不含内部细节
}

enum ContentBlock {
    Text { block_id, text },                          // 流式文本用 Y.Text
    Reasoning { block_id, text, visibility: Summary | Hidden },  // hidden 内容绝不发给无权客户端
    ToolCall { block_id, tool_call_id },
    Resource { block_id, resource_id, media_type, name },        // 只存引用，不嵌入内容
}

struct ToolCallProjection {
    tool_call_id: String,
    turn_id: String,
    name: String,
    status: Pending | AwaitingPermission | Running | Completed | Error | Cancelled,
    arguments: Option<Value>,       // 过滤内部/敏感字段后投影
    result: Option<Value>,          // 仅在公开投影预算内保留
    result_omitted: Option<bool>,   // true=省略，false=明确未省略，None=旧记录未知
    result_bytes: Option<u64>,      // Hub 观测到的紧凑 JSON 字节数；不含内容
    public_error: Option<PublicError>,
    permission_id: Option<String>,
    started_at: Option<String>,     // Hub 观测时间；旧快照可空
    completed_at: Option<String>,
}
```

物理映射：根对象/`entries`/`blocks`/`tool_calls` 用 `Y.Map`；顺序索引用 `Y.Array<String>`；流式文本用 `Y.Text`（避免每个 token 替换完整字符串）；删除采用领域 tombstone，不由客户端物理删除权威记录。

### 5.4 Control Doc schema（chat §5.3）

```rust
struct ControlDocRoot {
    schema_version: u32,            // 旧快照恢复时以版本判空幂等补结构
    projection_version: u32,
    chat: ChatInfoProjection, // chat_id/title/status/active_turn_id/created_at/updated_at
    agent: AgentStatusProjection,   // instance_id/session_id/status/capabilities/last_activity_at/public_error
    active_turn: Option<ActiveTurnProjection>,  // turnId + turnStatus + updatedAt —— 权威，前端由 turnStatus 派生展示
    pending_permissions: Map<String, PermissionProjection>,
    sessions: Map<String, SessionSummaryProjection>,  // agent 磁盘历史会话列表（10s 轮询全量同步，旧条目删除自愈）——与 Registry Doc chats 语义不同（§5.2）
}

struct PermissionProjection {
    permission_id: String,
    turn_id: String,
    tool_call_id: Option<String>,
    title: String,
    description: Option<String>,
    options: Vec<AllowOnce | AllowSession | Deny>,
    status: Pending | Resolved | Expired,
    expires_at: String,             // server 权威时钟生成（§4.7）
    decision: Option<Allow | Deny>, // CAS 迁移成功后写入；expired 保持 null
}
```

### 5.5 Registry Doc schema（acp-hub 特有）

```rust
struct RegistryDocRoot {
    schema_version: u32,
    instances: Map<String, InstanceView>,   // id/hostname/status(online|offline|unknown)/token_id
                                          // /registered_at/last_heartbeat/chat_count
    chats: Map<String, ChatSummary>,// 活跃 chat 摘要（id/instance_id/title/status/gap/updated_at）
                                          // —— 唯一权威源，server 状态源单写（§5.2 裁决）
    global: { status: Healthy | Degraded | Restarting },  // Degraded 判定规则见 §17
}
```

### 5.6 写入边界与隔离

- **唯一提交边界 = DocManager**【审查：架构 P1-3 + 开发 P0-2】：所有 Y.Doc 写入（聚合器投影、控制面状态迁移如 cancelling/interrupted/decision/标题、定时器 CAS）都必须经 DocManager 的进程内单写通道（§7.4 每 chat 单写者）；任何路径不得绕过 DocManager 直写 yrs。去重记录**不进** Doc（§4.4 outbox）。
- **server-authoritative 写入权限**【顾问：P0-4】：Y.Doc 的写权限**只存在于 server 进程内**，客户端（TUI/Web）是纯 reader。据此：`ysync.update` 是 S→C 单向广播（§4.2）；**客户端上行 update / state vector 一律拒绝**（连接级计数 + 日志，不参与合并、不视为同步提示）；不采用 y-sync 双向增量握手——多 reader 场景下客户端无需贡献任何 CRDT 写入，同步 = server 快照 + 增量广播，天然规避客户端写冲突面（与 chat 的 YJS 双向模式不同，此为架构差异的正当理由）。
- **敏感信息不进 Y.Doc**（chat §5.3 同源）：密钥、内部错误、原始凭证、instance 连接信息、组织上下文不得进入文档；租户/角色上下文由服务端连接绑定提供，不由文档字段声明。
- **schemaVersion 与 projectionVersion 分离**（chat §5.4）：前者描述结构，后者描述镜像进度；服务端升级 schema 时对存活 chat 以幂等结构初始化补齐（旧客户端忽略未知字段仍安全）。
- **ViewStore 隔离范围**【审查：架构 P2-1】：`ViewStore` trait 只隔离聚合器；persist（update 重放）、gateway（快照推送）、broadcaster（`Y.mergeUpdates`）直接接触 yrs 类型。§14「yrs 生态风险可控」承诺**限于聚合器与 doc 生命周期管理**；其余接触点以封装函数（如 `encode_state_as_update`/`merge_updates_v1` 薄包装）收敛，不承诺 API 级隔离。

---

## 6. 聚合层（ACP → Y.Doc）

### 6.1 规范化边界：ACPChannel（chat §6.2）

**`ACPChannel` 是唯一协议边界，位于 server 侧。** instance 透明转发原始 ACP 帧；server 的 ACPChannel 将其规范化为统一事件（NormalizedEvent），聚合层只消费规范化事件，不接受私有帧类型。双格式兼容：原始 `{ type, payload }` 与 JSON-RPC `session/update`（含包裹格式）统一提取。

私有帧 → 规范化事件映射（chat §6.3 同源，按 acp-hub 需要的子集）：

| 原始帧 | 规范化事件 |
|--------|-----------|
| `agent_message_chunk` | `message_delta` |
| `agent_thought_chunk` | `reasoning_delta` |
| `user_message_chunk` / 服务端单写注册 | `user_message` |
| `prompt_complete` / `agent_message_complete` | `turn_completed` |
| `session_error` | `turn_failed` |
| `tool_call` / `tool_call_update`（按 status 细分） | `tool_call_started` / `tool_call_updated` / `tool_call_completed`；pending/in_progress 映射为权威非终态 |
| `permission_request` / `permission_response` | `permission_requested` / `permission_resolved` |
| `session_update` / `available_commands_update` | `session_updated` |
| `session_list` 响应 | `session_list`（agent 磁盘历史，全量同步投影） |

**出站翻译边界**（chat §6.2 同源）：`Translator` 把客户端 Action 翻译为 ACP JSON-RPC（`session/prompt` / `session/cancel` / `session/load` / `session/list` 等），`cwd` 由 server 按已认证上下文注入（§4.3 裁决），`rpcId` 由 server 分配（避免消息被当作 notification）。

**可信 binding**：server 维护 `chat_id → chat_id` 映射；ACP 帧携带的 sessionId 与 binding 不一致直接丢弃；`chat_id` 只用于协议投递，不能成为 Doc 名称/广播频道/缓存键（chat §6.2 规则 5）。

### 6.2 chat 创建时序（spawn → binding）【审查：架构 P0-3】

M1 核心闭环，新增正式时序（现有 router.rs 的「spawn → initialize → session/new → 注册映射」三步流程的分布式映射）：

```
客户端 chat/create
  → server 选 instance（显式/默认本机）
  → server 下发 instance/spawn { command_id, chat_id, cmd, cwd }
  → instance 拉起 ACP 进程
  → instance 上报 instance/spawn_ack { ok | error }
  → （spawn 失败 → action_error AGENT_UNAVAILABLE(retryable)，去重记录清除）
  → server 经 instance 转发 initialize（透传 JSON-RPC，instance 保持 dumb）
  → server 经 instance 转发 session/new
  → instance 上报 session 创建结果（session_id）
  → server 建立 binding（chat_id → chat_id）
  → 此后该 chat 的 ACP 帧才允许投影（binding 建立前到达的帧一律丢弃，§6.4 丢弃语义在此挂钩）
  → action_ack committed（携带 chatId）
```

各步超时与失败映射：spawn 10s、initialize 10s、binding 建立 30s（沿用现有超时配置）；任一步超时 → `AGENT_UNAVAILABLE`（retryable）+ 清理半创建状态（kill 已拉起进程）。binding 建立失败不产生幽灵视图。

### 6.3 幂等聚合规则（chat §6.3）

映射必须是幂等的：**重放同一 ACP 帧不重复创建 Entry、工具调用或权限请求**。聚合器以 `turnId` / `entryId` / `toolCallId` / `permissionId` 与终态状态机确定写入目标；缺少必要关联信息的帧拒绝投影并记录脱敏诊断（返回 `ApplyResult { applied, reason }`，纯投影无 I/O 无日志副作用）。

| 规范化事件 | Y.Doc 写入位置 | 聚合规则 |
|-----------|---------------|---------|
| 文本增量 | Chat Doc entry block | 追加（微批次合并，§6.4） |
| 思考/推理增量 | Chat Doc reasoning block | 按可见性写 `summary`/`hidden`，hidden 绝不发给无权客户端 |
| 工具调用开始/更新/完成 | Chat Doc `tool_calls` | 按 `toolCallId` upsert；状态与证据均单调迁移：缺省 arguments 不清空旧输入，普通更新/完成不可越过权限等待，首个终态拥有 result/error/completedAt 且不可被晚到帧覆盖；超大结果只记录省略事实与字节数 |
| 权限请求/决议/过期 | Control Doc `pending_permissions` + Chat Doc `tool_calls` | 官方 permission request 保留完整 `toolCall` 快照；即使先于普通 tool 通知到达，也在同一 seq 原子创建可达工具卡并进入 awaitingPermission。稍后正式通知只补全字段，不越过等待或重开终态；allow 恢复 running，deny/expire 进入 cancelled；旧事件缺快照或未知可选关联时仍不抑制权限请求 |
| 权限请求/解决/过期 | Control Doc `pending_permissions` | 按 `permissionId` upsert；决议写 `decision`（CAS，§7.4） |
| Agent status/capabilities/session info | Control Doc `agent`/`session` | 覆盖当前状态；能力未确认前保持不可用 |
| `session_list` 响应 | Control Doc `sessions` | agent 磁盘历史，全量同步（幂等，10s 轮询），响应中不存在的旧条目删除（自愈） |
| turn 终态（完成/失败/取消/中断） | Chat Doc entry + Control Doc active_turn | 终态立即写入；之后的同 turn 增量丢弃（**interrupted 例外见下**） |

**终态守卫（含 interrupted 校准例外）**【审查：架构 P0-2 + 开发 P0-1】：

- turn 处于 `cancelling` 或不可校准终态（completed/failed/cancelled）时，晚到增量一律丢弃——避免「已取消但还在输出」的中间态。
- **`interrupted` 是可校准终态**：仅允许同 `turnId` 且**带补推序依据**（`(chat_id, seq)` 单调，见 §8.5）的终态事件（`turn_completed`/`turn_failed`/`turn_cancelled`）将其**恰一次**迁移为实际终态；其余事件仍丢弃。守卫实现从「状态位判断」改为「状态位 + 重放序判断」。

### 6.4 顺序、微批次与事务边界（chat §6.4 + 审查修订）

- 同一 chat 的 ACP 帧按收到顺序进入独立有界缓冲区，**绝不与其他 chat 混批**；聚合器为**每 chat 串行消费者**【审查：架构 P1-1】。
- 文本与 reasoning 增量可在固定时间窗（默认 16ms）或字节阈值内合并；单个批次通过一次 Y.Doc transaction 写入。
- **控制类更新先 flush 再立即写入**：工具状态、权限、Agent status、错误、turn 终态及断链——保证用户看到的状态不倒退。
- 批次达到大小上限、等待超时或广播队列满时立即 flush。**广播背压改述**【审查：开发 P2】：yrs 的 `observe_update` 回调是同步的、不能 await，无法「提前感知背压」；Rust 侧在监听回调中把 update 经 channel 送出，背压只能作用于 broadcaster 队列——**广播队列满时合并 update（`merge_updates`）或跳过发送（客户端重连后经快照重同步兜底）**；广播失败只影响连接传递，不能阻塞 ACP 读取循环。
- 不对 token 逐条创建日志或 trace；仅在聚合窗口、工具/权限状态和 turn 终态形成可观测的状态变化。

**提交点纪律**【审查：架构 P0-1】：user entry 的写入顺序为「outbox 记录落盘 → 下发 ACP → L1+L2 投递确认 → 投影 user entry → committed Ack」（§4.4）；聚合器对 `user_message` 事件的幂等仍以 `turnId` 判定。

**旧 turn 未完成时新 prompt 的裁决**【审查：开发 P2】：对齐 chat `applyUserMessage`——旧 assistant entry 置 `cancelled`（不向 ACP 发 cancel），新 prompt 正常转发；ACP 侧旧请求的终态事件到达时因 turnId 不匹配被终态守卫拒绝，收敛于旧 entry 的 cancelled 状态。

### 6.5 单写与绑定（chat §6.5）

- DocManager 是唯一允许把 ACP 运行态写入 Y.Doc 的边界（§5.6）；客户端、旧实例、已解绑 ACP session 与未通过校验的帧都不能直接修改 YJS。
- **用户消息由服务端单写**：`chat/prompt` 处理时 server 注册 `turnId` 并创建 user entry（幂等：同 `turnId` 重放跳过），ACP 的 `user_message_chunk` 增量以此映射。
- binding 不存在、已解绑、ACP session 已断链时立即丢弃事件；**不得重新创建旧 Doc，也不得缓存给未来实例使用**。

---

## 7. 状态机与并发规则

### 7.1 instance 生命周期

```
         auth.hello 成功（含双向认证，§9.2）
  ┌───────────────────────────┐
  ▼                           │
REGISTERED ──► ONLINE ◄──┐    │
   ▲           │   ▲     │    │
   │           │   └─────┘    │
   │           │   心跳恢复    │
   │           ▼              │
   │        OFFLINE ◄─────────┘   (心跳超时 30s / 连接断开)
   └── 重连 (指数退避 1s→2s→4s…上限 60s)
```

- 心跳：instance 每 5s 发 `instance/heartbeat`；server 30s 未收到（可配置）→ 标记 `offline`。
- 重连：instance 侧自动指数退避重连，重连后 `instance/hello` 携带缓冲水位/`buffer_lost`/seq 状态，server 据此协调补推（§8.3）；**hello 是幂等替换**——新 hello 到达即 fencing 旧连接【审查：架构 P1-4】。
- **instance 离线即刻生效**【审查：开发 P1】：判定离线那一刻，该机**所有非终态 turn（accepting/running/awaiting_permission/cancelling）统一 → interrupted**，该 chat 所有 pending 权限**批量 expired**（复用 chat `expireTurnPermissions`），聚合器更新 yjs，所有 TUI 同步可见。

### 7.2 Turn 状态机（chat §8.1 + 审查修订）

```
accepting ──► running ──► completed
   │             │  ▲        │
   │             │  └── awaiting_permission ──► running (allow)
   │             ▼              │
   └─► failed   cancelling ◄────┘ (用户取消 / deny / expiry)
   ▲               │
   │   (任意非终态均可取消)【审查：开发 P2】
   └───────────────┼───────────┐
                   ▼           ▼           ▼
                cancelled   interrupted   failed
                (Agent确认)  (取消超时/    (Agent或系统错误)
                             连接丢失)
```

- **终态不可逆**（`interrupted` 除外——可被带补推序依据的终态事件恰一次校准，§6.3 守卫例外）。恢复执行必须创建显式的新 turn。
- `chat.active_turn`（turnId + turnStatus + updatedAt）是权威投影，前端由 `turnStatus` 派生展示状态；chat 级扁平 status 枚举不承担展示语义。
- `cancelling` 非终态但输出已停止——用户已取消，晚到增量一律丢弃（§6.3 终态守卫）。

### 7.3 chat 生命周期与分区恢复裁决【审查：架构 P0-2】

```
chat/create 或 load ──► accepting ──► ... （turn 状态机驱动）
                              │
        ACP 进程退出 ──► ended（终态，视图保留供历史查看）
        用户关闭 ──► closed
        进程崩溃 ──► crashed
        instance 断线 ──► 活动 turn → interrupted（turn 级终态）
```

**分区恢复裁决（P4 的实现语义）**：

- **`interrupted` 是 turn 级终态，不是 chat 级终态**。
- chat 级状态独立演进：instance 分区期间 chat 置 `gap`（补推缺口，§8.5）；**补推完成、seq 追平后清除 gap，chat 恢复可用，可开新 turn**——用户可在原 chat 继续对话。
- 补推完成后 `active_turn` 恢复规则：旧 turn 保持 `interrupted`（或按 §6.3 例外校准为实际终态），新 turn 正常投影。
- 若补推无法完成（缓冲丢失/缺口不可补）：chat 保持 `gap`，TUI 提示「载入以校准」（`chat/load` 显式重建），不假装完整。
- 每次用户输入 = 新 turn；`chat/prompt` 创建 turn（`accepting`），ACP 确认后 `running`。

### 7.4 并发规则（chat §8.2 + 审查修订）

1. 同一 chat 的命令按**有界队列严格串行**执行（上限默认 64，超出返回 `RATE_LIMITED`）；串行性由进程内队列保证。
2. 默认每 chat**仅一个活动 turn**；若未来支持并行 turn，必须先引入独立 branch/thread 聚合，不能直接放宽约束。
3. `commandId` 去重记录持久化（§4.4），覆盖客户端最大重试窗口与 server 重启。
4. **Permission resolution 使用 compare-and-set**：仅 `pending → resolved` 原子迁移一次，重复或过期回答返回幂等结果（`duplicate` ack）；迁移成功后才向 ACP 进程发 `permission.resolve`。
5. 标题更新等非 Agent 操作可独立排队，但仍经服务端命令写入；不能借 YJS client update 绕过授权。

**每 chat 单写者（Y.Doc 写入串行化）**【审查：开发 P0-2】：

- 存在三个写入路径：instance 事件聚合器（§6.4 独立任务）、command-coordinator 执行路径（user entry/权限 CAS）、权限超时定时器（CAS 迁移）。**yrs 的 `transact_mut()` 对同一 doc 的并发事务会 panic**（tokio 多线程下无互斥即崩溃）。
- 强制约束：**每 chat 一个 writer task**——聚合器、命令写入请求、定时器 CAS 请求全部经该 chat 的 mpsc 通道串行执行（`&mut DocPair` 独占）；等价实现为 per-chat `tokio::sync::Mutex<DocPair>`。
- 跨 Chat/Control 双事务顺序**固定 chat → control**；禁止跨 await 持有 Y.Doc 事务。
- 命令入队检查（outbox 去重索引 + 队列上限）与 `in_flight` 标记必须在**同一临界区**内完成（chat 靠 JS 单线程天然原子，Rust 无此保证），否则并发重发可绕过去重表。

### 7.5 instance daemon 崩溃故障卡片【审查：运维 P0-2】

| 步骤 | 行为 |
|------|------|
| daemon 崩溃 | ACP 子进程随 daemon 死亡（kill_on_drop）；缓冲（内存+磁盘）全部丢失；per-chat `stream_epoch` 递增（§4.5.1）【顾问：P0-2】 |
| instance 重启重连 | `hello` 上报 `buffer_lost: true` + 存活 session 清单 + 新 `stream_epochs` |
| server 裁决 | 对「已标记 interrupted 但 instance 声称存活」的 chat：**默认下发 `instance/kill` 清理孤儿进程**，Registry 标记「已清理」，TUI 可见；不得静默保留 |
| 验收 | M1 验收矩阵补「kill -9 instance daemon」演练（P9） |

### 7.6 instance offline 时 chat/close（pending_close）【审查：开发 P1】

- instance offline 时 `chat/close` 无法下发 `instance/kill` → 返回 `INSTANCE_OFFLINE`（retryable），视图标记 **`pending_close`**（Registry chat 状态）；
- instance 重连后 server 对 `pending_close` 集合自动补发 `instance/kill`，完成后清标记；
- 重连对账时 `alive_sessions` 与 server 已 closed 集合协调：server 对已 close 的 session 统一下发 kill（§8.3）。

---

## 8. 韧性设计

### 8.1 原则（chat §2.1 同源）

1. **server 无状态化**：server 只持有元状态（Y.Doc + instance 注册表），不持有任何 agent 运行态。server 崩溃 ⇏ agent 中断；TUI 崩溃 ⇏ 任何影响。
2. **传输至少一次，领域效果恰好一次**：客户端与 ACP 链路允许重发；server 通过 `commandId`（去重记录持久化，§4.4）、`turnId` 和状态机实现幂等（P8）。
3. **流式增量可丢、最终状态不可丢**：token delta 可合并；turn 终态、错误、取消、工具调用和权限决策必须可靠投影。
4. **慢消费者不能阻塞 Agent**：广播与 ACP 读取解耦；连接达到背压阈值后合并/跳过发送（§8.6），而不是无限缓存。
5. **数据权威顺序**：ACP 进程运行态（权威）→ Y.Doc（实时镜像）→ 消息（传递载体）。Y.Doc 不保存可跨 ACP session 恢复的旧投影，也不作为持久化真相。
6. **判定性时间戳由 server 时钟**（§4.7），instance 只上报相对时序。

### 8.2 断链语义矩阵

| 断链对象 | Y.Doc 处理 | 后续行为 |
|---------|-----------|---------|
| **TUI/前端断开** | 不对 session 执行任何清理 | ACP session 存活时，重连后同步当前实时 Doc（快照 + 增量追平） |
| **instance 网络分区**（agent 还活着） | 不删除 Doc；活动 turn → interrupted（可校准）；chat 置 gap | instance 重连后缓冲补推（§8.3）；seq 追平后清除 gap、chat 恢复可用（§7.3） |
| **instance daemon 崩溃** | 同上（agent 随 daemon 死）；hello 上报 buffer_lost | server 对「已中断但声称存活」的 chat 默认 kill 清理（§7.5） |
| **ACP 进程退出/被杀**（ended/closed/crashed） | 终态写入视图，Doc 保留供历史查看（归档策略见 §8.4） | 不再接受该 chat 的新事件；缓冲清理 |

### 8.3 server 崩溃 / 重启（P3）

1. server 崩溃瞬间：instance 检测到 ws 断开。
2. instance 上的 ACP 进程**继续运行**；daemon 将原始 ACP 帧写入本地缓冲（内存，超限溢出到磁盘；上限默认 10MB/万条，可配置）。**「产出不丢」是有界承诺**【顾问：P0-3】：缓冲上限内不丢；超限按 §8.5 丢弃策略丢弃（delta 优先、控制帧最后），并以 `gap` 结构化呈现缺口——**不承诺无限缓冲**，避免「10MB 与产出不丢」矛盾表述。
3. instance 以指数退避重连 server。
4. server 重启后：加载持久化 Y.Doc（§8.4）与 **command outbox（重建去重索引，§4.4）**【顾问：P0-1】→ 各 instance 重连 `instance/hello`（含存活 session 清单、缓冲水位、`buffer_lost`、`stream_epochs`）→ **epoch 相同**的 chat 按 `instance/buffer_sync` 补推（`from_seq = last_seq + 1`，环形滑窗兜底）；**epoch 变化**的 chat 判不可校准缺口（§4.5.1）【顾问：P0-2】→ ACPChannel 规范化 → 聚合器按 §6.3 幂等规则重放（重放安全由 turnId/entryId/toolCallId 幂等键 + interrupted 校准例外保证）→ 视图校准。
5. **恢复对账**【审查：运维 P1-7】：重连完成后再逐 session 比对 `alive_sessions` 与 Registry 状态，输出对账摘要日志（存活/缺失/意外存活）；意外存活的 session 按 §7.5 裁决（kill 清理），已 close 的 session 补发 kill；Chat Doc 级无法对账的置 gap 并在 TUI 提示「载入以校准」。
6. TUI 断线期间自行退避重连，重连后经 §4.6 时序秒级恢复（快照携带 projection_version，TUI 可显示「校准中」）【审查：开发 P1】。

### 8.4 Y.Doc 持久化规范【审查：运维 P0-3 + P1-4】

- **blob 格式**：每条 update blob 自描述（长度前缀 + CRC32）；回放遇损坏**尾部截断**（保留损坏段供诊断）并告警 + `degraded`。
- **fsync 节奏**：默认 per-commit fsync（`committed` Ack 必须在**对应 outbox 记录与投影 update 均落盘后**返回——§4.4 的 committed 语义与落盘时序绑定，outbox 同纪律）【顾问：P0-1】；可配置 batch 模式（如 1s 批量 fsync）作为优化，但 batch 模式下 Ack 语义相应降级为「已入持久层队列」，须在配置中显式声明。
- **落盘失败（磁盘满等）**：置 `degraded` + 结构化日志告警，**绝不静默**；server 停止接受新 `committed` 承诺（新 Action 返回可重试错误）。
- **compact 契约**：触发阈值（update 日志 > 64MB 或 > 24h）；原子流程：写临时全量快照（含 `last_applied_seq` 边界）→ fsync → rename → 截断旧日志；中途崩溃可回退到旧日志重放（新快照未 rename 前旧日志完整）。
- **数据目录磁盘预算**：日志 + compact 快照 + 缓冲文件总量上限（默认 2GB），超出触发告警 + 最旧 chat 归档。
- **归档临时默认值**【审查：运维 P1-4 + 顾问2：P1-1】：已结束/关闭 chat 的 Doc 保留 90 天（或按磁盘预算压缩/导出提示）；归档策略的正式方案为开放问题 3。**归档与 outbox 保留解耦**【顾问2：P1-1】：视图历史（update 日志/Doc 快照）可裁剪，但**命令账本（outbox）不得在存在未裁决记录、可恢复 instance 关系或仍开放控制面时删除**——删除前置条件：chat 关闭 + instance 注销/不再重连 + outbox 全终态（completed/failed/delivery_unknown 已裁决）+ 保留期届满，缺一不可；90 天归档不得独立触发 outbox 清理。
- 持久化路径：`~/.local/share/acp-hub/`（或平台对应目录），`0600` 权限。
- 注意：Y.Doc 是实时镜像，重放只用于恢复 server 自身视图；ACP 进程（权威）断链后不依赖旧 Doc 继续写入（§8.1 原则 5）。

#### 8.4.1 原子性边界与恢复不变量【顾问：P0-5】

**原子性边界（不承诺跨文件事务）**：

- 持久化单元是**单文件内的单条记录**：update 日志按追加顺序、outbox 按 commandId 索引、`(epoch, last_seq)` 独立小文件——三者之间**不提供跨文件原子性**（chat→control 双 Doc 投影同理）；
- 单条记录内部原子（长度前缀 + CRC32，§8.4）；跨文件一致性靠**恢复顺序**达成，不靠事务。

**恢复不变量（M1 启动修复逻辑的契约，按序执行）**：

1. **outbox 先于一切**：启动先重放 outbox 重建去重索引，再接受任何 Action——否则重启窗口内重发的 commandId 可能穿透去重；
2. **last_seq 对齐**：`(epoch, last_seq)` 文件先行加载，与 update 日志最后一条的序号核对；不一致（日志尾部损坏截断后 seq 倒退）以较小者为准并告警；
3. **Doc 补齐**：Y.Doc 重放后以 schema_version 判空幂等补结构（§5.6），不假设旧快照完整；
4. **instance 对账后开门**：Registry 恢复为 `unknown` 状态，instance 重连（hello）后才转为 online/offline 并触发 §8.3 对账；对账完成前 chat 视图允许展示但禁止新 `chat/prompt` 之外的控制操作（防基于未对账状态的错误指令）；
5. **任一不变量失败**：进入 `degraded`（§17.2），可继续服务只读视图，拒绝新 committed 承诺（§8.4 落盘失败语义同源）。

**降级行为**：chat 与 control 双 Doc 中仅一个成功落盘时，允许视图短暂不一致（chat 有内容、control 无 agent 状态），**下一个控制事件 flush 时收敛**（§6.4 控制类先 flush），不允许恢复逻辑把两个 Doc 当原子对处理。

### 8.5 缓冲与补推契约（审查修订）

- instance 缓冲按 chat 分桶，帧带单调 `seq`（instance 侧分配）与 `stream_epoch`（daemon 重启/进程重建后 +1，§4.5.1）；**daemon 重启后 seq 可重置**——此时 epoch 变化，补推契约失效，chat 判不可校准缺口，不再按 seq 补推旧流【顾问：P0-2】。
- 补推按 `(chat_id, epoch, seq)` 按序发送；**from_seq 起点 = server 持久化的 per-chat `last_seq + 1`（epoch 相同前提下）**；instance 环形滑窗（最后 500 条）兜底 server 崩溃前已收未落盘段。
- **补推纪律**【审查：架构 P1-1】：重连后 **instance 先排空 `buffer_sync` 再恢复实时转发**（instance 侧保证）；补推完成前聚合器对该 chat 暂停实时应用。seq 只承担补推路径排序，**实时路径排序 = server 到达序**；两路径经聚合器每 chat 串行消费者合并（§6.4）。
- **gap 升级为结构化标记**【审查：开发 P1】：`gap: { count: u32, last_seq: u64, uncalibratable?: bool }`（缺口帧数、断点、是否可校准）；补推完成、seq 追平后**清除**（由聚合器写回）；`uncalibratable` 由 epoch 变化触发（§4.5.1），只能经 `chat/load` 显式重建消除【顾问：P0-2】。
- 缓冲超限丢弃：**delta 类帧优先丢弃、控制帧/终态帧最后丢弃**【审查：运维 P1-6】；单帧大小上限（默认 1MB，超限直接跳过并记 gap）；「内存 + 磁盘合计 10MB」为计数口径。
- 缓冲文件权限 `0600`（内容是 chat 正文）；chat 结束/清理时同步删除缓冲文件。
- 分区期间的 HITL 类交互（如权限请求）悬挂在 instance 侧缓冲，恢复后补推；产品语义明示。

### 8.6 背压、超时与资源治理（chat §11）

- 每连接维护有界发送队列：超过软阈值（64 KB）合并 update（`merge_updates`）或跳过发送（客户端重连后经快照重同步）；超过硬阈值以可恢复错误关闭连接。
- 连接配额：`YJS_MAX_CLIENTS` 默认 200。
- 取消超时 10s → `interrupted`；权限请求超时默认 5min → `expired`；spawn 10s / initialize 10s / binding 30s（§6.2）。
- Action payload 上限 1 MB；每 chat 命令队列上限 64。
- **回放窗口**（10s）：`chat/load`/`resume` 转发时开启，承接先于 JSON-RPC result 到达的历史回放流；窗口外或 Chat Doc 已有内容时保持聚合层拒绝语义。
- 服务关闭时：停止接收新 Action → 完成或中断在途提交 → 释放引用 → 关闭连接。
- 所有阈值集中到配置（§16 默认值表）。

---

## 9. 安全模型

### 9.1 威胁与边界

- server 下发 spawn/kill 到 instance 等价于**远程代码执行**能力 → instance 侧必须认证 server，server 侧必须认证 instance。
- chat 内容（密钥/代码）会出现在 Y.Doc 与事件流中 → 客户端必须认证，陌生设备不可见。
- 局域网默认明文 ws：可接受（M1–M3 场景），M4 升级 wss。**M1–M3 期间以「默认监听绑定 + 不可信网络禁用」缓解**（§16）。

### 9.2 token 模型与双向认证

server 首次启动生成 token（存储于 server 配置目录，`0600`）：

| token | 用途 | 权限 |
|-------|------|------|
| **instance token**（每机器一个） | instance 注册与认证 | 收 spawn/kill 指令、上报事件/心跳 |
| **client token**（**按设备签发**）【审查：运维 P1-5】 | TUI/Web 连接 | 读 yjs 状态、发 Action、订阅事件流；**预留 read-only 档位**（M3 Web 只读面板用，§9.2.2） |

**连接级双向认证**【审查：架构 P1-6 + 运维 P0-1 + 顾问2：P0-2】：

1. instance 发起连接 → 发送 `instance/hello`（含 token + **一次性 challenge_nonce**，32B CSPRNG，每次连接新生成）。
2. server 校验 token（含协议版本、角色匹配）→ 响应 `HMAC(instance_token 派生密钥, challenge_nonce ‖ connection_context ‖ protocol_version ‖ role)` 作为 server 身份证明。
3. **instance 校验通过前不执行任何 spawn/kill**；校验失败即断开（关闭码 4502 + 审计计数）。
4. 该机制在明文 ws 阶段生效（防 ARP/DNS 劫持冒充 server）；M4 的 wss 是补充而非替代。

**协议级属性（防「只有 HMAC 名称、无协议约束」）**【顾问2：P0-2】：

- **挑战新鲜性**：challenge_nonce 单次使用，server 侧记录已用 nonce（短期有效窗口 30s 过期）；连接断开即失效；
- **连接绑定**：HMAC 输入绑定该连接唯一的 `connection_context`（如连接级随机 id），响应无法跨连接重放；旧连接被 hello 幂等替换 fencing（§4.5），重放旧握手报文无效；
- **角色与版本绑定**：HMAC 输入含 role 与 protocol_version，防止跨角色/跨版本重放；
- **身份隔离**：每 instance 独立 token/派生密钥（§9.2.1 按机器签发），instance 之间密钥不可互换；
- **密钥生命周期**：轮换走宽限期共存（§9.2.1），吊销即刻生效；
- **失败语义**：认证失败关闭连接 + 失败认证计数（§17.1）+ 结构化日志，不静默；
- **边界声明**：HMAC 只提供认证与完整性，**不提供机密性**——M4 公网部署以 wss/TLS 为强制边界，不得以 HMAC 替代传输加密。

**线格式精度（消除跨实现签名歧义）**【顾问3】：

- 算法：`HMAC-SHA256`，输出 base64（标准 RFC 4648，无填充歧义）；
- MAC 输入字段规范化：`challenge_nonce ‖ connection_context ‖ protocol_version ‖ role` 按**固定字节序**拼接（各自长度前缀 + UTF-8 编码；challenge/connection_context 为 32B 原始字节），字段顺序即文档顺序，不得重排；
- 比较：常量时间比较（`subtle` 或等价实现），杜绝时序侧信道；
- 密钥：`instance_token` 经 HKDF 派生单连接密钥（派生上下文含 role），token 本体不出现在 MAC 输入；
- 测试向量必须以**字节级**定义（给定 nonce/context/version/role → 期望 MAC 输出），跨实现可验证。

#### 9.2.1 token 运维流程【审查：运维 P1-5】

- 生成：32B CSPRNG；instance 侧存储同样 `0600`。
- 备份清单：token 文件 + Registry Doc（数据目录丢失 = 全机重新上牌）。
- **宽限期轮换**：新 token 与旧 token 共存（server 同时接受）→ 逐机切换 → 吊销旧 token。静态轮换 + 重启 = 全机锁定，禁止。
- 失败认证计数：结构化日志 + 指标（§17），泄露可检测。
- 视图对象只暴露 `token_id`，绝不暴露 token 本体。

#### 9.2.2 client token 分级【审查：架构 P2-8】

- `full`：读状态 + 发 Action（TUI 用）。
- `read-only`：仅读 yjs 状态与订阅事件流（M3 Web 面板用）。M1 即预留档位，避免 M3 复用 full token 造成可写暴露。

### 9.3 数据脱敏（chat §10 同源）

- 错误分为内部诊断错误与 `PublicError`：Y.Doc 与 `action_error` 只允许稳定、脱敏的公开信息。
- 日志只记录关联 ID、状态、耗时和大小，不记录消息正文、工具参数、token、密钥或原始凭证。
- 浏览器输入、ACP 事件、工具参数与资源 URL 全部视为不可信；进入领域事件前执行 schema、大小、编码校验。
- **错误截断按信任边界与脱敏裁决，而非仅字节数**【顾问2：P1-2】：`action_error` 与 PublicError 只携带稳定错误码 + allowlist 摘要字段（状态/耗时/大小），**截断前先执行敏感字段剔除**——命令参数、env 值、认证材料、ACP 原始输出中的未授权内容不得进入错误回显；端到端长度上限（1MB/4KB）是容量约束，不替代脱敏规则。
- 对消息发送、连接、同步流量、工具调用和权限应答分别限流。

### 9.4 审计最小集【审查：运维 P2-3】

§1.3 排除完整审计，但**保留结构化操作日志**（动作类型/commandId/token_id/结果/耗时，天然由 §9.3 日志规范承载）作为未来审计基础，零成本。

### 9.5 M1 身份与授权边界【顾问：P1-7】

M1 的授权模型**显式收窄**，避免在设计期承诺多用户能力：

- **token 即身份**：M1 无用户概念；一个 token = 一个身份（instance 或 client），无账号、无会话登录、无用户级 ACL；
- **授权面 = token 角色**：`instance` 角色可收 spawn/kill 指令；`full` client 角色可读全部 Doc + 发 Action；`read-only` client 角色仅读（§9.2.2）。**没有 chat 级细粒度授权**——持有 client token 即可访问 server 上全部 chat（单用户本地/家庭局域网前提下的显式取舍）；
- **非 loopback 拒绝策略**：M1 默认监听 `127.0.0.1`；显式配置局域网监听（M2）时，配置文件中声明 `allow_non_loopback: true` 才接受非回环连接——默认拒绝，防误暴露（§16）；
- **边界声明**：多用户、配额、chat 级共享、审计合规均不在 M1–M3 范围（§1.3），演进到 M4 公网前必须重新评估授权模型，**不得在现有模型上打补丁**。

### 9.6 spawn.env 白名单【顾问：P1-7】

`instance/spawn` 的 `env` 参数（客户端 `chat/create` 可间接传递）**不开放任意键**：

- server 维护 **env 白名单**（默认空 = 仅继承白名单基集，如 `PATH`/`HOME`/`LANG`；配置可增补键名，§16）；
- 白名单外的键一律拒绝（`INVALID_STATE` 错误，非静默丢弃）——防止经 env 注入 `PERI_*`/`LD_PRELOAD` 等敏感覆盖；
- 白名单仅约束键名，值仍按 §9.3 不可信输入校验（长度上限、编码）；
- instance 侧对 spawn 指令携带的 env 再校验一次白名单（双端校验，防 server 配置漂移）。

---

## 10. TUI 视图层（acp-hub-tui）

### 10.1 定位

- 纯 client：连接 server（单 ws 多路复用），本地维护 **Y.Doc 只读镜像**（server-authoritative，不上行 update，§5.6）【顾问：P0-4】，渲染源 = Chat Doc / Control Doc / Registry Doc；操作 = Action/Ack。
- 与 peri-tui 视觉风格一致（ratatui + kit 风格），独立实现，不依赖 peri crate。
- TUI 崩溃零影响（P1），多 TUI 并存（P2）。

### 10.2 界面结构与数据源

| 区域 | 数据源 | 说明 |
|------|--------|------|
| 实例列表 | Registry Doc `instances` | 在线/离线、chat 数 |
| 对话列表 | **Registry Doc `chats`（唯一权威源，§5.2 裁决）** + Control Doc `sessions`（agent 磁盘历史，resume 浏览） | 状态、instance、标题、`gap` 徽标（含缺口计数） |
| 对话详情 | Chat Doc `entries`/`tool_calls` + Control Doc `active_turn` | 消息视图 + 工具卡片 + 权限请求；订阅经 `ysync.subscribe`（§4.2） |
| 状态栏 | `keep_alive` / 连接状态 | 连接状态、重连中指示、校准中指示（projection_version，§4.6） |

### 10.3 断线恢复

- 重连 → `auth` → 快照推送（含 projection_version）+ `ready` 握手（§4.6）→ 增量追平。元状态小（KB 级），秒级恢复；追平期间显示「校准中」。
- 原始事件订阅（`events/subscribe`）是独立流，不阻塞视图恢复。

---

## 11. 与 peri 的关系与边界

| 事项 | 决策 |
|------|------|
| 耦合点 | 仅 ACP 协议线格式（JSON-RPC over stdio）与 InitializeResponse 能力协商 |
| instance 上的 ACP 进程 | 默认 `peri acp`，可配置为任意符合 ACP 的 server |
| 依赖方向 | acp-hub 三个二进制均不依赖 peri crate；`acp-hub-proto` 独立 |
| stdio 路径 | peri 侧 stdio host（3.0）不受影响、不合并；hub 独立演进 |
| e2e | 独立测试矩阵：假 ACP 进程（现有 test-child 模式）+ 真 `peri acp`；不进入 peri 的 e2e 基建 |

---

## 12. 工程结构（建议，参照 chat-channel 目录）

```
acp-hub/
├── Cargo.toml            # workspace（独立于根 workspace 或保持现有成员地位）
├── proto/                # acp-hub-proto：帧/Action 信封/instance 协议/Y.Doc schema 类型镜像
├── server/               # acp-hub-server
│   ├── src/protocol/     # acp-channel（入站规范化）、translator（出站 action → ACP JSON-RPC）
│   ├── src/state/        # aggregator（幂等投影）、chat-writer（doc 写入原语）、
│   │                     #   doc-manager（doc 生命周期+微批次+唯一提交边界）、factory、
│   │                     #   permission（CAS）、session-list（轮询全量同步）、view-store（内部实现细节，仅隔离聚合器，§5.6）
│   ├── src/channel/      # gateway（ws 生命周期）、chat-channel（action 归一化）、
│   │                     #   command-coordinator（串行队列+commandId 去重持久化）、
│   │                     #   relay-event-handler（instance 入站）、broadcaster（fan-out+背压）、
│   │                     #   connection-registry（配额）
│   └── src/persist/      # update 日志（blob+CRC32+compact）+ command outbox（§4.4）+ (epoch, last_seq) 水位
├── instance/              # acp-instance：child.rs（现有资产迁入，补进程组 kill）、缓冲、重连
├── tui/                  # acp-hub-tui：视图层
└── docs/architecture.md  # 本文档
```

测试沿用仓库规范：单元测试 `*_test.rs` 同目录、集成测试 `tests/`。

**测试前提**【审查：开发 P2】：

- 聚合器 P0 契约测试（幂等/终态守卫含 interrupted 校准/gap）为**纯函数测试**：内存 Y.Doc + `fn apply(&mut DocPair, &NormalizedEvent) -> ApplyResult`，与 chat 测试形态一致，无需假连接；
- 控制面协议层测试需先抽象 ws 为 trait（如 `WsSink`，定义于 proto crate 或 server 内部），用假连接对象（chat ADR 决策 7 同源）；
- 16ms 微批次与心跳/超时测试需 `tokio::time::pause`（test-util feature）。

---

## 13. 演进路线

| 里程碑 | 范围 | 验收 |
|--------|------|------|
| **M1 本机闭环** | server + instance 同机 + tui + 三 Doc + token（含双向认证）+ 断线韧性 + **部署包** | P1–P9 全绿；TUI 崩溃重启不影响 agent；双 TUI attach 一致；**kill -9 server / kill -9 instance daemon 演练**【审查：运维 P1-1】；**§4.8 测试向量 1–10 全绿**【顾问2】 |
| **M2 局域网** | instance 部署到第二台机器、心跳/离线/重连、实例列表 UI、显式调度、**可观测性指标落地** | 断网 → turn interrupted 呈现 → 重连缓冲补推校准 → chat 恢复可用；gap 计数可见 |
| **M3 多端** | Web 只读面板（read-only token）、`events/subscribe` 稳定、awareness | 浏览器与 TUI 视图一致 |
| **M4 公网** | wss、token 管理/轮换 UI、限流 | 公网远程连接安全基线 |

每里程碑独立验收，不互相阻塞。

### 13.1 部署包（M1 验收项）【审查：运维 P1-1 + P2-6】

- systemd unit / launchd plist 示例：`Restart=on-failure`、SIGTERM 优雅关闭对接 §8.6；
- `acp-hub-server status` 子命令（或 Unix socket 健康端点），供进程管理器探测存活；
- 日志轮转约定：stderr 输出 + 外部轮转（logrotate/journald 限额）；
- **升级流程**：先升 server、后升 instance；`instance/hello` 携带 `protocol_version`（proto crate 定义），版本不匹配拒绝连接并报错，替代人肉纪律。

---

## 14. 风险与开放问题

| 风险 | 说明 | 对策 |
|------|------|------|
| yrs 生态成熟度 | Rust yjs 实现 API 变动 | 聚合器与 doc 生命周期经 `ViewStore` 隔离（内部实现细节）；其余接触点薄封装收敛（§5.6 隔离范围）；schema_version 版本化 |
| 事件序与聚合一致性 | 重连补推乱序/重复 | 补推纪律（排空后恢复实时）+ `(chat_id, seq)` 按序 + turnId/entryId/toolCallId 幂等 + interrupted 校准例外 + gap 计数（P0 契约测试固化） |
| 缓冲无限增长 | instance 断线期间事件堆积 | 内存 + 磁盘溢出 + 上限丢弃（delta 优先）+ gap 呈现 |
| 局域网 token 泄露 | 明文 ws + 文件权限 | token 0600 + 双向认证 + 失败认证指标；M4 wss 升级 |
| 多 TUI 写冲突 | 多端同时操作 | Action 有 Ack 与 commandId 幂等（去重持久化）；Y.Doc 仅 server 经 DocManager 单写 |
| Y.Doc 膨胀 | 长 chat 消息堆积 | 有损聚合（截断/摘要）+ 资源引用 + 归档（90 天临时默认） |
| 落盘与 Ack 一致性 | committed 先于落盘 → 崩溃丢失已确认状态 | per-commit fsync 默认 + Ack 在落盘后返回（§8.4） |
| 去重记录随 Doc 生命周期失效 | compact/归档裁剪导致去重表缺口 → 重发穿透 | 去重记录移出 Y.Doc，独立 command outbox（§4.4）【顾问：P0-1】 |
| 补推边界歧义 | daemon 重启后旧流残余与新流无法区分 | stream_epoch 代际标识 + 不可校准 gap（§4.5.1）【顾问：P0-2】 |
| 多文件持久化不一致 | chat/control 双 Doc 与 outbox 无跨文件事务 | 恢复不变量顺序修复 + degraded 降级（§8.4.1）【顾问：P0-5】 |
| L3 未知状态盲重试 | L2 后 ACP 侧状态未知时自动重发 → 重复外部副作用 | delivery_unknown 状态 + 非幂等命令禁止盲重试（§4.4）【顾问2：P0-1】 |

开放问题（排期时确认）：

1. ~~心跳间隔与离线判定阈值~~ → 已入配置默认值表（§16，5s/30s 可配置）。
2. `ai` 消息聚合截断长度（建议 4KB）是否按端区分（Web 可折叠全文）。**裁决方向**【顾问2：P1-2】：长度上限按端保留，但脱敏优先于截断（§9.3）；正式方案排期确认。
3. 归档策略正式方案（临时默认：结束 chat 保留 90 天 / 磁盘预算压缩，§8.4）。**裁决方向**【顾问2：P1-1】：正式方案必须与 outbox 保留解耦——视图历史可裁剪，命令账本按未裁决状态保留（§8.4）。
4. `projection_version` 乐观并发校验（`VERSION_CONFLICT`）：字段与错误码已预留，**M1 不强制校验**（与 §4.4 对齐）。**裁决方向**【顾问2：P1-3】：M1 依赖服务端实际状态 + 终态守卫 + commandId 幂等；**引入并发客户端、可编辑投影或改变命令含义的操作之前，强制执行 `projection_version` 并测试 `VERSION_CONFLICT`**——单用户不等于单连接，重连后的旧 TUI 镜像可能基于过期投影发指令。

---

## 15. 参考实现映射（chat-channel → acp-hub）

> 参考路径：`/Users/konghayao/code/pazhou/remote-control-server/packages/chat-channel/`（实现基线 `docs/arch/19-yjs-chat-streaming.md`）。

| chat-channel 组件 | acp-hub 对应 | 差异说明 |
|------------------|-------------|---------|
| `protocol/acp-channel.ts`（入站规范化） | `server/src/protocol/acp-channel` | 同构；输入来自 instance 转发的原始 ACP 帧而非直接 relay |
| `protocol/translator.ts`（出站翻译） | `server/src/protocol/translator` | 同构；`cwd`/`rpcId` 由 server 注入 |
| `state/aggregator.ts`（幂等投影） | `server/src/state/aggregator` | 同构（turnId/entryId/toolCallId/permissionId 幂等键、纯投影）；**新增 interrupted 校准例外**（chat 无此语义，§6.3） |
| `state/chat-writer.ts`（doc 写入原语） | `server/src/state/chat-writer` | 同构 |
| `state/doc-manager.ts`（doc 生命周期+16ms 微批次） | `server/src/state/doc-manager` | 同构；acp-hub 增加 Registry Doc 与每 chat 单写者约束（§7.4） |
| `state/permission.ts`（权限 CAS） | `server/src/state/permission` | 同构 |
| `state/session-list.ts`（10s 轮询全量同步） | `server/src/state/session-list` | **差异**：投影目标是 agent 磁盘历史（§5.2 裁决），与 chat「实例级对话列表」语义不同；活跃 chat 列表由 Registry 单写 |
| `channel/gateway.ts`（ws 生命周期/快照时序/keep_alive） | `server/src/channel/gateway` | 同构；补 `ready`/`pong`/`ysync.subscribe` 帧 |
| `channel/command-coordinator.ts`（串行队列+commandId 去重） | `server/src/channel/command-coordinator` | 同构；**去重记录持久化到 command outbox**（chat 为进程内 Map；acp-hub 跨 server 重启有效，§4.4）【顾问：P0-1】 |
| `channel/relay-event-handler.ts`（入站消费+断链清理） | `server/src/channel/relay-event-handler` | 入站源从 relay 变为 instance ws；断链语义差异见 §8.2 |
| `channel/broadcaster.ts`（fan-out+64KB 背压） | `server/src/channel/broadcaster` | 同构 |
| `channel/connection-registry.ts`（配额） | `server/src/channel/connection-registry` | 同构 |
| `persist/redis.ts`（Redis 快照 CAS） | `server/src/persist`（本地 update 日志） | **差异**：M1–M3 单节点无需 Redis；blob+CRC32+compact 本地实现（§8.4） |
| `transport/ws.ts`（前端同构 WS 客户端） | `tui/src/transport` + `acp-hub-proto` | 同构 |
| Chat Doc / Control Doc schema | 同 schema（§5.3/§5.4） | Registry Doc 为 acp-hub 新增；**Chat Doc 不含去重记录**（acp-hub 去重记录在 outbox，§4.4）【顾问：P0-1】 |
| 事件日志体系/租约 | 不实现 | 同 chat Q5 评审决策；去重持久化替代进程内 Map |
| 4004 关闭码 | 已删除 | chat 对应 environment 概念，acp-hub 无（§4.7） |

**已吸收的核心设计原则**（chat §2.1）：服务端单写、YJS 是实时状态投影不是命令总线、权威数据在 ACP 进程侧、传输至少一次/效果恰好一次、流式增量可丢/最终状态不可丢、慢消费者不阻塞 Agent。

---

## 16. 配置（新增章节）【审查：运维 P1-2】

配置来源优先级：**CLI > 环境变量 > 配置文件（`~/.config/acp-hub/config.toml`）> 默认值**。

| 项 | 默认值 | 说明 |
|----|--------|------|
| 监听地址 | `127.0.0.1` | M1 本机；M2 局域网显式配置为局域网地址/0.0.0.0（**明文 ws 暴露面由此决定**） |
| 监听端口 | `8456` | 可配置 |
| 数据目录 | `~/.local/share/acp-hub/` | 0600 |
| 配置/token 目录 | `~/.config/acp-hub/` | 0600 |
| 心跳间隔 / 离线判定 | 5s / 30s | §7.1 |
| 缓冲上限（内存+磁盘合计） | 10MB / 万条 | §8.5 |
| 单帧大小上限 | 1MB | §8.5 |
| 命令队列上限 | 64 | §7.4 |
| 连接配额 | 200 | §8.6 |
| 发送背压软/硬阈值 | 64KB / 128KB | §8.6 |
| 微批次窗口 | 16ms | §6.4 |
| 回放窗口 | 10s | §8.6 |
| 权限请求超时 | 5min | §7.1 |
| 取消超时 | 10s | §7.1 |
| spawn / initialize / binding 超时 | 10s / 10s / 30s | §6.2 |
| fsync 模式 | per-commit | batch 模式需显式声明并降级 Ack 语义（§8.4） |
| compact 触发 | > 64MB 或 > 24h | §8.4 |
| 磁盘预算 | 2GB | §8.4 |
| 归档保留 | 90 天 | §8.4 |
| 缓冲环形滑窗 | 500 条 | §8.5 |
| spawn env 白名单 | 空（仅继承基集） | §9.6；键名白名单，白名单外拒绝 |
| 非回环监听开关 | `allow_non_loopback: false` | §9.5；显式声明才接受非回环连接 |

（开放问题 1 由此表解决，删除 v2.0 中「5s/30s 先固定」的表述矛盾。）

---

## 17. 可观测性与 SLO（新增章节）【审查：运维 P1-3】

### 17.1 指标清单（tracing 字段来源，结构化日志可聚合）

| 类别 | 指标 |
|------|------|
| 连接 | 在线连接数、认证失败次数（按 token_id）、重连率、心跳超时次数、背压断连次数 |
| instance | 在线/离线数、缓冲水位（每 instance）、**缓冲溢出字节数与丢弃帧数（delta/控制帧分类）**、buffer_lost 次数 |
| 同步 | 初始同步耗时、gap 计数与缺口帧数、补推重放耗时与 backlog、实时/补推乱序丢弃数 |
| 聚合 | 聚合队列深度、微批次延迟、晚到丢弃数、去重命中数 |
| 持久化 | 落盘失败次数、fsync 耗时、compact 次数与耗时、**数据目录磁盘占用** |
| 对话 | chat 创建/binding 成功率与耗时、turn 终态分布、权限请求/过期/决议计数 |

### 17.2 Degraded 判定规则（Registry Doc `global.status`）

以下任一触发 `Degraded`：落盘失败 / 缓冲溢出丢弃 / 任一存活 chat 存在 gap / 镜像失败（聚合器异常）/ **启动恢复不变量失败（§8.4.1）**【顾问：P0-5】。`Restarting` 仅在 server 启动回放期间。判定规则集中实现于 server 状态源，TUI 状态栏呈现。

### 17.3 最小 SLO（对照 chat §12.2 缩放到本地单节点）

| SLO | 目标 |
|-----|------|
| 视图恢复（断线重连到可交互） | P95 < 2s |
| 已缓冲帧补推不丢 | 100%（缓冲未溢出前提下） |
| 已提交消息丢失率 | 0（per-commit fsync 下） |

---

## 附录 A：对抗面试与审查决策记录

| # | 议题 | 裁决 |
|---|------|------|
| 1 | 核心场景 | 本机常驻 + 局域网扩展；公网/多用户后置 |
| 2 | 多端定义 | 多 TUI + 未来 Web 面板；IDE 走 ACP 协议不进 yjs |
| 3 | TUI 操作权 | 可操作；控制走 Action/Ack（请求-响应），视图走 yjs |
| 4 | 项目定位 | acp-hub 是独立项目；与 peri 唯一耦合 = ACP 进程 |
| 5 | 二进制形态 | server 与 instance 两个独立二进制（共享 proto crate） |
| 6 | instance 接入 | instance 主动 outbound 连接 + token 注册 + 心跳 |
| 7 | 断线语义 | instance 断线 → 其上 chat 标记 interrupted，绑定不迁移 |
| 8 | chat 调度 | 显式指定 instance + 默认本机 |
| 9 | yjs 边界 | ACP 事件聚合为视图对象进 yjs；原始事件走独立订阅 |
| 10 | 通道组织 | 单 ws 连接多路复用（Action/Ack + y-sync） |
| 11 | 认证模型 | server 签发 token，instance/client 角色分权 |
| 12 | server 韧性 | server 崩溃 agent 继续跑；instance 缓冲补推 |
| 13 | 聚合参考实现 | 聚合逻辑参照 `@fenix/chat-channel`（用户指定） |
| 14 | 文档拆分 | 每 chat 双 Doc（Chat/Session）+ 全局 Registry Doc（chat §5.1 同源） |
| 15 | 控制面语义 | Action/Ack 两阶段 + commandId 幂等 + 稳定错误码（chat §7.1） |
| 16 | 幂等与终态 | turnId/entryId/toolCallId/permissionId 幂等键 + turn 终态不可逆（chat §6.3/§8.1） |
| 17 | interrupted 校准【审查】 | interrupted 为可校准 turn 级终态；chat 级 gap 可恢复（§7.3） |
| 18 | 去重持久化【审查】 | committed 记录入 server command outbox（v2.1 曾裁决入 Session/Chat Doc，v2.2 由顾问 P0-1 推翻——Y.Doc 是可丢弃镜像，去重事实必须独立于 Doc 生命周期）；提交点纪律（§4.4） |
| 19 | 单写者与提交边界【审查】 | 每 chat writer task 串行化；DocManager 为唯一提交边界（§5.6/§7.4） |
| 20 | 双向认证【审查】 | instance hello 携带 nonce，server 以 HMAC 应答；校验前不执行指令（§9.2） |
| 21 | instance 崩溃语义【审查】 | 缓冲不跨重启 + buffer_lost 上报 + 孤儿进程默认 kill 清理（§7.5） |
| 22 | command outbox【顾问】 | 去重记录移出 Y.Doc 独立持久化；delivery_confirmed 三级（L1 ws/L2 stdin/L3 ACP）；崩溃点×重试行为表（§4.4） |
| 23 | stream_epoch【顾问】 | per-chat 流纪元，daemon 重启/进程重建 +1；epoch 相同补推 last_seq+1，变化 → 不可校准 gap（§4.5.1） |
| 24 | P3 有界保证【顾问】 | 「产出不丢」改写为缓冲上限内不丢 + 溢出按策略丢弃 + gap 呈现，不承诺无限缓冲（§8.3/§8.5） |
| 25 | server-authoritative yjs【顾问】 | ysync.update 单向 S→C；客户端上行拒绝；不采用双向 CRDT 握手，同步 = 快照+增量广播（§5.6） |
| 26 | 恢复不变量【顾问】 | outbox 先行 → last_seq 对齐 → Doc 补齐 → instance 对账后开门；任一失败降级 degraded（§8.4.1） |
| 27 | minimal IDL 与授权收窄【顾问】 | MVP-M1 帧集白名单 + 测试向量（§4.8）；M1 token 即身份 + spawn env 白名单 + 非回环默认拒绝（§9.5/§9.6） |
| 28 | HMAC 双向认证保留【顾问，否决删减】 | advisor 建议删除（链路级信任替代），被否决——server→instance 验证在明文 ws 阶段防冒充，M4 wss 是补充非替代（§9.2） |
| 29 | delivery_unknown【顾问2】 | L3 依赖 peri ACP 关联 ID 能力，M1 前二选一裁决：路径 A（支持）关联 ID 查询 / 路径 B（不支持）非幂等命令禁止盲重试 + 对账/人工；未裁决前按路径 B 实现（§4.4） |
| 30 | HMAC 协议级规范【顾问2】 | 双向随机 challenge + 连接绑定 + 单次使用窗口 + 角色/版本绑定 + 身份隔离 + 轮换路径；HMAC 不提供机密性，M4 强制 TLS（§9.2） |
| 31 | 归档与 outbox 解耦【顾问2】 | 视图历史可裁剪；命令账本按未裁决状态保留，删除前置条件四合一（chat 关闭 + instance 注销 + outbox 全终态 + 保留期届满，§8.4） |
| 32 | 错误截断脱敏优先【顾问2】 | 错误码 + allowlist 摘要字段；截断前剔除敏感字段（§9.3） |
| 33 | projection_version 强制时机【顾问2】 | M1 不强制；引入并发客户端/可编辑投影前强制执行并测试 VERSION_CONFLICT（§14 开放问题 4） |
| 34 | 决策门禁与幂等分类【顾问3】 | 关联 ID 确认从开工门禁降为发布前决策门禁（路径 B 兜底开工）；未分类命令默认禁止自动重发（§4.4） |
| 35 | delivery_unknown runbook【顾问3】 | 裁决入口/权限/依据状态/三种迁移结果/审计记录；可查询可持久化可展示，不静默丢弃（§4.4） |
| 36 | HMAC 线格式精度【顾问3】 | HMAC-SHA256 + 固定字节序 MAC 输入 + 常量时间比较 + HKDF 派生；字节级测试向量（§9.2） |
