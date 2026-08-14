# F1 设计：acp-hub-proto 协议 crate

> 状态：设计稿（对应 Feature F1）
> 日期：2026-08-07
> 权威来源：`docs/architecture.md`（v2.4）§4.1/§4.2/§4.3/§4.3.1/§4.4/§4.5/§4.5.1/§4.7/§4.8/§5.3/§5.4/§5.5/§9.2/§9.2.2/§9.5/§16
> 约束：**忠于架构文档，不引入文档外的协议**。文档未指明处的命名/参数选择均标注「【决策】」并给出依据；文档明确的字段/枚举/语义照抄。

---

## 1. 目标与范围

`acp-hub-proto` 是三个二进制（server / machine / tui）共享的协议 crate，承载：

1. 全部线协议类型：帧模型（Frame）、Action/Ack 信封、machine 协议帧、连接生命周期帧、y-sync 帧 envelope；
2. Y.Doc schema 的 Rust 类型镜像（Chat/Session/Registry 三 Doc，§5.3–5.5）；
3. M1 帧集白名单机制（§4.8）；
4. HMAC 双向认证密码原语（§9.2 顾问3 线格式精度，纯函数、无 I/O）；
5. 版本常量与协议参数常量（§4.1/§5.3/§13.1/§16）。

**边界声明**（不属于 proto，定义在 server）：

- `NormalizedEvent`（ACPChannel 产物，§6.1）——定义在 `server/src/protocol/acp-channel`；proto 的 `event` 帧仅承载 envelope（`session_id/seq/frame`），`frame` 为不透明 JSON。
- command outbox 记录与状态机（`received → … → completed`，§4.4）——server 内部持久化语义，**不暴露给客户端**（§4.4 原文），定义在 `server/src/persist` + `server/src/channel/command-coordinator`；proto 只承载其线协议投影（`AckStatus`、`retryable`、`turnId`）。
- delivery 三级（L1/L2/L3）——达成条件属 server 执行语义，无独立线类型。
- server 运维配置（监听地址、数据目录、超时、fsync 等，§16）——定义在 `server/src/config`；proto 仅提供 §16 中**协议参数**的默认值常量（见 §11）。

## 2. 模块划分总览

```
proto/src/
├── lib.rs            # crate 文档 + 顶层 re-export（公开面收敛）
├── version.rs        # 版本常量：PROTOCOL_VERSION、CHAT_DOC_SCHEMA_VERSION、Y_UPDATE_ENCODING_VERSION 等
├── frame.rs          # Frame 枚举（serde tag "t"）+ FrameTag + parse 入口 + ProtoError
├── action.rs         # ActionEnvelope（tag "type"）+ 各 payload 结构
├── ack.rs            # AckStatus / ActionAck / ActionError / ErrorCode
├── event.rs          # EventFrame（S→C 事件推送 envelope，§4.3.1）
├── machine.rs        # machine 协议 9 帧（hello/heartbeat/event/buffer_sync/spawn/kill/spawn_ack/kill_ack/process_exit）
├── conn.rs           # Auth / AuthResponse / Ready / KeepAlive / Pong / DocId / CloseCode
├── ysync.rs          # YsyncSubscribe / YsyncUnsubscribe / YsyncUpdate / YsyncSync / YsyncAwareness
├── whitelist.rs      # 帧 tag 注册表 + M1 白名单 + 方向约束（§4.8）
├── hmac.rs           # 双向认证原语：MAC 输入规范化 / HKDF 派生 / 计算与常量时间校验（§9.2）
└── schema/           # Y.Doc schema 类型镜像（§5.3–5.5）
    ├── mod.rs        # 跨 Doc 公共枚举 + PublicError
    ├── chat.rs       # ChatDocRoot / ChatEntry / ContentBlock / ToolCallProjection
    ├── session.rs    # SessionDocRoot / SessionInfoProjection / AgentStatusProjection /
    │                 #   ActiveTurnProjection / PermissionProjection / SessionSummaryProjection
    └── registry.rs   # RegistryDocRoot / MachineView / SessionSummary / RegistryGlobal
```

测试布局：模块内 `*_test.rs`（单元，与仓库规范一致）+ `tests/` 集成（契约向量，见 §12）。

**序列化形态统一约定**：所有线协议结构 `#[serde(rename_all = "camelCase")]`（文档 JSON 示例均为 camelCase：`commandId`/`turnId`/`sessionId`/`permissionId`/`projectionVersion`/`bufferLost`/`streamEpochs`/`fromSeq`/`aliveSessions`/`sessionContext`）。时间字段为 RFC3339 字符串（`String`，与 §5.3 `created_at: String` 一致）。

**依赖变更**（workspace 均已声明，proto 增补引用）：

```toml
[dependencies]
sha2 = { workspace = true }   # HMAC-SHA256（§9.2）
hmac = { workspace = true }   # 0.12；Mac::verify_slice 自带常量时间比较
hkdf = { workspace = true }   # 单连接密钥派生
base64 = { workspace = true } # RFC 4648 标准字母表
rand = { workspace = true }   # challenge_nonce / session_context 生成（32B CSPRNG）
```

## 3. 帧模型：Frame 枚举（§4.2）

### 3.1 序列化形态

```rust
#[serde(tag = "t")]
pub enum Frame {
    #[serde(rename = "action")]
    Action(ActionEnvelope),           // ActionEnvelope 自身是 tag="type" 的 internally tagged 枚举（§4）
    #[serde(rename = "action_ack")]
    ActionAck(ActionAck),
    #[serde(rename = "action_error")]
    ActionError(ActionError),
    #[serde(rename = "event")]
    Event(EventFrame),
    #[serde(rename = "keep_alive")]
    KeepAlive(KeepAlive),
    #[serde(rename = "pong")]
    Pong(Pong),
    #[serde(rename = "ready")]
    Ready(Ready),
    #[serde(rename = "auth")]
    Auth(Auth),
    #[serde(rename = "auth_response")]
    AuthResponse(AuthResponse),
    #[serde(rename = "ysync.subscribe")]
    YsyncSubscribe(YsyncSubscribe),
    #[serde(rename = "ysync.unsubscribe")]
    YsyncUnsubscribe(YsyncUnsubscribe),
    #[serde(rename = "ysync.update")]
    YsyncUpdate(YsyncUpdate),
    #[serde(rename = "ysync.sync")]
    YsyncSync(YsyncSync),              // M1 不启用（§5.6 不采用双向增量握手），保留定义
    #[serde(rename = "ysync.awareness")]
    YsyncAwareness(YsyncAwareness),    // M3 启用，保留定义
    #[serde(rename = "machine/hello")]
    MachineHello(MachineHello),
    // … machine 帧（§6）逐变体显式 rename = "machine/*"
}
```

- **双层 internally tagged**：`Frame`（tag `"t"`）的 `Action` 变体 newtype 包裹 tag `"type"` 的 `ActionEnvelope`，序列化结果即文档 §4.3 形态：`{"t":"action","commandId":…,"type":"session/prompt","payload":{…}}`。
- tag 值含 `.`（`ysync.*`）与 `/`（`machine/*`），无法由 `rename_all` 派生，**逐变体显式 `#[serde(rename=…)]`**。
- 未知 `t` 由 serde 报 unknown variant → 上层映射为 `UNSUPPORTED_FRAME`（§9 白名单）。

### 3.2 帧类型全表（§4.2 完整面 → M1 收窄见 §9）

| `t` | 方向 | 载荷类型 | M1 | 说明 |
|-----|------|---------|-----|------|
| `action` | C→S | `ActionEnvelope` | ✓（5 种 type，见 §4） | 必须 Ack |
| `action_ack` | S→C | `ActionAck` | ✓ | 每 action 至多一个最终 Ack |
| `action_error` | S→C | `ActionError` | ✓ | 失败即返回 |
| `event` | S→C | `EventFrame` | —（M3） | `events/subscribe` 推送；类型保留 |
| `keep_alive` | S→C | `KeepAlive` | ✓ | 心跳 |
| `pong` | C→S | `Pong` | ✓ | keep_alive 回执 |
| `ready` | S→C | `Ready` | ✓ | 快照推送完成握手（§4.6） |
| `auth` | C→S | `Auth` | ✓ | 连接后第一帧；角色由 token 解析 |
| `auth_response` | S→M | `AuthResponse` | ✓（machine 面） | **【决策】帧名**：§9.2 要求 server 以 HMAC 响应作为身份证明，但未指定帧名；采用 `auth_response`（`auth` 面命名对称）；§4.8 M1 machine 帧表未列（见 §9.2 注） |
| `ysync.sync` | 双向 | `YsyncSync` | —（不启用） | y-sync Step 1/2；§5.6 已否决双向握手，保留定义 |
| `ysync.update` | S→C（单向） | `YsyncUpdate` | ✓ | 客户端上行一律拒绝（§5.6） |
| `ysync.subscribe` | C→S | `YsyncSubscribe` | ✓ | `{ docs: [...] }` |
| `ysync.unsubscribe` | C→S | `YsyncUnsubscribe` | ✓ | `{ docs: [...] }` |
| `ysync.awareness` | 双向 | `YsyncAwareness` | —（M3） | 保留定义 |
| `machine.*` | S↔M | 见 §6 | ✓（全 9 帧 + auth_response） | — |

### 3.3 解析入口与错误面

```rust
pub struct FrameTag(pub &'static str);          // "t" 的静态注册表条目（见 §9 whitelist）

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("malformed frame: {0}")]
    Malformed(String),                            // JSON 不可解析 / 字段缺失
    #[error("unsupported frame tag: {0}")]
    Unsupported(String),                          // t 未注册 或 不在当前白名单 → UNSUPPORTED_FRAME
    #[error("frame rejected by direction: {0}")]
    DirectionRejected(String),                    // 白名单方向约束违反（如 C→S 的 ysync.update）
}

impl Frame {
    /// 提取 "t" 并完整解析。未知 tag → Err(Unsupported)，不 panic、不静默。
    pub fn parse(raw: &str) -> Result<Frame, ProtoError>;
    pub fn tag(&self) -> FrameTag;
}
```

`parse` 先以 `serde_json::Value` 提取 `t` 查注册表（区分「未知 tag」与「已知但反序列化失败」），再反序列化到具体变体。

## 4. Action envelope 与方法面（§4.3）

### 4.1 ActionEnvelope

```rust
#[serde(tag = "type")]                       // 第二层 internally tagged
pub enum ActionEnvelope {
    #[serde(rename = "session/create")]
    Create { command_id: String, payload: CreateSessionPayload },
    #[serde(rename = "session/load")]
    Load { command_id: String, payload: LoadSessionPayload },        // M2
    #[serde(rename = "session/close")]
    Close { command_id: String, payload: CloseSessionPayload },
    #[serde(rename = "session/prompt")]
    Prompt { command_id: String, payload: PromptSessionPayload },
    #[serde(rename = "session/cancel")]
    Cancel { command_id: String, payload: CancelSessionPayload },
    #[serde(rename = "permission/resolve")]
    ResolvePermission { command_id: String, payload: ResolvePermissionPayload },
    #[serde(rename = "events/subscribe")]
    SubscribeEvents { command_id: String, payload: SubscribeEventsPayload },      // M3
    #[serde(rename = "events/unsubscribe")]
    UnsubscribeEvents { command_id: String, payload: UnsubscribeEventsPayload },   // M3（§4.3.1）
}
```

- `command_id`: String（uuid 形态；文档「uuid」，不做格式强校验，幂等键语义在 server）。
- **【决策】payload 判别方案**：`type` 判别放在 envelope 层（而非 payload 内 untagged 枚举）。理由：`session/load` 与 `session/close` 的 payload 同为 `{ session_id }` 单字段，untagged 无法区分；envelope 层 internally tagged 天然消除歧义（§12 有判别测试）。

### 4.2 payload 结构（字段照抄 §4.3/§4.3.1）

```rust
#[serde(rename_all = "camelCase")]
pub struct CreateSessionPayload { pub machine_id: Option<String>, pub cwd: Option<String>, pub title: Option<String> }
#[serde(rename_all = "camelCase")]
pub struct LoadSessionPayload { pub session_id: String }                 // M2
#[serde(rename_all = "camelCase")]
pub struct CloseSessionPayload { pub session_id: String }
#[serde(rename_all = "camelCase")]
pub struct PromptSessionPayload { pub session_id: String, pub message: String }
#[serde(rename_all = "camelCase")]
pub struct CancelSessionPayload { pub session_id: String }
#[serde(rename_all = "camelCase")]
pub struct ResolvePermissionPayload { pub session_id: String, pub permission_id: String, pub decision: PermissionDecision }
#[serde(rename_all = "camelCase")]
pub struct SubscribeEventsPayload { pub session_id: Option<String>, pub from_seq: Option<u64> }   // M3；from_seq 缺省=实时起
#[serde(rename_all = "camelCase")]
pub struct UnsubscribeEventsPayload { pub session_id: Option<String> }    // M3（§4.3.1）

#[serde(rename_all = "snake_case")]
pub enum PermissionDecision { Allow, Deny }                               // 也供 §7 schema 复用
```

注：`sessionId` 等由服务端按连接绑定补充与校验（§4.3 原文），proto 仅承载形态，不实现 binding。

## 5. Ack 与错误码（§4.4）

```rust
#[serde(rename_all = "snake_case")]
pub enum AckStatus { Accepted, Committed, Duplicate }   // "accepted" | "committed" | "duplicate"

#[serde(rename_all = "camelCase")]
pub struct ActionAck {
    pub command_id: String,
    pub status: AckStatus,
    pub turn_id: Option<String>,                        // 重发 duplicate 时必带（§4.4：返回原 Ack 与 turnId）
    pub session_id: Option<String>,                     // session/create 的 committed 必须携带（§4.4）
    pub committed_projection_version: Option<u32>,      // 字段预留（§4.4，乐观并发二期启用）
}

#[serde(rename_all = "camelCase")]
pub struct ActionError {
    pub command_id: String,
    pub code: ErrorCode,
    pub message: String,        // 脱敏信息（§9.3：截断前先剔除敏感字段）
    pub retryable: bool,
    pub retry_after_ms: Option<u64>,
}

#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    Unauthenticated, Forbidden, SessionNotFound, MachineOffline,
    VersionConflict, InvalidState, RateLimited, AgentUnavailable,
    PayloadTooLarge,                        // §4.4 九码
    UnsupportedFrame,                       // §4.8（白名单外 t → 稳定错误）
}
```

- **【决策】错误码为文档封闭集合**：§4.4 九码 + §4.8 的 `UNSUPPORTED_FRAME`；文档无 `INTERNAL` 等内部码——内部错误不直接上协议，经脱敏映射到现有码（§9.3）。枚举加 `#[non_exhaustive]` 防御性扩展，新增码必须走文档修订。
- **AckStatus 与 outbox 的关系**（协议投影，非状态机本体）：`accepted` = 进入有界队列；`committed` = 业务事实已持久化（对应 update 已落盘）；`duplicate` = 已提交命令重发（§4.4 去重表）。outbox 完整状态机（`received → accepted → intent_durable → dispatched → delivery_confirmed / delivery_unknown / failed → projection_committed / completed`）与 delivery 三级（L1/L2/L3）属 server 内部实现，不在 proto 定义（§1 边界声明）。
- retryable 分类事实源（§4.4）：`AGENT_UNAVAILABLE`/`MACHINE_OFFLINE` → true；`INVALID_STATE`/`FORBIDDEN`/`SESSION_NOT_FOUND` → false。proto 提供辅助 `ErrorCode::default_retryable()` 供两端对齐（server 裁决 + 客户端提示），不做协议字段默认。

## 6. machine 协议帧（§4.5 / §4.5.1）

全部 `#[serde(rename_all = "camelCase")]`。

### 6.1 下行（server → machine）

```rust
pub struct MachineSpawn {
    pub command_id: String, pub session_id: String,
    pub cmd: Vec<String>, pub cwd: String, pub env: Option<HashMap<String, String>>,
}   // 幂等键 session_id；env 白名单在 server/machine 双端校验（§9.6），proto 只承载形态

pub struct MachineKill {
    pub command_id: String, pub session_id: String, pub grace: Option<u64>,  // ms；幂等（已死成功返回）
}
```

### 6.2 上行（machine → server）

```rust
pub struct MachineHello {
    pub token: String,
    pub hostname: String,
    pub caps: serde_json::Value,                       // 【决策】文档未展开 caps 结构，M1 不透明透传
    pub buffered: Option<bool>,                        // 断线缓冲待补推
    pub buffer_lost: Option<bool>,                     // daemon 崩溃缓冲丢失（§7.5）
    pub stream_epochs: Option<HashMap<String, u64>>,   // per-session 流纪元映射（§4.5.1）
    pub nonce: String,                                 // challenge_nonce，32B CSPRNG，base64（§9.2/§10）
}   // 幂等替换语义：新 hello 到达即 fencing 旧连接（server 行为）

pub struct MachineHeartbeat { pub load: u32, pub alive_sessions: Vec<String> }
// 【决策】load 语义文档未展开，M1 取 0–100 整数百分比；alive_sessions 为 session_id 列表

pub struct MachineEvent { pub session_id: String, pub epoch: u64, pub seq: u64, pub frame: serde_json::Value }
// frame = 原始 ACP 帧（{type,payload} 或 JSON-RPC session/update，§6.1），machine 保持 dumb 透传；
// epoch 与 server 记录不一致的帧直接丢弃（server 行为，§4.5.1）

pub struct MachineBufferSync {
    pub session_id: String, pub epoch: u64, pub from_seq: u64,
    pub frames: Vec<BufferedFrame>,                    // frames 每帧带 seq
}
pub struct BufferedFrame { pub seq: u64, pub frame: serde_json::Value }
// 补推起点 from_seq = server 持久化 last_seq + 1（§4.5）；epoch 由 server 回传校验，不一致拒绝该批

pub struct MachineSpawnAck { pub command_id: String, pub session_id: String, pub ok: bool, pub error: Option<String> }
// error = 脱敏原因

pub struct MachineKillAck { pub command_id: String, pub session_id: String, pub ok: bool }
pub struct MachineProcessExit { pub session_id: String, pub code: i32 }
// code：退出码；crashed/ended 状态由此驱动（§4.5）
```

### 6.3 stream_epoch / seq 类型约定

- `epoch: u64`：machine 侧 per-session 流代际标识，daemon 重启或 ACP 子进程重建时 +1，session 新开为 1（§4.5.1）。
- `seq: u64`：machine 侧单调序号（每 session 独立）。
- server 持久化 `(epoch, last_seq)` 对——server 内部状态，无独立线类型；`buffer_sync`/`event` 回传 epoch 是线协议校验面（字段已含）。

## 7. Y.Doc schema 类型镜像（§5.3 / §5.4 / §5.5）

`schema/` 模块承载三 Doc 的 Rust 类型镜像，**字段与枚举严格照抄 §5.3/§5.4/§5.5**。定位：字段名/枚举/嵌套关系的事实源 + 测试与调试用 serde round-trip（镜像类型 derive Serialize/Deserialize，camelCase）；**不持有 yrs 句柄**——实际 yrs 读写由 server 聚合器经 `schema` 模块导出的类型与字段常量完成（架构 §12：`server/src/state/chat-writer`）。

物理映射（§5.3 原文）：根对象/`entries`/`blocks`/`tool_calls` 用 `Y.Map`；顺序索引用 `Y.Array<String>`；流式文本用 `Y.Text`（避免每 token 替换完整字符串）；删除采用领域 tombstone，不由客户端物理删除权威记录。

### 7.1 schema/mod.rs —— 跨 Doc 公共类型

```rust
#[serde(rename_all = "camelCase")]
pub struct PublicError { pub code: String, pub message: String }
// 【决策】§9.3 仅规定「稳定错误码 + allowlist 摘要字段（状态/耗时/大小）」，字段集未展开；
// M1 最小实现 code+message，摘要字段随 §9.3 增补。

// §5.3 枚举
pub enum EntryKind { Message, Tool, System }
pub enum EntryRole { User, Assistant, System }
pub enum EntryStatus { Pending, Streaming, Completed, Cancelled, Error }
pub enum BlockVisibility { Summary, Hidden }           // hidden 内容绝不发给无权客户端（§5.3）
pub enum ToolCallStatus { Pending, AwaitingPermission, Running, Completed, Error, Cancelled }

// §5.4 枚举
pub enum PermissionOptions { AllowOnce, AllowSession, Deny }
pub enum PermissionStatus { Pending, Resolved, Expired }
// 【决策】值域按 §7.2 turn 状态机定稿（accepting/running/awaiting_permission/cancelling/
// completed/cancelled/interrupted/failed），§5.4 未展开枚举值域
pub enum TurnStatus { Accepting, Running, AwaitingPermission, Cancelling, Completed, Cancelled, Interrupted, Failed }
// 【决策】值域按 §7.3 session 生命周期定稿（accepting/ended/closed/crashed），§5.4 未展开；
// gap 独立字段承载（§5.5），不进 status 枚举
pub enum SessionStatus { Accepting, Active, Ended, Closed, Crashed }

// §5.5 枚举
pub enum MachineStatus { Online, Offline, Unknown }
pub enum GlobalStatus { Healthy, Degraded, Restarting }   // Degraded 判定规则见架构 §17
```

### 7.2 schema/chat.rs —— Chat Doc（§5.3，`CHAT_DOC_SCHEMA_VERSION = 1`）

```rust
pub struct ChatDocRoot {
    pub schema_version: u32,           // == CHAT_DOC_SCHEMA_VERSION（§11）
    pub projection_version: u32,       // 每次成功投影 +1；与 schema_version 分离（§5.6）
    pub entry_order: Vec<String>,      // Y.Array<String>，与 entries 分离便于局部更新/未来分页
    pub entries: HashMap<String, ChatEntry>,
    pub tool_calls: HashMap<String, ToolCallProjection>,
    // 无 committed_commands：去重记录在 server command outbox（§4.4），不随 Doc 生命周期存亡
}

pub struct ChatEntry {
    pub entry_id: String,              // 派生规则：`{turnId}:user` / `{turnId}:assistant` / tool: 按 toolCallId
    pub turn_id: Option<String>,
    pub kind: EntryKind,
    pub role: EntryRole,
    pub status: EntryStatus,
    pub author_user_id: Option<String>,
    pub created_at: String,            // RFC3339（§5.3 created_at: String）
    pub completed_at: Option<String>,
    pub block_order: Vec<String>,      // Y.Array<String>
    pub blocks: HashMap<String, ContentBlock>,
    pub error: Option<PublicError>,    // 脱敏公开错误，不含内部细节
}

#[serde(tag = "kind", rename_all = "snake_case")]   // 镜像内部判别形态，非线协议
pub enum ContentBlock {
    Text { block_id: String, text: String },                          // 流式文本用 Y.Text
    Reasoning { block_id: String, text: String, visibility: BlockVisibility },
    ToolCall { block_id: String, tool_call_id: String },
    Resource { block_id: String, resource_id: String, media_type: String, name: String }, // 只存引用，不嵌入内容
}

pub struct ToolCallProjection {
    pub tool_call_id: String,
    pub turn_id: String,
    pub name: String,
    pub status: ToolCallStatus,
    pub arguments: Option<serde_json::Value>,   // 过滤内部/敏感字段后投影
    pub result: Option<serde_json::Value>,      // 仅在公开投影预算内保留
    pub result_omitted: Option<bool>,           // true/false 为明确事实，None 为旧记录
    pub result_bytes: Option<u64>,              // 紧凑 JSON 字节数，不含内容
    pub public_error: Option<PublicError>,
    pub permission_id: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}
```

### 7.3 schema/session.rs —— Session Doc（§5.4）

```rust
pub struct SessionDocRoot {
    pub schema_version: u32,           // 旧快照恢复时以版本判空幂等补结构（§5.4）
    pub projection_version: u32,
    pub session: SessionInfoProjection,
    pub agent: AgentStatusProjection,
    pub active_turn: Option<ActiveTurnProjection>,   // 权威投影，前端由 turnStatus 派生展示（架构 §7.2）
    pub pending_permissions: HashMap<String, PermissionProjection>,
    pub sessions: HashMap<String, SessionSummaryProjection>,  // agent 磁盘历史（10s 轮询全量同步）
}

pub struct SessionInfoProjection {
    pub session_id: String,
    pub title: String,
    pub status: SessionStatus,              // 【决策】值域见 §7.1
    pub active_turn_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub struct AgentStatusProjection {
    pub instance_id: String,
    pub acp_session_id: String,
    pub status: String,                     // 【决策】agent 状态值域文档未展开，M1 透传 ACP agent 状态
    pub capabilities: Vec<String>,
    pub last_activity_at: String,
    pub public_error: Option<PublicError>,
}

pub struct ActiveTurnProjection {
    pub turn_id: String,
    pub turn_status: TurnStatus,
    pub updated_at: String,
}

pub struct PermissionProjection {
    pub permission_id: String,
    pub turn_id: String,
    pub tool_call_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub options: Vec<PermissionOptions>,
    pub status: PermissionStatus,
    pub expires_at: String,                 // server 权威时钟生成（架构 §4.7）
    pub decision: Option<PermissionDecision>, // CAS 迁移成功后写入；expired 保持 null
}

pub struct SessionSummaryProjection {       // agent 磁盘历史会话条目
    pub session_id: String, pub title: String, pub status: String, pub updated_at: String,
}
// 【决策】§5.4 未展开字段；M1 以最小摘要实现，与架构 §15 映射对齐时定稿（§14 待确认 3）
```

### 7.4 schema/registry.rs —— Registry Doc（§5.5，acp-hub 特有）

```rust
pub struct RegistryDocRoot {
    pub schema_version: u32,
    pub machines: HashMap<String, MachineView>,
    pub sessions: HashMap<String, SessionSummary>,   // 活跃会话摘要——唯一权威源，server 状态源单写（§5.2）
    pub global: RegistryGlobal,
}

pub struct MachineView {
    pub id: String, pub hostname: String,
    pub status: MachineStatus,
    pub token_id: String,                // 只暴露 token_id，绝不暴露 token 本体（§9.2.1）
    pub registered_at: String,
    pub last_heartbeat: String,
    pub session_count: u32,
}

pub struct SessionSummary {
    pub id: String, pub machine_id: String, pub title: String,
    pub status: String,                  // 【决策】§5.5 未展开，M1 以架构 §7.3 session 状态字符串透传
    pub gap: Option<u64>,                // 补推缺口（§8.5），无缺口为 null
    pub updated_at: String,
}

pub struct RegistryGlobal { pub status: GlobalStatus }   // §5.5 global: { status }
```

## 8. 连接生命周期帧（§4.2 / §4.6 / §4.7 / §9.2）

```rust
#[serde(rename_all = "camelCase")]
pub struct Auth { pub token: String }                  // C→S；连接后第一帧；角色由 token 解析（§4.2）

#[serde(rename_all = "camelCase")]
pub struct AuthResponse {                              // S→M；§9.2 server 身份证明
    pub session_context: String,                       // 32B CSPRNG，base64
    pub hmac: String,                                  // HMAC-SHA256 输出，base64（§10 hmac 模块）
}
// 【决策】session_context 生成方：server 生成并随 auth_response 下发（machine 需其作为 MAC 输入）。
// 文档仅规定「连接级随机 id」与「32B 原始字节」，未指定生成方/传递帧——此为最小实现选择。

pub struct Ready { pub projection_versions: HashMap<DocId, u32> }   // §4.6 步骤 4
pub struct KeepAlive {}                                // S→C；§4.7 载荷为 ping
pub struct Pong {}                                     // C→S

pub struct DocId(String);                              // newtype：`chat:{sid}` / `session:{sid}` / `hub:registry`
impl DocId { pub fn chat(sid:&str)->Self; pub fn session(sid:&str)->Self; pub const REGISTRY: Self; }
// 【决策】命名形态照抄 §5.2 表；FromStr 校验 {sid} 段为合法标识符，防止 doc 名注入
```

**关闭码**（§4.7，`CloseCode` 枚举放 `conn.rs`，值为 ws 关闭码数字）：

| 常量 | 值 | 触发 | 客户端行为 |
|------|----|------|-----------|
| `CLOSE_MACHINE_OFFLINE` | 4500 | 机器离线 | 停止自动重连，手动重试 |
| `CLOSE_KEEPALIVE_TIMEOUT` | 4501 | keep_alive 超时 | 不后台自动重连 |
| `CLOSE_CONFIG_FATAL` | 4502 | 配置性永久失败 | 停止自动重连 |
| `CLOSE_GENERIC_FAILURE` | 1011 | 通用失败 | 退避重连 |
| `CLOSE_QUOTA_EXCEEDED` | 1013 | 连接配额超限 | 退避重连 |

【决策】client token 校验失败（P7：未知设备断开）的关闭码文档未指定；建议 `1011` + 失败认证计数（架构 §17.1），不引入新码。machine 认证失败按 §9.2 步骤 3 以 `4502` 关闭。

**y-sync 帧**（`ysync.rs`）：

```rust
#[serde(rename_all = "camelCase")]
pub struct YsyncSubscribe { pub docs: Vec<DocId> }
#[serde(rename_all = "camelCase")]
pub struct YsyncUnsubscribe { pub docs: Vec<DocId> }
#[serde(rename_all = "camelCase")]
pub struct YsyncUpdate {
    pub doc: DocId,
    pub update: String,                  // base64（Y.encodeStateAsUpdate / update diff，§4.1）
    pub projection_version: Option<u32>, // 快照必带（§4.6 步骤 3），增量不携带
}
pub struct YsyncSync { pub msg: String }            // y-sync Step 1/2，base64；不启用（§5.6）
pub struct YsyncAwareness { pub msg: String }       // y-protocol awareness，base64；M3
```

## 9. M1 帧集白名单（§4.8）

### 9.1 机制

`whitelist.rs` 维护两件事：

1. **全量帧 tag 注册表**（`FRAME_TAGS: &[FrameTag]`，含 M2/M3 保留帧，见 §3.2 表）——`Frame::parse` 用它区分「未知 tag」与「已知 tag 反序列化失败」；
2. **M1 白名单 + 方向约束**：

```rust
pub enum Role { Client, Machine }                      // 连接侧角色（由 token 解析，§9.5）
pub enum Direction { Inbound, Outbound }               // 相对 server 的方向

pub fn m1_allows(tag: FrameTag, role: Role, dir: Direction) -> bool;
// 客户端面：action（5 种 type）、action_ack、action_error、ysync.subscribe/unsubscribe、
//           ysync.update（S→C 单向，C→S 拒绝——向量 6）、ready、keep_alive、pong、auth
// machine 面：machine/* 全 9 帧 + auth_response
```

检查失败统一映射：未知 t / 已知非 M1 → `ProtoError::Unsupported`；方向违反 → `ProtoError::DirectionRejected`。server 侧两者均以 `UNSUPPORTED_FRAME` 回 `action_error`（若可回）或断开，并计数（§4.8「并计数，不静默」）。

### 9.2 M1 收窄内容（照抄 §4.8，标注里程碑）

- **action 的 type 子集**：M1 仅 `session/create`、`session/prompt`、`session/cancel`、`session/close`、`permission/resolve`；`session/load`（M2）、`events/subscribe`/`events/unsubscribe`（M3）**类型保留定义**，白名单外。
- 帧面：`event`、`ysync.sync`、`ysync.awareness` 不在 M1 白名单（类型保留）。
- machine 面 M1 即全量（9 帧）。
- **注**：§4.8 M1 machine 帧表未列 `auth_response`，但 §9.2 步骤 2 要求 server 以 HMAC 响应证明身份——文档内部不一致，按 §9.2（认证为连接必需）处理：`auth_response` 属 M1 machine 面白名单。记录为文档修订建议（§14 待确认 1）。

## 10. HMAC 双向认证（§9.2 顾问3 线格式）

`hmac.rs` 提供**纯函数**原语（无 I/O、无连接状态；nonce 单次使用等状态在 server `auth` 模块）：

```rust
pub const CHALLENGE_NONCE_LEN: usize = 32;
pub const SESSION_CONTEXT_LEN: usize = 32;
pub const HMAC_OUTPUT_LEN: usize = 32;                  // HMAC-SHA256
pub const NONCE_TTL: Duration = Duration::from_secs(30); // 短期有效窗口（§9.2 协议级属性）

pub fn generate_challenge_nonce() -> [u8; 32];          // rand::rng（CSPRNG）
pub fn generate_session_context() -> [u8; 32];

/// HKDF-SHA256 派生单连接密钥。ikm = machine_token（32B）；派生上下文含 role（§9.2）。
/// 【决策】salt = 空（RFC 5869 零串），info = b"acp-hub-auth" ‖ role_utf8，输出 32B。
/// 文档仅规定「派生上下文含 role」，salt/info 精确形态为实现固定点——随字节级测试向量固化（§12）。
pub fn derive_mac_key(machine_token: &[u8; 32], role: &str) -> [u8; 32];

/// MAC 输入规范化：challenge_nonce ‖ session_context ‖ protocol_version ‖ role
/// 每字段 = u16 大端长度前缀 + UTF-8 字节（challenge/session_context 为 32B 原始字节）；
/// 字段顺序即文档顺序，不得重排；protocol_version/role 用其 UTF-8 表示（如 "1"、"machine"）。
pub fn mac_input(challenge: &[u8; 32], context: &[u8; 32], protocol_version: &str, role: &str) -> Vec<u8>;

pub fn compute_mac(key: &[u8; 32], input: &[u8]) -> [u8; 32];   // HMAC-SHA256
/// 常量时间比较：优先 hmac::Mac::verify_slice（crate 内建常量时间，免新增 subtle 依赖）；
/// 比较前先对长度（32B）做防御。
pub fn verify_mac(key: &[u8; 32], input: &[u8], expected_b64: &str) -> Result<(), HmacError>;

pub enum HmacError { BadLength, InvalidBase64, Mismatch }
```

线格式要点（照抄 §9.2 顾问3，实现必须满足）：

1. 算法 `HMAC-SHA256`，输出 base64（RFC 4648 标准字母表 + padding）；
2. MAC 输入按**固定字节序（大端，长度前缀 u16）**拼接，字段顺序不可重排；
3. 比较常量时间（`verify_slice`）；
4. 密钥经 HKDF 派生，**token 本体不出现在 MAC 输入**；
5. 协议级属性（状态在 server）：nonce 单次使用 + 30s 窗口、session_context 连接绑定（跨连接重放无效）、角色/版本绑定、hello 幂等替换 fencing、失败即断开（machine 关闭码 4502）+ 审计计数。

client（TUI）连接**无**双向认证（§9.2 仅覆盖 machine 连接；client 走 `auth { token }` 单向校验，P7）。

## 11. 版本常量与协议参数（§4.1 / §5.3 / §13.1 / §16）

`version.rs` + `conn.rs`/`hmac.rs` 常量：

| 常量 | 值 | 依据 |
|------|----|------|
| `PROTOCOL_VERSION: u32` | 1 | §13.1（machine/hello 携带；版本不匹配拒绝连接）。【决策】数值取 1 |
| `CHAT_DOC_SCHEMA_VERSION: u32` | 1 | §5.3 明示（真相来源以本 crate 实现为准） |
| `SESSION_DOC_SCHEMA_VERSION: u32` | 1 | §5.4 未给数值【决策】取 1 |
| `REGISTRY_DOC_SCHEMA_VERSION: u32` | 1 | §5.5 未给数值【决策】取 1 |
| `Y_UPDATE_ENCODING_VERSION: u32` | 1 | §4.1「固定 update 编码版本 v1」 |
| 关闭码 4500/4501/4502/1011/1013 | — | §4.7（§8 表） |
| `CHALLENGE_NONCE_LEN`/`SESSION_CONTEXT_LEN` = 32 | — | §9.2 顾问3「32B 原始字节」 |
| `NONCE_TTL = 30s` | — | §9.2「短期有效窗口 30s 过期」 |

§16 协议参数默认值（proto `protocol::Defaults`，供 server config 引用为默认；server 可覆盖）：心跳间隔 5s、离线判定 30s、环形滑窗 500 条、单帧上限 1MB、缓冲上限 10MB/万条。**其余 §16 项（监听地址/端口、数据目录、命令队列 64、连接配额 200、背压 64KB/128KB、微批次 16ms、超时组、fsync、compact、磁盘预算、归档、env 白名单、allow_non_loopback）属 server 运维配置，不在 proto。**

## 12. 契约测试清单（§4.8 测试向量归属）

### 12.1 proto 层纯测试（本 crate 内，无 ws/进程/持久化）

**向量 6 —— 帧集白名单**（`whitelist_test.rs` + `frame_test.rs`）：

- 未知 t（如 `"foo"`）→ `ProtoError::Unsupported`；
- 已知但非 M1（`ysync.awareness`、`ysync.sync`、`event`、`action` 的 `events/subscribe`/`session/load` type）→ `Unsupported`；
- 方向约束：C→S `ysync.update` → `DirectionRejected`（§5.6 客户端上行拒绝）；
- M1 全帧 round-trip：每帧 tag 解析→序列化→再解析字节一致；
- payload 判别：`session/load` 与 `session/close` 同形 payload `{session_id}` 经 envelope 层 `type` 判别无歧义；
- 畸形 JSON / 缺字段 → `Malformed`（不 panic）。

**向量 12 —— HMAC 字节级向量**（`hmac_test.rs`）：

- 固定输入（测试专用 32B `machine_token` 常量 + 固定 nonce/context/version/role）→ 断言期望 MAC（base64）。真值生成：实现时以一次性脚本按 §10 公式计算后**固化进测试常量**，跨实现可验证；
- `mac_input` 规范化单测：断言长度前缀（u16 BE）与字段顺序字节串；
- 长度校验：nonce/context 非 32B → `HmacError::BadLength`；
- `verify_mac`：错误 base64 / 错误 MAC → `Mismatch`（常量时间 API 存在性由 `verify_slice` 保证）。

### 12.2 集成层（server/联调，标注入册，不在本 crate）

向量 1（握手/auth 断开、ready 前缓冲）、2（commandId 幂等 duplicate）、3（补推重放幂等聚合）、4（终态守卫）、5（kill -9 崩溃恢复 + buffer_sync 两分支）、7（delivery_unknown 路径 B/路径 A）、8（双向认证：重放旧握手、错误角色、过期 challenge、未知身份 → 拒绝关闭 + 审计计数）、9（错误脱敏）、10（outbox 归档保留）、11（delivery_unknown 跨重启保留）——分别挂到 server（auth/command-coordinator/state）与 e2e 测试计划，F1 不实现。

## 13. 依赖与文件清单（落地建议）

- 修改：`proto/Cargo.toml`（§2 依赖）、`proto/src/lib.rs`（模块声明 + re-export）；
- 新增：§2 模块文件 + 对应 `*_test.rs` + `tests/` 契约向量文件；
- 不动：server/machine 现有代码（本 feature 只交付协议 crate；两端消费在后续 feature）。

## 14. 待确认项（排期时定，不阻塞 F1）

1. `auth_response` 帧名（§3.2/§9.2 注）——建议回报架构文档修订（§4.8 M1 machine 帧表补 `auth_response`）；
2. `machine/hello.caps` 与 `machine/heartbeat.load` 的精确结构（§6 标注）；
3. `SessionDocRoot.sessions`（`SessionSummaryProjection`）字段集——§5.4 未展开，M1 以最小摘要（session_id/title/status/updated_at）实现，与架构 §15 映射对齐时定稿；
4. client auth 失败关闭码（§8，建议 1011）。
