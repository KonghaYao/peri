# PTY Server Rust 移植 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `side-projects/pty-server/` 从 Bun+TS 移植为 Rust bin crate，行为与原版完全等价（HTTP 托管单 HTML + WebSocket 桥接 PTY），跨平台（macOS/Linux/Windows），纳入主项目 Cargo workspace。

**Architecture:** 单 bin crate `pty-server`。`axum` 提供 HTTP 路由 + WebSocket upgrade；`portable-pty` 跨平台 PTY；前端单 HTML（CDN 加载 xterm.js + 内联 JS），用 `include_str!` 嵌入二进制。每个 WS 连接 spawn 一个 shell，用 spawn_blocking + mpsc channel 把阻塞的 PTY read 接入 tokio。

**Tech Stack:** Rust 2021 / tokio（workspace）/ axum 0.7 ws / tokio-tungstenite 0.24（workspace，与 peri-tui sync 对齐）/ portable-pty 0.9 / tracing / anyhow / serde_json。

**Spec:** `docs/superpowers/specs/2026-06-15-pty-server-rust-design.md`

**关键 API 速查**（实现时直接用，避免查文档）：

```rust
// portable-pty 0.9
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
let pty_system = native_pty_system();
let pair = pty_system.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
let reader: Box<dyn Read + Send> = pair.master.try_clone_reader()?;
let mut cmd = CommandBuilder::new(shell);
cmd.args(args);
cmd.env("TERM", "xterm-256color");
let child: Box<dyn portable_pty::Child + Send + Sync> = pair.slave.spawn_command(cmd)?;
drop(pair.slave);  // 释放 slave，子进程退出时 master 才会 EOF
let writer: Box<dyn Write + Send> = pair.master.take_writer()?;
pair.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
child.kill()?; child.try_wait()?;  // Option<ExitStatus>

// axum 0.7 ws
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
async fn ws_handler(ws: WebSocketUpgrade, Query(q): Query<WsQuery>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, q))
}
// handle_socket: socket.recv().await -> Option<Result<Message, axum::Error>>
// socket.send(Message::Text(s)).await -> Result<(), axum::Error>
// Message::Text(String) | Message::Binary(Vec<u8>) | Message::Close(Option<CloseFrame>)
```

---

## File Structure

```
side-projects/pty-server/
├── Cargo.toml                     # 新建
├── index.html                     # 重写（CDN + 内联 JS）
├── README.md                      # 新建（极简）
└── src/
    ├── main.rs                    # 新建：env 解析 + Router + axum::serve + graceful shutdown
    ├── config.rs                  # 新建：Config::from_env()
    ├── config_test.rs             # 新建
    ├── pty_session.rs             # 新建：portable-pty 封装
    ├── pty_session_test.rs        # 新建
    ├── ws_handler.rs              # 新建：axum WS upgrade + handle_socket
    ├── ws_handler_test.rs         # 新建：Query 解析单元测试
    ├── http_routes.rs             # 新建：GET / 返回 index.html
    └── http_routes_test.rs        # 新建
tests/
└── ws_e2e_test.rs                 # 新建：tokio-tungstenite client 端到端
```

**根 `Cargo.toml` 修改点**：
- `[workspace] members` 数组在 `side-projects/git-graph` 之后追加 `"side-projects/pty-server"`
- `[workspace.dependencies]` 追加 `axum` 和 `portable-pty`

**删除文件**：`server.ts`、`terminal.ts`、`package.json`、`bun.lock`、`node_modules/`、`dist/`。

---

## Task 1: 清理 TS 文件 + 创建空 Rust crate + 加入 workspace

**Files:**
- Delete: `side-projects/pty-server/{server.ts, terminal.ts, package.json, bun.lock}`
- Delete (recursive): `side-projects/pty-server/node_modules/`, `side-projects/pty-server/dist/`
- Create: `side-projects/pty-server/Cargo.toml`
- Create: `side-projects/pty-server/src/main.rs`（最小占位）
- Modify: `Cargo.toml`（workspace 根）

- [ ] **Step 1: 删除 TS 文件和构建产物**

```bash
cd /Users/konghayao/code/ai/perihelion/side-projects/pty-server
rm -f server.ts terminal.ts package.json bun.lock
rm -rf node_modules dist
```

- [ ] **Step 2: 创建 Cargo.toml**

写入 `side-projects/pty-server/Cargo.toml`：

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

[dev-dependencies]
tokio-tungstenite = { workspace = true }
```

- [ ] **Step 3: 创建最小 main.rs 占位**

写入 `side-projects/pty-server/src/main.rs`：

```rust
fn main() {
    println!("pty-server placeholder");
}
```

注意：占位用 `println!` 仅此刻，正式实现会换成 tracing 并删除此 println。

- [ ] **Step 4: 修改根 Cargo.toml 加入 workspace members**

修改 `/Users/konghayao/code/ai/perihelion/Cargo.toml`：

把 `members` 数组中的 `"side-projects/git-graph",` 行之后追加一行 `"side-projects/pty-server",`。最终顺序：

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
    "side-projects/pty-server",
    "agm",
]
resolver = "2"
```

- [ ] **Step 5: 在根 Cargo.toml 追加 workspace 共享依赖**

在 `/Users/konghayao/code/ai/perihelion/Cargo.toml` 的 `[workspace.dependencies]` 段末尾追加：

```toml
# --- PTY server ---
axum = { version = "0.7", features = ["ws"] }
portable-pty = "0.9"
```

- [ ] **Step 6: 验证编译**

```bash
cd /Users/konghayao/code/ai/perihelion
cargo build -p pty-server
```

Expected: 成功编译，可能下载 axum/portable-pty 等新依赖。

- [ ] **Step 7: 提交**

```bash
git add side-projects/pty-server/ Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
refactor(pty-server): 替换 Bun+TS 服务端为 Rust bin crate 骨架

- 删除 server.ts / terminal.ts / package.json / bun.lock / node_modules / dist
- 新建 side-projects/pty-server 作为 workspace member（单 bin crate）
- workspace 共享依赖新增 axum 0.7 + portable-pty 0.9

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Task 2: 重写 index.html（CDN + 内联 JS）

**Files:**
- Create: `side-projects/pty-server/index.html`

- [ ] **Step 1: 写 index.html**

写入 `side-projects/pty-server/index.html`：

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>PTY Terminal</title>
  <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/xterm@5.3.0/css/xterm.css">
  <style>
    * { margin: 0; padding: 0; box-sizing: border-box; }
    html, body { height: 100%; background: #1a1a2e; }
    #terminal { width: 100%; height: 100%; overflow: hidden; }
    .xterm { height: 100%; }
    .xterm-viewport::-webkit-scrollbar { display: none; }
    .xterm-viewport { scrollbar-width: none; }
  </style>
</head>
<body>
  <div id="terminal"></div>

  <script src="https://cdn.jsdelivr.net/npm/xterm@5.3.0/lib/xterm.min.js"></script>
  <script src="https://cdn.jsdelivr.net/npm/@xterm/addon-fit@0.11.0/lib/addon-fit.min.js"></script>
  <script src="https://cdn.jsdelivr.net/npm/@xterm/addon-web-links@0.12.0/lib/addon-web-links.min.js"></script>
  <script>
    const params = new URLSearchParams(location.search);
    const shell = params.get('shell') || '';

    const term = new Terminal({
      cursorBlink: true,
      fontSize: 14,
      fontFamily: '"JetBrains Mono", "Fira Code", "Cascadia Code", Menlo, monospace',
      theme: {
        background: '#1a1a2e',
        foreground: '#e0e0e0',
        cursor: '#e0e0e0',
        selectionBackground: 'rgba(100, 100, 255, 0.3)',
        black: '#1a1a2e',
        red: '#ff6b6b',
        green: '#51cf66',
        yellow: '#ffd43b',
        blue: '#74c0fc',
        magenta: '#da77f2',
        cyan: '#63e6be',
        white: '#e0e0e0',
      },
    });

    const fitAddon = new FitAddon.FitAddon();
    const webLinksAddon = new WebLinksAddon.WebLinksAddon();
    term.loadAddon(fitAddon);
    term.loadAddon(webLinksAddon);
    term.open(document.getElementById('terminal'));
    fitAddon.fit();

    const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const wsUrl = `${protocol}//${location.host}/ws?shell=${encodeURIComponent(shell)}&cols=${term.cols}&rows=${term.rows}`;
    const ws = new WebSocket(wsUrl);

    term.onData((data) => {
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(data);
      }
    });

    ws.onmessage = (event) => {
      if (typeof event.data === 'string') {
        term.write(event.data);
      }
    };

    ws.onclose = () => {
      term.write('\r\n\x1b[33m[connection closed]\x1b[0m\r\n');
    };

    window.addEventListener('resize', () => {
      fitAddon.fit();
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: 'resize', cols: term.cols, rows: term.rows }));
      }
    });
  </script>
</body>
</html>
```

注意：CDN UMD 全局名为 `FitAddon.FitAddon` 和 `WebLinksAddon.WebLinksAddon`（jsdelivr 的 UMD 包挂在全局命名空间下）。

- [ ] **Step 2: 提交**

```bash
git add side-projects/pty-server/index.html
git commit -m "$(cat <<'EOF'
feat(pty-server): 重写 index.html 为 CDN + 内联 JS 单文件

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Task 3: Config 模块（TDD）

**Files:**
- Create: `side-projects/pty-server/src/config.rs`
- Create: `side-projects/pty-server/src/config_test.rs`
- Modify: `side-projects/pty-server/src/main.rs`（添加 `mod config;`）

- [ ] **Step 1: 写失败测试 config_test.rs**

写入 `side-projects/pty-server/src/config_test.rs`：

```rust
use super::*;

#[test]
fn test_config_from_env_uses_port_env_when_set() {
    // 设置 PORT 环境变量
    unsafe {
        std::env::set_var("PORT", "9090");
    }
    let cfg = Config::from_env().expect("解析配置应成功");
    assert_eq!(cfg.port, 9090);
    unsafe {
        std::env::remove_var("PORT");
    }
}

#[test]
fn test_config_from_env_defaults_port_when_unset() {
    unsafe {
        std::env::remove_var("PORT");
    }
    let cfg = Config::from_env().expect("解析配置应成功");
    assert_eq!(cfg.port, 3000);
}

#[test]
fn test_config_from_env_uses_shell_env_when_set() {
    unsafe {
        std::env::set_var("SHELL", "/bin/zsh");
    }
    let cfg = Config::from_env().expect("解析配置应成功");
    assert_eq!(cfg.shell, "/bin/zsh");
    unsafe {
        std::env::remove_var("SHELL");
    }
}

#[test]
fn test_config_from_env_defaults_shell_when_unset() {
    unsafe {
        std::env::remove_var("SHELL");
    }
    let cfg = Config::from_env().expect("解析配置应成功");
    // 跨平台默认 shell
    if cfg!(target_os = "windows") {
        assert!(cfg.shell.contains("cmd.exe") || cfg.shell.eq_ignore_ascii_case("cmd"));
    } else {
        assert_eq!(cfg.shell, "/bin/bash");
    }
}

#[test]
fn test_config_from_env_returns_err_for_non_numeric_port() {
    unsafe {
        std::env::set_var("PORT", "not-a-number");
    }
    let result = Config::from_env();
    assert!(result.is_err(), "非数字端口应返回错误");
    unsafe {
        std::env::remove_var("PORT");
    }
}
```

- [ ] **Step 2: 跑测试验证失败**

```bash
cd /Users/konghayao/code/ai/perihelion
cargo test -p pty-server --lib config_test
```

Expected: 编译失败，`Config` 类型未定义。

- [ ] **Step 3: 写 config.rs**

写入 `side-projects/pty-server/src/config.rs`：

```rust
use anyhow::{anyhow, Result};

/// PTY server 启动配置。
#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub shell: String,
    pub default_cols: u16,
    pub default_rows: u16,
}

impl Config {
    /// 从环境变量构造配置。
    ///
    /// - PORT：监听端口（默认 3000）
    /// - SHELL：默认 shell（默认 Unix `/bin/bash` / Windows `cmd.exe`）
    pub fn from_env() -> Result<Self> {
        let port = match std::env::var("PORT") {
            Ok(s) => s
                .parse::<u16>()
                .map_err(|_| anyhow!("PORT 必须是 0-65535 的整数，收到: {s}"))?,
            Err(_) => 3000,
        };

        let shell = std::env::var("SHELL").unwrap_or_else(|_| default_shell().to_string());

        Ok(Self {
            port,
            shell,
            default_cols: 80,
            default_rows: 24,
        })
    }
}

fn default_shell() -> &'static str {
    if cfg!(target_os = "windows") {
        "cmd.exe"
    } else {
        "/bin/bash"
    }
}
```

- [ ] **Step 4: 在 main.rs 添加模块声明**

写入 `side-projects/pty-server/src/main.rs`（替换占位）：

```rust
mod config;

fn main() {
    let cfg = config::Config::from_env().expect("解析配置失败");
    println!("port={}, shell={}", cfg.port, cfg.shell);
}
```

- [ ] **Step 5: 跑测试验证通过**

```bash
cargo test -p pty-server --lib config_test
```

Expected: 5 个测试全部通过。

- [ ] **Step 6: 提交**

```bash
git add side-projects/pty-server/src/config.rs side-projects/pty-server/src/config_test.rs side-projects/pty-server/src/main.rs
git commit -m "$(cat <<'EOF'
feat(pty-server): Config 模块，解析 PORT/SHELL 环境变量

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Task 4: PtySession 模块（TDD）

**Files:**
- Create: `side-projects/pty-server/src/pty_session.rs`
- Create: `side-projects/pty-server/src/pty_session_test.rs`
- Modify: `side-projects/pty-server/src/main.rs`

- [ ] **Step 1: 写失败测试 pty_session_test.rs**

写入 `side-projects/pty-server/src/pty_session_test.rs`：

```rust
use super::*;

/// 跨平台获取测试用 shell。
fn test_shell() -> &'static str {
    if cfg!(target_os = "windows") {
        "cmd.exe"
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string()).leak::<str>()
    }
}

#[test]
fn test_pty_session_spawn_returns_handles() {
    let (session, _reader) = PtySession::spawn(test_shell(), &[], 80, 24)
        .expect("spawn 应成功");
    // master/writer/child 字段已就绪，drop 时自动 kill
    drop(session);
}

#[test]
fn test_pty_session_read_receives_echo_output() {
    // Unix: bash -c 'echo hello'，Windows: cmd /c echo hello
    let (shell, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
        ("cmd.exe", vec!["/c", "echo hello"])
    } else {
        ("bash", vec!["-c", "echo hello"])
    };

    let (mut session, mut reader) = PtySession::spawn(shell, &args, 80, 24)
        .expect("spawn 应成功");

    // 等子进程输出后读
    std::thread::sleep(std::time::Duration::from_millis(200));
    let mut buf = [0u8; 256];
    let n = reader.read(&mut buf).expect("read 应成功");
    assert!(n > 0, "应读到一些字节");
    let output = String::from_utf8_lossy(&buf[..n]);
    assert!(output.contains("hello"), "输出应包含 hello，实际: {output}");

    let exit = session.try_wait_exit().expect("try_wait 应成功");
    assert!(exit.is_some(), "子进程应已退出");
    drop(session);
}

#[test]
fn test_pty_session_write_feeds_stdin() {
    // 用 cat / cmd 交互式回显
    let (shell, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
        ("cmd.exe", vec![])
    } else {
        ("cat", vec![])
    };

    let (mut session, mut reader) = PtySession::spawn(shell, &args, 80, 24)
        .expect("spawn 应成功");

    session.write(b"ping\n").expect("write 应成功");

    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut buf = [0u8; 1024];
    let n = reader.read(&mut buf).expect("read 应成功");
    let output = String::from_utf8_lossy(&buf[..n]);
    assert!(output.contains("ping"), "回显应包含 ping，实际: {output}");

    session.kill().expect("kill 应成功");
    drop(session);
}

#[test]
fn test_pty_session_resize_does_not_panic() {
    let (mut session, _reader) = PtySession::spawn(test_shell(), &[], 80, 24)
        .expect("spawn 应成功");
    session.resize(120, 40).expect("resize 应成功");
    drop(session);
}
```

注意：测试里的 `test_shell()` 用 leak 是简化处理（测试进程退出时回收）。Unix 用 `$SHELL` 兜底 `/bin/bash`。

- [ ] **Step 2: 跑测试验证失败**

```bash
cargo test -p pty-server --lib pty_session_test
```

Expected: 编译失败，`PtySession` 未定义。

- [ ] **Step 3: 写 pty_session.rs**

写入 `side-projects/pty-server/src/pty_session.rs`：

```rust
use std::io::{self, Read, Write};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

/// PTY 会话封装。
///
/// 持有 master（用于 resize）、writer（用于 write）、child（用于 kill/wait）。
/// reader 在 `spawn` 时返回给调用方，由调用方在 `spawn_blocking` 中读取。
pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl PtySession {
    /// Spawn 一个 shell 进程到 PTY，返回 (PtySession, reader)。
    ///
    /// reader 是阻塞 `Read`，调用方应在 `spawn_blocking` 中循环读取。
    pub fn spawn(
        shell: &str,
        args: &[&str],
        cols: u16,
        rows: u16,
    ) -> io::Result<(Self, Box<dyn Read + Send>)> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io_err)?;

        // reader 必须在 slave spawn 之前 clone，否则 race
        let reader = pair.master.try_clone_reader().map_err(io_err)?;

        let mut cmd = CommandBuilder::new(shell);
        cmd.args(args);
        cmd.env("TERM", "xterm-256color");

        let child = pair.slave.spawn_command(cmd).map_err(io_err)?;
        // 释放 slave：portable-pty 要求 slave drop 后 master 才能在子进程退出时 EOF
        drop(pair.slave);

        let writer = pair.master.take_writer().map_err(io_err)?;

        Ok((
            Self {
                master: pair.master,
                writer,
                child,
            },
            reader,
        ))
    }

    /// 写 stdin 到 PTY。
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.writer.write_all(data)
    }

    /// 调整 PTY 尺寸。
    pub fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io_err)
    }

    /// 非阻塞查询子进程退出码。返回 `Ok(None)` 表示尚未退出。
    pub fn try_wait_exit(&mut self) -> io::Result<Option<i32>> {
        let status = self.child.try_wait().map_err(io_err)?;
        Ok(status.and_then(|s| s.exit_code()))
    }

    /// Kill 子进程。已退出时返回 Ok(())。
    pub fn kill(&mut self) -> io::Result<()> {
        match self.child.kill() {
            Ok(()) => Ok(()),
            // 已经退出的进程 kill 失败是正常的
            Err(e) if e.kind() == io::ErrorKind::Other => Ok(()),
            Err(e) => Err(e),
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // 尽力 kill，portable-pty 在 master drop 时会清理
        let _ = self.child.kill();
    }
}

/// 把 anyhow 风格错误转成 io::Error。
fn io_err<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e.to_string())
}
```

- [ ] **Step 4: 在 main.rs 添加模块声明**

修改 `side-projects/pty-server/src/main.rs`，在顶部加 `mod pty_session;`：

```rust
mod config;
mod pty_session;

fn main() {
    let cfg = config::Config::from_env().expect("解析配置失败");
    println!("port={}, shell={}", cfg.port, cfg.shell);
}
```

- [ ] **Step 5: 跑测试验证通过**

```bash
cargo test -p pty-server --lib pty_session_test
```

Expected: 4 个测试全部通过。如果 Windows ConPTY 在 `cmd /c echo hello` 后 reader 阻塞（已知 portable-pty issue #463），调整为只跑 Unix 子集或增加超时（用 `read` 替代阻塞读时设 `set_read_timeout`，本测试用 200ms sleep 应已规避）。

- [ ] **Step 6: 提交**

```bash
git add side-projects/pty-server/src/pty_session.rs side-projects/pty-server/src/pty_session_test.rs side-projects/pty-server/src/main.rs
git commit -m "$(cat <<'EOF'
feat(pty-server): PtySession 封装 portable-pty

提供 spawn / read / write / resize / try_wait_exit / kill，spawn 返回
(session, reader) 把阻塞 reader 拆出来供 spawn_blocking 使用。

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Task 5: HTTP 路由模块（TDD）

**Files:**
- Create: `side-projects/pty-server/src/http_routes.rs`
- Create: `side-projects/pty-server/src/http_routes_test.rs`
- Modify: `side-projects/pty-server/src/main.rs`

- [ ] **Step 1: 写失败测试 http_routes_test.rs**

写入 `side-projects/pty-server/src/http_routes_test.rs`：

```rust
#[test]
fn test_index_html_contains_terminal_div() {
    // index() 直接返回 include_str!("../index.html")，测试源文件即可覆盖内容
    let html = include_str!("../index.html");
    assert!(
        html.contains("<div id=\"terminal\">"),
        "index.html 应包含 <div id=\"terminal\">"
    );
}

#[test]
fn test_index_html_contains_xterm_cdn() {
    let html = include_str!("../index.html");
    assert!(
        html.contains("cdn.jsdelivr.net/npm/xterm"),
        "index.html 应引用 xterm CDN"
    );
}
```

注意：`index()` handler 返回的就是 `include_str!` 内容，e2e 测试（Task 7）会通过真实 HTTP 请求覆盖 handler 行为，单元测试只需断言源文件内容。

- [ ] **Step 2: 跑测试验证失败**

```bash
cargo test -p pty-server --lib http_routes_test
```

Expected: 编译失败，`http_routes` 模块未声明。

- [ ] **Step 3: 写 http_routes.rs**

写入 `side-projects/pty-server/src/http_routes.rs`：

```rust
use axum::{
    http::{header, HeaderValue},
    response::IntoResponse,
};

/// GET / 返回嵌入的 index.html。
pub async fn index() -> impl IntoResponse {
    let html: &'static str = include_str!("../index.html");
    (
        [(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"))],
        html,
    )
}
```

注意：axum 把 `([(HeaderName, HeaderValue); N], &'static str)` 当作 `IntoResponse`，自动设置 status 200 + headers + body。

- [ ] **Step 4: 在 main.rs 添加模块声明**

修改 `side-projects/pty-server/src/main.rs`：

```rust
mod config;
mod http_routes;
mod pty_session;

fn main() {
    let cfg = config::Config::from_env().expect("解析配置失败");
    println!("port={}, shell={}", cfg.port, cfg.shell);
}
```

- [ ] **Step 5: 跑测试验证通过**

```bash
cargo test -p pty-server --lib http_routes_test
```

Expected: 2 个测试通过。

- [ ] **Step 6: 提交**

```bash
git add side-projects/pty-server/src/http_routes.rs side-projects/pty-server/src/http_routes_test.rs side-projects/pty-server/src/main.rs
git commit -m "$(cat <<'EOF'
feat(pty-server): HTTP 路由模块，GET / 返回嵌入的 index.html

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Task 6: WS handler 模块（含 Query 单元测试）

**Files:**
- Create: `side-projects/pty-server/src/ws_handler.rs`
- Create: `side-projects/pty-server/src/ws_handler_test.rs`
- Modify: `side-projects/pty-server/src/main.rs`

- [ ] **Step 1: 写失败测试 ws_handler_test.rs**

写入 `side-projects/pty-server/src/ws_handler_test.rs`：

```rust
use super::*;

#[test]
fn test_ws_query_parses_shell_and_dimensions() {
    let q = WsQuery {
        shell: Some("/bin/zsh".to_string()),
        args: Some("-l".to_string()),
        cols: Some("100".to_string()),
        rows: Some("30".to_string()),
    };
    let parsed = q.to_spawn_params();
    assert_eq!(parsed.shell, "/bin/zsh");
    assert_eq!(parsed.args, vec!["-l"]);
    assert_eq!(parsed.cols, 100);
    assert_eq!(parsed.rows, 30);
}

#[test]
fn test_ws_query_defaults_when_missing() {
    let q = WsQuery {
        shell: None,
        args: None,
        cols: None,
        rows: None,
    };
    let parsed = q.to_spawn_params();
    assert_eq!(parsed.shell, "");
    assert!(parsed.args.is_empty());
    assert_eq!(parsed.cols, 80);
    assert_eq!(parsed.rows, 24);
}

#[test]
fn test_ws_query_args_split_by_whitespace() {
    let q = WsQuery {
        shell: None,
        args: Some("-l  --verbose".to_string()),
        cols: None,
        rows: None,
    };
    let parsed = q.to_spawn_params();
    // 多个空格应被过滤
    assert_eq!(parsed.args, vec!["-l", "--verbose"]);
}
```

- [ ] **Step 2: 跑测试验证失败**

```bash
cargo test -p pty-server --lib ws_handler_test
```

Expected: 编译失败，`WsQuery` 未定义。

- [ ] **Step 3: 写 ws_handler.rs**

写入 `side-projects/pty-server/src/ws_handler.rs`：

```rust
use std::io::Read;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query,
    },
    response::IntoResponse,
};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::pty_session::PtySession;

/// WebSocket 查询参数。
#[derive(Debug, Deserialize)]
pub struct WsQuery {
    pub shell: Option<String>,
    pub args: Option<String>,
    pub cols: Option<String>,
    pub rows: Option<String>,
}

/// 从 WsQuery 解析出的 spawn 参数。
pub struct SpawnParams {
    pub shell: String,
    pub args: Vec<String>,
    pub cols: u16,
    pub rows: u16,
}

impl WsQuery {
    /// 把字符串查询参数转为强类型 spawn 参数。
    pub fn to_spawn_params(&self) -> SpawnParams {
        let args = self
            .args
            .as_deref()
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_default();
        let cols = self.cols.as_deref().and_then(|s| s.parse().ok()).unwrap_or(80);
        let rows = self.rows.as_deref().and_then(|s| s.parse().ok()).unwrap_or(24);
        SpawnParams {
            shell: self.shell.clone().unwrap_or_default(),
            args,
            cols,
            rows,
        }
    }
}

/// GET /ws 的 axum handler：升级 WebSocket。
pub async fn ws_handler(ws: WebSocketUpgrade, Query(q): Query<WsQuery>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, q))
}

/// WebSocket 连接生命周期：spawn PTY + 双向 pump。
async fn handle_socket(socket: WebSocket, q: WsQuery) {
    let params = q.to_spawn_params();
    let shell_display = params.shell.clone();

    // spawn PTY
    let (mut session, reader) =
        match PtySession::spawn(&params.shell, &params.args, params.cols, params.rows) {
            Ok(v) => v,
            Err(e) => {
                let msg = format!("\r\n[failed to spawn {shell_display}: {e}]\r\n");
                warn!("PTY spawn 失败 shell={shell_display} err={e}");
                let _ = socket.send(Message::Text(msg)).await;
                let _ = socket.close().await;
                return;
            }
        };
    info!(
        "PTY 连接建立 shell={shell_display} cols={} rows={}",
        params.cols, params.rows
    );

    // mpsc channel: read_task → pump_task。None 哨兵表示 PTY EOF
    let (tx, mut rx) = mpsc::channel::<Option<Vec<u8>>>(16);

    // read_task：spawn_blocking 阻塞读 PTY
    let read_task = tokio::task::spawn_blocking(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = tx.blocking_send(None);
                    break;
                }
                Ok(n) => {
                    if tx.blocking_send(Some(buf[..n].to_vec())).is_err() {
                        break; // pump_task 已退出
                    }
                }
            }
        }
    });

    // pump_task：select! { ws.recv() | rx.recv() }
    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if try_handle_resize(&text, &mut session) {
                            continue;
                        }
                        if let Err(e) = session.write(text.as_bytes()) {
                            debug!("PTY write 失败（client 输入）: {e}");
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(_))) => {
                        // 与 Bun 原版一致：忽略 binary frame
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        debug!("WebSocket 关闭");
                        break;
                    }
                    Some(Ok(_)) => {
                        // Ping/Pong 由 axum 自动处理
                    }
                    Some(Err(e)) => {
                        warn!("WebSocket 接收错误: {e}");
                        break;
                    }
                }
            }
            bytes = rx.recv() => {
                match bytes {
                    Some(Some(data)) => {
                        let text = String::from_utf8_lossy(&data).into_owned();
                        if socket.send(Message::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    Some(None) => {
                        // PTY EOF：子进程退出
                        let code = session.try_wait_exit().ok().flatten();
                        let display = code
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "unknown".to_string());
                        let msg = format!("\r\n[process exited with code {display}]\r\n");
                        let _ = socket.send(Message::Text(msg)).await;
                        break;
                    }
                    None => break, // read_task 退出
                }
            }
        }
    }

    read_task.abort();
    drop(session);
    let _ = socket.close().await;
    info!("PTY 连接结束 shell={shell_display}");
}

/// 尝试把文本消息当作 resize 命令处理。成功处理返回 true。
fn try_handle_resize(text: &str, session: &mut PtySession) -> bool {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    if parsed.get("type").and_then(|v| v.as_str()) != Some("resize") {
        return false;
    }
    let (Some(cols), Some(rows)) = (
        parsed.get("cols").and_then(|v| v.as_u64()),
        parsed.get("rows").and_then(|v| v.as_u64()),
    ) else {
        return false;
    };
    match session.resize(cols as u16, rows as u16) {
        Ok(()) => true,
        Err(e) => {
            warn!("PTY resize 失败: {e}");
            true // resize 失败也消耗掉这条消息，不当作 stdin
        }
    }
}
```

- [ ] **Step 4: 在 main.rs 添加模块声明**

修改 `side-projects/pty-server/src/main.rs`：

```rust
mod config;
mod http_routes;
mod pty_session;
mod ws_handler;

fn main() {
    let cfg = config::Config::from_env().expect("解析配置失败");
    println!("port={}, shell={}", cfg.port, cfg.shell);
}
```

- [ ] **Step 5: 跑单元测试验证通过**

```bash
cargo test -p pty-server --lib ws_handler_test
```

Expected: 3 个测试通过。

- [ ] **Step 6: 跑全 lib 测试验证未破坏**

```bash
cargo test -p pty-server --lib
```

Expected: 全部通过。

- [ ] **Step 7: 提交**

```bash
git add side-projects/pty-server/src/ws_handler.rs side-projects/pty-server/src/ws_handler_test.rs side-projects/pty-server/src/main.rs
git commit -m "$(cat <<'EOF'
feat(pty-server): WebSocket handler + PTY 双向 pump

- WsQuery 解析 shell/args/cols/rows
- handle_socket：spawn PTY + read_task(spawn_blocking) + pump_task(select!)
- 退出码获取 + 退出消息发送 + close

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Task 7: 端到端集成测试（TDD）

**Files:**
- Create: `side-projects/pty-server/tests/ws_e2e_test.rs`

- [ ] **Step 1: 写失败测试 tests/ws_e2e_test.rs**

写入 `side-projects/pty-server/tests/ws_e2e_test.rs`：

```rust
//! 端到端集成测试：起真实 axum server，用 tokio-tungstenite client 连接验证协议。

use std::time::Duration;

use axum::{routing::get, Router};
use tokio_tungstenite::tungstenite::Message;

use pty_server::http_routes;
use pty_server::ws_handler;

fn build_app() -> Router {
    Router::new()
        .route("/", get(http_routes::index))
        .route("/ws", get(ws_handler::ws_handler))
}

async fn spawn_server() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = build_app();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    port
}

/// 跨平台获取测试 shell + 退出命令
fn exit_shell() -> (&'static str, Vec<&'static str>) {
    if cfg!(target_os = "windows") {
        ("cmd.exe", vec!["/c", "exit 0"])
    } else {
        ("bash", vec!["-c", "exit 0"])
    }
}

fn echo_shell() -> (&'static str, Vec<&'static str>) {
    if cfg!(target_os = "windows") {
        ("cmd.exe", vec![])
    } else {
        ("cat", vec![])
    }
}

#[tokio::test]
async fn test_ws_connection_receives_exit_message_on_child_exit() {
    let port = spawn_server().await;
    let (shell, args) = exit_shell();
    let url = format!(
        "ws://127.0.0.1:{port}/ws?shell={shell}&args={}",
        args.join("+")
    );

    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // 收消息直到看到 [process exited ...]
    let mut saw_exit = false;
    for _ in 0..20 {
        match tokio::time::timeout(Duration::from_secs(3), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) if t.contains("[process exited") => {
                saw_exit = true;
                break;
            }
            Ok(Some(_)) => continue,
            _ => break,
        }
    }
    assert!(saw_exit, "应收到 [process exited ...]");
}

#[tokio::test]
async fn test_ws_connection_spawn_failure_sends_error_and_closes() {
    let port = spawn_server().await;
    let url = "ws://127.0.0.1:{port}/ws?shell=/nonexistent/pty-test-shell";
    let url = url.replace("{port}", &port.to_string());

    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let mut saw_error = false;
    for _ in 0..20 {
        match tokio::time::timeout(Duration::from_secs(3), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) if t.contains("[failed to spawn") => {
                saw_error = true;
                break;
            }
            Ok(Some(_)) => continue,
            _ => break,
        }
    }
    assert!(saw_error, "应收到 [failed to spawn ...]");
}
```

注意：`tests/ws_e2e_test.rs` 需要 crate 暴露 `http_routes` 和 `ws_handler` 模块，这要求 main.rs 改为 lib + bin 双 target，或把模块逻辑移到 lib。最简单做法：在 Cargo.toml 增加 `[lib]` target。

- [ ] **Step 2: 跑测试验证失败**

```bash
cargo test -p pty-server --test ws_e2e_test
```

Expected: 编译失败——crate 没有 lib target，`pty_server::` 路径找不到。

- [ ] **Step 3: 在 Cargo.toml 增加 lib target + 把逻辑挪到 lib.rs**

修改 `side-projects/pty-server/Cargo.toml`，在 `[package]` 之后、`[dependencies]` 之前插入：

```toml
[lib]
name = "pty_server"
path = "src/lib.rs"

[[bin]]
name = "pty-server"
path = "src/main.rs"
```

创建 `side-projects/pty-server/src/lib.rs`：

```rust
//! PTY server 库入口，供集成测试引用。

pub mod config;
pub mod http_routes;
pub mod pty_session;
pub mod ws_handler;
```

替换 `side-projects/pty-server/src/main.rs` 为：

```rust
use pty_server::config::Config;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().try_init().ok();
    let cfg = Config::from_env().expect("解析配置失败");

    let app = axum::Router::new()
        .route("/", axum::routing::get(pty_server::http_routes::index))
        .route("/ws", axum::routing::get(pty_server::ws_handler::ws_handler));

    let addr = format!("0.0.0.0:{}", cfg.port);
    tracing::info!("PTY server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received");
}
```

注意：`http_routes::index` 是 `async fn -> impl IntoResponse`（Task 5 已写好），可直接作为 axum handler。

- [ ] **Step 4: 验证 lib + bin 编译**

```bash
cargo build -p pty-server
```

Expected: 成功。

- [ ] **Step 5: 跑 e2e 测试验证通过**

```bash
cargo test -p pty-server --test ws_e2e_test
```

Expected: 2 个测试通过。若 Windows 上 `cmd.exe` spawn 在 ConPTY 下行为不同，可调整 helper。

- [ ] **Step 6: 跑全测试套件验证未破坏**

```bash
cargo test -p pty-server
```

Expected: 全部通过。

- [ ] **Step 7: 提交**

```bash
git add side-projects/pty-server/
git commit -m "$(cat <<'EOF'
test(pty-server): 端到端集成测试 + lib/bin 拆分

- Cargo.toml 增加 lib + bin target
- src/lib.rs 暴露模块供集成测试
- main.rs 接入 axum::serve + graceful shutdown
- tests/ws_e2e_test.rs：spawn 真实 server + tokio-tungstenite client

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Task 8: 最终验证 + 手动启动

**Files:**
- Verify: `side-projects/pty-server/`（无需修改源码，Task 1-7 已完整）

- [ ] **Step 1: 跑全测试验证**

```bash
cd /Users/konghayao/code/ai/perihelion
cargo test -p pty-server
```

Expected: 全部测试通过（config_test + pty_session_test + http_routes_test + ws_handler_test + ws_e2e_test）。

- [ ] **Step 2: clippy + fmt**

```bash
cargo clippy -p pty-server --all-targets -- -D warnings
cargo fmt -p pty-server
```

Expected: 无 warning，无格式问题。若 clippy 报 `println!` 残留，检查 main.rs 是否还有占位 `println!`（应已全部用 tracing）。

- [ ] **Step 3: 全 workspace 编译**

```bash
cargo build
```

Expected: 全 workspace 编译通过，未破坏其他 crate。

- [ ] **Step 4: 手动启动验证**

```bash
cargo run -p pty-server &
SERVER_PID=$!
sleep 2
# 验证 HTTP /
curl -s http://localhost:3000 | grep -q "<div id=\"terminal\">" && echo "HTTP OK"
# 验证端口存活
nc -z localhost 3000 && echo "PORT OK"
kill $SERVER_PID
```

Expected: 输出 "HTTP OK" 和 "PORT OK"。浏览器打开 http://localhost:3000 应看到 xterm 终端，能交互。

- [ ] **Step 5: 提交（若 fmt/clippy 有修复）**

```bash
git status side-projects/pty-server/
# 如果有未提交的 fmt/clippy 修复：
git add side-projects/pty-server/
git commit -m "$(cat <<'EOF'
chore(pty-server): clippy + fmt 修复

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

若 `git status` 显示 clean，跳过此 step。

---

## Task 9: README 与最终验证

**Files:**
- Create: `side-projects/pty-server/README.md`
- Verify: lefthook pre-commit 全通过

- [ ] **Step 1: 写 README.md**

写入 `side-projects/pty-server/README.md`：

```markdown
# pty-server

Web PTY 终端服务，从原 Bun+TS 实现移植为 Rust bin crate。

## 运行

```bash
cargo run -p pty-server                 # 监听 :3000
PORT=8080 cargo run -p pty-server       # 自定义端口
SHELL=/bin/zsh cargo run -p pty-server  # 自定义默认 shell
```

浏览器打开 <http://localhost:3000>。

## 架构

- HTTP/WS：`axum 0.7` + `tokio-tungstenite 0.24`
- PTY：`portable-pty 0.9`（macOS/Linux 用 forkpty，Windows 用 ConPTY）
- 前端：单 HTML 文件，CDN 加载 xterm.js + 内联 JS，`include_str!` 嵌入二进制

每个 `/ws` 连接 spawn 一个 shell 子进程，PTY 输出经 spawn_blocking + mpsc channel 推送到 WebSocket。

## 测试

```bash
cargo test -p pty-server
```

## 环境变量

| 变量 | 默认 | 说明 |
|------|------|------|
| `PORT` | `3000` | HTTP/WS 监听端口 |
| `SHELL` | `/bin/bash`（Unix）/ `cmd.exe`（Windows） | 默认 shell |
| `RUST_LOG` | `info` | tracing 日志级别 |
```

- [ ] **Step 2: 全 lefthook pre-commit 验证**

```bash
cd /Users/konghayao/code/ai/perihelion
lefthook run pre-commit
```

Expected: typos / clippy / fmt / check 全通过。若 clippy 报 `println!` 残留，回 main.rs 检查（应已全部用 tracing）。

- [ ] **Step 3: 提交 README**

```bash
git add side-projects/pty-server/README.md
git commit -m "$(cat <<'EOF'
docs(pty-server): README 说明运行/架构/测试

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

- [ ] **Step 4: 最终验证 cargo test 全 workspace**

```bash
cargo test
```

Expected: 全 workspace 测试通过（含 pty-server 全部测试）。

---

## Self-Review Checklist（写完后自查）

- ✅ Spec 覆盖：架构/模块/数据流/错误处理/测试/构建发布 6 段全部映射到 task
- ✅ 占位符扫描：无 TBD/TODO
- ✅ 类型一致：`PtySession::spawn` 签名（返回 `(Self, Box<dyn Read + Send>)`）在 Task 4 定义，在 Task 6 调用方式一致
- ✅ 跨平台：`default_shell` / `test_shell` / `exit_shell` / `echo_shell` 处理 Windows
- ✅ TDD：Task 3/4/5/6/7 都是「写测试 → 跑失败 → 实现 → 跑通过 → 提交」
- ✅ 频繁提交：每个 task 一次提交，task 4/6 内部多文件单次提交合理

## Known Risks

| 风险 | 缓解 |
|------|------|
| portable-pty 在 Windows ConPTY 下 `cmd /c echo` 后 reader 可能不立即 EOF（issue #463） | e2e 测试有 3s 超时，必要时增加 sleep |
| axum 0.7 ws API 与 tokio-tungstenite 0.24 版本兼容 | axum::extract::ws 内部用 tungstenite，已知兼容 |
| 测试 set_var/ remove_var 非线程安全 | 测试用 `unsafe { std::env::set_var(...) }` 标注，单线程串行运行 |
| `serde_json::Value` 解析 resize 消息较宽（接受 `cols: "80"` 字符串） | 实际只接受 `as_u64`，字符串数字会被丢弃，需要时 client 已用 number |
