# 会话资源门面拆分与远程存储 Adapter（首期 Turso Cloud）

**状态**：Verify — 行为门面、SQLite/Turso adapter、部署配置与消费侧迁移已落地；Fable 对提交 `4ba2eefe` 的独立复验确认上一轮具体 P1/P2 反例均已修复，本机及**已登记存储范围**的云端行为通过。真实空库首次创建、双真实 store 同 root 端到端和大历史上限仍缺证据，issue 保持打开；不得将限定范围通过表述为无条件生产可用。
**更新日期**：2026-09-27（支持边界按用户裁决更新：登记/准入撤销，见下）

## 最新正向验证（2026-09-27，登记/准入撤销后）

| 路径 | 证据 |
| --- | --- |
| 云端全生命周期（真实 Turso 测试库） | `sessions::remote::cloud_deployment_tests::cloud_deployment_entry_point_full_lifecycle` PASS：部署入口建会话 → 追加/排空 → compact → fork（复用 source message id 被拒绝、重映射后落库）→ child → 标题 A→B→A → 关闭；**新进程**冷恢复 → 未决收敛 → 解除 ordinary dirty → rewind → 删树；只读两档（`fresh` 本机无执行库按 `NotFound` 拒绝且零文件、`registered` 读到远端历史并类型化拒绝写入）。 |
| 产品入口往返（真实二进制） | `peri --session-store 'env:TURSO_URL' --session-store-token-env TURSO_TOEKN acp` 的 `session/new` 建会话；临时 HOME 的本机库（v10）只有 `execution_runs` 一行，`threads` / `session_bindings` 全空、无任何远程痕迹表；同 HOME 用 `peri … meta session <id> --json` 读回该会话元数据 ⇒ 数据在远端、本机只留执行事实。 |
| 本机 schema 回退 | 既有本机库（v9，3.1 GB）含 v7..v9 写下的 5 张远程痕迹表，列形状与 `DROPPED_LOCAL_TABLES` 逐列一致（`require_columns` 通过）：下次写打开会整体删除这 5 张表并保留 `execution_runs` 行。 |

同轮门禁：`cargo check --workspace --all-targets`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --check` 均干净；`cargo test --workspace --lib` 全绿（`peri-resources` 325 passed / 22 ignored）。唯一例外是 `peri-tui` 的 `kit::acp_bridge` 两例**既有**不稳定性（同进程内多个用例改写全局 atom，与本次改动无关：同一台机器上两个不同构建的测试二进制各跑 3 次、各命中 1 次失败；该测试文件未被本次改动触碰）。

## 最新独立验收（Fable，提交 `4ba2eefe`）

下文各阶段记录是当时状态，不代表当前仍未实现。最新结论与剩余边界以本节为准。

| 原问题 | 独立复验结果 |
| --- | --- |
| 跨 StoreId 错结清 | PASS：B 的恢复不改 A 的 pending、执行代际、墓碑及旧 raw dirty，不自动登记 |
| 首次初始化竞争 | 判定层 PASS：本机真 SQL + 生产判定只有胜者可首登，Unknown 不发创建事实；真实空 Turso 上 `Created` **未观测** |
| child 父关系不一致 | PASS：矛盾快照拒绝且无行；合法 child 仍属原 root，不能获取独立 owner |
| close 失败重试 | PASS：连续未决均拒绝，Closing 可恢复；无 live lease 的 durable pending 仍阻止关闭，结清后成功；业务门面无全局关闭权 |
| 显式本机路径 / 缺凭证 | PASS：`env:` 字面文件名不再解引用，PathBuf 保真；缺凭证为 NotConfigured，零 registry I/O |
| 取消读后同实例连接 | 云 PASS：取消后同实例下一读为正确 `NotFound`，不是传输 `Unavailable`；只读临时 HOME 零文件 |
| 收据保留 | PASS：清理仅删除本轮合成会话/消息，新连接复核收据保留；不删除旧 run 的封闭证据 |
| 同 root 跨 store / 旧 dirty | 本机判定及单真实库反例 PASS；两个真实已登记 store 并发端到端 **未验证** |
| frozen 同源 | PASS：7 项 prepared 测试；保存/live 使用同一准备输入，准备后修改外部文件不会重读 |

本轮实际命中：`peri-resources --lib` 355 passed / 28 ignored，契约目标 6 passed；TUI bin 的 session_store 与 cli_meta 各 8 passed；ACP/Agent/Controller/Middlewares 编译检查和快照 fmt 通过。云端独立复验包含已登记部署生命周期、新进程冷恢复、两种真实强杀断点与只读取消恢复；未将 ignored 子入口的静默返回算作云证据。

支持边界（2026-09-27 用户裁决后更新）：**显式登记/接管入口与启动探测已被用户裁决撤销——现行语义是「配置即用」**：配置里指到哪个会话存储就直接用哪个，不要求先登记，也不再有本机登记、准入裁决、跨安装来源判定，因此也不再有会话操作前的主动探测与登记风险确认。撤销的落地物：v10 迁移整体删除 5 张本机远程痕迹表（`session_store_registrations` 等，先校验表形状、不符即拒绝升级）、`remote/{registration,local_execution}.rs`、`local_port.rs` 的接纳/登记方法、`session_resources.rs` 的 `ExecutionAvailability::NoLocalRegistration` 变体与全部引用、`peri-acp-types::workspace` 的 `StoreAdmission` / `RegisterStoreRequest` / `StoreRegistrationOutcome`、`PeriCaps::session_store_registration_v1`、两条 RPC（`peri/session_store_status` / `peri/session_register_store`）与 wire 错误 `storeNotRegisteredV1` / `storeRegisteredFromDifferentOriginV1`、TUI 客户端探测（`acp_client/client/store_registration.rs`、`RiskPrompt::StoreRegistration`）与 `store-registration-*` 文案键。**新目标**：远端库 = 本地库的同一种模式，**两个存储模式必须一致**——schema/SQL 的统一是另一段工作，本节只记录目标与边界，不代表已完成（见下方「统一进行中」）。本节随后保留 2026-09-27 撤销前的支持边界原文，仅作历史；其结论已被取代：

> **（已取代，保留为历史）** 显式登记/接管入口已落地，用户不再只有「由本安装初始化新库」一条路。两条经 `peri.sessionStoreRegistrationV1` 门控的 RPC（`peri/session_store_status`、`peri/session_register_store`）提供状态查询与登记；TUI 在会话操作前主动探测，需要时弹风险确认，用户显式接受后才登记。**未登记仍不自动认领**：已有数据而本机无登记时历史可读、执行拒绝，来源不一致拒绝且不覆盖既有登记；未协商能力、确认未呈现（`RiskChoice::NotShown`）、只读打开（`canRegister=false`）与用户取消都不得登记，且都留下用户可见结论。真实二进制 + 真实测试库已复现并修复三类静默路径：风险确认弹窗在「一帧装不下」时替用户按取消结清、弹窗位置被占用时静默跳过、启动期会话创建失败只写进程日志；修复后同一确认在窗口变化后仍成立（装不下的帧不再结清、未完整渲染的接受降级为 `NotShown`），登记成功、建会话、极短 prompt 与远端历史读回在真实 TUI 上成立，另以真实二进制 ACP stdio 走通「拒绝 → 登记 → 接纳 → 建会话 → 读回」。真实首登 `Created`、双真实 store E2E、大历史读写上限（P6）继续待证；已有登记夹具不能证明首次准入；未重新批量执行所有云实验或完整 workspace 套件，不将局部通过扩展到未测场景。

**统一已落地（远端库 = 本地库，两个存储模式一致）**：远端 adapter 现在**直接说本机形状的 SQL**——`threads` / `messages` / `session_bindings` / `projects` / `workspaces` 同名同列同语义，逐条建表/建索引清单、显式删除语句与 `messages.role` 派生都取自新的 `sessions/canonical.rs`（两种执行器共用的那一份 schema），两个 adapter 之间不再有表名/列名映射层。删掉的映射层：`peri_sessions`（meta 列 + 四个扁平绑定列）、`peri_session_messages`、远端独有的 `ordinal` 列与它的索引，以及单条 18 参数的会话插入（拆成 canonical 的 `threads` 行 + `session_bindings` 行 + child 的继承区写入，与本机同形）。

| 统一方案要回答的问题 | 落点 |
| --- | --- |
| 哪些表是 canonical（远端也要有） | `threads` / `messages` / `session_bindings` / `projects` / `workspaces`（`canonical::CREATE_TABLES`，`CREATE_INDEXES` 四条索引） |
| 哪些是本机执行事实（远端不建） | `execution_runs`（执行代际；远端没有执行面） |
| 本机独有的辅助业务表 | `thread_goals` / `extension_state` 这类**不是本模块建的**同库业务表：本模块只校验自己那几张表的形状，升级时原样保留、不进 canonical schema、远端不建 |
| 远端独有 | 执行器机制表：`peri_op_ledger`（幂等资格账本）、`peri_store_meta`（版本标记）——它们不是会话 schema，本机不建 |
| 版本标记放哪 | `peri_store_meta.schema_version` **就是**本机 `CURRENT_SCHEMA_VERSION`（同一个常量，不再是各写一份的 1）；远端写不了 `PRAGMA user_version`（服务端拒绝，§9.8 探测项 5b），载体差异保留 |
| 不一致怎么办 | fail-closed，三处都拒绝且不猜测：契约不是 `peri.session.store/v2`、版本高于本构建、或「有 `peri_sessions`/`peri_session_messages` 却没有本构建身份」 |
| 旧形状的库 | **拒绝，不迁移**：持有 v1 契约的库在读身份时被 `matches_build` 挡下；没有身份但有旧会话表的库在写打开初始化前被旧形状探测挡下（`Unsupported`）。新库没有历史数据，迁移路径没有被需求，也不覆盖使用者看不见的数据 |
| 重复打开 | DDL 全部 `IF NOT EXISTS`，且**一条语句一个 spec**：远端执行器的语句单元就是一条语句，多句拼一个请求只会执行第一条（本轮实测踩到过，见下） |
| 并发初始化 | 语义不变：身份竞争仍是元数据行的唯一键竞争，`Created` 只在「本事务插入并提交」时产生 |
| 部分建表失败 | DDL 在托管批里 → 整批回滚、零残留；下一次写打开按同一份清单补齐 |

执行器差异（同一份形状，差别只剩这些，且每条都有实测依据）：

| 差异 | 本机 SQLite | 远端 over HTTP |
| --- | --- | --- |
| 版本载体 | `PRAGMA user_version`（迁移收尾写入） | `peri_store_meta` 单行 |
| 幂等与资格 | 本地事务 + 主键冲突 | `peri_op_ledger` 资格行先于效果，同一托管批 |
| 父行检查 | DDL 的外键声明被强制执行（读写同连接打开 `foreign_keys`） | 写打开时把 `PRAGMA foreign_keys` 归位为 OFF：`projects`/`workspaces` 的**行**在远端没有来源（workspace 证据是本机事实），外键无从满足 |
| 删除 | 显式先删子行（`REFERENCES ... ON DELETE CASCADE` 保留但已退化为安全网） | 同一份显式删除语句（远端没有级联，也没有 `foreign_key_check` 等价物） |

真云验证（新库，`--ignored --nocapture --test-threads=1`）：`cloud_deployment_entry_point_full_lifecycle`（建会话 → 追加/排空 → compact → fork → child → 标题 A→B→A → 关闭 → 冷恢复 → 未决收敛 → rewind → 删树 → 只读两档）与 `cloud_session_*` / `cloud_history_*` / `cloud_identity_*`（2 例）/ `cloud_lifecycle_*`（2 例）/ `cloud_limit_*`（4 例）/ `cloud_mutation_*`（5 例）/ `cloud_recovery_*` / `cloud_tests::*`（3 例）全绿；`cloud_store_shape_snapshot` 盘点远端表集合 = `threads,messages,session_bindings,projects,workspaces,peri_store_meta,peri_op_ledger`（表名与形状即本机那一份），`schema_version=10`、`contract=peri.session.store/v2`。踩到并修掉的真缺陷：**DDL 一度被拼成一个多语句 spec**，远端只执行第一条 → 会话表根本没建，所有真云用例在清理阶段转红；改为逐条下发后全绿。

本轮未做/未验证（登记在案，不当作已解决）：`projects` / `workspaces` 在远端是**空表**——本机 workspace 证据（locator/root/对象身份）没有进入 `SessionDataPort` 输入的通道，要让远端也持有 workspace 记录需要把该事实带进端口输入（契约变更），本段不做；服务端 `PRAGMA foreign_keys` 是**跨连接共享的可变状态**（实测曾被先前的探测留在 1，导致绑定写入撞外键），生产路径因此每次写打开归位，但别的写者若在会话写入期间打开它，写入仍可能失败；并发初始化竞争与旧形状库的拒绝路径只有判定层证据（离线 + 真云单条路径），没有多进程竞争的真云实验；一次性探测装置 `cloud_transport_probe_test.rs` 已删除（其结论沉淀见 §9.28），它的探测表前缀与本次改动无关。

另：撤销只针对登记与准入，与登记无关的修复照旧保留——dirty 恢复的三态风险选择（`RiskChoice::{Accepted,Declined,NotShown}` 与完整渲染判定）仍是 TUI 的现行行为，启动期会话创建失败的用户可见通知（`session-creation-failed`）也仍在。

收据空间成本是刻意保留的恢复证据：本轮 deployment 报 `retained_receipts=11`，强杀演练报 `retained_receipts=6`；旧 Fable foreign pending 封闭记录按清理器构造保留，未独立直接读取确认。复现程序和测试 HOME 均在系统临时目录；凭证只在授权测试进程内解析，未回显其值。

修复提交：`f9fdab61`（StoreId 隔离与收据）、`2dcb9b14`（初始化/child 准入）、`b47e98f5`（关闭权与连接生命周期）、`4ba2eefe`（路径与单次准备输入）。独立 verdict 为本机 PASS、已登记云支持范围 PASS；未验证子项不计通过，**保持 Verify，不关闭本 issue**。

**目标**：完成整体会话资源门面的职责拆分，通过 adapter 接入可替换的数据存储；保留本机 SQLite 默认路径，首期以 Turso Cloud 为远程对象，验证完整会话链路、失败恢复和运行成本。不是只增加一个远程类，或在混合职责的 `ThreadStore` 外再套一层转发。

**范围**：会话资源门面、数据与本机执行端口、实例化与定位配置、消费侧迁移、SQLite / Turso Cloud adapter、契约测试及显式云端实验。不改变消息内容格式、compact 算法，不重设计前端展示；仅做接入能力和错误表达所需的调用方调整。

**关联**：[会话、项目与 Worktree 身份](../../docs/design/session-workspace-identity.md)、[架构契约](../../docs/standards/architecture-contracts.md)、[测试规范](../../docs/standards/testing.md)、[资源代码索引](../../docs/code-index/peri-resources.md)。现行身份设计限定在本机同一存储；本 issue 扩展持久化位置，不自动扩展跨主机执行权。下文为待实现目标，不表示现行契约已经改变。

## 1. 背景与迁移前现状

本节描述提出需求时的代码状态；当前实现与独立验收见文首。

会话存储最初只有本机 SQLite 一种生产实现。契约与实现已分离，主要消费侧通过 `Arc<dyn ThreadStore>` 注入；障碍不在于缺少 trait，而在于数据存取与本机执行责任混合，且打开入口绑定具体后端。

关键事实源：

- `peri-acp-types/src/store.rs::ThreadStore` 同时承载历史数据、工作区发现、binding、执行 lease 与 dirty reset；部分默认方法返回 no-op、空数据或缺失值，无法区分「不支持」与「确实不存在」。
- `peri-resources/src/context.rs::Resources::open_with` 接收 SQLite 路径；`sessions/mod.rs::open_thread_store_read_only` 是另一条打开入口。TUI、print、ACP stdio 和 `peri meta session` 都需纳入定位与装配迁移。
- `sessions/sqlite_store/{workspace,execution,compaction}.rs` 承载 binding/frozen 原子接纳、根 owner 写入授权、dirty 代际及 compact 事务，不能拆接口时丢失这些契约。
- `peri-agent/src/session/transcript{.rs,/persistence.rs}` 已有有序 writer、flush barrier 和 compact 提交不确定状态；远程接入应延续，而不是另建一份可漂移的历史。
- `FilesystemThreadStore` 是测试用途，未完整持久化 compact flags，不支持原子 compact lifecycle；不能用它证明远程方案成立。

## 2. 已明确目标与首期边界

1. **整体拆分作为本次完成条件。** 消费侧统一依赖稳定的会话资源门面；内部将数据存取与本机执行职责分离，通过明确端口组合。允许一次性迁移契约和调用点，迁移完成后切换 adapter 不再改业务调用点，不出现按 SQLite/Turso 类型分支的业务逻辑。
2. **两种真实 adapter。** 本机 SQLite 与 Turso Cloud 实现同一数据契约。首期验收不再要求数据库引擎「非 SQLite」，而是要求独立远程 adapter 与真实 Turso Cloud 读写成立；只在本机运行兼容引擎不算云端验收。
3. **首期为本机执行、远程权威持久化。** 执行环境、文件工具与进程仍在本机；远程不是备份副本，也不是远程执行宿主。不提供多机接管、跨机并发续写、自动改绑、跨工作区续作、离线双写或自动同步合并。
4. **单宿主范围可验证。** 明确远程存储实例身份、写入命名空间或授权隔离及本机执行登记的对应关系。相同存储的不同 locator 别名不能产生独立本机锁域；外来会话或丢失本机登记不得被当作 legacy 自动接纳。不用本机 OS 锁声称具有跨主机互斥，也不以 last-writer-wins 处理历史冲突。
5. **默认本机行为不变。** 默认仍是 `~/.peri/threads/threads.db`，保留现有数据、schema 升级、只读降级与生命周期语义。拆职责不强迫 SQLite 改为两库，也不增加默认路径的网络依赖。不得静默迁移或上传现有会话。

## 3. 目标职责与装配

```text
Agent / ACP / Controller / TUI 既有合法资源访问入口
                         │
                 稳定的会话资源门面
                         │
               ┌─────────┴─────────┐
               │                   │
           会话数据端口         本机执行端口
               │                   │
       SQLite / Turso adapter   发现与验证、根 owner、
                                OS lease、dirty 与写入排空
```

- **门面**：收口数据访问与执行授权的组合、能力判定、后端无关的错误及持久化完成边界。不是让调用方手工组合两面，也不是迁入 Agent 循环、ACP 协议生命周期或 TUI 状态；TUI 交互仍遵循现有 ACP 路径。
- **数据端口**：元数据、canonical payload（含可信 reminder）、消息顺序、flags、frozen、继承快照、轻量列表及分页、原子 compact、删除/rewind，以及必要的持久化身份记录。不得要求 adapter 运行 Git、检查本机目录或管理执行进程。
- **本机执行端口**：工作区发现和证据复核、绑定准入、根/子会话归属、独占所有权、dirty 代际恢复、写入准入关闭与排空证据。数据 adapter 不成为绕过 owner 的公开写入口；子 Agent、后台 writer、compact、rewind 和删除均受同一授权约束。
- **装配入口**：沿 `peri-acp-types` 契约、`peri-resources` 实现与 Resources 实例化入口演进。普通打开和只读打开共用后端选择；业务侧不泄漏连接、pool、SDK 或具体 adapter 类型。不为首期预建插件框架，也不以新增 crate 作为拆分是否完成的判断标准。
- **物理存储**：职责拆分不等于分库存放。SQLite adapter 可保留原 pool、schema 和事务；需要原子提交的持久化事实优先留在同一事务域。

具体类型名和方法集合在实施设计中确定，但完成时不得保留仍承担混合职责的旧生产旁路；必要兼容入口只能归一转入新门面。

### 3.1 行为优先，机制内聚

**门面与 adapter 实现的数据端口都以会话行为定义，不以数据库操作定义。** 适配面表达要保存/读取的会话事实、业务前提、可观察结果与失败语义；底层如何使用事务、条件更新、锁或重试实现这些保证，不进入接口。

- **尽可能纯化行为。** 历史裁剪、fork 的消息身份与 flags 映射、compact 变更的合法性判断等确定性逻辑，与 I/O 和后端选择分离；由领域侧根据显式输入计算结果，adapter 负责可靠存取，不重新解释 compact 算法或会话执行策略。不为此改算法或引入通用计划执行框架。
- **接口按完整行为设计。** 例如保存新会话、接纳旧会话、追加历史、保存派生会话快照、应用压缩结果、回退历史。需要共同成立的事实以有领域含义的输入一次交付，调用方不拼装多次 CRUD 来维持一致性；具体方法按真实调用需求收敛，不照搬这份示例清单。
- **不暴露存储机制。** 门面和数据端口不提供 `begin/commit/rollback`、事务句柄/闭包、连接、隔离级别、SQL batch、CAS 参数或底层重试令牌；也不引入通用 `UnitOfWork`、事务 DSL 或按后端选择的事务开关。
- **保证可见，机制不可见。** 「压缩结果全部生效或不生效」「冻结快照不被覆盖」「成功确认后可恢复」「顺序稳定」是行为后置条件，必须可测试；事务、CAS、去重、补偿与网络重试是实现手段，封装在 adapter 或资源模块私有协调实现中，不要求消费侧执行协议。
- **事实失败不能隐藏。** 无法确认是否已保存时，适配面返回后端无关的结果，门面阻止不安全续写并提供必要的恢复行为；不外泄事务状态机、数据库事务 ID 或要求调用方自行回滚。既有 owner/dirty/执行排空仍是本机执行领域语义，不因隐藏数据库机制而删除或移入远程 adapter。

## 4. 必须守住的契约

### 4.1 Binding 事实与本机执行语义分开

本机负责 binding 的发现、验证和授权；不可变 binding 记录是持久化事实，允许由数据 adapter 保存，不等于要求远程承担本机执行语义。实施前明确项目/工作区登记、位置证据、binding、执行代际分别由谁持有，以及本机登记丢失后的恢复边界。

- 新 thread 与 binding 的创建关系、legacy 接纳时 binding 与缺失 frozen 的原子提交必须保留；已有 binding/frozen 不可被普通 metadata 更新覆盖。
- 若方案把原本同事务的数据分到本地和远程，必须先给出可恢复的提交状态及故障验证；禁止以「先写 A，再写 B，失败时尽力删除」冒充原子性。
- frozen 的行为保证是只保存一次、不可覆盖，并返回权威快照或明确冲突；并发时的 CAS、胜者重读等由内部实现完成，不让调用方处理数据库竞争协议。新建/fork 中途失败不得发布可执行半成品；无法完成或无法确认完成时给出诚实结果，补偿过程留在内部。
- 普通 fork 保持 source binding 与精确 frozen；子会话冻结 inherited payload/flags 并维持 ancestor/own 边界，不把远程恢复变成重新读取当前父会话状态。

### 4.2 持久化确认与未知结果

- adapter 明确成功返回代表的持久化、顺序及读取可见性保证。flush barrier 确认此前写入已满足契约，而不是仅入队、进入 SDK 缓冲或本地缓存。
- 「应用压缩结果」保证摘要、flags、计数与缓存可见性一致生效，不由调用方分别更新；追加历史、派生快照、删除与回退分别定义完整的可观察结果。底层事务范围、幂等记录与缓存 epoch 留在内部，不作为调用参数或编排步骤。
- 对外区分确定未生效、确定已生效与无法确认结果；不输出数据库事务阶段。网络断连/超时/取消不证明远程未保存；重新读取一次也不证明旧请求已终止。
- adapter 与资源模块内部负责查询/收敛未知写入；门面只提供领域所需的恢复行为及结果。未收敛时不得盲目重试、回滚已保存历史、标 clean、自动 reset dirty 或放行新 owner；既有普通 dirty 恢复不能被用来忽略仍可能生效的远程请求。
- adapter 内部重试只能建立在已验证的幂等或操作身份保证上，相关记录与协议不外泄到消费侧；不能由通用网络重试掩盖重复追加和部分成功。存储版本/并发校验不等于取得执行所有权。
- 保留 writer 的有序批量写入、失败传播与热状态失效语义；评估远程慢请求对无界队列、关闭等待和资源占用的影响，落实有界超时、积压或背压策略。

### 4.3 能力、访问模式与错误

- 分开表达后端支持的会话行为、当前读写权限以及运行时健康/操作结果；提供类型化错误，不靠字符串匹配决定降级。能力面描述能否安全完成某项会话行为，不暴露是否支持 SQL 事务、CAS 或特定隔离级别；提供该行为的 adapter 必须自行满足完整后置条件。
- 调用方进入功能前可检查静态能力和已知访问模式；实际操作仍需处理权限变化、网络失败和提交结果未知。
- 完整可执行会话要求明确的最低能力集合。只读可用于历史访问；只写不具备恢复能力，不能伪装成完整会话后端。能力缺失时在副作用前拒绝相应操作。
- 清理 `ThreadStore` 及 adapter 中掩盖缺口的 no-op、空 flags、假缺失默认实现；真正的数据缺失、legacy 状态与不支持必须可区分。保留能够保证语义等价的便利方法，不一概删除默认实现。
- SQLite 现有只读打开降级保持原行为；Turso 的鉴权、服务不可用、限流等按自身事实分类，不能复用 SQLite 的失败分类或偷偷切换到本地临时库。

## 5. Turso Cloud Adapter 与配置

### 5.1 接入要求

- 实施前核实当时可用的官方 Rust SDK/协议、版本兼容性、事务与批量操作、读取一致性、取消/超时和服务限制，再选择依赖并记录证据。不能仅凭 SQL 兼容推定远程语义等同于本机 SQLite。
- 第一版直接验证远程权威存储，不混入 embedded replica、离线缓存写回或本地/远程双写；若客户端内部存在缓存，必须明确其确认与刷新语义。
- locator 表达存储位置和后端选择，鉴权以独立 secret 引用/环境变量传入。保留 `--db-path` 作为本机兼容入口并明确与新配置的冲突规则；覆盖 TUI、print、ACP stdio 与 `peri meta session` 的打开路径。
- 新 adapter 负责自身 schema 初始化与版本校验；迁移在独立测试目标验证，不以连接成功代替数据契约验证。
- scope/cursor 查询在数据端完成有界过滤与分页；避免将全部消息拉回本机筛选。对 fork 的逐条 flags 写入等热点评估批量方案，不让后端差异外溢到业务调用点。

### 5.2 测试凭证与安全边界

用户已说明项目 `.env` 中提供 `TURSO_URL` 与 `TURSO_TOEKN`。本 issue 只记录变量名，不读取、复制或记录其值，也不据此宣称已连通云端。

- `TURSO_TOEKN` 按用户提供的现有拼写记录；是否统一为 `TURSO_TOKEN` 在实际接入前确认。本轮不重命名 `.env`，实现不得静默假定另一名称或把拼写歧义固化为隐式兼容逻辑。
- 云端测试显式启用，通过安全的 dotenv 加载/环境注入读取配置；不得将 `.env` 当 shell 脚本执行。默认本机测试不要求云凭证；缺配置时明确 skip/阻塞，不能算通过。
- token、带凭证的连接信息不得进入源码、fixture、日志、错误、快照、测试报告或提交；定位信息只按必要的脱敏字段记录。不得打印 `.env` 或完整配置调试。
- 实际写测试前确认目标是独立测试数据库或已授权且可靠隔离的测试命名空间。默认只写合成会话，不上传现有历史、真实项目指引或用户 frozen 数据。
- 使用唯一运行标识登记测试创建的资源；清理仅针对本轮所有的数据，禁止清空共享数据库或改动既有数据。报告清理失败/残留，不以关闭连接冒充已清理。

## 6. 实施阶段与完成证据

详细设计入口：[总计划](2026-09-26-session-store-plan.md)。子计划分别覆盖 [A 行为契约](2026-09-26-session-store-sub-plan-a-contracts.md)、[B SQLite/本机执行](2026-09-26-session-store-sub-plan-b-local.md)、[C Turso adapter](2026-09-26-session-store-sub-plan-c-turso.md)、[D 配置与装配](2026-09-26-session-store-sub-plan-d-configuration.md)、[E 消费侧迁移](2026-09-26-session-store-sub-plan-e-consumers.md)、[F 验证与云实验](2026-09-26-session-store-sub-plan-f-verification.md)。下表为目标阶段概览，具体依赖和文件归属以总计划为准；计划编写不构成实施授权。

各阶段服务于同一次整体重构；只完成抽象或只连通 Turso 不构成本 issue 完成。

**review-2 进度（2026-09-26）**：设计闭合完成，未改实现。闭合项与落点见[总计划 §2.1](2026-09-26-session-store-plan.md)；本机基线 `cargo test -p peri-resources --lib` → exit 0，156 passed / 0 failed；两个新测试目标尚不存在（`cargo test … --test session_resources_contract` 返回 exit 101，实测记录在 [F §6.1](2026-09-26-session-store-sub-plan-f-verification.md)）。云端验收仍 **未执行**：原记录为未获授权（cloudAuthorized=false），已被后续用户授权更新：`.env` 指向测试库，允许初始化本次 schema、合成会话写读与仅清理本轮数据；未读取 `.env`、未连接或写入任何用户数据库；远程路线为 over-the-wire 权威读写，SDK/引擎候选两条（Turso 引擎 ↔ `turso_serverless`，libSQL 引擎 ↔ `libsql` remote），由 C-01 只读探测后选定（[C §5.0](2026-09-26-session-store-sub-plan-c-turso.md)）；sync/embedded replica/双写显式 Unsupported。

**A 阶段实施进度（2026-09-26，本轮）**：契约与纯逻辑已落代码；消费侧字段未迁移、两个 adapter 未接、未连接任何云库。

落盘内容：

- `peri-acp-types/src/session_resources.rs`：`SessionResources` 门面（行为清单逐项为**必需方法**，无 no-op 默认）、`AccessMode`/`DataCapabilities`/`ExecutionAvailability`/`SessionAvailability`（三者互不推导）、领域 I/O（`NewSession`/`NewSessionMeta`/`FrozenSnapshotBytes`/`ForkSnapshot`/`ChildSnapshot`/`SessionSnapshot`/`BindingState`/`FrozenState`/`SessionMetaPatch`/`RewindBoundary`）、`SessionResourceError`（失败原因与 `MutationOutcome` 分离，`Unknown` 只由未决持久化产生）、`ChildResumeClaim`、`PersistenceRecovery`。
- `peri-acp-types/src/store.rs` → `store/mod.rs`，新增 `store/history.rs`（纯变换）：fork 重映射（新 ID 经 `allocate_id` 注入，本模块不生成 UUID）、投影 flag 规则、flags 批次（默认即移除）、追加 ID 冲突检测、rewind 边界（`KeepThrough`/`RemoveFrom` 不合并）、compaction 变更应用；`CompactionLifecycle` 改名 `CompactionChange`（全仓 29 处引用同步）。
- `peri-resources/src/sessions/data.rs`：`SessionDataPort` 内部行为 seam（无事务/CAS/SQL batch/连接/重试令牌）+ `SessionStoreId`/`HostInstallationId`/`StoreRegistration`/`ChildResumeRecord`。A 阶段无 implementor，模块内以 `#[allow(dead_code)]` 标注并写明「B/C adapter 接入后删除」。
- 消费侧改用共享纯规则（行为不变，消除重复实现）：`peri-acp/src/dispatch/session_fork.rs`、`peri-acp/src/host/requests/rewind.rs`、`peri-agent/src/session/transcript.rs`。

门面类型链与兼容退出（最终类型见 [A §2.1](2026-09-26-session-store-sub-plan-a-contracts.md)；退出按 [E](2026-09-26-session-store-sub-plan-e-consumers.md) 逐引用替换）：

| 环节 | 最终类型 | 本轮状态 |
| --- | --- | --- |
| `peri-acp-types::store::ThreadStore` | 删除（契约由 `SessionResources` 取代） | 保留为**迁移桥**，模块文档写明「不扩展、不新增 no-op 默认」 |
| `peri-resources::Resources.thread_store()` | `session_resources: Arc<dyn SessionResources>` + `session_resources()` | 未改字段；结构体文档写明最终字段与访问器名 |
| `Controller::sessions()` | `Arc<dyn SessionResources>`（名称保留） | 未改；当前仍返回 `Arc<dyn ThreadStore>` |
| `HostAssemblyInput` / `AcpServerConfig.thread_store` | `session_resources`（`AcpServerConfig` 删除该字段） | 未改 |
| `CommandContext.thread_store` / `SubagentHost.thread_store` | `session_resources`（不保留同义双字段） | 未改 |
| `SessionExecutionLease` | 公共面仍是 `thread_id` + `mark_clean` 两项 | 未改，已是目标形状 |

本轮验证（全部 exit 0，未使用 no-op/ignore/删断言、未放宽任何断言）：

| 命令 | 结果 |
| --- | --- |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0（workspace 无 error/warning） |
| `cargo test -p peri-acp-types --lib` | 459 passed / 0 failed（新增 `store::history` 12 项 + `session_resources` 6 项） |
| `cargo test -p peri-resources --lib` | 156 passed / 0 failed（与 W0 基线一致） |
| `cargo test -p peri-agent --lib` | 868 passed / 0 failed |
| `cargo test -p peri-acp --lib` | 713 passed / 0 failed |
| `cargo check --workspace --all-targets` | exit 0 |
| `cargo doc -p peri-acp-types --no-deps` | exit 0；新模块无 rustdoc 警告（既有警告均为改动前条目） |

剩余事项（未完成，不声明已交付）：`SessionResources` 尚无生产实现（B 的 `SessionResourcesImpl`），生产路径仍走 `ThreadStore`；消费侧六处字段迁移、Turso adapter、云验收（cloudAuthorized=false）均未开始。（`SessionDataPort` 的 `dead_code` 放行已在 B 数据侧改为 `cfg_attr(not(test), allow(dead_code))`：测试目标不放行，门面接入后删除。）

**B 数据侧实施进度（2026-09-26，本轮）**：SQLite 数据面已落代码并接上内部端口；门面（`SessionResourcesImpl`）与消费侧迁移未开始，生产路径仍是 `ThreadStore` 桥，未连接任何云库、未读取 `.env`、未触碰本机真实数据库（全部测试库在 tempdir）。

落盘内容：

- `peri-resources/src/sessions/sqlite_store/database.rs`：私有 `SqliteSessionDatabase`（pool / read_only / canonical 路径 / lease 弱引用表）。数据面与执行面**共用同一 `Arc`**：同一个库只有一条连接真相，不建第二个 pool、不建第二个库文件。`SQLite` 连接、schema 锁、只读 shape probe 与 `close` 一并归它。
- `peri-resources/src/sessions/sqlite_store/session_data.rs`：`SqliteSessionData` 实现 `SessionDataPort` 的全部行为（新建/fork/child/legacy 接纳/一致 snapshot/轻量 metadata 与列表/append/投影/compaction/rewind/精确移除/定向 metadata/删除/子会话认领事实/登记读取/未决收敛/排空/关闭/flags）。事务、`BEGIN IMMEDIATE`、连接与推导语句都在实现内部，端口不导出任何一项。
  - `save_new_session`/`save_fork`/`save_child` 一次事务落 meta+binding+frozen（fork/child 另落 canonical 历史/继承区）；`save_child` 校验 child 的 frozen 逐字节等于 root 已保存快照（不重扫目录、不重冻结）、root 归属等于 parent 链的根、绑定身份继承父会话。
  - `load_snapshot` 在单连接延迟事务里读 meta/binding 分类/frozen/自有 payload/flags/继承区；`load_meta` 与列表用 `THREAD_META_COLUMNS`，不加载 `cached_context` 正文。
  - `append_history` 不再 `INSERT OR IGNORE`：批次内重复与库存碰撞都明确失败（失败批次不落半行），计数/自动标题同事务维护；`apply_message_projections`/`apply_compaction`/`rewind_history`/`remove_history_entries` 把 flags、计数、时间戳与派生缓存失效放在同一个事务里，并拒绝改到别会话的条目。
  - 生命周期锚点：`delete_tree` 在同一事务写 `tombstone/deleting`、显式删除 `execution_runs` 行、删除 `threads` 行，提交后置 `deleted`（崩在中途按已删除幂等修复）；`revoke_unpublished_session` 留 `creation_intent/abandoned` 终态锚点。本机同事务写入不存在 `mutation_pending` 窗口，因此本 adapter 只读该状态并按阻塞处理（`drain`/`recover_persistence`）。
  - `BindingState`：有绑定且登记一致 → `Bound`；绑定指向的本机登记已消失 → `ExternalOrUnregistered`；无绑定且是 child → `ExternalOrUnregistered`；无绑定根 → `Missing`（legacy 的目录来源证据由执行面在同一准入内判定，数据面不冒充 `LegacyConfirmed`）。`FrozenState::Unsupported` 留给能解码 envelope 的 frozen owner（ACP），数据面按 opaque 字节原样返回 `Present`/`LegacyAbsent`。
- `peri-resources/src/sessions/sqlite_store/session_rows.rs`：`threads` 行与 canonical binding 行的唯一写入原语，数据面与桥共用同一列清单与绑定形状校验。
- schema v6 → v7（`sqlite_store/schema.rs`）：同一 `BEGIN IMMEDIATE` 内 ①逐行复制重建 `execution_runs` 去掉 `threads` 外键（`generation`/`clean` 原样保留，复制前后校验行数）；②建 `session_lifecycle_commitments`（无外键，故不被级联带走）与 `session_store_registrations`；③`PRAGMA user_version = 7`。同名表形状不符即失败、整体回滚（版本保持 6、可重试）；辅助表与其他业务表不触碰。只读打开不读也不写 `user_version`、不建表，按必需列形状放行 v6/v7/未来列形状；写打开遇到本构建不认识的版本仍 `UnsupportedSchemaVersion{found, supported}` 拒绝降级（启动降级过滤不变）。
- `SqliteThreadStore` 降为消费侧迁移桥：只转发到共享库句柄 + 暴露 `data_port()`；`ThreadStore` 仍服务现有生产路径，等 E 删除。旧桥里的 `update_message_flags` 等保持原语义，新语义只在数据面生效。
- 既有实现的必要改动：`WorkspaceError` 加 `Clone`（错误经 `anyhow` 链落到 `SessionResourceError` 时按原变体重建，不重新解释）；`compaction::load_flags_on` 对损坏 message_id/projection 明确失败而不是静默跳过（B §7 要求，桥的 `load_message_flags` 也走这条路径）。
- 新增测试 26 项：`sqlite_store/session_data_test.rs`（22：完整新建/快照一致与轻量投影/append 碰撞/source 不变的 fork/child 的 root frozen 与关系约束/legacy 接纳与 dirty 防绕过/投影与 compaction 原子性与归属/rewind 两种边界与未知截止点/精确移除幂等/定向 metadata/子会话认领/登记歧义/删除墓碑与执行行/撤销锚点/未决门禁与崩溃收敛/只读与关闭/dirty 跨实例）、`sqlite_store/schema_v7_test.rs`（4：v6→v7 保留 dirty 与辅助表、迁移失败整体回滚、只读打开不迁移且写打开拒绝未来版本、升级后重开不重复迁移）。

本轮验证（全部 exit 0；未使用 no-op/ignore/删断言，未放宽任何断言）：

| 命令 | 结果 |
| --- | --- |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo check --workspace --all-targets` | exit 0 |
| `cargo test -p peri-resources --lib` | 182 passed / 0 failed（156 基线 + 22 数据面 + 4 v7 迁移） |
| `cargo test -p peri-acp-types --lib` | 459 passed / 0 failed（`WorkspaceError: Clone` 后无回归） |
| `cargo test -p peri-acp --lib` | 713 passed / 0 failed |
| `cargo test -p peri-agent --lib` | 868 passed / 0 failed |

剩余事项（B 数据侧之后，未完成，不声明已交付）：

1. `SessionResourcesImpl` 门面未实现：写入前的 owner/未决检查、`MutationOutcome` 三态与 guard、创建准入与补偿、close 协调都还没有落点；`SqliteThreadStore::data_port()` 目前唯一调用方是测试（已就地注明）。
2. 生产路径仍走 `ThreadStore` 桥：`create_bound_thread`/`create_thread` 的 INSERT 与 `session_rows` 原语尚未合并，`append_payloads` 仍是 `INSERT OR IGNORE`，`update_message_flags` 仍逐条写 + 另行 invalidation——这些都要在 E 切换时删除，不作为完成态。
3. `mark_clean` 的「行不存在」容忍分支仍只查 `threads` 行计数（B §4.4.5 要求同时要求墓碑存在）：`delete_tree`/`revoke` 现在会写墓碑，但执行面的判据切换属 B-03，未做。
4. `FrozenState::Unsupported` 的产生点在 ACP 的 frozen 解码侧（E），数据面按 opaque 返回。
5. C（Turso adapter）、D（locator/配置装配）、F（云实验）未开始。

**B 执行侧实施进度（2026-09-26，本轮）**：本机执行面与组合门面已落代码；生产消费侧未切换（仍走 `ThreadStore` 桥），未连接任何云库、未读取 `.env`、未触碰本机真实数据库（测试库全部在 tempdir，`HOME` 未参与）。上一段「剩余事项」中第 1、3 项已在本轮关闭，第 2 项属 E。

落盘内容：

- `peri-resources/src/sessions/sqlite_store/local.rs`（新）：`LocalExecution` —— 本机执行面唯一持有者（发现、登记、owner、dirty、创建准入、撤销、关闭）。可见性收在 `crate::sessions`，业务侧与其他 crate 拿不到。`create_with_lease` 是 B §5.1 的本地塌缩：先占稳定 OS 锁，再在一个 `BEGIN IMMEDIATE` 内校验绑定关系与关键文件对象、拒绝被撤销/删除过的 identity、写 `threads` + `session_bindings` + `execution_runs`（gen 1，`clean=0`）；失败则一行不留（锁文件句柄随返回值释放），提交后崩溃只是普通 dirty，恢复依据是完整数据加代际本身。`admit_existing` 只为「数据已保存、执行代际未写」的收敛补准入（已有代际时拒绝插队）。`legacy_confirmed` 只看本机来源证据（无绑定、无父会话、无执行代际，且保存的绝对 cwd 落在本机已登记工作区内）。
- `peri-resources/src/sessions/resources.rs` + `resources/{gate,claim}.rs`（新）：`SessionResourcesImpl` 实现 A 的全部行为。统一准入 `MutationGate`：能力/权限 → 未决持久化 → 本 root 有效 owner，检查在门面内部，不靠调用方先查。效果结清 `WriteScope::settle`：只有 `Applied | NotApplied` 才释放写入准入，`Unknown`（含取消）丢弃范围，由 `Drop` 在租约上留下 `mutation_uncertain`。
  - 只读不退化：已有会话上的写入返回 `ReadOnlyStore`（历史可读、执行权不可得）；需要登记新身份/新绑定的写入返回 `Workspace(ReadOnlyStore)`（连会话都还没有，没有可降级的对象）。`SessionResourceError::read_only_admission()` 把前者映射到既有 `ReadOnlyAdmission::ExecutionLeaseRequired`，消费侧迁移时不需要新的降级分支；`PersistenceUncertain` 不在该集合里。
  - 创建诚实结果：`create_session` 在 identity 已存在且**数据完整但没有执行代际**时尝试收敛准入（binding 身份一致 + 工作区证据仍然成立）；前提不成立则返回 `SavedButNotAdmitted`（效果为 `Applied`，调用方不得据此删数据），绝不谎称「确定未创建」。`save_fork` 先完整落库再准入，准入失败同样如实报告。
  - `abandon_initialization`：校验传入 lease 就是本进程这条 identity 的活 owner（按分配地址比较，另一条会话的 lease 不能替它补偿），随后关闭准入 → 等待在途写入 → 数据面撤销（终态锚点 + 显式删执行行 + 删数据行）→ 释放 OS 锁；补偿失败也释放锁，不吞掉补偿错误。
  - `claim_child_resume`：先在 root 的**写侧**门禁内完成「读状态 + 写 active」（并发认领只有一个能成功），handle 保存认领前记录；`mark_running`/`hand_off_to_background` 维持 active，`mark_failed`/`mark_terminated` 走同一条恢复路径把原状态写回；移交后台后前台不能覆盖后台持有的终态；handle 的每次写入同样过统一准入。
  - `delete_session_tree` 需要活 owner 且无未决写；`drain_persistence`/`close` 有界等待（10s）在途写入并报告未结清（超时是 `Timeout`，未决是 `PersistenceUncertain`）；`close` 只停止新准入（重复关闭幂等成功），不代写 clean、不关连接池——`Incomplete` 路径要保留 owner 与唯一关闭句柄让重试可行。
- `peri-resources/src/sessions/sqlite_store/failure.rs`（新）：数据面、执行面与门面共用的失败分类（行缺失 / workspace 语义 / 唯一键冲突 / 外键与未登记 / 解码 / 暂不可用），本机执行面失败不冒充「没有这条会话」。
- `execution.rs`：抽出 `owner_lease`（沿 parent 链找活 owner；有绑定而无 owner 是 `ExecutionLeaseRequired`，不是「无 owner」）与 `live_owner_lease`（诊断读取，不把无 owner 当错误）；新增 `ExclusiveExecutionGuard`（写侧门禁，与读侧同样按效果结清）与 `TransactionEffect`（把「提交自身的失败」单独标出）；`ExecutionLease::abandon_ownership`（关闭准入 → 等在途 → 补偿 → 释放锁）。**`mark_clean` 的「记录缺失」容忍分支改为只认墓碑**（`tombstone` + `deleting`/`deleted`）：记录缺失本身不再等于「已删除」，B §4.4.5 关闭。
- **v7 之后的删除语义修复（生产路径）**：`SqliteThreadStore::delete_thread` 现在与数据面删除同语义——同一事务写墓碑、**显式删除 `execution_runs` 行**、再删 `threads` 行，提交后置 `deleted`。v7 去掉 `execution_runs` 外键后，旧实现只删 `threads` 会静默留下永不收敛的孤儿执行行；墓碑同时让 ACP 新建/分叉失败补偿里的 `delete_thread` + `mark_clean()` 组合继续成立（容忍分支的新判据）。该处也改用 `TransactionEffect`：提交自身的失败不再被当成「没生效」。
- 端口与登记类型的 `allow(dead_code)` 全部删除；`SqliteThreadStore::data_port()` 删除（门面自己构造共享句柄，业务侧没有裸写入口）；仅在「生产调用方属 C/E」的三处（`save_new_session`、`load_store_registration`、`load_flags`）保留 `cfg_attr(not(test), allow(dead_code))` 并在文档里写明归属与删除条件。

本轮新增测试 23 项：`sessions/resources_test.rs`（17，含跨进程子进程用例）、`tests/session_resources_contract.rs`（6，F 固定的集成目标名）。覆盖：完整创建与 owner 一次成立、失败创建一行不留、identity 复用与已保存态收敛、`SavedButNotAdmitted` 前提变化、只读两条失败路径与零副作用、无 owner 拒绝、未决写阻塞全部 mutation（读取与列表不受影响）、效果结清三态（`NotApplied` 结清 / `Unknown` 不结清并阻断 clean 与排空）、取消后未决租约、撤销的 owner 校验与终态锚点、child 沿用 root owner 与 root 关闭后的写入拒绝、认领串行与恢复、删除墓碑在级联后仍可判定且 owner 能收尾、关闭的幂等与未结清上报、跨进程 busy/dirty/精确解除。

本轮验证（全部 exit 0；未使用 no-op/ignore/删断言，未放宽任何断言）：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | 199 passed / 0 failed（182 → 199，新增 17 项门面测试） |
| `cargo test -p peri-resources --test session_resources_contract` | 6 passed / 0 failed（F §6 固定的目标名，`--list` 6 tests） |
| `cargo test -p peri-acp-types --lib` | 460 passed / 0 failed（+1：`read_only_admission` 不变量） |
| `cargo test -p peri-acp --lib` | 713 passed / 0 failed |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |

剩余事项（本轮之后，未完成，不声明已交付）：

1. `SessionResourcesImpl` 尚未接入生产；消费侧六处字段迁移与旧 trait 退出属 E（`ThreadStore` 桥仍在，桥里 `create_bound_thread`/`append_payloads`/`update_message_flags` 仍是旧语义）。
2. 桥的委托型写入（compaction 的四个函数）仍按 SQL 层粒度结清：对本地 SQLite 而言「返回 `Err` 即已回滚」可证明，但 `Unknown` 只在新行为层（门面）表达——E 删除桥时一并消失，不在这里改成两套机制。
3. C（Turso adapter）与 D（locator/配置装配）未开始；远程登记表只有读取（`load_store_registration`，生产调用方属 C）。
4. `FrozenState::Unsupported` 的产生点仍在 ACP 解码侧（E）。
5. `SessionResources::close` 目前不关闭连接池（`Incomplete` 重试需要连接）；确定性 teardown 由 D 在装配层决定。

**B 执行侧复核与修复（2026-09-26，同日复核轮）**：本轮不新增设计，只对已落盘实现做逐条复核，发现并修复两处真实缺陷；未读取 `.env`、未连接云库、未触碰本机真实数据库、未执行任何 git 写操作，起始 WIP 的四个文件/子模块未改。

- **clippy 实际失败（上一轮「exit 0」不成立）**：`sqlite_store/local.rs:321` 的 `row.cwd = &binding_cwd` 是 `&&str` 多余借用（`path_text` 返回 `&str`），`cargo clippy --workspace --all-targets -- -D warnings` 报 `needless_borrow` 并中止。已修为 `row.cwd = binding_cwd`。
- **`save_child` 的写入门禁形同虚设**：门禁原本用 `with_mutation(&child.target.thread_id, …)`，而新 child 尚无 `threads` 行、也不会有自己的执行代际，`owner_lease` 沿链解析得到 `None` → `WriteScope::Concurrent(None)`，adapter 工作实际不在任何 guard 之内（违反 B §4.1.3「mutation guard 覆盖真正的 adapter 工作完成」，也与 A §112「使用已存在根 owner」不一致）：该写入被取消/超时后不会在 root 租约上留下未决证据，`close`/`mark_clean` 会把仍在途的 child 写入当成已结清。已改为挂在 root owner 上（`with_mutation(&child.root_id, …)`），授权判定（`same_lease`）不变。
- **新增回归测试**：`sessions/resources_test.rs::test_save_child_write_waits_for_the_root_gate` —— 占住 root 写侧门禁时 child 保存必须在准入处等待且一行数据都不落，释放后成立且 root 可 `mark_clean`。已按「先暴露原问题」验证：临时回退该修复后测试失败（`resources_test.rs` 断言处）、恢复后通过。
- **未改动但已核对**：`save_fork` 的落库不取租约门禁是有意的 —— 目标是新 identity，落库结果自描述（`threads` 行在、`execution_runs` 无），重试按「已保存、未准入」收敛，不需要别处留未决证据（与 child 的差别已就地写入门面注释）；`recover_persistence` 只做幂等墓碑收尾、`drain` 只读、`adopt_legacy_session` 属 legacy 豁免路径，均不构成缺口。
- **fmt 门禁仍未通过（本轮新增发现，未处理）**：`cargo fmt --all -- --check` 在 17 个文件上报告差异，全部来自 A/B 各阶段落盘的文件（`session_resources.rs`、`store/history_test.rs`、`session_fork.rs`、`transcript{, _test}.rs`、`data.rs`、`resources{.rs,_test.rs}`、`resources/{gate,claim}.rs`、`sqlite_store{.rs, local.rs, execution.rs, session_data.rs}`、`session_resources_contract.rs` 等），说明这些文件落盘时没有跑 rustfmt；`lefthook.yml` 的 `fmt: cargo fmt --check` 因此会失败（CI 只跑 clippy，故 CI 不拦）。**未本轮修复的原因**：差异同时覆盖起始 WIP 的受保护文件 `peri-middlewares/src/mcp/builtin_spike_test.rs`，在不动该文件的前提下无法让 `cargo fmt --check` 变绿，而逐文件部分格式化只会在多个前序文件里留下大量与本阶段无关的改动。本阶段只保证**自己新增的行** fmt 干净（已核对 `resources_test.rs` 新增测试的差异为 0）。解除条件：WIP 收尾后对上述文件跑一次 `cargo fmt`（纯格式，无语义改动）。

本轮复跑证据（实际执行，非沿用上轮结论）：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | 202 passed / 0 failed（上轮 201 + 本轮新增 1） |
| `cargo test -p peri-resources --test session_resources_contract` | 6 passed / 0 failed |
| `cargo test -p peri-acp-types --lib` | 460 passed / 0 failed |
| `cargo clippy --workspace --all-targets -- -D warnings` | 修复后 exit 0 |

| 阶段 | 交付 | 验证 |
| --- | --- | --- |
| A：行为契约与事实归属 | 以领域输入/输出定义门面和数据端口；分离确定性行为与 I/O，明确 binding 一致性、存储身份、最低行为能力及错误模型；核实 Turso 适配可行性 | 接口不含数据库事务/CAS/重试编排；逐项映射既有 workspace/frozen/compact/close 不变量，不可满足项先暴露，不削弱为假成功 |
| B：完整拆分与本机迁移 | SQLite 接入新端口，统一门面、locator 与普通/只读装配；迁移 Agent/ACP/Controller/TUI 合法调用点，清除旧生产旁路 | 既有本机身份、恢复、只读降级、事务、子会话与关闭测试回归；同数据规模成本基线 |
| C：Turso adapter | 实现同一数据契约，组合本机执行端口；配置、能力、错误、写入确认与恢复接通 | 同一行为契约覆盖两 adapter；本机可控故障测试覆盖断连、取消、丢响应与部分失败 |
| D：真实云端实验与收尾 | 显式运行 Turso Cloud 全链路，记录正确性、恢复、时延/请求数/积压及限制；同步事实源 | 冷启动重读验证、云端测试报告、清理结果；不是仅 SDK/SQL smoke test |

**E 执行侧增量修复（2026-09-26，告警与测试挂载批次）**：本轮不新增设计、不改生产行为，只处理前序 E 搬迁遗留的编译告警与测试挂载缺口。全仓 `cargo check --workspace --all-targets --message-format=short` 由 37 条告警降至 6 条（exit 0）。已清：`session/transcript.rs` 两处多余 `mut self`；`session/subagent.rs`、`agent/compact_v2/trigger_test.rs`、`agent/stages/stages_test.rs`、`session/subagent_test.rs`、`session/subagent/provenance_test.rs` 的未用导入；`stages_test.rs`/`full_test.rs`/`provenance_test.rs` 三处「改用 `MockSessionResources` 后遗留」的无用 `dir`/`path` 绑定；`peri-middlewares` 的 `tool_test/resume_test.rs` 11 处被遮蔽的重复 `parent` 构造（同一构造逐字出现两次）及其在占位符用例中已无消费方的 `create_thread`/`cwd` 夹具装配；`test_resources/mock.rs` 一处多余 `mut`；`tool_test.rs` 中诞生即无调用方的 `SessionFixture::open()`。

测试挂载：`session/transcript_test.rs` 相比 HEAD 丢了 1 个 `#[test]` 属性与 9 个用例（前序重写中断所致）。本轮恢复 `test_new_transcript_is_empty` 的 `#[test]` 与用例 `test_flush_persistence_without_backend_is_ok`（断言逐字取自 HEAD，未弱化），`cargo test -p peri-agent --lib -- transcript` = 54 passed / 0 failed。其余 8 个丢失用例（3 个 commit_compaction_lifecycle、1 个 compaction_reload reminder、2 个 apply_compaction_batch、1 个 flush 多错首个、1 个 failed_writer 批次截止）**未恢复**，因它们断言的是逐条 append/flags 部分失败与 invalidation 次序；E §「单次完整行为」已把这些细粒度观察合并，且写侧改为窗口批量（3 次 append 合并为 1 次调用），旧故障注入模型不再对应现契约。是否以新形态重建这些行为断言属 E/F 剩余工作。

**剩余 6 条告警为有意保留**，全部落在本轮新增的测试支撑文件：`session/test_resources.rs` 的 `db_path`、`session/test_resources/mock.rs` 的 `fail_append_at`/`fail_flags_at`/`fail_rewind`/`flag_updates`/`status_writes`、店铺式夹具方法（`list_child_threads`/`list_threads`/`append_payloads`/`load_inherited_context`/`create_bound_thread`/`resolve_workspace`）、`note_budget_failure`/`budget_failures`、`is_clean` 及 `lease`/`budget_failures` 字段。核对结论：这些替身 API 当前无调用方，但对应 HEAD 时代仍存在的真库夹具路径——`session/subagent/provenance_test.rs` 模块注释仍声明「Real SQLite spawn/compact/reopen/resume regression」，`TestSession::db_path` 正是冷重开真库的入口。删 API 会让「恢复真库用例」更难，故按「先核对挂载、不删 API」处理，留给 E/F 批次二选一：恢复真库用例消费它们，或收缩替身。

**本批未触碰的失败用例（如实记录，非本轮引入）**：`cargo test -p peri-agent --lib` = 859 passed / 4 failed；`cargo test -p peri-middlewares --lib -- resume` = 23 passed / 3 failed。失败点：`session/subagent/provenance_test.rs:183`（`spawn_subagent: parent session ... has no execution binding (Missing)`）、`session/exec/executor_provenance_test.rs:101`、`session/subagent_test.rs:1860/1931`（取消后持久化状态收尾超时、已提交写入的 owner 未保留）、`tool_test/resume_test.rs:271`（`thread not found`）、`resume_integration_test.rs:353`、`active_message_test.rs:129`（`bound subagent belongs to another root session execution owner`）。原因集中于门面/所有权夹具尚未迁移，属 E 计划消费侧剩余工作。

本轮为单会话直接实施（本会话未暴露子代理工具，未能按既有分工走「另一模型编码 + 独立验证」），证据为本会话直接执行的命令输出；未新增 `#[allow(dead_code)]`/`#[ignore]`，未改生产行为，未读 `.env`、未连云库、未执行 git 写操作，起始 stage 的 `CLAUDE.md` 及其它 WIP 未改。ACP 生命周期与 Controller/middleware 迁移、Turso adapter 仍未开始。

**E 执行侧行为回归与修复（2026-09-26，W2b 批次）**：本轮不改 A/B 任何实现，不新增设计；只对 Agent 已迁移行为做验证与最小修复，起点为本机 `cargo test -p peri-agent --lib`（隔离 `HOME`/`XDG_*`，保留 `CARGO_HOME`/`RUSTUP_HOME`）= **859 passed / 4 failed**。

1. **生产缺陷（本批修复，非仅测试）**：`session/subagent/factory/claim.rs` 在迁移中被改写成「内联 await `claim_child_resume` + `Drop` 里 detach 一个恢复旧值的任务」，丢掉了 HEAD 的 **claim worker 所有权**。后果有二：①调用方 future 被取消会连同资源侧**在进行中的 active 写入**一起丢掉（`ResumeClaim` 的 Drop 只发补偿，写入本身已被取消）；②运行中被取消时 `Drop` 只把状态写回「认领前」，不再写领域终态 `cancelled`——线程以旧的 `done` 记录存活，取消事实丢失。已按资源侧 handle 语义恢复 worker：worker 独占「校验 → 认领（读状态+写 active）→ 等决定」全序列，调用方只提交领域结果（`HandOff` / `Finish(status)` / 关闭=准备失败），`Drop` 运行中发 `Finish(Cancelled)`、准备阶段直接关闭决定通道；`JoinHandle` 只 detach 不 abort，写入永不因调用方取消而中断。`validate_thread` 仍在 worker 内、仍可被取消（只读，无补偿需求）。
2. **两处失效回归改走真实 SQLite 门面**（此前是全 mock 自洽）：
   - `session/subagent/provenance_test.rs`：模块声明「Real SQLite spawn/compact/reopen/resume regression」，但夹具曾被 `MockSessionResources` 顶替——父会话无绑定、`spawn_subagent` 需要 root 执行所有权、且「冷重开」写成 `MockSessionResources::new()`（**另一个空库**，语义上不可能是重开）。现改为真门面 `SessionResourcesImpl`：真绑定 root + 持有 `SessionExecutionLease` 落 child、真库读回 child/父 flags 与自有 payload、冷重开为**同一库文件的第二个句柄**（先 drop 原 lease 释放 OS 锁，再由新句柄按精确代际 `reset_dirty_execution(accept_risk)` 取回执行权）。断言逐条保留，未弱化。
   - `session/exec/executor_provenance_test.rs`：执行侧读入口已迁到门面（`load_session_snapshot`），用例只注入旧 `thread_store`，导致执行路径读不到继承区（`ancestor_len` 断言 0 != 1）。已在同一库文件上补注入真 `SessionResourcesImpl`，用例转绿。
3. **未挂载测试盘点**：`peri-agent/src/agent/events_test.rs`（174 行 / 7 个用例）全仓无 `mod`/`#[path]` 挂载，且 HEAD 同样未挂载（非本批产生；其 `use super::*` 依赖的 `ExecutorEvent` 已迁至协议层，接入前需先修 import/类型归属），本轮只记录不动。

本轮复跑证据（实际执行；未新增 `#[allow(dead_code)]`/`#[ignore]`，未删任何断言，未改 ACP 主生命周期与 Turso，未读 `.env`、未连云库、未执行 git 写操作）：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-agent --lib`（隔离 HOME/XDG） | **863 passed / 0 failed，exit 0**（起点 859/4） |
| `cargo test -p peri-agent --lib -- resume` | 30 passed / 0 failed |
| `cargo test -p peri-agent --lib -- provenance` | 5 passed / 0 failed（含上述两条真库回归） |
| `cargo test -p peri-agent --lib -- transcript` | 54 passed / 0 failed |
| `cargo test -p peri-agent --lib -- compact` | 200 passed / 0 failed |
| `cargo check --workspace --all-targets --message-format=short` | exit 0；`peri-agent` 测试目标告警仍为上一批保留的 6 条（替身夹具 API），本批改动文件 0 告警 |

本批 changed files：`session/subagent/factory/claim.rs`（生产修复）、`session/subagent/provenance_test.rs`、`session/exec/executor_provenance_test.rs`（夹具迁真库）。

剩余（交 W2c）：`cargo test -p peri-middlewares --lib -- resume` 的 3 例（`tool_test/resume_test.rs:271` 等，属该 crate 消费侧夹具，本轮未动）；替身夹具 6 条告警的最终处置（消费或收缩）；`events_test.rs` 的归属判定。

**E 消费侧收尾（2026-09-26，W2c 批次）**：只收敛 W2a/b 遗留的 `peri-middlewares` 失败项与 clippy，未改任何生产实现，未触 ACP 生命周期、Turso 与配置。起点 `cargo test -p peri-middlewares --lib -- subagent` = 203 passed / 3 failed。

三处失败是三类不同根因，均属夹具迁移遗漏或前提被现行契约取代：

1. `active_message_test.rs::test_active_message_resumed_background_execution_accepts_info`：预置 thread 传 `parent_thread_id: None` 建成了自己的根，而工具执行所有权属夹具父会话 → `bound subagent belongs to another root session execution owner`。同目录其它用例一律传 `Some(parent_id)`，此处为遗漏；改为挂在夹具父会话下，断言未改。
2. `resume_integration_test.rs::test_resume_multiple_times_keeps_thread_id_and_completes`：迁真门面后补了父会话句柄，但「cancel 前置」仍靠工具注入 token。`derive_cancel_token` 在 Cascade 下 parent 优先、注入 token 仅作 parent 缺席回退，故注入失效，首次 spawn 直接跑完（实测返回 `echo: task` 而非中断文本）。改为由父会话持有已取消 token（`Session::new_with_cancel`）；第 3 步换回未取消 token，并补齐此前缺失的 `parent_thread_id`/`execution_owner`/`parent_session`（该步在修好第 1 步后必然失败）。
3. `resume_test.rs`：原 `test_resume_thread_id_parent_mismatch_not_rejected` 的前置是 child 的 `parent_thread_id = "some-other-parent"` 指向不存在的 thread。真库下该状态不可读：`sqlite_store/context.rs::resolve_ancestor_chain_on` 对缺失行是 `fetch_optional` + break 的宽容语义，而 `load_inherited_context_on` 对链上每个成员用 `fetch_one`，两者不一致 → 快照读直接报 `NotFound`（实测 `load_messages` = `Err("session not found (NotApplied)")`），resume 因此报「thread not found」；换成指向另一个**真实根**则被归属校验拒绝。无论哪种，「parent 链不匹配仍可恢复」在现行契约下都不成立（§4.1：不得仅持有 child_thread_id 推断执行权），故改写为 `test_resume_thread_id_parent_mismatch_is_rejected_by_root_ownership`：断言拒绝原因正是执行根归属、且被拒绝的恢复不改动 thread 状态。这是按现行契约重写而非弱化。

**clippy 收敛**：清除 9 处 `clippy::clone_on_copy`（`ProjectId`/`WorkspaceId`/`CancelPolicy` 在测试夹具中的多余 `.clone()`）：`peri-agent/src/session/test_resources/mock.rs`、`session/exec/executor_helpers/compact_cancel_test.rs`、`session/subagent_test.rs`、`peri-middlewares/src/subagent/tool/tool_test.rs`。`peri-middlewares` 目标 clippy 已 0 error。

本轮复跑证据（实际执行；未新增 `#[allow(dead_code)]`/`#[ignore]`，未删断言，未改生产行为，未读 `.env`、未连云库、未执行 git 写操作）：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-agent --lib`（隔离 HOME/XDG） | 863 passed / 0 failed，exit 0 |
| `cargo test -p peri-middlewares --lib` | 1817 passed / 0 failed / 5 ignored（5 例 `#[ignore]` 均为既有：终端 120s 等待、2 处 marketplace 网络、hooks 真实 settings、attribution），exit 0 |
| `cargo test -p peri-middlewares --lib -- subagent` | 206 passed / 0 failed（起点 203/3） |
| `cargo test -p peri-middlewares --lib -- assembly` / `-- resume` | 35 / 26 passed，0 failed |
| `cargo test -p peri-resources --test session_resources_contract` | 6 passed / 0 failed |
| `cargo check --workspace --all-targets --message-format=short` | exit 0；告警仍为 `peri-agent` 测试支撑的 6 条（与上批相同，未增） |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 101；剩余 8 条 = 6 条 dead_code（同上，待 E/F 处置）+ 2 条 `clone_on_copy`（`peri-acp/src/session/command/compact_test.rs:188-189`，属 ACP 批次） |

**剩余（交下一工作流）**：

- `peri-acp` raw 桥：`host/mod.rs:194` `AcpServerConfig.thread_store: Arc<dyn ThreadStore>`（同处 197-198 注释已写明「迁移完成后该桥删除」）；经它扩散的生产调用点 `host/assemble.rs:128,202,215,287,596,660,663`、`host/requests/session_lifecycle.rs:38,60,723,1109,1125,1241`、`host/workspace.rs:72`、`host/prompt.rs:219,516`、`host/prediction.rs:19,101`、`host/stdio/mod.rs:127,149`、`session/mod.rs:140,216,232`、`dispatch/session_fork.rs:8,13`（`&dyn ThreadStore` 形参）、`dispatch/session_load.rs`（经 `Controller::sessions()`）。契约约束：`session_resources` 与 `thread_store` 必须出自同一次打开（同库句柄、同一 owner 登记，`host/mod.rs:195-199`），迁移是「删桥 + 调用点改走门面」，不是并存两套入口。
- `peri-controller/src/controller.rs:172-173,203,295-296`：`Controller::new(sessions: Arc<dyn ThreadStore>)` 与 `sessions()` 仍以 raw store 为通道，是 ACP dispatch 的唯一存储入口，需一并换成门面。
- `peri-tui/src/{app,thread,kit,acp_client/client}` 仍有 5 处 `ThreadStore` 引用（含 `peri-tui/tests`），随 ACP 批次一并处理。
- 替身夹具 6 条 dead_code 告警的最终处置（消费或收缩）与 `mock.rs` 拆分（>1000 行）未做；`peri-agent/src/agent/events_test.rs` 仍未挂载（HEAD 亦然）。
- `sqlite_store/context.rs` 的祖先链语义不一致（`resolve_ancestor_chain_on` 宽容 / `load_inherited_context_on` 对链上成员 `fetch_one` 严格）未改：当前只影响「父行被删后遗留的子会话」（`delete` 路径可达），需与「丢失本机登记/外来会话」的边界一起定契约，不宜在本批顺手改。

**ACP 生命周期迁移（2026-09-26，ACP 批次）**：把 `session/new`、legacy 恢复准备、frozen 读取与执行准入迁到完整门面。未触 A/B 数据侧、Turso、配置、`Controller`（`sessions()` 仍返回 raw store），未做任何 git 写操作。

生产改动：

- `host/requests/session_lifecycle.rs`：`handle_new` 改为 prepare（只读定格）→ `create_session(NewSession)`（meta/binding/frozen/执行代际/owner 一次成立）→ `validate_session` 复核 → `assemble_prepared` → 发布；失败走 `abandon_initialization`（装配失败时环境尚未建立，无对外资源需要排空，资源排空后的撤销语义留给 fork 未迁移路径）。ACP 侧 `create_bound_thread` / `acquire_execution_lease` / `store_frozen_snapshot_if_absent`+`delete_thread` 补偿链已从 new 路径删除（`store_new_frozen_snapshot_or_compensate` 仍服务于未迁移的 fork）。`load_frozen_data` 改读门面快照并按 `FrozenState` 三分（Present 解码 / LegacyAbsent 报「Bound session has no frozen snapshot」/ Unsupported 报本构建不可读）。
- `host/requests/legacy_session.rs`：`prepare_for_restore` 改经门面判定 `BindingState`——`Bound` 直接返回；`ExternalOrUnregistered` 不再当作 legacy；`Missing` 先按保存的绝对 cwd 解析登记后复判（新库/新节点场景）。frozen 用门面 `FrozenState`，接纳走 `adopt_legacy_session`（权威事实，不再有 CAS bool）。
- `host/workspace.rs`：新增 `resource_error`（workspace 语义保留既有载荷，其余按行为失败上报）；`try_acquire_lease` 走门面 `acquire_execution(id, workspace)`，`clear_dirty_generation` 走 `reset_dirty_execution`（`accept_risk: true`——host 自动解除是既有裁决，现在由门面要求显式承担）；删除仅为 raw 错误链存在的 `read_only_reason`。
- `peri-acp-types/src/workspace.rs`：新增 `SessionBinding::from_workspace`（绑定构造唯一入口，版本/revision 由契约固定）；`peri-resources` 私有 `binding_for` 删除并委托它。
- `peri-resources/src/sessions/sqlite_store/local.rs`：`legacy_confirmed` 目录比较改按文件系统事实（两侧 canonicalize）。原字面 `starts_with` 把 macOS `/var` 与 `/private/var` 的同一目录判成外来会话，本机 legacy 因此无法确认（本轮回归实测暴露，`legacy_history_freezes_saved_workspace_configuration_and_plugins` 失败后修复）。
- `peri-acp/src/session/command/compact_test.rs`：夹具改用 `SessionBinding::from_workspace`（顺带清掉 2 条 `clone_on_copy`）。

新增回归：`host/prepared_test.rs::new_session_persists_prepared_frozen_bytes_once`——new 之后持久化 frozen 字节等于同参数准备输入的字节、binding 为 `Bound`、live frozen 与持久化字节同源。

证据（隔离 HOME/XDG；未新增 `#[allow]`/`#[ignore]`，未删断言）：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-acp --lib` | 719 passed / 0 failed（起点 718，+1 新回归） |
| `cargo test -p peri-resources --lib` | 206 passed / 0 failed |
| `cargo check -p peri-acp --all-targets` | exit 0 |
| `cargo clippy -p peri-acp -p peri-acp-types -p peri-resources --all-targets -- -D warnings` | exit 0（`compact_test.rs` 2 条 `clone_on_copy` 与 `prepared.rs` 2 条新告警已清） |

**ACP 剩余生命周期迁移（2026-09-26，ACP 续批）**：把 fork、close/delete、rename、reset-dirty、prediction 标题、scoped list 与 metadata 轻量读迁到门面，并消除 ACP 生产侧 `cfg.thread_store` 直写与存储补偿。fork 改为「一致 source 快照（`load_session_snapshot`）→ 领域纯 ID 映射（`remap_fork_history`）→ 一次 `save_fork`」，删除逐条 `update_message_flags`、`delete_thread` 补偿与 forked-frozen 二次存写；`dispatch/session_fork.rs` 的 raw `fork_session` 与其注入故障的 mock 存储一并删除，测试改走真实 SQLite 门面（ID/flags 独立性、未闭合工具调用拒绝）。close/delete 走 `drain_persistence` → `delete_session_tree` → `mark_clean`，未加载会话的 rename/delete 经 `workspace::acquire_transient_owner`（按保存 cwd 解析 + 取得 owner）；rename/prediction 经 `update_session_meta` 定向更新；`session/list` 与 scoped list 经 `list_sessions`；metadata 走 `load_session_meta`。`retain_failed_assembly` 的生产调用点随 fork 补偿链消失，其形态移入测试夹具保留关闭重试不变量（非新增 allow）。

证据（隔离 HOME/XDG；未新增 `#[allow]`/`#[ignore]`，未删断言，未做 git 写操作）：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-acp --lib` | 718 passed / 0 failed（fork 测试 3→2，其中 2 个补偿用例由「原子保存 + 未闭合调用拒绝」替代） |
| `cargo check -p peri-acp --all-targets` | exit 0，零告警 |
| `cargo clippy -p peri-acp -p peri-acp-types -p peri-resources --all-targets -- -D warnings` | exit 0 |

仍未完成（不得标记完成）：`AcpServerConfig.thread_store` / `HostAssemblyInput.thread_store` / `Resources.thread_store` 双句柄仍在（TUI 与测试夹具仍消费）；`SessionManager` 仍持 raw store（`list_sessions()`/`thread_store()` 无生产消费者，切换需把同步夹具改 async 开 SQLite 门面）；保留的 raw **读**为 `session_context_payload`/`check_expected` 的 binding 复核与 history replay（`load_context_payloads` 祖先链语义需先成为领域函数，且门面缺「按 session identity 复核绑定 → workspace」与轻量 binding 投影）；`Controller::sessions()` 仍返回 raw store；`MutationGate` 仍具体绑定 `SqliteSessionData` + `LocalExecution`（C 泛化）；Turso/配置/云/Fable 未开始。

下一步（未做，不得标记完成）：`Controller::sessions()` 仍返回 `Arc<dyn ThreadStore>`；`AcpServerConfig.thread_store` / `HostAssemblyInput.thread_store` / `Resources.thread_store` 双句柄仍在，dispatch（`session_load`）、`prompt`、`SessionManager`、stdio 与 TUI 仍走 raw；history replay 仍用 `load_context_payloads`（ancestor 链组合语义需领域函数后再迁）；`check_expected` 的 binding 复核（`validate_session_binding`/`reassert_session_binding`）未迁；`MutationGate` 仍具体绑定 `SqliteSessionData` + `LocalExecution`（C 泛化）；Turso/配置/云/Fable 未开始。

## 7. 验收矩阵

### 7.1 行为与生命周期

| 场景 | 必须观察到的结果 |
| --- | --- |
| 两 adapter 切换 | 仅改变装配/定位配置，不改业务调用点；具体后端类型不出实现与装配层 |
| 行为纯化与机制封装 | 确定性变换可用显式输入/输出独立验证；同一行为契约测试覆盖两 adapter 的可观察结果，不断言共享事务步骤。审阅门面/数据端口及调用点：无事务句柄、CAS/隔离参数、SQL batch、底层重试或补偿编排 |
| 新建、追加、flush、关闭、重开 | 通过真实门面与会话路径运行；新进程/新连接重读顺序、payload（含 reminder）、计数与元数据一致 |
| load/resume 与 frozen | 恢复原 snapshot，不从当前环境重冻；未来版本、损坏、缺失本机登记与 legacy 缺失分别处理 |
| 普通 fork 与子 Agent | source binding/frozen 精确保留；fork 新 ID 和 flags 正确；继承快照与 own region 分离，写入仍受根 owner 约束 |
| compact / rewind / 删除 | compact 原子提交与缓存失效，失败不产生半套状态；rewind/删除精确作用于目标，冷恢复后结果一致 |
| scoped list 与分页 | 查询不加载大字段/消息正文、不全量拉取后过滤；范围与分页语义保持 |
| 只读、只写、能力缺失 | 进入相应功能前明确可用性；写入/执行被正确拒绝，无 no-op、假缺失或静默丢数据 |
| 本机多进程与存储别名 | 同一会话至多一个 owner；不同 locator 别名不绕过锁域；子会话不能绕过根 owner |
| dirty、取消与 close | 等写入及 owned 执行排空后才 clean；未知提交另有收敛路径，不借普通 dirty reset 跳过；未完成关闭保留真实阻塞 |
| 外来会话/本机登记丢失 | 不按当前 cwd 自动接纳或改绑；历史可读性与执行资格分别表达 |
| SQLite 兼容 | 原默认位置、现有历史、升级/legacy 接纳、只读降级、取消和恢复用例保持；无新增网络依赖 |

### 7.2 故障与远程实验

- 可控故障测试覆盖：发送前失败、事务拒绝、部分步骤失败、提交成功但响应丢失、请求取消后远端晚提交、鉴权失败、只读权限、服务不可用，以及 new/fork 补偿失败。不能只 mock 成一个自洽的成功流程。
- 对提交后丢响应/晚提交，断言不重复追加、不撤销已提交 compact、不发布错误 clean，也不允许新 owner 与旧写入并发；记录实际采用的结果收敛证据。
- 真实 Turso Cloud 测试是显式、隔离的外部实验，不替代默认确定性测试。至少经门面跑通新建、追加、fork、compact、关闭、冷 load/resume；进程重启后比对 canonical 数据与派生状态，不以热内存或本机 fake 通过代替云端证据。
- 使用相同合成历史规模记录 SQLite 基线与 Turso 的操作时延、请求次数、队列积压、flush/close 耗时；声明环境、样本规模与测量方法。SQLite 不得因拆分出现未解释的退化；Turso 的性能结论以实测为准，不预填收益数字。
- 验收记录包含运行命令、SDK/协议版本、已覆盖场景、未验证项、失败诊断及测试数据清理结果；无凭证/未执行/环境阻塞不得标为通过。

## 8. 文档与完成边界

实现完成时按 `DOC-UPDATE-001` 同步受影响的 architecture contracts、身份设计、模块指引、code-index 与测试入口；本 issue 不长期复制现行事实源。只有整体门面拆分、SQLite 回归和 Turso Cloud 实验均有证据时才进入完成评估；若 Turso 无法满足关键契约，保留阻塞及实验结论，不以“接口已预留”或静默降级关闭 issue。

## 7. ACP 生命周期迁移 — 唯一门面链（PARTIAL，2026-09-26）

本批把「生产双句柄」拆掉：`Resources` / `Controller` / `HostAssemblyInput` /
`AcpServerConfig` / `SessionManager` / TUI services 只持 `SessionResources` 门面。

- A 层新增三个只读投影（此前只能走裸存储）：`load_session_binding`（轻量绑定分类，
  不拉历史）、`validate_bound_workspace(id, BindingRecheck)`（按 identity 复核绑定并给
  workspace）、`load_session_history`（继承区 + 自有 payload 的历史回放）。B 层在数据端口
  补同名读取并由门面实现；`BindingRecheck::{Full,Recorded}` 对应原
  `validate_session_binding` / `reassert_session_binding` 的两次复核力度。
- 删除：`AcpServerConfig.thread_store`、`HostAssemblyInput.thread_store`、
  `Controller::with_session_resources/session_resources`、
  `SessionManager::thread_store/list_sessions`、`Resources::thread_store`、
  `peri_agent::resources::{open_thread_store,open_thread_store_with,open_store_and_session_resources_with}`、
  `peri_agent::thread::{ThreadStore,SqliteThreadStore,FilesystemThreadStore}` re-export、
  `ExecutorContext.thread_store` 与 `WorkflowAgentConfig.thread_store`（生产恒 None 的死字段）。
- 保留的测试入口：`peri_resources::sessions::open_store_and_facade_for_tests`（配对构造
  裸句柄 + 门面，供夹具逐条构造事实）；`peri-resources::sessions::open_session_resources_read_only`
  是 `peri-cli meta` 的只读生产入口。
- 证据：6 个 crate `--lib` 全部通过（peri-acp / peri-controller / peri-agent / peri-tui /
  peri-resources / peri-middlewares）；peri-resources、peri-tui、peri-controller
  `--all-targets` 通过；`peri-acp --all-targets` 仍失败（86 处，全部在测试夹具：
  `cfg.thread_store` 51+16 处、`SessionContext.thread_store` 5 处、旧夹具函数 3 处、
  测试替身缺三个新 trait 方法 1 处等）。
- 未完成：peri-acp 测试夹具迁移（改用 `cfg.session_resources` / `open_session_resources_with`
  + 显式夹具入口）；`AgentState`（v1）的 `with_persistence` / `with_thread_context` 仍持裸
  trait 类型但生产无调用方（待随 v1 退役清理）；Turso / 配置 / 云 / Fable 未开始。


## 8. 迁移收尾：夹具/门面收敛（2026-09-26）

本批只闭合第 7 节遗留的测试夹具迁移与测试替身收敛，不新增契约、不改生产数据路径。

- 门禁：`cargo check --workspace --all-targets` exit 0（零 error、零 warning）；
  `cargo clippy --workspace --all-targets -- -D warnings` exit 0（第 7 节的红是
  `peri-agent` 测试替身的 dead_code，已用「删除无消费者的多余替身 API」收敛，
  未新增 `allow(dead_code)`/`#[ignore]`）；`cargo fmt --all --check` 仅剩独立 WIP
  `peri-middlewares/src/mcp/builtin_spike_test.rs`（本批未触碰）。
- 夹具迁移：peri-acp 的 requests/legacy/recovery/stdio/session 夹具改吃门面
  （`cfg.session_resources`）；legacy、缺 binding、坏 frozen、未知版本等坏数据由
  `peri_resources::sessions::open_store_and_facade_for_tests` 配对裸句柄按原表播种，
  生产 config 未恢复 raw 入口。断言逐文件等量保留（177/31/40 等）。
- 证据：`peri-acp --lib` 718、`peri-agent --all-targets` 863+4、`peri-resources`
  206+6、`peri-controller` 131、`--workspace --doc` 全通过。
- 删除 `AgentState` 的裸存储遗留路径（`store`/`thread_id`/`persist_tx`/`persist_handle`
  字段与 `with_persistence`/`with_thread_context`/`store()`/`own_thread_id()`/
  `shutdown_persistence`/`is_persistence_shutdown`/`ancestor_len`）：全仓（含测试）
  零消费点；会话历史事实源是 `MessageTranscript`（绑 `SessionResources` 门面）。
- `peri-agent` 测试替身按职责拆分为 `test_resources/mock/{mod,observe,fixtures,session_resources}.rs`
  （原单文件 1160 行），并删除无消费者的替身 API：裸存储形状的
  `list_threads`/`list_child_threads`/`append_payloads`/`load_inherited_context`/
  `close`/`create_bound_thread`/`resolve_workspace`，注入面 `fail_append_at`/
  `fail_flags_at`/`fail_rewind`/`status_writes`/`flag_updates` 与预算计数、`is_clean`、
  `TestSession::db_path`（冷重开真库的用例自建 tempdir 路径，见
  `session/subagent/provenance_test.rs`）。第 7 节「先核对挂载、不删 API」的二选一
  在此按「收缩替身」结项。
- 剩余 raw 桥：`open_store_and_facade_for_tests`（仅测试用配对构造）；
  `peri-resources` 内部 `SqliteThreadStore`/`FilesystemThreadStore` + `SqliteSessionData`
  仍是门面背后的 B 层实现（其实现级测试直接绑 `Arc<dyn ThreadStore>`）；生产侧
  `peri-agent::resources`、ACP、Controller、TUI 只持门面（无 `dyn ThreadStore` 字段）。

下一接点（D/C，均未开始）：D（配置）从 `Resources::open_with` /
`peri_agent::resources::open_session_resources_with` 的打开面接后端选择；C（Turso）
在 `SessionResources` 门面后替换 B 层数据端口，接点是 `MutationGate` 仍具体绑定
`SqliteSessionData` + `LocalExecution`（需泛化），门禁为
`peri-resources` 实现级测试 + `tests/session_resources_contract.rs`。

## 9. C 远程基础与 D 配置基础（2026-09-26，本轮实施）

授权更新：用户确认 `.env` 指向**测试库**，允许初始化本任务 schema、合成数据，以及只清理本轮对象；
C 子计划 §6.1 的 `cloudAuthorized=false` 与「只写计划」已失效。本轮**未读取真实历史、未上传任何
项目数据**，探测只发只读请求；凭证只在本进程内解析，不进命令行、日志或报告。

### 9.1 C-01 只读探测证据（引擎/驱动选择）

| 观测 | 结果 |
| --- | --- |
| `.env` 键存在性（只列键名） | 16 个键；引擎相关键名为 `TURSO_URL`、`TURSO_TOEKN`（拼写与计划一致） |
| locator scheme / 主机家族 | `turso://`；官方 Turso Cloud 域 |
| `GET /version` | 404：该端点在官方文档里是 libSQL/sqld 的版本身份入口，**但 404 不能单独证明目标库不是 sqld**（服务端可不暴露该路由）——引擎身份依据是「官方驱动↔引擎对应关系 + 选定驱动上的 SQL 行为实验」，不是这个端点 |
| `POST /v2/pipeline` 只读 `SELECT` | 200、`results[0].type=ok`，Bearer 认证通过 |
| 协议层参数绑定回环 | text / 64 位整数（9007199254740993）/ NULL 全部原值返回 |
| SDK 连接路径（`turso_serverless` 0.1.3） | 连接成功；SDK 层 text/int64/NULL 绑定回环全部 true |
| 引擎方言 | `sqlite_version()` = 3.50.4 |

按官方「驱动匹配引擎」对应关系选定 **`turso_serverless` 0.1.3**（2026-09-04 发布；依赖
reqwest 0.13 / tokio 1 / thiserror 2，与工作区既有版本同族）。`libsql` 0.9.30 未采用，
停更的 `libsql-client` 不采用，`turso` crate 的 sync 路线按 C §1 明确排除。
C §5.1 的 P1–P7（原子批、唯一键冲突判别、写事务串行化、冷进程权威读、SDK 重试策略、
超限拒绝、收据保留）**未实测**，远程写路径保持关闭。

### 9.2 本轮落盘

- `peri-resources/src/sessions/remote/`：端点解析（引擎只由已确认 scheme 或显式选择给出，
  `https://` 缺引擎时明确要求显式选择）、凭证来源与值分离（`Debug` 脱敏、只接受显式注入、
  不搜索 `.env`）、失败分类与脱敏（SDK 载荷文本不进领域失败）、连接与只读参数绑定回环
  （单次调用 20s 预算；SDK 0.1.3 无客户端超时配置，由 `tokio::time::timeout` 兜住）。
- `peri-resources/src/sessions/open.rs`：typed locator 与打开请求（本机路径保留 Windows
  drive/UNC；`env:` 只解引用一次、不递归；远程缺凭证来源直接报配置错误；locator 原文与
  凭证值不进 `Debug`）。
- `Resources`：`open()`/`open_with()` 归一为同一 open request；新增公开 `open_locator(locator,
  engine, credential_env, read_only)` 与 crate 内 `open_request`；显式只读走既有只读 seam
  （不建目录/库/锁、不迁移 schema）；远程 locator 返回类型化 `RemoteStoreNotWired`，
  **不静默回落**本机库。
- 依赖：`peri-resources` 增 `turso_serverless = "0.1.3"` 与 `url`；测试目标用 `reqwest`。

### 9.3 验证证据

- `cargo test -p peri-resources`：226 lib + 6 集成通过；3 个云端探测默认 `#[ignore]`。
- `cargo clippy -p peri-resources --all-targets -- -D warnings`：exit 0。
- `cargo check --workspace --all-targets`：exit 0。
- 显式云探测实跑（`--ignored`）：输出仅键名存在性、脱敏特征与成功/失败分类，见 9.1。

明确未做：远程写路径与 schema 初始化、StoreId 首次竞争、C-02/C-03 adapter、
D-04（TUI/print/stdio/meta 部署参数迁移）、`MutationGate` 泛化到远程组合。

### 9.4 D 配置面补齐（同日第二轮，仍不含 D-04/D-05）

- `peri-resources/src/sessions/open.rs`：`StorageLocator`（未解析输入）、`SessionStoreOpenRequest`
  （locator + 引擎 + 凭证来源 + 访问意图）、`AccessIntent` 纯解析（只接受 `read-write` /
  `read-only`，无别名与大小写模糊匹配，失败不回显原始取值）、`AccessIntentError`。
- `Resources::open_locator(locator, engine, credential_env, access)`：公开签名接收访问意图拼写，
  在进入任何 I/O 之前完成纯解析；`Resources::open_request` 仍是唯一后端选择点（普通与只读共用）。
- 显式只读：走独立只读 seam，不建目录/库/锁、不迁移 schema、不登记 owner 与 binding；只读失败
  的类型分类（`ReadOnlyThreadStoreError::kind()`）保留在 source chain，路径只加在 context 上。
- 环境变量读取面固定为两处（`env:` locator 与远程 adapter 取凭证值）：仅存在云 URL/token 变量
  不切换后端，也不影响默认本机库。
- 远程 locator 返回类型化 `RemoteStoreNotWired { engine }`，模块文档注明这是 **C-02/C-03 adapter
  落地前的临时状态**（门面照常返回行为结果或 `Unsupported`），不静默回落本机库、不假成功。

验证证据（本机离线）：`cargo test -p peri-resources` 232 lib + 6 集成通过（3 个云探测默认
`#[ignore]`，本轮未实跑云）；`cargo clippy -p peri-resources --all-targets -- -D warnings` exit 0；
`cargo check --workspace --all-targets` exit 0；`cargo fmt -p peri-resources -- --check` exit 0。
新增/强化的回归：访问意图拼写（含拼写错误在 I/O 前失败）、环境变量存在不切云、带凭证 URL 被拒
且不回显、`file://` 不被当本机路径、显式只读不建父目录/库/侧车、只读与普通打开共享同一选择点、
远程 locator 类型化 unsupported 不降级。

明确未做（与 9.3 一致，且不含真实云读写）：远程 SDK 读写路径与 schema 初始化、StoreId 首次竞争、
C-02/C-03 adapter、`MutationGate` 泛化、D-04/D-05（TUI/print/stdio/meta 部署参数、`--db-path` 与
`--session-store` 互斥 grammar、关闭 owner 与协议映射）。

### 9.5 D-04 部署参数迁移（同日第三轮）

把部署面从 `Option<PathBuf>` 迁到 typed 定位描述，各入口不再各自解释存储位置：

- 新增跨层中性类型 `peri_acp_types::session_store::SessionStoreDeployment`（locator 原文、可选引擎名、
  凭证**来源**变量名、`AccessMode`）。它只承载部署事实：不含凭证值、不建连接、不做解析；`Debug`
  仅给形态与「是否配置」，不回显 locator 原文。
- 新增 `Resources::open_deployment(&SessionStoreDeployment)` 作为各入口唯一装配点：进入任何 I/O 前
  经 `SessionStoreOpenRequest::from_deployment` 一次性转成 typed open request（引擎名、locator 形态、
  凭证来源与本机/远程一致性冲突全部在此失败），再进唯一后端选择点。第二轮的
  `Resources::open_locator` 与 `AccessIntent::parse`/`AccessIntentError` 随之删除（部署面无访问意图
  拼写输入，`AccessIntent` 改由 `AccessMode` 单向映射），不留第二入口或死接口。
- CLI：新增 `--session-store` / `--session-store-token-env` / `--session-store-engine`（含 camelCase
  别名），遵循 D1 规则（引擎只由已确认语法或显式选择给出；凭证只表来源、无默认名与别名；
  `env:` 只解引用一次）。`--db-path`/`--dbPath` 保留；两个定位入口互斥由 `validate_cli` 判定并
  返回参数错误（早于任何 I/O），部署参数构造同样拒绝，不设隐式覆盖顺序。meta 的受限 grammar
  同步为只接受这些定位参数与 session 自身的 `--json`。
- 入口接线：TUI `main`/`TuiOptions`/`TuiLaunchOptions`/`App::new`、`-p` print、ACP
  `StdioInput.session_store` → `peri_agent::resources::open_session_resources_deployment`、
  `peri meta session` 早启动路径都传同一份定位描述并只打开一次；`App` 持有门面后 `attach_acp`
  与恢复会话不重新解析存储（恢复 cwd 不改变存储）。`peri_tui::thread` 对消费侧的独立只读
  seam re-export 删除。
- meta 只读路径：UUID 校验 → 只读 deployment → 统一入口；新增公开 `StoreOpenFailure` +
  `classify_open_failure`（按类型化 source chain 分类，不解析错误文本、不回显 locator/凭证），
  meta 新增 `store_not_configured`(exit 2) 与 `store_unavailable`(exit 4)，缺库仍
  `database_not_found`(exit 3)——远程失败不再被统一回报成「数据库不存在」。meta 仍不加载
  provider/MCP/Agent，也不新建本机执行登记。

证据（本机离线；未跑云、未新增 `#[allow]`/`#[ignore]`、未删断言）：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources` | 233 lib + 6 集成 passed / 0 failed（`sessions::open_tests` 17 条） |
| `cargo test -p peri-tui --lib` | 1671 passed / 0 failed |
| `cargo test -p peri-tui --bin peri` | 85 passed / 0 failed |
| `cargo test -p peri-tui --test print_exit` | 9 passed / 0 failed（`--db-path` 兼容未退化） |
| `cargo test -p peri-acp --lib host::stdio` | 17 passed / 0 failed |
| `cargo check --workspace --all-targets`、`cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |

真实二进制端到端（隔离 `HOME` + 临时目录；只读、无网络、无凭证值读取）：缺库 → exit 3
`database_not_found`；`--db-path` 与 `--session-store` 同给 → exit 2 `invalid_argument`；远程 locator
→ exit 4 `store_unavailable`（stderr 不含 locator 原文）；非法 UUID → exit 2 `invalid_session_id`；
四条命令结束后临时目录仍为空（不建目录/库/侧车）。新增回归覆盖：CLI 解析与别名、互斥在 I/O 前、
部署参数归一与 `Debug` 脱敏、各入口（`open_with` / 本机 locator / `local_path`）等价于同一选择点、
meta 的三类错误映射。

明确未做：远程 adapter（C-02/C-03）与远程读写/schema 初始化/StoreId、`MutationGate` 泛化、
D-05（关闭 owner 与 E 的协议映射联调）、任何真实云操作与凭证读取——`--session-store` 指向远程时
仍返回类型化 `RemoteStoreNotWired`，不降级、不假成功。

### 9.7 C0/D1/D2 收尾复核与下一 C 开工清单（同日第四轮）

本轮不加功能面：复核前三轮落盘是否真被消费侧接线，并把下一轮 C 完整 adapter 需要的接口/装配
缺口写清。**远程后端仍不可用**：`--session-store` 指向 Turso locator 时各入口统一返回类型化
`RemoteStoreNotWired`（meta exit 4 `store_unavailable`）；本轮全部离线（未连云、未读凭证值，
云探测 3 条仍默认 `#[ignore]`）。

复核证据（本机，隔离 `HOME`/临时目录；本批未改 ACP 迁移核心，故不重跑其全量回归）：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | 233 passed / 0 failed（3 ignored = 云探测） |
| `cargo test -p peri-resources --test session_resources_contract` | 6 passed / 0 failed |
| `cargo test -p peri-tui --lib` / `--bin peri` | 1671 / 85 passed，0 failed |
| `cargo test -p peri-tui --test print_exit` / `meta_session_cli` / `print_background_exit` | 9 / 19 / 2 passed |
| `cargo test -p peri-acp --lib host::stdio` | 17 passed / 0 failed |
| `cargo test -p peri-agent --lib resources` | 2 passed / 0 failed |
| `cargo check --workspace --all-targets`、`cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |

**下一 C 完整 adapter 所需**（现状事实，不是已完成能力）：

1. 可复用私有基础（`peri-resources/src/sessions/remote/`）：`RemoteEngine` / `RemoteEndpoint`
   （`sdk_url` / `locator_digest` / `host_class`，`Debug` 不含 host/path）、`CredentialSource::env`
   → `SessionStoreCredential`（值不进 `Debug`）、`RemoteConnection::{connect, engine_read_facts,
   bind_roundtrip, close}`（20s 预算由 `tokio::time::timeout` 兜住，SDK 无超时配置）、
   `RemoteFailureClass`（SDK 载荷文本不进领域失败）。已实测证据见 §9.1；**写路径、schema 初始化、
   `op_ledger`/receipt 与恢复收敛全部不存在**（C §5.1 P1–P7 未实测前保持关闭）。
2. **data 口可替换（gate 泛化）**：`MutationGate.data` 仍是具体 `SqliteSessionData`，
   `SessionResourcesImpl::from_local` 是唯一构造点（数据口由 `LocalExecution::data_port()` 派生）。
   远程组合需要 `Arc<dyn SessionDataPort>`（或泛型）与「远程数据面 + 本机执行面」的构造点；门面的
   数据调用已全部只经 `self.gate.data()`，影响面限于 gate 字段、构造与 `data()` 返回类型（另有
   `abandon_initialization` 里 `data.clone()` 的具体类型依赖）。
3. **本机事实与权威数据的端口归属**：`SessionDataPort` 混有两类事实——权威会话数据，与**本机**记录
   （`load_store_registration` 读 `session_store_registrations`；`has_pending_persistence` /
   `recover_persistence` / `drain` 读 `session_lifecycle_commitments`）。远程组合下本机事实必须由
   本机库回答，远程 adapter 不得把它们写到远端；落地前需在 A/B 范围内定一次端口切分（第二个本机口，
   或由组合持有本机 SQLite 数据口转发），不在 D 面临时拼旁路。
4. **预留面未接线**：`SessionStoreOpenRequest::{access, credential_source, input}` 当前无生产调用方
   （`allow(dead_code)` 显式标记，本轮修正了其中「D-04 迁移后使用」的过时注释）；`access()` 与
   `intent().mode()` 等价，接线时应消费掉或删除。`remote/mod.rs` 的模块级
   `#![cfg_attr(not(test), allow(dead_code))]` 同样应在接入时移除，避免继续掩盖未接线范围。
5. **兼容入口归属**：`Resources::open_with(Option<PathBuf>)` 与
   `peri_agent::resources::open_session_resources{,_with}` 在生产已无消费方（调用方只剩测试），
   生产面统一走 `open_deployment`；下一批应归一或删除，不并存两套「路径即存储」语义。
6. **D-05 未做**：关闭 owner 与 E 的协议映射联调未开始；远程只读/写入的真实行为依赖第 2、3 项。

未验证项（不得据此宣称可用）：真实云读写、远程 schema 初始化、StoreId 首次竞争、P1–P7、
远程只读全链路与 `--session-store` 的成功路径。

### 9.8 C-02 部分 + §5.1 机制实测（同日第五轮；写路径仍未面向业务开放）

落盘（`peri-resources/src/sessions/`）：

- `remote/mutation.rs`：私有可变连接——`BEGIN IMMEDIATE` 托管事务批（单请求 all-or-nothing）、
  20s 预算、只读打开拒绝写入、连接已有打开事务时拒绝执行（SDK 会静默加入该事务）；结果三分类
  「已生效 / 确定未生效 / 无法证明」，回滚失败、超时、写忙、网络失败一律判未决。
- `remote/schema.rs`：远程独立 schema——`peri_store_meta` 单行（版本/契约/store 身份）；身份读取
  只有 SELECT；显式初始化在唯一键竞争下产生身份，已存在时读回既有身份，未知版本/契约/形状一律拒绝
  且不做 DDL、不覆盖。
- `remote/ledger.rs`：私有操作账本——资格写是原子批第一条语句、与效果同生共死；终态封闭与资格写
  竞争同一主键；操作 id 与收据只在本模块可见，Debug 脱敏。
- `host_facts.rs`：本机登记与未决锚点从数据端口迁出（`HostLocalFacts`），第二个 adapter 不必也不得
  实现本机事实；`data.rs` 只保留两个 adapter 共同承担的会话行为。
- 测试：`remote/{schema,ledger,mutation}_test.rs`（离线）；`remote/cloud_mutation_test.rs`（5 个显式
  `#[ignore]` 实验，只操作本轮 run 命名空间，结束时用正常 mutation 路径清理并复核已清空）。

实测证据（授权测试库，实跑输出只含计数/布尔/类别）：

| 实验 | 观察 | 对应前置条件 |
| --- | --- | --- |
| 原子批中途约束失败 | `not_applied`，被拒语句=效果重复插入；资格行与两条效果行在新连接上都不存在 | P1 |
| 同一 operation_id 重复调用 | 第二次 `applied_replayed` 且返回**原收据**（收据随机生成，重算值不会相等），第二个效果不存在、原效果在 | P2 |
| 两连接并发同一 operation_id | 恰好一方 `applied`、另一方 `applied_replayed`，效果只出现一次 | P2/P3 |
| 封闭先提交 → 迟到原请求 | 封闭 `closed_never_applied`；迟到原请求不可能生效，其效果不存在 | §5.1(3)(4) |
| 封闭已生效操作 | 返回原收据，原效果不动 | §5.1(4) |
| 新连接读已提交行 / 身份读取 / 初始化幂等 | 新连接读到同一收据；`sqlite_master` 只读身份检查在该引擎可用；重复初始化返回同一 StoreId | P4、C §6 |

仍未证明（不得据此宣称可用）：业务表 schema 与 new/fork/child/compact 等完整行为（C-03）、恢复组合
与未知结果收敛全链路（C-04）、D 装配与 `--session-store` 成功路径；P5（仅静态读源：SDK 0.1.3 无重试/
退避代码，未做故障注入）、P6（超限先拒绝）、P7（收据保留的空间成本）。`remote/mod.rs` 的模块级
`allow(dead_code)` 只表示「实现已落地、消费方未接线」，D 接入时必须删除。

### 9.9 C-03 第一批：远程会话数据 adapter（同日第六轮；写路径仍只由显式云实验驱动）

落盘（`peri-resources/src/sessions/remote/`）：

- `session_schema.rs`：会话事实表与历史表（`peri_sessions` / `peri_session_messages`，
  `message_id` 全局主键、`ordinal` 定序）；不存本机登记/执行 owner/未决锚点，也不存
  `cached_context` 这类派生缓存。DDL 全部 `IF NOT EXISTS`。
- `session_sql.rs`：静态 SQL + 全绑定参数；列投影由宏 `meta_columns!` 一处展开为
  事实/列表两种投影，解码下标与投影同源（离线测试核对列名与下标）。
- `session_codec.rs`：行值 ↔ 领域值编解码——payload 复用 `PersistedPayload` envelope、
  继承区复用 `InheritedContext::to_json/from_json`；形状不符（缺列、负数计数、非法枚举、
  时间戳不可解析、行内 ID 与主键不一致）一律 `Corrupt`，不猜、不默认。
- `session_read.rs` / `session_write.rs`：一致读取在**一个只读事务批**（`BEGIN DEFERRED`）里取
  会话事实行与历史行；写入走「资格先于效果」的托管事务批，操作 id = store 身份 + 语义标签 +
  内容摘要（SHA-256 前 16 位），同一内容重试命中同一 id（重放不产生第二次效果），不同内容
  不会互相冒充。
- `mutation.rs` 增补：`apply_schema`（幂等 DDL 批，无账本资格写）、`read_batch`（只读一致读）、
  `apply_qualified_reporting`（读回语句级受影响行数，用于「恰好一行」后置条件）；`apply_qualified`
  行为不变（C-02 的 5 个云实验回归通过）。
- `session_data.rs`：`SessionDataPort` 实现——已落地 `load_snapshot/load_meta/load_binding/
  load_session_history/load_flags/list_sessions/list_children/list_session_tree/save_new_session/
  save_fork/save_child/update_meta/drain/close`；`append_history`、`apply_compaction`、
  `apply_message_projections`、`rewind_history`、`remove_history_entries`、`delete_tree`、
  `revoke_unpublished_session`、`adopt_legacy_session`、child resume 记录、`recover_persistence`
  一律 `Unsupported`（阶段状态，D 装配激活前需补齐或由门面显式拒绝；`recover_persistence` 需要
  本机未决锚点，属 C-04）。

实测证据（授权测试库，实跑；本轮清理后复核命名空间计数为 0）：

| 实验 | 观察 |
| --- | --- |
| 写—重连读 | 新建/`update_meta`/fork（2 条 payload + 1 条非默认 flag）/child（继承区 + root frozen 原文）在**新连接只读打开**上逐字段读回；fork `message_count=2`；root `cached_context` 为 `None` |
| 列举与树 | `children` 只有直接子会话、`tree` 含根与后代；scoped 分页只列出带历史的会话，条目只带绑定事实（`workspace_root=None`，不虚构本机根目录） |
| 只读与关闭 | 只读打开的写路径返回 `ReadOnlyStore`；`close` 后调用明确失败 |
| 拒绝与重放 | 同 id 不同内容 → `InvalidInput` 且不覆盖已保存的 frozen；同一内容重试 → `Ok` 且只一行；child 的 frozen 非 root 原文 → `InvalidInput` 且不落行；新连接复核行数 1/0 |

已知边界（不得据此宣称远程可用）：云端无本机目录校验与执行 owner，`LegacyConfirmed` 由门面按本机
证据判定；`load_binding` 只回答绑定事实是否完整。首次实测发现并修复的真实缺陷：可写打开原先只在
「身份刚建立」时建会话表，导致身份早于会话表存在的库上写入失败——现改为可写打开一律执行幂等 DDL。
`remote/mod.rs` 的 `allow(dead_code)` 与这些 `Unsupported` 都应随 D 装配消失。

### 9.10 C-03 第二批：历史与生命周期行为补齐（同日第七轮；写路径仍未接线）

落盘（`peri-resources/src/sessions/remote/`）：

- `session_history.rs`（新增）：追加、投影/flags、compact、rewind、精确移除。写入仍是
  「一次端口调用 = 一个托管事务批（资格先于效果）」；**批内守卫**用单行表
  `peri_store_meta(singleton)` 的主键冲突中止整批——`UPDATE` 匹配 0 行不会失败，
  因此「目标不存在」不能靠受影响行数兜住。序号由语句内 `MAX(ordinal)+1` 推进（批内顺序即
  序次序），计数按本机同一规则**重数**，自动标题复用本机 `extract_title` 且仅 `title IS NULL`
  时写。
- `session_lifecycle.rs`（新增）：legacy 接纳（binding 与缺失 frozen 一次成立、已有值不变）、
  未发布撤销（有子会话拒绝，本机同一文案）、删除会话树（子树 id 只读确认 + 同批删除）、
  child resume 认领事实（`agent_status` + 由状态派生的 `claimed`）。远端**不写**墓碑
  `session_lifecycle_commitments` / `creation_intent` / `execution_runs` / `workspace` 登记：
  那些是本机事实，远端没有跨机副本，删除就是删除。
- 领域纯规则复用：`extract_title` 与绑定相对路径校验改为跨模块可见（`pub(crate)` /
  `pub(super)`），远端不复制一份业务规则；未改任何公共契约、未动 `RemoteStoreNotWired`、
  未激活 factory（消费者只有门面与两个 adapter）。
- `recover_persistence` 仍为 `Unsupported`（需本机未决锚点，属 C-04）。

验证证据：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | **277 passed / 0 failed / 10 ignored**，exit 0（上一轮 266/10） |
| `cargo clippy -p peri-resources --all-targets -- -D warnings` | 零告警，exit 0 |
| `cargo check --workspace --all-targets` | exit 0 |
| `rustfmt --config skip_children=true --check`（仅 8 个改动文件） | 干净（注意：`rustfmt` 默认递归子模块，全仓/整文件校验会误报无关文件的既有漂移） |

新增离线断言 11 条：语句占位符数与绑定参数数一致（**测试当场抓到我把复用占位符数错**：
追加语句 `?2` 被主查询与子查询共用 = 7 个、计数刷新 = 3 个）、守卫确实落在单行主键上、
rewind 两边界只差比较符、计数是重数而非自增、标题仅缺失时写、接纳只补 NULL、删除范围整树、
认领标记不由独立列承载。

未完成与风险（不得据此宣称远程可用）：**真实云探测本轮未跑**（本会话无委派工具，预算耗尽），
因此批内守卫在主引擎上的实际中止行为只有离线断言支撑，属**未验证**；`recover_persistence`
未实现；本机执行/登记组合与 D 装配激活（含删 `allow(dead_code)`）仍在下一批。两处**有意的偏离**
已记录在代码注释里：目标行不存在时远端明确失败（本机若干路径对 0 行更新返回成功）、
recursive-CTE 删除范围改为「先只读取子树 id 再同批删除」（避免在 `DELETE` 里依赖 CTE 求值）。

### 9.11 C1–C3 数据 adapter 的诚实失败缺口 + 真引擎行为契约（同日第八轮；写路径仍未接线）

本轮两件事：补齐「读不完整被当成缺失」这一类缺口，并在**真引擎**上跑 C-03 的行为契约
（9.10 自陈的最大证据缺口）。

落盘（均在 `peri-resources/src/sessions/remote/`）：

- `mutation.rs`：读取出口统一校验**结果集数量 == 请求语句数**（`ensure_result_sets`），
  新增 `read_pair`（两段结果的一致读取）与 `sole_row`（主键查询至多一行，多行即错误）。
  分类为 `Internal`：不是 `NotFound`（会把不完整回复伪装成「没有这一行」）、不是 `Corrupt`
  （存储没坏）、不是 `Unsupported`（能力在）。
- `session_read.rs`：快照与历史读取改走 `read_pair` + `sole_row`，去掉
  `batches.next().unwrap_or_default()`——原来少一个结果集会被读成「这个会话没有历史」。
- `session_lifecycle.rs`：`delete_tree` 不再「先 `exists` 再用默认值兜住子树读取」——空子树
  等价于根不存在 ⇒ `NotFound`，不再出现「什么都没删却返回 `Ok`」；`revoke_unpublished_session`
  的子会话计数只有**明确读到 0** 才继续（`revocation_gate`），读不出来即拒绝，不用不可证明的
  证据做破坏性决定。两个纯函数 `tree_ids` / `revocation_gate` 带离线断言。
- `session_history.rs`：**云端实验当场抓到并修复两个真实缺陷**——操作身份摘要缺项，导致不同
  操作撞同一个 `operation_id`、第二次被当成重放**静默跳过**：(a) rewind 方向未进摘要
  （同一边界上的 `KeepThrough` 与 `RemoveFrom` 撞车）；(b) 追加与 compact 只按内容摘要
  （同内容的两批追加、第二次同正文 compact 都被静默丢掉）。摘要现含方向与消息 id
  （`entry_inputs`），并有「同内容不同 id 必须是不同身份」的离线断言。
- `remote/mod.rs`：模块状态段与实现对齐（只剩 `recover_persistence` 未实现；
  `allow(dead_code)` 与 `Unsupported` 明确标注为**临时状态**，装配前必须补齐或由门面拒绝）。
- 测试结构：本轮 run 命名空间的共建夹具（清理/计数/合成输入）上移到 `cloud_tests`，
  新增 `cloud_history_test.rs` 与 `cloud_lifecycle_test.rs`（都在 700 行内）。

验证证据：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | **282 passed / 0 failed / 13 ignored**，exit 0（上一轮 277/10） |
| `cargo clippy -p peri-resources --all-targets -- -D warnings` | 零告警，exit 0 |
| `cargo check --workspace --all-targets` | exit 0 |
| `cargo test -p peri-resources --doc` | exit 0 |
| `cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1`（已授权测试库） | **13 passed / 0 failed**（44.5s） |

新增云实验断言（每条都读回可观察结果，不靠请求成功推断）：追加的顺序/计数重数/`title IS NULL`
时补自动标题；批内重复 id 与撞已有行主键都被拒绝，且**撞主键的那一批一条都没落**；投影与
compact 的 flags 定向生效、第二次同正文 compact 仍然落地；rewind 两边界按 `ordinal` 生效、
未知边界无变更、二次移除幂等、跨会话移除被拒；child resume 认领由状态派生、legacy 接纳
已有值不变且错误前置条件照实拒绝、删树移除整棵子树且二次删树 `NotFound`、有子会话的撤销
被拒且一行都不删；**批内守卫在真引擎上的两条分支**——谓词成立 → 整批回滚（效果与资格写都不留），
谓词不成立 → 守卫一行都不插、批照常提交。所有实验结束都在新连接上复核本轮命名空间计数为 0。

未完成（不得据此宣称远程可用）：`recover_persistence` 仍 `Unsupported`（C-04）；本机执行/登记
组合与未决锚点；D 装配——`Resources::open_deployment` 对 remote 仍返回 `RemoteStoreNotWired`、
`allow(dead_code)` 未删；`MutationGate` 仍持有具体 `SqliteSessionData`/`LocalExecution`。
本轮云实验覆盖的是**行为契约**，未做取消/超时/网络中断一类故障注入（P5/P6/P7 仍未实测）。
安全：云实验只输出计数/布尔/领域类别，只清理本轮 run 命名空间并复核为 0，未读 `.env` 内容
（selector 只传用户给出的键名）。

### 9.12 C-04 第一批：操作身份修正与本机操作日志（同日第九轮；远程写路径仍未接线）

本轮修的是**身份模型**，不是再加一层壳：操作 id 由「store + 标签 + 内容摘要」派生时，
「同内容的后一次领域调用」会撞上前一次的 id → 被当成历史重放**静默丢掉效果**
（状态 A→B→A、标题 x→y→x、同边界 rewind 后追加再 rewind 都命中）。现在：

- **每次领域调用铸造一个唯一操作 id**（`OperationId::mint`，形状 `{thread}.{uuid}`），
  内容摘要不参与身份生成，只做一致性校验（`remote/ledger.rs`）。
- **发送前本机落盘**：新增本机表 `session_remote_operations`（schema v7 → v8，
  `sqlite_store/remote_operations.rs`），记录 id/store/thread/root/行为/摘要/终态；
  登记失败即**不发送**。表无外键：远端会话行不在时记录仍可查（9.11 的缺口）。
  选择独立表而不是 `session_lifecycle_commitments`：后者按 `thread_id` 一行，装不下
  「每次调用一个 id、重启后逐个可查」（该表仍作为本机写入锚点，未决判定把两者并集）。
- **确定终态才结清**：`Applied`/`NotApplied`/`ClosedNeverApplied` → `applied`/`never_applied`；
  `Unknown` 保持 `pending`，门禁继续阻塞同根写入（§5.1 第 6 步）。
- **恢复**：`recover_persistence` 不再 `Unsupported`——按本机日志逐条向远端账本求证；
  账本缺行时用**同一 id** 做终态封闭竞争（封闭先提交 ⇒ 此后不可能再生效；对方已提交 ⇒
  读回原收据）；账本行不可解释或封闭未确认则保持未结清。
- **摘要在重放上真正起作用**：资格冲突读回的行摘要与本次身份不一致 → 不是重放，
  按确定未生效拒绝（`remote/mutation.rs` 的 `resolve_after_conflict`），不再把别人的效果
  认成自己的。新建/fork/child 的摘要输入补齐 binding 与 metadata（`session_inputs`），
  不靠「同内容恰好在别处撞车」掩盖身份错误。

验证证据：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | **290 passed / 0 failed / 14 ignored**，exit 0（上一轮 282/13） |
| `cargo clippy -p peri-resources --all-targets -- -D warnings` | 零告警，exit 0 |
| `cargo check --workspace --all-targets` | exit 0 |
| `cargo test -p peri-resources --doc` | exit 0 |
| `cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1`（已授权测试库） | **14 passed / 0 failed**（48.3s） |

新增/更新的关键实验：

- `cloud_identity_test.rs`（新）：状态 A→B→A→B 落到 B、标题 x→y→x→y 落到 y、
  同 boundary rewind ⇒ 追加 ⇒ 再 rewind 只剩一条、本轮会话账本行数 == 领域调用数（11）、
  本机日志全部结清且 `recover_persistence` 返回 `Recovered`。全部断言走新连接读回。
- `remote_operations_test.rs`（新，离线）：发送前登记→未结清阻塞→结清放行、记录在结清后
  仍可查、按 root 阻塞整棵树（远端会话靠日志里的 thread→root 关联）、同 id 异内容登记被拒、
  结清单调（同终态幂等/异终态矛盾/`pending` 不是终态）、状态取值不认识即损坏、
  v7 库只读打开查询缺表不报错且只读不升级。
- `remote/session_write.rs` 单元测试：同输入三次调用得到三个不同 id、同输入同摘要、
  新建摘要覆盖 binding 与 metadata。
- 语义更新（`cloud_session_test.rs` 实验二）：操作身份不再由内容派生后，「同内容再保存」
  是**新的领域调用**，照实撞会话主键 → 确定未生效（`InvalidInput`），且不改动已有行；
  重放语义只覆盖同一次操作（由本机日志的原 id 与封闭竞争承担）。

未完成（不得据此宣称远程可用）：本机执行/登记组合（adapter 与执行面尚未在同一门面上组合，
远程会话的 owner/dirty/准入仍缺）、D 装配（`RemoteStoreNotWired` 未替换、`allow(dead_code)`
未删、`MutationGate` 仍持有具体 `SqliteSessionData`/`LocalExecution`）；恢复路径未做故障
注入实测（发送前崩溃、响应丢失、取消、迟到写仍是设计结论，交互式验证留给下一批）；
`data_port()` 目前只有云实验夹具消费。安全：本机日志写在系统临时目录、只含 id/摘要/终态，
不含 payload/frozen/凭证；云实验只清理本轮 run 命名空间并复核为 0，未读 `.env` 内容。

### 9.13 C-04 第二批 + D 装配激活：真实门面组合与接纳裁决（同日第十轮）

本轮把「数据在远端、本机管准入」从两套实现收敛成**一个门面 + 三个端口**，并接上 D 装配。
不是加壳：`RemoteStoreNotWired` 已删除，`open_deployment` 的远程 locator 现在真的装配。

- **端口化**：`MutationGate` 持 `Arc<dyn SessionDataPort>`（canonical 数据）、
  `Arc<dyn LocalExecutionPort>`（本机证据/owner/代际/锁）、`Arc<dyn HostLocalFacts>`
  （未决锚点、远端操作日志、存储登记）。`lease` 只出现在执行端口，数据端口没有它；
  本机 `LocalExecution` 仍是同一份 SQLite 实现（本地塌缩不变），远程是本机执行面的第二个
  实现，公开行为仍只有门面的那 33 条。
- **接纳链**（B §6.1 第 3–5 环）：`StoreId`（远端权威身份）→ 本机安装身份 → 登记 →
  binding 复核 → root owner。按 StoreId 查登记（别名同域、URL 文本不进锁主键）；
  「远端已有数据 + 本机无登记」⇒ 读取可用、执行与写入按 `WorkspaceError::StoreNotRegistered`
  拒绝，**不自动登记**；来源（引擎/locator 摘要）不一致 ⇒ 同样拒绝且不改写登记；
  首次登记只在「远端由本次打开初始化 + 写打开」时发生。只读打开连本机 schema 都不动
  （缺库时按 `NoLocalRegistry` 如实回答「没有记录」，不建文件）。
- **root 归属**：远端会话在本机没有 `threads` 行，执行代际与 sidecar 锁**按 root** 归属
  （`admit_remote_root`/`acquire_remote_root`/`root_{write,exclusive}_guard`），
  不为使用旧 SQLite lease 造假历史行；子会话的执行权属于 root（与本地语义一致）。
  远端绑定与本机 workspace 证据走同一套判定（`validate_binding_value`），
  `LegacyConfirmed` 在远程首期恒不成立。
- **创建/删除/关闭**：远程 `create_session` = 远端 durable 保存 → 本机准入，准入失败按
  `saved_but_not_admitted` 上报（数据自描述：行在、代际无，重试走 `admit_existing` 收敛）；
  删除在发送远端删除**之前**落本机墓碑锚点（`anchor_deletion`）；`close` 按活 owner 有界排空。

验证证据：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | **294 passed / 0 failed / 15 ignored**，exit 0（上一轮 290/14） |
| `cargo clippy -p peri-resources --all-targets -- -D warnings` | 零告警，exit 0 |
| `cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1 cloud_`（已授权测试库） | **15 passed / 0 failed**（64.2s；含本轮新增 1 项） |

新增关键实验与测试：

- `cloud_admission_test.rs`（新，显式云）：真门面组合上 `create_session` 成功并给出活 owner
  （远端保存 + 本机代际两步接线成立）、同一 root 在结清后回到 `Available`、
  `append_history`/`load_session_history` 走远端数据、`close` 排空；
  **另一份全新安装**打开同一 store：历史可读、`execution == NoLocalRegistration`、
  mutation 报 `StoreNotRegistered`；两份安装的本机登记库都在系统临时目录。
- `registration_test.rs`（新，离线）：已有数据不自动登记、只读打开不写登记、
  空库首次登记一次且复用同一安装身份、来源不一致拒绝且不改写登记。
- `open_test.rs`：远程 locator 的配置失败在任何 I/O 之前发生（缺凭证 ⇒ 类型化
  `CredentialError`，不再有「未落地」这个临时结论）。

未完成（不得据此宣称远程可交付）：故障注入实测（发送前崩溃、响应丢失、取消、迟到写仍是
设计结论）；`recover_persistence` 之外的崩溃恢复演练；E 全链路消费侧接入。
安全：只读 `.env` 由测试进程的安全 parser 完成（只取两个显式键名的值，不 `source`/`eval`、
不回显），云实验只清理本轮 run 命名空间并复核为 0，本机登记库/日志都在系统临时目录。

### 9.14 C-04 第三批：未决收敛接线、统一门禁与故障注入实测（同日第十一轮）

本轮把 `recover_session_persistence` 从「已实现的 adapter 行为」接成**门面级可用的收敛路径**，
并把故障从设计结论变成注入到真实批上的实测。修掉两个真实缺陷：

- **未决阻塞范围按后端解析**（新 `LocalExecutionPort::pending_scope`）：远程会话在本机没有
  `threads` 行，之前用本机父链解析未决范围会得到「没有根」，于是 root 有未结清写时同根
  **子会话的首次写入**漏过门禁（该子会话在本机日志里也还没有自己的记录，两条回退都落空）。
  门面现在按「传入 id ∪ 该 id 的 root」两问取并集；本机组合的答案仍是本机父链（行为不变），
  远程组合的答案来自远端父链（父关系创建后不变，带缓存）。
- **`mark_clean` 统一门禁**：宣告 `clean = 1` 之前读同一张 `session_remote_operations`、
  同一条未结清谓词（`has_pending_operations_on`，与 `has_pending_persistence` 共用），
  未结清时按 `WorkspaceError::RecoveryRequired` 拒绝且不关闭准入——「clean」的语义是
  「这条 root 没有无法证明终态的效果」。本机组合从不写该表，这一步是恒真的空查询。
- **恢复入口不需要令牌**：`recover_session_persistence(id)` 先等本进程在途写入结束（有界，
  超时按 `Timeout` 上报）再让数据面判定；`drain` 与写入门禁同样按 root 判定。恢复**不消费**
  调用方任何 operation id：身份在发送前已落进本机日志，恢复按日志里的原 id 还原。

故障注入面（`FaultPlan`，仅测试构建，生产构建没有这些字段）落在真实批上，不是替代返回值：

- `drop_reply`：批**照常提交**（真实事务、真实账本行），结果按未决上报 ⇒ 响应丢失 /
  取消后仍提交 / 已提交但本机结清失败 / 进程在结清前结束的等价物；
- `drop_before_send`：批在发出前消失，远端什么都没有 ⇒ 发送前崩溃的等价物。

验证证据：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | **298 passed / 0 failed / 20 ignored**，exit 0（上一轮 294/15；ignored 全部是显式云实验） |
| `cargo clippy -p peri-resources --all-targets -- -D warnings` | 零告警，exit 0 |
| `cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1 cloud_`（已授权测试库） | **20 passed / 0 failed**（92.9s，含本轮新增 4 项：`cloud_recovery_test.rs` 3 项 + `cloud_local_fault_test.rs` 1 项） |

新增关键测试：

- `remote/cloud_recovery_test.rs`（新，显式云）：① 响应丢失 → 本机日志停在 pending、
  效果**真的在远端**（新连接读回）、恢复判「已生效」并结清，之后写入恢复；② 发出前消失 →
  恢复用**同一唯一键**封闭成「从未生效」，随后**迟到原请求**（同一 id + 同一摘要 + 真实效果）
  到达 → 整批回滚、`ClosedNeverApplied`、效果为零、账本仍是封闭态；③ 真门面：root 有未结清写
  （记录由**另一份安装**的子会话构成，本机登记库里没有它的记录）→ 同根子会话首次写入被拒 →
  恢复（不需要令牌）→ 放行；④ 重启 + 目标已删：同一份本机日志的**新打开**（句柄落下 = 进程
  状态结束）＋ 另一份安装删掉会话 → 恢复判「已生效」（依据是账本行，不是「目标读不到」）、
  未决集合清空、目标仍 `NotFound` 且**不被重建**。
- `remote/cloud_local_fault_test.rs`（新，显式云）：故障注入在**本机事实端口**（真实
  `SqliteSessionData` 的装饰器，不是替代返回值）。`record_remote_operation` 失败 ⇒ 整次
  mutation 中止、错误**不是**未决、远端零效果、无需收敛；`settle_remote_operation` 失败
  （批已提交）⇒ 本机保守保留阻塞、效果真的在远端，恢复判「已生效」并放行。
- `resources_test.rs`（离线，真实门面 + 真实 SQLite）：未结清远端操作阻塞同根**全部入口**
  （写入 root/子会话、fork 来源、删除、`acquire_execution`、`reset_dirty_execution`、
  `drain_persistence`、`close`、`mark_clean`）；ordinary dirty 的显式解除**不解除**远程未决
  （拒绝后仍是 dirty，未结清记录原封不动）；未决事实跨进程可见（同一条本机库另一次打开仍然
  阻塞，本机组合如实报 `StillBlocked` 而不是假装收敛）。

未完成（不得据此宣称远程可交付）：删除墓碑在恢复后的收尾（远端删除已生效时本机墓碑停在
`deleting`；它不参与未决判定，各条查询都把 `deleting`/`deleted` 同等看待，identity 仍是终态，
但记录里少了 `deleted` 这一步）；真实进程 kill 演练（上面第 ④ 项用「同一份日志的新打开」
作重启等价物，没有真的杀进程）；E 全链路消费侧接入。
安全：`.env` 仍只由测试进程的安全 parser 读取（只取两个显式键名的值），云实验只操作并清理
本轮 run 命名空间，本机日志/登记库都在系统临时目录，未上传真实历史或仓库内容。

### 9.15 C-05 第一批：部署入口端到端（跨进程冷恢复）与残留标记清理（同日第十二轮）

前面几轮分别打的是 adapter 行为、门面接纳、故障收敛，没有任何一条实验从**部署入口**
（`Resources::open_deployment`，CLI/TUI/print/stdio/meta 共用的那一个装配点）走完整个生命周期。
本轮补上，并把「已落地、消费方未接入」的残留标记清掉。

新增两个文件：`remote/cloud_deployment_test.rs`（父进程：提供合成环境、拉起子进程、用**新连接**
在阶段之间核对远端事实）与 `remote/cloud_deployment_child_test.rs`（三个子进程阶段；没有标记
变量时直接返回）。每一段都在**独立进程**里跑：进程边界消失之后仍然成立的事实才是 durable 事实。

| 阶段 | 进程 | 实测结果 |
| --- | --- | --- |
| 写入 | 子进程 A（temp HOME + 合成 git workspace） | 创建 → 追加 2 条 → `drain_persistence` → compact（标记 + 摘要，读回 3 行）→ fork → child → 标题 A→B→A → `close`；远端 **3 会话 / 6 消息 / 8 账本行** |
| 冷恢复 | 子进程 B（同一 HOME，**新进程**） | `recover_session_persistence` 判 `Recovered` → 上一个进程未写 clean 留下的 ordinary dirty → 显式风险接受解除 → `acquire_execution`（全量绑定复核）→ rewind（3 → 1 行）→ 删树（root 树消失、fork 树仍在：**1 会话 / 3 消息**）→ `mark_clean` |
| 只读（全新 HOME） | 子进程 C | 远端历史可读（3 行）；执行准入 `NoLocalRegistration`；写入拒绝 `StoreNotRegistered`；**HOME 一个文件都不建** |
| 只读（已登记 HOME） | 子进程 D | 只读打开不改本机状态（目录快照前后一致）；写入拒绝 `ReadOnlyStore` |

本轮证到的事：

- **标题 A→B→A 经真实入口落到 `title-a`**：第三次领域调用不被当成历史重放（R1 的身份修正
  在完整装配路径上成立，不只是 adapter 级实验）。
- **fork 复用 source 的 `message_id` 被明确拒绝**（`InvalidInput`：「remote constraint violation」），
  不是静默丢行；产品路径的重映射语义（`peri-acp::dispatch::session_fork` 的纯 ID 重映射）因此
  第一次有了回归。本轮实验第一版就是按「复制原 id」写的，云端当场报约束冲突——adapter 的
  诚实失败是对的，改的是实验。
- **只读零副作用**：两个只读阶段之后，会话/消息行数与账本行数与阶段前**完全一致**（父进程用
  新连接在前后各核对一次），本机目录快照也一致。

顺带清理（不留「永不可用」的假完成面）：

- `SessionStoreOpenRequest::{access,credential_source,mode}` 的 `allow(dead_code)` 删除——三个
  入口现在都有生产调用方（远程装配）。
- `SqliteThreadStore::data_port` 生产无消费方（门面与远程组合都直接持 `LocalExecution`，
  同一实现的两个句柄），**删除**；两处测试夹具改用 `LocalExecution`（`close` 由数据面端口的
  `close` 承担，同一动作）。
- `open_test.rs` 里「数据 adapter 尚未落地」的注释改为当前事实（断言本身仍有效：配置不完整
  必须在任何 I/O 之前失败）。
- `remote/mod.rs` 的模块级 `allow(dead_code)` 保留，但按实测重写：非测试构建里的未使用项只有
  「显式 cloud 探测/回环面」与「逐条断言的 SQL 片段常量」两类，并写明新增未使用项必须属于这两类。

验证证据：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | **301 passed / 0 failed / 21 ignored**，exit 0（上一轮 298/20；新增 3 个子进程阶段测试 + 1 个显式云父测试） |
| `cargo clippy -p peri-resources --all-targets -- -D warnings` | 零告警，exit 0 |
| `cargo check --workspace --all-targets` | exit 0 |
| `cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1 cloud_`（已授权测试库） | **21 passed / 0 failed**（116.0s；新增 1 项端到端 15.2s，其余为上几轮的 adapter/门面/故障实验） |

未完成（不得据此宣称远程可交付）：删除墓碑在恢复后的收尾（同 §9.14）；真实进程 kill 演练
（本轮的「新进程」是真进程，但上一进程是正常退出，不是被 kill）；E 全链路消费侧接入。
安全：`.env` 仍只由测试进程的安全 parser 读取（两个显式键名 + 绝对路径），云实验只操作并清理
本轮 run 命名空间（复核为 0），合成 workspace/HOME/本机登记库都在系统临时目录，未上传真实
历史或仓库内容。

### 9.16 C-05 第二批：真进程强杀、P5–P7 实测与锚点收尾（同日第十三轮）

第十二轮列出的未完项里有两件这一轮落到证据上（第三件是 E 消费侧，不在 C 范围）：

1. **真进程强杀演练**：`SIGKILL` 结束正在写远端的进程，同一份本机库的新进程收敛。
2. **P5/P6/P7 从「未实测」变成有观测**：SDK 重试行为的静态审计 + 取消在途调用、超大单批、
   收据保留与空间成本三条实验。
3. **远程删除锚点收尾**：`anchor_deletion` 之后补上 `finalize_deletion`（删除确认 → 墓碑收终态）。

顺带**发现一处真实缺陷**（本批新证据，未修）：放弃一个**已经发出**的在途请求之后，同一个 store
实例的连接不再可用。见本节末尾「本轮发现的连接生命周期缺口」。

#### 强杀演练怎么保证「死点是确定的、死法是真实的」

新增 `remote/cloud_kill_test.rs`（父进程）与 `remote/cloud_kill_child_test.rs`（子进程阶段），
只在 Unix 编译（`kill -9` 与信号语义）。装配仍是同一条路径：`open_remote` 拆成
「本机事实建立（`open_local_facts`）+ 远端打开与装配（`assemble_remote`）」，测试用
`open_remote_with_facts` 只替换本机事实端口这一层（`#[cfg(test)]` 缝，见 `composition.rs`），
远端 adapter、本机执行面、门禁、门面都是生产实现。

- 子进程在注入点写出 `PROBE KILL_READY` 后**停住不再返回**（本机事实端口的两个委托点：
  登记之后、结清之前）；
- 父进程读到标记后执行 `kill -9 <pid>`，并断言子进程**以信号 9 结束**（不是正常退出）；
- 两个注入点各自跑一次完整闭环：写入 → 强杀 → 父进程用新连接核对远端 → 同一 HOME 的新进程收敛
  → 父进程再用本机登记库核对 durable 事实。

| 注入点（死点） | 死之前的 durable 事实 | 实测收敛 | 证据 |
| --- | --- | --- | --- |
| 本机登记已落盘、远端请求**未发出** | 本机日志 1 条 `pending`，远端账本无此操作 | 收敛为「从未生效」：远端历史仍是 1 条（被杀那次写入永远不出现），新调用落地（2 条），被杀的写入出现 **0** 次 | `cloud_kill_before_send_converges_to_never_applied` |
| 远端批**已提交**、本机**未结清** | 本机日志 1 条 `pending`，远端账本已有收据 | 收敛为「已生效」：远端历史 2 条，新调用落地（3 条），被杀那次恰好 **1** 份（不重复、不撤销） | `cloud_kill_before_settle_converges_to_applied_once` |

父进程独立核对的本机 durable 事实（不经过门面，直接读本机登记库）：收敛后
`session_remote_operations` 里**没有任何 `pending` 行**，且阶段二删除整棵树之后该 root 的墓碑
`state = 'deleted'`。

#### 远程删除锚点的收尾（本批新代码）

`LocalExecutionPort` 增加 `finalize_deletion`：本机组合是空操作（数据面 `delete_tree` 在提交后
同一动作里收尾，`sqlite_store/session_data.rs`），远程组合把本机墓碑从 `deleting` 收到终态
（`LocalExecution::finalize_deletion` → `finalize_tombstones`）。门面在**删除已生效之后**调用它，
失败只记录不翻转结论——删除不可逆，而所有消费方（`mark_clean` 的记录缺失容忍、执行行缺失容忍、
identity 终态判定）都把 `deleting` 与 `deleted` 同等看待，缺的只是收尾标记。上表最后一行就是
这条路径的端到端证据（真实装配路径删除 → 父进程用本机库新连接读到 `deleted`）。

#### C §5.1 P5/P6/P7 实测

新增 `remote/cloud_limit_test.rs`（四个实验，全部经真实门面）。判据刻意分成两条：**权威判据是
新连接的原始计数**（不依赖 adapter 的读取预算），adapter 读回只作附加一致性检查——这样
「整批生效或零部分结果」不会被读路径的预算问题掩盖。

| 前置条件 | 观测方式 | 实测 |
| --- | --- | --- |
| **P5** SDK 不自动重试 mutating 请求 | 读 SDK 源码 + 我方请求路径 | `turso_serverless` 0.1.3 全部源码里**没有** retry/backoff/sleep 逻辑（只有关于嵌套事务与 HTTP 失败的错误文档）；我方 `connection.rs` 是单次调用 + 20s 预算（`REQUEST_BUDGET`），预算超时归 `Exceeded`→`Timeout`，**不推断**远端是否生效。因此同一次发送不会有第二个请求；重试只可能是调用方重新发起，而那是**新的领域调用**（新操作 id，R1 的身份语义） |
| **P5** 放弃在途调用后仍有 durable anchor | 直接 drop 调用 future（400ms 预算），再核对本机日志 | 两次运行各自落在合法的一侧，实验对两侧都断言：**① 已过发送前登记**（`anchor_written=true`、`local_append_records=2`）⇒ 那次写入最终**恰好一份**（`rows=2`，请求在被 drop 之前已到达并提交），后续调用再落 1 行；**② 没来得及登记**（`anchor_written=false`、`local_append_records=1`）⇒ 那次写入**从不出现**（`rows=1`），后续调用再落 1 行。两侧共同点：`local_pending=0`，取消从不产生第二份效果 |
| **P6** 超大单批输入先拒绝或整批生效，无部分结果 | 单条 256 KiB 与 1 MiB 消息正文各一次 `append_history`，每次用新连接的原始计数核对 | 两档都**整批生效**（`class=applied`；256 KiB 写入 720–1384 ms、1 MiB 写入 1382–2520 ms，两次运行的实测区间），256 KiB 档 adapter 读回与新连接计数一致（1 行）。**单请求上限没有被定位**：本批没有分块实现，没有触发「先拒绝」分支 |
| **P7** 收据不按 TTL 清理，记录空间成本 | 本轮收据条数与列字节数（`COUNT(*)` + `SUM(LENGTH(各列))`）在收敛与重读之后重算 | 收敛与重读前后**逐条不变**（2 行 / 454 字节，=`create_session` + 一次 `append_history`）；没有任何清理路径删收据。成本口径：每行几十到几百字节，只含 id/kind/摘要/终态/原收据/时间戳，**不含 payload** |

另外补上一条此前只被文档化的拒绝路径：远程 `create_session` 收到带父会话的输入时返回
`InvalidInput`，且**远端零行**（该 id 不存在、本轮会话数仍为 1）。

#### 本轮发现的连接生命周期缺口（未修，下一批第一件）

取消实验把两件事分开记录：① 同一次打开上尝试收敛；② 换一次打开（新连接、新 owner）收敛。

- 当取消切在**请求已经发出去之后**（实测那一次：`anchor_written=true`），① 的结果是
  `Unavailable { detail: "remote session store transport" }`：**同一个 store 实例的连接不再可用**，
  该实例上后续任何请求都在传输层失败；
- 当取消切在**发送之前**（实测另一次：`anchor_written=false`），① 直接返回 `Recovered`
  ——没有任何请求在途，也就没有可坏的连接；
- ② 在**新打开**上两种情况都正常（收敛为 `Recovered`、行数 0/1、无未决、后续调用恰好 +1）。

同样现象在超大写入实验里独立复现：累计历史 ~1 MiB 时 adapter 的整段读回撞上 20s 预算
（`Timeout`，请求被 drop），紧接着的普通调用也是传输层 `Unavailable`。

成因在我方 adapter 的连接缓存：`RemoteSessionData` 把 `RemoteStore`（内含 SDK `Connection`）
缓存在 `RwLock<Option<RemoteStore>>` 里，**失败后不重建**；SDK 公开面也没有客户端超时配置。
影响：一次超时或一次取消会让该进程内的远程操作全部以传输层失败上报，直到重开 store——
**不假恢复**（不会谎称成功），但可用性上是真缺口。修法（下一批）需要 adapter 在传输类失败后
标记连接失效并在下次请求重建，这会要求组合层把凭证保留到 adapter 内（或把连接交给组合层
管理），因此是设计选择，不在本批硬塞。

#### 验证证据

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | **303 passed / 0 failed / 27 ignored**（上轮 301/21：新增 2 个强杀父测试 + 4 个边界实验） |
| `cargo test -p peri-resources --all-targets` | 303 passed / 0 failed（含 6 个独立目标测试） |
| `cargo clippy -p peri-resources --all-targets -- -D warnings` | 零告警 |
| `cargo check --workspace --all-targets` / `cargo test --workspace --doc` | exit 0 / exit 0 |
| `cargo test -p peri-acp --lib`（消费侧恢复链路） | 717 passed / **1 failed**：`host::prepared_tests::new_session_persists_prepared_frozen_bytes_once`（断言是「持久化的 frozen 字节必须与准备输入同源」）。**该模块单独运行 6 passed**，失败只在全量并行跑时出现；文件属他轮未跟踪 WIP（`peri-acp/src/host/prepared{,_test}.rs`），本轮未改 peri-acp 任何文件，判定为隔离/竞态问题，留给 Fable 用同一棵树核对是否与既有状态相关 |
| `cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1 cloud_`（已授权测试库） | **27 passed / 0 failed**（225.0s；新增 2 个强杀演练 + 4 个边界实验，其余为上几轮实验） |

#### 本批实测命令（可照抄复跑）

```bash
# 离线：策略与行为（默认 SQLite 组合完全不变）
cargo test -p peri-resources --lib
cargo test -p peri-resources --all-targets
cargo clippy -p peri-resources --all-targets -- -D warnings
cargo check --workspace --all-targets && cargo test --workspace --doc

# 消费侧（恢复链路，门面类型化分类经 ACP 派发）
cargo test -p peri-acp --lib

# 显式云（授权测试库；键名由选择器给出，值只在测试进程内解析）
PERI_CLOUD_ENV_FILE=<绝对路径>/.env PERI_CLOUD_URL_KEY=<url 键名> PERI_CLOUD_TOKEN_KEY=<token 键名> \
  cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1 cloud_
```

#### Fable 应验证的实际效果（不要只看结论）

1. **强杀演练真的杀在注入点**：跑 `cloud_kill_` 两个实验，确认子进程以**信号 9**结束、父进程
   读到过 `KILL_READY`；把 `KillSwitch::stop_until_killed` 改成直接返回，实验应当失败。
2. **收敛不是「读不到就算没发生」**：把 `recover_persistence` 的账本求证短路成「无行即
   never_applied」，`before-settle` 实验会失败（那次操作**有**收据且已提交）。
3. **同一 RPC 重试 vs 新领域调用**：`OperationId::mint` 每次调用铸 id、摘要只做一致性校验；
   A→B→A（状态/标题/rewind 边界）覆盖在 `cloud_identity_test.rs` 与部署端到端（标题 A→B→A
   落到 A）两处，`OperationId::scoped` 仅 `#[cfg(test)]`。审阅时确认生产路径里没有第二处把
   内容拼进身份。
4. **锚点收尾真的写了**：把 `finalize_deletion` 的远程实现改回 `Ok(())`，强杀父测试读到的墓碑
   应当停在 `deleting`（这条断言是本批新增的）。
5. **连接缺口是真缺口**：跑 `cloud_cancelled_write_keeps_a_durable_anchor`，看
   `same_instance_recover=` 那一行是不是传输层失败；再确认它没有被我改写成「换个实例就当通过」
   ——实验的结论只来自第二段（新打开），第一段是**如实记录**。
6. **默认 SQLite 未受影响**：`cargo test -p peri-resources --lib` 全绿即本地塌缩语义未变
   （本批对本地只加了一个空操作端口方法与一个 `StoreAccess::of`）。

#### 尚未证明（不要当成已完成）

- **P6 的「先拒绝」分支**：单请求上限没有定位；实测只在 256 KiB / 1 MiB 下证明了 all-or-nothing。
  累计历史 ~1 MiB 时 adapter 的整段读回超出 20s 单请求预算（本窗口实测，不当作成功）。
- **连接生命周期缺口未修**（见上）：取消/超时之后同进程内的远程操作不可用，必须重开 store。
- **迟到写与取消的组合**：端到端证据来自 R3 的 adapter 级实验（原始请求整批回滚的零效果断言），
  本批补的是门面级取消 + 本机 anchor。
- **强杀时序抖动**：注入点确定，但「同一请求两次强杀之间」的时序组合没有穷举。
- **E 全链路消费侧**：CLI/TUI/print/stdio/meta 仍只把 `SessionStoreDeployment` 交给同一个门面，
  没有跨进程的消费侧端到端（本轮强杀演练走 `open_remote`，部署入口端到端在 §9.15）。
- **删除墓碑收尾的故障分支**：`finalize_deletion` 失败时只记录不翻转，这条容忍路径没有故障注入
  实测（缺收尾时消费方同等看待是代码事实，不是实测结论）。

### 9.17 Fable P1 关闭：跨 store 结清与封闭证据保留（同日第十四轮）

Fable 独立复现判 FAIL 的两条 P1 已修并给出可复跑证据。**其余 finding 维持 FAIL**（见本节末）。

#### 1) 跨 store 结清（P1）

- **缺陷**：`RemoteSessionData::recover_persistence` 用 `anchors.pending_remote_operations(root)`
  取「本 root 全部未结清」，而 SQL 只按 thread/root 过滤、**未限定 store_id**；同一个 HOME 里
  store A 的未结清记录会被 store B 的恢复当成自己的，B 据此向**自己的**远端账本做终态封闭，
  把 A 的记录结清成 `never_applied`。
- **修法**（都在内部，公共接口不暴露后端机制）：未结清谓词以 `store_id` 为第一项，结清语句
  `WHERE store_id=?3 AND operation_id=?4 AND state='pending'`；门禁/可用性/关闭/`mark_clean` 的
  未决查询按确切作用域提问；本机域不认领任何远端日志；busy/dirty/clean 的行键与 sidecar 锁名改
  由内部 `ExecutionDomain::{Local, Remote(store)}` 派生（远程域长度前缀编码，本机域保持 thread id
  原文，历史行不重写）；adapter 逐条核对 `record.store_id == self.store_id()`，不匹配则**不发远端
  SQL、不改本机记录**，外来记录原样保留——每个 store 只报告其范围结果。
- **证据**：

| 类型 | 命中 |
| --- | --- |
| 离线 | `stores_do_not_share_pending_operations_for_the_same_root`、`stores_do_not_share_execution_generations_for_the_same_root`、`mark_clean_is_blocked_only_by_its_own_store`、`local_sessions_do_not_share_execution_generations_with_remote_roots`、`test_remote_operation_log_is_store_scoped_and_cross_process_visible`、`test_unsettled_remote_operation_blocks_the_whole_root` 全绿 |
| 离线反例 | 把谓词与结清语句回退成旧形状（S1 实测）：`stores_do_not_share_pending_operations_for_the_same_root`、`mark_clean_is_blocked_only_by_its_own_store` → 2 failed |
| 云反例 | 同时回退「store 谓词 + adapter 防御分支」（S2 实测）：`cloud_foreign_store_pending_record_is_never_settled_through_another_store` 子进程失败于 `another store's recovery must not settle a foreign pending record`、父进程失败、`retained_receipts=3`（比修复后多 1 条 = B 的账本里写入了外来操作 id 的封闭回执） |
| 云（本批终态） | 同一命令在最终树上 **1 passed**：真实 store B + 本机合成 store A、同 HOME、与 B 的会话**同 root id**；子进程 `write_open_foreign_pending=preserved` / `read_only_foreign_pending=preserved`，父进程**新连接**读 B 账本 `foreign_receipts=0` |

#### 2) 云清理器删除封闭证据（P1 第二半）

- **缺陷**：`cloud_test.rs` 的 `DELETE_RUN_LEDGER_SQL` 与 `cloud_mutation_test.rs` 的 `CLEANUP_SQL`
  （`DELETE FROM peri_op_ledger`）会删掉本轮收据——收据是「这次操作发生过」的证据，删掉后迟到的
  同 id 请求失去判据，与 P7 口径（收据不按 TTL 清理）冲突。
- **修法**：清理器**只删本轮合成会话/消息**（静态 SQL + 全绑定参数，`run` 只作绑定值，仅命中
  `{run}%`，不动未知/shared/其他 run）；收据一律保留并报告条数。清理后的复核改到**新连接**上：
  本轮合成数据为 0、本轮收据只增不减、清理自身那张收据仍能经 adapter 读回 `Applied`。
- **证据**：

| 类型 | 命中 |
| --- | --- |
| 离线契约 | `test_cleanup_never_deletes_ledger_receipts`（效果语句不含 `peri_op_ledger`、`run` 不进 SQL 文本）、`test_cleanup_retention_check_reports_retained_receipts`（收据丢失或数据未清都失败） |
| 云（新连接） | 上表那次回归的清理复核即走新连接：`retained_receipts=2`（本轮真实写入 1 + 清理自身 1），读回无 `Absent/Closed` |
| 云反例 | 把新连接的收据读回指向一个不存在的操作 id（本批实测，非破坏性）→ 命令失败于 `cleanup receipt must stay readable on a new connection: Absent`，证明该复核不是空断言 |

- **有意保留的空间成本**：本批 4 次云运行各在本轮命名空间留下 2 条收据（3 次成功运行观察到
  `retained_receipts=2`；那次故意失败的控制运行在复核前中止、未打印计数，其结构相同）。收据只含
  id/kind/摘要/终态/时间戳、**不含 payload**，不随 TTL 清除。
- Fable 留的探针产物（`peri-fable-foreign-…pending`）原样保留：它是探针在**自己的 tempdir HOME**
  里插入的一行记录（进程结束随目录消失）；本批以只读连接复核真实 HOME 库
  （`~/.peri/threads/threads.db`）：**不存在**该外来记录行（该库没有 `session_remote_operations`
  表，该 id 只作为会话文本出现在 `-wal` 里），本批无任何清理路径触碰它（探针目录
  `peri-fable-probe-*` 亦只做只读核对）。

#### 本批命令与结果

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | **310 passed / 0 failed / 28 ignored** |
| `cargo test -p peri-resources --all-targets` | 310 passed + **6 passed**（契约目标 `session_resources_contract`） |
| `cargo check --workspace --all-targets` | Finished（无 error/warning） |
| `cargo clippy -p peri-resources --all-targets -- -D warnings` | Finished（零告警） |
| `cargo fmt -p peri-resources -- --check` | 无差异 |
| `... --ignored --test-threads=1 cloud_foreign_store_pending_record_is_never_settled_through_another_store` | **1 passed**（5.3s；`foreign_scope=ok`、`foreign_receipts=0`、`retained_receipts=2`） |

#### 仍未关闭（不得据此宣称完成）

- Fable 本批其余 finding 维持 **FAIL**，本批一行未改：StoreId 首次竞争、child、close、path、conn、frozen。
- `session_lifecycle_commitments` 仍按 `thread_id` 单键（无 store 列）：同 id 跨 store 共享删除墓碑，
  影响面限于删除锚点；`mutation_pending` 在生产路径不写。
- §9.16 的连接生命周期缺口（取消/超时后同进程内远程操作不可用）未修。
- 本轮**只跑了一个云回归**，没有重跑 P5–P7 全量云实验：§9.16 的云结论仍是上轮读到的；本批改动不
  改变 P7 口径（收据只增不减，清理器不再有该表的删除语句）。
- `recover_persistence` 的跨域防御分支没有「从门面外部制造来路不符的本机日志」的专门测试（门面/夹具
  无法生产该形状），由严格谓词 + adapter 再检查 + 上表云反例共同覆盖。
- 远程执行域行键对**升级前**已存在的远程 root 行不做重写（本机域语义不变、远程域看不到它们）：
  本批的明确取舍，见 S2 残留。

### 9.18 S1–S3 同因收尾：远程域独立存储与旧键残余处置（同日第十五轮）

范围只有三件，都是 §9.17「仍未关闭」里那两条的直接原因，别的一行未碰（Fable 其余 finding、init、child、
close、path、conn、云实验都不在本批）。

1. **远程墓碑不再与本机墓碑共用键空间**（原缺口：`session_lifecycle_commitments` 按 `thread_id` 单键）。
   远程删除锚点改写在 `remote_lifecycle_commitments(store_id, thread_id)`（schema v9）；本机墓碑表一字未改。
   远程侧的 `anchor_deletion`／`finalize_deletion`／`identity_anchored` 全部走两列键，且
   `identity_anchored` 分成本机域与 `identity_anchored_in(store, …)` 两条路：本机墓碑不回答远程问题，
   远程墓碑也不回答本机问题。
2. **旧 raw 执行代际与旧锁不再被忽略**（原缺口：S2 把远程键改成编码后直接看不到发布版写下的 raw 行）。
   远程代际改写在 `remote_execution_runs(store_id, root_id)`（同样 v9），与本机 `execution_runs` 不相交；
   旧键残余按三条保守规则处置：**取不到旧锁就 `ExecutionBusy`**（旧 owner 活着不许越过）；**唯一可证才迁移**
   （本机没有同 id 会话身份，且日志里这个 root 只属于这一个 store，或本机只登记过这一个 store），在旧锁内
   与本次代际推进同一次提交里搬运，`generation`/`clean` 原样保留（dirty 仍 dirty，报 `RecoveryRequired`
   精确代际）；**歧义一律挡住**（`Corrupt`，旧行不删不改写，不自动归入当前 store）。报错的那次调用整体回滚，
   因此事实要么还在旧行、要么已经在本域表。
3. **两域真正不相交，不再依赖对 id 内容的假设**。`ExecutionDomain::row_key`（编码键）删除：本机域用
   `thread_id` 原文，远程域是两列主键；sidecar 锁同名问题一起解决——本机锁仍是 `<sha256(id 原文)>.lock`，
   远程锁落在 `.execution-locks/remote/` 子目录，内存 owner 登记键改成 `LeaseKey` 枚举（不是字符串）。
   `ThreadId` 是不透明字符串，构造一个恰好等于旧编码键的本机 id 不再可能串域（有专门回归）。

**迁移兼容结论**：schema 8 → 9 只加两张表，既有行（含未结清 dirty、墓碑、登记、操作日志收据）逐行保留，
升级可重复；只读打开旧库仍按列形状放行；本机域的行键、锁名、语义与既有测试完全不变。

**本批命令与结果**

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | **317 passed / 0 failed / 28 ignored** |
| `cargo test -p peri-resources --test session_resources_contract` | **6 passed / 0 failed** |
| `cargo clippy -p peri-resources --all-targets` | 零 warning / 零 error |
| `rustfmt --edition 2021 --check <本批 13 个文件>` | 无差异（只格式化本批文件，未全仓） |

新增回归（`sqlite_store/remote_execution_test.rs`，7 条）：墓碑按 store 分域、可归属旧 dirty 迁移后仍 dirty
并按旧代际步进、不可归属旧行挡住且原样保留、外来旧行两域都不动、活旧锁 busy、等于旧编码键的本机 opaque id
不串域、v8→v9 只加表不改行。

**仍未关闭**

- 旧键残余的处置只覆盖「同一时刻只有一个写者」：与旧二进制同时写不做互斥（本批明确不需要）。
- 云回归本批未跑（跨 store 云用例由上批口径覆盖，远程域存储换了表，云断言按新表名改过一处）。

### 9.19 §9.18 追加：未决判定的作用域（同一轮，提交前收尾）

§9.18 报告把 `pending_persistence_on` 说成「不在三缺口内」，那是误判：它读的
`session_lifecycle_commitments` 正是 StoreId 隔离目标里的那张表，而且它对本机与远程两个作用域
**都**先跑 `thread_root_on` 本机 `threads` 父链——同名本机 child 会把远程 root 映射到别人的树上，
随后那次 raw 锚点查询就跨域了。本批一并修掉：

- `LocalStore`：本机父链的根 + 本机写入锚点（与本机域既有语义一致）。
- `RemoteStore`：只问本 store 的事实——日志里的 thread → root 关联（没有记录按自身，不再用本机链猜）、
  `remote_lifecycle_commitments` 里本 store 的未决锚点、本 store 未结清的远端操作。
- 本机表里的旧 raw `mutation_pending`：不忽略也不乱认，走 `remote_execution` 已有归属规则
  （可证属于本 store 计入未决、可证属于别的 store 不计、归属不明报 `Corrupt`）。

新增回归（`session_data_test.rs`）：同名本机 child 的父级锚点不回答远程作用域；远端未结清操作不冒充
本机未决，本机旧 raw 锚点不冒充远程未决（无证据时报 `Corrupt`，有唯一证据时计入）。
`remote_execution.rs` 的注释里「发布版」措辞已改成「早期实现（本分支 checkpoint 之前，尚未发布）」，
不再宣称这些 raw 行来自已发布版本。

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | **319 passed / 0 failed / 28 ignored** |
| `cargo test -p peri-resources --test session_resources_contract` | **6 passed / 0 failed** |
| `cargo clippy -p peri-resources --all-targets` | 零 warning / 零 error |

### 9.20 Fable P1（第一半）关闭：身份初始化竞争的胜者表达（同日第十六轮）

只处理「创建事实与竞争判定」。**本条不含** child 父关系校验（Fable 第二条 P1），也不含 close/连接
重建/path/frozen/首次产品登记，它们各自独立成批。

- **缺陷**：`RemoteSessionData::open` 在调用 `initialize_store` **之前**就把 `CreatedByThisOpen`
  预设好，而 `initialize_store` 只回一个身份值——唯一键冲突时它读回胜者身份，**败方因此也获准首次
  登记**（两个全新 registry 对同一个空库并发打开时，败方会认领别人的存储并拿到执行资格）。
- **修法**（都在内部；公共行为接口不暴露 SQL/事务/CAS/重试）：
  `initialize_store` 返回结构化 `StoreIdentityOutcome::{Created, Existing}`，两者都带权威身份；
  created 的唯一证据是**本事务在 `META_INSERT_INDEX` 上确切影响 1 行且批确认提交**
  （`schema::inserted_meta_row` + `initialization_evidence` 纯判定），唯一键冲突与「批成功却无插入
  证据」一律 `Existing` 并**读回**既有身份，结果未知（丢响应/超时/回滚失败）直接失败：不发身份、
  不发创建事实。`CreatedByThisOpen` 只剩一个来源——`open_verdict(Created)`；「打开前读到空库」不再
  参与推导。本机一侧未改：门禁仍只认 `CreatedByThisOpen`。
- **证据**：

| 类型 | 命中 |
| --- | --- |
| 离线新增（`remote/initialization_test.rs`，真 SQLite 引擎 + 真本机登记库，非桩） | 5 passed：竞争（胜者 `Created`／败方 `Existing` 且身份相同、败方零登记零资格）、已有身份不认领、未知结果不创建、读回拒绝不可解释身份、形状判定共用 |
| 离线反例（变异检验） | 把 `open_verdict` 改回「一律 `CreatedByThisOpen`」（等价修前行为）：`only_the_winner_of_the_identity_race_may_register_first` → 1 failed |
| 离线全量 | `cargo test -p peri-resources --lib` → **324 passed / 0 failed / 28 ignored**；`cargo clippy -p peri-resources --all-targets` 零警告；`rustfmt --edition 2021 --check <本批 6 个文件>` 无差异（`registration.rs` 只同步一句门禁文档） |

- **边界（诚实记录）**：**竞争判定本身只有本地 seam 证明**——`initialization_test.rs` 用真 SQLite
  引擎跑生产同一份初始化 SQL/读取计划，经生产同一条 `initialization_evidence → open_verdict`，本机侧
  是真登记库；共享云 store 的身份不能重置（重建 `peri_store_meta` 即伪造历史），云端不重放竞争。
  云侧只覆盖「再次初始化返回既有身份、不再发创建事实」这半边，第十八轮已真跑（见 9.22）。
  **首次创建的运行证据仍缺失**：尚未在空 Turso store 上观测 `Created` 与首次接纳；SDK 在该位置返回
  `rows_affected == 1` 的假设只有本地 seam 支撑。若实际返回 0/缺项，当前代码会保守判 `Existing`、
  拒绝首次接纳，而不是错误发放资格；因此不能据这批补丁宣称新库首登已通过云验收。

### 9.21 Fable P1（第二半）关闭：child 父子关系「两处声明」只留一个真相（同日第十七轮）

只处理 `save_child` 的父子/根归属输入一致性。**本条不含** close/连接重建/path/frozen/首次产品登记。

- **缺陷**：门面与两个 adapter 校验的是快照声明字段 `parent_id`/`root_id`，而**落库**用的是
  `target.meta.parent_thread_id`。给一个声明合法 parent/root、但 `target.meta.parent_thread_id = None`
  的 child，会被写成一条**没有父的独立 root**；此后这条 identity 还能自己取得执行权（Fable 复现路径）。
- **修法**：`data::ensure_child_relation` 是唯一一条规则（不读存储、不看「库里有没有会话」）：
  `target.meta.parent_thread_id` 必须等于 `parent_id`，`parent_id`/`root_id` 都不得等于
  `target.thread_id`。门面在**任何副作用之前**调用它（不发门禁、不留未决证据），本机与远程 adapter
  各自再调一次作防御——三处同一份代码，不是三套规则。字段不合并（`parent_id` 是继承来源、`root_id`
  是执行域），一致性在入口强制。
- **证据**：

| 类型 | 命中 |
| --- | --- |
| 门面反例（真 SQLite，`resources_test.rs`） | 4 种不自洽（meta 无父 / meta 指向别的父 / 自指父 / 自指根）全部 `InvalidInput`，且零行、零绑定、零执行代际、无锚点、root 原样（树里只有自己、owner 照常可写）；同一份合法快照随后仍成立，落库父 = 声明父，解析到的仍是 root 那条 owner |
| 数据面反例（`sqlite_store/session_data_test.rs`） | 不经门面直接调用：同样拒绝且零行；改回与 meta 一致后同一次保存才落库 |
| 远程入口反例（新增 `remote/session_child_guard_test.rs`，离线） | 输入不自洽 → `InvalidInput` 且本机收据表零行、未决查询为空（校验先于任何 I/O）；自洽输入不被拦（继续走到连接，报 `Internal`） |
| 变异检验 | 逐个移除三处守卫：门面 + 本机 adapter 移除 ⇒ 门面反例在 `unwrap_err` 上失败（**修前确实保存成功**）；单独移除本机 adapter ⇒ 数据面反例同样失败；单独移除远程守卫 ⇒ 远程反例拿到 `Internal`（它已去取连接） |
| 离线全量 | `cargo test -p peri-resources` → **327 passed / 0 failed / 28 ignored** + 契约 6 passed；`cargo clippy -p peri-resources -p peri-acp-types --all-targets -- -D warnings` 零警告；`rustfmt --edition 2021 --check`（本批 9 个文件）无差异；`cargo build --workspace` 通过 |

- **边界（诚实记录）**：`save_child` 的**远端落库反例**（被拒后不落行、原 root 不变）仍只在离线层面
  证明（校验先于 I/O、被拒零收据），远端父子列与 `target.meta` 同源由 `session_sql` 形状测试覆盖；
  近邻路径「远程 `create_session` 只接受 root」第十八轮已真跑（见 9.22）。E2E subagent 用例本批未跑。
- **未关闭（不得标记完成）**：整体 Fable 仍 **FAIL**——**close、conn、path、frozen、首次产品登记**
  各自独立成批，本批一行未碰。

### 9.22 前两半收尾：真云半边实测与状态板（同日第十八轮）

第十八轮仅做记录和云端验证；代码修复在 9.20/9.21 对应轮次落地，并随同一修复提交交付。该验证轮把前两节留在 `#[ignore]` 的云半边真跑，
并重跑离线全量确认最终状态。

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources` | lib **327 passed / 0 failed / 28 ignored** + 契约 **6 passed** |
| `cargo test -p peri-resources --lib -- initialization_tests registration_tests child` | **39 passed / 0 failed / 1 ignored**（被忽略的是云用例 `cloud_pending_blocks_child_first_write`） |
| 真云 `cloud_mutation_committed_rows_are_visible_to_a_new_connection` | 1 passed：`initialize_verdict=existing`、`initialize_is_idempotent=true`、`cross_connection_read_after_write=true`（新连接读回同一条收据）、两连接同一 store 身份（`schema_version=1`） |
| 真云 `cloud_receipts_are_retained_with_measured_cost` | 1 passed：`retained_receipts=3`、`ledger_rows=2 ledger_bytes=454`、`recovery=Recovered`、`receipt_retention=ok` |
| 真云 `cloud_remote_create_refuses_parent_input` | 1 passed：带父输入 → `InvalidInput`（"child sessions must be saved through the child path"），远端 `sessions=1`（只有 root 行）、`retained_receipts=2` |
| `cargo clippy -p peri-resources -p peri-acp-types --all-targets -- -D warnings` | 零警告 |
| `cargo check --workspace --all-targets` | 通过 |
| `rustfmt --edition 2021 --check --config skip_children=true`（前两批 15 个改动 .rs 文件） | 无差异 |

- **服务端真实验证的部分**：再次初始化返回既有身份且不重铸身份、提交行对新连接可见、收据在清理与
  重读后逐条逐字节保留、远程 `create_session` 的 root-only 拒绝（远端零 child 行）。
- **只有本地 seam 的部分**：身份竞争本身（胜者 `Created`／败者 `Existing`、败者零登记零资格）与
  「丢响应不得误认 created」——共享库身份不可重置（重建 `peri_store_meta` 即伪造历史），云端不重放
  竞争；`save_child` 的远端零落行同样只有离线证明。
- 云清理沿用前轮修正后的**共用实现**（`cloud_tests::cleanup_effects` / `with_cleanup`）：只删本轮 run
  命名空间，ledger 收据有意保留（`retained_receipts` 为证），未复制任何旧清理器。
- **状态板**：9.20、9.21 两条 P1 关闭（实证见上）；**close、conn、path、frozen、首次产品登记仍 FAIL**，
  各自独立成批。整体 Fable 结论仍 **FAIL**。

### 9.23 path：部署定位类型区分（同日第十九轮）

`SessionStoreDeployment` 的 locator 由 `Option<String>` 改为中性枚举 `SessionStoreLocator`（`Default` /
`LocalPath(PathBuf)` / `Locator(String)`）；`--db-path` 归一为 `LocalPath`，资源层按**类型**分派，不再把
路径塞回字符串再解释。修掉两处同一成因的缺陷：`PathBuf::to_string_lossy` 让 Unix 非 UTF-8 文件名在
往返中损坏（U+FFFD），以及 `--db-path <名字>` 里合法的 `env:` 字面文件名被当成环境引用解（缺变量时
报配置错误、文件打不开）。

- 语义边界：`--db-path env:UNSET` 是名为 `env:UNSET` 的本机文件；`--session-store env:UNSET` 才解引用
  环境变量。`LocalPath` 不解 `env:`、不判 URL（Windows drive/UNC 形状不按 scheme 猜），不经字符串往返。
- 早失败：`LocalPath` 与 `Default` 一样拒绝远程专属参数（引擎名 → `EngineForLocalStore`；凭证来源 →
  `CredentialSourceForLocalStore`），在入口转换处失败，不静默忽略；`Debug` 仍只给形态（新增
  `<local-path>`），不回显路径/locator/凭证名。
- 回归（新增，离线）：`db_path_file_named_like_an_env_reference_opens_as_a_file`（真文件，旧实现报
  `EnvValueMissing`）、`db_path_keeps_non_utf8_bytes_end_to_end`（字节保真；macOS syscall 拒绝非 UTF-8
  路径 EILSEQ、Linux 建库，两种平台都不得落到 lossy 变体）、`db_path_windows_shapes_skip_locator_parsing`
  （形状解析，不要求本机是 Windows）、`env_colon_literal_is_a_file_name_for_db_path_but_a_reference_for_session_store`、
  `remote_only_parameters_on_confirmed_local_path_fail_early`。
- 证据：`cargo test -p peri-acp-types --lib` 468 passed / 0 failed；`cargo test -p peri-resources --lib`
  351 passed / 0 failed / 28 ignored；`cargo test -p peri-tui --test meta_session_cli --test print_exit`
  19 + 9 passed；`cargo check -p peri-acp --all-targets` 通过；clippy（types/resources/tui all-targets）
  零警告；`rustfmt --edition 2021 --config skip_children=true` 本批文件无差异。
- **既有失败（不得算作本批完成）**：`cargo test -p peri-tui --bin peri` → 84 passed / **1 failed**，
  `cli_meta::tests::unwired_remote_store_reports_unavailable` 期望 exit 4 `store_unavailable`、实得 exit 1
  `internal_error`。因果：该用例只给远程 locator 与凭证**来源**、不设变量，`CredentialSource::resolve()`
  返回 `CredentialError::Missing`，而 `classify_open_failure` 只识别 `LocatorError`，遂归 `Internal`；
  与本批改动无关——`from_deployment` 的 `Locator` 分支调用与 HEAD 逐字相同，远程链一行未碰（判定依据：
  diff 与静态调用等价；本批禁用 stash/checkout，未在 HEAD 工作树上实跑复核）。修法属另一批：要么测试
  自带隔离变量，要么给 `classify_open_failure` 补 `CredentialError` 分类——需先定「凭证缺失」的 kind。
- 仍 FAIL（各自独立成批）：close / conn / frozen / 首次产品登记（首次云 `Created` 观测持续未完成）；
  prepared 冻结字节 seam（目标 2）本批未开工。

### 9.24 frozen：发布段只消费同一份准备输入（同日第二十轮）

修掉目标 2 的「拿两次准备相等当同源」假证明：`handle_new` 拆为 resolve workspace → `prepare_new`（new 路径**唯一**
一次准备）→ `new_session_from_prepared`（发布段：写 meta/binding/frozen、取执行 owner、复核准入、装配环境、发布
live 状态）。发布段接收调用方定格的准备对象，不读配置、不加载插件、不重建 frozen；生产与测试同调该函数，因此
「保存字节 == 给定准备输入的字节」第一次成为可断言的性质。

- 回归重写：`new_session_persists_frozen_bytes_from_its_single_preparation`（端到端只走生产入口：持久化字节解出的
  `claude_md` 等于本次工作区输入、live frozen 重编码逐字节等于持久化字节、binding/owner 成立）；新增
  `new_session_from_prepared_does_not_reread_external_frozen_inputs`（准备后改写 cwd/CLAUDE.md，再以该准备对象直投
  发布段：保存字节精确等于给定字节、live frozen 逐字节同源、改写内容不得进入 live 状态；`rebuilt` 哨兵断言「外部
  输入已变 → 二次准备必须给出不同字节」，使任何重读/重建都必失败）。
- **判别力实证（变异测试）**：临时在发布段内再 `prepare_new` → 用例在「保存字节必须精确等于给定准备输入的字节」
  处失败；回退后 7 passed。不加 `serial_test`、不重跑取绿、不放宽日期字段、不改 frozen envelope。
- 删旧的 `new_session_persists_prepared_frozen_bytes_once`：其失败模式正是 §9.16 记录的并行 flaky（同一用例内两次
  准备之间，并行用例可能改变进程级探测输入），且该断言从未证明是同一对象。
- 证据：`cargo test -p peri-acp --lib prepared` 7 passed / 0 failed；`cargo test -p peri-acp --lib` **721 passed /
  0 failed / 0 ignored**（§9.16 记录的该模块全量并行失败已消失）；`cargo clippy -p peri-acp --all-targets -- -D warnings`
  零告警；`rustfmt --edition 2021 --check --config skip_children=true` 本批文件无差异。
- 仍 FAIL（各自独立成批）：close / conn / 首次产品登记（首次云 `Created` 观测仍未完成，不得假称整体完成）。

### 9.25 收尾复核：路径不经 lossy 往返、两种定位入口语义分离、冻结字节同源（同日第二十一轮）

本轮只做复核与证据固定，**无生产改动**。静态闸门：`peri-acp-types/src/session_store.rs` 与
`peri-resources/src/sessions/open.rs` 内 `to_string_lossy` 零命中（全文检索）；`from_deployment` 的
`LocalPath` 分支直接 `StorageLocator::LocalPath(path.clone())`，`resolve_locator` 该变体直接
`ResolvedLocator::Local(path.clone())`，全程无字符串往返。`SessionStoreDeployment` /
`SessionStoreLocator` **没有 serde 实现**（仅 `Clone/PartialEq/Eq` + 手写脱敏 `Debug`），因此不进入
JSON-RPC wire；meta 的 JSON 输出只经 allowlist DTO（`json_success_is_one_exact_allowlisted_object` 通过）。

- 语义分离复核（真跑）：`env_colon_literal_is_a_file_name_for_db_path_but_a_reference_for_session_store`、
  `db_path_file_named_like_an_env_reference_opens_as_a_file`（真文件）、`db_path_keeps_non_utf8_bytes_end_to_end`、
  `remote_only_parameters_on_confirmed_local_path_fail_early` 通过；CLI 侧 `test_db_path_conflicts_with_session_store_before_io`、
  `test_session_store_deployment_normalizes_locator_options`、`test_session_store_deployment_debug_keeps_locator_out`
  通过。
- 证据（本轮实跑命中/退出）：`cargo test -p peri-acp-types --lib session_store` 8 passed、全量 468 passed；
  `cargo test -p peri-resources --lib sessions::open` 22 passed、全量 351 passed / 28 ignored；
  `cargo test -p peri-resources --test session_resources_contract` 6 passed；`cargo test -p peri-acp --lib prepared`
  7 passed、全量 721 passed；`cargo test -p peri-tui --test meta_session_cli --test print_exit` 19 + 9 passed；
  `cargo test -p peri-tui --bin peri session_store` 8 passed；`cargo check --workspace --all-targets` exit 0；
  `cargo clippy`（types/resources/acp/tui all-targets，`-D warnings`）exit 0；本批文件按各自 crate edition 过 rustfmt
  （`peri-tui` 2024、其余 workspace 2021；edition 不符会报 style-edition 假差异）无差异。
- **既有失败（仍在，非本批引入；不得算作完成）**：`cargo test -p peri-tui --bin peri` → 84 passed / **1 failed**，
  `cli_meta::tests::unwired_remote_store_reports_unavailable` 期望 exit 4、实得 exit 1。补强判定：`git show HEAD:`
  复核 HEAD 的 `from_deployment` 远程分支与当前逐字相同，`cli_meta.rs` 与 `classify_open_failure` 本批未改，
  `cli_meta_test.rs` 唯一改动是构造器更名；且 `RemoteStoreNotWired` 已在 C 批删除（远程分支是真装配），
  「远程未接线」这个用例前提本身已过期——装配前先解析凭证值 → `CredentialError::Missing` 不在
  `classify_open_failure` 的识别集 → `Internal`。修法属另一批：先定「凭证缺失」的 kind（配置错误 exit 2 或
  按新前提改写用例），不靠改测试掩盖。
- 未验证 / 未完成（继续显式记账，不假称整体完成）：首次产品登记仍 FAIL——首次云 `Created` **观测仍未完成**；
  close / conn 仍 FAIL（各自独立成批）。首登产品边界不变：**仅本安装初始化的新库可获首次登记；已初始化但无本机
  registry 只能读**（未加 register/接管，不自动登记，不 seed 真实 registry）。Windows drive/UNC 仅形状断言
  （未在 Windows 实跑）；macOS 非 UTF-8 路径按平台如实断言 EILSEQ（Linux 建库）。上述失败用例里「stderr 不回显
  locator 原文」的断言因其先断言 exit code 而未被执行，该路径脱敏本批未取得证据（脱敏另有
  `url_with_embedded_secret_is_rejected_without_echo`、`request_debug_keeps_host_and_credentials_out` 通过）。
  本轮未跑云、未跑 LLM。
- 父代理 commit 范围（本批 13 个文件）：`peri-acp-types/src/session_store.rs`、`session_store_test.rs`、
  `peri-acp/src/host/requests/session_lifecycle.rs`、`peri-acp/src/host/prepared_test.rs`、
  `peri-resources/src/context.rs`、`peri-resources/src/sessions/open.rs`、`open_test.rs`、
  `remote/cloud_deployment_child_test.rs`、`peri-tui/src/main.rs`、`main_test.rs`、`cli_meta_test.rs`、
  `docs/code-index/peri-acp.md`、`docs/code-index/peri-resources.md`、本母 issue。**不属于本批**（勿一并提交）：
  `.github/workflows/ci.yml`、`CLAUDE.md`、`peri-cool`、`peri-middlewares/src/mcp/mod.rs` 及未跟踪的
  `peri-middlewares/src/mcp/builtin_spike_test.rs`。

### 9.26 缺凭证按类型归配置错误、meta 过期用例改写（同日第二十二轮）

远程 locator 的凭证**来源**没配好时（变量未设置/空值/非 Unicode/名字非法、注入值为空），`CredentialError`
此前不在 `classify_open_failure` 的识别集（只认 `LocatorError`），被归 `Internal`（meta exit 1）。现在按
**类型**分类：source chain 上出现 `CredentialError` → `StoreOpenFailure::NotConfigured`（meta exit 2
`store_not_configured`），不解析错误文本；失败仍早于任何本机 I/O 与网络调用。`CredentialError` 保持 crate
内可见（`sessions` 的最小 `pub(crate)` re-export 仅供分类用），不进公共 API，不向 TUI 交底 SDK 类型或凭证
值；`SessionStoreCredential` 仍无 `Debug`/序列化。未扩大其他错误分类与迁移。

- 过期用例改写（前提 `RemoteStoreNotWired` 已在 C 批删除，远程分支是真装配）：
  `cli_meta_test.rs::unwired_remote_store_reports_unavailable` →
  `missing_credential_configuration_is_a_configuration_error`：显式不存在的变量（用例内移除并断言不存在，
  受控环境、不连网）+ 远程 locator 哨兵，断言 exit 2 / `store_not_configured`，locator 原文与凭证来源名
  都不回显（meta 错误文案是固定串）。
- 新增回归：`context_test.rs` 对 source chain 包装的五种 `CredentialError` 逐一断言 `NotConfigured`
  （没有凭证原因的普通失败仍是 `Internal`，分类不被 context 放大）；子进程 + 临时 HOME 真跑
  `Resources::open_deployment`：缺凭证 → `NotConfigured`，先断言 `local_registry_path()` ==
  `$HOME/.peri/threads/threads.db`（HOME 控制生效），再断言临时 HOME 目录项数 == 0（登记库/库/侧车零副作用）。

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-tui --bin peri` | **85 passed / 0 failed**（此前的 `unwired_...` 失败随用例改写消失） |
| `cargo test -p peri-resources --lib` | 355 passed / 0 failed / 28 ignored |
| `cargo test -p peri-resources --lib context` / `sessions::open` | 21 / 22 passed |
| `cargo test -p peri-tui --test meta_session_cli` | 19 passed |
| `cargo clippy -p peri-resources -p peri-tui --all-targets -- -D warnings` | exit 0 |
| rustfmt（各 crate edition，`skip_children=true`） | 本批文件无差异 |

- 现状：close / conn 已由本 HEAD 批（`b47e98f5` 等）修好，**待 Fable 复验**（不再记 FAIL）；首次云 `Created`
  观测与 E 全链路消费侧仍未完成，各自独立成批。本批文件：`peri-resources/src/context.rs`、`context_test.rs`、
  `peri-resources/src/sessions/mod.rs`、`peri-tui/src/cli_meta_test.rs`、本母 issue。未读 `.env`、未用真实
  HOME（子进程临时 HOME）、未连网、未跑 LLM，无新增 `#[allow]`/`#[ignore]`。

### 9.27 v10 撤销后的真云回归收口：root-only 判定回家、删除表达两端统一（同日第二十三轮）

v10 撤销（`120dc8cb`）拆掉本机远程痕迹表与整条登记/准入链之后，真云回归第一次整体重跑：21 通过、1 失败。
失败的不是过期用例，是**真回归**；同轮把「一份删除逻辑跑两种执行器」的最后一处引擎依赖补齐。

**① 远程新建的 root-only 输入判定在撤销时丢了。**

- 症状：带父的输入不再被拒绝，而是真的往远端写出一条父关系，随后本机准入报 `SavedButNotAdmitted`——
  远端留下一条绕过 child 通路的会话行，调用方还拿到一个误导的错误分类（像是「本机没登记」，实际是
  「这个输入不该走这条入口」）。绑定合法时准入会成功，那条没经过 `ensure_child_relation`、root owner
  门禁与 frozen 继承判定的会话就直接上线。
- 根因：判定原先实现在被删的 `remote/local_execution.rs` 里；v10 把远程写路径内联进 `resources.rs` 时
  只带了数据面调用，漏掉了这条输入判定。
- 落点：判据回到**远程 adapter**（`remote/session_write.rs::write_new_session` 首行，构造语句与取连接之前）：
  `parent_thread_id.is_some()` → `InvalidInput`。**不放门面**：该规则按文档只约束远程入口，而「本机新建
  接受带父的目标」是既有夹具的前提（`peri-middlewares` 的 `preset_resumable_thread(..., Some(parent))`
  等 13+ 处、`peri-agent` 的 `save_new_session`），上移会连坐本机模式。
- 离线回归：`remote/session_child_guard_test.rs::test_remote_root_entry_rejects_a_parent_before_any_io`，
  用连接已关闭的 adapter 证明判定**先于任何远端读取**（带父 → `InvalidInput`；同一入口的 root 输入照常
  走到存储路径 → `Internal`），与既有的 child 输入一致性用例同形。
- 真云复核：`cloud_remote_create_refuses_parent_input` 复跑通过（`refused_class=InvalidInput`、`sessions=1`、
  `parent_input=refused`）。

**② 本机删除不再借 `ON DELETE CASCADE`：两端共用同一份删除语句。**

本机三处删除 `threads` 行的路径（数据面 `delete_tree`、`revoke_unpublished_session`、迁移桥
`SqliteThreadStore::delete_thread`）原先让 `messages`/`session_bindings` 靠级联消失，而远端 schema 不含任何
`REFERENCES`，级联无从谈起（`PRAGMA foreign_keys` 在远端默认读 0、跨连接共享、无 `foreign_key_check`
等价物，见 §9.28 的例外 2/3/4）。现在三处都按**同一份语句**显式先删子行
再删父行：`session_rows::THREAD_CHILD_DELETES` + `delete_thread_child_rows()`（`execution_runs` 沿用 v7 起
就有的显式删除）。`ON DELETE CASCADE` 声明**保留**（删外键要重建实盘库的表，收益只是省几条 DELETE），但
已退化为空操作式安全网——承重的是显式语句。

不变量由 `sqlite_store/thread_child_delete_test.rs`（4 项）守：期望集合从**运行库真实 schema** 派生
（`sqlite_master` × `pragma_foreign_key_list`），与生产声明双向核对（新增一张 `REFERENCES threads` 的子表
必然变红）；三条生产删除路径都跑在 `PRAGMA foreign_keys = OFF` 的池上（夹具自证读数为 0），因此「删完没有
孤儿行」只可能由显式删除满足——在 `foreign_keys = ON` 的本机上，把显式删除删掉是**不会红**的。四个测试都
实测能变红（逐个注释掉显式删除 → 各自红；临时插一张子表 → 交叉核对红）。

**③ 「本地不留任何 store 痕迹」在真云端到端里被断言。**

`cloud_deployment_test.rs` 的父测试原先只核对云端计数与本机库**文件存在**。现在每个阶段的新连接核对之后，
再用只读连接盘点本机执行面库（真实落盘文件，不是门面自陈）：

- schema 版本与表集合必须与**同一构建在本机模式下新建的库**逐表相同（期望值现场派生，不另抄名单）；
- v10 删掉的五张本机远程表不得回归（名单从 `sqlite_store/schema.rs` 的 `DROPPED_LOCAL_TABLES` 原文派生）；
- `threads`/`messages`/`session_bindings` 全为 0，`projects`/`workspaces` > 0（workspace 证据是授权的本机事实）；
- 本轮 run 的执行代际必须精确等于预期：写入后 root 与 fork 两条 `clean = 0`（child 由 root 的租约持有，
  自己不落代际）、冷恢复删掉 root 树后只剩 fork、只读打开之后库内容逐项不变。

同时把子进程与父测试共用的三个会话后缀提为常量（`ROOT_SUFFIX`/`CHILD_SUFFIX`/`FORK_SUFFIX`），「哪条会话
有本机执行事实」不再在两处各写一份。

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | 330 passed / 0 failed / 22 ignored |
| `cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1`（真云全量，隔离单跑） | **22 passed / 0 failed**（140.22s；含此前转红的 `cloud_remote_create_refuses_parent_input`） |
| `cargo clippy -p peri-resources --all-targets -- -D warnings` | exit 0 |
| `cargo fmt --check` | 本批文件无差异 |

- 并发边界（实测）：这两条真云用例**不是并发安全的**——一轮全量门在另一个测试进程同时跑同一个
  `cloud_deployment_` 用例（对方正在做故障演示、临时改坏断言）时红过一次；同一份最终代码隔离单跑
  22/22。真云套件按既有约定串行单跑（`--test-threads=1`，且同一时刻只有一个进程），共享库上的
  合成 run 命名空间隔离的是**数据**，隔离不了「对同一份源码快照的并发假设」。

- 边界：本地 schema 的 `ON DELETE CASCADE` 声明与 v10 的 `DROPPED_LOCAL_TABLES` 都不动（前者删外键要重建
  实盘库；后者是回退迁移的删除清单）。同类「本机靠引擎特性、远端没有」的潜在分歧已记录、本轮未处理：
  唯一性冲突语义（本机靠主键冲突、远端靠批内守卫）、`session_bindings` 的复合外键 `(workspace_id,
  project_id) REFERENCES workspaces`（远端无外键，绑定字节只是列）、列级 `NOT NULL`/`UNIQUE` 的逐列等价性。
- 本批文件：`peri-resources/src/sessions/remote/{session_write.rs,session_child_guard_test.rs,mod.rs,
  cloud_deployment_test.rs,cloud_deployment_child_test.rs}`、`peri-resources/src/sessions/sqlite_store
  /{session_rows.rs,session_data.rs}`、`peri-resources/src/sessions/sqlite_store.rs`、
  `peri-resources/src/sessions/sqlite_store/thread_child_delete_test.rs`（新）、`docs/code-index/peri-resources.md`、
  本母 issue。真云测试用既有 `.env` 凭证（测试进程内解析，未进 shell 环境、未打印）。

### 9.28 传输面 49 项实测的结论沉淀（2026-09-27；探测装置已删除）

「一份 schema、两种执行器」的可行性原先由一套一次性探测装置给出（`remote/cloud_transport_probe_test.rs`，
49 项 + 中断清道夫 + 脱敏守护，在授权测试库上建 `peri_probe_%` 合成对象并逐轮清空）。**该文件已删除**：
它是一次性测量装置、不对应任何生产代码，留在仓库里只会被当成要维护的契约测试。结论沉淀在这里。

**可用（实测通过，可作为统一 schema 的表达基础）**：`CREATE TABLE` / `CREATE INDEX` / partial index（真
按 `WHERE` 存）/ `ALTER TABLE ADD COLUMN` / `ALTER TABLE RENAME TO` / `DROP TABLE`（连带索引）；托管批内
DDL 可用且**失败整批回滚**；隐式 rowid 可投影、按插入序、`WHERE rowid > ?` 可用、跨连接稳定；upsert
（`ON CONFLICT … DO UPDATE`）、`UPDATE`/`DELETE … WHERE` 的受影响行数、`RETURNING`、`COUNT`/`GROUP BY`/
子查询；`sqlite_master`/`sqlite_schema`、`pragma_table_info(?1)`（含绑定参数形态）、`PRAGMA table_info`/
`index_list`；TEXT 往返**字节相等**（ascii/cjk/emoji/控制符/**NUL**/转义/SQL 元字符/64 KiB 共 8 档）。

**必须写明的例外（5 条，全部实测为不支持或行为不同）**：

| # | 例外 | 对「一份 schema 两种执行器」的约束 |
| --- | --- | --- |
| 1 | **多语句序列不原子**：同一请求里顺序下发的语句，失败前的部分**留在库里**（对照：托管批失败整批回滚） | 需要原子性的初始化只能走托管批（`apply_schema` 就是这么走的），不能靠「一次请求里多写几条」 |
| 2 | **`PRAGMA foreign_keys` 连接建立时读数为 0**（库未被别的客户端设置过时），违例子行**被接受** | 远端默认**不强制外键**；写入正确性不能建立在外键上 |
| 3 | **没有 `pragma_foreign_key_check` 等价物**（块形式报 0 行、表值形式 `no such table`） | 本机迁移里的「先校验再收尾」闸门在远端没有对应物，远端 schema 变更要自己造校验 |
| 4 | **`PRAGMA foreign_keys` 是跨连接共享的可变状态**：另一条连接写 `OFF` 之后，本连接“立刻”读到 0 | 同一库的写入者之间**没有隔离**；外键开关不能当作连接级配置来依赖（阻塞级例外） |
| 5 | **`PRAGMA user_version` 只能读不能写**（写被服务端拒：`SQL not allowed statement`） | 本机迁移收尾那句在远端无效；`remote/schema.rs` 已改用 `peri_store_meta` 单行承载版本与身份 |

**其它实测事实（信息级，供后续判断）**：建表与删表权限不分层（同一凭证可建可删）；`PRAGMA foreign_keys
= ON` 写生效、行为立即改变（所以「建连接后显式打开」这条路存在，只是它是共享状态、不是安全边界）；
托管批内自带 `BEGIN`/`COMMIT` 被**驱动**在发出 HTTP 之前拒绝（misuse），序列内自带事务控制则被服务端
接受；`PRAGMA user_version`/`foreign_keys` 之外未测 `journal_mode`/`busy_timeout`/`defer_foreign_keys` 等；
限额只探到下限——2000 条语句（约 17 万字节）、4 MiB 单行、2000 行读取均未被拒，**上限位置未定位**，
驱动（reqwest）不设超时，服务端何时掐断在途请求不可判定；服务端错误文本带 `Tursodb error:` 前缀，与
§9.1 记录的引擎身份一致。

设计侧的落点已经落进代码：删除路径两端共用同一份**显式**语句（`session_rows::THREAD_CHILD_DELETES`，
见 §9.27②，例外 2/3/4 是它的依据）；远端身份与版本走 `peri_store_meta`（例外 5）；初始化走托管批
（例外 1）。上表在后续统一 schema 的工作里是**输入**，不是待办。
