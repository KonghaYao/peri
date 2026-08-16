# peri-resources 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-08-16
> 依据：peri-resources/src 源码、lib.rs 模块注释（伞形 PRD 决策 20）

## 架构速览

- 定位：外部系统数据访问通道（§0），以 context 形式提供给 Agent / Middleware / Controller；消费方不直接依赖底层 crate（peri-lsp / peri-workflow / peri-sessions），统一经本 crate 门面
- 结构：`config`（peri-config：直操配置文件）、`sessions`（peri-sessions：直操 sqlite，自 peri-agent/src/thread 迁入）、`lsp` / `workflow`（资源实现门面，仅类型/能力出口）、`context`（`Resources` 唯一实例化入口）
- 稳定不变量：`ThreadStore` trait / `ThreadMeta` / `BaseMessage` / `MessageFlags` 事实源在 `peri-acp-types`（sessions/mod.rs 注释）；本 crate 只实现、不解释业务语义

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 打开全部资源（会话存储） | `src/context.rs` | `Resources::open`（:26）；`open_with`（:36）；`thread_store`（:54，返回 `Arc<dyn ThreadStore>`） | 默认路径 `~/.peri/threads/threads.db`（`SqliteThreadStore::default_path`）打开失败 fallback 临时目录；显式 `Some(path)` 失败直接报错不 fallback |
| 改会话存储 SQL 实现 | `src/sessions/sqlite_store.rs` | `SqliteThreadStore::new`（:39）；`default_path`（:63）；`init_schema`（:73）；`ThreadStore` impl（:292） | trait 方法须与 `peri-acp-types/src/store.rs::ThreadStore` 签名一致；含 compaction 生命周期（`supports_compaction_lifecycle` :719 / `commit_compaction_lifecycle` :723）与 context cache（`invalidate_context_cache` :636 / `get_context_cache_epoch` :644） |
| 改消息读写/祖先链 | `src/sessions/sqlite_store.rs` | `create_thread`（:293）；`append_messages`（:318）；`load_messages`（:360）；`load_context`（:494）；`resolve_ancestor_chain`（:135） | load_context 按祖先链拼装 + context cache（`save_context_cache` :191）；`delete_messages_since`（:818）供回滚类操作 |
| 改测试用文件存储 | `src/sessions/filesystem.rs` | `FilesystemThreadStore`（:25）；`new`（:30）；`default_path`（:37） | 纯测试用途（sessions/mod.rs:3），生产实现是 sqlite |
| 改全局配置路径 | `src/config/mod.rs` | `peri_dir`（:9，`~/.peri`）；`settings_path`（:14，`~/.peri/settings.json`） | 仅路径入口，配置读取语义之外的逻辑不迁入本 crate |
| 引用 LSP 能力 | `src/lsp.rs` | 门面：`pub use peri_lsp::{client, config, diagnostics, error, jsonrpc, pool, protocol, uri}` | 唯一引用入口；实例化/持有（池生命周期）收口至 Resources context 后，本模块仅类型/能力出口 |
| 引用 Workflow 能力 | `src/workflow.rs` | 门面：`pub use peri_workflow::{error, journal, progress, protocol, registry, rpc, runner, tool}` | 同上；消费方（Middleware 等）不直接依赖 peri-workflow |

## 子系统

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| Resources 门面（唯一实例化入口） | src/context.rs | `Resources`（:17，持 `Arc<dyn ThreadStore>`） |
| 全局配置路径 | src/config/mod.rs | `peri_dir` / `settings_path` |
| SQLite 会话存储 | src/sessions/sqlite_store.rs | `SqliteThreadStore`（:33）；角色映射 `role_of`（:210）；标题提取 `extract_title`（:261） |
| 测试文件存储 | src/sessions/filesystem.rs | `FilesystemThreadStore`（:25） |
| 会话存储 re-export | src/sessions/mod.rs | `SqliteThreadStore` / `FilesystemThreadStore`（:10-11） |
| LSP 门面 | src/lsp.rs | 全量 re-export peri_lsp 模块 |
| Workflow 门面 | src/workflow.rs | 全量 re-export peri_workflow 模块 |

## 跨模块契约

- 消费方：`peri-tui/src/app/mod.rs:88` 与 `peri-tui/src/cli_print.rs:136`（`Resources::open_with`，含 fallback 语义注释 app/mod.rs:46）；`peri-controller/src/controller.rs:222`（`Resources::open()` 后调用）；`peri-middlewares/src/`（lsp/middleware.rs:11-12、lsp/tool.rs:6-7、plugin/loader.rs:14、workflow/mod.rs、assembly.rs）
- 契约类型：`ThreadStore` trait / `ThreadMeta` / `BaseMessage` / `MessageFlags` 事实源在 `peri-acp-types/src/store.rs`（sessions/mod.rs 明确「接口契约归 peri-acp-types」）
- 门面依赖：Cargo.toml 依赖 `peri-lsp`、`peri-workflow`（决策 20：既有 crate 归位），门面仅 re-export 不解释业务语义
