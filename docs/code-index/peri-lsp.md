# peri-lsp 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-08-16
> 依据：peri-lsp/src 源码、peri-resources/src/lsp.rs（门面）、源码注释

## 架构速览

- 数据流：`LspServerPool（按扩展名路由）→ LspClient（进程管理 + 协议）→ MessageDispatcher（后台分发）→ DiagnosticsRegistry（诊断聚合）`
- 入口：`src/pool.rs::LspServerPool::new`（:44，惰性初始化，`disabled=true` 跳过）；单服务器启动走 `src/client.rs::LspClient::start`（:107，并发安全：start_lock 互斥 + 二次检查）
- 稳定不变量：协议类型 `LspServerConfig/LspConfigSource` 事实源在 `peri_acp_types::lsp`（`src/config.rs:8` 仅 re-export）；`LspServerPool` 实现 `peri_acp_types::ports::LspPoolPort`（pool.rs:327，跨层 downcast 用）；池初始化检查-插入由 `start_lock`（tokio::sync::Mutex）保证原子

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 加/改 LSP 服务器配置 | `src/config.rs`（`LspServerConfig` 定义在 `peri-acp-types/src/lsp.rs`，本文件 re-export 于 :8） | `load_global_lsp_config`（:71，settings.json 的 `config.lspServers`）；`lsp_config_from_plugin`（:111，插件场景） | name 以 settings.json 的 key 为准；`expand_env_vars`（:17）只查进程环境；插件配置额外按注入 env 展开 `${CLAUDE_PLUGIN_ROOT}`（:139-148） |
| 改服务器启动/握手 | `src/client.rs` | `start`（:107）；`do_start`（:134）；`shutdown`（:442） | spawn 子进程 → 注册 publishDiagnostics 通知处理器 → initialize 请求（startup_timeout_ms，缺省 30s，:33）→ initialized 通知；启动失败须 close 子进程防孤儿 |
| 改重启/冷却语义 | `src/client.rs` | `try_restart`（:485）；`check_and_increment_restart`（:459） | 60s 窗口内计数不重置，超出 `max_restarts` 返回 ServerCrashed 进入冷却（RESTART_WINDOW :30）；重启清空 open_files 与 diagnostics（:498-500） |
| 发请求/通知或文件同步 | `src/client.rs` | `request`（:241，带超时）；`notify`（:315）；`did_open`（:327）/`did_change`（:349）/`did_save`（:395） | request 失败/超时须 `cancel_request` 移除 pending 注册防 oneshot 残留；did_open 幂等（open_files 已含 uri 直接返回）；did_change 版本号自增 |
| 改消息分帧/分发 | `src/jsonrpc/` | codec：`encode_message`/`decode_message`（codec.rs:8/:28）；transport：`LspTransport::spawn`（:28）、`MessageDispatcher::new`（:134）、`run_dispatch_loop`（:363） | Content-Length 分帧，body 上限 64MB（codec.rs:20）；spawn 后立即 try_wait 捕获参数错误即退；EOF/失败自动 kill 子进程（transport.rs:184-188）；close() 先 kill 再 abort read task |
| 改诊断聚合/限流 | `src/diagnostics.rs` | `handle_publish_diagnostics`（:82）；`get_for_file`（:119）/`get_all`（:124）/`summary`（:133）/`clear_all`（:156） | 单文件上限 10、总量上限 30（:55-56）；按 uri 索引；clear_all 后服务器再推会重新入库 |
| 新增 LSP 请求/通知方法 | `src/protocol/requests.rs` / `notifications.rs` | 例 `goto_definition_request`（requests.rs:7）、`initialize_params`（:94）；`did_open_notification`（notifications.rs:7）、`parse_publish_diagnostics`（:61） | 请求须携带自增 id；通知不期望响应；lsp_types 类型经 protocol/mod.rs re-export |
| 改 URI 转换 | `src/uri.rs` | `path_to_uri`（:21）；`uri_to_path`（:56） | 幂等（已有 file:// 原样返回）；相对路径绝对化；RFC 3986 percent-encode 保留 `/` 与 `:`；Windows 盘符输出 `file:///C:/a/b` 空 authority 形式 |
| 改扩展名路由/按需启动 | `src/pool.rs` | `ensure_server_for_file`（:139）；`ensure_initialized`（:92）；`add_server`（:260）；`server_for_file`（:207） | 扩展名小写化映射；start_lock 保证并发 ensure 只 spawn 一次；`shutdown`（:244）逐个关停并清 initialized |

## 子系统

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 客户端（进程 + 协议状态机） | src/client.rs | `LspClient::new`（:73）；`infer_language_id`（:418）；`ServerState`（:21）；`open_files` 版本簿 |
| 服务器池（路由 + 生命周期） | src/pool.rs | `LspServerPool`（:20）；`LspServerInfo`（:36）；`root_uri`（:312）；`any_server`（:306） |
| 诊断注册表 | src/diagnostics.rs | `DiagnosticsRegistry`（:63）；`DiagnosticEntry`（:11）；`DiagnosticSeverity`（:21）；`DiagnosticSummary`（:41） |
| JSON-RPC 分帧 | src/jsonrpc/codec.rs | Content-Length 编解码；大小写不敏感头部 |
| JSON-RPC 消息类型 | src/jsonrpc/message.rs | `JsonRpcRequest`（:14）/`JsonRpcNotification`（:56）/`JsonRpcResponse`（:35）/`RequestId`（:7） |
| 传输 + 分发 | src/jsonrpc/transport.rs | `LspTransport`（:20）；`DispatchState`（:114）；`cancel_request`（:227）；未知请求回 -32601（:322） |
| 协议请求构造 | src/protocol/requests.rs | hover/definition/references/symbol/callHierarchy 构造器 |
| 协议通知构造 | src/protocol/notifications.rs | didOpen/didChange/didSave/initialized/publishDiagnostics 解析 |
| 错误类型 | src/error.rs | `LspError`（ContentModified 判定 `is_content_modified`） |

## 跨模块契约

- 事实源：`LspServerConfig`/`LspConfigSource` 定义于 `peri-acp-types/src/lsp.rs`，`peri-lsp/src/config.rs:8` 仅 re-export（3.0 批 2 波 1 迁出）；`LspPoolPort` 定义于 `peri-acp-types/src/ports.rs:158`，`LspServerPool` 实现于 pool.rs:327（跨层 downcast 经 `downcast_arc`）
- 消费方：本 crate 不直接被业务代码依赖，统一经 `peri-resources/src/lsp.rs` 门面出口（`pub use peri_lsp::*`）；实际调用方为 `peri-middlewares/src/lsp/`（middleware.rs:11-12、tool.rs:6-7）、`peri-middlewares/src/plugin/loader.rs:14`（`lsp_config_from_plugin`）
