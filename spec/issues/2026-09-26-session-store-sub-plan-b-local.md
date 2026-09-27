# 会话资源拆分 — 子计划 B：SQLite Adapter 与本机执行职责

> 状态：**B 数据侧与执行侧均已落代码**（2026-09-26）：同一库共享句柄（`SqliteSessionDatabase`）、SQLite 数据端口实现（`SqliteSessionData`）、schema v6→v7（执行行去外键 + 生命周期锚点 + 本机登记表），以及本机执行面（`LocalExecution`）与组合门面（`SessionResourcesImpl`，B-02/B-03）。消费侧迁移（E）与远程 adapter（C）尚未实施；旧 `ThreadStore` 生产路径仍在，等 E 切换。日期：2026-09-26。
> 上级：[总计划](2026-09-26-session-store-plan.md)。依赖：[A](2026-09-26-session-store-sub-plan-a-contracts.md)。远程内部确认见 [C](2026-09-26-session-store-sub-plan-c-turso.md)，ACP/Agent 准入顺序见 [E](2026-09-26-session-store-sub-plan-e-consumers.md)，验收见 [F](2026-09-26-session-store-sub-plan-f-verification.md)。

## 1. 当前实现基线

> 实施后基线（2026-09-26）：pool/read-only/canonical 路径/lease 弱引用表已移入私有 `SqliteSessionDatabase`，数据面与执行面共用同一句柄；`SqliteThreadStore` 降为消费侧迁移桥并转发到该句柄。`CURRENT_SCHEMA_VERSION` 已是 7。

- `SqliteThreadStore` 持唯一 `SqlitePool`、read-only 标记、canonical 数据库路径及 root lease 弱引用表（`sqlite_store.rs:34-41`）。
- `schema.rs:11::CURRENT_SCHEMA_VERSION` 当前为 **6**（review-2 复核确认，不要沿用旧索引中的版本数）。`execution_runs.thread_id` 外键指向 `threads` 且 `ON DELETE CASCADE`（`schema.rs:210-213`）；`session_bindings.thread_id`、`messages.thread_id` 同样级联。现有 lease 必须在 thread 存在后取得。
- `schema.rs:34-92::inspect` 只认 `PRAGMA user_version == CURRENT`、2..5 与「无版本但有 threads/messages」的 legacy；其他版本直接 `UnsupportedSchemaVersion{found, supported}`。升级在 `BEGIN IMMEDIATE` 内完成，DDL 与 `PRAGMA user_version` 同事务提交（`schema.rs:133-242`）。
- `workspace.rs` 将项目/工作区登记、binding、新建/legacy 接纳与列表 SQL 放在同一模块；`discovery.rs` 是本机 Git/文件证据。
- `execution.rs` 用稳定 sidecar OS 锁（`lock_execution`，`db_path + ".execution-locks/<sha256(thread_id)>.lock"`，锁文件永不删除）、代际记录、mutation gate 和 `mutation_uncertain` 保护执行关闭；Drop 不代表 clean。`mark_clean` 用 `UPDATE … WHERE thread_id = ? AND generation = ? AND clean = 0` 精确 CAS，并在 `threads` 行已被删除（新建补偿路径）时容忍记录缺失（`execution.rs:59-103`）。
- `compaction.rs` 的 Full lifecycle 已有同事务 flags/摘要/计数/cache 更新；部分 flags/delete/rewind 后另行 invalidation——实际逐条写来自消费侧：`peri-agent/src/session/transcript/persistence.rs:175-191` 对 `ApplyCompactionBatch` 逐条 `update_message_flags` 后再 `invalidate_context_cache`，需收口为完整行为。
- 写打开/只读回退的版本与列形状判定不同；默认 `threads.db` 中还可能有其他业务表，不能复制 schema 时只保留本模块表。

本计划不重新设计本机项目/目录身份规则，不新增后台 daemon，不让远程 adapter 承担 Git 或 OS 锁。

## 2. 逻辑拆分，不强迫物理拆库

| 当前 | 目标逻辑归属 | 物理处理 |
| --- | --- | --- |
| `SqliteThreadStore` pool/read_only/db_path | 私有 `SqliteSessionDatabase`（拟名） | 同一 pool、同一库；不造第二条连接真相 |
| messages、flags、thread metadata、frozen/inherited、列表 | `SqliteSessionData` 实现 A 的内部数据端口 | 保留原表及编码，新增行为专用方法 |
| `discovery.rs` | 本机 execution/discovery | 先迁职责再移文件，保留 key-object/旧 Git 兼容测试 |
| projects/workspaces 证据、OS锁、execution_runs | 本机 execution registry | SQLite 模式仍可同库存放；逻辑 owner 不因物理位置改变 |
| 写入授权、根子关联、未知结果及 close 协调 | `SessionResourcesImpl` + 本机执行 owner | 消费只能通过门面进入写行为 |
| 内部 SQL 事务与关系检查 | SQLite 私有协调代码 | 不跨 A 接口传 transaction/connection/closure |

SQLite 的同库原子性可由行为专用私有函数继续维护；不要求把每张表强行拆成 trait。若一个函数需同时复核本机事实与保存数据，可由私有 SQLite 组合实现调用本机验证器及同库 SQL，不把 Git 或目录解释职责移给数据 adapter。

## 3. 事实持有者

| 事实 | SQLite 默认 | Turso 组合 |
| --- | --- | --- |
| canonical 历史、binding、frozen、继承快照、thread 树 | 原 `threads.db` | 远端数据 adapter |
| 项目/工作区登记、路径/文件对象/Git证据 | 原项目/工作区表 | 本机受保护 registry（同一张本机表） |
| 列表用 root/cwd 展示信息 | 现有 SQL 投影 | 随 durable 会话保存的展示事实，不用于执行验证 |
| root owner、dirty generation、准入/排空状态 | `execution_runs` + OS sidecar | 本机 `execution_runs` + OS sidecar（**同一张本机表**） |
| 创建意图 / 删除墓碑 / 未决操作锚点 | 新表 `session_lifecycle_commitments`（§5.3） | 同一张本机表；C 只写不透明 detail |
| 远程未决写入的远端侧收据 | 不引入云专用机制 | 远端 `op_ledger`（C §5），本机只存锚点与 operation_id |
| StoreId 与宿主登记 | 本机沿 canonical path 既有身份范围 | 远端稳定 StoreId + 本机 `session_store_registrations`（§6.1） |

远程 registry 不是历史缓存/复制库，不存另一份可编辑 canonical transcript。只保存本机执行事实和恢复最小信息。durable binding 只有一份权威记录，本机允许保存其不可变引用/校验摘要而不是能独立改写的副本。

**本机状态库的位置与形态（review-2 关闭）**：本机 registry 就是 canonical 本机路径下同一个 SQLite 库（默认 `~/.peri/threads/threads.db`），远程模式下它**只**保存本机执行事实（projects/workspaces 证据、`execution_runs`、`session_lifecycle_commitments`、`session_store_registrations`），不保存 canonical 历史，也不引入第二个本机数据库文件。因此远程模式下不允许把 `execution_runs` 留在对 `threads` 的外键上（该行在远程模式下根本不存在），见 §5.3 的 v7 迁移。

## 4. 统一写入准入

### 4.1 规则

1. 门面在所有 mutation 前检查行为能力、访问权限、thread/root 归属、本机有效 owner 和未决持久化状态。
2. 子会话沿 parent 链找根，保留循环检测及 legacy 已接纳根约束；不能因为 child 自身无 binding 就免授权。
3. mutation guard 覆盖真正的 adapter 工作完成。关闭停止新增准入，再等待已准入写入；不得在等待外部 future 时持普通全局 mutex。
4. 数据端口不公开到 Agent/ACP/middlewares，生产构造不导出无 guard 的 adapter。`create_thread` 对全未绑定链的旧豁免只可作为明确 legacy/测试路径，不能用于新生产会话。
5. 只在**效果已确定**时才 `finish()`。门面已按三态落地（`resources/gate.rs::WriteScope::settle`）：`Applied | NotApplied` 才释放写入准入，`Unknown`（含取消、超时、提交后丢响应）丢弃范围，由 `Drop` 置 `mutation_uncertain` 并保留未决证据；`MutationOutcome` 由 `SessionResourceError::effect()` 派生，调用方拼不出矛盾组合。桥（`ThreadStore` impl）里带显式事务的两处已改用 `TransactionEffect` 标出「提交自身的失败」；其余单语句写入在本地 SQLite 上「返回 `Err` 即已回滚」可证明，保持 SQL 层粒度并由 E 随桥删除。数据面（`sqlite_store/session_data.rs`）的写事务提交阶段同口径：全部 `commit()` 失败经 `failure.rs::commit_failure` 固定为 `Unknown`（IO/驱动未分类失败不再冒充「没生效」），由租约留下未决证据并阻断续写与 clean；`commit()` 之前的失败与只读事务（`load_snapshot`）保持原因分类，不判 `Unknown`。同一口径覆盖本机执行面（`local.rs::create_with_lease` / `admit_existing`）与 compaction 事务（`compaction.rs` 三处 `commit()`）；`write_failure` / `execution_failure` 先保留已带效果的领域失败（`Unknown` / `Applied` 不经 `anyhow` 第二次降级），桥侧三个 compaction 转发方法据此只在确定效果时 `finish()`（2026-09-26 提交前核实修复）。
6. 门面不得将跨根授权混入通用 SQL 参数；执行能力是本机领域权限，adapter 仅接收已准入的完整行为。

### 4.2 close 与 dirty

ACP/Agent 排空 owned 资源后请求结清；门面再等待 persistence worker/adapter 内部任务及未知结果收敛，最后写 clean 并释放锁。

- `Incomplete` 保留 owner、锁及唯一关闭句柄；不移动句柄后让重试无从等待。
- 本机普通 dirty 维持现有精确 `(thread_id,generation)` CAS 语义，但 CAS 不出执行实现。
- 远程 `PersistenceUncertain` 不能映射成普通 `RecoveryRequiredDetails`；未协商 recovery 客户端的自动 reset 路径同样不能解除未知写。`ReadOnlyAdmission::from_workspace_error`（`peri-acp-types/src/workspace.rs:141-154`）只认 `ExecutionBusy | RecoveryRequired | ExecutionLeaseRequired`，`PersistenceUncertain` 必须落在该集合之外并单独映射。
- `reset_dirty_execution` 自身检查未决持久化，不能仅依赖 ACP 先检查。
- 进程崩溃只释放 OS锁；下一进程先恢复 C 的未决写，再处理普通 dirty。
- delete 后仍保留未完成本机执行/恢复证据；远端记录被删除不证明进程和请求均已结束。

### 4.3 未决持久化门禁矩阵（review-2 闭合）

“未决持久化”= 本 root 存在 `session_lifecycle_commitments` 中 `kind='mutation_pending'` 且 `state != closed` 的锚点（其远端事实由 C 的 `op_ledger` 决定）。门禁对以下入口一律生效，且检查在门面内部，不靠调用方：

| 入口 | 存在未决时 | 说明 |
| --- | --- | --- |
| `acquire_execution` | `PersistenceUncertain`，不发 lease | 新 owner 不得与仍可能生效的旧写并发 |
| `mark_clean` | `RecoveryRequired`/`PersistenceUncertain`，不放锁 | 现有 `mutation_uncertain` 分支之外再查锚点（跨进程可见） |
| `reset_dirty_execution` | `PersistenceUncertain`，不解除代际 | 普通 dirty reset 不解除未决写 |
| `delete_session_tree` | `PersistenceUncertain`，不删除 | 删除前必须先有未决收敛证据（§4.4 墓碑） |
| `save_fork` / `save_child` / `create_session` | `PersistenceUncertain`（source/root 有未决） | 不把未决来源复制进新会话 |
| rewrite/compact/append 等普通 mutation | 拒绝并等待收敛 | 与 §4.1 规则 1 一致 |
| 只读读取与列表 | 允许 | 历史可读性与执行资格分开表达 |

### 4.4 删除的独立锚点（review-2 闭合）

级联事实：`DELETE FROM threads` 会连带删除 `execution_runs`、`session_bindings`、`messages` 行，因此**不能**用这些表证明“删除时该会话已收敛”。同时 `require_execution_lease` 对已绑定根要求活 lease，删除本身已受 owner 约束。为使删除后的证据不被 cascade 抹掉：

1. 删除前：门面确认 root 无未决持久化与未完成执行（§4.3）。
2. 删除事务内（与 `DELETE FROM threads …` 同一 `BEGIN IMMEDIATE`）：写入 `session_lifecycle_commitments` 墓碑行 `kind='tombstone', state='deleting'`，使“删除是刻意行为”与数据删除同事务成立。**同一事务内显式 `DELETE FROM execution_runs WHERE thread_id IN (被删 thread 集合)`**：现有 `delete_thread`（`sqlite_store.rs:402-434`）只删 `threads` 行，`messages`/`execution_runs` 行靠 `ON DELETE CASCADE` 清除；v7 去掉 `execution_runs` 的外键后该级联对它不再生效，若仍按原样只删 `threads`，会静默留下孤儿执行行（dirty 行永不收敛）。显式删除是保持本地 delete 语义与清理代价不变的必要步骤。
3. 提交后：本地 adapter 将墓碑置 `state='deleted'`；若进程在两步之间崩溃，`state='deleting'` 且 `threads` 行已不存在的墓碑按 `deleted` 处理（幂等修复），**永不复活**该 identity。
4. 远程 adapter：本机与远端不是一个事务，顺序固定为「先写本机墓碑 `deleting`（durable 锚点）→ 远端在 C 的操作身份内删除 → 远端确认后把墓碑置 `deleted`」。结果未知时保留 `deleting` + `mutation_pending` 锚点，不报告成功、不放行同 identity 的重新创建/登记；`deleting` 状态本身不被当作已删除，除非远端后续读到目标已不存在且封闭竞争确认（C §5.1）。
5. `mark_clean` 的“行不存在”容忍分支从此要求存在对应墓碑（`deleting|deleted`），而不是只看 `threads` 行是否缺失。**已实施**：判据切到 `session_lifecycle_commitments` 的 `tombstone`；同时把生产桥的 `delete_thread` 改成与数据面删除同语义（同事务写墓碑 + 显式删 `execution_runs` 行 + 删 `threads` 行，提交后置 `deleted`），否则 v7 去掉外键后旧实现会留下孤儿执行行，且 ACP 的 `delete_thread` + `mark_clean()` 补偿组合会失去依据。
6. 墓碑不按 TTL 自动清理：首期无法证明旧请求已失效时保留一行代价，不牺牲正确性换清理。同 identity 的重新登记被墓碑拒绝。

## 5. 创建与本机准入

### 5.1 默认路线

采用 **完整 durable 会话保存 + 本机执行准入**，而非先创建缺 frozen 的可用行。顺序固定为：

1. **frozen 预备**：E 用 `PreparedSessionInputs` 一次读取 config/plugins/skills/agents/date/env 并构建 frozen 字节；只读、无 cache repair、无执行资源（E §3.2）。
2. **本机 creation intent**：以稳定 `ThreadId` 在 `session_lifecycle_commitments` 写 `kind='creation_intent', state='reserved'`，并预留稳定 OS 锁（`lock_execution`）。此预留是私有准入机制，不是对外 transaction，也不是尚未存在 thread 的现有 execution lease。意图落盘失败即不发送任何远程请求。
3. **完整数据保存**：数据行为一次保存 meta、binding、frozen 与初始化必需数据。
4. **执行代际**：成功后建立 durable 执行代际（`execution_runs` 行 `clean=0`）并转为 root owner，复核关键目录对象。
5. **返回并发布**：返回已保存身份/owner/权威事实，ACP 才开始有副作用的环境装配（MCP/LSP/hooks/cron）。

新 ThreadId 及初始内容在整个尝试中稳定；调用方超时不能再生成一个新 ID 自动重试。

**本地（SQLite）adapter 的顺序塌缩**：同库同事务内可顺序插入 `threads` → `session_bindings` → frozen → `execution_runs`，因此 creation intent、数据保存与执行代际在本地是**一个事务**；`reserved`/`data_saved` 中间态在该模式下不可达，也不需要恢复逻辑。生产新建路径按本地事实一次提交，不为了与远程对称而人为拆成多步。下面的状态机与中断表只在「durable 数据不在本机」的远程模式下承担恢复责任。

### 5.2 创建状态机与崩溃点（review-2 闭合）

`session_lifecycle_commitments` 中 `kind='creation_intent'` 的 `state`：

| state | 含义 | 允许的后续 |
| --- | --- | --- |
| `reserved` | 意图已落盘、已占 OS 锁，未确认任何 durable 数据 | 继续保存 / 收敛后 abandon |
| `data_saved` | durable 数据已确认完整（meta/binding/frozen 原子成立） | 准入（admit）或报告 `SavedButNotAdmitted` |
| `admitted` | 执行代际已建立并持有 owner | 转入正常生命周期；锚点改为依赖 `execution_runs` |
| `abandoned` | 初始化被撤销（数据未发布或已补偿），锁已释放 | 终态；同 identity 不再复用 |

| 崩溃点 | 恢复依据与行为 |
| --- | --- |
| 意图提交前崩溃 | 无副作用、无锁；同一 ThreadId 可安全重试（等价于未开始） |
| `reserved` 且尚未发送远程请求 | 锚点 + C 的 `op_ledger` 均无该 operation → 由 C 执行封闭竞争后确认未生效，abandon |
| `reserved` 且请求在途/已丢响应 | 先读 C 的 `op_ledger`：`applied` → 走 `data_saved` 分支；缺失/未知 → 由 C 封闭竞争，**封闭确认成功**才允许 abandon；封闭仍未知则保持阻塞 |
| `data_saved`，执行代际未写 | 业务前提仍成立（workspace 证据一致）→ 准入收敛；否则报 `SavedButNotAdmitted`，保留 identity，不重造 binding/frozen |
| 执行代际已写、环境装配失败 | `execution_runs.clean=0` 即普通 dirty；ACP 排空资源后请求 `abandon_initialization`，由门面内部补偿并 mark_clean |
| 进程在完整保存后崩溃 | 下次依据原 creation intent + 完整事实收敛；绝不生成新 binding/frozen 或新 ThreadId |

`SavedButNotAdmitted` 是独立领域结果：数据已完整保存、可定位 identity、执行准入未成立；不得包装成“确定未创建”，也不得让调用方据此删除数据。`abandon_initialization` 只处理本次未发布创建（new/fork/child），内部完成数据撤销 + 墓碑并报告是否完成；不是通用 rollback。

创建意图需要本地 schema 变更：随 §5.3 的 v7 受控升级进行，完整保留旧数据。默认 SQLite 不因 Turso 引入独立本地 registry 数据库；远程模式的本地事实仍落在同一本机库（§3）。

### 5.3 本地 schema v6 → v7 迁移矩阵（review-2 闭合）

新增/变更 SQL（全部在 `BEGIN IMMEDIATE` 内，与 `PRAGMA user_version = 7` 同事务）：

```sql
-- 1) 生命周期锚点：无外键，故不被 threads 级联删除
CREATE TABLE session_lifecycle_commitments (
    thread_id    TEXT PRIMARY KEY,
    root_id      TEXT NOT NULL,
    kind         TEXT NOT NULL,   -- 'creation_intent' | 'mutation_pending' | 'tombstone'
    state        TEXT NOT NULL,   -- §5.2 创建态 / §4.4 墓碑态 / closed|unknown（未决）
    generation   INTEGER,
    operation_id TEXT,            -- C 的 operation identity；不导出到消费侧
    detail       TEXT,            -- 不透明恢复信息（禁止凭证/正文）
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE INDEX idx_lifecycle_root_kind ON session_lifecycle_commitments(root_id, kind);

-- 2) 远程存储身份的本机登记；只有明确空的新存储才可初始化
CREATE TABLE session_store_registrations (
    store_id        TEXT PRIMARY KEY,
    engine          TEXT NOT NULL,
    locator_digest  TEXT NOT NULL,
    installation_id TEXT NOT NULL,
    created_at      TEXT NOT NULL
);

-- 3) execution_runs 去掉对 threads 的外键（远程模式下本机不存在 threads 行）
CREATE TABLE execution_runs_local (
    thread_id  TEXT PRIMARY KEY,
    generation INTEGER NOT NULL,
    clean      BOOLEAN NOT NULL
);
INSERT INTO execution_runs_local (thread_id, generation, clean)
    SELECT thread_id, generation, clean FROM execution_runs;
DROP TABLE execution_runs;
ALTER TABLE execution_runs_local RENAME TO execution_runs;
```

| 维度 | v6 → v7 行为 |
| --- | --- |
| 触发 | 任何写打开且 `user_version = 6`（`inspect` 新增 `Version6` 分支）；v2..v5 先走既有升级路径再落到 v7 |
| 已有 dirty 保留 | `execution_runs` 逐行复制，`clean=0` 与 generation 原样保留；迁移后同一 `(thread_id, generation)` 仍触发 `RecoveryRequired` |
| 删除语义保持 | v7 起 `execution_runs` 无外键，删除路径必须在同一事务显式删除被删 thread 的执行行（§4.4）；不靠已失效的级联，不把孤儿 dirty 行留给后续进程 |
| 已有 binding/frozen/历史 | 不动；`threads`/`messages`/`session_bindings` 不重建 |
| 其他业务表 | 不触碰（沿用现有「只要求必需表存在」的判定） |
| 行数校验 | 复制后校验 `execution_runs` 行数不变；不一致 → 迁移失败并整体回滚 |
| 中途失败 | 事务内回滚，`user_version` 保持 6，库仍可被本构建写打开并重试；不出现半迁移状态 |
| 只读打开 | **不迁移，也不按版本号拒绝**：沿用现有只读 shape 兼容规则（`connection.rs::probe_load_meta_shape` 的必需列超集判定），只读路径不读也不写 `user_version`、不建表、不建锁文件；只有必需列缺失（形状不符）才 `SchemaIncompatible`。v6 与 v7 都满足必需列，因此只读都能进 |
| 旧二进制读 v7 | 只读仍按上述 shape 规则放行（现有只读实现不查版本号）；写打开才走版本判定：`found > supported` → `UnsupportedSchemaVersion`，不回退、不降级写 |
| 未来版本 | **写打开**拒绝（`UnsupportedSchemaVersion{found, supported}`），不自动迁移、不猜列形状；只读打开不因版本号本身拒绝，仍按 shape 判定，且绝不降级写 |
| 远程适配 | 远端 schema 独立版本化（C §4）；本机 v7 迁移与远端 schema 无关，两者不共享 DDL |

`execution_runs` 去外键是本矩阵唯一的结构性重建：它是 `threads` 的子表，删除/重建不需要 `PRAGMA foreign_keys = OFF`（现有 registration rebuild 才需要），也不影响 `session_bindings` 的级联语义。

分支说明：`Empty`/`Legacy`（新库或旧库）在既有 DDL 段直接按 v7 形状创建——`execution_runs` 一开始就不带外键，并新建两张 v7 表——不做“复制-删除-改名”；只有 `Version2..Version6` 才走重建路径。`inspect` 需要新增 `Version6` 与 `Version7(=Current)` 两个分支，`needs_registration_rebuild` 保持只覆盖 2..5。

## 6. Binding、legacy 与本机身份

- 保留 canonical locator + 文件对象证据联合登记；独立 clone 不按 remote 合并，linked worktree 规则不变。
- 每次准入一次完整发现；准入内后续 reassert 只查关系与关键对象，不重复 Git。
- SQLite legacy 接纳仍校验保存的绝对 cwd、根身份、既有 execution 记录/frozen；binding 与缺失 frozen 同时生效。数据行为返回权威快照，不让 ACP 处理 bool 胜负。
- Turso 首期缺 binding 不是 legacy。外来 HostRegistration、registry 缺失、目录相同但对象不同都阻止执行。
- 同远程 StoreId 的 locator 别名、URL 规范化和连接路由不改变本机锁域。首次 HostRegistration 登记只可在明确空的新范围初始化；不能对已有数据自动重新认领。
- 首期远程以独立数据库及单宿主写凭证为支持边界；若后续用 namespace，权限隔离需独立证明。应用 WHERE 条件不是授权隔离。
- 本机 SQLite 延续 canonical path 锁域；hardlink/网络共享多机/复制数据库不属于现行保证。测试并说明支持范围，不把远程 StoreId 承诺误套为已支持所有本机别名。

### 6.1 locator → StoreId → 登记 → binding → workspace 证据 → owner（review-2 闭合）

链上每一环是独立事实，缺环不自动补，也不因“同一目录”或“同一 URL”而推定一致。`AccessMode`/`DataCapabilities`/`ExecutionAvailability` 见 A §5.3。

| # | 环节 | 事实持有者 | 只读打开允许 | 写入/执行前置 | 必须拒绝的情形 |
| --- | --- | --- | --- | --- | --- |
| 1 | locator 解析（本机路径 / `env:` / 远程 URL） | D 的解析层 | 只解析、不连接、不读凭证以外的环境 | 解析成功 + 引擎/协议明确 | 未知 scheme、userinfo/带凭证 URL、drive 字母被当 scheme |
| 2 | 远端 schema 兼容性 | 远端 store | **允许读 schema/版本**，无写探测 | 版本已知且被本构建接受 | future schema、形状不符 → 拒绝（不 DDL、不迁移） |
| 3 | `SessionStoreId` | 远端 durable 行（adb 内唯一身份记录） | **允许读**（读不到 = 未初始化，不是“空库可用”） | 存在且与本机登记匹配 | 读取失败不当作“空存储”；不自动创建 |
| 4 | 本机 `session_store_registrations` 登记 | 本机库（§5.3） | **允许读** | 命中 store_id 且 `locator_digest` 与当前 locator 一致 | 无登记 + 远端已有数据 → 只读历史，禁止执行、禁止自动登记；有登记但 digest/来源不匹配 → 拒绝执行 |
| 5 | 首次登记（仅空存储） | 同上 | 不允许写 | 远端可证明为空（无 store_id 行）且本次为写打开 | 已有数据范围不得重新认领/迁移 |
| 6 | binding（durable 数据） | 数据 adapter | 允许读；`BindingState` 区分 Bound/LegacyConfirmed/ExternalOrUnregistered/Missing | 已绑定 + 关系一致 | `Missing`/来源不匹配 → 只读历史，不当作 legacy 自动接纳（SQLite 的 legacy 规则见 §6 首段，远程首期不启用） |
| 7 | workspace 证据（项目/工作区/文件对象/Git） | 本机 registry | 允许读已登记项 | 登记存在、对象身份一致、`validate_session_binding` 通过 | 目录被替换/移动、对象不同、登记缺失 → 拒绝执行 |
| 8 | owner（执行代际 + OS 锁） | `execution_runs` + sidecar 锁 | 不取得，不创建锁文件 | 1–7 全通过且无未决持久化（§4.3） | 他处持有、dirty 未解除、未决持久化、只读打开 → 只读准入 |

只读打开（`AccessMode::ReadOnly`）允许做第 1/2/3/4/6/7 项的**读取**，但不得做任何写：不初始化 schema、不创建 StoreId、不写登记、不建锁文件、不取得 owner、不修复 plugin cache（E §3.2）。「读不到 store_id」与「store 为空可初始化」是两件事，只有写打开 + 明确空 + 首次登记才可初始化。

**跨系统引用不能伪装成外键（review-2 闭合）**：`session_bindings` 的 `project_id`/`workspace_id` 在本地模式下是同一库的外键；远程模式下 binding 是远端 durable 事实，而 project/workspace 登记是本机事实，远端不得建立指向本机表的外键，也不得自行判断目录证据。规则固定为：

- 远端 binding 行保存 `schema_version`、`project_id`、`workspace_id`、`relative_cwd` 这四个**本机登记标识**，远端只做完整性校验（非空、版本接受），不校验登记是否存在；
- 执行准入由本机门面校验第 4/7 环（登记命中 + workspace 证据一致）；登记缺失 → 只读历史，不自动重建；
- 本地模式继续使用原外键与同事务关系，不因远程改本地 schema（§5.3 只动 `execution_runs`）。

## 7. SQLite 数据行为迁移

- 新建/fork/child：分别收口完整快照；frozen/inherited 写失败无需 ACP/Agent 逐步删除数据。
- append：保留批量和事务计数/标题更新；`INSERT OR IGNORE` 不足以证明相同 ID 内容相同，碰撞须区分完全重复与冲突，不能静默跳过错误数据。
- snapshot read：同一读取视图获得 own payload/flags、inherited、binding/frozen，兼顾只读/WAL；metadata/list 不调用 full snapshot。
- flags/compact：持久化归属校验在内部；损坏 projection/MessageId 不默默降为 None/跳过。
- delete/rewind/projection：目标选择、改变及派生缓存维护为一项行为。保留现有未知截止点的无变更语义，避免本次顺便改变用户 rewind 规则。
- metadata：定向字段更新；历史 status/标题并发不整份覆盖。
- 列表：保留 scope/cursor、隐藏/空线程过滤和 legacy 路径关联；不要把 `THREAD_META_COLUMNS` 的内容大小聚合搬到轻量列表。

缓存 epoch、pool、SQLx transaction 不出实现；同一行为内避免占着事务连接再取 pool 导致容量死锁。

## 8. 迁移步骤与范围

- B-01：读取并锁定原 schema/只读行为及测试基线；不打开真实用户数据库做实验。已记录基线：`cargo test -p peri-resources --lib` → exit 0，156 passed / 0 failed（详见总计划 §8 基线表）。
- B-02：建立共享 SQLite 内核与门面，按 A 提供完整数据行为；旧 trait 暂时仅向门面转发。**已完成（2026-09-26）**：共享内核 `SqliteSessionDatabase`、完整数据行为 `SqliteSessionData` 与门面 `SessionResourcesImpl` 均已落；「旧 trait 向门面转发」未做——桥仍直接持有共享句柄，该转发在 E 删除桥时不再需要（不是遗留旁路，见 B-06）。
- B-03：提取本机发现/执行职责，迁统一 guard、三态 `MutationOutcome`、显式 Unknown 与 close/reset 门禁；不在远程 adapter 复制锁代码。**已完成（2026-09-26）**：`LocalExecution` 持有发现/登记/owner/dirty/创建准入/撤销；`MutationGate` 统一「能力/权限 → 未决持久化 → 本 root owner」；`WriteScope::settle` 按 `MutationOutcome` 结清；撤销、认领、删除、排空、关闭各有显式门禁；远程 adapter 不参与锁。
- B-04：新建/fork/child/legacy 快照收口；只读默认路由继续复用现有连接检查。**已完成（数据侧）**：新建/fork/child 一次事务落完整快照，child 用 root 原始 frozen 字节，legacy 接纳保持原语义；只读路由未改动（连接检查仍在 `connection.rs`）。
- B-05：实现 v7 迁移（§5.3：锚点表、登记表、`execution_runs` 去外键）、删除墓碑（§4.4）、创建意图状态机（§5.2）、远程登记链（§6.1），并测试 dirty 保留、迁移回滚、只读不迁移、别名与登记丢失。**已完成（本机侧）**：v7 迁移 + 删除墓碑 + 撤销锚点 + 登记表读取已落并测试（dirty 保留、迁移回滚、只读不迁移、登记歧义）；`creation_intent` 的 `reserved`/`data_saved` 中间态在本机塌缩下不可达（§5.1），远程登记写入属 C。
- B-06：与 E 切换生产调用并删除旧混合实现出口（符号删除 + 全 target 编译，A §7.1）；保留有限测试替身但诚实声明能力，且不得保留静默 no-op。**未开始**（旧 trait 与桥仍在；E 负责切换，属下一阶段）。

现有文件主要为 `peri-resources/src/{context,sessions/mod,sessions/sqlite_store}.rs` 及 `sqlite_store/{connection,schema,workspace,execution,discovery,context,compaction,row_mapping}.rs`；拟新增位置按 A。D 拥有外部 locator/factory 参数修改，涉及 `context.rs` 时顺序接续，不并行覆盖。

## 9. 退出条件

F 中 V-03…12、V-14/16 通过；原 schema/legacy/只读/执行竞争/排空的回归仍有效；v7 迁移在 v6 库上保留全部 dirty 且可回滚；删除后墓碑仍可判定；默认路径没有第二套历史或网络成本。数据与执行接口可分别实现，但所有生产变更仍受同一门面授权。未知写入不能通过普通 dirty reset、删除或 Drop 绕过。

门面侧的当前落点（2026-09-26）：V-03（创建/收敛/诚实结果）、V-10/V-11 的跨进程 owner 与崩溃 dirty、V-12（只读与不支持在副作用前失败）、V-17（三态结清）、V-19（墓碑在级联后仍可判定）已由 `sessions/resources_test.rs`（20 项，含复核轮为 `save_child` 门禁缺口新增的 1 项）与 `tests/session_resources_contract.rs`（6 项）覆盖并通过；V-18 由 `sqlite_store/schema_v7_test.rs`（4 项）覆盖。剩余的是生产切换（E）与远程行为（C）。
