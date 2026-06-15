# peri-web-pty: Web PTY 终端升级设计

**日期**: 2026-06-15
**状态**: 已批准

## 目标

将 `side-projects/pty-server` 升级为独立顶层 crate `peri-web-pty`，并为 `peri-tui` 新增 `peri web` 子命令，实现一键启动 Web PTY 终端服务。

## 动机

- 统一 crate 命名规范：所有 Peri 核心功能以 `peri-` 前缀命名
- 统一入口：用户不需要知道 `pty-server` 这个独立命令，`peri web` 一站式搞定
- 体验提升：自动随机端口 + 自动打开浏览器

## 设计

### 1. 新 Crate: `peri-web-pty/`

- **目录位置**：`peri-web-pty/`（顶层，与 `peri-agent/` 同级）
- **Package name**: `peri-web-pty`
- **Lib name**: `peri_web_pty`
- **来源**：`side-projects/pty-server/` 全部代码迁移，改名

**文件结构**（与原名一致）：

```
peri-web-pty/
├── Cargo.toml
├── src/
│   ├── lib.rs            # pub mod + pub async fn start_server(config)
│   ├── main.rs           # 独立运行入口
│   ├── config.rs
│   ├── session_state.rs
│   ├── pty_session.rs
│   ├── ws_handler.rs
│   ├── http_routes.rs
│   └── *_test.rs
├── index.html
├── README.md
└── tests/
    └── ws_e2e_test.rs
```

**库入口**：

```rust
pub async fn start_server(config: Config) -> anyhow::Result<()>
```

- 复用原有 `main()` 的服务启动逻辑
- `main.rs` 保持独立，可单独运行 `cargo run -p peri-web-pty`

### 2. 随机端口 + 自动打开浏览器

**端口**：

- `Config.port` 默认值从 `3000` 改为 `0`（OS 自动分配）
- 启动后从 `TcpListener::local_addr()` 获取实际端口
- 终端打印 `Web PTY server: http://localhost:{port}`

**浏览器打开**：

- macOS: `open http://localhost:{port}`
- Linux: `xdg-open http://localhost:{port}` 或 fallback 打印 URL
- Windows: `start http://localhost:{port}` 或 fallback 打印 URL
- 命令执行失败时静默跳过（仅打印 URL）

### 3. peri-tui 新增 `Web` 子命令

```rust
enum Commands {
    // ... existing ...
    /// 启动 Web PTY 终端服务
    Web {
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        cmd: Option<String>,
    },
}
```

**执行路径**：`Web` 分支直接 `peri_web_pty::start_server(config).await`。

**依赖**：`peri-tui/Cargo.toml` 新增 `peri-web-pty = { path = "../peri-web-pty" }`。

### 4. Workspace 更新

`Cargo.toml` 的 `[workspace].members`：

```diff
- "side-projects/pty-server",
+ "peri-web-pty",
```

### 5. 旧目录清理

删除 `side-projects/pty-server/`。

## 非目标

- 不改变 PTY 核心逻辑（pty_session, ws_handler 等）
- 不改变前端 xterm.js 界面
- 不改变 index.html 嵌入方式（`include_str!`）
- 不在 TUI 内新增终端面板
- 不做 CLI 参数变更（仅 `port` 默认值变化）

## 实现步骤

| # | 步骤 | 内容 |
|---|------|------|
| 1 | 创建 `peri-web-pty/` | 从 `side-projects/pty-server/` 复制全部文件，改名 |
| 2 | 改 Cargo.toml 命名 | package.name → `peri-web-pty`，lib.name → `peri_web_pty` |
| 3 | 抽取 `start_server` | 原有 `main()` 逻辑抽取为 `pub async fn start_server(config)` |
| 4 | 随机端口 | `Config.port` 默认 `0`，从 listener 取实际端口 |
| 5 | 自动打开浏览器 | `open`/`xdg-open`/`start` fallback |
| 6 | 更新 workspace Cargo.toml | members 替换 |
| 7 | peri-tui 加 `Web` 子命令 | `Commands::Web { port, cwd, cmd }` |
| 8 | peri-tui Cargo.toml 加依赖 | 加 `peri-web-pty` |
| 9 | 删除 `side-projects/pty-server/` | |
| 10 | 编译验证 + 二进制体积对比 | 对比 `peri` 二进制体积变化 |

## 验证

- `cargo build -p peri-web-pty` 编译通过
- `cargo test -p peri-web-pty` 全部通过
- `cargo build -p peri-tui --release` 编译通过
- `cargo test -p peri-tui` 全部通过
- `ls -lh target/release/peri` 体积在可接受范围
