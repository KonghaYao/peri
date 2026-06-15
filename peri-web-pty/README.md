# pty-server

Web PTY 终端服务，从原 Bun+TS 实现移植为 Rust bin crate。

## 运行

```bash
cargo run -p pty-server                          # 监听 :3000
cargo run -p pty-server -- --port 8080            # 自定义端口
cargo run -p pty-server -- --cwd /path/to/proj   # 指定工作目录
cargo run -p pty-server -- --cmd "npm run dev"    # 第一个终端自动注入命令
cargo run -p pty-server -- --shell /bin/zsh       # 自定义默认 shell
```

所有参数同时支持环境变量：`PORT`、`CWD`、`CMD`、`SHELL`。

```bash
CWD=/path/to/proj CMD="npm run dev" cargo run -p pty-server
```

浏览器打开 <http://localhost:3000>。

## 功能

**多终端分屏**：顶部工具栏支持新建终端、水平/垂直分屏、自适应网格、恢复单列。每个终端对应独立的 WebSocket + PTY 会话。

**启动命令注入**：`--cmd` 或 `CMD` 指定的命令仅在第一个终端连接时自动执行，后续新终端不受影响。

**工作目录**：`--cwd` 或 `CWD` 指定的目录会应用于所有 PTY 会话。

## 架构

- HTTP/WS：`axum 0.7` + `tokio-tungstenite 0.24`
- PTY：`portable-pty 0.9`（macOS/Linux 用 forkpty，Windows 用 ConPTY）
- CLI：`clap 4` derive + env fallback
- 前端：单 HTML 文件，CDN 加载 xterm.js + 内联 JS，`include_str!` 嵌入二进制

每个 `/ws` 连接 spawn 一个 shell 子进程，PTY 输出经 spawn_blocking + mpsc channel 推送到 WebSocket。

## 测试

```bash
cargo test -p pty-server
```

## CLI 参数

| 参数 | 环境变量 | 默认 | 说明 |
|------|----------|------|------|
| `--port` | `PORT` | `3000` | HTTP/WS 监听端口 |
| `--shell` | `SHELL` | `$SHELL` 或 `/bin/bash` | 默认 shell |
| `--cwd` | `CWD` | 当前目录 | 所有终端的工作目录 |
| `--cmd` | `CMD` | 无 | 第一个终端自动注入的命令 |
| `--default-cols` | — | `80` | 默认终端列数 |
| `--default-rows` | — | `24` | 默认终端行数 |
| `RUST_LOG` | — | `info` | tracing 日志级别 |
