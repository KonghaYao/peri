# peri-web-pty 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-08-16
> 依据：peri-web-pty/src 源码、README.md

## 架构速览

- 数据流：`浏览器/xterm.js ↔ WebSocket（axum）↔ read_task/pump_task（spawn_blocking 阻塞读）↔ portable_pty（PtySession）↔ shell`
- 入口：`start_server`（lib.rs:24）— axum Router：`GET /`（index.html）+ `GET /ws`（ws_handler），优雅关闭 Ctrl-C/SIGTERM；启动后尝试打开系统浏览器（lib.rs:58）
- 稳定不变量：连接 = 一个 PTY 进程（ws_handler.rs 注释「PTY 连接建立」）；平台差异集中在 pty_session.rs（Unix drop slave / Windows 保活 ConPTY + `normalize_crlf`）；child 退出探测 Windows 靠 100ms 轮询 try_wait（CHILD_EXIT_POLL_INTERVAL ws_handler.rs:25）

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改启动参数/端口 | `src/config.rs` + `src/lib.rs` | `Config::from_args`（config.rs:38，clap）；`from_env`（:44，嵌入式场景，initial_cmd 默认 `"peri"`）；`start_server`（lib.rs:24） | env：HOST/PORT/SHELL/CWD/CMD；port 默认 0 = 随机分配并打印实际 URL（lib.rs:41-44）；`default_shell`（config.rs:72，Windows 默认 powershell.exe） |
| 改 PTY spawn 细节 | `src/pty_session.rs` | `PtySession::spawn`（:68，返回 (Self, reader)）；`write`（:124）；`resize`（:145）；`kill`（:166）；`try_wait_exit`（:157） | Unix 必须在 spawn 内 drop slave 否则 master read 永不 EOF（:105-106）；Windows slave 保活到 close_slave（:182，提前 drop 破坏 ConPTY 引用计数）；TERM=xterm-256color |
| 改 Windows 换行/DSR 处理 | `src/pty_session.rs` + `src/ws_handler.rs` | `normalize_crlf`（pty_session.rs:36，仅 Windows）；read_task 内 DSR 应答（ws_handler.rs:154-168） | 裸 `\r`/`\n` 归一化为 `\r\n`（PSReadLine 只认）；`\x1b[6n` 光标查询回复 `\x1b[1;1R`（仅一次，跨 read 块保留 3 字节尾部拼接） |
| 改 WebSocket 协议 | `src/ws_handler.rs` | `WsQuery::to_spawn_params`（:46）；`handle_socket`（:85）；`try_handle_resize`（:287） | 文本/二进制帧等价（binary 按 UTF-8 lossy 解码）；`{"type":"resize","cols","rows"}` JSON 消息拦截为 resize；其余文本写入 PTY stdin |
| 改 PTY→WS 读取泵 | `src/ws_handler.rs` | read_task（:134，spawn_blocking）；pump select 循环（:210） | mpsc `Option<Vec<u8>>`（None = EOF 哨兵）；跨 4096 字节边界的 UTF-8 残字节缓冲（leftover，:169-200）；EOF 后 `close_slave` + `send_exit_message` |
| 改首会话命令注入 | `src/session_state.rs` | `SessionState::new`（:24）；`try_mark_done`（:33） | 全局只注入一次（Arc<Mutex<bool>> 原子标记）；注入在 spawn 后延迟 200ms（ws_handler.rs:111-118） |

## 子系统

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 服务入口/路由 | src/lib.rs | `start_server`（:24）；`open_browser`（:58，失败静默）；`shutdown_signal`（:78） |
| 启动配置 | src/config.rs | `Config`（:6，clap Parser + env） |
| PTY 会话 | src/pty_session.rs | `PtySession`（:14）；`clone_writer`（:140，共享写入句柄）；`Drop`（:188 尽力 kill）；`io_err`（:196） |
| 服务端共享状态 | src/session_state.rs | `SessionState`（:4，cwd + initial_cmd + first_session_done） |
| WebSocket 处理 | src/ws_handler.rs | `ws_handler`（:76，upgrade）；`handle_socket`（:85，spawn + 双泵 + 退出轮询） |
| 首页 HTTP | src/http_routes.rs | `index`（:7，内嵌 HTML） |
| 独立可执行 | src/main.rs | `main`（:5，`start_server(Config::from_args())`） |

## 测试

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| WebSocket 端到端 | tests/ws_e2e_test.rs | `spawn_server`（:20）；`test_ws_connection_receives_exit_message_on_child_exit`（:44）；spawn 失败发错误文本并关闭（:75） |
| PTY 会话单测 | src/pty_session_test.rs | spawn/resize/kill/close_slave 平台行为（Windows ConPTY 语义） |

## 跨模块契约

- 消费方：`peri-tui/src/main.rs:587-595` — `peri web` 命令：`peri_web_pty::config::Config::from_env()` + `peri_web_pty::start_server(config)`（block_on 运行）
- 独立可执行：`src/main.rs` 为独立 bin 入口；`start_server` 以库形式供嵌入
- Windows ConPTY 行为以 `src/pty_session.rs`、`src/ws_handler.rs` 的平台分支及 `src/pty_session_test.rs` / `tests/ws_e2e_test.rs` 为事实源；历史事故过程不在 `docs/` 保留
