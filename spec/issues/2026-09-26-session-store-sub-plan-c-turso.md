# 会话资源拆分 — 子计划 C：Turso Cloud Adapter

> 状态：设计计划；C-01～C-04 已落代码并在授权测试库实测，C-05 第一批（部署入口端到端）与第二批（真进程 `SIGKILL` 演练、P5–P7 实测、删除锚点收尾）已落；剩余为 E 全链路。日期：2026-09-26。
> 实施进度：引擎由授权测试库只读探测确认（`turso://` + 官方域；`GET /version` 404 只是该端点未暴露，
> **不足以**单独断定非 sqld；引擎依据是官方驱动对应关系 + 选定驱动上的 SQL 行为实验），选定
> `turso_serverless` 0.1.3；私有 `sessions::remote` 已落连接、参数绑定、错误脱敏与显式 cloud 入口
> （默认 `#[ignore]`），第五轮加落可变连接、独立 schema 与操作账本，并在授权测试库实跑 5 组最小实验：
> P1（原子批零部分结果）、P2（唯一键冲突可判别、重放返回原收据）、P3（并发同 operation_id 至多一方提交）、
> P4（新连接权威读）与终态封闭记录（封闭先提交则迟到原请求不可能生效）成立。
> P5（SDK 无自动重试的静态审计 + 取消在途调用后仍有 durable anchor）与 P7（收据保留与空间成本）
> 已有观测；**P6 只证到 all-or-nothing**：1/8 MiB 单批都整批生效，单请求上限本身未定位（既未
> 触发「先拒绝」分支，也没有分块实现）。证据见[母需求](2026-09-26-session-store-remote-backend.md)
> §9.8 与 §9.16。§5.0 的两条候选路线中 libSQL 路线未采用（非「判为 Unsupported」）。
> 第四轮（同日）收尾复核证据与下一轮开工清单（data 口可替换、本机事实端口归属、预留面、
> 兼容入口）见母需求 §9.7。
> 上级：[总计划](2026-09-26-session-store-plan.md)。依赖：[A 行为契约](2026-09-26-session-store-sub-plan-a-contracts.md)、[B 本机执行与 SQLite](2026-09-26-session-store-sub-plan-b-local.md)；装配见 [D](2026-09-26-session-store-sub-plan-d-configuration.md)，验收见 [F](2026-09-26-session-store-sub-plan-f-verification.md)。

## 1. 目标与不做事项

实现与 SQLite 满足同一会话行为契约的 Turso adapter。底层连接、SQL、事务、去重和故障收敛全部留在 `peri-resources` 私有实现中；业务端不传事务对象、CAS 参数、重试令牌或远程操作编号。

本期直接读写远程权威数据，不引入 embedded replica、sync/push/pull、本地历史缓存写回、离线续写或双写。执行仍由本机 owner 管理。云端存储可保存本机验证过的 binding 事实，但不运行 Git、不检查目录、不判断进程是否结束。

## 2. 已核实外部证据及限制

2026-09-26 阅读以下官方页面；它们是设计输入，不是对用户目标数据库的运行证明：

| 来源 | 已观察到的描述 | 对计划的影响 |
| --- | --- | --- |
| [Rust Quickstart](https://docs.turso.tech/sdk/rust/quickstart) | 远程 Turso 引擎用 `turso_serverless`，远程 libSQL 引擎用 `libsql` 的 `remote` feature | 不能因为产品名都是 Turso Cloud 就选定同一个 SDK |
| [Rust Reference](https://docs.turso.tech/sdk/rust/reference) | 区分本地、sync、远程客户端；提供交互式事务示例 | 首期只选 over-the-wire 客户端；SDK 有事务方法不证明全部失败语义满足本项目 |
| [SQL over HTTP Reference](https://docs.turso.tech/sdk/http/reference) | 参数绑定、baton、结果数组、连接关闭；页面记载交互事务 5 秒窗口、连接闲置 10 秒关闭 | 不在远程事务中等待 Git、模型、文件发现或用户；数值属于待复核服务限制，不写入领域接口 |
| [官方 HTTP v2 协议](https://github.com/tursodatabase/libsql/blob/main/docs/HTTP_V2_SPEC.md) | 同 stream 串行使用 baton；pipeline 即使前项失败仍执行后续 request | 一组 SQL 放进 pipeline 不等于原子行为；必须验证失败后不会继续执行错误的提交 |

仓库当前使用 `sqlx 0.9.0`（`features = ["runtime-tokio","sqlite"]`）、`reqwest 0.13.4`、`url 2`（`Cargo.toml:51,80,81`，`Cargo.lock` 中无 `libsql`/`turso*` 条目）——review-2 复核确认。依赖与 SDK 精确版本在 C-01 后锁定，不根据未读取的 `.env` 猜测引擎，不预先引入两套 SDK。选定路线见 §5.0。

**review-3（2026-09-26 本轮）重新联网复核**：只抓公开官方页面与 crates.io/docs.rs 元数据，未接触任何用户数据库、未读取 `.env`、未使用凭证。

| 复核项 | 本轮实际观察到的事实 | 影响 |
| --- | --- | --- |
| 引擎 ↔ 远程 SDK 对应 | Rust Quickstart 原文：over-the-network 时 “use the crate that matches your database engine”，`turso_serverless` 对应 Turso databases，`libsql`（`remote` feature）对应 libSQL databases | §5.0 的路线划分成立，不能因产品同名互认 |
| 远程驱动版本 | crates.io：`turso_serverless` 0.1.3（2026-09-04 更新，仓库 tursodatabase/turso）；`libsql` 稳定 0.9.30（`0.10.0-pre.*` 为预发布）；`libsql-client` 0.33.4 停更（2024-01）；`turso` 0.7.2 是本地引擎/`turso::sync` 入口（sync 已被 §1 排除），不是本计划的 over-the-wire 远程入口 | 首期远程候选是 `turso_serverless`（Turso 引擎）与 `libsql` 0.9.30 `remote`（libSQL 引擎）两条；C-01 用「目标库只读探测 + 官方对应关系」二选一并锁定精确版本，预发布限制要记录，不得改用停更的 `libsql-client` 或改成 sync 路线 |
| 原子批处理入口 | docs.rs `turso_serverless` 0.1.3 公开面含 `Builder::new_remote(..).with_auth_token(..)`、`Connection::{query,batch,transactional_batch}`、`Transaction`/`TransactionBehavior`；文档称 `transactional_batch` 为同一 HTTP 请求内的原子批 | §5.1 的“单一原子操作”在选定 SDK 上有对应入口；是否真原子仍须 P1/P2 实测，不因文档措辞直接宣称 |
| 官方默认推荐 | Quickstart 明确 “For most applications, we recommend running a local database with sync (`turso::sync`)”；over-the-wire 远程用于“cannot store a local database file” | 母需求要远程权威数据，本计划**有意偏离**该推荐；实现不得自行改成 sync/embedded replica（§1 不做事项） |
| URL scheme 与引擎 | 该 crate 的 remote 示例 URL 形如 `libsql://my-db.turso.io` | scheme 不能辨识引擎；D §3.2 必须要求显式引擎选择，禁止按 scheme 或端口猜引擎 |

C-01 仍需在获得授权后重跑并记录精确版本、限制与失败证据（本轮只是复核路由与可行性入口，不是引擎语义证明）。

### C-01：实现前可行性闸门

未来获得实施授权后，先确认目标引擎和独立测试数据库，再选择一个 SDK 做最小隔离验证：

1. 参数绑定、64 位整数、UTF-8、NULL、blob/大 payload 与错误分类。
2. 完整行为所需原子操作：中途约束失败时零部分结果，提交后的新连接读取一致。
3. 冷进程读取的权威性；不能把 embedded replica 的 read-your-writes 文档套到远程新连接。
4. SDK 是否自动重试 mutating 请求、如何设置请求时限/禁止不安全重试、取消后后台工作是否仍在运行。
5. 服务对单次请求大小、语句数量、结果大小、事务时限及 schema DDL 的实际限制。
6. 日志/错误是否包含 URL、Authorization、SQL 参数和消息内容，如何在 adapter 边界截断为安全诊断。

这一步只用于确认一个具体实现可用；失败则记阻塞并调整 adapter 内部方案，不能为适应 SDK 改弱 A 的行为后置条件。若改用直接 HTTP，需明确额外协议维护成本、认证转发/路由校验与等价测试，不作为静默兜底。

## 3. 文件与内部职责

建议新增位置（尚不存在）：

| 位置 | 内容 |
| --- | --- |
| `peri-resources/src/sessions/turso/mod.rs` | 数据行为 adapter；构造、访问模式、行为实现与安全错误转换 |
| `turso/connection.rs` | 选定 SDK、连接及请求预算、关闭证据；测试可替换的私有传输 seam |
| `turso/schema.rs` | 远程独立 schema 版本、初始化、能力及兼容性检查 |
| `turso/history.rs` | 追加、完整快照读取、压缩/回退/删除、顺序与派生摘要维护 |
| `turso/initialization.rs` | 完整会话/子会话/派生快照发布，初始化失败处理 |
| `turso/recovery.rs` | 私有发送前登记、结果收敛与恢复阻塞；不导出底层状态机 |
| 相邻 `_test.rs` | adapter 故障、序列化与 schema 行为测试 |

按实现体量再分文件，不为每条方法设一个 module。Cargo 依赖声明由本计划负责，D 仅消费已确定的构造端口。

实际落位（2026-09-26 实施）：连接、端点、凭证与失败分类已落在
`peri-resources/src/sessions/remote/`（`mod.rs` / `connection.rs` / `endpoint.rs` /
`credentials.rs` / `failure.rs`），**未**新建 `sessions/turso/`。下一轮写路径、schema、
`op_ledger` 与恢复收敛在本目录内扩写，不再起第二个远程目录。

## 4. 远程数据组织

远程 schema 与本地 schema 分别版本化，不直接运行本地 `PRAGMA user_version` 升级脚本。复用的应是领域编码和验证，不是一个泛化 SQL executor。

远程版本标记必须是引擎可移植的：优先使用普通表行（例如单行 `store_meta(schema_version, store_id, …)`），不把 `PRAGMA user_version` 当作远程契约——它是 SQLite 方言，选定引擎/驱动是否支持必须由 C-01 实测；未证明前不得写进远程初始化路径。读取版本只读不迁移：版本未知、形状不符或高于本构建接受范围时按 Unsupported 拒绝，不自行 DDL、不猜列形状。

- 保持现有 canonical payload、frozen 与 inherited envelope 字节格式，不为远程改消息模型。
- 会话 metadata、不可变 binding、frozen、inherited、历史 payload/flags 为权威数据；列表摘要、计数和缓存是派生数据，写行为同时维护一致性。
- 显式持久化每个 thread 内的历史顺序，不依赖 UUID 排序或跨库的物理 rowid；MessageId 保持现有语义，独立 fork 仍生成新 ID。
- 远程持久化 `StoreId` 与会话来源 installation 身份；客户端解析定位别名后按同一 `StoreId` 使用本机状态。来源标记不构成跨机认证，写权限仍须限制在已授权宿主。
- 项目/工作区 ID 和列表展示快照可随会话事实保存；它们不是本机文件证据，不能授予执行资格。本机目录证据、OS 锁与 dirty 仍按 B 管理。
- 历史读取一次获得相互一致的 payload、flags、继承区与 frozen/身份视图；跨多个服务调用时一致性由 adapter 内部保证。分页历史若引入，必须固定读取视图，不拼接不同版本。
- 列表按 scope/cursor 在远端过滤，投影不含消息正文、frozen/inherited 大字段；不用本机 registry 全量 join 远程历史。
- 新建/fork/child 输入超出服务原子请求上限时，先明确拒绝；若需要分块，只能内部 staging 后完整发布，未发布数据不可被正常 load/list 消费。不以逐块成功假称整体成功。

## 5. 提交结果未知：内部恢复方案

这是适配实现设计，不是对外接口。A 只暴露「行为已生效 / 确定未生效 / 结果无法确认」及必要的会话恢复行为。

### 5.0 引擎/schema 选择路线（review-2 闭合，证据优先）

| 路线 | 处置 | 理由 |
| --- | --- | --- |
| Turso 远程引擎（Turso Cloud 的 Turso Database 引擎） | **首期远程候选之一**（over-the-wire；驱动 `turso_serverless`）；SDK/协议在 C-01 依据目标库探测结果 + 公开官方资料锁定并记录版本 | 与“远程权威数据 + 事务原子性 + 唯一约束”要求一致；是否就是目标库引擎必须由 C-01 只读探测证实，不预先认定 |
| libSQL 远程引擎（`libsql` 的 `remote` feature） | **同样保留为首期远程候选**；与 Turso 引擎候选实现同一 A 行为契约，由 C-01 按目标库只读探测结果二选一 | 用户只指定 Turso Cloud（产品名），未指定库引擎；官方要求驱动与引擎匹配，因此在探测前不得把任一路线预先判为 Unsupported，也不得用「拒绝另一种」代替交付 |
| embedded replica / sync / 双写 | **明确 Unsupported**，构造时返回类型化错误，不做静默兼容 | 首期不做副本与双写（§1）；这与引擎选择无关，不因选哪条远程路线而变 |
| 用户现有数据库实例 | 只在用户确认的测试库上做只读探测与合成数据实验；不做“已兼容”声明 | 目标库引擎与凭证由用户确认（`.env` 指向测试库）；探测只读、最小化，结果只记脱敏结论 |

配置分辨率必须显式：locator 无法唯一决定引擎时要求显式选择（D §3.2），**不轮流试两种 SDK**、不隐式别名 token 变量名。选定依据是 C-01 对授权测试库的**只读引擎探测**（版本/引擎标识）+ 官方驱动对应关系，不是按 URL scheme 或「另一个更省事」猜；探测前两条候选都算可行，探测后的选择与证据一起记录（只记脱敏结论，不记 URL/token）。

本节外部依据来自 §2 的官方页面记录（review-2 初读，review-3 重新抓取复核；两轮都只读公开页面）；**复核只覆盖路由与入口可行性，不是引擎语义证明**，因此 C-01 必须重新核对所选 SDK 的当前版本、维护状态与限制，并把实际读到的页面与版本写进证据，不能沿用本节的转述当作已复核事实。

远端 binding 行的 `project_id`/`workspace_id` 是本机登记标识而非远端外键（B §6.1 末段）；远端 schema 不建立指向本机表的引用，也不判断目录证据。

### 5.1 首选机制及必要性

选择**发送前本机持久登记 + 远端同一原子操作内的身份资格 + 终态封闭竞争**。原因：只读一次发现目标数据不存在，不能证明旧网络请求以后不会到达；本机取消 future 也不取消远端 SQL。

机制（最小正确形态，**不引入 Open/Applying 多阶段状态机**）：

1. 发送前：adapter 生成内部 operation identity，把 thread/root、行为类别、输入摘要与恢复所需最小信息作为 `kind='mutation_pending'` 锚点写进本机库（B §5.3 的 `session_lifecycle_commitments`），登记失败即不发送。
2. 远端每个 operation 在 `op_ledger(operation_id TEXT PRIMARY KEY, state, digest, receipt, …)` 有一行唯一身份。
3. **资格先于效果**：mutation 的远端执行是**一个原子操作**，其第一步就是取得身份资格——在**同一事务**内先写/占用 `op_ledger` 行（`INSERT`，唯一键冲突即失败），再执行全部业务变更，最后把 receipt 与 `state='applied'` 写入同一行，然后提交。业务变更不在资格之外发生，也不存在“先写数据、后登记”的窗口。
4. **封闭竞争**：恢复端对同一 `operation_id` 执行终态封闭——`INSERT` 终结行/把状态推进到 `closed`（唯一键竞争，`INSERT` 冲突即输）。竞争结果只有两种：
   - 封闭先提交 → 原请求的资格写入必然冲突，其整个事务回滚，**业务变更从未生效且以后也不会生效**；
   - 原请求先提交 → 封闭冲突/读到 `applied`，恢复直接返回原 receipt（幂等），不再执行第二次。
5. 因此“封闭成功”是**写确认过的证明**，不是推断：不需要空查询、不需要超时、不需要读旧请求是否还活着。封闭本身结果未知时保持 `PersistenceUncertain`。
6. 已确认结果后更新本机锚点；客户端在远端成功后、本机结清前崩溃，下次通过 receipt 收敛。
7. 未决期间：禁止同根后续写入、clean、新 owner、普通 dirty reset、删除与 fork（B §4.3 门禁矩阵）。

第三种中间情形（必须显式处理，不能猜）：原请求已写入资格行但尚未提交，封闭方的写入会撞上写锁。此时封闭方只允许三种结论——阻塞到原事务落定后重判、按预算重试后重判、返回 `StillUnknown`；**“写忙/冲突/超时”一律不得当作 `ClosedNeverApplied`**。“封闭成功”的判据固定为：封闭方自己的写入**提交成功**（响应确认，且后续可被冷连接读到）。

封闭可证明性的实现前提（review-3 明确，实现不得偏离）：

1. **同一唯一键空间**：封闭记录与原身份记录必须竞争同一个唯一约束——同一行上的条件状态转移（`… WHERE operation_id = ? AND state <> 'applied'`，`rows_affected = 0` ⇒ 已 applied）或同一唯一键下的插入冲突。另建一张“closed 表/独立索引”不构成互斥：串行化只保证先后顺序，不阻止原请求其后照常提交业务变更，属于设计错误。
2. **资格先于业务效果**：写身份资格与全部业务变更在同一事务内，且资格写在该事务的第一条业务语句之前；不存在“先写数据、后登记”的窗口。
3. **不靠观测推断**：不用一次空查询、一次超时、future 取消或客户端断开推断安全性；这些都不属于 `ClosedNeverApplied` 的证据。

上述安全性依赖以下引擎前置条件，C-01 必须逐条实测（缺一项即保持未决、不放开写）：

| # | 前置条件 | 观测方式 |
| --- | --- | --- |
| P1 | 单一原子操作/短事务的 all-or-nothing 提交，无部分可见结果 | 中途约束失败后零部分结果；新连接读不到中间态 |
| P2 | 唯一主键冲突可判别（错误分类或 `rows_affected = 0`），且冲突方整个事务不提交 | 并发插入同一 `operation_id`：至多一方提交 |
| P3 | 写事务被串行化，无 lost update（不存在两方都“提交成功”的序列化结果） | 交替并发写同一 key，断言最终状态与 receipt 一致 |
| P4 | 提交后的行对新连接/新进程可读（权威读，不是本连接缓存） | 冷进程重读 `op_ledger` 与数据 |
| P5 | SDK 默认不自动重试 mutating 请求；重试必须复用同一 `operation_id` | 读 SDK 重试配置并在故障注入下验证 |
| P6 | 大型 new/fork/child 输入超单请求上限时**先拒绝**；若分块，只能 staging 后由唯一一次“发布”操作取得资格（未发布 staging 对 load/list 不可见） | 超限输入返回明确拒绝；staging 行不可被正常读取 |
| P7 | 收据/封闭记录不按任意 TTL 删除；只有能证明旧请求失效的代际清理才能回收 | 首期不清理，记录空间成本 |

“原请求已开启事务”这一情形由 P2+P3 覆盖：任何长事务要么先提交（封闭失败 → 幂等返回），要么后提交时资格冲突而整体回滚。**不允许**把提交确认建立在“读一次为空”“客户端超时”“future 被取消”之上。不要求实现名为 journal/receipt 的具体表，但替代方案须提供相同的崩溃与晚提交证据。

结果封闭的内部接口（私有，不外泄）：

```rust
enum OperationResolution {
    Applied(Receipt),          // 读到 applied 与 receipt，返回原结果
    ClosedNeverApplied,        // 封闭竞争胜出，证明不可能再生效
    StillUnknown(Reason),      // 封闭也未确认：保持阻塞，不清理、不放行
}
```

### 5.2 边界情况

| 故障点 | 必须得到的行为 |
| --- | --- |
| 发送前本机登记失败 | 确定未生效，无远端副作用 |
| 已登记、尚未发送时崩溃 | 恢复可封闭该操作，不重新构造业务输入盲写 |
| 远端已保存、响应丢失 | 恢复返回原结果；摘要、消息、删除不可执行第二次 |
| 原请求挂起，恢复先封闭 | 原请求迟到不再产生数据变化 |
| 正常结果到达后本机结清失败 | 保守保留恢复阻塞，不能发布 clean |
| 新建/删除目标 thread 不存在 | 操作结果仍能按存储身份和 root/operation 找回，不能依赖目标行还在 |
| 网络长期不可用 | 可见有界失败；不转本地库、不继续生成无界未持久化历史 |

内部恢复记录不得含凭证；若需保存 payload，按本机历史同等级权限管理且不形成第二份可编辑历史。首期**不新建独立的本机恢复数据库**：本机锚点写在 B §5.3 的 `session_lifecycle_commitments`（`kind='mutation_pending'`，`operation_id` + 不透明 `detail`），远端收据写在远端 `op_ledger`；两者都不按任意 TTL 删除——只有能证明旧请求已失效的代际清理才能回收，首期不能证明时保留并记录空间成本，不能牺牲正确性换清理。C-01 未通过前（§5.1 P1–P7 任一未证明），远程写路径保持关闭，只允许只读历史。

## 6. 单宿主与本机执行组合

- 远程 adapter 没有执行 lease 方法。所有业务变更经资源门面取得本机写入准入，再进入 adapter；准入 guard 覆盖真实后台工作完成，不以调用 future 返回/取消为界。
- 恢复未决写入发生在新 owner 放行、ordinary dirty reset、fork/source snapshot 与 clean 之前；B 的执行 dirty 和 C 的持久化未知分别判定。
- 同一远程存储实例身份在云端持久化，URL/SDK路由别名不作为锁主键。首次初始化也需要唯一身份竞争，失败者读取胜者，不各造一个 StoreId。
- 本机登记缺失或安装身份与保存来源不同：仅允许读取历史，拒绝自动接纳为本机 legacy。首期不提供安装身份迁移/复制工具。
- 单宿主限制由配置、来源检查、部署权限一起落实；它不是分布式 lease。共享凭证或复制整个本机状态目录到另一台机器不在安全接管承诺内。

## 7. 行为实现约束

- SDK mutation 重试默认关闭，只有内部相同操作身份确认安全时才重发。
- 正常 RPC 成功仅在整项行为持久化后返回；flush/close 等待 adapter 内部任务，不把「已提交给 worker」当完成。
- 所有请求和收尾均有界；首次保守预算由 C-01/F 的测量确定。超时返回不改变远端结果分类。
- 状态/标题更新是定向行为，不能读取整份 ThreadMeta 后覆盖写；cache 失效属于写行为内部。
- 取消不是 rollback。清理失败必须进入可观察阻塞，并保留唯一资源 owner，不能 Drop 掩盖后台任务仍在工作。

## 8. 任务与退出条件

- C-01：确认引擎/SDK并完成前置实验；记录版本、限制和失败证据。
- C-02：实现私有连接/schema/StoreId；拒绝未知 schema，先验证读路径和只读路径。
- C-03：实现新建、恢复、追加、flags/compact、fork/child、rewind/delete、列表和定向 metadata 行为。
  第一批已落：列表（scoped 分页/children/tree）、一致读取（snapshot/meta/binding/history/flags）、
  新建（meta+binding+frozen）、fork、child、定向 metadata。
  第二批已落：追加（保序 + 批内重复 id 与全局主键冲突拒绝）、投影/flags、compact（flags + 追加 +
  重数计数）、rewind（显式 `KeepThrough`/`RemoveFrom`，未知边界保持无变更）、精确移除（幂等删除、
  跨会话拒绝）、删除会话树、未发布撤销（有子会话拒绝）、legacy 接纳（已有值不变）、child resume
  认领事实（状态派生标记）。写入统一走「一次端口调用 = 一个托管事务批 + 批内守卫」，
  「0 行受影响」不会以成功收场。
  第三批（诚实失败缺口 + 真引擎行为契约）：读取出口校验结果集数量、单行查询不接受多行、
  删树空子树 ⇒ `NotFound`、撤销的子会话计数必须**明确读到 0**；云端实验在真引擎上验证了上述
  行为与守卫的两条分支，并当场抓到两个真实缺陷（rewind 方向与追加/compact 的消息 id 未进操作
  身份摘要 → 不同操作撞同一 `operation_id` 被当成重放静默跳过，已修）。
  未落：`recover_persistence`（属 C-04，需本机未决锚点）、本机执行/登记组合与 D 装配激活（下一批）。
  证据表与未完成项见母 issue §9.11。
- C-04：接内部恢复机制及本机执行协调；覆盖发送前/后、响应丢失、取消、重启和迟到写。
  第一批已落（操作身份与本机操作日志）：每次领域调用铸造唯一操作 id（内容摘要只做一致性
  校验，不再派生身份）；发送前把 id/store/thread/root/行为/摘要登记进本机表
  `session_remote_operations`（schema v7 → v8，登记失败即不发送），确定终态才结清；
  `recover_persistence` 按本机日志向远端账本求证，缺行时用同一 id 做终态封闭竞争。
  真引擎回归：状态 A→B→A→B、标题 x→y→x→y、同 boundary rewind 复用都真的生效，
  每次调用各留一行账本，本机日志全部结清。
  第二批已落（本机执行/登记组合与 D 装配激活）：`MutationGate` 改持三个端口
  （`Arc<dyn SessionDataPort>` + `Arc<dyn LocalExecutionPort>` + `Arc<dyn HostLocalFacts>`），
  远程组合把远端 adapter、本机执行面与同一份本机事实装进**同一个门面**（没有第二套公开行为）；
  `StoreId → HostInstallation → 本机登记 → binding 复核 → root owner` 全链落地：本机没有登记
  或来源不一致时读取可用、执行与写入按 `StoreNotRegistered` 拒绝，已有数据的存储永不自动认领；
  执行代际与 sidecar 锁按 **root** 归属（不为使用旧 SQLite lease 造 `threads` 假行），
  远端绑定与本机 workspace 证据走同一套 `validate_binding_value`；
  `RemoteStoreNotWired` 已删除，`open_deployment` 的远程分支是真装配。
  **未落**：故障注入实测（发送前崩溃/响应丢失/取消/迟到写仍是设计结论）、`recover_persistence`
  之外的崩溃恢复演练、E 全链路消费侧。证据见母 issue §9.13。
  第三批已落（未决收敛接线与统一门禁）：`recover_session_persistence` 接成门面级收敛路径
  （先等本进程在途写入，再按本机日志里的原 id 向远端账本求证；缺行时用同一唯一键做封闭竞争；
  不需要调用方令牌）；未决阻塞范围按后端解析（`LocalExecutionPort::pending_scope`：
  远程会话的 root 只能由远端父链给出，否则同根子会话的首次写入会漏过门禁），
  门面按「id ∪ root」取并集；`mark_clean`/`drain`/写入门禁读同一条未结清谓词，
  ordinary dirty 的解除不解除远程未决；故障注入面（`FaultPlan`，仅测试构建）落在真实批上，
  真引擎回归覆盖「响应丢失 → 已生效」「发出前消失 → 封闭 → 迟到原请求整批回滚」
  「root 未决阻塞同根子会话首次写入 → 恢复后放行」「重启（同一份本机日志的新打开）+ 目标
  被另一份安装删掉 → 按账本收敛、目标不被重建」，本机事实端口上的注入覆盖「登记失败 ⇒
  不发送且不是未决」与「结清失败 ⇒ 保留阻塞、恢复判已生效」。**未落**：删除墓碑在恢复后的
  收尾（不影响未决判定与 identity 终态）、真实进程 kill 演练、E 全链路。证据见母 issue §9.14。
- C-05：接 D 装配、E 全链路，用 F 确定性测试和显式云实验分别验收。
  第一批已落（部署入口端到端）：新增 `remote/cloud_deployment_test.rs`（父进程拉起子进程 +
  用新连接在阶段之间核对）与 `remote/cloud_deployment_child_test.rs`（写入 / 冷恢复 / 只读三
  个进程阶段），全部经 `Resources::open_deployment` 真实入口、temp HOME 与合成 workspace：
  创建 → 追加/排空 → compact → fork → child → 标题 A→B→A → close → **新进程**冷恢复
  （未决收敛 → ordinary dirty → rewind → 删树）→ 两种本机状态的显式只读（全新 HOME 一个文件
  都不建、已登记 HOME 写入按 `ReadOnlyStore` 拒绝，两者都不新增行与账本）。落在完整装配路径上
  的两条事实：标题 A→B→A 落到第三次的值；fork 复用 source `message_id` 被明确拒绝。
  同时清掉「已落地、消费方未接入」的残留标记（`SessionStoreOpenRequest` 的三个访问器、
  桥的 `data_port`）。**未落**：E 全链路消费侧接入、真实 kill 演练。证据见母 issue §9.15。

退出条件：两 adapter 使用同一领域结果断言；真实 Turso 冷恢复成立；未知结果不会让旧写与新 owner 并发；业务接口没有新增存储机制。任何一项未证明，保持未完成。
