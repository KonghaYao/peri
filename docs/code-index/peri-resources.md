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
| 改工作区身份、执行锁与项目会话列表 | `src/sessions/sqlite_store/{discovery,workspace,execution}.rs` | `resolve_workspace`、`create_bound_thread`、`adopt_legacy_thread`、`validate_session_binding`、`acquire_execution_lease`、`list_scoped_threads` | Git common dir 区分项目，checkout 区分工作区；Git 发现只用旧版也认得的选项（common dir 由 Git 写入的 `commondir` 文件推导，不请求 `--git-common-dir`；相对输出按 cwd 还原，`worktree list` 不支持 `-z` 时退回换行分隔、子命令整体缺失时跳过成员交叉核对，真实失败不退回）；Git 可执行文件缺失时以 cwd 建目录工作区，已有绑定仍复核完整快照；权限/损坏/中途失败不降级；默认 threads.db；对象身份只使用 Unix device/inode 或 Windows volume/file index，schema 3→4 在事务内规范化旧 identity JSON；开库不回填绑定；列表保留 nullable binding 的旧历史，恢复时原子接纳旧根 binding + frozen；lease 覆盖含旧未绑定子会话的写入，dirty 显式恢复；同一目录对象再次解析时复用原项目/工作区 ID，仅刷新观测快照（`git init` / 移除 `.git` 不再 `NeedsRelink`）；Git 未回答的目录观测不覆盖已登记仓库布局；登记键是 (canonical root, 该目录的文件对象证据) 组合，同一路径的新对象或同一对象的新路径各自登记（新项目/新工作区），旧绑定按各自证据复核并失败关闭，项目只在定位与证据同时一致时复用（linked worktree 换位仍属原项目） |
| 打开全部资源（会话存储） | `src/context.rs` | `Resources::open`；`Resources::open_with`；`Resources::thread_store` | 默认路径 `~/.peri/threads/threads.db`（`SqliteThreadStore::default_path` 与只读入口共用 `sessions::default_database_path`）或显式路径打开失败时直接返回包含路径的错误；不使用共享临时数据库 fallback |
| 只读打开已有 session 数据库 | `src/sessions/mod.rs` + `src/sessions/sqlite_store/connection.rs` | `open_thread_store_read_only`；`SqliteThreadStore::open_existing_read_only`；`probe_load_meta_shape`；`classify_shape_probe_failure`；`ReadOnlyThreadStoreError` | 显式路径或默认路径只选择一个已存在普通文件；SQLite 使用 read-only、`create_if_missing(false)`、单连接和有界 busy timeout；按 `load_meta` 所需 schema shape fail closed，不创建目录/数据库、不初始化或迁移 schema；只有表/列缺失或 `SQLITE_CORRUPT`/`SQLITE_NOTADB` 才判定 schema 不兼容，锁竞争与 IO 等瞬时失败归 `database_unreadable` |
| 改会话存储 SQL 实现 | `src/sessions/sqlite_store.rs`（唯一 pool owner / ThreadStore impl）+ `sqlite_store/connection.rs`（连接/close）+ `sqlite_store/schema.rs`（事务升级） | `SqliteThreadStore::new`；`close`；`default_path`；`init_schema`；`ThreadStore` impl；轻量列表 `list_thread_entries`；`load_frozen_snapshot` / `store_frozen_snapshot_if_absent` | trait 方法须与 `peri-acp-types/src/store.rs::ThreadStore` 签名一致；`close` 等待连接释放并要求 `-wal`/`-shm` 收尾已完成（仅 `Drop` 返回不保证）；frozen owner state 存在独立 nullable `frozen_context` 列且不进入 list projection，写入使用 `IS NULL` CAS（ARC-FROZEN-001）；TUI 列表查询只投影 thread 摘要并在 SQL 层按 cwd/hidden/message_count 过滤；另含 compaction 生命周期与 context cache |
| 改消息读写/祖先链 | `src/sessions/sqlite_store/context.rs`（trait 委托入口在 `sqlite_store.rs`） | `store_inherited_context`；`load_inherited_context`；`load_context_payloads`；`resolve_ancestor_chain`；`load_payloads_up_to` | child 的版本化只读继承快照及 frozen flags 存于独立 `inherited_context` 列；own payloads 只来自当前 thread；legacy 逐边读取 child metadata 保存的截止 ID，指定截止不存在或循环 fail closed；未记录截止时继承区为空；snapshot 损坏或未来版本不得覆盖，旧快照缺失时无法重建历史时刻的 flags |
| 改消息载荷持久化保真（含 provider 原生历史） | `src/sessions/sqlite_store.rs` + `src/sessions/sqlite_store/row_mapping.rs`；envelope 事实源 `peri-acp-types/src/store.rs` | `append_messages` / `load_messages`；`serialize_persisted_payload` / `deserialize_persisted_payload` | 消息经版本化 envelope JSON 落库，存储层不解释 `ContentBlock`（含 Responses `responses_native_history` 载体）：不解码、不脱敏、不丢字段，密文与来源身份随载体逐字往返；日志/Debug 出口的脱敏归 `peri-acp-types` 手写 `Debug`，存储层不复制该规则 |
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
| SQLite 连接与解码 | src/sessions/sqlite_store/{connection,schema,row_mapping}.rs | connection.rs（连接、read-only probe、安全错误）；schema.rs（按必需真实表/列识别旧库、保留额外业务表、共享列定义、事务升级并移除无状态 revision 列；schema 4→5 在事务内重建 projects / workspaces 把单列唯一放宽为组合登记键，重建需在事务外关闭外键并在提交前用 `PRAGMA foreign_key_check` 补齐校验）；row_mapping.rs（`ThreadRow` :24、`meta_from_row` :54、`role_of` :43、`extract_title` :96、完整/列表列投影） |
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
