# PTY Server Rust 移植设计

**Date**: 2026-06-15
**Status**: Approved

## Problem

`side-projects/pty-server/` 当前是一个 Bun + TypeScript 实现的 Web PTY 终端：
- `server.ts` 用 `Bun.serve` 启动 HTTP + WebSocket
- 每个 WS 连接 spawn 一个 shell（默认 `/bin/bash`），用 Bun 内置 PTY API 实现交互
- `terminal.ts` + `index.html` 前端用 xterm.js

需要把它移植为 Rust crate，纳入主项目 Cargo workspace（与 `side-projects/git-graph` 一致），与主项目的工具链（cargo、lefthook、tracing）统一。

## Current State

| 文件 | 说明 |
|------|------|
| `server.ts` | Bun HTTP+WS 服务，约 100 行 |
| `terminal.ts` | xterm.js 前端初始化 + WS client，约 60 行 |
| `index.html` | 静态 HTML，引用本地 node_modules 的 xterm |
| `package.json` / `bun.lock` / `node_modules/` / `dist/` | Bun 构建链产物 |

主项目已有：`tokio` / `tracing` / `anyhow` / `serde` / `tokio-tungstenite = "0.24"`（peri-tui sync 模块用作 WS client）。主项目**未**使用 axum，也未引入任何 PTY 库。

## Goals

- **1:1 移植**：行为完全等价的 Web PTY 终端，Bun/TS 服务端用 Rust 替换
- 前端简化为**单 HTML 文件**（CDN 加载 xterm.js + 内联 JS），无前端构建步骤
- **单 bin crate**，纳入 workspace，复用 workspace 共享依赖
- **跨平台**：macOS / Linux / Windows

## Non-Goals

- 不抽 lib crate（YAGNI）
- 不对接主项目（不引入 peri-middlewares 集成）
- 不做 TLS、鉴权、多会话
- 不引入 headless browser 前端测试

## Design

### 1. 总体架构

**Crate 位置**：`side-projects/pty-server/`（保留原目录），加入根 `Cargo.toml` 的 workspace members。

**Crate 名**：`pty-server`（package name），单 bin。

**职责**：HTTP `GET /` 返回嵌入的 `index.html`；HTTP `GET /ws?shell=&args=&cols=&rows=` WebSocket upgrade 后每个连接 spawn 一个 portable-pty 子进程。

**协议**（与 Bun 原版严格一致）：

| 方向 | 格式 | 含义 |
|------|------|------|
| Client → Server | `string`（非 JSON） | stdin 直接写入 PTY |
| Client → Server | `{"type":"resize","cols":N,"rows":M}` | 调整 PTY 尺寸 |
| Server → Client | `string`（PTY 字节按 UTF-8 lossy 转 String） | PTY 输出 |
| Server → Client | `\r\n[process exited with code N]\r\n` | 进程退出，随后 close |

**环境变量**（与 Bun 原版对齐）：

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `PORT` | `3000` | HTTP/WS 监听端口 |
| `SHELL` | `/bin/bash` | 默认 shell |
| `RUST_LOG` | `info` | 日志级别 |

### 2. 模块划分

```
side-projects/pty-server/
├── Cargo.toml
├── index.html                 # CDN + 内联 JS 的单文件前端
├── README.md                  # 可选，极简
└── src/
    ├── main.rs                # CLI/env 解析 + tokio runtime + 启动 axum
    ├── config.rs              # Config::from_env()：PORT / SHELL / 默认尺寸
    ├── pty_session.rs         # portable-pty 封装：spawn / read / write / resize / kill
    ├── ws_handler.rs          # axum WebSocket upgrade + 消息循环
    └── http_routes.rs         # GET / 返回 include_str!("../index.html")
tests/
└── ws_e2e_test.rs             # tokio-tungstenite client 端到端
```

**各模块职责**：

| 模块 | 职责 | 不做的事 |
|------|------|----------|
| `main.rs` | 解析 env，构造 Router，启动 axum + graceful shutdown | 不处理业务逻辑 |
| `config.rs` | `Config::from_env() -> anyhow::Result<Config>` | 不读 env 以外的来源 |
| `pty_session.rs` | `PtySession::spawn(shell, args, cols, rows) -> io::Result<Self>`、`.read(&mut buf) -> io::Result<usize>`（**同步阻塞**，由上层 `spawn_blocking` 包裹）、`.write(&[u8]) -> io::Result<()>`、`.resize(cols, rows) -> io::Result<()>`、`.try_wait_exit() -> io::Result<Option<i32>>` | 不做 WebSocket 协议解析，不自己异步化 |
| `ws_handler.rs` | `async fn ws_handler(ws, query)`：升级 WS、spawn PTY、双向 pump、退出时 close | 不直接调 portable-pty（委托给 PtySession） |
| `http_routes.rs` | `async fn index() -> impl IntoResponse` 返回 `[(CONTENT_TYPE, "text/html; charset=utf-8")], include_str!("../index.html")` | 不读文件系统 |

**关键 struct**：

```rust
pub struct Config {
    pub port: u16,
    pub shell: String,
    pub default_cols: u16,
    pub default_rows: u16,
}

pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}
```

**PTY read 异步策略**：portable-pty 的 `PtySession::read` 是同步阻塞 API。用 `tokio::task::spawn_blocking` 在阻塞线程池循环 read，通过 `tokio::sync::mpsc::channel::<Vec<u8>>(16)` 把字节流传回 async 侧。WS→PTY 方向（write / resize）由 pump_task 在 async 上下文直接调用 `PtySession::write` / `PtySession::resize`（两者内部短暂同步但很快）。resize 与 write 都在 pump_task 内串行执行，保证 `master` 唯一写者。

### 3. 数据流（WebSocket 连接生命周期）

**连接建立**：

```
1. Client: new WebSocket('ws://host:port/ws?shell=bash&cols=80&rows=24')
2. axum: WebSocketUpgrade + Query<WsQuery> 提取参数
3. ws_handler:
   a. PtySession::spawn(shell, args, cols, rows) → master + child
   b. 创建 mpsc::channel::<Vec<u8>>(16)，用于 read_task → pump_task 推送 PTY 字节
   c. 启动 read_task（spawn_blocking 循环 PtySession::read → tx.send(bytes)；EOF 时发 None 哨兵）
   d. pump_task 用 select! { ws.recv() | rx.recv() } 双向分发
4. WebSocket 连接建立完成
```

**两条数据流（pump_task 同时承担两个方向）**：

```
PTY child ──stdout/stderr──▶ read_task(spawn_blocking)
                                  │
                                  └─mpsc Vec<u8>─▶ pump_task ──ws.send(text)──▶ Client (xterm)
Client   ──ws.recv()────────────────────────────▶ pump_task ──parse──────────▶ PTY stdin
                                                                  │
                                                                  ├─ Text → PtySession::write(bytes)
                                                                  └─ JSON resize → PtySession::resize(cols, rows)
```

**二进制处理**：与 Bun 原版一致，PTY 字节按 UTF-8 lossy 转 `String`，发 WS text 帧。binary WS 帧不支持。

**退出流程**：

```
1. PTY 子进程退出 → PtySession::read() 返回 Ok(0)（EOF）
2. read_task 检测到 EOF，发 None 哨兵给 pump_task 后退出
3. pump_task 收到 None 哨兵：
   a. 调用 PtySession::try_wait_exit() 拿退出码（同步、子进程已退出所以快）
   b. ws.send("\r\n[process exited with code N]\r\n")（N=unknown 时显示 unknown）
   c. ws.close()
4. pump_task 退出，drop PtySession（master + child）触发 portable-pty Drop 自动 kill 残留
```

**Client 断开**：

```
1. pump_task 的 ws.recv() 返回 None
2. pump_task 退出，drop PtySession（master + child）触发 portable-pty Drop 清理
3. read_task 的 PtySession::read() 返回 EOF 或 Err，自动结束
```

**并发约束**：

- `PtySession` 的 `write` / `resize` / `try_wait_exit` 都只在 pump_task 内调用，保证 `master` 唯一写者
- `child` 的 kill 由 portable-pty 的 `Child::Drop` 自动处理
- 退出消息发送后再 close，确保 client 看到退出码

### 4. 错误处理

**错误类型分层**：

| 层 | 类型 | 策略 |
|----|------|------|
| `main.rs` | `anyhow::Result<()>` + `.context()` | 启动失败 `?` 抛出，进程退出码非零 |
| `config.rs` | `anyhow::Result<Config>` | env 解析失败 → anyhow context |
| `pty_session.rs` | `io::Result<...>` | spawn/resize 失败保留裸 `io::Error`，由上层决定是否 abort 连接 |
| `ws_handler.rs` | `anyhow::Result<()>` + `tracing::warn!` | 单连接失败不影响其他连接，**绝不**冒泡到 axum 主循环 |

**关键场景**：

1. **PTY spawn 失败**（shell 路径错误）：
   ```
   ws_handler 立刻 ws.send("\r\n[failed to spawn: {err}]\r\n") → ws.close()
   不让连接 hang，client 看到 clear 错误信息
   ```

2. **PTY read 中途错误**（master 被对端关闭）：
   ```
   read_task 遇 Err → tracing::debug! → 退出 task（与 EOF 同路径）
   ```

3. **PTY write 失败**（client 发数据但 PTY 已退出）：
   ```
   pump_task 的 PtySession::write() 遇 Err → tracing::debug! → break 循环
   ```

4. **WS frame 解析失败**（JSON parse 错或非 text frame）：
   ```
   与 Bun 原版一致：JSON.parse 失败 → catch 吞掉 → 当作普通 stdin 直接 write
   非 text frame（binary）→ 忽略
   ```

5. **端口被占用**（bind 失败）：
   ```
   axum::serve bind 失败 → main.rs 的 ? 抛出 → tracing::error! → 进程退出码非零
   ```

**Graceful shutdown**：

```
main.rs 监听 tokio::signal::ctrl_c → axum::serve 的 .with_graceful_shutdown()
→ 等所有活跃 WS 连接自然结束（不强制 kill 已连接的 PTY 子进程）
→ 进程退出
```

**日志规范**（遵循 CLAUDE.md）：

- 全程 `tracing`，禁 `println!`/`eprintln!`
- `info`：启动日志、WS 连接建立/断开
- `debug`：PTY spawn 参数、resize 事件
- 错误统一 `warn!`（连接级）或 `error!`（启动级）

### 5. 测试策略

**测试金字塔**（CLAUDE.md 测试规范：中文注释、`_test.rs` 同目录分离、`test_<对象>_<场景>` 命名、Arrange-Act-Assert、最小依赖）：

| 层级 | 测试内容 | 文件 |
|------|---------|------|
| 单元 | `config.rs` env 解析 | `src/config_test.rs` |
| 单元 | `pty_session.rs` spawn/read/write/resize/kill | `src/pty_session_test.rs` |
| 单元 | `http_routes.rs` index 路由 | `src/http_routes_test.rs` |
| 集成 | WS 协议端到端 | `tests/ws_e2e_test.rs` |

**关键测试用例**：

```rust
// pty_session_test.rs
test_pty_session_spawn_echo_and_read       // spawn echo hello，read 含 "hello"
test_pty_session_resize_does_not_panic     // spawn cat，resize 不 panic
test_pty_session_kill_terminates_child     // spawn sleep，drop，child 已退出

// ws_e2e_test.rs
test_ws_connection_echo_roundtrip          // shell=cat，ws.send("hi\n") 收到 "hi\n"
test_ws_connection_receives_exit_message   // shell=echo，spawn 后退出，收到 [process exited...]
test_ws_connection_spawn_failure_sends     // shell=/nonexistent，收到 [failed to spawn...]
```

**跨平台测试**：

- Windows 默认 shell 是 `cmd.exe`，不是 bash。集成测试用 `env::var("COMSPEC")` 或 `env::var("SHELL")` 兜底。
- `cat` 在 Windows 不存在。集成测试用 `echo`（双平台都有）验证最简回显。
- 通过 `fn test_shell() -> &'static str` helper 集中处理平台差异。

**不测试**：

- 前端 JS（手测，无 headless browser）
- graceful shutdown 精确时序
- axum/tokio-tungstenite/portable-pty 本身（信任上游）

### 6. 构建与发布

**目录最终形态**：

```
side-projects/pty-server/
├── Cargo.toml
├── index.html
├── README.md（可选）
├── src/
│   ├── main.rs
│   ├── config.rs
│   ├── config_test.rs
│   ├── pty_session.rs
│   ├── pty_session_test.rs
│   ├── ws_handler.rs
│   ├── ws_handler_test.rs
│   ├── http_routes.rs
│   └── http_routes_test.rs
└── tests/
    └── ws_e2e_test.rs
```

**删除清单**：`server.ts`、`terminal.ts`、`package.json`、`bun.lock`、`node_modules/`、`dist/`。

**根 `Cargo.toml` 改动**：

```toml
[workspace]
members = [
    "peri-agent",
    "peri-middlewares",
    "peri-tui",
    "peri-acp",
    "langfuse-client",
    "peri-widgets",
    "peri-lsp",
    "side-projects/git-graph",
    "side-projects/pty-server",      # 新增
    "agm",
]

[workspace.dependencies]
# 新增
axum = { version = "0.7", features = ["ws"] }
portable-pty = "0.8"
# tokio-tungstenite 已有 0.24，直接复用
```

**`side-projects/pty-server/Cargo.toml`**：

```toml
[package]
name = "pty-server"
version.workspace = true
edition.workspace = true

[dependencies]
tokio = { workspace = true }
tokio-util = { workspace = true }
futures = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
anyhow = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
axum = { workspace = true }
tokio-tungstenite = { workspace = true }
portable-pty = { workspace = true }
```

**开发命令**：

| 操作 | 命令 |
|------|------|
| 开发运行 | `cargo run -p pty-server`（PORT=3000） |
| 指定端口/shell | `PORT=8080 SHELL=/bin/zsh cargo run -p pty-server` |
| Release 构建 | `cargo build -p pty-server --release` |
| 测试 | `cargo test -p pty-server` |
| 单测 | `cargo test -p pty-server --lib -- test_pty_session_spawn` |

**index.html 重写要点**：

- 删除 `<link rel="stylesheet" href="xterm/css/xterm.css">`，换为 CDN：
  ```html
  <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/xterm@5.3.0/css/xterm.css">
  <script src="https://cdn.jsdelivr.net/npm/xterm@5.3.0/lib/xterm.min.js"></script>
  <script src="https://cdn.jsdelivr.net/npm/@xterm/addon-fit@0.11.0/lib/addon-fit.min.js"></script>
  <script src="https://cdn.jsdelivr.net/npm/@xterm/addon-web-links@0.12.0/lib/addon-web-links.min.js"></script>
  ```
- `terminal.ts` 内容内联到 `<script>`（去 TS 类型，~60 行 JS）
- 其余（HTML 结构、CSS 主题、xterm 初始化、WS 连接逻辑）保持原样

**部署**：

```bash
cargo build -p pty-server --release
# 产物：target/release/pty-server（或 .exe）
# 单一二进制，零外部依赖（CDN 由浏览器解析，服务端无需前端文件）
./target/release/pty-server
```

**集成检查**（重构后必跑）：

```bash
cargo build                                    # 全 workspace 编译通过
cargo test -p pty-server                       # 本 crate 测试通过
cargo clippy -p pty-server -- -D warnings      # 无 warning
cargo fmt -p pty-server                        # 格式化
lefthook run pre-commit                        # 全量 pre-commit 通过
```

## Future Work

- TLS 支持（`axum-server` + rustls）
- 鉴权（query token 或 header）
- 多会话/标签页
- 把 `PtySession` 抽成 lib crate 供 `peri-middlewares` 复用（届时本 bin crate 改为依赖该 lib）

## Risks

| 风险 | 缓解 |
|------|------|
| portable-pty 在 Windows ConPTY 行为差异 | 集成测试用跨平台 shell helper，CI 跑双平台 |
| WS text 帧用 UTF-8 lossy 转换会破坏二进制序列（鼠标 SGR 等） | 与 Bun 原版行为一致，xterm.js 客户端用 string 模式接收，正常 |
| axum 0.7 ws API 与 tungstenite 0.24 版本兼容 | axum::extract::ws 内部用 tungstenite，已对齐版本 |
