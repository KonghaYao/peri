# peri-web-pty

Web PTY 终端服务，浏览器即开即用的远程 shell。Rust lib + bin crate，可独立运行，也可被主项目 `peri` 通过 `peri web` 子命令嵌入。

## 运行

```bash
cargo run -p peri-web-pty                          # 随机端口（推荐）
cargo run -p peri-web-pty -- --port 8080           # 自定义端口
cargo run -p peri-web-pty -- --cwd /path/to/proj   # 指定工作目录
cargo run -p peri-web-pty -- --cmd "npm run dev"   # 第一个终端自动注入命令
cargo run -p peri-web-pty -- --shell /bin/zsh      # 自定义默认 shell
```

所有参数同时支持环境变量：`PORT`、`CWD`、`CMD`、`SHELL`。

```bash
CWD=/path/to/proj CMD="npm run dev" cargo run -p peri-web-pty
```

启动后控制台日志会打印实际监听地址（如 `http://localhost:53891`），并尝试自动打开浏览器。

## 功能

- **多终端分屏**：顶部工具栏支持新建终端、水平/垂直分屏、自适应网格、恢复单列。每个终端对应独立的 WebSocket + PTY 会话。
- **启动命令注入**：`--cmd` 或 `CMD` 指定的命令仅在第一个终端连接时自动执行，后续新终端不受影响。
- **工作目录**：`--cwd` 或 `CWD` 指定的目录会应用于所有 PTY 会话。

## 架构

- HTTP/WS：`axum 0.7`（内置 WebSocket 升级）
- PTY：`portable-pty 0.9`（macOS/Linux 用 openpty，Windows 用 ConPTY）
- CLI：`clap 4` derive + env fallback
- 前端：单 HTML 文件，CDN 加载 xterm.js + 内联 JS，`include_str!` 嵌入二进制

每个 `/ws` 连接 spawn 一个 shell 子进程，PTY 输出经 `spawn_blocking` + mpsc channel 推送到 WebSocket。

## 测试

```bash
cargo test -p peri-web-pty
```

## CLI 参数

| 参数 | 环境变量 | 默认 | 说明 |
|------|----------|------|------|
| `--port` | `PORT` | `0`（随机分配） | HTTP/WS 监听端口 |
| `--shell` | `SHELL` | `$SHELL` 或 `/bin/bash` | 默认 shell |
| `--cwd` | `CWD` | 当前目录 | 所有终端的工作目录 |
| `--cmd` | `CMD` | 无 | 第一个终端自动注入的命令 |
| `--default-cols` | — | `80` | 默认终端列数 |
| `--default-rows` | — | `24` | 默认终端行数 |
| `RUST_LOG` | — | `info` | tracing 日志级别 |
