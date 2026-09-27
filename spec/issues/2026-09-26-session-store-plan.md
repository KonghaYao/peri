# 会话资源门面拆分与 Turso Cloud — 总设计计划

> 状态：**Verify / 实现与缺陷修复已提交**。Fable 已对 `4ba2eefe` 独立复验：原具体 P1/P2 反例通过，本机与已登记存储范围的云端行为通过；真实首登 `Created`、双真实 store 同 root E2E 和大历史上限仍待证。当前状态与命令证据以[母需求文首“最新独立验收”](2026-09-26-session-store-remote-backend.md)为准，不以本计划的历史批次状态推断尚未实现或无条件完成。
> 授权：用户已授权实施、使用 `.env` 中的测试库做合成数据验证及阶段性提交。凭证仅由测试进程安全解析；无真实会话上传，不自动 push，独立 WIP 保持原状。
> 本文后续 review/W0/A/B 段为当时设计与阶段证据，保留供核对；远程当前选用 `turso_serverless 0.1.3`，不将历史 SDK 候选或“未连接/未提交”记录当作当前状态。
> 母需求：[会话资源门面拆分与远程存储 Adapter](2026-09-26-session-store-remote-backend.md)。本组计划属于 active spec，不替代现行 standards/design。
> **2026-09-27 用户裁决撤销了本组的「本机登记 / 准入裁决」设计（配置即用；远端库 = 本地库的同一种模式）**：显式登记/接管入口与启动探测都不再存在，`session_store_registrations` 等本机远程痕迹表由 v10 删除。最新支持边界见母需求「支持边界」段；本文件与各分计划的登记相关段落是当时设计与阶段证据，保留供核对，不代表当前行为。

## 1. 交付目标

完成会话资源门面的整体职责拆分：消费侧统一依赖行为门面；数据端口由 SQLite/Turso Cloud adapter 实现，本机执行端口独立持有发现、绑定验证、owner/dirty 和排空事实。首期远程是本机执行 + 远程权威数据，不是跨机执行、同步、备份或离线双写。

接口按会话行为定义，事务、CAS、SQL batch、隔离级别、重试令牌与补偿协议不进入门面或 adapter 数据端口。原子性、持久化确认、顺序和诚实失败是必须保留的行为保证，不因隐藏机制而删除。

## 2. 分计划索引与事实源

| 计划 | 范围 | 计划事实源 |
| --- | --- | --- |
| [A：行为契约与纯逻辑](2026-09-26-session-store-sub-plan-a-contracts.md) | 门面/内部端口、输入输出、错误/能力、旧方法映射、纯变换 | 公共行为接口与结果语义 |
| [B：SQLite 与本机执行](2026-09-26-session-store-sub-plan-b-local.md) | 共享 SQLite 内核、registry/lease/dirty、guard、创建准入、本机兼容 | 本机事实与执行授权 |
| [C：Turso adapter](2026-09-26-session-store-sub-plan-c-turso.md) | 官方能力证据、SDK闸门、远程数据组织、私有确认与恢复 | 远程实现与故障收敛 |
| [D：配置与装配](2026-09-26-session-store-sub-plan-d-configuration.md) | locator/凭证、Resources 打开、CLI、TUI/print/stdio/meta、只读 | 部署输入与后端选择 |
| [E：消费侧迁移](2026-09-26-session-store-sub-plan-e-consumers.md) | Agent transcript/subagent、ACP 生命周期、Controller/middleware、无旁路迁移 | 调用顺序和生命周期整合 |
| [F：验证与云实验](2026-09-26-session-store-sub-plan-f-verification.md) | 现有回归、行为矩阵、故障注入、真实云测试、性能/清理证据 | 验证入口与证据口径 |

同一语义不在多个子计划独立裁决。接口变更先改 A；本机/远程实现只能在接口保证内选择机制。本文负责总范围、依赖、文件所有权和开工闸门。

## 2.1 review-2 闭合索引（12 项）

独立审阅提出 12 个开工前必须闭合的问题；下表记录本轮以当前代码核实后的关闭位置。判定为“审阅要求过度”的部分一并写明，避免后续实施者机械照做。

| # | 问题 | 结论与落点 |
| --- | --- | --- |
| 1 | 公共依赖图与字段最终类型 | 已闭合：门面链与全部最终字段见 [A §2.1](2026-09-26-session-store-sub-plan-a-contracts.md)；`Resources`/`Controller.sessions` 返回 `Arc<dyn SessionResources>`，`AcpServerConfig.thread_store` 删除，`SessionExecutionLease` 保持两项公共方法（不是事务句柄） |
| 2 | schema 6 与删除后证据被 cascade 抹掉 | 已闭合：v6 → v7 迁移矩阵、`execution_runs` 去外键、删除墓碑见 [B §4.4/§5.3](2026-09-26-session-store-sub-plan-b-local.md)；选择独立锚点而非“删除前全部收敛”的证明 |
| 3 | C 的 operation 收据封闭可证明性 | 已闭合：资格先于效果 + 同一原子操作 + 终态封闭竞争同一 identity，引擎前置条件 P1–P7 见 [C §5.0/§5.1](2026-09-26-session-store-sub-plan-c-turso.md) |
| 4 | 创建顺序、creation intent、崩溃点、SavedButNotAdmitted | 已闭合：状态机与崩溃点表见 [B §5.1/§5.2](2026-09-26-session-store-sub-plan-b-local.md)；SQLite 同库塌缩为一个事务，远程明确为“durable 数据 + 本机准入”非分布式事务 |
| 5 | PreparedSessionInputs 固定输入、lease 前只读 | 已闭合：字段表、只读规则、插件 manifest 修复分离、new/legacy/fork/child 四条路径见 [E §3.2](2026-09-26-session-store-sub-plan-e-consumers.md) |
| 6 | locator→StoreId→登记→binding→证据→owner 状态表 | 已闭合：[B §6.1](2026-09-26-session-store-sub-plan-b-local.md) 八环矩阵（允许的读、执行前置、必须拒绝）；只读允许读 schema/StoreId/binding，无写探测 |
| 7 | 写 guard 只在效果确定时 finish；pending 门禁统一 | 已闭合：三态 `MutationOutcome` 见 [A §5.4](2026-09-26-session-store-sub-plan-a-contracts.md)，guard 规则与门禁矩阵见 [B §4.1/§4.3](2026-09-26-session-store-sub-plan-b-local.md) |
| 8 | AccessMode/DataCapabilities/ExecutionAvailability 独立 | 已闭合：三个枚举与不变量见 [A §5.3](2026-09-26-session-store-sub-plan-a-contracts.md) |
| 9 | 迁移矩阵（v6 数据、dirty、回滚、只读、future） | 已闭合：[B §5.3](2026-09-26-session-store-sub-plan-b-local.md) 迁移矩阵 + F 的 V-18 |
| 10 | F 的精确命令与非零命中 | 已闭合：[F §6/§6.1](2026-09-26-session-store-sub-plan-f-verification.md)；目标不存在时 cargo 退出码 101 已实测 |
| 11 | 旧 ThreadStore 以编译证据退出 | 已闭合：[A §7.1](2026-09-26-session-store-sub-plan-a-contracts.md)（符号删除 + 全 target 编译；`Controller.sessions` 名称保留）+ F 的 V-15 |
| 12 | FilesystemThreadStore 非完整实现；child frozen 取原字节 | 已闭合：[A §7.1-6](2026-09-26-session-store-sub-plan-a-contracts.md)（`HistoryReadOnly`、no-op 删除）、[E §6](2026-09-26-session-store-sub-plan-e-consumers.md)（不可变 parent/root 已持久化 frozen 字节） |

未采纳的审阅倾向（保留最小正确机制，不额外建框架）：不为“封闭竞争”增加 Open/Applying 多阶段状态机（唯一键竞争 + 同一事务已足够，前提是 P1–P3 成立）；不为“删除前收敛”增加分布式证明（改为墓碑锚点）；不新增 crate、不做通用 UnitOfWork/事务 DSL。

### 2.2 review-3 复核（2026-09-26，本轮）

上一轮的闭合文本逐条以当前代码复核，并补上查证中发现的缺口（只改计划，不改实现）：

| 复核动作 | 结果 |
| --- | --- |
| §2.1 的十二项落点逐条对照代码 | 主张与代码一致：`Resources.thread_store`（`context.rs:19,101`）、`HostAssemblyInput.thread_store`（`assemble.rs:123`）、`AcpServerConfig.thread_store`（`host/mod.rs:194`）与 `cfg.controller.sessions()` 双路径（`controller.rs:172,290`）、`SessionExecutionLease` 仅两方法（`workspace.rs:209-213`）、schema 6 与三处 `ON DELETE CASCADE`（`schema.rs:11,148,202,211`）、连接启用 `foreign_keys=ON`（`connection.rs:146`，级联确实生效）、`finish()` 无条件调用（`sqlite_store.rs:402-434` 等多处）、`completed: bool`（`execution.rs:33-51`）、`ReadOnlyAdmission` 三原因（`workspace.rs:141-153`）、`FilesystemThreadStore` 静默 no-op（`filesystem.rs:452-463`）、E §2.1 的旁路行号（`persistence.rs:171-191`、`session_fork.rs:50,117,143-172`、`claim.rs:105-140`、`prediction.rs:19,101`）均在位 |
| 需要修正/补齐的落点 | A：`CommandContext` 实体在 `peri-acp-types`（`command.rs:79,111`，`peri-acp` 再导出）；补 `SubagentHost`/`SubagentSpawnConfig`/`SubagentResumeConfig`（`subagent/types.rs:100,183,388`）、middlewares `spawn_context`/`configuration`、TUI `Services` 三处生产字段的最终类型；`filesystem.rs` 行号改 452-463。B：v7 去掉 `execution_runs` 外键后，删除路径必须同事务显式删除执行行（否则留下孤儿 dirty 行），已写入 §4.4/§5.3；`mark_clean` 容忍分支行号改 `execution.rs:77-98`。C：封闭记录必须与原身份记录竞争**同一唯一键空间**（另建表不构成互斥），已写入 §5.1；远程版本标记改用引擎可移植的表行，不假设 `PRAGMA user_version`（§4）。D：`libsql://` 同样不能辨识引擎（§3.2）。E：date/env 一次定格、装配不重探（§3.2） |
| 外部证据 | C §2 四条官方页面描述本轮重新抓取复核，全部一致；新增 crates.io/docs.rs 事实：`turso_serverless` 0.1.3（pre-1.0，对应 Turso 引擎）、`libsql` 稳定 0.9.30（`0.10.0-pre.*`，`remote` feature 对应 libSQL 引擎）、`libsql-client` 已停更、官方推荐本地+sync 而被本计划有意排除（sync 排除与引擎选择无关） |
| 本机基线 | 隔离 `HOME` 到临时目录后复跑，结果与 review-2 一致：`--list` 156 tests / exit 0；`--lib` 156 passed / 0 failed / exit 0（8.03s） |
| B 数据侧后的基线（2026-09-26） | `cargo test -p peri-resources --lib` → 182 passed / 0 failed（156 + 22 数据面 + 4 v7 迁移）；`cargo test -p peri-acp-types --lib` → 459；`-p peri-acp --lib` → 713；`-p peri-agent --lib` → 868；`cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo check --workspace --all-targets` 均 exit 0 |
| B 执行侧后的基线（2026-09-26） | `cargo test -p peri-resources --lib` → 199 passed / 0 failed（182 + 17 门面）；`cargo test -p peri-resources --test session_resources_contract` → 6 passed / 0 failed（F §6 固定目标名，不再是不存在的目标）；`-p peri-acp-types --lib` → 460；`-p peri-acp --lib` → 713；`-p peri-agent --lib` → 868；clippy 全 target exit 0。生产路径仍走 `ThreadStore` 桥，消费侧切换属 E |
| 目标不存在 | 按 F 的精确命令复跑：`session_resources_contract` 与 `session_resources_turso -- --ignored --list` 均 exit 101（`no test target named …`），与 §6.1 记录一致。**更新（B 执行侧）**：`session_resources_contract` 目标已按 F 的名字建立（`peri-resources/tests/`），`--list` → 6 tests、运行 6 passed；`session_resources_turso` 仍不存在（属 C/F） |

## 3. 现场证据与设计影响

| 已核对事实 | 影响 |
| --- | --- |
| `ThreadStore` 已经 trait 注入，但包括工作区/lease/dirty 与数据方法；`peri-acp/src/host/mod.rs:194` 与 `controller.rs:172` 是同一 Arc 的两条路径 | 不是再套一层 wrapper；必须迁移所有写调用并封闭 raw adapter，且消除双路径 |
| `Resources::open_with`、`open_thread_store_read_only`、`App::new(db_path)`、`cli_print`、stdio、meta 各自绑定 SQLite 路径 | 全部启动入口纳入 D，meta 的早启动不能牺牲 |
| SQLite 当前 schema 为 6（`schema.rs:11`），`execution_runs`/`session_bindings`/`messages` 对 `threads` 均 `ON DELETE CASCADE`（`schema.rs:148,202,211`） | 不能在保存 thread 前直接取得旧 lease；删除会抹掉 dirty 证据 → v7 墓碑锚点 |
| 新建目前是 resolve → `create_bound_thread` → lease → `SessionEnvironment::assemble` → frozen → 保存（`session_lifecycle.rs:520-620`），失败逐次 `delete_thread` 补偿 | E 拆 frozen 输入准备，A/B 提供完整 new/fork/child 行为 |
| `build_legacy_frozen_data` 不启动 session 资源，但插件发现会经 `try_generate_synthetic_manifest_fallback` 往插件缓存写 `plugin.json`（`loader.rs:91-140`，经 `generate_synthetic_manifest` 落盘） | 不能宣称它是纯函数；lease 前准备要移除写副作用（只读加载入口） |
| `ExecutionWriteGuard::finish()` 在所有 mutation 的正常返回（含 `Err`）后无条件调用（`sqlite_store.rs` 多处），Drop 只置 `mutation_uncertain` | 三态 `MutationOutcome`；只有效果确定才 finish |
| compact 已有原子 lifecycle 和 Unknown 热态失效；消费侧 `persistence.rs:175-191` 逐条写 flags 后另行 invalidation；writer 用 unbounded channel + 64 条/100ms 批量 | 沿用可信恢复方向，收口为完整行为，补 slow network 积压预算 |
| SQL 默认 no-op/假缺失真实存在（`store.rs` 的 `update_message_flags`/`delete_messages_since`/`load_message_flags`/`get_context_cache_epoch` 默认体；`filesystem.rs:452-459` 静默 no-op） | adapter 行为测试要排除假成功；不支持必须显式失败 |
| Rust 官方资料区分 Turso 与 libSQL 引擎、两种远程 SDK（C §2） | 远程路线是 over-the-wire 权威读写；`turso_serverless`（Turso 引擎）与 `libsql` remote（libSQL 引擎）都保留为候选，由 C-01 按目标库只读探测 + 官方对应关系二选一（C §5.0）；sync/replica/双写仍显式 Unsupported |
| 仓库依赖为 `sqlx 0.9.0`/`reqwest 0.13.4`/`url 2`，无 `libsql`/`turso*` 条目（`Cargo.toml:51,80,81`） | 不在计划阶段预引入 SDK |
| 远程驱动候选两条：`turso_serverless` 0.1.3（pre-1.0）对应 Turso 引擎；`libsql` 0.9.30 对应 libSQL 引擎；官方默认推荐本地+sync（review-3 联网复核） | 用户只指定 Turso Cloud（产品名），未指定库引擎：SDK 由目标库只读探测结果 + 官方事实选定，探测前不预先排除任一条；不因 SDK 年轻或官方推荐而改用 sync/双写，未证明即保持远程写关闭 |
| HTTP v2 pipeline 在前项失败后仍执行后项 | pipeline 不是自动事务；批处理与原子行为须单独验证 |

以上来自源码/测试静态核对和公开文档，不是本次运行通过证据。外部来源集中在 C；本轮没有访问用户账号或实际 URL。

## 4. 总体设计决策

### P-01：一个生产门面，两个内部职责

`peri-acp-types` 定义 `SessionResources` 行为契约；`peri-resources` 实现并装配内部数据/本机执行职责。Controller 与 session context 引用同一门面；业务不可取得裸数据写端口。Runtime 不新增持久化状态。

### P-02：完整行为而不是通用事务

新建、legacy 接纳、fork、child、compact、projection、rewind/delete 各有完整领域输入与后置条件。领域侧做确定性变换，adapter 不做 compact 算法或 frozen 渲染。禁止通用操作列表/SQL executor/UnitOfWork 外露。

### P-03：Binding 事实与验证分开

不可变 binding 与会话/frozen 在数据端保持原子关系；本机 registry 持有项目/工作区位置证据及执行资格。SQLite 可保持同库，Turso 不要求远端理解 inode/Git。

### P-04：明确数据保存与执行准入不是一个跨系统事务

完整数据保存后才建立可执行会话；本机失败可留下完整历史但不得发布执行成功。私有创建意图和锁预留用于恢复，不让 ACP 拼存储补偿。已有资源创建/关闭仍由其原 owner 管理。

### P-05：未知写入与 ordinary dirty 分离

C 的内部协议证明结果已生效或不可能再生效后，才能重新放行写入；一次空读取、超时、取消或用户同意 dirty 风险都不足。判定手段是**封闭记录与原始操作竞争同一唯一身份**（C §5.1）：封闭提交成功即证明原操作不可能再生效，封闭本身未确认则保持阻塞。公开只表达 session 级恢复结果，不暴露 operation token。

### P-06：默认本机路径保守迁移

仍使用 `~/.peri/threads/threads.db`、既有历史/只读兼容语义，同库共享 pool；不引入云网络或第二份本机历史。远程模式下同一本机库只保存本机执行事实（registry、`execution_runs`、锚点、登记），canonical 历史仍在远端，不新建第二个本机数据库文件。旧生产 trait 不作为最终兼容路径，退出以符号删除 + 全 target 编译证明。

### P-07：首期单宿主远程范围

专用远程库/授权写入范围、稳定 StoreId 与本机登记配对；外来会话或 registry 丢失只读，不自动 legacy 接纳，也不自动初始化已有数据的存储。OS锁不承诺跨机互斥，禁止外部 writer/旧二进制混用同一可写范围。

## 5. 实施前闸门与未确认项

本轮不打断规划要求用户提供 secrets；以下在相应施工阶段前解决：

| 闸门 | 待确认/验证 | 阻塞范围 |
| --- | --- | --- |
| G-01 | 本组行为与事实归属设计评审（含完整创建与 Unknown 语义） | **已解除**（review-2 闭合 12 项，见 §2.1）；剩余实现级细节不构成开工阻塞 |
| G-02 | 目标数据库引擎与 SDK/版本，及其原子行为/权威读/晚提交恢复可行性 | 已缩小：远程路线为 over-the-wire 权威读写，候选 SDK 为 `turso_serverless`（Turso 引擎）与 `libsql` remote（libSQL 引擎）两条（C §5.0/§2），由 C-01 对授权测试库只读探测后在两者中选定一条并锁定精确版本；sync/replica/双写显式 Unsupported。仍需 C-01 实测 P1–P7，未证明前远程写保持关闭 |
| G-03 | 独立测试数据库写入/清理授权，以及 `TURSO_TOEKN` 拼写是否确认 | **blocked**：用户未作答（cloudAuthorized=false）。只交付 adapter/显式云入口/本机确定性验证；不读 `.env`、不连接；变量名只按原样记录，不设别名 |
| G-04 | frozen 预备输入可无写副作用准备，并与后续装配复用 | 设计已闭合（E §3.2：只读加载入口 + 同一对象消费），实现待验证 |
| G-05 | 远程未知写入能恢复终态并阻止迟到写；本机多进程/别名锁域证明 | 设计已闭合（C §5.1 + B §6.1），证据待 C-01/F；未证明只保留读/阻塞，不关闭 issue |
| G-06 | 初始性能与积压基线，确定实际条数/字节/时限预算 | 远程默认可用性承诺；不凭经验预填 SLA |

不把外部待证事实伪装成已定 API。涉及首期多机接管、离线模式、自动导入旧历史等范围变更需单独裁决。

## 6. 批次与依赖

```text
W0：F-01 基线 + C-01 可行性证据 + A 接口/纯逻辑评审
     （review-2 已完成：设计评审闭合 + 本机基线，见 §2.1/§9；C-01 与云授权仍未完成）
          ↓
W1：A 行为契约 + B SQLite/本机门面实现
          ↓
W2：E 本机端到端迁移 + D 统一 locator/只读入口
          ↓
W3：B 远程 registry + C Turso 行为/内部恢复 + D 远程 factory
          ↓
W4：F 默认故障测试 + E 全链路 adapter 切换验证
          ↓
W5：显式 Turso Cloud 实验、成本/清理证据、事实源更新
```

W0 中外部权限未就绪可先完成纯设计/本机基线；但不得跳过 C-01 开始臆测远程实现。共享文件顺序施工，每批保持可编译/可测；短期旧 wrapper 退出清单绑定 W2，不允许 W5 仍存在 raw store 生产旁路。

## 7. 文件所有权与协作约束

| 文件域 | 主负责计划 | 接续方 |
| --- | --- | --- |
| `peri-acp-types/src/store.rs`（删除 trait、保留 payload/flags/继承编码）、新 `session_resources`/history 类型及 crate 导出 | A | E 更新消费字段时按 A 接口，不独立改结果模型 |
| `peri-acp-types/src/workspace.rs` | A 领域错误/执行类型 | B 行为实现、E 协议映射顺序接续 |
| `peri-resources/src/sessions/*` 本机/门面/registry | B | C 新增 Turso；不同时改共用门面 |
| `peri-resources/src/context.rs`、只读入口 | B 先完成门面返回 | D 接管外部 open request/factory；C 只提供构造函数 |
| `peri-resources/src/sessions/turso/*`、workspace/resource Cargo 依赖 | C | F 加测试和锁定证据 |
| TUI CLI/launch/print/meta、Agent resource wrapper、stdio 外部参数 | D | E 装配内部句柄引用顺序接续 |
| Controller、ACP lifecycle/dispatch、Agent transcript/subagent、middlewares bridge | E | F 契约/链路覆盖 |
| 新契约/云实验测试目标及证据组织 | F | 各计划自带相邻回归，不把测试拖到最后 |

本次规划开始时已有 `.github/workflows/ci.yml`、`peri-middlewares/src/mcp/mod.rs`、`peri-middlewares/src/mcp/builtin_spike_test.rs`、`peri-cool` 的非本任务改动；未来开工重新核对，不覆盖、不混入提交。共享工作树内各阶段串行，开工前先看前序 diff。

## 8. 验收和文档收尾

完成要求不是“七份文档已勾完”，而是：

- A 接口无底层机制，业务不按后端分支；新旧写入旁路收敛（符号删除 + 编译证据）。
- B 保住 SQLite 的历史、schema、只读、owner/dirty、legacy、close 与成本基线；v7 迁移保留 dirty 且可回滚。
- C 对完整会话行为及未决写入有故障证据（含封闭竞争的 P1–P7）；D 所有部署入口一致且不泄露凭证。
- E 的 Agent/ACP/Controller/middleware 使用同一路径；new/fork/child/compact 不在业务侧手工补偿。
- F 的确定性矩阵与真实 Turso 冷恢复、性能、清理证据分开记账；任何未运行/缺凭证/0 tests 都不算通过。

实现时按 DOC-UPDATE-001 更新受影响的 architecture contracts、身份设计、code-index、模块路由与测试命令；本轮仅添加 active plans，不提前把它们写成已实现的权威设计。未来验收记录在获得实施授权后建立，不在本轮制造空的“已验收”文件。

## 9. review-2 / review-3 进度与证据（2026-09-26）

review-2 已完成独立审阅 12 项的闭合（§2.1）与当时基线（`cargo test -p peri-resources --lib` → exit 0，156 passed / 0 failed，7.37s；两个目标不存在 → exit 101）。review-3 为同一日期的复核轮：只做设计闭合与事实核对，不改实现。记录如下，供后续阶段直接引用：

| 项 | 结果 |
| --- | --- |
| §2.1 十二项 | 落点逐条对照当前代码复核；修正/补齐 9 处落点与措辞（见 §2.2），无被推翻的结论 |
| 以代码核实的关键事实 | 见 §3；本轮新增 `foreign_keys=ON`（级联确实生效）、`mark_clean` CAS 与容忍分支（`execution.rs:59-104`）、`.execution-locks` sidecar（`execution.rs:118-121`）、插件合成 manifest 落盘、`MessageTranscript.store`（`transcript.rs:180`）、`command/mod.rs:36` 再导出、`workspace.rs:71` 装配复用、`SESSION_BINDING_VERSION=1`（`workspace.rs:44`） |
| 本机基线 | 隔离 `HOME` 到 `mktemp -d`（保留真实 `CARGO_HOME`/`RUSTUP_HOME`）后：`cargo test -p peri-resources --lib -- --list` → exit 0，156 tests；`cargo test -p peri-resources --lib` → exit 0，156 passed / 0 failed（8.03s）。无预存在失败 |
| 不存在的测试目标 | `cargo test -p peri-resources --test session_resources_contract -- --list` → exit 101；`cargo test -p peri-acp --test session_resources_turso -- --ignored --list` → exit 101（现有目标仅 `concurrent_bg_agent_test`/`integration_test`/`prompt_cache_boundary`） |
| 外部证据 | 仅抓公开页面与包里元数据（`docs.turso.tech` 的 Rust Quickstart/Reference、SQL over HTTP Reference、libSQL HTTP v2 规范、crates.io/docs.rs）；未读 `.env`、未连接任何数据库、未使用任何凭证；C §2 的四条描述与本轮抓取一致，另加版本与 API 面事实 |
| 云实验 | **blocked**：cloudAuthorized=false（G-03），本轮未执行任何云命令；`TURSO_URL`/`TURSO_TOEKN` 仍只按原样记名 |
| 仍属未验证 | C-01 的引擎/驱动精确版本与 P1–P7 实测、远程端到端链路、性能/积压预算、v7 迁移与墓碑的实际运行证据（实现阶段产出） |

下一步（不在本轮授权内）：按 §6 的 W1 起开工，先落 A 的接口与 B 的 SQLite/v7 迁移，再按 W2→W5 推进；每批以 F §6 的精确命令取证，`--list` 非零才开始。远程写路径在 C-01 通过前保持关闭。
