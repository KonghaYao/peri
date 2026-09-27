# 会话资源拆分 — 子计划 A：行为契约与纯逻辑

> 状态：**本计划的行为契约与纯逻辑已落代码**（2026-09-26；落盘清单与验证证据见[母需求](2026-09-26-session-store-remote-backend.md)「A 阶段实施进度」段）；消费侧字段迁移（E）、`SessionResourcesImpl`（B）与 adapter（C）尚未实施。日期：2026-09-26。
> 上级：[总计划](2026-09-26-session-store-plan.md)。母需求：[issue](2026-09-26-session-store-remote-backend.md)。本文件是分计划间行为接口和结果语义的唯一计划事实源；底层机制见 B/C，调用顺序见 E。

## 1. 目标与代码事实

当前 `peri-acp-types/src/store.rs::ThreadStore` 混合数据、workspace、owner/dirty 与缓存方法。`peri-resources/src/sessions/sqlite_store.rs` 是 pool owner 和该 trait 的实现，生产消费大多持 `Arc<dyn ThreadStore>`。因此既不能只重命名 trait，也无需新增全套资源框架。

目标是：业务侧通过一个会话资源门面获得稳定行为；内部本机执行与数据存取分别实现。adapter 的接口同样是完整行为，不提供通用数据库操作。

## 2. 模块与类型归属

以下名称为计划固定术语，后续实现若改名需同步本表；不是已存在 API。

| 内容 | 计划位置 | 可见性/责任 |
| --- | --- | --- |
| `SessionResources` trait | `peri-acp-types/src/session_resources.rs`（拟新增） | 消费侧统一门面，`Arc<dyn SessionResources>` 注入 |
| 行为输入输出、`SessionResourceError` | 上述模块及必要的相邻子模块 | 中性领域类型，不依赖 SDK/资源实现 |
| `SessionDataPort` | `peri-resources/src/sessions/data.rs`（拟新增） | 资源模块内部行为 seam；两 adapter 实现，不给业务裸写句柄 |
| `LocalExecution` | `peri-resources/src/sessions/execution/`（拟拆分） | 本机发现、registry、root owner、写入准入与排空 |
| `SessionResourcesImpl` | `peri-resources/src/sessions/resources.rs`（拟新增） | 门面实现，组合两面，统一写入授权及恢复门禁 |
| payload/flags/inherited 编码 | 现有 `peri-acp-types/src/store.rs` | 保持格式，旧混合 trait 最终删除；按体量分文件，不把数据类型删掉 |
| 纯历史变换 | `peri-acp-types/src/store/history.rs`（拟新增） | 显式输入输出；不读取时钟、生成 UUID、访问环境或数据库 |
| frozen 构建/编码 | 现有 ACP `session/{frozen,frozen_snapshot}.rs` | 不为存储反向依赖 ACP；adapter 保存版本化 opaque snapshot，领域构建/解码保持原 owner |

不新增 crate；不建立 generic repository、UnitOfWork、transaction DSL、mutations 数组或 `execute_plan`。业务侧也不手工拿 data+execution 两个句柄拼装。

## 2.1 最终公共类型与字段（review-2 闭合）

以下为**最终类型**；实施时不得再引入第二个门面、第二个存储句柄或按后端分支的业务字段。名称为计划固定术语，改名需同步本表。

### 门面与装配链（唯一句柄）

| 位置 | 最终类型 | 说明 |
| --- | --- | --- |
| `peri-acp-types::session_resources::SessionResources` | `pub trait SessionResources: Send + Sync` | 唯一跨 crate 存储契约，取代 `store::ThreadStore` |
| `peri-resources::Resources` | 字段 `session_resources: Arc<dyn SessionResources>`（原 `thread_store`） | `Resources::open/open_with` 签名不变；访问器 `session_resources()` 取代 `thread_store()` |
| `peri-agent::resources` | `open_session_resources{,_with}(…) -> anyhow::Result<Arc<dyn SessionResources>>` | 取代 `open_thread_store*`；仍只做声明边转发 |
| `peri-acp::host::assemble::HostAssemblyInput` | `session_resources: Arc<dyn SessionResources>`（替换 `thread_store`） | provider/peri_config/config_source/permission_mode/cwd/bare/drive_cron_tick 不变 |
| `peri-acp::host::AcpServerConfig` | **删除** `thread_store` 字段；保留 `controller` 与 `session_manager` | 存储只经 `cfg.controller.sessions()` 取得，不再有第二条等价路径 |
| `peri-controller::Controller` | `sessions: Arc<dyn SessionResources>`；`Controller::new(Arc<dyn SessionResources>)`；`pub fn sessions(&self) -> Arc<dyn SessionResources>` | **名称保留**（review-2 明确允许），只改返回类型 |
| `peri-acp::session::SessionManager` | 持有装配传入的同一 `Arc<dyn SessionResources>` | 不新建连接、不另存注册表 |
| `peri-acp::session::command::CommandContext` | 字段 `thread_store: Option<Arc<dyn ThreadStore>>` → `session_resources: Option<Arc<dyn SessionResources>>` | 实体定义在 `peri-acp-types::command::CommandContext`（`peri-acp-types/src/command.rs:79,111`；`peri-acp/src/session/command/mod.rs:36` 再导出），逐引用替换，不保留同义双字段 |
| `peri-agent/src/session/subagent/types.rs::SubagentHost` | 字段 `thread_store: Option<Arc<dyn ThreadStore>>`（`types.rs:100`）→ `session_resources: Option<Arc<dyn SessionResources>>` | 同文件 `SubagentSpawnConfig.thread_store`（`types.rs:183`）与 `SubagentResumeConfig.thread_store`（`types.rs:388`，必填）同批替换，按 E-08 逐引用迁移，不保留双字段 |
| `peri-middlewares/src/subagent/tool/{spawn_context,configuration}.rs` | `spawn_context.rs:155` 的 `thread_store: Arc<dyn ThreadStore>` 字段与 `configuration.rs:121` 的 `with_thread_store(Arc<dyn peri_agent::thread::ThreadStore>)`（`configuration.rs:122` 写 `host.thread_store`）→ 字段/构造器更名为 `session_resources`／`with_session_resources`，取 `Arc<dyn SessionResources>` | 禁止再经 `peri_agent::thread::ThreadStore` 传递，否则旧符号无法删除 |
| `peri-tui/src/app/service_registry.rs::Services` | 字段 `thread_store: Arc<dyn ThreadStore>`（`service_registry.rs:82`）→ `session_resources: Arc<dyn SessionResources>` | TUI 只消费 D 的统一选择结果，不持有具体 adapter |
| `peri_agent::session::transcript::MessageTranscript` | `store: Option<Arc<dyn SessionResources>>` | writer 仍只有一个（§E） |
| `peri_agent::session::subagent::factory::claim::ResumeClaim` | `acquire(Arc<dyn SessionResources>, …)` | 领域语义改为 `claim_child_resume` |
| `peri-acp::host::SessionState` | `execution_owner: Option<Arc<dyn SessionExecutionLease>>` **不变** | 公共执行能力，不是事务句柄 |
| `peri_agent::thread` / `peri_tui::thread` re-export | 生产路径删除 `ThreadStore`/`SqliteThreadStore`/`FilesystemThreadStore`/`open_thread_store_read_only` 导出 | 测试经明确测试支持路径；见 §7.1 |

`Arc<dyn SessionResources>` 是唯一跨层句柄：任何层都不得同时持有门面与数据端口、也不得从门面取回裸 adapter。

### 领域输入/输出字段

| 类型 | 字段 | 约束 |
| --- | --- | --- |
| `NewSession` | `thread_id: ThreadId`; `created_at: String`(RFC3339，构建时一次); `meta: NewSessionMeta`; `binding: SessionBinding`; `frozen: FrozenSnapshotBytes` | UUID/时钟一次生成，重试不重建；`FrozenSnapshotBytes` 是版本化 opaque 字节，沿用现有 JSON envelope |
| `NewSessionMeta` | `title: Option<String>`; `cwd: String`; `parent_thread_id: Option<ThreadId>`; `hidden: bool`; `cancel_policy: CancelPolicy`; `snapshot_at_message_id: Option<MessageId>` | 不携带 `message_count`/`updated_at`/`cached_context`/`context_cache_epoch`（由行为维护） |
| `ForkSnapshot` | `target: NewSession`; `source_id: ThreadId`; `payloads: Vec<PersistedPayload>`; `flags: HashMap<MessageId, MessageFlags>` | 两者已完成 ID 重映射（§6 纯函数）；source 只读 |
| `ChildSnapshot` | `target: NewSession`; `parent_id: ThreadId`; `root_id: ThreadId`; `inherited: InheritedContext` | frozen 由不可变 parent/root 来源解析并复制原字节，不重新扫描目录 |
| `SessionSnapshot` | `meta: ThreadMeta`; `binding: BindingState`; `frozen: FrozenState`; `payloads: Vec<PersistedPayload>`; `flags: HashMap<MessageId, MessageFlags>`; `inherited: InheritedContext` | 一次一致读取；不含 pool、缓存 epoch、事务状态或执行 handle |
| `CompactionChange` | 采用现有 `CompactionLifecycle` 的领域含义：`flag_updates: Vec<(MessageId, MessageFlags)>` + `appended_messages: Vec<BaseMessage>` | 改名去掉“调用方管理提交”的机制命名 |
| `RewindBoundary` | `KeepThrough(MessageId)` / `RemoveFrom(MessageId)` | 区分 transcript 保留目标与用户 rewind 移除目标 |
| `SessionMetaPatch` | `title: Option<Option<String>>`; `status: Option<AgentStatus>`; `cancel_policy: Option<CancelPolicy>`; `config: Option<…>` | 定向更新，禁止整份 `ThreadMeta` 覆盖 |
| `SessionStoreId` / `HostInstallationId` | newtype over `String` | 只出现在资源层与持久化记录；不进业务 DTO、不进 ACP wire |
| `PreparedSessionInputs` | 见 [E §3.2](2026-09-26-session-store-sub-plan-e-consumers.md) | ACP 内部类型，不是数据端口类型 |

只读入口的返回必须能区分 `BindingState::{Bound, LegacyConfirmed, ExternalOrUnregistered, Missing}` 与 `FrozenState::{Present(…), LegacyAbsent, Unsupported}`；`Option<SessionBinding>` 的 `None` 不再同时表达 legacy、不支持和损坏。

## 3. 数据与访问视图

### 3.1 复用与新增领域输入

继续使用 `ThreadId`、`MessageId`、`PersistedPayload`、`MessageFlags`、`InheritedContext`、`SessionBinding`、`ScopedThreadQuery/Page` 等现有类型。

拟新增：

- `NewSession`：固定 thread identity/创建时间、初始 metadata、不可变 binding、完整 frozen snapshot；UUID/时钟在构建层产生一次，不在自动重试时重建。
- `ForkSnapshot`：目标身份、source 身份/确定截止点、由 source 精确复制的 binding/frozen（装在 `target: NewSession` 内）、重映射后的 own payload/flags。不是远端让 source 再执行一次 fork 算法。字段以 §2.1 表为准。
- `ChildSnapshot`：parent/root 归属、继承 payload/flags、binding；冻结来源由不可变 parent/root 关系解析，数据行为校验根 frozen 存在且有效存储，必要复制其原始字节，禁止重新扫描目录。Agent 不编码 ACP frozen，不把父历史当前 flags 当继承快照。
- `SessionSnapshot`：metadata、binding 分类、frozen 状态、own payload/flags、inherited 的一致视图；不含 pool、缓存 epoch、事务状态或执行 handle。
- `CompactionChange`：采用现有 `CompactionLifecycle` 的领域含义（flags 更新与摘要追加），移除“调用方管理提交”的机制命名。
- `RewindBoundary::{KeepThrough,RemoveFrom}`：显式区分保留目标与移除目标。现有 transcript rewind 保留目标，而 ACP 用户 rewind 会移除目标及以后，禁止合并时失真。
- 定向 metadata 输入：标题、status、已有实际需要的 config/cancel policy 等；不允许借整份 `ThreadMeta` 修改 cwd、binding、计数、缓存或父子身份。

frozen 用什么 Rust wrapper 不改变现有 JSON envelope；格式升级不在范围。小型 metadata/summary 读取不能为了共用 `SessionSnapshot` 加载整份历史。

### 3.2 缺失必须有语义

不得再以 `Option<SessionBinding>` 的 None 同时表达 legacy、不支持和损坏。区分本机确认的 legacy、已绑定、外来/本机登记缺失、数据损坏、版本不支持；后两种是错误。本机 legacy 是来源和记录状态联合判定，Turso 首期不启用 legacy 自动接纳。

只读历史允许 binding 的本机位置不可用；可执行恢复要求有效本机登记。frozen 缺失仅在已有 legacy 规则允许时补齐，不能把不支持读快照当作缺失。

## 4. 行为接口清单

以下是行为粒度与后置条件，不要求每行必须独立成 trait；实现可合并同义读入口，但不能遗漏生产场景。

| 门面行为（拟名） | 输入/输出 | 数据端对应行为与保证 |
| --- | --- | --- |
| `inspect_availability` | 行为能力、访问模式、执行可用性 | 不暴露数据库 transaction/CAS 能力 |
| `resolve_workspace` / `validate_session` | cwd 或 session identity → 已验证 workspace | 本机执行负责；数据端只读取持久化事实 |
| `create_session` | `NewSession` → 会话身份与 root owner | `save_new_session` 完整保存 meta/binding/frozen；owner 成功后才返回执行准入 |
| `abandon_initialization` | 未发布会话 identity、有效 owner 与已排空执行资源的上下文 → 已撤销或明确阻塞 | 门面内部撤销本次初始化数据/登记；只针对本次未发布创建，不是通用 rollback，不修改既有 source 会话 |
| `adopt_legacy_session` | 已确认 legacy、保存 cwd 和候选冻结输入 → 权威恢复事实 | 接纳结果整体成立，竞争时返回胜者事实，不返回 CAS bool |
| `load_session_snapshot` | identity → `SessionSnapshot` | payload/flags/frozen/inherited 一致读取，不让调用方拼多次跨时刻查询 |
| `load_session_meta` / `list_sessions` | ID 或 scope/cursor/limit → 小型投影 | 不加载历史/大快照；列表数据端过滤 |
| `list_children` / `list_session_tree` | parent/root → 所需树关系与 metadata | 为现有子 Agent 查询保留，不滥用完整历史 |
| `append_history` | thread + canonical payload 批次 | 稳定顺序、计数和自动标题一致维护；相同 ID 的冲突不可静默忽略 |
| `save_fork` | `ForkSnapshot` → 新根身份和 owner | 完整目标快照保存，source 未改变 |
| `save_child` | `ChildSnapshot` → 新 child identity | 继承区和父子关系一起成立；使用已存在根 owner |
| `claim_child_resume` | child/root identity → 权威 metadata 与领域认领 handle | 在有效根 owner 下串行认领；handle 接收开始运行/移交后台/准备失败/终止等领域结果，内部保存 active/原状态/终态，调用方不拼补偿写入 |
| `apply_compaction` | thread + `CompactionChange` | 摘要/flags/计数/缓存视图全部生效或不生效 |
| `apply_message_projections` | thread + flags 变更集 | Micro/投影更新整体维护缓存，不让 writer 逐条写后另 invalidation |
| `rewind_history` / `remove_history_entries` | thread + 显式边界/ID 集合 | 只改目标历史，派生计数/缓存同步 |
| `delete_session_tree` | 目标 identity | 数据删除一致完成；执行关闭和 pending 证据不被 cascade 提前抹掉 |
| `rename_session` / `set_status` 等定向更新 | identity + 领域字段 | 不整份覆盖 metadata，避免并发丢更新 |
| `acquire_execution` / `reset_dirty_execution` | identity / 精确代际 | 本机 owner 领域行为；先排除持久化未知，再遵守现有 dirty 风险确认 |
| `recover_session_persistence` | identity → 可重载或仍阻塞 | adapter 内部收敛，不让调用方提供 operation token |
| 持久化排空/部署关闭 | 资源 owner 发起，业务方仅等待所需完成 | 保留唯一关闭权，不以 Drop 或 enqueue 成功充当保存成功 |

`SessionExecutionLease` 是执行能力，不是数据库锁参数。保留其领域职责；普通数据方法内部由门面检查本 root 有效 owner。只读句柄不能靠创建一个 unbound thread 绕过授权。

`SessionExecutionLease` 的最终公共面仍是 `fn thread_id(&self) -> &ThreadId` + `async fn mark_clean(&self) -> Result<()>` 两项，不含事务、代际参数、CAS 或重试令牌。未决持久化是**内部**前置条件：`mark_clean`、执行准入（`acquire_execution`）、`reset_dirty_execution`、`delete_session_tree`、`save_fork` 与 `create_session` 都先在门面内部检查本 root 是否存在未决持久化，存在即返回 `PersistenceUncertain`，不靠调用方先查。`reset_dirty_execution` 只解除本机 dirty 代际，永远不解除未决持久化，也不接受把普通 dirty 自动映射成 reset。

## 5. 结果、错误与能力

### 5.1 结果语义

- 正常成功：领域后置条件已满足，包括存储可恢复性；之后的调用不需补做 flags/cache/补偿。
- 确定未生效：输入无效、行为不支持、权限拒绝或已证实无保存结果等。
- `PersistenceUncertain`：无法证明生效与否，携带安全 session/root 关联，不含 SQL、token、事务 ID。门面使热态失效并阻塞续写/clean。
- 「数据已完整保存，但执行准入失败」单独表达，包含可定位的 session identity；不得包装成确定未创建。完整数据的撤销若属于新建失败策略，由门面内部承担并报告是否完成。

错误原因和效果确定性分别建模，不从 `Timeout`/`Unavailable` 自动推导未生效；实现用结构化枚举避免互相矛盾的布尔组合。仅保留调用方确实需要分支的结果，不输出内部恢复状态机。

### 5.2 能力与权限

行为能力描述「能否安全完成 compact/派生快照/rewind 等」；凡声明支持都必须满足完整后置条件，不允许 `supports_transactions` 或 `supports_cas`。

访问模式为读取能力/写权限事实，不把只写后端用于可执行恢复。执行资格由本机准入另判。静态能力检查不替代实际操作错误；权限动态变化和网络失败仍返回明确结果。

`WorkspaceError` 保留本机语义；读取失败不再一律包装为 workspace 不可用。旧 ACP/CLI 映射在 E/D 收敛，业务不识别 SQLx/Turso 错误字符串。

### 5.3 三个独立枚举（review-2 闭合）

```rust
/// 本次打开实际取得的读写权限（配置/授权事实，不推导数据能力）
pub enum AccessMode { ReadWrite, ReadOnly }

/// 后端能安全完成的会话行为面
pub enum DataCapabilities { Complete, HistoryReadOnly }

/// 本机执行资格（与数据能力、与访问模式都独立）
pub enum ExecutionAvailability {
    Available,
    NoLocalRegistration,
    BindingMissing,
    WorkspaceUnavailable,
    OwnedElsewhere,
    Dirty(RecoveryRequiredDetails),
    PersistencePending,
    ReadOnlyStore,
    Unsupported,
}
```

不变量：

- 三者互不推导。`AccessMode::ReadOnly` 不蕴含 `HistoryReadOnly`（远程只读授权下数据能力仍可为 `Complete`，只是本次不允许写）；`DataCapabilities::Complete` 不蕴含可执行（缺本机登记/binding 时 `ExecutionAvailability` 仍拒绝）。
- `DataCapabilities::Complete` 表示 §4 全部行为都满足完整后置条件；`HistoryReadOnly` 下所有 mutation 在副作用前返回 `Unsupported`。凡声明 `Complete` 的 adapter 不得对任何行为退化为 no-op。
- `ExecutionAvailability::PersistencePending`（C 的未决写）与 `Dirty`（本机执行代际）是两个不同事实，映射到不同 wire 结果，禁止互相代替。
- 显式只读（`AccessMode::ReadOnly`）不初始化 schema、不创建 StoreId、不写本机登记、不取得 owner，也不创建任何本机文件/目录。

### 5.4 写入三态与 guard 语义（跨 B/C 统一）

```rust
pub enum MutationOutcome {
    Applied,                            // 领域后置条件已满足，含存储可恢复性
    NotApplied(NotAppliedReason),       // 已证明无效果：输入无效/不支持/权限拒绝/发送前失败/引擎明确拒绝
    Unknown(UnknownReason),             // 未证明：超时、断连、取消、提交后丢响应、部分结果
}
```

- 只有 `Applied | NotApplied` 允许释放写入准入（`ExecutionWriteGuard` 的 finish）；`Unknown` 必须留下未决证据并阻塞同根后续写入与 clean。
- `NotApplied` 必须来自实际证据（本地事务未开始、引擎明确拒绝、发送前失败），不得由 `Timeout`/`Unavailable`/取消自动推导；一次空查询或一次超时都不是证据。
- 门面按三态决定热态失效、可否 clean、可否放行新 owner；内部状态机不外泄。

## 6. 纯化范围与后置验证

- 提取 `session_fork.rs` 的 payload/flags 映射，参数提供新旧 MessageId 映射；UUID 生成留调用层。
- 提取 transcript 的 compact 引用合法性：消息存在、不修改 ancestor、追加 ID 不冲突。adapter 同时基于权威数据检查归属，不能只信纯函数对热态的验证。
- 复用 `InheritedContext` 的版本及引用完整性校验，纯裁剪传入截止点。
- 提取 rewind 边界与 canonical payload 裁剪；不在此改文件恢复算法。
- frozen/config 构建包含环境读取，不宣称是纯函数；只将确定性转换与 I/O 分层，具体见 E。

不为了“纯”复制一套消息模型，不从 adapter 反向调用 compact planner，不改变已有算法结果。

## 7. 旧方法处置

| 旧方法族 | 处置 |
| --- | --- |
| `create_thread`、`create_bound_thread`、独立 frozen/inherited 写入 | 生产创建迁到完整 new/fork/child/legacy 行为；测试替身显式实现所需能力 |
| `append_messages`、`append_message`、`append_payloads` | canonical payload 作为存储事实，便利包装保证语义等价即可保留 |
| `load_messages`、`load_payloads`、`load_context*`、flags 读取 | 全历史恢复走一致 snapshot；消息视图从 snapshot 派生，保留必要轻量查询 |
| `update_meta` | 改定向更新，逐个核对实际字段消费者 |
| `update_message_flags` | 由完整 projection/compact 行为替代逐项持久化编排 |
| `invalidate_context_cache`、`get_context_cache_epoch` | 不再是消费侧存储步骤，内部派生维护 |
| `supports_compaction_lifecycle` | 改领域行为能力判定 |
| 默认 no-op/假缺失 | 删除该兼容语义，未支持显式报错或类型上不提供；便利函数不一刀切删除 |

最终生产不得同时存在旧 trait 和新门面两条可独立写入的路径。短期兼容 wrapper 只能委托新门面，设置迁移退出清单，不作为完成态。

### 7.1 旧 API 退出以编译为证据（review-2 闭合）

退出不是“grep 不到即可”，而是旧符号在类型上不可达：

1. `peri-acp-types::store::ThreadStore`（trait）与 `store_frozen_snapshot_if_absent` 等默认体随 trait 一起**删除**；数据类型（`PersistedPayload`/`MessageFlags`/`InheritedContext`/`CompactionLifecycle`/序列化 helper）保留在 `peri-acp-types`，只是不再挂在 trait 上。
2. `SqliteThreadStore`/`FilesystemThreadStore` 不再从 `peri_agent::thread`、`peri_tui::thread` 生产路径导出；`peri-resources` 只公开返回 `Arc<dyn SessionResources>` 的构造点。
3. 证据 = 符号不存在 + `cargo build --workspace --all-targets`、`cargo clippy --workspace --all-targets -- -D warnings` 通过；任何残留引用都直接编译失败。grep 仅用于**发现遗漏**，不作为通过证据。
4. 测试替身与故障注入：实现 `SessionResources` 的测试替身必须在自身声明 `DataCapabilities`（例如 `HistoryReadOnly`），不得默认继承完整能力。peri-resources 内部的私有传输 seam 只经 `#[cfg(any(test, feature = "test-support"))]` 暴露，该 feature 不被任何生产二进制启用（F 记录启用方式）。
5. 不为“编译失败测试”引入 `trybuild` 等新依赖；退出证明由符号删除 + 全 target 编译承担。
6. `FilesystemThreadStore` 是测试替身、不是完整 adapter 契约实现：迁移后声明 `DataCapabilities::HistoryReadOnly`，未支持的行为必须**显式失败**。已核实其现状（`peri-resources/src/sessions/filesystem.rs:452-463` 的 `update_message_flags` 是静默 no-op、`commit_compaction_lifecycle` 直接 bail），该 no-op 一并删除；需要 flags/compact 语义的测试改用 SQLite 实现。生产路径不得构造测试替身，`DataCapabilities::Complete` 也不得由测试替身声明。

## 8. 任务、测试与完成条件

- A-01：按第 4/7 节列全生产调用行为，核对真实缺失项，不先建大量抽象。
- A-02：定义门面、领域数据、效果/错误/能力分类和内部端口；过编译迁移时不靠新增默认成功兜底。
- A-03：提取纯变换并添加显式输入输出测试；消息/frozen 格式不变。
- A-04：与 B/C/E 检查创建、执行准入、恢复和关闭边界；没有事务机制跨端口。
- A-05：完成旧方法映射，更新契约注释及后续 doc tests 路由。

验证由 F 的 V-01/02/06/12 覆盖；实施前 A 的接口与结果语义评审通过才允许 B/C 扩散。类型/方法的变更必须同步各子计划，不允许由某个 adapter 的 SDK 决定公共接口。
