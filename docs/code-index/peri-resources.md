# peri-resources 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-10（SQLite 存储职责拆分）
> 依据：peri-resources/src 源码、lib.rs 模块注释（伞形 PRD 决策 20）

## 架构速览

- 定位：外部系统数据访问通道（§0），以 context 形式提供给 Agent / Middleware / Controller；消费方不直接依赖底层 crate（peri-lsp / peri-workflow / peri-sessions），统一经本 crate 门面
- 结构：`config`（peri-config：直操配置文件）、`sessions`（peri-sessions：直操 sqlite，自 peri-agent/src/thread 迁入）、`lsp` / `workflow`（资源实现门面，仅类型/能力出口）、`context`（`Resources` 唯一实例化入口）
- 稳定不变量：`ThreadStore` trait / `ThreadMeta` / `BaseMessage` / `MessageFlags` 事实源在 `peri-acp-types`（sessions/mod.rs 注释）；本 crate 只实现、不解释业务语义

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 打开全部资源（会话存储） | `src/context.rs` | `Resources::open`；`Resources::open_with`；`Resources::thread_store` | 默认路径 `~/.peri/threads/threads.db`（`SqliteThreadStore::default_path`）或显式路径打开失败时直接返回包含路径的错误；不使用共享临时数据库 fallback |
| 只读打开已有 session 数据库 | `src/sessions/mod.rs` + `src/sessions/sqlite_store/connection.rs`；错误类型经 `sqlite_store.rs` 稳定 re-export | `open_thread_store_read_only`；`SqliteThreadStore::open_existing_read_only`（connection.rs:126）；`probe_load_meta_shape`（:163）；`ReadOnlyThreadStoreError` | 显式路径或默认路径只选择一个已存在普通文件；SQLite 使用 read-only、`create_if_missing(false)`、单连接和有界 busy timeout；按 `load_meta` 所需 schema shape fail closed，不创建目录/数据库、不初始化或迁移 schema |
| 改会话存储 SQL 实现 | `src/sessions/sqlite_store.rs`（唯一 pool owner / ThreadStore impl）；`sqlite_store/connection.rs`（连接/schema） | `SqliteThreadStore::new`（connection.rs:97）；`default_path`；`init_schema`；轻量列表 `list_thread_entries`（sqlite_store.rs:274）；`load_frozen_snapshot`（:223）/`store_frozen_snapshot_if_absent`（:232） | trait 方法须与 `peri-acp-types/src/store.rs::ThreadStore` 签名一致；frozen owner state 存在独立 nullable `frozen_context` 列且不进入 list projection，写入使用 `IS NULL` CAS（ARC-FROZEN-001）；TUI 列表查询只投影 thread 摘要并在 SQL 层按 cwd/hidden/message_count 过滤；另含 compaction 生命周期与 context cache |
| 改消息读写/祖先链 | `src/sessions/sqlite_store.rs`（消息 CRUD）；`sqlite_store/context.rs`（祖先链/缓存） | `create_thread`（sqlite_store.rs:44）；`append_messages`（:69）；`load_messages`（:78）；`load_context_payloads`（context.rs:89）；`load_context`（:105）；`resolve_ancestor_chain`（:18） | 祖先读取按 thread + snapshot rowid 加载 payload 并验证消息身份；load_context 从 payload 投影消息后写 context cache |
| 改 compaction 持久化 / flags / 回滚删除 | `src/sessions/sqlite_store/compaction.rs`；trait 入口仍在 `sqlite_store.rs` | `commit_compaction_lifecycle`（:78）；`update_message_flags`（:44）；`delete_messages_since`（:173） | compact flags、追加消息、message_count 与 cache epoch 在同一事务提交；精确检查被更新消息归属当前 thread，缺失消息导致整批回滚 |
| 改测试用文件存储 | `src/sessions/filesystem.rs` | `FilesystemThreadStore`；`new`；`default_path`；`frozen_snapshot_path`；`atomic_write_json_if_absent` | 纯测试用途（sessions/mod.rs:3），生产实现是 sqlite；frozen snapshot 使用每 thread 的 `frozen.json` sidecar，不写入 `index.json`，完整 temp + hard-link 提供 no-clobber write-once |
| 改全局配置路径 | `src/config/mod.rs` | `peri_dir`（:9，`~/.peri`）；`settings_path`（:14，`~/.peri/settings.json`） | 仅路径入口，配置读取语义之外的逻辑不迁入本 crate |
| 引用 LSP 能力 | `src/lsp.rs` | 门面：`pub use peri_lsp::{client, config, diagnostics, error, jsonrpc, pool, protocol, uri}` | 唯一引用入口；实例化/持有（池生命周期）收口至 Resources context 后，本模块仅类型/能力出口 |
| 引用 Workflow 能力 | `src/workflow.rs` | 门面：`pub use peri_workflow::{error, journal, progress, protocol, registry, rpc, runner, tool}` | 同上；消费方（Middleware 等）不直接依赖 peri-workflow |

## 子系统

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| Resources 门面（唯一实例化入口） | src/context.rs | `Resources`（:17，持 `Arc<dyn ThreadStore>`） |
| 全局配置路径 | src/config/mod.rs | `peri_dir` / `settings_path` |
| SQLite 会话存储 | src/sessions/sqlite_store.rs | `SqliteThreadStore`（:35，唯一 pool owner）；唯一 `ThreadStore` impl 处理 metadata/payload/frozen，context/compaction 委托私有模块 |
| SQLite 连接与解码 | src/sessions/sqlite_store/{connection,row_mapping}.rs | connection.rs（schema、read-only probe、安全错误）；row_mapping.rs（`ThreadRow` :24、`meta_from_row` :54、`role_of` :43、`extract_title` :96、完整/列表列投影） |
| SQLite 上下文与事务 | src/sessions/sqlite_store/{context,compaction}.rs | context.rs（ancestor payload、cache、child/session tree）；compaction.rs（flags、事务提交、回滚删除） |
| 测试文件存储 | src/sessions/filesystem.rs | `FilesystemThreadStore`（:25） |
| 会话存储 re-export / 只读入口 | src/sessions/mod.rs | `SqliteThreadStore` / `FilesystemThreadStore`（:10-11）；`open_thread_store_read_only`（:19） |
| LSP 门面 | src/lsp.rs | 全量 re-export peri_lsp 模块 |
| Workflow 门面 | src/workflow.rs | 全量 re-export peri_workflow 模块 |

## 跨模块契约

- 消费方：`peri-tui/src/app/mod.rs:88` 与 `peri-tui/src/cli_print.rs:136`（`Resources::open_with`，默认或显式路径失败均直接传播）；`peri-controller/src/controller.rs:222`（`Resources::open()` 后调用）；`peri-middlewares/src/`（lsp/middleware.rs:11-12、lsp/tool.rs:6-7、plugin/loader.rs:14、workflow/mod.rs、assembly.rs）
- 契约类型：`ThreadStore` trait / `ThreadMeta` / `BaseMessage` / `MessageFlags` 事实源在 `peri-acp-types/src/store.rs`（sessions/mod.rs 明确「接口契约归 peri-acp-types」）
- 门面依赖：Cargo.toml 依赖 `peri-lsp`、`peri-workflow`（决策 20：既有 crate 归位），门面仅 re-export 不解释业务语义
