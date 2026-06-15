# peri-web-pty Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 `side-projects/pty-server` 升级为顶层 crate `peri-web-pty`，并为 `peri-tui` 新增 `peri web` 子命令。

**Architecture:** `peri-web-pty` 提供库入口 `start_server(config)`，`peri-tui` 的 `Commands::Web` 直接调用它。随机端口 + 自动打开浏览器。

**Tech Stack:** Rust 2021, axum 0.7, portable-pty 0.9, tokio, clap 4

---

### Task 1: 创建 peri-web-pty 目录结构和文件

**Files:**
- Create: `peri-web-pty/Cargo.toml`
- Create: `peri-web-pty/src/lib.rs`
- Create: `peri-web-pty/src/main.rs`
- Create: `peri-web-pty/src/config.rs`
- Create: `peri-web-pty/src/session_state.rs`
- Create: `peri-web-pty/src/pty_session.rs`
- Create: `peri-web-pty/src/ws_handler.rs`
- Create: `peri-web-pty/src/http_routes.rs`
- Create: `peri-web-pty/src/config_test.rs`
- Create: `peri-web-pty/src/http_routes_test.rs`
- Create: `peri-web-pty/src/pty_session_test.rs`
- Create: `peri-web-pty/src/ws_handler_test.rs`
- Create: `peri-web-pty/index.html`
- Create: `peri-web-pty/README.md`
- Create: `peri-web-pty/tests/ws_e2e_test.rs`

- [ ] **Step 1: 创建目录结构并复制文件**

```bash
mkdir -p peri-web-pty/src peri-web-pty/tests
cp side-projects/pty-server/Cargo.toml peri-web-pty/
cp side-projects/pty-server/index.html peri-web-pty/
cp side-projects/pty-server/README.md peri-web-pty/
cp side-projects/pty-server/src/config.rs peri-web-pty/src/
cp side-projects/pty-server/src/session_state.rs peri-web-pty/src/
cp side-projects/pty-server/src/pty_session.rs peri-web-pty/src/
cp side-projects/pty-server/src/ws_handler.rs peri-web-pty/src/
cp side-projects/pty-server/src/http_routes.rs peri-web-pty/src/
cp side-projects/pty-server/src/config_test.rs peri-web-pty/src/
cp side-projects/pty-server/src/http_routes_test.rs peri-web-pty/src/
cp side-projects/pty-server/src/pty_session_test.rs peri-web-pty/src/
cp side-projects/pty-server/src/ws_handler_test.rs peri-web-pty/src/
cp side-projects/pty-server/tests/ws_e2e_test.rs peri-web-pty/tests/
```

- [ ] **Step 2: 验证文件就位**

```bash
ls -la peri-web-pty/src/ peri-web-pty/tests/
```

期望：14 个文件全部存在。

- [ ] **Step 3: Commit**

```bash
git add peri-web-pty/
git commit -m "chore: copy pty-server to peri-web-pty/"
```

---

### Task 2: 修改 peri-web-pty Cargo.toml（命名 + 依赖）

**Files:**
- Modify: `peri-web-pty/Cargo.toml`

- [ ] **Step 1: 改 package name 和 lib name，加 anyhow 依赖**

编辑 `peri-web-pty/Cargo.toml`，将内容替换为：

```toml
[package]
name = "peri-web-pty"
version.workspace = true
edition.workspace = true

[lib]
name = "peri_web_pty"
path = "src/lib.rs"

[[bin]]
name = "peri-web-pty"
path = "src/main.rs"

[dependencies]
tokio = { workspace = true }
futures = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
axum = { workspace = true }
tokio-tungstenite = { workspace = true }
portable-pty = { workspace = true }
clap = { workspace = true }
anyhow = { workspace = true }

[dev-dependencies]
tokio-tungstenite = { workspace = true }
serial_test = { workspace = true }
```

原名为 `pty-server`/`pty_server`，改为 `peri-web-pty`/`peri_web_pty`。新增 `anyhow` 供 `start_server` 使用。

- [ ] **Step 2: Commit**

```bash
git add peri-web-pty/Cargo.toml
git commit -m "chore(peri-web-pty): rename package, add anyhow dep"
```

---

### Task 3: 修改 peri-web-pty/src/lib.rs（抽取 start_server）

**Files:**
- Modify: `peri-web-pty/src/lib.rs`

- [ ] **Step 1: 重写 lib.rs，添加 start_server 和 shutdown_signal、open_browser**

将 `peri-web-pty/src/lib.rs` 替换为：

```rust
//! Web PTY 终端服务库入口。

use anyhow::Context;
use axum::Router;
use config::Config;
use session_state::SessionState;

pub mod config;
pub mod http_routes;
pub mod pty_session;
pub mod session_state;
pub mod ws_handler;

#[cfg(test)]
mod config_test;
#[cfg(test)]
mod http_routes_test;
#[cfg(test)]
mod pty_session_test;
#[cfg(test)]
mod ws_handler_test;

// 供 `#[cfg(test)] mod xxx_test` 中 `use super::*` 找到顶层类型
#[cfg(test)]
use config::Config;
#[cfg(test)]
use pty_session::PtySession;
#[cfg(test)]
use ws_handler::WsQuery;

/// 启动 Web PTY 终端服务。
pub async fn start_server(config: Config) -> anyhow::Result<()> {
    let cwd = config.cwd.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });
    let state = SessionState::new(Some(cwd), config.initial_cmd.clone());

    let app = Router::new()
        .route("/", axum::routing::get(http_routes::index))
        .route("/ws", axum::routing::get(ws_handler::ws_handler))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", config.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .context("failed to bind TCP listener")?;
    let actual_port = listener.local_addr()?.port();
    let url = format!("http://localhost:{}", actual_port);

    tracing::info!("Web PTY server: {}", url);
    println!("Web PTY server: {}", url);

    // 尝试自动打开浏览器
    open_browser(&url);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    Ok(())
}

/// 尝试用系统默认浏览器打开 URL。失败时静默跳过。
fn open_browser(url: &str) {
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(target_os = "linux") {
        std::process::Command::new("xdg-open").arg(url).spawn()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", url])
            .spawn()
    } else {
        return;
    };

    match result {
        Ok(_) => tracing::info!("browser opened: {}", url),
        Err(e) => tracing::warn!("failed to open browser: {e}"),
    }
}

/// 优雅关闭信号监听。
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl+C handler");
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

改动：
- 新增 `use anyhow::Context`
- 原 `main()` 逻辑抽出为 `pub async fn start_server(config) -> anyhow::Result<()>`
- `Router::new()` 的 route 从 `pty_server::http_routes::index` 改为 `http_routes::index`（同 crate 内直接用 `crate::` 路径的简化写法）
- 新增随机端口逻辑：`listener.local_addr()?.port()`
- 新增 `open_browser()` 辅助函数，跨平台静默 fallback
- `shutdown_signal()` 从 `main.rs` 移入 `lib.rs`

- [ ] **Step 2: Commit**

```bash
git add peri-web-pty/src/lib.rs
git commit -m "feat(peri-web-pty): extract start_server with random port + browser open"
```

---

### Task 4: 修改 peri-web-pty/src/main.rs（精简为调用 start_server）

**Files:**
- Modify: `peri-web-pty/src/main.rs`

- [ ] **Step 1: 重写 main.rs**

替换为：

```rust
use peri_web_pty::config::Config;
use peri_web_pty::start_server;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().try_init().ok();
    start_server(Config::from_args()).await
}
```

改动：
- 移除 `SessionState` import（start_server 内部处理）
- 移除 `shutdown_signal` 函数（移到 lib.rs）
- 移除 `Router` 构建逻辑（移到 lib.rs）
- 直接从库调用 `start_server`

- [ ] **Step 2: Commit**

```bash
git add peri-web-pty/src/main.rs
git commit -m "refactor(peri-web-pty): main.rs delegates to start_server"
```

---

### Task 5: 修改 peri-web-pty/src/config.rs（端口默认值改为 0）

**Files:**
- Modify: `peri-web-pty/src/config.rs`

- [ ] **Step 1: 改 port 默认值和 command name**

```diff
-    #[arg(long, env = "PORT", default_value_t = 3000)]
+    #[arg(long, env = "PORT", default_value_t = 0)]
     pub port: u16,
```

以及：

```diff
- #[command(name = "pty-server", about = "Web PTY terminal server")]
+ #[command(name = "peri-web-pty", about = "Web PTY terminal server")]
```

- [ ] **Step 2: Commit**

```bash
git add peri-web-pty/src/config.rs
git commit -m "feat(peri-web-pty): random port by default (0)"
```

---

### Task 6: 更新 workspace Cargo.toml members

**Files:**
- Modify: `Cargo.toml`（根）

- [ ] **Step 1: 替换 members 条目**

```diff
     "side-projects/git-graph",
-    "side-projects/pty-server",
+    "peri-web-pty",
     "agm",
```

- [ ] **Step 2: 验证 workspace 解析正确**

```bash
cargo metadata --no-deps --format-version 1 | jq '.workspace_members[]' | grep peri-web-pty
```

期望输出包含 `peri-web-pty`。

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "chore(workspace): replace pty-server with peri-web-pty"
```

---

### Task 7: peri-tui 新增 Web 子命令

**Files:**
- Modify: `peri-tui/src/main.rs`

- [ ] **Step 1: 在 Commands enum 末尾添加 Web 变体**

在 `Plugin { ... }` 之后、`}` 之前插入：

```rust
    /// 启动 Web PTY 终端服务
    Web {
        /// 监听端口（默认 0 = 随机分配）
        #[arg(long)]
        port: Option<u16>,
        /// 工作目录
        #[arg(long)]
        cwd: Option<String>,
        /// 启动后自动注入的命令
        #[arg(long)]
        cmd: Option<String>,
    },
```

- [ ] **Step 2: 在主函数 match 块中添加 Web 分支**

在 `Some(Commands::Plugin { action }) => { ... }` 之后、`}` 之前插入：

```rust
        Some(Commands::Web { port, cwd, cmd }) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(4 * 1024 * 1024)
                .enable_all()
                .build()?;
            rt.block_on(async {
                let config = peri_web_pty::config::Config {
                    port: port.unwrap_or(0),
                    shell: None,
                    cwd,
                    initial_cmd: cmd,
                    default_cols: 80,
                    default_rows: 24,
                };
                peri_web_pty::start_server(config).await
            })
            .map_err(|e| {
                eprintln!("Web PTY server error: {e:#}");
                std::process::exit(1);
            })
        }
```

- [ ] **Step 3: 验证 CLI 解析工作**

```bash
cargo build -p peri-tui 2>&1 | head -20
```

期望：编译通过，无语法错误。

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/main.rs
git commit -m "feat(peri-tui): add peri web subcommand"
```

---

### Task 8: peri-tui Cargo.toml 添加 peri-web-pty 依赖

**Files:**
- Modify: `peri-tui/Cargo.toml`

- [ ] **Step 1: 在 [dependencies] 中添加**

在 `peri-acp = { path = "../peri-acp" }` 之后添加一行：

```toml
peri-web-pty = { path = "../peri-web-pty" }
```

- [ ] **Step 2: Commit**

```bash
git add peri-tui/Cargo.toml
git commit -m "chore(peri-tui): add peri-web-pty dependency"
```

---

### Task 9: 删除旧 pty-server 目录

**Files:**
- Delete: `side-projects/pty-server/`（整个目录）

- [ ] **Step 1: 删除**

```bash
rm -rf side-projects/pty-server
```

- [ ] **Step 2: Commit**

```bash
git add side-projects/pty-server/
git commit -m "chore: remove old side-projects/pty-server"
```

---

### Task 10: 编译验证 + 二进制体积对比

- [ ] **Step 1: 记录改前 peri 二进制体积（如果已存在）**

```bash
ls -lh target/release/peri 2>/dev/null || echo "no previous release build"
```

- [ ] **Step 2: 编译 peri-web-pty**

```bash
cargo build -p peri-web-pty 2>&1
```

期望：编译成功。

- [ ] **Step 3: 运行 peri-web-pty 测试**

```bash
cargo test -p peri-web-pty 2>&1
```

期望：全部通过。

- [ ] **Step 4: 编译 peri-tui debug**

```bash
cargo build -p peri-tui 2>&1
```

期望：编译成功。

- [ ] **Step 5: 运行 peri-tui 测试**

```bash
cargo test -p peri-tui 2>&1
```

期望：全部通过。

- [ ] **Step 6: 编译 peri-tui release + 对比体积**

```bash
cargo build -p peri-tui --release 2>&1
ls -lh target/release/peri
```

- [ ] **Step 7: 验证 peri web --help 输出**

```bash
cargo run -p peri-tui -- web --help
```

期望：打印 `peri web` 的帮助信息，包含 `--port`、`--cwd`、`--cmd` 参数。

- [ ] **Step 8: 最终 commit**

```bash
# 如无其他修改则跳过
```
