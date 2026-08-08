# F5 设计：server 协议/通道/控制面（protocol + channel + control）

> 状态：设计稿（对应 Feature F5，本工程最核心的装配层）
> 日期：2026-08-07
> 权威来源：`docs/architecture.md`（v2.4）§3.1/3.3、§4.2–4.8、§6.1–6.5、§7.1–7.6、§8.2–8.6、§9.2/9.5/9.6、§10、§16、§17
> 前置依赖：proto（F1：帧/Action envelope/machine 协议/DocId/白名单/HMAC）、server config+auth（F2）、persist（F3：outbox 状态机/水位/update 日志）、state（F4：NormalizedEvent/DocManager 单写者/Aggregator/Permission CAS/Registry）
> 约束：**忠于架构文档，不引入文档外的协议与语义**；只操作 `server/src/protocol.rs`/`channel.rs`/`control.rs` 三个占位单文件（git rm 后建目录）；不修改 `lib.rs`/`Cargo.toml`/`docs/architecture.md`/其他 feature 模块；日志走 tracing 且脱敏（§9.3：不记正文/工具参数/token/密钥）；文档未指明处的命名/参数/阈值均标注「【决策】」并给出依据。

---

## 1. 目标与范围

F5 把 server 从「CLI + 占位常驻」装配成**完整中心控制面**：连接层（ws accept/认证/快照时序/心跳）、控制面（Action/Ack 两阶段、commandId 去重、machine 指令下发）、协议边界（ACPChannel 入站规范化 + Translator 出站翻译）、状态协同（machine 注册表、session 注册表、断链恢复、补推对账）。产线为「TUI/Web 连接 + machine 连接」两类 ws 会话，统一消费 proto 帧面（§4.2），受 M1 帧集白名单约束（§4.8）。

**范围（任务 14 项映射）**：

1. `protocol/`：acp_channel.rs（入站规范化，§6.1）+ translator.rs（出站翻译，§6.1）；
2. `channel/`：gateway.rs（ws 生命周期/快照时序/keep_alive 接线）、session_channel.rs（客户端连接归一化）、command_coordinator.rs（串行队列 + 去重 + 两阶段 Ack）、relay_event_handler.rs（machine 入站消费 + 断链清理）、broadcaster.rs（fan-out + 背压）、connection_registry.rs（配额 + ConnectionCtx）；
3. `control/`：machine_registry.rs（machine 生命周期 + 指令下发）、session_registry.rs（会话状态机 + binding + 对账）、hub.rs（装配）、heartbeat.rs（keep_alive）、close_codes.rs（关闭码）；
4. 服务装配（`run_with` 扩展）、Degraded 判定入口、孤儿进程清理钩子；
5. 测试清单（各模块单元测试 + fake machine ws 客户端）。

**边界声明**（不属本 feature）：

- machine 侧产品实现（`machine/src/transport.rs`/`buffer.rs`/`auth.rs` 的填充）不在本 feature——本 feature 只定义 server 侧行为 + **测试用 fake machine ws 客户端**（`server/tests/common/`）；
- 聚合/投影（F4 已交付）只被消费，不修改；`UpdateSink` 的持久化生产实现（persist adapter）若 F6 未提供，由 F5 装配一个薄 adapter（见 §14）；
- 10s 轮询 session_list、events/subscribe（M3）、session/load（M2）不在 M1 帧面（§4.8），类型面保留、不实现；
- TUI 客户端（acp-hub-tui）是独立二进制，不在本 feature。

---

## 2. 模块划分与文件布局

占位单文件 `server/src/protocol.rs`/`channel.rs`/`control.rs` 扩展为目录（实现时各 `git rm` 原单文件后建目录，lib.rs 模块名不变）：

```
server/src/protocol/
├── mod.rs          # 模块文档 + re-export（AcpChannel/Translator/OutboundCtx/OutboundMessage…）
├── acp_channel.rs  # 入站规范化：双格式 sessionId 提取 + 事件映射表（§6.1）+ NormalizeOutcome
└── translator.rs   # 出站翻译：Action → ACP JSON-RPC（cwd/rpcId 注入，§6.1/§4.3）

server/src/channel/
├── mod.rs
├── gateway.rs             # ws accept、连接认证接线、快照时序（§4.6）、keep_alive/关闭码接线
├── session_channel.rs     # 客户端连接归一化：relayReady、ready 前 Action 缓冲、帧分派
├── command_coordinator.rs # 每 session 串行队列（64）+ commandId 去重 + outbox 推进 + 两阶段 Ack
├── relay_event_handler.rs # machine 入站消费：epoch/seq/binding 校验 → DocManager；断链清理
├── broadcaster.rs         # fan-out（每连接每 doc 订阅）+ 64KB/128KB 背压 + merge_updates
└── connection_registry.rs # 配额（200）+ 连接上下文生命周期

server/src/control/
├── mod.rs
├── machine_registry.rs    # REGISTERED/ONLINE/OFFLINE + 30s 离线判定 + hello fencing + spawn/kill ack 跟踪
├── session_registry.rs    # 会话状态机 + binding + pending_close 补发 + alive_sessions 对账
├── hub.rs                 # 装配（Hub）：组件实例化 + run_server 入口 + 优雅关闭 + Degraded 入口
├── heartbeat.rs           # keep_alive 5s 周期 + pong 超时 → 4501（§4.7）
└── close_codes.rs         # 关闭码 4500/4501/4502 常量 + 客户端重连策略（§4.7 表）
```

依赖方向（单向，防环）：`protocol`（纯函数，零依赖）← `channel`（依赖 protocol + state + persist + auth）← `control`（依赖 channel + state + persist + auth）；`control/hub.rs` 是唯一装配点，`channel/gateway.rs` 依赖 control 的注册表句柄（machine 连接认证后移交 `MachineRegistry`，客户端帧经 `SessionChannel` 分派到 coordinator）。

模块间关键句柄传递（避免循环依赖的通道——均以 `Arc<dyn Trait>` 或 struct 引用注入）：

| 生产者 | 消费者 | 载体 |
|--------|--------|------|
| `DocManager`（F4） | `Broadcaster` | `subscribe_updates() -> UnboundedReceiver<DocUpdate>` |
| `DocManager`（F4） | `CommandCoordinator`/`RelayEventHandler` | `submit_event` / `submit_command` / `try_reserve` |
| `Store`（F3） | `CommandCoordinator` | outbox 状态机接口（insert/mark_*/clear_for_retry） |
| `MachineRegistry` | `CommandCoordinator` | `send_spawn`/`send_kill`/`forward_rpc` + ack oneshot |
| `SessionRegistry` | `RelayEventHandler`/`CommandCoordinator` | `resolve(acp_session_id) -> hub session_id`（binding，§6.1） |
| `RegistryState`（F4） | `SessionRegistry`/`RelayEventHandler` | `set_session_gap`/`set_session_status`/`report_condition` |
| `AuthService`（F2） | `Gateway` | `authenticate_machine`/`authenticate_client` + nonce sweep |

---

## 3. protocol/acp_channel.rs —— 入站规范化（§6.1）

`AcpChannel` 是**唯一协议边界**：machine 透明转发原始 ACP 帧，server 在此规范化为 `NormalizedEvent`（F4 定义，14 种变体），聚合层只消费规范化事件。**纯函数、无 I/O**（与 F4 `Aggregator::apply` 同构），保证单测完备与幂等重放。

```rust
/// 入站规范化（§6.1）：唯一协议边界。纯函数：输入原始帧，输出规范化事件。
pub struct AcpChannel;

/// 单帧规范化的结果。
pub enum NormalizeOutcome {
    /// 业务事件（供聚合器消费；envelope 已含 hub session_id/epoch/seq）。
    Event(NormalizedEvent),
    /// JSON-RPC response（无 method，含 id）：L3 受理确认输入（§4.4）。
    /// id 与 Translator 登记的 rpcId 匹配后完成 delivery_confirmed（§8）。
    RpcResponse { id: String, is_error: bool },
    /// 丢弃（未知帧/无法提取 sessionId/字段缺失）：**不静默**——返回原因供计数，
    /// 计入 gap/指标（§17.1），不 panic。
    Dropped(DropReason),
}

/// 丢弃原因（脱敏诊断，不携带帧正文）。
pub enum DropReason {
    /// 未知 ACP 帧 type / JSON-RPC method（非 §6.1 表条目）。
    UnsupportedFrame,
    /// 双格式 sessionId 均无法提取（§3.3：machine 侧记本地缺口随 hello 上报）。
    NoSessionId,
    /// 缺少必要关联信息（无 turn_id 的增量等，§6.3 同源拒绝）。
    MissingField,
    /// 帧结构非法（非对象、payload 非对象）。
    Malformed,
}

/// 主入口：原始 ACP 帧 → 规范化事件。
///
/// `session_id` 为 **hub 侧** id（调用方已按 binding 翻译与校验，§6.1 可信
/// binding）；`epoch`/`seq` 为 machine 侧流纪元与单调序号（§4.5.1，透传进
/// NormalizedEvent envelope）；`now_rfc3339` 为 server 权威时钟（§4.7——
/// permission expires_at 判定性时间戳由 server 生成，machine 只上报相对时序）。
pub fn normalize(
    &self,
    session_id: &str,
    epoch: u64,
    seq: u64,
    now_rfc3339: &str,
    frame: &serde_json::Value,
) -> NormalizeOutcome;
```

### 3.1 双格式 sessionId 提取（§3.3 同源）

```rust
/// 双格式 sessionId 提取（§3.3/§6.1 兼容规则；提取到的是原始 acp_session_id）。
///
/// 1. 原始 `{type, payload}`：`payload.sessionId` → `payload.session_id` → 顶层 `sessionId`；
/// 2. JSON-RPC 包裹：`{"jsonrpc":"2.0","method":"session/update","params":{…}}` →
///    `params.sessionId` → `params.session_id`（notification 与 response 同规则，
///    response 另按 `id` 面处理）；
/// 3. 均缺失 → None（上层按 [`DropReason::NoSessionId`] 丢弃并计数）。
pub fn extract_session_id(frame: &serde_json::Value) -> Option<String>;
```

### 3.2 事件映射表（§6.1 表 → `NormalizedEvent::EventBody` 14 变体）

| 原始帧（type/method） | 规范化事件（EventBody） | 提取要点 |
|---|---|---|
| `agent_message_chunk` | `MessageDelta` | turn_id/entry_id/block_id/text |
| `agent_thought_chunk` | `ReasoningDelta` | + visibility（summary/hidden，§5.3） |
| `user_message_chunk` | `UserMessage` | 服务端单写注册的映射（§6.5）；幂等以 turn_id |
| `prompt_complete` / `agent_message_complete` | `TurnTerminal{Completed}` | 终态 |
| `session_error` | `TurnTerminal{Failed}` | public_error 脱敏（§9.3） |
| `tool_call` | `ToolCallStarted` | tool_call_id/name/arguments/created_at |
| `tool_call_update`（status=running/streaming） | `ToolCallUpdated` | arguments 全量覆盖（M1） |
| `tool_call_update`（status=completed/error/failed） | `ToolCallCompleted` | result/public_error；超大 result 只留受授权引用（§9.5） |
| `permission_request` | `PermissionRequested` | expires_at 由 `now_rfc3339` + 权限超时（5min，§16）注入 |
| `permission_response` | `PermissionResolved` | decision；CAS 在聚合器（F4） |
| `session_update` / `available_commands_update` | `SessionInfo` / `Capabilities` | 部分更新（Option 字段，§6.3） |
| `session_list` 响应 | `SessionListResponse` | 全量同步投影（F4 SessionList） |
| agent status 帧 | `AgentStatus` | status/public_error |
| JSON-RPC response（有 id、无 method） | `RpcResponse` | L3 确认（不产生业务事件） |

> **冲突记录**：任务描述事件名为 `agent_reasoning_chunk`，架构文档 §6.1 表为 `agent_thought_chunk`。**以架构文档为准**；实现时两者均接受（`agent_reasoning_chunk` 作为别名映射同一 `ReasoningDelta`），文档不改。

### 3.3 未知帧与白名单

- 未知 type / JSON-RPC method → `Dropped(UnsupportedFrame)` + 计数（§4.8 精神同源：不静默、不 panic）；**不产生 `action_error`**——该面只对 client 帧（`UNSUPPORTED_FRAME` 由 gateway 对 client 帧检查，§6）。
- binding 不存在的 session 帧：`RelayEventHandler` 在调 `normalize` **前**校验（§6.1 丢弃语义，§6.5），不进入规范化。

---

## 4. protocol/translator.rs —— 出站翻译（§6.1 出站翻译边界）

```rust
/// 出站翻译：客户端 Action → ACP JSON-RPC（§6.1）。`cwd` 由 server 按已认证
/// 上下文注入，`rpcId` 由 server 分配（避免消息被当作 notification）。
pub struct Translator {
    next_rpc_id: AtomicU64,
}

/// 出站上下文（server 按连接绑定注入，客户端字段不可覆盖 binding，§4.3）。
pub struct OutboundCtx {
    /// 最终 cwd：已认证上下文默认目录（§4.3 裁决）。
    pub cwd: String,
    /// binding 翻译后的 acp_session_id（hub session_id → 协议投递 id，§6.1）。
    pub acp_session_id: String,
}

/// 出站产物。
pub enum OutboundMessage {
    /// 单条 JSON-RPC（带 id）：prompt/cancel/resolve/initialize。
    JsonRpc(serde_json::Value),
    /// `session/new`（M1 create 序列第二步，§6.2）。
    SessionNew(serde_json::Value),
}

/// 翻译入口（M1 方法面子集：prompt/cancel/resolve/create 序列）。
///
/// create 不在此一次性翻译：§6.2 时序要求 spawn 成功后分两步下发
/// （initialize → session/new），由 coordinator 流程分两次调用。
pub fn translate(&self, action: &ActionEnvelope, ctx: &OutboundCtx) -> Result<OutboundMessage, TranslateError>;

/// rpcId 分配：【决策】全局单调，格式 `hub-{n}`（n 从 1 起）。
/// 文档只要求「server 分配」，未指定形态；全局计数避免 per-session 状态，
/// 与 pending_rpc 表（§8）以字符串匹配。
pub fn alloc_rpc_id(&self) -> String;
```

方法面映射（§4.3 表）：

| Action | JSON-RPC | params |
|---|---|---|
| `session/prompt` | `session/prompt` | `{sessionId, message}` + `id: rpc_id` |
| `session/cancel` | `session/cancel` | `{sessionId}` + `id: rpc_id` |
| `permission/resolve` | `permission.resolve` | `{permissionId, decision}` + `id: rpc_id` |
| `session/load` | `session/load` | M2 启用（类型面保留） |
| create 序列 | `initialize` → `session/new` | initialize 透传（machine 保持 dumb，§3.1） |

`cwd` 注入：【决策】M1 默认目录 = **server 进程工作目录**（常驻进程由托管系统设定，是「已认证上下文」可得的唯一稳定目录；ws 无法获取 TUI 本地 cwd）。客户端传入 cwd 的合法性校验：绝对路径 + 无 NUL/控制字符 + 长度 ≤ 4KB；合法性只做形态校验，存在性由 machine spawn 结果判定（失败走 `AGENT_UNAVAILABLE`）。

---

## 5. channel/connection_registry.rs —— 配额与连接上下文（§8.6/§9.5）

```rust
/// 连接配额（§8.6 默认 200）+ 连接上下文生命周期。超配额以 1013 关闭（§4.7）。
pub struct ConnectionRegistry { quota: usize }

/// 注册连接（认证**前**占位，防未认证连接占满配额；认证失败释放）。
pub fn register(&self, ctx: ConnectionCtx) -> Result<ConnHandle, RegistryFull>;
/// 连接结束释放。
pub fn unregister(&self, conn_id: ConnId);
/// 在线连接数（§17.1 指标）。
pub fn online(&self) -> usize;

/// 连接句柄（发送侧 + 生命周期 id；`ConnectionCtx` 携带 token_id/role/peer/
/// hostname/established_at，§9.5 token 即身份）。
pub struct ConnHandle { pub id: ConnId, pub ctx: ConnectionCtx }
```

- 非回环拒绝（§9.5）在 gateway accept 时用 `Config::allow_peer`（F2 已实现），不进注册表。
- `ConnectionCtx`（F2 auth 已定义）复用，不新增字段。

---

## 6. channel/session_channel.rs —— 客户端连接归一化（§4.6）

每个 client 连接一个 `SessionChannel`，承载连接级协议状态；`relayReady` 前 Action 进入**有界缓冲**（§4.6：不处理；上限 = 命令队列上限 64，超限按 `RATE_LIMITED` 回 error 不排队），`ready` 后 flush。

```rust
/// 客户端连接的会话归一化层（§4.6 步骤 4 + 帧分派）。
pub struct SessionChannel {
    ctx: ConnectionCtx,
    relay_ready: bool,
    pending: VecDeque<ActionEnvelope>, // ready 前有界缓冲（≤64）
    subscriptions: HashSet<DocId>,     // ysync.subscribe 状态（§4.2）
}

/// 入帧分派（单帧同步方法；I/O 经注入的依赖句柄）。
///
/// 分派规则：auth 后首帧必须是 ysync.subscribe/action（其余 → UNSUPPORTED_FRAME
/// 或断开）；M1 白名单检查（proto `m1_check`，§4.8）在 gateway 先行，本方法
/// 只处理已放行的帧。
pub fn dispatch(&mut self, frame: Frame, deps: &ChannelDeps) -> DispatchResult;

/// ready 握手完成（§4.6 步骤 4）：置 relayReady = true，返回待 flush 的缓冲 Action。
pub fn mark_ready(&mut self) -> Vec<ActionEnvelope>;
/// 订阅状态维护（ysync.subscribe/unsubscribe）。
pub fn set_subscriptions(&mut self, docs: Vec<DocId>);

/// 分派依赖（channel 层内部接口，hub 装配注入）。
pub struct ChannelDeps {
    pub coordinator: Arc<CommandCoordinator>,
    pub broadcast: Arc<Broadcaster>,
    pub machine: Arc<MachineRegistry>,
    pub sessions: Arc<SessionRegistry>,
    pub conns: Arc<ConnectionRegistry>,
}
```

---

## 7. channel/command_coordinator.rs —— 串行队列 + 去重 + 两阶段 Ack（§4.3/§4.4/§7.4）

**核心纪律**（§7.4 规则 6 + §4.4）：commandId 去重检查、入队上限检查与 `in_flight` 标记必须在**同一临界区**完成（Rust 无 JS 单线程原子性）；去重记录持久化到 outbox（F3，跨 server 重启有效）。

```rust
/// 每 session 串行命令队列（上限 64，§7.4 规则 1）+ commandId 去重（§4.4）。
pub struct CommandCoordinator {
    gate: Mutex<()>,          // 去重 + 入队 + in_flight 同一临界区（§7.4 规则 6）
    store: Arc<Store>,        // outbox（F3）
    doc: DocManager,          // try_reserve + submit_command（F4）
    machine: Arc<MachineRegistry>,
    sessions: Arc<SessionRegistry>,
    translator: Translator,
    queue_cap: usize,
    /// 执行器：每 session 一个串行 task（§7.4 规则 1；队列满 → RATE_LIMITED）。
    executors: RwLock<HashMap<String, mpsc::Sender<ExecCmd>>>,
}

/// 提交结果（同步返回的部分）：accepted 立即；终态经 oneshot。
pub enum SubmitAck {
    /// 已入队（accepted，§4.4：只表示进入有界处理队列）。
    Accepted { command_id: String },
    /// 已提交命令重发（§4.4）：duplicate + 原 turnId，**不重复调用 Agent**。
    Duplicate(ActionAck),
    /// 同步失败（RATE_LIMITED/SESSION_NOT_FOUND/INVALID_STATE…）→ action_error。
    Failed(ActionError),
}

/// 提交入口：临界区内 去重判定 → try_reserve → outbox.insert(received) → 入队。
pub async fn submit(&self, ctx: &ConnectionCtx, action: ActionEnvelope) -> SubmitAck;

/// 去重判定（§4.4）：committed 记录 → Duplicate；delivery_unknown 非幂等命令
/// 禁止自动重发（路径 B，§4.4）；retryable 失败已 clear_for_retry 的记录 → 放行。
fn dedup_check(&self, command_id: Uuid, session_id: Uuid) -> DedupVerdict;

/// 每 session 执行器（串行消费）：outbox 状态机推进 + 下发 + L1/L2/L3 + 投影。
async fn execute(&self, session_id: &str, cmd: ExecCmd);
```

### 7.1 提交点纪律（§4.4/§6.2，prompt 路径）

```
client action (commandId)
  → [临界区] dedup → try_reserve(64) → outbox.insert(received)     // 同一临界区
  → accepted Ack 立即返回
  → 执行器（该 session 串行）：
      1. outbox.mark_intent_durable()        // 意图落盘（提交点第一步）
      2. Translator.translate（rpcId 登记 pending_rpc: rpc_id → command_id）
      3. machine.forward_rpc / send_spawn     // 下发 ACP
      4. L1+L2：machine 转发确认（spawn_ack / 转发确认）→ mark_dispatched + mark_delivery_confirmed
      5. L3（仅 prompt 前置）：JSON-RPC response（id 匹配 pending_rpc）→
         delivery_confirmed 完成；30s 无 L3 → mark_delivery_unknown（路径 B，§4.4）
      6. 投影 user entry：doc.submit_command(RegisterUserEntry)（挂落盘应答）→
         mark_projection_committed
      7. mark_completed → committed Ack（turnId；create 另带 sessionId）
```

- **顺序不可倒置**（§4.4）：outbox 落盘 → 下发 → L1+L2 → 投影 → committed；投递确认前崩溃 → 无 dispatched 记录 → 客户端重发重新执行，无幽灵 turn；投影前崩溃 → 重发 `duplicate` 由 outbox 兜底。
- 失败路径：`AGENT_UNAVAILABLE`/`MACHINE_OFFLINE`（retryable）→ `mark_failed` + `clear_for_retry`（允许重发，§4.4）+ `action_error(retryable=true)`；`INVALID_STATE`/`FORBIDDEN`/`SESSION_NOT_FOUND` → `mark_failed`（终态）+ `action_error(retryable=false)`。
- `permission/resolve`：先经 DocManager `ResolvePermission` CAS（F4，§7.4 规则 4），迁移成功后才下发 ACP；CAS 已迁移的重发 → `duplicate`。
- `session/cancel`：`cancelling` 状态幂等迁移（§7.2；ACP 侧确认终态由聚合器投影）。
- create 执行器内嵌 §6.2 时序：`send_spawn`（10s 超时）→ spawn_ack → initialize（10s）→ session/new → binding 建立（30s）→ committed(sessionId)；任一步超时 → `AGENT_UNAVAILABLE`(retryable) + 清理半创建状态（补发 kill，§6.2）。
- 队列上限：`DocManager::try_reserve` 已实现 in_flight 计数（F4）；coordinator 临界区先 outbox 去重再 reserve——**session 不存在 → SESSION_NOT_FOUND，队列满 → RATE_LIMITED**。

---

## 8. channel/relay_event_handler.rs —— machine 入站消费与断链清理（§4.5/§8.2/§8.5）

```rust
/// machine 入站事件消费（§4.5）：epoch/seq 校验（防御）→ binding 校验（§6.1）
/// → ACPChannel 规范化 → DocManager.submit_event（F4 单写者 + 微批次 + 落盘）。
pub struct RelayEventHandler {
    doc: DocManager,
    sessions: Arc<SessionRegistry>,   // binding 解析
    channel: AcpChannel,
    /// pending_rpc 表（rpc_id → command_id；L3 确认，§4.4）——与 coordinator
    /// 共享的 in-memory 表（【决策】放本模块，coordinator 登记、本模块匹配）。
    pending_rpc: RwLock<HashMap<String, String>>,
    /// 每 machine 每 session 的 buffer_sync 进行中状态（§8.5 排空纪律）。
    syncing: RwLock<HashMap<(String, String), SyncState>>,
}

/// `machine/event` 消费：epoch 与持久化记录不一致 → 丢弃 + 计数（§4.5.1
/// 防御）；binding 不存在 → 丢弃（§6.5）；其余经 normalize → submit_event。
pub async fn on_machine_event(&self, machine_id: &str, ev: &MachineEvent) -> ConsumeResult;

/// `machine/buffer_sync` 消费（§8.5 补推纪律）：epoch 校验（不一致拒绝整批）
/// → 逐帧按 from_seq 连续性投递（乱序/重复丢弃计数）→ 排空完成判定（见下）
/// → 聚合器 seq 追平写回 gap 清除（F4 已实现 gap_dirty → registry 上报）。
pub async fn on_buffer_sync(&self, machine_id: &str, sync: &MachineBufferSync) -> ConsumeResult;

/// 断链清理（§8.2 矩阵 machine 行 + §7.1 离线即刻生效）：
/// 该 machine 全部活 session：活动 turn → MarkTurnInterrupted（DocCommand）、
/// pending 权限批量 ExpirePermission（复用 CAS，§7.1）、registry.set_session_gap
/// 置标记（缺口数量由补推时聚合器精确计算）。
pub async fn on_machine_disconnect(&self, machine_id: &str) -> Result<(), RelayError>;
```

- **「写 outbox 意图记录」澄清（任务表述偏差）**：machine 入站事件**不进 outbox**（outbox 是命令账本，§4.4）；入站事件的持久化 = 经 DocManager → `UpdateSink` 落 update 日志 + `(epoch, last_seq)` 水位（F3 `append_update`/`WatermarkStore`）——此即补推起点（`from_seq = last_seq + 1`，§8.5）的事实源。本模块的持久化职责即「把规范化事件推进到该链路」。
- **排空完成判定**【决策】：machine 侧排空后恢复实时（§8.5 由 machine 保证）；server 侧以「buffer_sync 帧序列 seq 连续且下一条实时 `machine/event` seq 紧跟」判定排空完成（machine 先发 buffer_sync 再发实时帧的序契约），seq 追平由聚合器判 gap（F4 `judge_stream`）并写回清除。
- **L3 确认**：`normalize` 返回 `RpcResponse{id}` → 查 `pending_rpc` → 匹配成功通知 coordinator（经 in-memory 通知通道）→ `mark_delivery_confirmed` 完成。

---

## 9. channel/broadcaster.rs —— fan-out 与背压（§8.6/§6.4）

```rust
/// 状态广播（§4.2 `ysync.update` S→C 单向）：每连接每 doc 订阅。
///
/// 输入：DocManager 广播 channel（§6.4 观察回调不能 await → 本模块经
/// unbounded 通道接收，背压只作用于此）。
pub struct Broadcaster {
    soft: usize,    // 64KB（§16）
    hard: usize,    // 128KB
    subs: RwLock<HashMap<ConnId, HashMap<DocId, mpsc::Sender<OutboundFrame>>>>,
}

/// 附着 DocManager 广播（hub 装配时调用一次；返回后台 fan-out task 句柄）。
pub fn attach(&self, rx: mpsc::UnboundedReceiver<DocUpdate>) -> JoinHandle<()>;

/// 客户端订阅/退订（ysync.subscribe/unsubscribe 驱动）。
pub fn subscribe(&self, conn_id: ConnId, docs: Vec<DocId>, tx: mpsc::Sender<Frame>) -> Result<(), SubError>;
pub fn unsubscribe(&self, conn_id: ConnId, docs: Vec<DocId>);

/// 每连接发送循环（背压策略，§8.6）：
/// - 队列 ≤ soft（64KB）：直接发；
/// - soft < 队列 ≤ hard：`merge_updates` 合并（proto 层不承担——此处用
///   state 的 `merge_updates_v1` 薄包装，§5.6 隔离范围）或跳过（客户端重连
///   后快照重同步兜底）；
/// - > hard（128KB）：以可恢复错误关闭连接（§4.7 1011 类）。
async fn send_loop(conn_id: ConnId, rx: mpsc::Receiver<OutboundFrame>, ws_tx: ...);
```

- 广播失败只影响连接传递，**不阻塞 ACP 读取循环**（§8.1 原则 4）。
- 订阅语义：`ysync.subscribe { docs }` 的 Doc 集合即 fan-out 目标；首个客户端订阅某 Doc 时由 gateway 侧打开/恢复该 Doc（§4.6 步骤 2——Doc 打开由 `DocManager::open_session` 幂等保证，F4）。

---

## 10. channel/gateway.rs —— ws 生命周期（§4.6/§4.7/§9.2/§9.5）

```rust
/// ws 入口：accept → 配额/回环检查 → 认证 → 角色分派 → 快照时序/机器会话。
pub struct Gateway {
    cfg: Config,
    auth: AuthService,                  // F2：machine 双向 / client 单向
    conns: Arc<ConnectionRegistry>,
    channel: Arc<SessionChannel 工厂>,
    coordinator: Arc<CommandCoordinator>,
    relay: Arc<RelayEventHandler>,
    machine: Arc<MachineRegistry>,
    sessions: Arc<SessionRegistry>,
    broadcast: Arc<Broadcaster>,
    doc: DocManager,
    heartbeat: Heartbeat,
}

/// accept 循环（hub 装配调用；tokio-tungstenite）。
pub async fn run(&self, listener: TcpListener) -> Result<(), GatewayError>;

/// 连接任务（每连接一个）：首帧等待（10s 超时，§4.6）→ 认证 → 分派。
///
/// 认证前不做任何业务处理（§4.6 步骤 1 前 Action 不处理）；认证失败 → 关闭
/// （machine 4502 + 审计计数，§9.2；client 断开）。配额检查在认证前（§8.6）。
async fn connection_task(&self, stream: TcpStream, peer: SocketAddr);
```

### 10.1 连接建立时序（§4.6 客户端）+ 认证（§9.2 machine）

客户端：

1. 配额检查（`ConnectionRegistry::register`，超限 1013）→ `Config::allow_peer`（非回环拒绝，§9.5）；
2. 首帧 `auth {token}`（M1 白名单校验先行，`UNSUPPORTED_FRAME`/`DirectionRejected` 处理，§4.8）；
3. `AuthService::authenticate_client` → `ConnectionCtx`；失败断开 + 计数（§17.1）；
4. 按订阅清单打开/恢复 Doc（`DocManager::open_session` 幂等）→ 推送全量快照（`encode_state_as_update` + 各 Doc `projection_version`）；
5. `ready {projection_versions}` → `SessionChannel::mark_ready` → flush 缓冲 Action；
6. 进入帧循环（`SessionChannel::dispatch`）+ 心跳（§4.7）。

machine：

1. 首帧 `machine/hello`（含 token + nonce）→ `AuthService::authenticate_machine`（§9.2：nonce 防重放 → token 校验 → session_context + HKDF → HMAC）；
2. 下发 `auth_response`（server 身份证明）→ **machine 校验通过前不执行任何 spawn/kill**（§9.2 步骤 3）；
3. `MachineRegistry::on_hello`：注册/fencing（幂等替换，§4.5）→ 补推协调（§11）；
4. 机器会话循环：`machine/heartbeat`（5s，更新 last_heartbeat + alive_sessions）、`machine/event`/`machine/buffer_sync` → `RelayEventHandler`、`spawn_ack`/`kill_ack`/`process_exit` → `MachineRegistry` ack 路由；下行 `machine/spawn`/`kill` + `forward_rpc` 经连接发送。

### 10.2 心跳与关闭码接线（§4.7）

- `Heartbeat::run_for`（§13）：每 5s 下发 `keep_alive`；pong 超时 → 关闭码 4501；
- 关闭码决策（§13 `close_codes.rs`）：
  - 4500：机器离线（`MACHINE_OFFLINE`）——【决策】M1 单 machine 语义：client 连接上 action 分派遇 `MACHINE_OFFLINE` 且连接不再可服务时由 server 关闭（停止自动重连、手动重试）；多 machine 时代改为仅 `action_error`，本设计在 close_codes 中保留策略表；
  - 4502：配置性永久失败（spawn 配置错误 / 认证失败）；
  - 4501：keep_alive 超时；
  - 1011/1013：通用失败 / 配额超限（退避重连）。

---

## 11. control/machine_registry.rs —— 机器注册表与指令下发（§7.1/§4.5）

```rust
/// machine 生命周期（§7.1：REGISTERED → ONLINE ⇄ OFFLINE）+ 指令下发/ack 跟踪。
pub struct MachineRegistry {
    machines: RwLock<HashMap<String, MachineEntry>>,
    offline_timeout: Duration,   // 30s（§16；判定性时间戳 server 权威，§4.7）
}

/// §7.1 状态机。
pub enum MachineState { Registered, Online, Offline }

struct MachineEntry {
    state: MachineState,
    conn: Option<MachineConn>,                 // 在线连接发送句柄（fencing 后替换）
    last_heartbeat: Instant,                   // server 时钟
    pending_acks: HashMap<String, oneshot::Sender<MachineAck>>, // command_id → ack
    hello: MachineHello,                       // buffer_lost/stream_epochs 对账输入
}

/// hello 处理（认证在 gateway 完成；§4.5 幂等替换）：同 machine_id 新连接
/// → 旧连接 fencing（旧连接事件丢弃、关闭）；返回补推协调所需信息。
pub async fn on_hello(&self, machine_id: &str, conn: MachineConn, hello: &MachineHello)
    -> Result<HelloOutcome, MachineError>;

/// 心跳更新（§7.1：5s；alive_sessions 供对账，§8.3）。
pub async fn on_heartbeat(&self, machine_id: &str, hb: &MachineHeartbeat) -> Result<(), MachineError>;

/// 离线判定 tick（与心跳同 tick）：30s 无心跳 → OFFLINE；返回本次离线集合，
/// 由 hub 联动 RelayEventHandler::on_machine_disconnect（§7.1 离线即刻生效）。
pub async fn sweep_offline(&self, now: Instant) -> Vec<String>;

/// 指令下发（§4.5）：发送 + ack 表登记 + 超时（spawn 10s，§16）。
/// 超时 → AgentUnavailable（retryable）；machine OFFLINE → MachineOffline。
pub async fn send_spawn(&self, machine_id: &str, cmd: MachineSpawn) -> Result<SpawnOutcome, MachineError>;
pub async fn send_kill(&self, machine_id: &str, cmd: MachineKill) -> Result<KillOutcome, MachineError>;

/// JSON-RPC 透传（prompt/cancel/resolve 出站；L1+L2 的传输确认由 machine
/// 转发确认承载，§4.4 M1 合并）。
pub async fn forward_rpc(&self, machine_id: &str, msg: &serde_json::Value) -> Result<(), MachineError>;

/// ack 路由（spawn_ack/kill_ack/process_exit，§4.5）：按 command_id 回填 oneshot。
pub fn on_ack(&self, machine_id: &str, command_id: &str, ack: MachineAck) -> bool;
```

**孤儿进程清理钩子（§7.5）**：`on_hello` 返回 `HelloOutcome` 含 `buffer_lost` 与存活清单；hub 据此对「已标记 interrupted 但 machine 声称存活」的 session 调用 `send_kill`（默认清理，Registry 标记「已清理」），**实际进程清理在 machine 侧 kill**——server 只负责下发与 ack 跟踪（任务第 13 项）。

---

## 12. control/session_registry.rs —— 会话状态机与 binding（§7.3/§6.2/§7.6/§8.3）

```rust
/// 会话生命周期（§7.3）+ 可信 binding（§6.1）+ pending_close（§7.6）+ 对账（§8.3）。
pub struct SessionRegistry {
    sessions: RwLock<HashMap<String, SessionEntry>>,
    bindings: RwLock<HashMap<String, String>>,     // acp_session_id → hub session_id
    pending_close: RwLock<HashSet<String>>,        // §7.6 补发集合
}

/// §7.3/§7.6 会话级状态（Registry Doc `sessions.status` 的进程内镜像）。
pub enum SessionState { Accepting, Ended, Closed, Crashed, Gap, PendingClose }

pub struct SessionEntry {
    state: SessionState,
    machine_id: String,
    acp_session_id: Option<String>,   // binding 建立前为 None
    created_at: DateTime<Utc>,        // server 权威时钟
    updated_at: DateTime<Utc>,
}

/// binding 建立（§6.2：session/new 结果 → acp_session_id → session_id）。
/// 此后该 session 的 ACP 帧才允许投影（binding 前到达的帧一律丢弃，§6.2）。
pub async fn bind(&self, session_id: &str, acp_session_id: &str) -> Result<(), SessionError>;

/// binding 查询（RelayEventHandler/ACPChannel 投递前校验，§6.1 规则 5：
/// acp_session_id 只用于协议投递，不能成为 Doc 名/广播频道/缓存键）。
pub fn resolve(&self, acp_session_id: &str) -> Option<String>;

/// machine offline 时的 close（§7.6）：MACHINE_OFFLINE（retryable）+ pending_close
/// 标记（Registry 状态写回）。
pub async fn request_close_offline(&self, session_id: &str) -> Result<(), SessionError>;

/// 重连后：pending_close 集合自动补发 kill（§7.6）+ alive_sessions 对账（§8.3
/// 步骤 5）：输出摘要（存活/缺失/意外存活），意外存活 → §7.5 kill 裁决，
/// 已 close → 补发 kill；Chat Doc 级无法对账的置 gap（TUI「载入以校准」）。
pub async fn reconcile_alive(&self, machine_id: &str, alive: &[String])
    -> Result<ReconciliationReport, SessionError>;

/// 状态迁移 + Registry Doc 同步（create/binding/终态/close 时更新，§5.2 单写）。
pub async fn transition(&self, session_id: &str, state: SessionState) -> Result<(), SessionError>;
```

状态机图（§7.3 会话级 + §7.2 turn 级；turn 状态机主体在 F4 聚合器/终端守卫，coordinator 维护 `commandId → turnId` 映射供 duplicate Ack 与 cancelling 迁移）：

```
session: create → accepting ──binding──► (turn 状态机驱动)
              ACP 进程退出 → ended（终态，视图保留）
              用户关闭    → closed
              进程崩溃    → crashed
              machine 断线 → 活动 turn → interrupted（turn 级）；session → Gap
              close 遇 offline → PendingClose ──machine 重连补发 kill──► Closed
恢复：补推完成、seq 追平 → Gap 清除 → 恢复可用、可开新 turn（§7.3 分区恢复裁决）

turn: accepting → running → completed
                  │  ├── awaiting_permission → running(allow)
                  │  └── cancelling → cancelled（Agent 确认）
                  ├── failed / interrupted（断链/取消超时 10s）
      interrupted 可被带补推序依据的终态事件恰一次校准（§6.3 例外，F4 已实现）
```

---

## 13. control/heartbeat.rs + close_codes.rs —— 心跳与关闭码（§4.7）

```rust
/// keep_alive（§4.7）：server 每 5s 下发 `keep_alive`；pong 超时 → 4501 关闭。
pub struct Heartbeat { interval: Duration, timeout: Duration }

/// 每连接心跳任务（gateway 接线）。
pub async fn run_for(&self, conn_id: ConnId, send: mpsc::Sender<Frame>,
                     recv: &mut mpsc::Receiver<Frame>) -> HeartbeatOutcome;
// 【决策】pong 超时 = 3 × interval（15s）：文档仅定义「超时未回以 4501 关闭」，
// 未给判定时长；与 machine 离线 30s 解耦（keep_alive 只测连接活性，不判机器）。

/// 关闭码（§4.7 表）。
pub const CLOSE_MACHINE_OFFLINE: u16 = 4500;    // 停止自动重连，手动重试
pub const CLOSE_KEEPALIVE_TIMEOUT: u16 = 4501;  // 不在后台自动重连
pub const CLOSE_CONFIG_FATAL: u16 = 4502;       // 配置性永久失败（spawn 配置错误/认证失败）
// 1011 通用失败 / 1013 配额超限（标准码，退避重连）

/// 关闭码 → 客户端重连策略（§4.7 表；gateway 关闭前选择）。
pub fn reconnect_policy(code: u16) -> ReconnectPolicy;  // Stop / ManualOnly / Backoff
```

---

## 14. control/hub.rs —— 装配（run_with 扩展）与优雅关闭（§8.6）

```rust
/// 控制面装配：全部组件实例化与接线（§12 目录结构 + §8.6）。
pub struct Hub {
    pub store: Arc<Store>,
    pub doc: DocManager,
    pub coordinator: Arc<CommandCoordinator>,
    pub relay: Arc<RelayEventHandler>,
    pub machine: Arc<MachineRegistry>,
    pub sessions: Arc<SessionRegistry>,
    pub conns: Arc<ConnectionRegistry>,
    pub broadcast: Arc<Broadcaster>,
    pub gateway: Gateway,
}

/// 装配入口（main `run_with` 的替换，§3.3 f2 骨架的扩展点）：
///
/// 1. `Store::open` + `Store::recover`（§8.4.1 恢复不变量：outbox 先行重建去重
///    索引 → last_seq 对齐 → Doc 补齐；失败 → degraded）；
/// 2. `DocManager::new(BatchConfig, sink)`——sink：【决策】若 F6 未提供
///    `UpdateSink` 生产实现，本 feature 提供薄 adapter（`Store` 落 update 日志
///    + `(epoch, last_seq)` 水位，F3 接口）；否则复用 F6 实现；
/// 3. 各注册表/协调器/广播器实例化 → `Gateway::run(listener)`；
/// 4. 后台任务：心跳 tick（keep_alive + machine 离线 sweep + nonce sweep 同 tick，
///    §4.7 判定性时间戳 server 权威）、broadcaster attach、恢复对账任务。
pub async fn run_server(cfg: &Config, store: Arc<Store>) -> anyhow::Result<()>;

/// 优雅关闭（§8.6 顺序）：停止接收新 Action → 完成或中断在途提交（outbox
/// 保留 dispatched 记录供重启后裁决）→ 释放引用 → 关闭全部连接。
pub async fn shutdown(self, signal: impl Future<Output = ()>) -> ...;

/// Degraded 判定入口（§17.2）：`RegistryState::global_status()`——Degraded/
/// Restarting 时 gateway 拒绝新 committed 承诺（新 Action 返回 retryable 错误，
/// §8.4 落盘失败语义同源）；Healthy 恢复。
pub fn can_accept_committed(&self) -> bool;
```

- main.rs `run_with` 的占位 loop 替换为：`Store::open` → `recover` → `Hub::run_server` + signal 等待（SIGINT/SIGTERM）→ `shutdown`。监听默认 `127.0.0.1:8456`（§16，F2 Config）。
- 恢复期（`Restarting`）：允许展示只读视图、禁止新 committed 承诺；machine 重连（hello）后才从 `unknown` 转 online/offline 并触发对账（§8.4.1 不变量 4——`RegistryState::set_restarting`/`clear_restarting` 已提供，F4）。

---

## 15. 断链与恢复汇总（§8.2/§8.3/§8.5 + 本设计归属）

| 场景 | 触发 | 处理（归属） |
|---|---|---|
| TUI 断开 | ws 断 | 不对 session 做任何清理（§8.2）；重连后快照 + 增量追平（§10.1 步骤 4/5） |
| machine 网络分区 | 连接断 / 心跳 30s 超时 | `MachineRegistry::sweep_offline` → `RelayEventHandler::on_machine_disconnect`（turn → interrupted、权限批量 expired、session 置 gap，§7.1/§8.2） |
| machine daemon 崩溃 | hello 带 `buffer_lost` + 新 `stream_epochs` | epoch 变化 → 不可校准缺口（聚合器 `UncalibratableGap`，F4）；孤儿进程默认 kill 清理（§7.5，§11 钩子） |
| ACP 进程退出 | `machine/process_exit` | 终态写视图（F4 `SetSessionTerminal`）；不再接受新事件；缓冲清理 |
| server 崩溃重启 | `Store::recover` + machine 重连 | 恢复不变量（§8.4.1）→ hello 对账 epoch：相同 → `buffer_sync`（`from_seq = last_seq + 1`）；变化 → 不可校准 gap → `alive_sessions` 对账（§12） |
| close 遇 offline | `session/close` | `MACHINE_OFFLINE` + `pending_close`；重连自动补发 kill（§7.6/§12） |

补推数据流：`machine/buffer_sync` → `RelayEventHandler::on_buffer_sync`（epoch 校验 → 逐帧规范化 → `DocManager.submit_event`）→ 聚合器 seq 追平清 gap → session 恢复可用（F4 gap_dirty → registry 写回）。

---

## 16. 测试清单

### protocol/acp_channel（纯函数单测，无 I/O）
1. 双格式 sessionId：`{type,payload}` 三种路径、JSON-RPC `params.sessionId`/`params.session_id`、均缺失 → `Dropped(NoSessionId)`；
2. 事件映射全表（§6.1 13 行 + RpcResponse）：字段提取、`expires_at` 由注入时钟生成（5min）；
3. `agent_reasoning_chunk` 别名 → ReasoningDelta；
4. 未知 type → `Dropped(UnsupportedFrame)`（不 panic 不静默）；非对象帧 → Malformed；
5. `RpcResponse`：id 提取 + is_error 区分（L3 输入）。

### protocol/translator
6. 方法面映射（prompt/cancel/resolve）；rpcId 分配单调且注入 `id`（非 notification）；
7. cwd 注入：默认目录 / 客户端 cwd 校验（相对路径拒绝、NUL 拒绝）；create 序列两段式（initialize → session/new 由 coordinator 驱动，translator 单步验证）。

### channel/connection_registry
8. 配额 200：第 201 个 → RegistryFull（1013）；认证失败释放后可用；unregister 幂等。

### channel/command_coordinator（fake client + fake machine 或纯内存依赖）
9. 串行性：同 session 并发 10 命令按序执行；队列满（64）→ `RATE_LIMITED`；
10. commandId 去重：prompt 重发 → `duplicate` + 原 turnId，**不重复调用 Agent**（L2 调用计数断言）；permission 重复应答 → `duplicate`；
11. 提交点顺序：outbox 落盘（intent_durable）先于下发、投影（projection_committed）先于 committed Ack（状态序列断言）；
12. 崩溃点 × 重试（§4.4 表）：dispatched=false 重发重新执行；dispatched=true + 无 L3 → `delivery_unknown` 且**非幂等禁止自动重发**；retryable 失败（AGENT_UNAVAILABLE）→ clear_for_retry 后可重发；
13. L3：rpcId 匹配响应 → delivery_confirmed 完成；30s 无响应 → delivery_unknown（路径 B）；
14. create 时序：spawn_ack 失败 → AGENT_UNAVAILABLE + 清理（补发 kill）；binding 超时 30s → 同码 + 无幽灵视图。

### channel/relay_event_handler
15. epoch 不一致帧丢弃计数（§4.5.1 防御）；binding 不存在帧丢弃（§6.5）；seq 乱序 → 聚合器拒绝（F4）；
16. buffer_sync：epoch 不符整批拒绝；from_seq 连续性、追平后 gap 清除（registry mock 断言）；排空完成判定（实时 event seq 紧跟）；
17. 断链清理：活动 turn → interrupted、pending 权限批量 expired（CAS 断言）、session 置 gap（registry mock）。

### channel/broadcaster
18. fan-out：两连接订阅不同 doc 集互不串扰；退订后不再收；
19. 背压：>64KB 合并（`merge_updates_v1` 断言单帧输出）；>128KB 慢连接断开；广播失败不阻塞 DocManager（§8.1 原则 4）。

### channel/gateway（fake client ws）
20. 握手时序：auth 错误 token → 断开（无任何数据）；auth 后先 subscribe → 快照 → ready → 缓冲 Action flush 顺序（§4.6 步骤 1–4 断言）；
21. ready 前 Action 缓冲、ready 后 flush；缓冲超限 → RATE_LIMITED；
22. 非回环拒绝（`allow_non_loopback=false` 时回环可连、非回环拒绝，§9.5）；配额 1013；
23. keep_alive：5s 收 ping、pong 回执、超时 4501 关闭；未知帧 → `UNSUPPORTED_FRAME`；客户端上行 `ysync.update` → 拒绝（§5.6，白名单 `DirectionRejected`）。

### control/machine_registry
24. 生命周期：hello → REGISTERED/ONLINE；心跳续期；30s 超时 → OFFLINE（时间用注入时钟）；重连心跳 → ONLINE；
25. hello fencing：同 machine 新连接 → 旧连接关闭、旧连接事件丢弃（§4.5 幂等替换）；
26. spawn/kill ack 跟踪：ack 回填 oneshot；10s 超时 → AgentUnavailable；OFFLINE 时下发 → MachineOffline；
27. §7.5 孤儿清理钩子：buffer_lost + 存活清单 → 下发 kill 断言（fake machine 收 kill 并 ack）。

### control/session_registry
28. binding：session/new 结果 → bind 生效（resolve 命中）；bind 前帧被丢弃路径断言（§6.2）；
29. pending_close：offline close → 标记 + MACHINE_OFFLINE；重连 → 自动补发 kill → 清标记（§7.6）；
30. 对账：alive_sessions 与 Registry 比对（存活/缺失/意外存活）→ 摘要 + 意外存活 kill 断言（§8.3 步骤 5）。

### 集成（fake machine ws 客户端，`server/tests/common/fake_machine.rs`）
31. **fake machine**：tokio-tungstenite 客户端，实现 hello（含 nonce + 校验 `auth_response` HMAC，字节级向量复用 proto 测试）、心跳、事件转发、缓冲补推、指数退避重连、kill_on_drop 语义（测试工具，非产品代码）；
32. 全链路：client create → spawn → binding → prompt → L1/L2/L3 → committed → 事件聚合投影可见（yjs 读断言）；
33. 断线恢复：fake machine 断开 → turn interrupted + gap；重连 → hello 对账 → buffer_sync 追平 → gap 清除 → 新 prompt 可用（§8.3/§7.3）；
34. epoch 变化分支：daemon 重启模拟（新 epoch + buffer_lost）→ 不可校准 gap、不按旧 seq 补推（§4.5.1）；
35. server 重启：kill -9 server → fake machine 缓冲 → 重启 → buffer_sync 追平（P3 演练，§4.8 测试向量 5）；
36. 白名单：未知 `t` → UNSUPPORTED_FRAME；双向认证：重放旧握手/错误角色 → 拒绝 + 审计计数（§4.8 向量 6/8/12）。

---

## 17. 关键决策摘要

1. **ACPChannel 纯函数化**：`normalize(session_id, epoch, seq, now, frame) -> NormalizeOutcome` 无 I/O——与 F4 `Aggregator::apply` 同构，binding 校验与持久化在调用方（RelayEventHandler）；未知帧返回 `Dropped(reason)` 供计数，不 panic 不静默；`RpcResponse` 作为 L3 确认的专门面，避免把 JSON-RPC response 误当业务事件。
2. **去重 + 入队同一临界区（§7.4 规则 6）**：`CommandCoordinator` 以 `Mutex` 包住「outbox 去重判定 → `DocManager::try_reserve`（64）→ `outbox.insert(received)`」三连，杜绝 Rust 并发下重发穿透去重表；accepted Ack 在插入后立即返回，提交点顺序（intent_durable → 下发 → L1+L2 → L3 → 投影 → committed）由执行器保证。
3. **L1+L2 与 L3 解耦（§4.4 M1 语义）**：create/close 的 delivery_confirmed 只要求 L1+L2（spawn_ack/kill_ack/转发确认）；prompt 额外以「JSON-RPC response 匹配 Translator 分配的 rpcId」为 committed 前置，30s 无 L3 → `delivery_unknown`（路径 B：非幂等禁止自动重试）。pending_rpc 表（rpc_id → command_id）放 RelayEventHandler，coordinator 登记、L3 匹配，两模块以注入共享。
4. **buffer_sync 排空完成的判定**：以「buffer_sync 帧序列 seq 连续且下一实时 `machine/event` seq 紧跟」为契约（machine 侧保证先排空后实时，§8.5），server 不做额外结束帧；gap 的精确计数与追平清除完全交给 F4 聚合器（`judge_stream`/gap_dirty → registry 写回），F5 只负责断链时置 gap 标记与补推投递。
5. **控制面单一装配点 `Hub`**：`run_with` 只做「Store 恢复（§8.4.1）→ DocManager（含 UpdateSink 薄 adapter，若 F6 未提供）→ 注册表/协调器 → Gateway.run」；心跳 tick、machine 离线 sweep、nonce sweep 合并为单一周期任务（判定性时间戳 server 权威，§4.7）；Degraded 入口 = `RegistryState::global_status()`，非 Healthy 时拒绝新 committed 承诺；孤儿进程清理 = server 下发 kill + ack 跟踪（实际清理在 machine 侧）。
6. **冲突记录**：任务描述 `agent_reasoning_chunk` vs 架构 §6.1 表 `agent_thought_chunk`——以架构文档为准，实现接受两者为别名；任务所述「RelayEventHandler 写 outbox 意图记录」与 §4.4 语义冲突（outbox 是命令账本）——已澄清为「事件经 DocManager → UpdateSink 落 update 日志 + 水位」（§8）。
