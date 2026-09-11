# peri-lsp 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-11
> 依据：peri-lsp/src、peri-resources/src/lsp.rs、docs/standards/architecture-contracts.md

## 架构速览

- 数据流：`LspServerPool（扩展名路由）→ LspClient（连接与握手）→ MessageDispatcher（请求登记/分发/任务 owner）→ DiagnosticsRegistry`。
- `client::Connection` 在同一锁内保存当前 RegisteredConnection 与 ServerState，完成 initialize/initialized 后才发布 Running；RegisteredConnection 将 dispatcher 与文档版本缓存绑定到一次连接，start、restart、shutdown 共用 lifecycle mutex。
- pool 的 active 仅表示动态添加时是否自动启动，不保存第二份服务器就绪集合；实际就绪以 client 为准。

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 加/改服务器配置 | `src/config.rs` | `load_global_lsp_config` / `lsp_config_from_plugin` / `expand_env_vars` | 配置类型由 peri-acp-types 定义，此处 re-export；全局配置取 settings.json，插件支持注入的 CLAUDE_PLUGIN_ROOT |
| 改启动/握手/取消 | `src/client/lifecycle.rs` | `start` / `do_start` / `Startup` | 旧连接先关闭；私有 request_on 执行初始化，正常请求仅使用 Running 连接；失败回收，启动 future 被取消时撤回其注册 |
| 改关闭/重启 | `src/client/lifecycle.rs` | `shutdown` / `Shutdown` / `try_restart` / `check_and_increment_restart` | 关闭先撤回就绪但保留注册供重试 join；取消关闭等待时 begin_close 同步拒绝请求并终止任务；重启计数在固定窗口累计 |
| 发请求/通知 | `src/client/requests.rs` | `ready_dispatcher` / `request_on` / `request` / `notify` | 一次捕获同一 dispatcher 完成登记、发送和等待；超时覆盖排队/写入/响应，PendingRequest guard 在取消时清理原登记 |
| 改文件同步 | `src/client/documents.rs` + `src/client.rs` | `did_open` / `did_change` / `did_save`；`RegisteredConnection` / `infer_language_id` | 一次捕获 Running 连接及其缓存；先等 writer permit，文档锁内规划版本、序列化、同步准入与缓存提交，锁外等写入确认；准入前取消不记缓存，准入后取消不撤销已入队帧；旧连接任务不能写新缓存 |
| 改路由与动态服务器 | `src/pool.rs` | `ensure_initialized` / `ensure_server_for_file` / `add_server` / `shutdown` | 池操作锁串行化路由选择、添加与关闭；同名替换先关闭旧 client，清除其旧扩展名；ensure 查询当前 client 就绪 |
| 改进程管道 | `src/jsonrpc/transport.rs` | `LspTransport::spawn` / `read_message` / `kill` | 启动设置 kill_on_drop，独立 stdin/stdout 管道，立即 try_wait 检查早退 |
| 改请求登记/分发 | `src/jsonrpc/transport/dispatcher.rs` | `register_owned_request` / `DispatchState::dispatch` / `run_dispatch_loop` | admission 同锁管理 closed/pending/writer；guard 用原 state 弱引用和独立 token 防止删除复用 ID；双向请求先按 method 分类，result/error 响应才消费 pending |
| 改写入背压与取消 | `src/jsonrpc/transport/dispatcher.rs` + `dispatcher/writer.rs` | `reserve_notification` / `NotificationPermit::enqueue` / `WriteCompletion::wait`；`run` / `Frame` | 唯一 writer 持 stdin，16帧有界队列；容量等待、closed 门控下同步准入与实际写入确认分离，已入队帧不被调用者取消截断；错误关闭准入并通知上层 |
| 改任务回收 | `src/jsonrpc/transport/dispatcher.rs` | `begin_close` / `close` / `abort_and_join` | 同步拒绝 pending、请求杀进程并 abort；异步 close 回收 child、join writer/stdout/stderr/dispatch；join 句柄跨关闭取消留在 owner 槽位 |
| 改分帧和消息类型 | `src/jsonrpc/{codec,message}.rs` | `encode_message` / `decode_message` / `JsonRpcRequest` / `JsonRpcResponse` | Content-Length 分帧，body 上限64MB，头部大小写不敏感；服务器字符串 ID 原样响应 |
| 改诊断聚合 | `src/diagnostics.rs` | `handle_publish_diagnostics` / `get_for_file` / `summary` / `clear_all` | 按 URI 聚合与限流；client 的通知回调用当前 dispatcher 身份门控，旧连接关闭后再清重启诊断 |
| 新增协议方法 | `src/protocol/{requests,notifications}.rs` | `initialize_params` / `goto_definition_request` / `did_open_notification` | 请求构造携带递增 ID，通知不等待响应；lsp_types 经 protocol re-export |
| 改 URI 转换 | `src/uri.rs` | `path_to_uri` / `uri_to_path` | file:// 幂等、相对路径绝对化、RFC3986 percent encoding、Windows 盘符空 authority |

## 跨模块契约

- `LspServerConfig` / `LspConfigSource` 事实源在 `peri-acp-types/src/lsp.rs`；`LspPoolPort` 在 `peri-acp-types/src/ports.rs`，pool 实现该端口，装配通过 downcast 复用同一池。
- 业务消费者经 `peri-resources/src/lsp.rs` 门面访问；middleware 工具、middleware hook 与 plugin loader 不引入另一份连接池。
- 根 `client` 保留 LspClient / ServerState / DEFAULT_STARTUP_TIMEOUT_MS 公开路径；transport re-export MessageDispatcher / DispatchState / run_dispatch_loop。
- 公开 run_dispatch_loop 的外部调用方负责自己的任务；client 内部分发由 dispatcher 持有。Drop 仅作终止保底，不能替代显式 close 的异步 join。

## 验证入口

- `client_test.rs` / `client_lifecycle_test.rs`：真实 Perl wire 的并发握手、请求取消、启动/关闭取消、冷却窗口与进程清理。
- `client_document_test.rs`：真实 Perl wire 的 initialize gate、16槽 writer 背压下准入前/后取消，以及旧任务跨真实 restart 的新缓存隔离。
- `pool_test.rs`：实际服务器复用、关闭后重新 ensure、同名替换及扩展名更新。
- `jsonrpc/transport_test.rs` 经 `transport::dispatcher::tests` 挂载：双向同 ID、畸形响应、完整帧写入、队列背压、关闭与 pending guard。
- 目标命令：`cargo test -p peri-lsp --lib`；文档与平台证明范围遵循 `docs/standards/testing.md`。
