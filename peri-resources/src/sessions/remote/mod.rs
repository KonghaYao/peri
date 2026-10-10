//! 远程会话存储（Turso Cloud，over-the-wire）——私有实现，不进入公开 API。
//!
//! 边界：凭证、端点、SDK 类型、请求预算与失败分类都只在本模块内出现；业务侧只看到
//! 行为结果（A：事务、CAS、SQL batch、连接与重试令牌不出行为接口）。
//!
//! ## 引擎与驱动选择（C-01 只读探测，2026-09-26）
//!
//! 在用户确认的测试库上只读执行（`GET /version`、`POST /v2/pipeline` 的只读 `SELECT`）：
//!
//! - locator scheme 为 `turso://`，host 属于官方 Turso Cloud 域；
//! - `GET /version` 返回 **404**。该端点在官方文档里是 libSQL/sqld 的版本身份入口，
//!   但 404 **不能单独证明目标库不是 sqld**（服务端可以不暴露该路由或版本不同）；
//!   引擎身份的依据是「官方驱动 ↔ 引擎对应关系 + 选定驱动上的 SQL 行为实验」，不是这个端点；
//! - `POST /v2/pipeline` 返回 200 且 `results[0].type = ok`：SQL over HTTP 可用、Bearer 认证通过；
//! - 同一请求内的参数绑定回环成立：text、64 位整数（9007199254740993，超出 f64 精确范围）
//!   与 NULL 均原值返回。
//!
//! 与官方 Rust Quickstart 的对应关系（Turso 数据库用 `turso_serverless`，libSQL 数据库用
//! `libsql` 的 remote feature）一致，因此选定 **`turso_serverless` 0.1.3**
//! （2026-09-04 发布；依赖 reqwest 0.13 / tokio 1 / thiserror 2，与工作区既有版本同族）。
//!
//! ## 本模块当前能证明什么
//!
//! 已实现（C-02 部分 + §5.1 机制）：私有只读连接、参数绑定、失败分类与脱敏；
//! **可变连接**（`mutation::RemoteStore`，请求预算、只读拒绝、确定性三分类）；
//! **独立 schema**（`schema`：`peri_store_meta` 单行版本/契约/store 身份，只读检查 +
//! 显式初始化竞争，未知 schema 不覆盖）；**内部操作账本**（`ledger`：资格先于效果、
//! 同唯一键空间的终态封闭、私有操作 id 与收据）。
//!
//! ## 两种存储模式现在说同一份形状（2026-09-27 统一）
//!
//! 远端不再有自己的会话表：`threads` / `messages` / `session_bindings` / `projects` /
//! `workspaces` 与本机 SQLite 共用 canonical 形状，**DDL 与删除语句的唯一来源是 `sessions::canonical`**
//! （逐条建表/建索引清单、`THREAD_CHILD_DELETES`、`DELETE_THREAD_ROW_SQL`、`payload_role`），
//! canonical 历史顺序也统一到 `messages.rowid`。远端只剩执行器自己的机制表（`peri_op_ledger`
//! 幂等账本、`peri_store_meta` 版本标记），版本值与本机 `CURRENT_SCHEMA_VERSION` 同源。
//! 旧形状的库（`peri_sessions` 那套，或持 `peri.session.store/v1` 契约）一律**拒绝、不迁移**；
//! v2 的 `machines` / `workspaces` 在远端保存 Session 归属，执行目录发现快照随绑定保存在远端。写打开会把 `PRAGMA foreign_keys` 归位；引用完整性由显式写入/删除顺序保证。
//!
//! 已实现（C-03 第一批，`session_data` + `session_read`/`session_write`/`session_sql`/
//! `session_codec`/`session_schema`）：会话表 schema、一致读取（snapshot/meta/binding/
//! history/flags）、scoped 分页与 children/tree、新建（meta+binding+frozen）、fork、child、
//! 定向 metadata 更新，以及幂等 schema 补建与真正的连接关闭。
//!
//! 已实现（C-03 第二批，`session_history` + `session_lifecycle`）：追加历史（保序、批内重复
//! id 与全局主键冲突拒绝）、message projections、compact（flags + 追加 + 重数计数）、rewind
//! （显式两边界，未知边界保持无变更）、精确移除、删除会话树、未发布撤销（有子会话拒绝）、
//! legacy 接纳（已有值不变）、child resume 认领事实。写入统一走「一次端口调用 = 一个托管
//! 事务批 + 批内守卫」，0 行受影响不会以成功收场；读取端对「回复不完整」按错误处理（结果集
//! 或行数不符不会被当成「没有数据」）。
//!
//! 已撤销（2026-09-27 用户裁决，v10 回退）：**本机远端操作日志**（`session_remote_operations`
//! 与 `sqlite_store/remote_operations.rs`：操作 id、发送前的 durable 锚点、终态结清、按本机
//! 日志向远端账本求证）、**本机登记与接纳链**（`remote/{registration,local_execution}.rs` 与
//! `HostLocalFacts`：`StoreId → 本机安装身份 → 登记 → binding 复核 → root owner`），以及
//! **真进程强杀演练**（`cloud_kill_*_test.rs`：死点原先就设在被删除的本机事实端口上）。
//!
//! 现行语义是**配置即用**：门面按**两个**端口组合（`Arc<dyn SessionDataPort>` +
//! `Arc<dyn LocalExecutionPort>`），远程组合装配在 [`composition::open_remote`]，并由
//! `Resources::open_deployment` 在远程 locator 上真实接通（见 `context.rs`）。配了哪个 store
//! 就直接用哪个，不再有本机 store 登记、准入裁决与启动探测；Turso 模式不打开本机 SQLite，
//! 工作区发现由执行端口完成。没有本机锚点之后
//! `recover_persistence` 收敛为「会话数据可读即已收敛」，未结清由门面的 persistence gate 判定。
//!
//! 已实现（C-05 第二批，边界实验）：P5/P6/P7 边界实验（`cloud_limit_test.rs`：取消在途调用、
//! 超大单批、收据保留与空间成本）。**删除不写墓碑**：v10 撤销本机生命周期锚点后，
//! `delete_tree` 在一个托管批里对子树每个节点先清子行（`messages` → `session_bindings`）
//! 再清会话行（`threads`），整棵子树要么全在要么全不在，没有「deleting → deleted」这种
//! 中间态（见 `session_lifecycle.rs` 的删除文档）。
//!
//! 消费侧接入（E）已落地：TUI、print、ACP stdio 与 `peri meta session` 都经同一个装配点
//! `Resources::open_deployment`（见 `context.rs`），真实云端端到端回归见
//! `cloud_deployment_test.rs`。P6 的「单请求上限先拒绝」分支在实测尺寸内没有触发，上限位置
//! 未定位（传输面实测的可用下限：2000 条语句 / 4 MiB 单行，结论见母 issue §9.28）；本批没有
//! 分块实现。
//!
//! 已实现（连接代际与重建）：放弃一个**已经发出**的在途请求（取消或 20s 预算超时）之后，
//! 用到它的那一代连接被记为失效（`generation` 的代际守卫在 future 被丢弃的 **drop 点**
//! 同步落下这个事实），会话数据 adapter 在下一次访问时按同一份打开事实重建连接：
//! 重新核实 store 身份（只读检查）、替换槽位、关闭退场连接。失效水位只增不减，旧代际的
//! 迟到失效不会撤销已记录的新代际失效。关闭开始后不再重连；真实连接保留在关闭句柄中，
//! 失败或取消允许再次关闭同一连接，只有确认成功才成为 `Closed`。
//! 守卫交出之前还会复核一次这一代是否已被记为失效（判定与取守卫之间没有原子性），
//! 因此一条已经不可证明的连接不会被交给调用方。
//!
//! **关闭（shutdown）的定义**：① 本机传输面的关闭走完（连接被保留在关闭句柄里直到成功，
//! 失败或取消都可重试、不新建连接）；② 门面侧的未结清检查通过——先通过 persistence gate 等待在途写入
//! 结束，再拒绝仍未结清的会话（见 `resources.rs` 的 `close`）。
//! 两件都成立才算确认关闭。SDK 的 `Connection::close` 恒返回 `Ok(())` 并显式吞掉远端关闭
//! 错误，因此 ① **不能**证明服务端连接已释放，也**不能**拿它证明任何未知的远端写没有执行。
//!
//! 重建**不**自动重发任何 mutation：重建只重核实 store 身份（只读检查），不带业务写入；
//! 未决的收敛不由连接重建承担（本机锚点已随 v10 撤销）。`Unsupported` 只剩「只读打开尚未
//! 初始化的 store」与「不认识的 store schema」两处——都是**拒绝**而不是未实现的行为。

// 非测试构建里的未使用项只有两类，都按「同一个交付面」标注：**显式 cloud 探测/回环面**
// （原始连接与参数绑定回环 `connection`、只读资格与终态封闭工具 `mutation::{apply_qualified,
// close_operation,resolve_operation}` 与 `ledger` 的封闭语句、store 身份的只读访问器
// `endpoint`/`schema`/`generation`、直接注入凭证的构造 `credentials`）与**逐条断言的 SQL
// 片段常量**（`session_sql`/`session_schema`，测试按列校验投影时使用）。生产路径已全部接线
// （adapter、组合、D 装配），所以这里不是「消费方尚未接入」的临时状态；精确到项的标注会把
// 同一个交付面的说明打散在八个文件里，收益不清，因此留在这里。新增未使用项必须属于上面
// 两类之一，否则应删掉（`SessionDataPort::load_flags` 与它的远端读路径就是按这条删的：
// flags 由一致快照读取，独立入口没有消费方）。
#![cfg_attr(not(test), allow(dead_code))]

mod composition;
mod connection;
mod credentials;
mod endpoint;
mod environment;
mod execution;
mod failure;
mod generation;
mod ledger;
mod mutation;
mod oauth_credentials;
mod schema;
mod schema_upgrade;

#[cfg(test)]
#[path = "schema_upgrade_test.rs"]
mod schema_upgrade_tests;
mod session_catalog;
mod session_codec;
mod session_data;
mod session_evidence;
mod session_history;
mod session_lifecycle;
mod session_read;
mod session_schema;
mod session_sql;
mod session_write;
mod sql;

pub(crate) use composition::open_remote;
pub(crate) use composition::open_remote_in_environment;
#[cfg(test)]
pub(crate) use connection::RemoteConnection;
#[cfg(any(test, target_os = "emscripten"))]
pub(crate) use credentials::SessionStoreCredential;
pub(crate) use credentials::{CredentialError, CredentialSource};
pub(crate) use endpoint::{EndpointError, RemoteEndpoint, RemoteEngine};
pub use environment::RemoteWorkspaceEnvironment;
#[cfg(test)]
pub(crate) use failure::RemoteFailureClass;

#[cfg(test)]
#[path = "remote_test.rs"]
mod tests;

#[cfg(test)]
#[path = "ledger_test.rs"]
mod ledger_tests;

#[cfg(test)]
#[path = "mutation_test.rs"]
mod mutation_tests;

#[cfg(test)]
#[path = "schema_test.rs"]
mod schema_tests;

#[cfg(test)]
#[path = "initialization_test.rs"]
mod initialization_tests;

#[cfg(test)]
#[path = "session_shape_test.rs"]
mod session_shape_tests;

#[cfg(test)]
#[path = "session_child_guard_test.rs"]
mod session_child_guard_tests;

#[cfg(test)]
#[path = "session_close_test.rs"]
mod session_close_tests;

#[cfg(test)]
#[path = "recovery_fixture_test.rs"]
mod recovery_fixture_tests;

#[cfg(test)]
#[path = "connection_recovery_test.rs"]
mod connection_recovery_tests;

#[cfg(test)]
#[path = "connection_close_test.rs"]
mod connection_close_tests;

#[cfg(test)]
#[path = "cloud_test.rs"]
mod cloud_tests;

#[cfg(test)]
#[path = "cloud_mutation_test.rs"]
mod cloud_mutation_tests;

#[cfg(test)]
#[path = "cloud_session_test.rs"]
mod cloud_session_tests;

#[cfg(test)]
#[path = "cloud_history_test.rs"]
mod cloud_history_tests;

#[cfg(test)]
#[path = "cloud_lifecycle_test.rs"]
mod cloud_lifecycle_tests;

#[cfg(test)]
#[path = "cloud_identity_test.rs"]
mod cloud_identity_tests;

#[cfg(test)]
#[path = "cloud_recovery_test.rs"]
mod cloud_recovery_tests;

#[cfg(test)]
#[path = "cloud_limit_test.rs"]
mod cloud_limit_tests;

#[cfg(test)]
#[path = "cloud_deployment_test.rs"]
mod cloud_deployment_tests;

#[cfg(test)]
#[path = "cloud_deployment_child_test.rs"]
mod cloud_deployment_child_tests;
