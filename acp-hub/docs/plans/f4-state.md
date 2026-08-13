# F4 设计：server 状态层（state：Y.Doc 聚合）

> 状态：设计稿（对应 Feature F4）
> 日期：2026-08-07
> 权威来源：`docs/architecture.md`（v2.4）§5.1–5.6 / §6.1 / §6.3 / §6.4 / §6.5 / §7.2 / §7.3 / §7.4 / §8.5 / §12 / §17.2
> 前置依赖：`acp-hub-proto`（F1 已交付：三 Doc schema 镜像 `proto/src/schema/*`、`DocId`、版本常量、machine 帧含 `MachineEvent`）
> 约束：**忠于架构文档，不引入文档外的协议与语义**；只操作 `server/src/state` 模块文件；不修改 `lib.rs`/`Cargo.toml`/`docs/architecture.md`；日志走 tracing 且脱敏（不记正文/工具参数/token/密钥）。文档未指明处的命名/参数/阈值均标注「【决策】」并给出依据。

---

## 1. 目标与范围

state 层是 server 的 Y.Doc 聚合面：**把 ACPChannel（F5）产出的规范化事件经幂等聚合投影到每 session 双 Doc（Chat/Session）**，并承担 Registry Doc 的 server 状态源单写、权限 CAS、session 历史列表投影与全局 Degraded 判定。所有 Y.Doc 写入必须经 DocManager 唯一提交边界（§5.6），yrs 并发 panic 由每 session 单写者排除（§7.4）。

**范围**：

1. `NormalizedEvent` 定义（§6.1 事件表全子集 13 种）——state 层定义，供 F5 ACPChannel 产出；
2. `ViewStore`：yrs 薄封装（§5.6 隔离范围）；
3. `DocManager`：doc 生命周期 + 每 session 单写者 + 16ms 微批次 + 控制类先 flush + 广播 channel + 唯一提交边界；
4. `Factory`：doc 创建 + `schema_version` 幂等补结构；
5. `ChatWriter`：doc 写入原语；
6. `Aggregator`：纯函数 `apply(&mut DocPair, &NormalizedEvent) -> ApplyResult`（§12 测试前提），含幂等键、终态守卫（interrupted 校准例外）、gap 状态；
7. `Permission`：CAS（pending → resolved 原子一次；expired）；
8. `SessionList`：10s 轮询全量同步投影（纯函数 diff）；
9. `Registry`：machine 视图 + 活跃 session 摘要 + `global.status`，server 状态源单写接口；
10. P0 契约测试清单。

**边界声明**（不属本 feature）：

- ACPChannel（F5）消费本模块的 `NormalizedEvent` 并产出；本模块不解析原始 ACP 帧；
- persist（F6）实现 update 落盘/outbox；本模块只定义 `UpdateSink` trait 并调用，不实现持久化；
- broadcaster（F7/channel 层）订阅 `DocManager::subscribe_updates()` 做背压与 fan-out；本模块只负责「把 update 经 channel 送出」（§6.4 观察回调不能 await，背压只能作用于 broadcaster 队列）；
- 命令队列/commandId 去重（command-coordinator，F7）经 `DocManager::submit_command` 提交写入命令，本模块不定义去重与 outbox 状态机；
- 10s 轮询调度器（F7）：本模块只提供 `session_list` 投影纯函数与写入原语。

---

## 2. 模块划分与文件布局

占位单文件 `server/src/state.rs` 扩展为目录（实现时 `git rm server/src/state.rs` 后建目录）：

```
server/src/state/
├── mod.rs          # 模块文档 + 公开面收敛（re-export）
├── normalized.rs   # NormalizedEvent：envelope + 13 种事件体（§6.1 事件表全子集）
├── doc_pair.rs     # DocPair { chat, session } + StreamState（epoch/last_seq/gap）——apply 的 &mut 载体
├── view_store.rs   # ViewStore trait + YrsViewStore 实现（yrs 薄封装，§5.6 隔离范围）
├── factory.rs      # Factory：create_*_doc + ensure_schema（schema_version 判空幂等补结构，§5.6）
├── chat_writer.rs  # 写入原语（以 TransactionCtx 为参数；entry 创建/block 追加/Y.Text 增量/reasoning 可见性/tool_call upsert/终态迁移）
├── aggregator.rs   # Aggregator::apply（纯函数，无 I/O 无日志副作用，§6.3）+ ApplyResult/ApplyReason + gap 计算
├── doc_manager.rs  # DocManager：每 session 单写者通道 + 16ms 微批次 + 控制类先 flush + 广播 + 唯一提交边界
├── permission.rs   # CasOutcome + resolve/expire（CAS 原语，供聚合器与命令路径共用）
├── session_list.rs # diff 纯函数 + apply_diff 写入原语（§6.3 全量同步，旧条目删除自愈）
└── registry.rs     # RegistryState：machine 视图/活跃 session 摘要/global.status 单写 + Degraded 判定（§17.2）
```

模块内依赖方向（单向）：`normalized.rs ← aggregator.rs ← doc_manager.rs`；`factory/view_store/chat_writer` 是 doc_manager 的实现细节；`permission/session_list` 被 aggregator 与 doc_manager 复用；`registry.rs` 独立于 per-session 链路，仅经 DocManager 提交（Registry Doc 也是 Doc，受唯一提交边界约束，§5.6）。

公开面（`state/mod.rs` re-export）：`NormalizedEvent`/`EventBody`、`DocManager`、`DocCommand`、`SubmitResult`、`ApplyResult`/`ApplyReason`、`CasOutcome`、`RegistryState`、`DegradeCause`、`UpdateSink`、`DocUpdate`、`ViewStore`、`Factory`。

---

## 3. NormalizedEvent（§6.1 事件表全子集）

**由 state 层定义、供 F5 ACPChannel 产出**（proto `event.rs` 的 `EventFrame.frame` 是此类型的 serde 投影——`events/subscribe` 推送不透明 JSON，结构以本类型为准）。

### 3.1 envelope 与 body 分离

```rust
/// 规范化事件（§6.1）：ACPChannel 产物的统一形态。
///
/// envelope 携带路由与重放序依据：`(session_id, epoch, seq)` 是终态守卫
/// （§6.3）与 gap 计数（§8.5）的输入；body 只含业务字段。事件按 session
/// 路由到对应写者，聚合器校验 envelope.session_id 与自身 session 一致（防串）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedEvent {
    /// hub 侧 session_id（经 binding 翻译，非原始 acp_session_id）。
    pub session_id: String,
    /// machine 侧单调 seq（同 epoch 内；§4.5.1）。
    pub seq: u64,
    /// stream_epoch（machine 侧流代际标识；epoch 变化 → 不可校准缺口）。
    pub epoch: u64,
    pub body: EventBody,
}
```

### 3.2 EventBody（13 种，§6.1 事件表全子集）

字段按 §5.3/§5.4 投影需要裁剪；时间字段为 RFC3339 字符串（server 权威时钟生成，§4.7——由产出方 ACPChannel / 命令路径填充，聚合器照写不生成）。序列化 tag `"type"`、camelCase，与线协议约定一致。

```rust
/// 事件体（§6.1 事件表全子集）。serde tag `"type"`（对齐 ACP `{type, payload}` 习惯）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventBody {
    /// 文本增量 → Chat Doc entry block（Y.Text 追加；微批次合并，§6.4）。
    /// 字段: turn_id, entry_id, block_id, text
    MessageDelta {
        turn_id: String,
        entry_id: String,
        block_id: String,
        text: String,
    },

    /// 思考/推理增量 → Chat Doc reasoning block；按可见性写 summary/hidden，
    /// hidden 绝不发给无权客户端（§5.3）。
    /// 字段: turn_id, entry_id, block_id, text, visibility
    ReasoningDelta {
        turn_id: String,
        entry_id: String,
        block_id: String,
        text: String,
        visibility: BlockVisibility,
    },

    /// 用户消息（服务端单写注册，§6.5）：`session/prompt` 处理时注册 turnId 并创建
    /// user entry，ACP 的 `user_message_chunk` 以此映射。幂等：同 turn_id 重放跳过。
    /// 字段: turn_id, entry_id, text, author_user_id?, created_at
    UserMessage {
        turn_id: String,
        entry_id: String,
        text: String,
        author_user_id: Option<String>,
        created_at: String,
    },

    /// 工具调用开始 → Chat Doc tool_calls（按 tool_call_id 创建，幂等）。
    /// 字段: turn_id, tool_call_id, name, status, arguments?, created_at
    ToolCallStarted {
        turn_id: String,
        tool_call_id: String,
        name: String,
        arguments: Option<serde_json::Value>,
        created_at: String,
    },

    /// 工具调用更新 → tool_calls upsert（arguments 有值时全量覆盖，缺省保留旧值；status? 经
    /// 服务端单调状态机迁移，旧事件缺失 status 时仅更新参数）。
    /// 字段: turn_id, tool_call_id, status?, arguments?
    ToolCallUpdated {
        turn_id: String,
        tool_call_id: String,
        arguments: Option<serde_json::Value>,
    },

    /// 工具调用完成 → tool_calls 状态迁移 Completed/Error（upsert）。
    /// 超大 result 不写内容；ToolCallProjection 显式记录 result_omitted 与
    /// result_bytes，避免与 ACP 真正返回空结果混淆（截断策略见 §9.5）。
    /// 字段: turn_id, tool_call_id, result?, public_error?
    ToolCallCompleted {
        turn_id: String,
        tool_call_id: String,
        result: Option<serde_json::Value>,
        public_error: Option<PublicError>,
    },

    /// 权限请求 → Session Doc pending_permissions（按 permission_id upsert）。
    /// 字段: permission_id, turn_id, tool_call_id?, tool?, title, description?, options, expires_at
    PermissionRequested {
        permission_id: String,
        turn_id: String,
        tool_call_id: Option<String>,
        /// 官方 request 内完整 toolCall 快照；permission-first 时与权限投影
        /// 同一 seq 原子创建工具卡。旧日志缺失时默认 None。
        tool: Option<PermissionToolSnapshot>,
        title: String,
        description: Option<String>,
        options: Vec<PermissionOptions>,
        expires_at: String,   // server 权威时钟（§4.7）
    },

    /// 权限解决 → pending_permissions CAS：仅 pending → resolved 原子迁移一次
    /// （§7.4 规则 4），迁移成功后 decision 写入；重复回答幂等返回（§10）。
    /// 字段: permission_id, decision
    PermissionResolved {
        permission_id: String,
        decision: PermissionDecision,
    },

    /// 权限过期 → pending → expired（CAS；decision 保持 null，§5.4）。
    /// 来源两条：ACP 事件流 / server 定时器（§4.7 判定性时间戳）——都落到同一 CAS 原语。
    /// 字段: permission_id
    PermissionExpired { permission_id: String },

    /// Agent 状态覆盖 → Session Doc agent.status/public_error（§6.3）。
    /// 能力未确认前保持不可用（见 Capabilities）。
    /// 字段: status, public_error?
    AgentStatus {
        status: String,
        public_error: Option<PublicError>,
    },

    /// 能力声明覆盖 → Session Doc agent.capabilities。
    /// 字段: capabilities
    Capabilities { capabilities: Vec<String> },

    /// Session 元信息覆盖 → Session Doc session（title/status/active_turn_id）。
    /// 字段均 Option：缺省字段不覆盖（部分更新）。
    SessionInfo {
        title: Option<String>,
        status: Option<SessionStatus>,
        active_turn_id: Option<String>,
    },

    /// `session_list` 响应 → Session Doc sessions（agent 磁盘历史，全量同步投影，
    /// §5.2 裁决：与 Registry 活跃会话语义不同、互不替代）。
    /// 字段: entries（响应中不存在的旧条目删除，自愈）
    SessionListResponse { entries: Vec<SessionSummaryProjection> },

    /// Turn 终态（completed/failed/cancelled/interrupted）→ Chat Doc entry 终态迁移
    /// + Session Doc active_turn 更新（§7.2）。终态立即写入；之后的同 turn 增量丢弃
    /// （interrupted 例外：带 envelope 重放序依据恰一次校准，§6.3）。状态仅限终态四值。
    /// 字段: turn_id, status, completed_at, public_error?
    TurnTerminal {
        turn_id: String,
        status: TurnStatus,   // 【决策】取值限定 Completed | Failed | Cancelled | Interrupted（终态集，§7.2）
        completed_at: String,
        public_error: Option<PublicError>,
    },
}
```

> 【决策】`TurnTerminal.status` 用全 `TurnStatus` 枚举（proto 已定稿）但文档约束只出现终态四值；产出方（F5）负责约束，聚合器对非终态值按 `InvalidTerminalStatus` 拒绝（防御）。
> 【决策】`SessionInfo` 字段全 Option（部分更新语义）——架构「覆盖当前状态」未指明全量/部分，部分更新避免标题更新与 agent 状态更新互相踩踏。

---

## 4. DocPair 与 StreamState

```rust
/// 每 session 的双 Doc 组合 + 流状态（§5.2 / §8.5）。
///
/// 由 Factory 创建；只允许被该 session 的单写者 writer task 独占（§7.4）。
/// `&mut DocPair` 是聚合器纯函数 `apply` 的载体（§12 测试前提：内存 Y.Doc）。
pub struct DocPair {
    /// `chat:{session_id}`（§5.3，高频内容流）。
    pub chat: yrs::Doc,
    /// `session:{session_id}`（§5.4，低频控制状态）。
    pub session: yrs::Doc,
    /// 聚合器流状态（不进 yrs：可丢弃镜像不承载校准事实，§8.1 原则 5）。
    pub stream: StreamState,
}

/// 聚合器流状态：gap 计算与 interrupted 校准的重放序水位（§8.5 / §6.3）。
///
/// 启动时从 persist 的 `(epoch, last_seq)` 水位（F6）恢复；运行期内存维护，
/// 与 update 日志落盘同步更新（随提交 flush 交给 persist）。
pub struct StreamState {
    /// 当前流纪元（§4.5.1）；与 machine/event 帧 epoch 不一致 → 帧丢弃并计数。
    pub epoch: u64,
    /// 已应用的最大 seq（同 epoch 单调；校准与 gap 判定依据）。
    pub last_seq: u64,
    /// 累计缺口帧数（seq 跳变增量；追平后清零）。
    pub gap_count: u64,
    /// epoch 变化/缓冲丢失触发 → 不可校准缺口（§8.5 uncalibratable）。
    pub uncalibratable: bool,
    /// 待上报的 gap 变化（上次上报后是否有 gap_count/uncalibratable 变化）。
    pub gap_dirty: bool,
}
```

> 关键决策：**校准事实（gap/uncalibratable/终态校准状态）不写入 Chat/Session Doc**——Y.Doc 是可丢弃镜像（§8.1 原则 5），doc 内只存视图所需的权威投影（`active_turn`、entry.status 等，§7.2）；gap 的唯一视图落点是 Registry Doc `sessions[].gap`（§5.5，由 RegistryState 单写，见 §12.4）。

---

## 5. ViewStore（yrs 薄封装，§5.6 隔离范围）

聚合器与 doc 生命周期经 `ViewStore` 隔离 yrs；persist/gateway/broadcaster 直接接触 yrs 类型但以薄封装函数收敛（§5.6：`encode_state_as_update`/`merge_updates_v1`——后两者定义为 free function，见 §5.2）。

```rust
/// 聚合器可见的 yrs 抽象（§5.6「ViewStore trait 只隔离聚合器」）。
///
/// 承诺边界：聚合器与 doc 生命周期管理不直接命名 `yrs::Doc` API（除事务别名
/// `TransactionCtx`，见下）；写操作一律经 [`crate::state::chat_writer`] 原语。
pub trait ViewStore {
    /// 导出全量状态更新（快照推送 / persist 首写）。
    fn encode_state_as_update(&self) -> Vec<u8>;
    /// 应用外部 update（启动重放，§8.4.1 恢复路径；聚合器运行期不调用）。
    fn apply_update(&self, update: &[u8]) -> Result<(), ViewStoreError>;
    /// 注册 update 观察：yrs 回调是同步的、不能 await（§6.4），此处把 update
    /// 经 unbounded channel 送出；背压作用于下游 broadcaster 队列（F7）。
    fn observe_update(&self, tx: mpsc::UnboundedSender<Vec<u8>>) -> ViewStoreSubscription;
    /// 事务入口：聚合器在闭包内经 writer 原语写入（单事务边界，§6.4「一次
    /// Y.Doc transaction 写入」）。禁止跨 await 持有（§7.4）。
    fn with_txn<R>(&mut self, f: impl FnOnce(&mut TransactionCtx<'_>) -> R) -> R;
}

/// yrs 事务别名（隔离聚合器对 yrs 的直接命名；实现细节在 view_store.rs）。
pub type TransactionCtx<'a> = yrs::TransactionMut<'a>;

/// yrs 0.27 的具体实现（state 模块内部细节）。
pub struct YrsViewStore {
    doc: yrs::Doc,
    subscription: Option<yrs::Subscription>,
}

/// 薄封装 free function（§5.6：persist/gateway/broadcaster 直接接触 yrs 类型的收敛点）：
///   pub fn encode_state_as_update(doc: &yrs::Doc) -> Vec<u8>;   // Y.encodeStateAsUpdate
///   pub fn merge_updates_v1(updates: &[Vec<u8>]) -> Result<Vec<u8>, ViewStoreError>;  // Y.mergeUpdatesV1
```

> 【决策】`ViewStore` 用 trait 而非具体结构：§5.6 原文「`ViewStore` trait」，且为聚合器测试保留替身可能（但 §12 P0 契约测试直接用内存真实 DocPair，无需 mock——trait 单实现 `YrsViewStore`，不引入抽象代价）。
> 【决策】事务句柄经别名暴露 `TransactionCtx`，聚合器代码不出现 `yrs::` 路径（隔离承诺的落点：yrs API 变动只影响 `view_store.rs`/`chat_writer.rs`/`factory.rs` 三文件）。

---

## 6. Factory（doc 创建 + schema_version 幂等补结构）

```rust
/// Doc 创建与结构补齐（§5.6 schema_version/projection_version 分离；§8.4.1「Doc 补齐」）。
///
/// 不假设旧快照完整：重放后以 schema_version 判空幂等补结构，缺失键补空结构、
/// 不覆盖已有数据；旧客户端忽略未知字段仍安全（服务端是唯一写入者）。
pub struct Factory {
    /// 各 Doc 的当前 schema_version（proto 版本常量，§5.3/5.4/5.5）。
    chat_schema: u32, session_schema: u32, registry_schema: u32,
}

impl Factory {
    /// 创建空 Chat/Session Doc（仅根 Map + schema_version/projection_version = 0，
    /// 其余结构惰性补齐由 ensure_schema 完成）——或直接建全结构（M1 取后者，简单）。
    pub fn create_chat_doc(&self) -> DocPair;         // chat + session + StreamState::default
    pub fn create_registry_doc(&self) -> yrs::Doc;

    /// 幂等补结构：读根 Map `schema_version`——
    /// 缺失 → 写入当前版本 + 全结构；相等 → 检查必需键、缺失者补空结构；
    /// 大于当前 → `FactoryError::FutureSchema`（启动恢复不变量失败路径，上报 §12.5 degraded）。
    /// 任何路径都不覆盖已存在数据。
    pub fn ensure_schema(&self, doc: &mut yrs::Doc, kind: DocKind) -> Result<(), FactoryError>;
}
```

**物理映射表**（§5.3 原文，`chat_writer`/`factory` 共同遵守）：

| 结构 | yrs 类型 | 键/位置 |
|------|---------|--------|
| Chat 根 `schema_version`/`projection_version` | 标量（u32） | 根 `Y.Map` |
| `entry_order` | `Y.Array<String>` | 根 |
| `entries` / `tool_calls` | `Y.Map<String, Y.Map>` | 根 |
| entry 的 `block_order` | `Y.Array<String>` | entry Map |
| entry 的 `blocks` | `Y.Map<String, Y.Map>` | entry Map |
| Text/Reasoning 块的正文 | `Y.Text` | block Map 的 `text` 值位（避免每 token 替换完整字符串，§5.3） |
| Session 根、`session`/`agent`/`active_turn`/`pending_permissions`/`sessions` | `Y.Map` | 根/子 Map |
| Registry 根、`machines`/`sessions`/`global` | `Y.Map` | 根 |

删除语义：领域 tombstone（状态位迁移/条目移除由投影写回驱动），客户端不物理删除权威记录（§5.3）。

---

## 7. ChatWriter（doc 写入原语）

以 `TransactionCtx` 为参数的一组**纯 yrs 操作**（不持有 doc、不做守卫判定——守卫在聚合器）；每条原语幂等或由调用方保证幂等（§6.3）。

```rust
/// doc 写入原语（§5.3 物理映射的执行层）。所有函数在调用方事务内执行；
/// 不做幂等/终态判定（判定归 aggregator），只保证「写出的结构合法」。
pub mod chat_writer {
    /// 确保 entry 存在（entry_id 幂等：已存在返回 false，不覆盖）。
    pub fn ensure_entry(txn: &mut TransactionCtx, root: &yrs::MapRef, entry: &ChatEntry) -> bool;
    /// 创建 user entry（turn_id 幂等：同 turnId 已存在则跳过，§6.5「同 turnId 重放跳过」）。
    pub fn create_user_entry(txn: &mut TransactionCtx, root: &yrs::MapRef,
        ev: &UserMessage, entry_id: &str) -> bool;
    /// 创建 assistant/system entry 骨架 + 首块（message_delta 的 entry 未知时由聚合器先建）。
    pub fn ensure_entry_with_blocks(txn: &mut TransactionCtx, root: &yrs::MapRef,
        entry_id: &str, kind: EntryKind, role: EntryRole, turn_id: Option<&str>, created_at: &str) -> bool;
    /// 追加内容块（block_id 幂等），返回块引用。
    pub fn append_block(txn: &mut TransactionCtx, root: &yrs::MapRef,
        entry_id: &str, block: ContentBlock) -> bool;
    /// 文本增量追加：block 不存在则先建（block_id 幂等），`Y.Text` insert（block 尾部）。
    pub fn append_text_delta(txn: &mut TransactionCtx, root: &yrs::MapRef,
        entry_id: &str, block_id: &str, delta: &str, kind: ContentKind) -> bool;
    /// reasoning 可见性设置（summary/hidden；hidden 绝不发给无权客户端，§5.3）。
    pub fn set_reasoning_visibility(txn: &mut TransactionCtx, root: &yrs::MapRef,
        block_id: &str, visibility: BlockVisibility) -> bool;
    /// tool_call upsert（tool_call_id 幂等：存在则更新字段，不存在则创建）。
    pub fn upsert_tool_call(txn: &mut TransactionCtx, root: &yrs::MapRef, tc: &ToolCallProjection) -> bool;
    /// entry 终态迁移（status/completed_at/error；Chat Doc 侧）。
    pub fn migrate_entry_terminal(txn: &mut TransactionCtx, root: &yrs::MapRef,
        entry_id: &str, status: EntryStatus, completed_at: &str, error: Option<&PublicError>) -> bool;
    /// active_turn 更新（Session Doc 侧；§7.2 权威投影）。
    pub fn set_active_turn(txn: &mut TransactionCtx, root: &yrs::MapRef,
        active: Option<&ActiveTurnProjection>) -> bool;
    /// projection_version += 1（每次成功投影 +1，§5.3/§5.6）。
    pub fn bump_projection_version(txn: &mut TransactionCtx, root: &yrs::MapRef) -> u32;
}
```

> 【决策】`append_text_delta` 在 block 缺失时自动建块（消息 delta 首帧常见）；块文本 `Y.Text` 定位经块 Map 的 `text` 值位（yrs 支持 `MapValue::YText`）。

---

## 8. DocManager（唯一提交边界 + 单写者 + 微批次 + 广播）

### 8.1 结构

```rust
/// 唯一提交边界（§5.6）：所有 Y.Doc 写入（聚合投影、控制面状态迁移、权限 CAS、
/// 定时器、Registry 更新）都必须经 DocManager 的进程内单写通道；任何路径不得
/// 绕过 DocManager 直写 yrs（§6.5）。yrs `transact_mut()` 并发 panic 由单写者排除（§7.4）。
pub struct DocManager {
    sessions: RwLock<HashMap<String, SessionHandle>>, // 每 session 写者句柄
    registry: RegistryHandle,                          // 全局 Registry 写者
    cfg: BatchConfig,                                  // 16ms / 字节阈值 / 队列上限 64
    sink: Arc<dyn UpdateSink>,                         // 落盘（F6 实现）
    update_broadcast: mpsc::UnboundedSender<DocUpdate>,// 广播（§6.4）
}

pub struct BatchConfig {
    pub batch_window: Duration,   // 默认 16ms（§6.4 / §16）
    pub batch_bytes: usize,       // 【决策】默认 4KB（增量字节阈值；与 §14 开放问题 2 的 4KB 截断对齐）
    pub session_queue: usize,     // 【决策】默认 64（§8.6 每 session 命令队列上限 64）
}

/// 提交产物：update 的消费者（persist 实现落盘 + 归档；§8.4 提交点纪律）。
/// 【决策】trait 而非具体类型——F6 提供实现；DocManager 只要求「落盘完成后再应答」。
#[async_trait]
pub trait UpdateSink: Send + Sync {
    /// 落盘单个 Doc 的 update（per-commit fsync 默认由 F6 负责）；返回落盘结果。
    async fn persist_update(&self, doc: DocId, update: Vec<u8>) -> Result<(), PersistError>;
}

/// 广播载荷（§4.2 `ysync.update` 的素材；背压/合并/跳过属 broadcaster，F7）。
pub struct DocUpdate { pub doc: DocId, pub update: Vec<u8> }
```

### 8.2 每 session 单写者（§7.4）

- 每 session 一个 **writer task**（`tokio::spawn`），独占 `DocPair` + `Aggregator`；
- 入站通道 `mpsc::channel(session_queue)`（有界 64，满 → `RATE_LIMITED` 语义由调用方映射，§8.6）；
- 消息形态：`SessionMsg::Event(NormalizedEvent)` / `SessionMsg::Command(DocCommand)` / `SessionMsg::Req(oneshot)`；
- **需要应答的提交**（user entry 注册、权限 CAS 等控制类）挂 `oneshot`：writer 应用 + flush + 落盘确认后回填 `SubmitResult`（提交点纪律 §4.4「投影 user entry → committed Ack」）；
- 幂等/终态判定全部发生在 writer task 内（聚合器 `apply`），**命令入队检查（队列上限）与 in_flight 标记在同一临界区**（§7.4 规则 6——由 F7 的 command-coordinator 在提交侧保证，DocManager 提供同步 `try_reserve` 原语配合，见 §8.5）；
- 写者退出：session close/ended 时发送 `Shutdown`，writer 完成在途批次后退出（不丢已接收事件）。

### 8.3 微批次与先 flush（§6.4）

- 仅 **MessageDelta / ReasoningDelta** 进入 16ms 微批次缓冲（`tokio::select!`：`interval.tick()` vs `rx.recv_many`）；字节阈值 4KB 提前 flush；
- **控制类先 flush**：tool_call 状态、权限、Agent status、错误、turn 终态、断链、SessionInfo、session_list、全部 DocCommand——到达即**先 flush 已缓冲的 delta 批次，再立即写入**（同一 writer task 串行，天然顺序正确），保证用户看到的状态不倒退（§6.4）；
- 单个批次一次 yrs transaction 写入（`with_txn`，§6.4）；
- 跨 Chat/Session 双事务顺序**固定 chat → session**（两个独立事务，顺序固定；禁止跨 await 持有，§7.4）；
- flush 完成 → `encode_state_as_update` 的**增量 update**（yrs 事务后取 update）→ `sink.persist_update`（落盘）→ `update_broadcast.send`（广播；unbounded，背压在下游 broadcaster，§6.4）。

### 8.4 提交面

```rust
impl DocManager {
    pub fn new(cfg: BatchConfig, sink: Arc<dyn UpdateSink>) -> Self;

    /// 打开 session：Factory 创建双 Doc + ensure_schema（补结构）→ spawn writer task
    /// → RegistryState 写活跃摘要（§12.4）。重复打开按幂等处理（返回现有句柄）。
    pub async fn open_session(&self, session_id: &str, machine_id: &str, title: Option<&str>) -> Result<(), DocManagerError>;

    /// 聚合路径（F5 ACPChannel 产物 / 补推流）：经该 session 写者应用；
    /// 需要落盘应答的调用方 await（user entry 提交点纪律）。
    pub async fn submit_event(&self, ev: NormalizedEvent) -> SubmitResult;

    /// 控制路径（F7 command-coordinator / 定时器）：注册 user entry、权限 CAS、
    /// 标题更新、断链 interrupted、gap 同步、Registry 更新等（§8.5 DocCommand 表）。
    pub async fn submit_command(&self, cmd: DocCommand) -> SubmitResult;

    /// 关闭 session：写者 drain 后退出；Doc 保留（终态视图供历史查看，§8.2）。
    pub async fn close_session(&self, session_id: &str) -> Result<(), DocManagerError>;

    /// 广播订阅（unbounded）：broadcaster（F7）消费做背压与 fan-out。
    pub fn subscribe_updates(&self) -> mpsc::UnboundedReceiver<DocUpdate>;

    /// 同步入队检查（§7.4 同一临界区）：调用方在 outbox 去重索引更新前调用，
    /// 返回 false 表示队列满（RATE_LIMITED）或 session 不存在（SESSION_NOT_FOUND）。
    pub fn try_reserve(&self, session_id: &str) -> bool;
}

pub enum SubmitResult {
    /// 已应用（含 applied=false 的幂等/守卫拒绝——调用方按 reason 处理）。
    Applied(ApplyResult),
    /// 队列满 / session 已关闭等。
    Rejected(SubmitError),
    /// 落盘失败（F6 persist 错误；§17.2 degraded 输入）。
    PersistFailed,
}
```

### 8.5 DocCommand 表（控制路径命令，§6.5/§7.4/§12）

```rust
/// 控制路径写入命令（§5.6「控制面状态迁移如 cancelling/interrupted/decision/标题、
/// 定时器 CAS」全部经此）。
#[derive(Debug, Clone)]
pub enum DocCommand {
    /// 服务端单写用户消息注册（§6.5；幂等：同 turn_id 跳过）。
    RegisterUserEntry { turn_id: String, entry_id: String, text: String, author_user_id: Option<String>, created_at: String },
    /// 权限 CAS：resolve（pending → resolved 原子一次；§7.4 规则 4）。
    ResolvePermission { permission_id: String, decision: PermissionDecision },
    /// 权限 CAS：expire（pending → expired；定时器路径，§4.7）。
    ExpirePermission { permission_id: String },
    /// 断链 → 活动 turn 置 interrupted（§7.3 分区恢复；turn 级终态）。
    MarkTurnInterrupted { turn_id: String },
    /// 标题更新（§7.4 规则 5：可独立排队，仍经服务端命令写入）。
    UpdateTitle { title: String },
    /// 旧 turn 未完成时新 prompt 的裁决（§6.4：旧 assistant entry 置 cancelled，不发 ACP cancel）。
    CancelStaleAssistantEntry { turn_id: String, entry_id: String },
    /// session 级终态（ended/closed/crashed，§7.3）写视图。
    SetSessionTerminal { status: SessionStatus },
    /// Registry：活跃 session 摘要 upsert/移除/gap 同步（§12.4）。
    RegistryUpsertSession(proto::schema::SessionSummary),
    RegistryRemoveSession { session_id: String },
    /// Registry：machine 视图与全局状态（§12.4/§12.5）。
    RegistryUpsertMachine(proto::schema::MachineView),
    RegistrySetMachineStatus { machine_id: String, status: MachineStatus },
    RegistrySetGlobal { status: GlobalStatus },
}
```

> 【决策】Registry 系命令并入 `DocCommand`（提交面统一），但路由到全局 registry 写者而非 session 写者（Registry Doc 无 session 维度；低频、无微批次、写者即到即写）。

---

## 9. Aggregator（幂等聚合 + 终态守卫 + gap）

### 9.1 apply 纯函数

```rust
/// 幂等聚合器（§6.3）。**纯函数**：无 I/O、无日志副作用（脱敏日志由调用方在
/// 返回后统一打，§9.3/§12 测试前提）；可重入——同一事件流应用两次视图等价。
pub struct Aggregator { /* 无跨调用状态：全部状态在 DocPair 内（§4） */ }

impl Aggregator {
    pub fn apply(&mut self, pair: &mut DocPair, ev: &NormalizedEvent) -> ApplyResult;
}

pub struct ApplyResult { pub applied: bool, pub reason: Option<ApplyReason> }

/// 拒绝原因（§6.3「拒绝投影并记录脱敏诊断」）。
pub enum ApplyReason {
    /// 幂等键（turn_id/entry_id/tool_call_id/permission_id）已存在，重放跳过。
    DuplicateIdempotent,
    /// 终态守卫：turn 处于 cancelling/completed/failed/cancelled，晚到增量丢弃（§6.3）。
    TurnTerminalGuard,
    /// interrupted 状态下：非终态事件丢弃；或终态事件缺重放序依据（§6.3 例外）。
    InterruptedGuard,
    /// interrupted 校准恰一次：该 turn 已被校准（active_turn 已是实际终态）或 seq 非单调。
    CalibrationDone,
    /// 缺少必要关联信息（关联 turn/tool_call/permission 未知），§6.3。
    UnknownTurn, UnknownToolCall, UnknownPermission,
    AwaitingPermissionGuard,
    /// 防御性：epoch 与当前流不一致（§4.5.1 帧直接丢弃并计数）。
    EpochMismatch,
    /// 防御性：seq 回退（低于 last_seq；补推纪律下不应出现，§8.5）。
    SeqOutOfOrder,
    /// session 已终态（ended/closed/crashed），拒绝新事件（§8.2）。
    SessionClosed,
    /// 不可校准缺口存在时的补推事件（epoch 变化路径，§8.5）——拒绝除
    /// `session/load` 显式重建（F7 命令路径）外的一切投影。
    UncalibratableGap,
}
```

### 9.2 判定顺序（apply 内，逐事件）

1. **epoch 校验**：`ev.epoch != pair.stream.epoch` → `EpochMismatch`（防御；正常路径 hello 已对账，§4.5.1）；
2. **seq 水位**：`ev.seq <= last_seq` → `SeqOutOfOrder`；否则推进 `last_seq`、按跳变累计 `gap_count`（gap 计算见 §9.4）；
3. **session 终态**：session.status ∈ {Ended, Closed, Crashed} → `SessionClosed`（§8.2）；
4. **幂等键**：按事件体查 doc（entries/tool_calls/pending_permissions/active_turn）——已存在 → `DuplicateIdempotent`（重放安全；user_message 同 turn_id 跳过，§6.5）；
5. **终态守卫**（§6.3）：读 doc `active_turn`——
   - turn 处于 `cancelling` / `completed` / `failed` / `cancelled` → 除 `TurnTerminal` 外一律 `TurnTerminalGuard`（避免「已取消但还在输出」中间态）；`TurnTerminal`（同 turn）→ `CalibrationDone`（终态不可逆，§7.2）；
   - turn 处于 `interrupted` → 仅 `TurnTerminal`（同 turnId）且 **seq 单调**（`ev.seq > last_seq`，§9.3 恰一次）才应用；其余一律 `InterruptedGuard`；
6. **关联检查**：delta/tool_call/permission 事件引用的 turn/entry/tool_call/permission 未知 → `Unknown*`（缺必要关联信息拒绝投影，§6.3）；
7. **应用**：chat 写入 →（若涉 session 投影）session 写入，事务顺序 chat → session（§6.4/§7.4）；`projection_version` 在每次成功投影后 +1（Chat/Session 各自，§5.3/5.4）；
8. **gap 同步**：状态变化 → `stream.gap_dirty = true`，writer task 在 flush 时上报 RegistryState（§12.4）。

### 9.3 interrupted 校准（§6.3 例外，双条件）

校准允许 ⇔ `active_turn.status == Interrupted`（状态位）**且** `ev.seq > pair.stream.last_seq`（重放序单调）。恰一次由 doc 状态保证：校准后 `active_turn` 迁移为实际终态，后续任何终态事件（同序/低序/高序）都被步骤 5 的状态位守卫拒绝（`CalibrationDone`）——即使乱序补推也无法二次迁移。

> 与架构一致性：守卫实现从「状态位判断」改为「状态位 + 重放序判断」（§6.3 原文）；重放序依据 = envelope `(session_id, seq)`（本模块从 machine/event 帧透传，session 由路由绑定）。

### 9.4 gap 计算与写回（§8.5）

- 期望 seq = `last_seq + 1`（同 epoch）；`ev.seq > 期望` → `gap_count += ev.seq - 期望`；
- `ev.seq == 期望`（无缺口）且此前有 gap → **追平**：`gap_count = 0`，上报清除（写回 Registry `sessions[].gap = None`，§5.5）；
- `ev.epoch` 变化（经步骤 1 校验不通过且为合法新纪元）→ `uncalibratable = true`，上报 `gap = Some(count)` + 不可校准标记（仅 registry 状态源与日志可见；视图以 Registry gap 字段呈现，§12.4）；
- 不可校准只能经 `session/load` 显式重建消除（F7 命令路径 reset stream 状态，§8.5）。

> 分歧记录（不改架构文档）：架构 §5.5 `SessionSummary.gap: Option<u64>` 与 §8.5 结构化标记 `{ count, last_seq, uncalibratable? }` 形态不一致；proto（F1）已按 §5.5 定稿 `Option<u64>`。本设计：**视图落点 `gap: Option<u64>` 承载 count**；`last_seq`/`uncalibratable` 留在 `StreamState`（§4）与 registry 状态源内部，不新增线协议字段。建议后续架构文档 §5.5/§8.5 对齐（不阻塞 M1）。

### 9.5 工具结果截断

`ToolCallCompleted.result` 超过阈值（【决策】默认 4KB，对齐 §14 开放问题 2 方向）→ 不写 result，改记脱敏大小日志 + 视图以 Resource 引用语义省略（§5.3「超大结果仅保留受授权资源引用」）；敏感字段过滤在 ACPChannel（§9.3），本层只做大小预算。

---

## 10. Permission（CAS，§7.4 规则 4 / §5.4）

```rust
/// 权限请求 CAS 原语（pending → resolved 原子一次；§7.4 规则 4）。
///
/// 供两条路径共用（同一单写者通道内执行，无并发窗口）：
///  - 聚合器处理 `PermissionResolved`/`PermissionExpired` 事件（ACP 流）；
///  - 控制路径 `DocCommand::ResolvePermission`/`ExpirePermission`（客户端应答 / 定时器）。
pub fn resolve(pair: &mut DocPair, permission_id: &str, decision: PermissionDecision) -> CasOutcome;
pub fn expire(pair: &mut DocPair, permission_id: &str) -> CasOutcome;

pub enum CasOutcome {
    /// pending → resolved 原子迁移成功（唯一一次；调用方此刻才向 ACP 发 permission.resolve）。
    Migrated,
    /// 已 resolved/已 expired/未知：幂等返回（`duplicate` ack 语义，§4.4）。
    Duplicate,
    /// 已过期（expires_at < now 判定在定时器路径，§4.7；CAS 不重复判定时间）。
    Expired,
    /// 无此 permission_id。
    Unknown,
}
```

- `Migrated`：写入 `status = Resolved` + `decision`（CAS 迁移成功后写入，§5.4）；
- `expire`：仅 `pending → expired`，`decision` 保持 `null`（§5.4）；已 resolved → `Duplicate`（不覆盖已裁决）；
- **判定性时间戳由 server 权威时钟**（§4.7）：`expires_at` 生成与「是否过期」判定都在定时器/命令路径（F7），本原语只做状态迁移，不读时钟（保持纯函数可测）。

---

## 11. SessionList（10s 轮询全量同步投影，§6.3/§5.2）

```rust
/// `session_list` 响应全量同步投影（§6.3：幂等，10s 轮询；响应中不存在的旧条目
/// 删除——自愈）。纯函数：给定现有 Map 与响应计算 diff。
pub struct SessionListDiff {
    pub upsert: Vec<SessionSummaryProjection>,
    pub remove: Vec<String>,   // 现存 key 中不在响应内的（§6.3 旧条目删除）
}

/// 纯函数 diff（不触碰 doc；可单测）。
pub fn diff(current: &HashMap<String, SessionSummaryProjection>,
            incoming: &[SessionSummaryProjection]) -> SessionListDiff;

/// 应用 diff 到 Session Doc `sessions`（Y.Map 写；upsert 覆盖、remove 删键）。
/// 由聚合器在收到 `SessionListResponse` 时经 chat_writer 原语调用（session 写者内执行）。
pub fn apply_diff(txn: &mut TransactionCtx, root: &yrs::MapRef, diff: &SessionListDiff);
```

- 轮询调度（每 10s 发 ACP `session/list`）属 F7 command-coordinator；本模块只提供投影纯函数与写入原语；
- 与 Registry Doc `sessions` 语义不同、互不替代（§5.2 裁决）。

---

## 12. Registry（server 状态源单写，§5.2/§5.5/§17.2）

### 12.1 定位

Registry Doc（`hub:registry`）是 TUI 会话列表与机器列表的**唯一权威源**：活跃 session 摘要由 server 状态源单写（session 生命周期事件驱动：create/binding/终态/close 时更新），**不从 Session Doc 聚合**（§5.2 裁决）。聚合器不直写 Registry Doc（gap 经上报路径，§9.4）。

### 12.2 单写接口

```rust
/// Registry Doc 写入者（server 状态源单写接口，§5.2）。
///
/// 内部经 DocManager 全局 registry 写者执行（§8.5 命令路由）；调用方为
/// channel 层（machine 生命周期，F7/F8）与恢复流程（F6）。
pub struct RegistryState { /* 内部：DocManager 提交句柄 + 判定状态 */ }

impl RegistryState {
    /// machine 视图 upsert（hello 注册/心跳/offline；§7.1）。
    pub async fn upsert_machine(&self, m: MachineView) -> Result<(), RegistryError>;
    pub async fn set_machine_status(&self, machine_id: &str, status: MachineStatus) -> Result<(), RegistryError>;

    /// 活跃 session 摘要 upsert（create/binding 建立/标题/终态/gap 同步）。
    pub async fn upsert_session(&self, s: SessionSummary) -> Result<(), RegistryError>;
    /// 移除（session close 清理）。
    pub async fn remove_session(&self, session_id: &str) -> Result<(), RegistryError>;
    /// gap 写回（聚合器上报，§9.4）：`Some(count)` 置缺口、`None` 追平清除。
    pub async fn set_session_gap(&self, session_id: &str, gap: Option<u64>) -> Result<(), RegistryError>;
    /// session 状态迁移（accepting/active/ended/closed/crashed + pending_close，§7.3/§7.6）。
    pub async fn set_session_status(&self, session_id: &str, status: &str) -> Result<(), RegistryError>;

    /// 全局状态（§17.2）：条件上报/清除，判定集中于此。
    pub async fn report_condition(&self, cause: DegradeCause) -> Result<(), RegistryError>;
    pub async fn clear_condition(&self, cause: DegradeCause) -> Result<(), RegistryError>;
    /// 启动回放期置 Restarting；恢复不变量完成置 Healthy（§8.4.1）。
    pub async fn set_restarting(&self) -> Result<(), RegistryError>;
}

/// Degraded 判定输入（§17.2：任一触发 Degraded）。
pub enum DegradeCause {
    PersistFailure,      // 落盘失败（F6 上报）
    BufferDropped,       // 缓冲溢出丢弃（channel 层上报，§8.5）
    SessionGap,          // 任一存活 session 存在 gap（聚合器上报，§9.4）
    ProjectionError,     // 镜像失败（聚合器/writer task 异常，§17.2）
    RestoreInvariant,    // 启动恢复不变量失败（§8.4.1，F6/恢复流程上报）
}
```

### 12.3 global.status 判定（§17.2，集中实现）

- 任一 `DegradeCause` 活跃 → `Degraded`（可继续服务只读视图，拒绝新 committed 承诺，§8.4 同源语义由 F7 消费该状态）；
- 全部清除 → `Healthy`；
- `Restarting` 仅在 server 启动回放期间（恢复流程显式置入/置出，§8.4.1）；
- 状态变更即写 Registry Doc `global.status`（经 registry 写者）。

---

## 13. 测试清单（P0 契约测试）

形态遵循 §12 测试前提：聚合器契约为**纯函数测试**（内存 Y.Doc + `fn apply(&mut DocPair, &NormalizedEvent) -> ApplyResult`），无假连接；微批次/并发用 `tokio::time::pause`（test-util 已启用）与 `serial_test`。文件：`aggregator_test.rs`/`doc_manager_test.rs`/`permission_test.rs`/`session_list_test.rs`/`factory_test.rs`。

| # | 测试 | 断言 |
|---|------|------|
| 1 | **幂等重放**（§4.8 向量 3） | 同一 `machine/event` 流补推两次 → 无重复 entry/toolCall/permission；`apply` 第二次全部 `DuplicateIdempotent`；视图（entries/tool_calls/pending_permissions/entry_order）深度相等 |
| 2 | **user_message 幂等**（§6.5） | 同 turn_id 重放跳过；同 commandId 重发场景（F7 集成时对拍） |
| 3 | **终态守卫 cancelled 晚到丢弃**（§4.8 向量 4） | turn 终态 cancelled 后 message_delta/reasoning_delta/tool_call_updated → `TurnTerminalGuard`，doc 不变；cancelling 状态同断言 |
| 4 | **interrupted 校准恰一次**（§6.3 例外） | interrupted 后带 `seq` 单调的终态事件 → 恰一次迁移（doc 变为实际终态）；第二次同 turn 终态事件（同序/低序/高序）→ `CalibrationDone`；interrupted 状态下 delta → `InterruptedGuard` |
| 5 | **gap 计数**（§8.5） | seq 跳变 → gap_count 增量 + `gap_dirty`；seq 追平 → 清零 + 上报清除；epoch 变化 → `uncalibratable` + `EpochMismatch` 防御 |
| 6 | **单写者串行化**（§7.4） | tokio 并发提交 N 事件/命令 → 无 yrs panic（`transact_mut` 独占由通道保证）；结果与串行应用顺序等价 |
| 7 | **微批次与控制类先 flush**（§6.4） | `time::pause`：16ms 窗内 delta 合并为一次事务（update 数/事务数断言）；控制类事件到达 → 已缓冲 delta 先 flush 且控制类立即落 |
| 8 | **permission CAS**（§7.4 规则 4） | pending→resolved 恰一次（`Migrated` + decision 写入）；重复 resolve → `Duplicate`；expired 后 decision 保持 null；expire 已 resolved → `Duplicate` |
| 9 | **session_list 全量同步**（§6.3） | diff 纯函数：旧条目删除/新条目 upsert/无变化 no-op；apply_diff 后 Map 与响应一致 |
| 10 | **factory 补结构**（§5.6/§8.4.1） | 空 doc → ensure_schema 全结构；已有正确版本快照 → 幂等无重复；缺键旧快照 → 补缺不覆盖；未来版本 → `FutureSchema` |
| 11 | **双 Doc 事务顺序**（§7.4） | 同时写 Chat+Session 的批次：chat 事务先提交、session 后提交（顺序可观测断言：session update 内引用的投影版本 ≥ chat） |

---

## 14. 依赖记录与待确认项

**依赖（需主管处理）**：

1. `server/Cargo.toml` 当前**缺少 `acp-hub-proto` 引用**（经 Grep 确认仅 proto 自身声明）。state 层依赖 proto 的 `schema::{...}` 镜像类型、`DocId`、`version::{CHAT_DOC_SCHEMA_VERSION, SESSION_DOC_SCHEMA_VERSION, REGISTRY_DOC_SCHEMA_VERSION}`、`machine::MachineEvent`（F5 透传）——需补：
   ```toml
   acp-hub-proto = { path = "../proto" }
   ```
2. 无新增第三方依赖：`yrs 0.27`、`tokio`（full + test-util）、`tracing`、`serde_json`、`uuid`、`chrono`、`async-trait` 均已预填。

**待确认项（不阻塞实现，排期时定）**：

1. §5.5 与 §8.5 的 gap 形态对齐（§9.4 分歧记录）；
2. 工具结果截断阈值 4KB（§9.5）与 §14 开放问题 2 的裁决方向对齐；
3. `SessionInfo` 部分更新语义（§3.2 决策）在 F5 产出侧的字段枚举约定。

---

## 15. 关键决策摘要

1. **NormalizedEvent 由 state 层定义（envelope + 13 种 body）**：envelope 携带 `(session_id, epoch, seq)` 重放序依据（终态守卫与 gap 计数的输入），body 只含 §5.3/5.4 投影所需业务字段；F5 ACPChannel 只负责从 machine 帧提取并填充 envelope——协议边界单一、聚合器无私有帧知识。
2. **三层写入架构**：`ViewStore`（trait，聚合器唯一 yrs 接触面，§5.6 隔离承诺落点：聚合器不命名 `yrs::` 路径）+ `ChatWriter`（全部 yrs 写操作收敛为以 `TransactionCtx` 为参数的原语）+ `DocManager`（唯一提交边界：每 session writer task 单写者 + 有界队列 64 + 16ms 微批次 + 控制类先 flush + 跨双 Doc 事务固定 chat → session + 落盘确认后应答）。
3. **校准事实不进 Doc**：gap/uncalibratable/重放序水位全部在 `DocPair.stream`（内存，随提交同步 persist 水位），doc 内只存视图权威投影（`active_turn`/entry.status）；gap 的视图落点 = Registry Doc `sessions[].gap`，由 RegistryState（server 状态源）单写——聚合器只上报、不直写 Registry（§5.2 裁决）。
4. **interrupted 校准 = 状态位 + 重放序双条件**（§6.3）：恰一次由 doc 内 `active_turn` 终态迁移保证（校准后状态位天然拒绝一切后续终态事件），重放序 `seq > last_seq` 单调作为第二条件，乱序补推无法二次迁移。
5. **Registry 判定集中**：Degraded 五个输入（落盘失败/缓冲丢弃/session gap/镜像失败/恢复不变量）经 `DegradeCause` 上报集中于 RegistryState 判定（§17.2），全链路模块不各自发散判定。另：**server 需补 `acp-hub-proto` path 依赖**（当前缺失，已记录待主管处理）。
