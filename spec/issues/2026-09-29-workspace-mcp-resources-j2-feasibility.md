# J2 可行性深挖：两阶段会话准入与 frozen 同源（B1–B3 + B5）

> 日期：2026-09-29。状态：**取证与设计结论；条件可行；实施须待 W3 并经用户批准**。
> 上游输入：`spec/issues/2026-09-29-workspace-mcp-resources-decisions.md`（W0 取证、X 项已拍板）与 plan `spec/issues/2026-09-29-workspace-mcp-resources-plan.md:385,469`。
> 本轮未修改生产代码与 plan，不 commit；未新增测试文件/函数（理由见 §9.3），全部为静态证据与既有测试设施盘点。
> 三态口径：现状=`file:line` 静态源码；目标/草案=待实施；未验证项显式标注。

**路径约定**：本文 `workspace.rs` 默认指 `peri-acp/src/host/workspace.rs`，`session_lifecycle.rs` 指 `peri-acp/src/host/requests/session_lifecycle.rs`，`execution.rs` / `local.rs` / `session_data.rs` 指 `peri-resources/src/sessions/sqlite_store/` 下同名文件；其余同名文件（`mcp-packages/workspace/src/workspace.rs`、`peri-resources/src/sessions/sqlite_store/workspace.rs`）出现处标注完整路径。

## 1. 结论摘要

**总裁定：J2 条件可行（conditionally feasible）。** 现有存储原语组合足以支撑「先 lease → 读资源 → 后提交 frozen」，不需要分布式事务，也不需要改协议面；但**必须新增一组未发布创建的中间态接口与一条崩溃清理路径**，否则不可行（现有 `NewSession.frozen` 是必填、创建即写全量 frozen，`peri-acp-types/src/session_resources.rs:132-140`）。

条件清单（全部为新增接口/装配调整，均为本地实现，无 wire 变化）：

1. 存储面：新增 `begin_initialization`（写身份/绑定/执行代际，frozen 暂空）+ `commit_frozen`（一次性 CAS）+ `abandon`（复用既有撤销语义）三个原语（§4）。
2. 崩溃恢复：新增半写草稿的检测与清理（判据 `bound && frozen IS NULL`，与 legacy 判据互斥；§5.3）。
3. frozen 同源：冷 load/resume 与 legacy 的装配必须改为消费持久 blob（winner），禁止装配期第二次构建（§6）。
4. 发布顺序：资源读取与 frozen 提交必须发生在 `sessions.insert` 之前；`insert` 仍是唯一对外可见点（§7）。
5. 验证：实施波次内补运行测试（§9）；本轮静态结论不替代实施验证。

不可行的替代路线（已排除）：

- 「把 Mcp 槽位前挪」违反 ARC-MIDDLEWARE-001（decisions §2 与 plan `:441`），不采用。
- 「用临时 pool 预检资源、再重建 pool 发布」会让资源读取与发布使用不同连接代际，同一 digest 无法保证；且 AW3-11 只允许 initialize 前一次输入（`peri-acp/src/host/assemble.rs:125-127,414-425`），不采用。
- 「远端也做分布式事务」超出 v10 已撤销的能力（`peri-resources/src/sessions/remote/session_data.rs:717-726`），不采用。

## 2. 现有原语盘点（现状，静态证据）

| 原语 | 证据 | 可用于 J2 的部分 | 缺口 |
| --- | --- | --- | --- |
| OS sidecar 排他锁（flock，按 thread id 摘要命名） | `peri-resources/src/sessions/sqlite_store/execution.rs:27-33,299-329`；进程退出自动释放 | 跨进程互斥；崩溃后锁自动消失，重启无残留 | 不表达「已提交 frozen」 |
| 执行代际 + clean 位 | `execution.rs:381-405`（取得：`INSERT ... ON CONFLICT` 写 `clean=0`）；`:233-241`（`mark_clean` CAS）；`:331-355`（`reset_dirty` 按 generation CAS） | 崩溃后由 `clean=0` 走既有 dirty 恢复；不引入新锚点 | 与 frozen 提交状态无联合判据（需新增） |
| 本机创建一次提交（数据+代际同事务） | `peri-resources/src/sessions/sqlite_store/local.rs:293-350` | 复用形状做两阶段的第一阶段 | 现在必须同时写 frozen（`session_rows.rs` 的 insert 绑定 `frozen_context`） |
| 未发布创建撤销（补偿顺序：关准入→补偿→放锁） | `execution.rs:255-283`（`abandon_ownership`，补偿失败不动所有权）；`local.rs:118-145`；`session_data.rs:295-339`（删 execution_runs + 子表 + threads 行，拒绝有子会话者） | 直接复用为 `abandon`，语义与幂等性已锚定 | 撤销对象现在含已提交 frozen；两阶段后需确认「已 commit 的草稿不可撤销」（见 §5.2） |
| 在途写入屏障 | `execution.rs:74-79,191-228`（写侧门禁 + `mark_clean` 前等待）；`gate.rs:28-53`（按效果结清，`Unknown` 留给 Drop） | 提交 frozen 与排空的顺序证明 | 无 |
| 有界排空/关闭（含资源 owner drain） | `peri-acp/src/host/workspace.rs:202-253`；重试语义测试 `peri-acp/src/host/requests_workspace_cases_test.rs:408-478` | 资源读取失败后的环境补偿入口 | 无 |
| MCP 生命周期门控（initialize 等待 activation） | `peri-acp/src/host/assemble.rs:535-557`；`workspace.rs:126-133,176-177` | 资源读取必须在 activate 之后；activation 单向不可重来（失败=重建或放弃） | 缺「资源准入完成」的判据（见 §4.4） |
| System MCP 就绪证据（tools 面） | `peri-middlewares/src/mcp/client/readiness.rs:1-99`（只覆盖 transport/lifecycle/协商/live `tools/list`） | 作为资源证据的形状参考 | 现有证据不含 resources/skills（decisions E5 同款缺口） |
| 会话列表投影（`message_count > 0` 才可见） | `peri-resources/src/sessions/sqlite_store/workspace.rs:421-424` | 半写草稿（0 消息）不出现在会话列表，降低外泄面 | 不代表可清理；`load` 仍会命中 |
| legacy 判据（无绑定 + 无代际行） | `local.rs:152-189` | 与「bound 且无 frozen」的半写判据天然互斥 | 无 |

## 3. 「先 lease → 读资源 → 后提交 frozen」逐步可行性

现状时序（new，`peri-acp/src/host/requests/session_lifecycle.rs:499-651`）：`prepare_new` 先构建 frozen → `create_session`（frozen 必填，一次提交并返回 lease）→ 装配（pool 挂起）→ `sessions.insert` → `activate`。

目标时序（本文件建议）：

| 步 | 动作 | 现状是否可表达 | 需要的新东西 |
| --- | --- | --- | --- |
| P0 | 只读准备：配置/严格只读插件/运行环境探测 | 是（`host/prepared.rs:95-138`） | 无 |
| P1 | `resolve_workspace` + 取得 lease（写草稿：thread/binding/execution_runs，frozen 空） | **否**（`create_session` 必须带 frozen） | `begin_initialization`（§4.1） |
| P2 | 装配会话环境（构造 pool/LSP 描述/hooks 组；initialize 挂起） | 是（`assemble_prepared`；`workspace.rs:68-133`） | 准备输入需可无 frozen（现在 `PreparedSessionInputs.frozen` 必填，`host/prepared.rs:49-51`） |
| P3 | `activate` + 等待资源准入（system MCP resources + skills manifest + meta/instructions 读取） | 部分（activation 可；等资源无判据） | 资源准入等待/证据（§4.4） |
| P4 | 构建 frozen 字节（消费 P3 读到的内容） | 是（`build_frozen_data_with_config_and_runtime`，唯一生产构建点 `host/prepared.rs:110`；另见 §6.4 的 fallback） | 无（改调用时点） |
| P5 | `commit_frozen`（一次性；CAS 到草稿行） | **否** | `commit_frozen`（§4.2） |
| P6 | 发布：`sessions.insert` + 命令/通知 | 是（`:623-651,681-695`） | 发布点须移到 P5 之后（顺序调整，非新接口） |
| F | 失败补偿：环境 drain → `abandon`（删草稿+释放 lease） | 是（`abandon_initialization` + `environment.shutdown`） | 幂等编排（§5.1） |

**可行性判定**：P1/P5 是唯一两处硬缺口，均为存储面新增方法（无协议面、无跨进程协调）；P2–P4 是既有能力的重排；F 复用既有补偿原语。故结论为**条件可行**。

### 3.1 load/resume/fork/legacy 的对应关系

- load/resume（bound 会话，正常路径）：不需要两阶段；需要的是 §6 的同源修正（用持久 blob 装配，而不是重建）。
- load/resume（legacy 缺 frozen）：保留「构建一次 + write-once 接纳」语义（`host/requests/legacy_session.rs:74-108`），修正竞争后 winner 的装配输入（§6.3）；**不需要** lease-先于读取（此处读取内容在接纳事务前构建，是既有契约的兼容路径）。
- fork：目标 frozen 来自 source 字节（`host/prepared.rs:85-93,122-125`），无需读资源；需要的是目标发布前的同源一致性（source 冷准入链若重建，仍按 §6.2 修正）。
- legacy 首次接纳：属 §6.3；不引入两阶段。

## 4. 需要新增的接口清单（草案形状）

草案是**行为契约**，不是逐文件施工清单；命名待 W1 冻结。所有新增均为 `peri-acp-types`（契约）+ `peri-resources`（实现）内部面，不改 ACP wire。

### 4.1 `begin_initialization`（存储/门面）

```rust
// peri-acp-types/src/session_resources.rs（草案）
/// 未发布创建：身份/绑定/执行代际已成立，frozen 尚未提交。
#[derive(Clone, Debug)]
pub struct NewSessionDraft {
    pub thread_id: ThreadId,
    pub created_at: String,
    pub meta: NewSessionMeta,
    pub binding: SessionBinding,
}

pub trait SessionResources {
    /// 取得这条身份的执行所有权（草稿态）；同 identity 并发只有一个 owner。
    async fn begin_initialization(
        &self,
        draft: &NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn SessionInitialization>>;
}
```

实现要点（本机组合）：`BEGIN IMMEDIATE` 内 insert thread（`frozen_context = NULL`）+ binding + `execution_runs(gen=1, clean=0)`，commit 后登记 lease——逐字复用 `local.rs:297-350` 的形状，仅把 frozen 值改为 `NULL`，并复用同目录锁与代际。远程组合：`data.save_new_session_draft` + `local.admit_existing`（`local.rs:364-407`），失败语义沿用 `saved_but_not_admitted`（`resources.rs:403-416`）。

**可表达性证据**：`threads.frozen_context` 自 v7 起为可空列（`peri-resources/src/sessions/sqlite_store/schema.rs:196`；v7/v10 测试 schema 同形状），且既有 legacy 路径已经写入过 `NULL`（`session_data.rs:423-430` 的 `COALESCE` 明示「尚可为空」）。因此「草稿行 + 空 frozen」不需要 schema 迁移。

### 4.2 `commit_frozen` 与草稿句柄

```rust
pub trait SessionInitialization: Send + Sync {
    fn thread_id(&self) -> &ThreadId;
    /// 一次性提交 frozen：仅当该 identity 从未提交过；重复调用返回 typed 冲突，不覆盖。
    async fn commit_frozen(&self, frozen: &FrozenSnapshotBytes) -> SessionResourceResult<()>;
    /// 撤销未发布的创建（幂等）；已提交后调用返回 typed 冲突，不删除。
    async fn abandon(self: Arc<Self>) -> SessionResourceResult<()>;
}
```

实现要点：`commit_frozen` 执行 `UPDATE threads SET frozen_context = ?1 WHERE id = ?2 AND frozen_context IS NULL`，要求 `rows_affected == 1`；并在同一 `BEGIN IMMEDIATE` 内校验 `execution_runs(thread_id).clean = 0` 且本进程仍是该 identity 的活 owner（`registered_lease`，`execution.rs:425-432`），否则返回 typed 错误且不写入。`abandon` 复用 `abandon_initialization`（`resources.rs:420-435`）与 `revoke_unpublished_session`（`session_data.rs:295-339`）——**需补一条判据**：已提交 frozen 的草稿不得走删除（把「未发布创建」定义为「frozen IS NULL」即自然满足；`commit` 后 `abandon` 必须拒绝，避免「撤销一个已可发布的会话」）。

幂等性要求：`commit_frozen` 至多生效一次（CAS 天然保证；重复调用返回 typed 冲突）；`abandon` 幂等（`abandon_ownership` 补偿成功后所有权关闭，`execution.rs:264-283`），重复调用返回冲突或成功（须在实现时二选一并写测试）；两者都不做无界重试。

### 4.3 门面行为：`SessionResources` 的扩展面

新增 2 个方法（`begin_initialization` 在 `session_resources.rs:464-527` 的「创建与接纳」段；`SessionInitialization` 句柄新类型）。既有 `create_session`（带 frozen）保留：new 的**非资源路径**（若 W3 后仍存在）与 fork/child 继续使用；两阶段只服务「需要资源读取后才能定稿 frozen」的路径。是否让 fork/child 也迁移为两阶段：**本轮不建议**（fork 复用 source 字节，无资源读取需求；child 继承 parent 字节，`session_resources.rs:154-168`）。

### 4.4 资源准入证据（P3 的等待条件）

现状 readiness 只覆盖 tools（`readiness.rs:1-19,66-99`）。J2 需要「资源读取完成」的显式证据，建议**新增独立证据类型**而不是扩窄现有 `DiscoveryEvidence`：

```rust
/// 本代资源准入证据（草案）：仅由真实成功的 resources/list|read 提交。
pub struct ResourceAdmissionEvidence {
    pub generation: u64,
    /// system 实例中参与冻结的每个来源是否已按 X5/X8 语义取到（或明确为空/不适用）。
    pub system_sources: Vec<SystemSourceOutcome>,
}
```

要点：与 tools 证据同纪律（一次成功后提交、旧代不接受）；`workspace` 为进程内 builtin，读取不经网络，`run_initialize` 后其 `list_resources` 即真实可用（`mcp-packages/workspace/src/workspace.rs:219-249`）；因此 P3 的等待主要是「本代 system pool 已完成 initialize 且 system 来源读取成功」。TUI/print/stdio 三入口的等待预算与失败语义按 X5A：已声明且被选中的 system 来源失败 = fail-closed（拒绝发布），其余降级。

## 5. 失败补偿、崩溃恢复与幂等

### 5.1 两阶段每步失败的补偿矩阵

| 断点 | 已发生副作用 | 补偿动作 | 依据/现状 |
| --- | --- | --- | --- |
| P1 写草稿事务内失败 | 无（事务未提交） | 无 | `BEGIN IMMEDIATE` 失败即未生效（`gate.rs:39-52` 的效果结清纪律） |
| P1 提交成功、lease 登记失败 | 数据已保存 | 返回 `saved_but_not_admitted`；不删数据（诚实口径） | `local.rs:432-436`；`resources.rs:403-416` |
| P2 装配失败（环境未建） | 草稿行 + lease | `abandon`（删草稿 + 释放 lease） | 现有 `assemble_prepared` 失败即 `abandon_initialization`（`session_lifecycle.rs:586-592`），顺序可复用 |
| P3 资源读取失败/超时/取消 | 环境已建（pool initialize 可能已起外部进程） | 先 `environment.shutdown()`（drain + 关闭 pool；允许重试，未确认前不得 mark_clean） | `workspace.rs:202-253`；重试语义测试 `requests_workspace_cases_test.rs:408-478`（失败保留资源直到 cleanup retry） |
| P3 成功、P4 frozen 构建失败 | 环境已建、资源已读 | 同上 shutdown → `abandon` | 同上；构建失败为本地错误，不发布 |
| P5 `commit_frozen` 失败（CAS 冲突/DB 错） | 环境已建 | 补偿前**先重读** `threads.frozen_context` 单条判据：读到非 NULL ⇒ 提交实际生效，转为发布；读到 NULL 且 DB 可用 ⇒ 确证未提交，shutdown → `abandon`；判据不可得 ⇒ 不得删除，保留草稿与 dirty 代际由 `RecoveryRequired` 显式恢复 | 效果结清纪律 `gate.rs:39-52`；`TransactionEffect`（`execution.rs:146-183`）提供「提交前可证未生效 / 提交后不可证」的边界 |
| P6 发布前崩溃（进程死） | 草稿（可能已 commit frozen） | 见 §5.3 恢复扫描 | 新增 |

**顺序不变量**：`abandon` 必须在环境 drain 完成之后（否则资源持有已删除会话的 lease/handle）；`mark_clean` 必须在资源全部停止之后（`execution.rs:191-228` 与 `workspace.rs:210-240` 已同序）。任何「先删行再排空」的实现都是错的，须在测试中锚定。

### 5.2 幂等性与「提交后不可撤销」

- `commit_frozen` 一次性；成功后 `abandon` 拒绝（typed 冲突），因为该会话已满足发布前提，删除会销毁「已提交、未发布」的合法中间态。
- `abandon` 幂等：首次成功（行已删、所有权关闭）；重复调用返回冲突或成功（实现时二选一，测试必须覆盖）。
- 环境关闭可重试（`requests_workspace_cases_test.rs:408-478` 已锚定「未确认排空前保持 Closing、不接受竞争 lease、重试后成功」）。

### 5.3 崩溃恢复：半写草稿与僵尸 lease

**判据（新增，与 legacy 互斥）**：

- 半写草稿 = `session_bindings` 有行（bound）**且** `threads.frozen_context IS NULL`；通常伴随 `execution_runs.clean = 0`。
- legacy = 无 binding 行 **且** 无 `execution_runs` 行（`local.rs:152-189` 的 `legacy_confirmed` 逐条要求），因此两判据不可能同时命中。
- 已提交未发布 = `bound && frozen_context IS NOT NULL && execution_runs.clean = 0`。

**僵尸 lease**：OS flock 随进程退出自动释放（`execution.rs:299-329` 每次重新 `try_lock` 同一 inode），本进程的 `execution_leases` 是弱引用表（`execution.rs:413-417`），崩溃即失效；持久判据只有 `execution_runs.clean = 0`，由 `acquire_execution` 的 `RecoveryRequired`（`execution.rs:387-393`）与显式 `reset_dirty`（`:331-355`）覆盖——**J2 不改变这条链**，只在其中补 frozen 维度。

**恢复动作（两个候选，推荐 A）**：

- A（推荐）**自动清理半写草稿**：在会话门面 open 之后、（按需）首次触达时扫描 `bound && frozen IS NULL` 的 identity：确认本进程无活 owner 且可取得 OS 锁后，复用 `revoke_unpublished_session` 删除（0 消息、不出现在列表，`peri-resources/src/sessions/sqlite_store/workspace.rs:421-424`；未发布创建没有用户可见内容）。理由：半写草稿不携带任何用户数据，删除是唯一无歧义的收敛；不新增 RPC。
- B（保守）**保留并显式报告**：扩展 `peri/session_reset_dirty` 或新增内部清理入口，由用户/宿主显式触发。适用于「未来若允许草稿承载用户可见内容」的演进，但当前无此需求。

清理幂等：判据严格（`frozen IS NULL`），提交成功的会话不会被误删；重复扫描安全。扫描有界（按需单条，不做全表递归；批量策略留 W3 定）。

**已提交未发布**（`frozen NOT NULL && clean=0`）**不得自动删除**：它已有定稿 frozen，语义上是「崩溃前已定稿、未发布」，`load` 可按既有 bound 会话读取（frozen 可解码），只需走既有 dirty 恢复（`RecoveryRequired` → 用户接受风险 → `reset_dirty`）。因此两阶段不引入新的用户可见损坏面。

## 6. frozen 同源性方案（B2）

### 6.1 单一事实源规则（目标）

1. **bound 会话**：持久化 blob 是唯一事实源。任何装配（含会话环境的关闭集派生）只消费该 blob 的解码视图。
2. **未发布阶段**：本次准备构建的字节是唯一事实源，`commit_frozen` 写入后立即升为规则 1。
3. **legacy 缺失**：构建一次 + write-once 接纳；接纳竞争后 winner 是唯一事实源（见 6.3）。
4. 禁止任何消费者在会话生命周期内第二次构建（含「防御性 fallback」，见 6.4）。

### 6.2 冷 load/resume 的现状偏差与修正

现状：普通冷恢复在 `prepare_existing` 内先 `load_frozen_data`（持久 blob，解码为 `FrozenSessionData`，用于 `SessionState.frozen`），但环境装配走 `SessionEnvironment::assemble(cfg, &cwd, id)`（`session_lifecycle.rs:164-175`），而 `assemble` 内部先 `prepare_new`（`workspace.rs:56-60`）——在磁盘/配置当前状态上再构建一份 frozen，并把它用于：
- `builtin_closed` 派生（`workspace.rs:99-104`，喂给 pool 的订阅建立门 `client.rs:290-295`，即外层 git 订阅闸门）；
- `PreparedPlugins`（skill roots / agent dirs，`workspace.rs:120-124`）。

因此「历史 prompt 不变」（有测试：`requests_frozen_cases_test.rs:4-85`）但**环境 policy 可能漂移**（例：新建后把 `WorkspaceMiddleware` 改为 false 再冷恢复，`SectionState.frozen.meta_harness.disabled_middlewares` 与 pool 的实际关闭集不一致——前者来自旧 blob，后者来自新配置）。这是 J2 语义（「冻结前取得、失败补偿、无半提交」）的直接障碍。

**修正方案（最小改动面）**：让装配显式接收 frozen 事实。

```rust
// 草案：装配签名显式化 frozen 来源（命名待 W1 冻结）
pub(crate) async fn assemble_with_frozen(
    host: &AcpServerConfig,
    cwd: &str,
    session_id: &str,
    frozen: &FrozenSessionData,      // 唯一事实源：持久 blob 或本次构建产物
    plugins: &PreparedPlugins,       // 唯一事实源：本次准备/持久登记
) -> Result<Option<Arc<Self>>, AcpError>;
```

`assemble_prepared`（`workspace.rs:68`）改为消费者：其 `inputs.frozen` 由调用方注入或校验；冷 load/resume 传**持久 blob 解码视图**（`load_frozen_data` 已在 `:164` 调用，直接复用），new/legacy 首次构建传本次产物。这样 `prepare_new` 不再被恢复路径间接调用（`SessionEnvironment::assemble` 的 `prepare_new` 调用点，`workspace.rs:59`，在恢复路径上应删除或仅保留给「无会话环境注册」的旧入口）。

回退安全：本修正不改 wire、不改持久格式（`frozen_snapshot.rs:13-45` 不变），只改调用方传入的事实源。

### 6.3 legacy 竞争的 winner 一致性

现状：`prepare_for_restore` 在 `LegacyAbsent` 时生成 `prepared`（含 frozen 候选），随后 `adopt_legacy_session` 以 write-once 提交（`session_data.rs:423-430` 的 `COALESCE`：已有值不变），竞争失败方不会覆盖 winner。但 `prepare_existing` 的装配在 `legacy_prepared` 存在时直接用它（`session_lifecycle.rs:165-171`），**未与 winner 比对**；若本进程被竞争落下（另一进程先提交了不同字节），环境 policy 会与最终 blob 不一致。

**修正方案**：装配输入改为「adopt 后重读的 winner」：

1. `prepare_for_restore` 保持现状（候选构建 + `adopt` 提交）。
2. `prepare_existing` 在 `adopt` 成功后**总是**调用 `load_frozen_data`（已有调用，`session_lifecycle.rs:164`）取 winner，并把 winner 传给 `assemble_with_frozen`（6.2 的签名）；`legacy_prepared` 仅保留其非 frozen 事实（cwd 记录，`legacy_session.rs:99-107`），不再提供 frozen。
3. 若 winner 与本次候选不同（竞争证据），以 winner 为准；候选被丢弃且不写入（`COALESCE` 已保证）。
4. 测试锚点（新增，实施时）：并发两个 store 句柄同时接纳同一 legacy（一个先成功），断言双方最终装配的 `disabled_middlewares`/订阅闸门都等于 winner blob 的投影。

### 6.4 第三构建入口（fallback）的处置

`peri-acp/src/host/prompt.rs:477-485` 存在防御性 `FrozenFallbackBuilder`（注释自称「生产不可达」；`turn.frozen=None` 时回落 `build_frozen_data`）。它是同源规则的第三入口。**本轮只登记**：W3 实施要么证明不可达（加断言/测试），要么改为 fail-closed（不构建，报内部错误），不得保留「静默用当前状态重冻」的路径。`build_frozen_data_with_config_and_runtime` 的生产构建点只有 `prepared.rs:110` 一处（grep 证据：其余调用均为测试），这是同源收口可以做到的实证基础。

## 7. 「无准备期执行」的范围与发布顺序（B3）

### 7.1 范围定义（建议作为 W3 验收口径）

| 类别 | 允许（发布前） | 不允许 |
| --- | --- | --- |
| 配置/插件 | 配置解析；插件严格只读发现（不读写插件目录文件，`peri-middlewares/src/plugin/loader.rs:607,804-882`） | 插件安装/卸载/清理（`assemble.rs:619-635` 的 `PluginCleanup` 只在顶层装配 spawn） |
| 进程 | 平台探测子进程（`sw_vers`，`peri-acp/src/prompt/mod.rs:90-95,470-475`）；MCP 外部 server 的 transport 进程（它是资源来源，P3 必须启动） | 工具执行（Bash/文件写等）、LSP 语言服务器进程启动、git 采样（采样只在成功工具调用后触发，`mcp-packages/workspace/src/workspace.rs:126-143,274-293`） |
| Agent/模型 | 无 | 任何模型请求；任何 hook 执行；cron tick 驱动 |
| 存储 | 身份/绑定登记（`resolve_workspace`，`peri-resources/src/sessions/sqlite_store/workspace.rs:61-93`）与草稿行 | 已提交 frozen 的对外可见会话 |

**口径关键**：`workspace` 资源读取发生在 MCP `initialize` 之后，因此「准备期」若定义为「P0–P2」则零 MCP 进程；若定义为「P0–P5（发布前）」则包含 builtin/外部 MCP 启动与资源读取，但**不含工具/模型/hook**。两种口径在验收断言上不同，必须二选一并写死（建议后者，因为 J2 的价值正是「发布前完成内容准入」）。现有测试可支撑前者断言（`prepared_test.rs:188-202`：准备不写会话状态）。

### 7.2 发布顺序与原子性边界

目标顺序：

```
P1 草稿+lease → P2 环境装配（pool 挂起） → P3 activate + 资源准入
→ P4 构建 frozen → P5 commit_frozen → P6 sessions.insert（唯一对外可见点）→ after_new_response
```

边界论证：

- **P6 之前不可执行**：`require_owner` 只检查 `sessions` map 中的 `SessionState`（`workspace.rs:321-328`），而 `sessions.insert` 是唯一登记点（`session_lifecycle.rs:623-648,1169-1197`）；prompt 入口同样先查 map（`prompt_dispatch.rs:60-80,140-160` 的 `require_owner` 调用点）。因此「不在 map」即不可提示、不可执行。
- **P6 之后的原子性**：现状 new 是 `insert` 然后 `activate`（`:648-652`），load 是 `activate` 然后 `insert`（`:226-229`），两者之间无 `await`，同一任务内不可分割。J2 后 `activate` 提前到 P3，`insert` 成为唯一发布动作；「已 activate 未 insert」的新窗口里资源已就绪但会话不可见，取消/失败走 §5.1 的 F 路径，无半提交。
- **P3 的 activate 是不可逆点**：`CancellationToken` 单向（`workspace.rs:176-177`），一旦 activate，initialize 只能前进或关闭。因此「资源准入失败后的重试」必须重建环境（新 pool），不能复用已 activate 的 pool；实现时 P2→P5 的失败重试策略须为「重建或放弃」，不得「部分重试」。
- **外部可见性**：草稿行不进列表（`message_count > 0` 过滤），cold load 命中半写草稿时按 typed 错误 fail-closed（`session_lifecycle.rs:62-65` 的 "Bound session has no frozen snapshot"）——这是既有行为的自然结果，不是新泄漏；清理见 §5.3。

## 8. B5：rmcp custom-method 与 resources/templates 的 wire 结论

### 8.1 静态结论（现状）

| 面 | SDK 事实 | 本库事实 | 现状行为 |
| --- | --- | --- | --- |
| custom method | `ServerHandler::on_custom_request` 有默认实现返回 `METHOD_NOT_FOUND`（`/Users/konghayao/.cargo/registry/src/rsproxy.cn-e3de039b2554c837/rmcp-3.1.4/src/handler/server.rs:503-515`）；dispatcher 把 `ClientRequest::CustomRequest` 分派到该 hook（同文件 `:223-226`） | `BuiltinServerHandler`（`peri-middlewares/src/mcp/builtin/dispatch.rs:43-143`）只覆写 info/tools/resources/订阅，**未**覆写 `on_custom_request` | `skills/list`、`skills/get` 经 builtin `workspace` 链路返回 `-32601` |
| templates | 默认实现返回空 `ListResourceTemplatesResult`（`server.rs:387-395`） | dispatch 同样未覆写 | `resources/templates/list` 返回空表（不是错误） |
| 标准 resources | `list_resources`/`read_resource` 已由 workspace 实现（`mcp-packages/workspace/src/workspace.rs:219-249`） | dispatch 已转发（`dispatch.rs:100-122`） | 可用（git ref） |

结论：**SDK 承载足够，缺口全在 dispatch 与 provider 侧**；W1 需要覆写 `on_custom_request`（skills 两方法）并按需补 `list_resource_templates`（若 X5A 选择声明模板）。这与 plan `:470`（W1 wire 验收）一致。

### 8.2 可执行验证方法（本轮未执行，W1 用）

既有设施：`peri-middlewares/src/mcp/builtin_subscription_workspace_wire_test.rs:35-86` 提供 `connect_workspace(cwd)`——真实 `WorkspaceMcpServer` + 生产链路 + 生产 client 握手，返回 `McpServiceWrapper`/`Peer<RoleClient>`。

验证步骤（在既有测试文件新增测试函数即可，无需改生产代码）：

1. `let link = connect_workspace(cwd).await;` → `peer().send_request(CustomRequest::new("skills/list", params))`，断言当前返回 `-32601`（W1 改造后改为断言正常响应）。
2. `peer().list_resource_templates(Default::default())` → 断言当前空表；改造后断言包含 `skill://` 模板（若声明）。
3. 回归：`peer().list_resources`、`read_resource(workspace://git/ref)`、订阅面断言不变（既有用例继续锁）。

本轮未写该测试：新增函数须改既有测试文件（虽在授权内），而 B5 对 J2 主路径只是间接依赖（资源读取失败语义），且需要整包构建；故只给方法。**说明：未新增测试文件/函数，无保留或删除问题**（§9.3）。

## 9. 验证清单与阻塞判定

### 9.1 实施期验证清单（可执行）

| 验证 | 命令（示例，实施后按实际测试名核对） | 断言 |
| --- | --- | --- |
| 存储两阶段 | `cargo test -p peri-resources --lib -- sessions::` | begin 后：草稿行存在、frozen NULL、`clean=0`；第二个宿主 `acquire_execution` 返回 busy；commit 后 frozen 可读；重复 commit 冲突 |
| 崩溃矩阵 | 同上（新增用例） | begin→drop（不 commit）→重开：列表无该会话；cold load typed 错误；清理后行消失；`commit` 后 drop：cold load 可读 + `RecoveryRequired` |
| 补偿顺序 | `cargo test -p peri-acp --lib -- host::requests_workspace_cases`（既有）+ 新增 | 失败装配保持 Closing/owner/资源，未确认排空前不接受竞争 lease；abandon 仅在 drain 之后 |
| frozen 同源 | `cargo test -p peri-acp --lib -- host::requests_frozen_cases`（既有）+ 新增 | 冷 load 的 `builtin_closed`/订阅闸门 == 持久 blob 投影（改动 config 后仍成立） |
| legacy 竞争 | `cargo test -p peri-resources --lib`（新增并发用例） | 双方装配均按 winner；候选不覆盖 |
| wire 基线 | `cargo test -p peri-middlewares --lib -- mcp::builtin_subscription_workspace_wire`（既有）+ §8.2 新增 | 现状 `-32601`/空表；改造后功能断言 |

### 9.2 阻塞判定更新

| 项 | 原状态 | 本轮结论 | 依据 |
| --- | --- | --- | --- |
| B0 | 未拍板 | **已关闭**：七项 X 按推荐 A 记录（decisions §3） | 用户 2026-09-29 拍板 |
| B1（lease/事务） | 未证明 | **降级为条件可行并关闭为设计阻塞**：缺口=2 个新原语（`begin_initialization`/`commit_frozen`），原语形状与可行性已证明；剩余为实施期运行验证 | §3–§5；`local.rs:297-350`、`execution.rs:255-283,331-355`、`schema.rs:196` |
| B2（frozen 同源） | 未证明 | **降级为条件可行并关闭为设计阻塞**：缺口=装配消费持久 blob/winner + 删第二构建路径（§6.2/6.3/6.4）；现有测试已覆盖 prompt 面，policy 面待补 | §6；`session_lifecycle.rs:164-175`、`workspace.rs:56-60,99-104` |
| B3（无执行范围/发布顺序） | 未证明 | **降级为条件可行并关闭为设计阻塞**：范围表与发布点已定义（§7）；需在 W3 写死口径并加断言 | §7；`workspace.rs:321-328`、`session_lifecycle.rs:226-229,648-652` |
| B4（规范第 9/10/7.3 章） | 开放 | 未变 | 仍需同版本章节 digest |
| B5（wire） | 开放 | **部分收敛**：SDK 承载与 dispatch 缺口已静态定位（§8.1）；运行验证方法已给（§8.2），实施期执行 | §8 |
| B6（隔离/批准/缓存运行证据） | 开放 | 未变 | 需 W1/W2 运行测试 |
| B7（drain/采样 task 生命周期） | 开放 | 未变；J2 补偿依赖其行为（§5.1 的 P3 行） | `workspace.rs:138-184` 的采样 `tokio::spawn` 仍未证明被 owner join |
| B8（格式/回退） | 开放 | 未变 | 需版本化回归 |

**W3 可放行条件**：B1–B3 的设计缺口已闭合（本文件）；实施前还需（a）用户确认 §4 接口草案与 §7.1 口径，（b）B5 wire 基线测试落地，（c）§9.1 的验证清单进入 W3 验收。plan 的 W0 总门（`plan:469`「无法证明则阻塞后续」）在本轮后对 B1–B3 由「无法证明」变为「已证明可行、待实施验证」。

### 9.3 本轮验证手段说明

- 未新增测试文件或测试函数：B5 的验证需要修改既有测试文件或注册模块（跨出「仅新增独立文件」的操作面），且 J2 的最终证明必须在实施后以新增接口的测试完成（本轮接口尚不存在，写测试只能测旧行为）。**无保留/删除决定**。
- 未运行构建/测试/服务；所有结论为静态证据。`/Users/konghayao/.cargo/.../rmcp-3.1.4` 的 SDK 读取为只读。
- 未修改生产代码、未改 plan、未 commit；本轮唯一写入为本文件与 decisions 文档的状态回填。

## 10. 与既有裁决的关系

- J2 语义 = plan `:385`（两阶段 + lease + 原子发布 + 失败补偿）；本文件把它落成接口与恢复规则，不改变语义。
- 不违反 AW3-11（initialize 前一次输入）：P2 仍是一次输入，P3 只是解除 activation 等待（`assemble.rs:125-127,414-425,535-557`）。
- 不违反 ARC-FROZEN-001（历史精确复用）与 ARC-WORKSPACE-001（lease 先于执行）：本文件强化两者（cold load 不再重建 policy）。
- 与 X5A/X8A 一致：system 已声明来源失败 fail-closed（P5 不提交）；MetaHarness 覆盖不可得仍可退内置（X8A），但按 X5A 的交叉优先关系，同一 workspace 其他必选来源失败仍拒绝。
- 关闭语义：半写草稿清理与「关闭 workspace ⇒ 本地技能不可用」不冲突（草稿清理是身份收敛，不是能力回退）。
