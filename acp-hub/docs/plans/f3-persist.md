# F3 设计：server 持久化层（persist）

> 状态：设计稿（对应 Feature F3）
> 日期：2026-08-07
> 权威来源：`docs/architecture.md`（v2.4）§4.4/§4.5.1/§8.1/§8.3/§8.4/§8.4.1/§8.5/§12/§16/§17.2
> 约束：**忠于架构文档，不引入文档外的持久化契约**。文档未指明处的命名/参数/格式选择均标注「【决策】」并给出依据；文档明确的语义照抄。
> 协作边界：只改动 `server/src/persist/` 模块（单文件 `persist.rs` → 目录，`git rm` 原单文件后建 `persist/mod.rs`）；不碰 `lib.rs`/`Cargo.toml`/其他 feature 模块/`docs/architecture.md`。

---

## 1. 目标与范围

`acp-hub-server` 的持久化层，承载三种并列的持久化实体（§8.4）：

1. **update 日志**：Y.Doc 投影 update 的追加日志（blob+CRC32+compact），启动回放恢复 server 自身视图；
2. **command outbox**：commandId 去重账本（§4.4），跨 server 重启成立 P8；
3. **(epoch, last_seq) 水位**（§4.5.1/§8.5）：补推起点，与 update 日志同目录独立小文件。

三者之间**不提供跨文件原子性**（§8.4.1）：持久化单元是单文件内的单条记录，跨文件一致性靠恢复不变量顺序（§7）达成。

**边界声明**（不在 persist 实现）：

- 提交点纪律的**编排**（outbox 落盘 → 下发 ACP → L1+L2 → 投影落盘 → committed Ack）属 `channel/command-coordinator`（§4.4）；persist 只提供两个独立的 fsync 点 API。
- Y.Doc 结构补齐（§8.4.1 不变量 3）属 `state/doc-manager`；persist 输出回放记录，doc-manager 应用。
- machine 对账后开门（不变量 4）属 `channel` + machine 注册表；persist 只提供 `recover()` 完成信号与 outbox 索引查询。
- `degraded` 的对外呈现（Registry Doc `global.status`，§17.2）属 `state`；persist 提供 `Store::status()` 数据源。
- 归档策略正式方案为开放问题 3（§14），M1 只提供接口与条件检查（§9.3）。

## 2. 模块划分与目录布局

```
server/src/persist/
├── mod.rs            # crate 文档、StoreError、RecoveryResult、Store 公共 re-export、目录布局说明
├── store.rs          # Store：目录初始化（0600）、session 分片管理、恢复编排（recover）、
│                     #   磁盘预算记账、degraded 汇聚、归档接口
├── update_log.rs     # blob 线格式原语（pub(crate)，outbox/watermark 复用）+ UpdateLog
│                     #   （append/回放尾部截断/compact/快照）
├── outbox.rs         # OutboxStore：OutboxRecord/OutboxStatus/RetryableClass、
│                     #   状态机迁移 API、重放重建索引、清理策略
├── watermark.rs      # Watermark/WatermarkStore：加载/写入/对齐规则
└── *_test.rs         # 各模块单元测试（仓库规范：*_test.rs 同目录）+ tests/ 集成
```

数据目录布局（`~/.local/share/acp-hub/`，0600，§16）：

```
~/.local/share/acp-hub/                 # 目录权限 0700→文件 0600【决策：目录 0700、文件 0600】
├── sessions/
│   └── <session_id>/                   # session_id = server 生成 uuid（§4.4）
│       ├── updates.log                 # 投影 update 追加日志（blob 记录）
│       ├── updates.snapshot            # compact 全量快照（单条 blob 记录，含 last_applied_seq 边界）
│       ├── outbox.log                  # command outbox 追加日志（blob+JSON 记录）
│       ├── watermark.json              # (epoch, last_seq) 单条 blob 记录
│       └── corrupt/                    # 损坏段保留（诊断，§8.4）
└── archive/<session_id>/               # 归档（§9.3，M1 简化）
```

命名「.json」仅表意（JSON 体），实际均为 blob 外壳包裹（§4.1）【决策：统一 blob 外壳，后缀仅诊断可读性】。

**实现步骤**：`git rm server/src/persist.rs` → 建目录与上述文件（纪律 2，只限本模块）。

**依赖**：无需新增——`crc32fast`、`uuid`、`chrono`、`serde_json`、`tracing`、`tempfile`（dev）均已预填于 `server/Cargo.toml`。

## 3. 公共类型总览

### 3.1 StoreError

```rust
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io error on {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("corrupt record in {path}: {detail}")]
    Corrupt { path: PathBuf, detail: String },      // CRC 失败 / 结构非法 / len 越界
    #[error("invalid outbox transition {from} -> {to} for command {command_id}")]
    InvalidTransition { command_id: Uuid, from: OutboxStatus, to: OutboxStatus },
    #[error("duplicate command {command_id} already in state {state}")]
    DuplicateCommand { command_id: Uuid, state: OutboxStatus },  // 重发穿透防护
    #[error("session {session_id} not found")]
    SessionNotFound { session_id: Uuid },
    #[error("persist store is degraded: {reason}")]
    Degraded { reason: String },                    // 已 degraded，拒绝新 committed 承诺（§8.4）
    #[error("disk budget exceeded: used {used}B > limit {limit}B")]
    BudgetExceeded { used: u64, limit: u64 },
}
```

### 3.2 RecoveryResult（§8.4.1 不变量 1-2 的聚合产物）

```rust
pub struct RecoveryResult {
    pub degraded: bool,                  // 任一不变量失败 / 任一文件损坏（§17.2）
    pub warnings: Vec<RecoveryWarning>,  // 截断 / 对齐 / epoch 告警（不阻塞，仅告警）
    pub truncated_total_bytes: u64,      // 尾部截断总字节数（§17.1 指标）
    pub corrupt_artifacts: Vec<PathBuf>, // 保留的损坏段与失效快照（corrupt/ 下）
}
pub struct RecoveryWarning {
    pub code: WarningCode,               // TailTruncated / WatermarkCorrupt / SeqMismatch /
                                         //   EpochMismatch / SeqNonMonotonic / SnapshotInvalid
    pub path: PathBuf,
    pub message: String,                 // 脱敏（无正文/内容）
}
```

### 3.3 PersistConfig 与 FsyncMode（§16 默认值）

```rust
pub struct PersistConfig {
    pub data_dir: PathBuf,                     // 默认 ~/.local/share/acp-hub/
    pub fsync_mode: FsyncMode,                 // 默认 PerCommit（§16）
    pub compact_threshold_bytes: u64,          // 默认 64MB
    pub compact_interval: Duration,            // 默认 24h
    pub disk_budget: u64,                      // 默认 2GB
    pub outbox_retention: Duration,            // 默认 7 天（§4.4「session 关闭后保留 7 天」）
    pub archive_retention: Duration,           // 默认 90 天（§16，开放问题 3）
}
pub enum FsyncMode {
    PerCommit,                                 // committed Ack 在落盘后返回（§8.4）
    Batch(Duration),                           // 如 1s 批量 fsync；Ack 语义降级为「已入持久层队列」，
}                                              //   须配置显式声明（§8.4/§16）
```

## 4. update 日志（UpdateLog）

### 4.1 blob 线格式（三种文件共用外壳）【决策】

```
┌──────────────────────────────────────────────────────┐
│ len: u32 LE       —— 记录体字节数（不含本字段与 crc32）│
│ crc32: u32 LE     —— CRC32(记录体)                    │
│ body              —— 记录体（len 字节）               │
└──────────────────────────────────────────────────────┘
```

- CRC 覆盖**整个记录体**（含 version/kind 头）：len 字段本身被间接保护——len 被改小 → 读到错位体 → CRC 失败；len 被改大 → EOF/越界 → 判损坏。任一失败即按损坏处理（§4.4 尾部截断）。
- 自描述：body 首字节为 `version: u8`（当前 0x01），版本不符 = 损坏。
- 防御上限：`MAX_RECORD_BYTES` = 64MB【决策：单帧上限 1MB（§16），一个微批次（§6.4）+ 双 Doc 段的逻辑提交远小于此；越界视为损坏】。
- 读流程：`read_exact(8)` → 校验 len ≤ MAX → `read_exact(len)` → CRC 校验 → 解析 body。

### 4.2 逻辑提交记录模型

**一条记录 = 一个逻辑提交**（聚合器一次 flush / 微批次事务边界，§6.4）【决策，见 §10 决策摘要 3】：

```
body = version:u8 | kind:u8 | epoch:u32 LE | seq:u64 LE | payload
kind = 0x01 doc_commit（M1 唯一），预留扩展
doc_commit payload = 重复段：doc_id:u8（0=chat,1=session）| len:u32 LE | yjs update 字节
```

- `seq`：本提交**覆盖的最大帧 seq**（machine 侧分配，§8.5）。一个微批次合并多条帧时，中间帧的 seq 不单独成记录——补推 `from_seq = last_seq + 1` 会重推中间帧，由聚合器幂等（turnId/entryId/toolCallId，§6.3）兜底，不产生重复副作用。
- `epoch`：流纪元（§4.5.1），写入记录体用于回放校验与水位对齐。
- 记录内 `(epoch, seq)` 随 batch 的多个 doc 段作为一个原子单元落盘。

### 4.3 append 与 fsync

```rust
impl UpdateLog {
    /// 追加一个逻辑提交并落盘。返回 = 本提交已 durable（PerCommit 模式）。
    /// 顺序：append blob → file.sync_data() → watermark 更新（§6）→ 返回。
    pub async fn append(&mut self, epoch: u32, seq: u64, docs: &[(DocId, &[u8])])
        -> Result<(), StoreError>;
    pub fn stats(&self) -> UpdateLogStats;      // { bytes, records, last_seq }（§17.1 指标）
    pub fn degraded(&self) -> bool;
}
```

- 写锁（`tokio::sync::Mutex`）串行化 append 与 compact（§8）。
- **fsync 纪律**（§8.4）：PerCommit 模式 `sync_data()` per append；创建/rename 文件后对**目录**做 fsync；Batch 模式延迟到定时 flush（Ack 语义降级由 channel 层声明）。fsync 失败（磁盘满等）→ 置 degraded + `tracing::warn!(path, error, session_id)`，**绝不静默**；上层经 `Store::status()` 感知后拒绝新 committed 承诺（新 Action 返回可重试错误）。
- 日志字段脱敏（§9.3/协作纪律）：只记 `session_id/epoch/seq/bytes/elapsed_ms/error`，不记 yjs 字节与消息正文。

### 4.4 启动回放（尾部截断恢复）

```rust
pub struct LogRecord { pub epoch: u32, pub seq: u64, pub docs: Vec<(DocId, Vec<u8>)> }
pub struct ReplayOutcome {
    pub records: Vec<LogRecord>,            // 按追加序
    pub truncated: Option<CorruptionInfo>,  // { offset, bytes_kept, reason }
    pub degraded: bool,
}
impl UpdateLog {
    /// 顺序读取全部记录；遇损坏（CRC 失败/越界/结构非法/version 不符）：
    /// 截断于损坏点，损坏点至 EOF 字节写入 corrupt/<file>.<offset>.bin，告警 + degraded。
    pub fn replay(&mut self) -> Result<ReplayOutcome, StoreError>;
    pub fn load_snapshot(&mut self) -> Result<Option<Snapshot>, StoreError>;
}
pub struct Snapshot { pub last_epoch: u32, pub last_applied_seq: u64,
                      pub docs: HashMap<DocId, Vec<u8>> }   // 双 Doc 全量 state update
```

- 尾部截断语义（§8.4）：损坏点之后**全部放弃**（不尝试定位中间完好记录）；损坏段保留供诊断。
- 回放防御性校验：同 epoch 内 seq 非递减，违反 → `SeqNonMonotonic` 告警（不阻断）【决策：防御性，正常路径下聚合器串行消费者保证】。
- 快照存在时（compact 产物，§8）：基线 = 快照 + 日志中 `seq > snapshot.last_applied_seq` 的记录；日志中 `seq ≤ 快照点` 的重复记录由聚合器幂等兜底（§8 崩溃时序 C）。快照 CRC 失败 → 移入 corrupt/ → 纯日志回放 + degraded。
- 回放返回的 `records` 交由 `state/doc-manager` 应用（不变量 3，§7）。

## 5. command outbox（OutboxStore）

### 5.1 记录结构与持久化形态

```rust
pub struct OutboxRecord {
    pub command_id: Uuid,
    pub session_id: Uuid,
    pub command_type: CommandType,          // create/prompt/cancel/close/resolve（§4.8 M1 五种）
    pub turn_id: Option<Uuid>,              // 仅 prompt；同 commandId 重试复用（§4.4）
    pub status: OutboxStatus,
    pub retryable_class: RetryableClass,    // 命令固有幂等性分类（§4.4 顾问3）
    pub dispatched_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,          // 判定性时间戳由 server 时钟（§4.7）
    pub updated_at: DateTime<Utc>,
    pub last_error: Option<LastError>,      // { code, retryable, at }（delivery_unknown 对账展示）
    pub attempt_count: u32,                 // 可观测性（§17.1 指标）
}
pub enum OutboxStatus { Received, Accepted, IntentDurable, Dispatched,
                        DeliveryConfirmed, ProjectionCommitted, Completed, Failed, DeliveryUnknown }
pub enum RetryableClass { SafeToRedeliver, NoAutoRedeliver }   // 见 §5.2 分类表
```

- **持久化形态【决策，见摘要 2】**：`outbox.log` 为**追加式状态快照日志**——每次状态迁移追加一条**完整记录**（JSON body，后者覆盖前者）；删除 = 追加 tombstone 记录（`{v, commandId, status: "removed"}`）。启动重放顺序应用（insert/update/remove）重建 `Map<command_id, OutboxRecord>`。物理压缩（重写文件）只在清理时发生（§5.5）。理由：与 update 日志同构（追加 + per-commit fsync，§8.4 同一纪律）；重放代码单遍线性；单条记录内部原子（blob 外壳）。
- 记录 JSON 字段与 §4.4 的 `commandId → {type, turnId, status, dispatched_at}` 一一对应（+ 补充字段）。

### 5.2 状态机与迁移 API（§4.4 顾问2 图 + 顾问3 分类）

**状态机迁移表**（非法迁移一律 `InvalidTransition` 拒绝并 `tracing::warn`，不静默）：

| from | to | 触发方 | 说明 |
|------|-----|--------|------|
| received | accepted | coordinator 入队 | 两阶段 Ack 之 accepted |
| accepted | intent_durable | 意图落盘 | 提交点纪律第一步「outbox 记录先行落盘」（§4.4） |
| intent_durable | dispatched | 下发 machine | 置 `dispatched_at`；此后崩溃 → 重发由 outbox 兜底返回 `duplicate` |
| intent_durable | （删除记录）| retryable 失败 | `AGENT_UNAVAILABLE`/`MACHINE_OFFLINE` 清除记录，允许重发重新执行（§4.4） |
| received / accepted / intent_durable | failed | 非 retryable 失败 | `INVALID_STATE`/`FORBIDDEN`/`SESSION_NOT_FOUND`（§4.4 重试分类） |
| dispatched | delivery_confirmed | L1+L2 达成 | M1 合并 L1+L2（machine 转发确认隐含写成功，§4.4） |
| dispatched | delivery_unknown | L2 后 L3 不可得 | M1 路径 B（§5.3） |
| delivery_confirmed | projection_committed | 投影 update 落盘后 | user entry 投影写入 update 日志（§4.4 提交点纪律） |
| delivery_confirmed | failed | 业务失败 | 客户端收 `action_error`（§4.4） |
| projection_committed | completed | committed Ack 返回 | 终态 |
| delivery_unknown | completed | 人工裁决「确认已送达」 | §5.3 runbook |
| delivery_unknown | （删除记录）| 人工裁决「确认未送达」 | 清除记录允许重发（§5.3） |
| delivery_unknown | delivery_unknown | 裁决「仍未知」 | 保持（幂等，重载不推进） |

非法迁移示例（拒绝）：`completed/failed` → 任何状态；`dispatched → projection_committed`（跳过确认）；`delivery_unknown → dispatched`（非幂等禁止自动重发）；`received → dispatched`（跳过 accepted/intent_durable）。

**retryable 分类**（§4.4 顾问3，命令进入 outbox 前必须显式分类，未分类默认 `NoAutoRedeliver`）：

| 命令 | 分类 | 依据 |
|------|------|------|
| session/create、session/close | SafeToRedeliver | 以 session_id 为天然幂等键（§4.5） |
| session/prompt、session/cancel、permission/resolve | NoAutoRedeliver | 非幂等，禁止盲重试（路径 B，§4.4） |

```rust
impl OutboxStore {
    // —— 状态机迁移 API（每个迁移 = 追加一条记录 + fsync；PerCommit 模式）
    pub fn insert(&mut self, rec: NewOutboxRecord) -> Result<(), StoreError>;          // → Received
    pub fn mark_accepted(&mut self, id: Uuid) -> Result<(), StoreError>;
    pub fn mark_intent_durable(&mut self, id: Uuid) -> Result<(), StoreError>;
    pub fn mark_dispatched(&mut self, id: Uuid, at: DateTime<Utc>) -> Result<(), StoreError>;
    pub fn mark_delivery_confirmed(&mut self, id: Uuid) -> Result<(), StoreError>;
    pub fn mark_projection_committed(&mut self, id: Uuid) -> Result<(), StoreError>;
    pub fn mark_completed(&mut self, id: Uuid) -> Result<(), StoreError>;
    pub fn mark_failed(&mut self, id: Uuid, err: LastError) -> Result<(), StoreError>; // → Failed（终态）
    pub fn clear_for_retry(&mut self, id: Uuid) -> Result<(), StoreError>;            // retryable 失败清除 / 裁决未送达：tombstone
    pub fn mark_delivery_unknown(&mut self, id: Uuid) -> Result<(), StoreError>;
    pub fn resolve_delivery_unknown(&mut self, id: Uuid, verdict: DeliveryVerdict) -> Result<(), StoreError>;
    // —— 查询 / 重放 / 清理
    pub fn get(&self, id: Uuid) -> Option<&OutboxRecord>;          // 去重索引查询（重发判定）
    pub fn replay(&mut self, records: impl IntoIterator<Item=OutboxLogEntry>) -> ReplayStats; // 启动重建
    pub fn cleanup(&mut self, now: DateTime<Utc>, session_closed: bool) -> CleanupStats;      // §5.5
}
pub enum DeliveryVerdict { ConfirmedDelivered, ConfirmedNotDelivered, StillUnknown }
```

- `insert` 遇已存在 commandId：若状态为可重发前态（received/accepted/intent_durable）→ 返回 `DuplicateCommand` 由上层按「重发」处理；终态（completed）→ 直接返回原 Ack（`duplicate`）+ turnId，**不重复调用 Agent**（§4.4）。判定逻辑在 coordinator，persist 提供 `get`。
- 每次裁决/清除写审计日志（§9.4 结构化日志：`tracing::info!(event="outbox.resolve", command_id, verdict, session_id)`，不记载荷）。

### 5.3 delivery_unknown（M1 路径 B，§4.4 顾问2/3）

- M1 默认路径 B：L2 后未取得 L3 的记录置 `delivery_unknown` 持久化；`NoAutoRedeliver` 命令**禁止自动重试**。
- 记录必须可查询、可持久化、可展示，重启/归档不得静默丢弃：persist 保证（a）`get()` 可查、（b）状态在日志中 durable、（c）session 目录归档前置条件含 outbox 全终态（§9.3）。
- 人工裁决入口 `resolve_delivery_unknown`：`ConfirmedDelivered` → completed；`ConfirmedNotDelivered` → tombstone 清除（允许重发）；`StillUnknown` → 保持。权限（server 操作员）与依据（agent 状态查询/进程存活/用户确认）属 control 层，persist 只提供迁移 API 与审计日志。

### 5.4 启动重放重建去重索引

- `Store::recover()` 内逐 session 顺序读 `outbox.log`：完整记录 → `index.insert`；tombstone → `index.remove`；损坏 → 尾部截断 + 告警 + degraded（与 update 日志同纪律）。
- 恢复不变量 1（§8.4.1）**对外语义**：`recover()` 返回（索引可用）前，channel 层不得接受任何 Action（§7 门禁）。
- 重放结果：`dispatched`/`delivery_unknown` 等未完成记录**保留**（供恢复对账与裁决）；`completed`/`failed` 记录保留至清理策略执行。

### 5.5 清理策略（§4.4「显式清理策略」）

**删除前置条件**（§8.4 顾问2，缺一不可）：session 已关闭 + machine 注销/不再重连（调用方传入/确认）+ outbox 记录全终态 + 保留期届满。persist 负责检查「终态 + 保留期」，`session_closed` 由 control 层在 close 完成时调 `SessionStore::mark_closed()` 记录；machine 注销判断由调用方在触发清理时确认（persist 不依赖 machine 注册表）【决策：解耦，条件组合由调用方保证，persist 校验自己可校验的部分】。

- **7 天保留**：终态（completed/failed）记录自 `updated_at` 起保留 `outbox_retention`（默认 7 天），期满且 session 已关闭 → tombstone + 物理压缩。
- **磁盘预算淘汰**：预算超限（§9.2）时优先淘汰最旧终态记录（不受 7 天约束，仍受前置条件约束）。
- **M1 简化版（接口完整）**：`cleanup()` 实现 7 天保留与压缩；预算淘汰触发入口保留（`Store::enforce_budget()`），M1 自动触发只做「告警 + 归档候选提示」，不自动删除未满保留期的记录【任务允许简化，接口完整】。
- 物理压缩：重写 `outbox.log`（临时文件 → fsync → rename → 目录 fsync），与 compact 同纪律（§8）；压缩在 Mutex 内进行。
- 清理时机：启动 `recover()` 后 + session 关闭时 + 预算巡检（§9.2）。

## 6. (epoch, last_seq) 水位（Watermark）

```rust
pub struct Watermark { pub epoch: u32, pub last_seq: u64 }
impl WatermarkStore {
    /// 加载水位文件（单条 blob + JSON）。损坏 → 告警 + degraded + 视为 None（无水位）。
    pub fn load(&self) -> Result<Option<Watermark>, StoreError>;
    /// 写水位（覆盖写 + fsync）。
    pub fn write(&self, wm: &Watermark) -> Result<(), StoreError>;
    /// 对齐：与日志最后一条 (epoch, seq) 核对（§8.4.1 不变量 2）。
    pub fn align(&self, wm: Option<Watermark>, log_tail: Option<(u32, u64)>)
        -> (Watermark, Option<AlignmentWarning>);
}
```

- 每 session 独立小文件 `watermark.json`（§4.5.1「随 outbox/update 日志同目录独立文件」）。
- **更新时机**：每次 `UpdateLog::append` 成功后更新（epoch 相同只推进 seq；epoch 变化则替换）——append 顺序 = 写日志记录 → fsync → 写水位 → fsync → 返回（§4.3）。崩溃于两者之间 → 水位落后 → 对齐规则吸收。
- **加载与对齐规则**（§8.4.1 不变量 2，`align`）：
  1. 水位缺失（新 session/文件损坏）→ 以日志尾部为准；无日志 → `(epoch=0, last_seq=0)`（从 1 开始补推，machine 环形滑窗 500 条兜底，§8.5）；
  2. 水位与日志尾部 **epoch 相同**：`last_seq = min(水位, 日志)`；两者不等 → `SeqMismatch` 告警（日志尾部截断 seq 倒退场景，以较小者为准——补推重复段由聚合器幂等兜底）；
  3. **epoch 不同**：旧流 seq 空间作废（§4.5.1），以水位为准（水位的 epoch/last_seq 为权威代际），`EpochMismatch` 告警；是否判不可校准 gap 属上层（session 状态机）裁决，persist 只报告。
- 对齐结果写入内存（`SessionStore.watermark`），供补推起点查询：`WatermarkStore::current() -> Watermark`。

## 7. 恢复编排与不变量（§8.4.1）

```rust
impl Store {
    /// 恢复编排。完成 = 不变量 1-2 就绪（outbox 索引可用 + 水位已对齐）。
    /// 返回 RecoveryResult（§3.2），degraded 汇总自全部子项。
    pub async fn recover(&self) -> RecoveryResult;
}
```

**persist 内实现（不变量 1-2）**，逐 session（目录名排序保证日志确定性）：

1. 清理残留：`updates.snapshot.tmp` 存在 → 删除（rename 未发生，旧日志完整，§8 崩溃时序 A）；
2. **水位先行加载**（不变量 2 前提）：`WatermarkStore::load()`；
3. **outbox 重放**（不变量 1）：重建去重索引（§5.4）；
4. **update 日志回放**（§4.4）：尾部截断 + 快照基线选择；
5. **水位对齐**（§6）：与日志尾部核对；
6. 汇总 warnings/degraded/统计 → `RecoveryResult`。

**与其他层协作（不变量 3-5）**，接口标注：

| 不变量 | 实现层 | persist 提供的接口 |
|--------|--------|-------------------|
| 3 Doc 补齐（schema_version 判空幂等补结构，§5.6） | `state/doc-manager` | `ReplayOutcome.records`（按序应用）；`Snapshot.docs`（compact 基线） |
| 4 machine 对账后开门（Registry unknown → online/offline；对账完成前禁止新 prompt 之外控制操作） | `channel` + machine 注册表 | `recover()` 完成信号（开门门禁的**前置**条件之一）；`outbox.get()` 供重发判定 |
| 5 任一失败 → degraded（§17.2） | `state`（Registry `global.status`） | `RecoveryResult.degraded` + `Store::status()`（运行期落盘失败同源，§4.3） |

**degraded 运行期语义**（§8.4）：落盘失败 / 回放损坏 / 快照失效 → degraded；可继续服务只读视图，拒绝新 committed 承诺（新 Action 返回可重试错误）。degraded 为 Store 级汇聚（`AtomicBool` + 原因），`Store::status() -> PersistStatus { degraded, reason, disk_used, disk_limit }`。

## 8. compact 流程（§8.4 契约）

**触发条件**（`UpdateLog::maybe_compact`，append 后与定时器检查，§16 默认值）：

- `updates.log` 大小 > `compact_threshold_bytes`（64MB），**或**
- 距上次 compact > `compact_interval`（24h）。

**原子流程**（持写锁；快照内容由调用方 doc-manager 提供双 Doc 全量 state update）：

```
1. 记快照点 s = 当前 last_seq（锁内无并发 append）
2. 写 updates.snapshot.tmp（单条 blob：{v, lastEpoch, lastAppliedSeq=s, createdAt, docs{chat,session}}）
3. fsync(tmp) → fsync(目录)
4. rename(tmp → updates.snapshot) → fsync(目录)     ← 原子点：此后旧日志可弃
5. truncate(updates.log, 0) → fsync
```

**崩溃时序**（§8.4「中途崩溃可回退到旧日志重放」）：

| 崩溃点 | 磁盘状态 | 启动恢复 |
|--------|---------|---------|
| A：rename 前 | tmp 残留 + 旧日志完整 | 删 tmp，纯旧日志回放 |
| B：rename 后、truncate 前 | 快照 + 旧日志完整（锁内无新 append，日志尾部 seq = s） | 快照基线 + 日志重复段幂等跳过（§4.4），截断日志 |
| C：truncate 后 | 快照 + 空日志 | 快照基线 |

快照无效（CRC/解析失败）→ 移 `corrupt/` + degraded → 纯日志回放。

## 9. 磁盘预算与归档

### 9.1 权限与目录

- 根目录 `0700`、文件 `0600`（§8.4「0600 权限」）【决策：目录 0700 保证文件可达性一致】；启动 `Store::open` 校验并修复（`mkdir` + `set_permissions`），失败 → `StoreError::Io`。
- 持久化路径默认 `~/.local/share/acp-hub/`（`dirs-next` 解析，§16 可配置）。

### 9.2 磁盘预算（默认 2GB，§16）

- 记账范围：`sessions/` 与 `archive/` 全部文件（日志 + 快照 + outbox + 水位 + corrupt 段）【决策：corrupt 计入以约束诊断膨胀】。
- 检查点：append / compact / cleanup 之后（`Store::check_budget()`）。
- 超限行为（§8.4）：`tracing::warn!(event="disk_budget.exceeded", used, limit)` + 触发淘汰流程：最旧已关闭 session 归档候选（§9.3）+ outbox 最旧终态记录淘汰（§5.5，M1 简化只告警+候选）；**绝不静默**，持续超限且无可淘汰 → degraded（落盘失败语义同源）。

### 9.3 归档（临时默认 90 天，开放问题 3）

- `Store::archive_session(id)`：条件检查——session 关闭 + outbox 全终态 + `closed_at + archive_retention` 届满；满足则移动目录至 `archive/` 并记录清单。
- **归档与 outbox 保留解耦**（§8.4 顾问2）：90 天归档不得独立触发 outbox 清理；outbox 删除前置条件见 §5.5。
- M1 简化：提供接口与条件检查；自动触发由启动巡检 + 预算巡检调用；归档内容（压缩/导出）后置开放问题 3。

## 10. 与其他模块的接口（调用点）

| 调用方 | 接口 | 对应章节 |
|--------|------|---------|
| `channel/command-coordinator` | `OutboxStore` 全 API + 提交点纪律编排（outbox fsync → 下发 → L1+L2 → update 日志 append → committed Ack） | §5、§4.3 |
| `channel/relay-event-handler` | `UpdateLog::append(epoch, seq, docs)`（machine/event → 规范化 → 投影落盘）；`watermark.current()` 供 `buffer_sync` epoch 校验（不一致拒绝该批，§4.5.1） | §4.3、§6 |
| `channel`（gateway/开门） | `recover()` 完成信号（不变量 4 门禁前置） | §7 |
| `state/doc-manager` | 回放 `records` 应用（不变量 3）；compact 时提供全量快照字节 | §7、§8 |
| `state`（Registry） | `Store::status()` → `global.status`（degraded） | §7 |
| `config` | `PersistConfig` 默认值（§3.3） | — |

**并发模型**：`Store` 内 `HashMap<SessionId, SessionStore>`；`SessionStore` 持 `Mutex<UpdateLog>` / `Mutex<OutboxStore>` / `Mutex<WatermarkStore>`（tokio 异步锁，文件 I/O 为同步小操作，M1 本机可接受【决策】）。

## 11. 测试清单

单元测试（模块内 `*_test.rs`，tempfile 隔离；时间注入 = 参数化 `now`/保留期）与集成测试（`tests/`，构造磁盘状态 → `recover()` 断言）：

| # | 场景 | 断言 |
|---|------|------|
| T1 | blob roundtrip（update/outbox/watermark 三类） | 读写一致；len=0、len 越界（> MAX_RECORD）→ Corrupt |
| T2 | CRC 损坏截断 | 3 条记录破坏第 2 条 payload → 回放返回 1 条 + `TailTruncated` 告警 + corrupt 段保留 + degraded |
| T3 | fsync 语义（per-commit） | append 返回后立即 reopen 可见全部记录（模拟崩溃 = drop 后重开）；Batch 模式未 flush 数据重开缺失（Ack 降级语义由上层测试） |
| T4 | 状态机非法迁移拒绝 | 迁移表全部非法对 → `InvalidTransition`，文件无新增记录；合法路径全绿 |
| T5 | 重启重放重建索引 | 多状态迁移 + tombstone → 重开 recover → index 与内存态一致；dispatched/delivery_unknown 记录保留 |
| T6 | 水位对齐 | 水位 100 vs 日志 90 → last_seq=90 + `SeqMismatch` 告警（不 degraded）；水位 CRC 损坏 → degraded + 按无水位处理；epoch 不同 → 以水位为准 + `EpochMismatch` |
| T7 | compact 原子性（崩溃时序 A/B/C） | A：tmp 残留 → 重开纯日志回放；B：快照+重复日志 → 快照基线；C：快照+空日志 → 快照基线；触发条件（大小阈值注入小值） |
| T8 | outbox 清理策略 | 7 天保留期（注入）届满 + session_closed → 终态记录删除 + 文件压缩；未过期/非终态保留；预算淘汰候选接口 |
| T9 | 磁盘预算 | 注入小预算 → 超限告警 + 淘汰候选（最旧归档 + 最旧终态）；无候选时 degraded |
| T10 | 恢复编排集成 | 构造多 session 混合状态（截断日志 + delivery_unknown 记录 + 水位落后）→ recover → RecoveryResult 汇总正确（degraded/warnings/字节统计） |
| T11 | 目录权限 | 根目录 0700 / 文件 0600（unix；tempfile 平台限制标注） |

## 12. 实现顺序建议

1. `store.rs`（StoreError/目录/权限）+ blob 原语（update_log.rs）→ T1/T11；
2. `update_log.rs`（append/回放）→ T2/T3；
3. `watermark.rs`（load/align）→ T6；
4. `outbox.rs`（状态机/重放/清理）→ T4/T5/T8；
5. `store.rs` recover 编排 + compact + 预算 → T7/T9/T10；
6. `tests/` 集成 + 与 channel/doc-manager 的接口联调（§10 调用点）。

**M1 简化点汇总**：outbox 预算淘汰自动触发（接口完整，M1 告警+候选）、归档自动执行（接口+条件检查）、L3 路径 A（关联 ID 查询）——均不影响本层接口形态。
