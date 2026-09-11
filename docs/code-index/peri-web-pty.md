# peri-web-pty 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-11
> 依据：peri-web-pty/src 源码、README.md（本 crate 无 CLAUDE.md/AGENTS.md）。

## 架构速览

- 数据流：浏览器/xterm.js ↔ WebSocket ↔ 平台 connection ↔ portable-pty shell。Unix 使用 AsyncFd 非阻塞 PTY 读写；Windows 保留 ConPTY 阻塞 reader adapter。
- 入口：`start_server` 装配 `/` 与 `/ws`，`ws_handler` 在升级后 spawn PTY 并交给平台 owner；一个连接对应一个 child。
- Unix 连接独占 nonblocking 转换；普通公共 `PtySession::spawn/write/clone_writer` 实例继续使用原同步接口。正常终态显式关闭 I/O，再在 blocking worker kill/wait 并 join；shutdown await 被取消后保留 handle 供重试，整体 owner Drop 仅让后台收尾继续，不代表已 join。

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改启动参数/端口 | `src/config.rs` + `src/lib.rs` | `Config::from_args`、`from_env`、`default_shell`、`start_server` | HOST/PORT/SHELL/CWD/CMD；port 0 分配随机端口；启动后尝试打开浏览器 |
| 改 PTY spawn/同步 API | `src/pty_session.rs` | `PtySession::spawn`、`write`、`resize`、`clone_writer` | TERM=xterm-256color；Unix spawn 内释放 slave，Windows 保留 slave 引用；Windows 输入经 normalize_crlf；公共路径保持阻塞语义 |
| 改 WebSocket 查询参数与升级 | `src/ws_handler.rs` | `WsQuery::to_spawn_params`、`ws_handler`、`handle_socket` | shell/空白拆分 args/尺寸缺省解析；spawn 失败发原错误文本并关闭；平台 owner 接收全部会话资源 |
| 改 resize/UTF-8/DSR 协议 | `src/ws_handler/protocol.rs` | `try_handle_resize`、`OutputDecoder::push/finish`、`exit_message` | Text/Binary 都按文本解释，resize 消耗输入；跨块 UTF-8 残字节仅在结束时 flush，DSR 跨块探测并只回应一次；退出消息格式统一 |
| 改 Unix 连接/退出状态 | `src/ws_handler/connection.rs` | `Connection::new/shutdown`、`run` | 固定 interval 检查 child；reap 后读尽当前可读 PTY 与有界输出队列再发送退出消息，不依赖其他 slave holder 产生 EOF；WebSocket feed/flush 分开记录进度，取消不会重发输出 |
| 改 PTY 待写输入与背压 | `src/ws_handler/input.rs` | `InputQueue::enqueue/pending/advance/close` | 用户输入、初始命令、DSR 回复共用 16 帧 admission；部分写入保留 offset，满队列统一过载关闭，写端关闭后丢弃所有晚到输入 |
| 改 Unix PTY readiness | `src/ws_handler/io.rs` + `src/pty_session.rs` | `PtyIo::new/read/write/try_read`、`master_fd`、`finish` | 连接私有 fd 转换使用 AsyncFd；输入背压不阻塞 reactor，闲读可取消；待写输入达到 16 帧后再收到数据即关闭连接，仍始终读取 Close/Ping；Unix EIO 视为 PTY EOF；kill/wait 委托 blocking worker |
| 改 Windows ConPTY adapter | `src/ws_handler/windows.rs` + `src/pty_session.rs` | `run`、`normalize_crlf`、`close_slave` | 保留阻塞 read_task 与 DSR writer；child 使用固定 interval；slave/master 共持 ConPTY 引用，单独 close_slave 不保证 reader EOF；原生取消与 join 尚未完成，不能把 abort 当作读线程已退出 |
| 改首会话命令注入 | `src/session_state.rs` + 平台 connection | `SessionState::try_mark_done`、`run` | 全局原子标记确保只注入一次；命令仍在 200ms 后写入，Unix 等待期间继续处理连接事件 |
| 改服务入口与首页 | `src/lib.rs` + `src/http_routes.rs` | `start_server`、`shutdown_signal`、`open_browser`、`index` | axum HTTP/WS 路由；Ctrl-C/SIGTERM 触发服务关闭；HTML 内嵌；独立 bin 在 main.rs |

## 测试与平台边界

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| WebSocket 基础端到端 | `tests/ws_e2e_test.rs` | child exit 文本、spawn 失败错误与关闭 |
| Unix 连接端到端 | `tests/ws_lifecycle_test.rs` | 持续 Ping 不重置 child 检查、完整 CJK/emoji tail 先于 exit、断连 reap、满 PTY 输入时仍响应断连、Text/Binary resize 与 stdin 等价 |
| Unix I/O owner | `src/ws_handler/connection_test.rs` | 闲读取消后 shutdown 显式 reap；另一个 fixture 仍持有 slave 时也不依赖 EOF；blocking pool 饱和下取消 shutdown 后重试原 join |
| 输入队列契约 | `src/ws_handler/input_test.rs` | 部分写入与满队列下的真实跨块 DSR 解码、关闭写端后的晚到 query/command；拒绝新输入不覆盖已接收字节 |
| 输出协议 | `src/ws_handler/protocol_test.rs` | 多字节每个 read 边界、跨块 DSR 仅回应一次、不完整尾字节只 flush 一次 |
| 同步会话与配置 | `src/pty_session_test.rs`、`src/config_test.rs`、`src/ws_handler_test.rs` | 既有同步 spawn/read/write/resize、Windows CRLF、配置与查询参数 |

Windows 的 ConPTY 原生读取消、所有 writer/master/slave 关闭次序与 reader join 需要 Windows 真实 fixture/CI 验证；Unix fixture 不构成该平台的回收证据。仍需实施与验收的边界见 [ConPTY 关闭 owner](../../spec/issues/2026-09-11-windows-conpty-close-owner.md)。

## 跨模块契约

- `peri-tui` 的 `peri web` 通过 `Config::from_env` 与 `start_server` 嵌入；公开调用路径不变。
- 通用 Rust async 与平台测试边界：`docs/standards/rust.md`（RUST-ASYNC-001）、`docs/standards/testing.md`。
