# peri-resources 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-12（模块职责拆分与 compact/历史恢复修复合并）
> 依据：peri-resources/src 源码、lib.rs 模块注释（伞形 PRD 决策 20）

## 架构速览

- 定位：外部系统数据访问通道（§0），以 context 形式提供给 Agent / Middleware / Controller；消费方不直接依赖底层 crate（peri-lsp / peri-workflow / peri-sessions），统一经本 crate 门面
- 结构：`config`（peri-config：直操配置文件）、`sessions`（peri-sessions：直操 sqlite，自 peri-agent/src/thread 迁入）、`lsp` / `workflow`（资源实现门面，仅类型/能力出口）、`context`（`Resources` 唯一实例化入口）
- 稳定不变量：`ThreadStore` trait / `ThreadMeta` / `BaseMessage` / `MessageFlags` 事实源在 `peri-acp-types`（sessions/mod.rs 注释）；本 crate 只实现、不解释业务语义

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改工作区身份、执行锁与项目会话列表 | `src/sessions/sqlite_store/{discovery,workspace,execution}.rs` | `resolve_workspace`、`create_bound_thread`、`adopt_legacy_thread`、`validate_session_binding`、`reassert_session_binding`、`acquire_execution_lease`、`reset_dirty_execution`、`list_scoped_threads` | Git common dir 区分项目，checkout 区分工作区；一次准入至多一次完整 Git 发现（`resolve_workspace` 或 `validate_session_binding`），准入内的后续复核用 `reassert_session_binding`：SQL 关系加关键文件对象（`Discovery::reassert_key_objects`），不启动外部进程，目录替换/换位/Git 位置消失仍然失败；Git 发现只用旧版也认得的选项（common dir 由 Git 写入的 `commondir` 文件推导，不请求 `--git-common-dir`；相对输出按 cwd 还原，`worktree list` 不支持 `-z` 时退回换行分隔、子命令整体缺失时跳过成员交叉核对，真实失败不退回）；Git 可执行文件缺失时以 cwd 建目录工作区，已有绑定仍复核完整快照；权限/损坏/中途失败不降级；默认 threads.db；对象身份只使用 Unix device/inode 或 Windows volume/file index，schema 3→4 在事务内规范化旧 identity JSON；开库不回填绑定；列表保留 nullable binding 的旧历史，恢复时原子接纳旧根 binding + frozen；lease 覆盖含旧未绑定子会话的写入；`acquire_execution_lease` 复用同一 stable lock 文件，发现前代 `clean=0` 时以 `WorkspaceError::RecoveryRequired(target)` 返回精确 `(thread_id, generation)`；`reset_dirty_execution` 只在同一锁内以事务 CAS 精确解除该代际，锁被活 owner 持有时是 `ExecutionBusy` 而非可解除的 dirty；`lock_execution` 在有界预算内重试（10ms 间隔、最多 500ms）以吸收子进程 `fork` 到 `exec` 窗口内被继承描述符造成的瞬时持有（`CLOEXEC` 只在子进程 exec 时生效），预算耗尽后仍按 `ExecutionBusy` 上报；同一目录对象再次解析时复用原项目/工作区 ID，仅刷新观测快照（`git init` / 移除 `.git` 不再 `NeedsRelink`）；Git 未回答的目录观测不覆盖已登记仓库布局；登记键是 (canonical root, 该目录的文件对象证据) 组合，同一路径的新对象或同一对象的新路径各自登记（新项目/新工作区），旧绑定按各自证据复核并失败关闭，项目只在定位与证据同时一致时复用（linked worktree 换位仍属原项目）；只读打开的 store 不写：观测快照的 UPDATE 在只读下跳过，未登记目录与新线程登记在进入 SQL 前按 `WorkspaceError::ReadOnlyStore` 失败（读请求按「本节点没有这条登记」失败，而不是交给 SQLite 报只读） |
| 打开全部资源（会话存储） | `src/context.rs` | `Resources::open`；`Resources::open_with`；`open_with_default`（注入默认路径的 seam）；`open_read_only` / `degradable_open_failure`；`Resources::thread_store` | 默认路径 `~/.peri/threads/threads.db`（`SqliteThreadStore::default_path` 与只读入口共用 `sessions::default_database_path`）；先写打开，写打开失败且属于可恢复占用（schema 锁被占、库文件/WAL 不可写）时降级为只读打开并记 `tracing::warn!`——「写打不开」不等于「历史读不了」，不再挡住进入；不认识的 schema（`UnsupportedSchemaVersion`/`UnsupportedDatabaseSchema`）在写打开走到版本判定时不降级；写打开在版本判定前失败（锁被占、库或目录不可写）时降级只按读取兼容的列形状把关、不复查 `user_version`，由更新构建写入且列形状兼容的库可被只读读取；只读打开也失败时返回写打开的原错误；不使用共享临时数据库 fallback |
| 只读打开已有 session 数据库 | `src/sessions/mod.rs` + `src/sessions/sqlite_store/connection.rs` | `open_thread_store_read_only`；`SqliteThreadStore::open_existing_read_only`；`SqliteThreadStore::require_writable`；`probe_load_meta_shape`；`classify_shape_probe_failure`；`ReadOnlyThreadStoreError` | 显式路径或默认路径只选择一个已存在普通文件；SQLite 使用 read-only、`create_if_missing(false)`、单连接和有界 busy timeout；按 `load_meta` 所需 schema shape fail closed，不创建目录/数据库、不初始化或迁移 schema；只有表/列缺失或 `SQLITE_CORRUPT`/`SQLITE_NOTADB` 才判定 schema 不兼容，锁竞争与 IO 等瞬时失败归 `database_unreadable`；同一入口也是启动降级的落点（`Resources::open_with` 写打开失败后复用）；只读 store 的写入在进入 SQL 前被 `require_writable` 按 `WorkspaceError::ReadOnlyStore` 拒绝，不把「attempt to write a readonly database」留给 SQL 层 |
| 改会话存储 SQL 实现 | `src/sessions/sqlite_store.rs`（唯一 pool owner / ThreadStore impl）+ `sqlite_store/connection.rs`（连接/close）+ `sqlite_store/schema.rs`（事务升级） | `SqliteThreadStore::new`；`close`；`default_path`；`init_schema`；`ThreadStore` impl；轻量列表 `list_thread_entries`；`load_frozen_snapshot` / `store_frozen_snapshot_if_absent` | trait 方法须与 `peri-acp-types/src/store.rs::ThreadStore` 签名一致；`close` 等待连接释放并要求 `-wal`/`-shm` 收尾已完成（仅 `Drop` 返回不保证）；frozen owner state 存在独立 nullable `frozen_context` 列且不进入 list projection，写入使用 `IS NULL` CAS（ARC-FROZEN-001）；TUI 列表查询只投影 thread 摘要并在 SQL 层按 cwd/hidden/message_count 过滤；另含 compaction 生命周期与 context cache |
| 改消息读写/祖先链 | `src/sessions/sqlite_store/context.rs`（trait 委托入口在 `sqlite_store.rs`） | `store_inherited_context`；`load_inherited_context`；`load_context_payloads`；`resolve_ancestor_chain`；`load_payloads_up_to` | child 的版本化只读继承快照及 frozen flags 存于独立 `inherited_context` 列；own payloads 只来自当前 thread；legacy 逐边读取 child metadata 保存的截止 ID，指定截止不存在或循环 fail closed；未记录截止时继承区为空；snapshot 损坏或未来版本不得覆盖，旧快照缺失时无法重建历史时刻的 flags |
| 改 compaction 持久化 / flags / 回滚删除 | `src/sessions/sqlite_store/compaction.rs`；trait 入口仍在 `sqlite_store.rs` | `commit_compaction_lifecycle`（:78）；`update_message_flags`（:44）；`delete_messages_since`（:173） | compact flags、追加消息、message_count 与 cache epoch 在同一事务提交；精确检查被更新消息归属当前 thread，缺失消息导致整批回滚 |
| 改测试用文件存储 | `src/sessions/filesystem.rs` | `FilesystemThreadStore`；`new`；`default_path`；`frozen_snapshot_path`；`store_inherited_context` / `load_inherited_context`；`atomic_write_json_if_absent` | 纯测试用途（sessions/mod.rs:3），生产实现是 sqlite；frozen snapshot 使用每 thread 的 `frozen.json` sidecar，不写入 `index.json`，继承上下文另存 `inherited.json`；完整 temp + hard-link 提供 no-clobber write-once |
| 改全局配置路径 | `src/config/mod.rs` | `peri_dir`（:9，`~/.peri`）；`settings_path`（:14，`~/.peri/settings.json`） | 仅路径入口，配置读取语义之外的逻辑不迁入本 crate |
| 引用 LSP 能力 | `src/lsp.rs` | 门面：`pub use peri_lsp::{client, config, diagnostics, error, jsonrpc, pool, protocol, uri}` | 唯一引用入口；实例化/持有（池生命周期）收口至 Resources context 后，本模块仅类型/能力出口 |
| 引用 Workflow 能力 | `src/workflow.rs` | 门面：`pub use peri_workflow::{error, journal, progress, protocol, registry, rpc, runner, tool}` | 同上；消费方（Middleware 等）不直接依赖 peri-workflow |

## 子系统

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| Resources 门面（唯一实例化入口） | src/context.rs | `Resources`（:17，持 `Arc<dyn ThreadStore>`） |
| 全局配置路径 | src/config/mod.rs | `peri_dir` / `settings_path` |
| SQLite 会话存储 | src/sessions/sqlite_store.rs | `SqliteThreadStore`（:35，唯一 pool owner）；唯一 `ThreadStore` impl 处理 metadata/payload/frozen，context/compaction 委托私有模块 |
| SQLite 连接与解码 | src/sessions/sqlite_store/{connection,schema,row_mapping}.rs | connection.rs（连接、read-only probe、安全错误）；schema.rs（按必需真实表/列识别旧库、保留额外业务表、共享列定义、事务升级并移除无状态 revision 列；不认识的 `user_version` 以 `WorkspaceError::UnsupportedSchemaVersion { found, supported }` 拒绝并复述两个版本号，`CURRENT_SCHEMA_VERSION` 是接受判定、收尾写入与上限文案的单一来源；schema 2–5→6 在事务内重建 projects / workspaces，把单列唯一放宽为组合登记键（含被旧 writer 误标 5 的漏迁移库，保留健康 5 已有组合登记）；2/3 先完成原有 revision / 身份载荷迁移，重建需在事务外关闭外键并在提交前用 `PRAGMA foreign_key_check` 补齐校验）；row_mapping.rs（`ThreadRow` :24、`meta_from_row` :54、`role_of` :43、`extract_title` :96、完整/列表列投影） |
| SQLite 上下文与事务 | src/sessions/sqlite_store/{context,compaction}.rs | context.rs（ancestor payload、cache、child/session tree）；compaction.rs（flags、事务提交、回滚删除） |
| 测试文件存储 | src/sessions/filesystem.rs | `FilesystemThreadStore`（:25） |
| 会话存储 re-export / 只读入口 | src/sessions/mod.rs | `SqliteThreadStore` / `FilesystemThreadStore`（:10-11）；`open_thread_store_read_only`；`default_database_path`（读写共用的纯路径解析） |
| LSP 门面 | src/lsp.rs | 全量 re-export peri_lsp 模块 |
| Workflow 门面 | src/workflow.rs | 全量 re-export peri_workflow 模块 |

## 跨模块契约

- Worktree 归属见[身份设计](../design/session-workspace-identity.md)：新会话使用 ProjectId / WorkspaceId / SessionBinding 与跨进程 lease；历史 cwd 接口仅保留精确目录兼容语义。

- 消费方：`peri-tui/src/app/mod.rs:88` 与 `peri-tui/src/cli_print.rs:136`（`Resources::open_with`，默认或显式路径失败均直接传播）；`peri-controller/src/controller.rs:222`（`Resources::open()` 后调用）；`peri-middlewares/src/`（lsp/middleware.rs:11-12、lsp/tool.rs:6-7、plugin/loader.rs:14、workflow/mod.rs、assembly.rs）
- 契约类型：`ThreadStore` trait / `ThreadMeta` / `BaseMessage` / `MessageFlags` 事实源在 `peri-acp-types/src/store.rs`（sessions/mod.rs 明确「接口契约归 peri-acp-types」）
- 门面依赖：Cargo.toml 依赖 `peri-lsp`、`peri-workflow`（决策 20：既有 crate 归位），门面仅 re-export 不解释业务语义
