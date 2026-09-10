# meta session 只读打开的偶发失败：误诊已改正，但未证明偶发已消除

**状态**：Open（修复已落地，缺目标环境证据）
**优先级**：高
**类型**：CI 稳定性 / 错误分类 / 只读数据访问
**创建日期**：2026-09-10
**来源**：`cargo test -p peri-tui --test meta_session_cli` 在远程 Ubuntu CI 偶发失败，重跑有时通过

## 现象

同一份代码在 CI 上偶发失败、重跑有时通过，失败集中在一个错误标签上：

- `human_success_is_stdout_only_and_escapes_persisted_controls`：期望 exit `0`，实际 exit `4`，stderr 为 `schema_incompatible: thread database schema is incompatible`；
- `every_error_kind_has_human_real_binary_stream_and_exit_evidence`：期望 `corrupt_session_data`，实际得到 `schema_incompatible`。

本地 macOS 无法复现（串行重复、并发实例、多轮压测均全绿），与"负载敏感的时序问题"特征一致，但**没有 CI 上的失败样本，也没有修复后的 CI 对照**。

## 已确认的代码事实

1. `probe_load_meta_shape` 原先把 probe 查询的**任何**错误折叠成 `SchemaIncompatible`。锁竞争、IO 抖动等瞬时失败因此被上报成 schema 判定。
2. `SqliteThreadStore::new` 的 sqlx `Pool` 依赖 `Drop` 收尾；`Drop` 只异步调度连接关闭，最后一次连接关闭触发的 WAL checkpoint 与 `-wal`/`-shm` 清理可能晚于夹具返回，与子进程的只读打开重叠。
3. 只读路径使用 `READ_ONLY_BUSY_TIMEOUT = 250ms`、`max_connections(1)`，probe 查询没有重试。
4. 查询阶段的分类**不含**第 1 条缺陷：`load_meta` 的只读分支已区分 `RowNotFound` → `SessionNotFound`、`Decode`/`ColumnDecode` → `CorruptSessionData`、其余 → `DatabaseUnreadable`。

## 已实施的改动

- `peri-resources/src/sessions/sqlite_store.rs`：新增 `classify_shape_probe_failure`，仅 `SQLITE_CORRUPT`(11) 与 `SQLITE_NOTADB`(26) 判 `SchemaIncompatible`，其余归 `DatabaseUnreadable`；新增 `SqliteThreadStore::close()` 提供确定性收尾。
- `peri-tui/tests/meta_session_cli.rs`：夹具在子进程启动前调用 `store.close().await`。
- `docs/design/meta-control.md`（§7/§8/§11）与 `docs/code-index/peri-resources.md` 同步错误语义与入口。

## 为什么这批改动还不能算稳定

### 1. 分类修正不改变失败概率

`classify_shape_probe_failure` 只改变**诊断标签**。若 CI 上锁竞争仍然超出 busy 上限，`human_success_...` 依然失败：期望 exit `0`，实得 exit `4`（`DatabaseUnreadable` 与 `SchemaIncompatible` 都映射到 exit 4，只有标签从 `schema_incompatible` 变成 `database_unreadable`）。误诊被改正了，测试的确定性没有因此提高。

### 2. `close()` 的确定性只有本地 macOS 证据

"`close()` 返回后 WAL 已 checkpoint、`-wal`/`-shm` 已清理"来自一次性的本地 scratch 探针（该探针在诊断结束后已删除，不可重放）。Windows 的文件删除与句柄语义不同，而 CI 矩阵包含 `windows-latest`。

### 3. 时序余量没有改变

`READ_ONLY_BUSY_TIMEOUT` 仍是 250ms，probe 仍无重试。夹具只是缩短了竞态窗口，没有消除"只读打开撞上写端收尾"这一类事件，也没有为它建立失败注入测试。

### 4. 失败注入只覆盖分类函数，不覆盖打开路径

分类单测走进程内 test double（直接构造 `sqlx::Error::Database` 调分类函数），不经过真实 SQLite。真实路径的 `SQLITE_BUSY` 有真实覆盖（`test_readonly_open_lock_contention_is_bounded` 用真实 SQLite 断言 `DatabaseUnreadable`），但 `SQLITE_IOERR`(10)、`SQLITE_CANTOPEN`(14) 等其余瞬时码在真实打开路径上没有注入覆盖。

### 5. `close()` 的 API 边界未定义

新增 public 方法只承诺"返回后本进程不再持有连接"，没有阻止 close 之后继续使用该 store；后续调用会得到 `PoolClosed` 类运行时错误，而不是编译期拒绝。

## 独立验证结果（2026-09-10）

独立 agent 在本地 macOS 用跨进程夹具（真实 `peri` 二进制 + 真实 SQLite）复核，总裁决 **PARTIAL**：修复的机制主张成立，但"消除 CI 偶发失败"不成立。

**已验证**

- `close()` 后夹具返回时 `-wal`/`-shm` 100% 已清理、无连接句柄残留、`pragma integrity_check` 通过；仅靠 `Drop` 时侧车文件在 40 轮中 30 轮仍残留（300ms 后依旧）。机制主张成立。
- 真实打开路径分类正确：`SQLITE_BUSY`(5)、`SQLITE_CANTOPEN`(14) → `database_unreadable`；`SQLITE_NOTADB`(26)、`SQLITE_CORRUPT`(11) → `schema_incompatible`。持锁期间调用真实 `peri meta session`，5/5 得到 `database_unreadable`。
- 查询阶段（`load_meta` 只读分支）不含同类缺陷；生产代码中 `SchemaIncompatible` 只出现在确定性分支。

**已证伪**

- 修复**不提高测试确定性**：构造"锁竞争超过 busy 上限"后两个用例仍失败（`human_success_...` 期望 exit 0 实得 exit 4；`every_error_kind_...` 期望 `corrupt_session_data` 实得 `database_unreadable`）。

**未复现**

- 任何"只读打开撞锁"的实例：约 800 次跨进程读者调用（含 512MiB WAL、QoS 饥饿、3 路并发读者）零失败；正对照（持有活 writer 时外部独占锁）3/3 被正确阻塞，检测手段灵敏。**没有修复前的失败样本**。

**未验证**

- `close()` 在 Ubuntu/Windows CI 上的后置条件与 Windows 句柄/删除语义。
- WAL 收尾与只读打开重叠时"短暂持有 shm 独占锁"这一机制描述未被观测证实——观测到的只是侧车文件残留。该措辞已从设计文档与代码注释移除，只保留规范性要求。

## 待办

1. **为目标环境补可重复证据**：在 CI 等价负载下重复运行 `cargo test -p peri-tui --test meta_session_cli`（CI 循环或资源受限容器），或证明无法复现。
2. **若仍可复现**：为只读打开建立确定性策略而不是继续依赖时序 —— 候选：提高/参数化 busy timeout、对瞬时失败做有界重试、或把写端收尾与只读打开之间的窗口交给显式协议。
3. **失败注入**：在真实打开路径上注入尚未覆盖的瞬时码（`SQLITE_IOERR`、`SQLITE_CANTOPEN`），断言 `database_unreadable` 且不出现 `schema_incompatible`；`SQLITE_BUSY` 已由 `test_readonly_open_lock_contention_is_bounded` 覆盖。
4. **平台验证**：确认 macOS/Windows CI 上 `close()` 的收尾语义；在补齐平台证据前，不把该结论当作已确立的跨平台事实。
5. **`close()` 契约**：明确 close 之后的行为（文档约定或类型层面约束）。

## 验收标准

- [ ] 修复前失败样本或等价复现（CI 日志、受限环境复现步骤）至少一份。
- [ ] 重复运行证据：目标环境下 `meta_session_cli` 连续 N 次（N 与命令记录在 issue）无失败，或给出仍可复现的反证。
- [ ] 存在不依赖计时余量的失败注入测试，覆盖真实只读打开 + probe 路径。
- [ ] `close()` 的后置条件在文档或类型层面明确。
- [ ] CI 失败时 `schema_incompatible` 不再由瞬时失败产生（由上述注入测试保证）。

## 相关文件

- `peri-resources/src/sessions/sqlite_store.rs`
- `peri-resources/src/sessions/sqlite_store_test.rs`
- `peri-tui/tests/meta_session_cli.rs`
- `peri-tui/src/cli_meta.rs`
- `docs/design/meta-control.md`
- `docs/code-index/peri-resources.md`
- `.github/workflows/ci.yml`
