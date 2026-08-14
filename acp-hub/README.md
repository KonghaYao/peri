# acp-hub

acp-hub 是本地 ACP agent 的持久 Web 工作台。server 负责认证、项目/会话元数据、
运行实例编排和 Yjs 只读投影；SolidJS Web 只消费 server 事实，不在浏览器里伪造
对话历史。

## 快速开始

前置环境：Rust toolchain、Bun，以及可由 `acp-instance` 启动的 ACP agent。

```bash
cd acp-hub
./dev.sh
```

`dev.sh` 每次都会重新构建 Web 和 Rust 二进制，随后启动 loopback server 和本地
instance；只有 listener 就绪且 instance 完成认证注册后才会打印“全部就绪”。脚本
只清理自己启动并记录 PID 的进程，不会用进程名终止其他 acp-hub。
如果目标端口已有 listener，脚本会在构建前停止并直接提示打开现有页面或先释放端口；
它不会尝试接管、覆盖或终止那个进程。
默认页面是 <http://127.0.0.1:8456/>；每次运行使用独立的
`.tmp/server.<pid>.log` 和 `.tmp/instance.<pid>.log`，避免旧 daemon 输出污染新一轮
readiness 判定。日志、instance token 和运行时目录以私有 umask 创建。按 `Ctrl+C`
停止本次开发进程。

第一次打开页面需要一个 `full` token。登录页的“令牌在哪里？”会显示**当前
运行 server 实际使用的** token 文件和可复制生成命令；不要猜测配置目录。
也可以在默认配置下执行：

```bash
cargo run -q -p acp-hub-server -- token generate --name web --role full
```

完整 token 只打印一次。它只应粘贴到本机登录页，不要提交到 Git、日志、issue
或聊天记录。浏览器登录成功后使用 HttpOnly opaque cookie，不把 bearer 保存到
URL、Web Storage 或 WebSocket 帧中。

server 仅在 stderr 直连交互终端时显示首次 bootstrap instance token；由
`dev.sh`、systemd 或日志管道启动时不会把它写入日志，只会提示受 `0600` 权限保护的
`tokens.toml` 路径。

## 产品模型

- 左栏只显示由 Hub 创建或由用户明确导入的持久会话；ACP 历史不会自动污染目录。
- project session 是持久入口，ACP session 是 agent thread，runtime chat 是一次进程
  激活；三者身份不可互换。
- server 重启后不会伪装成恢复旧进程。打开持久会话时使用精确 ACP session id
  建立新 runtime，并通过 `session/load` 恢复上下文。
- project/session 元数据保存在 `<data_dir>/metadata.sqlite3`；聊天 Yjs 日志与 outbox
  保持独立的崩溃恢复语义。

完整身份模型与安全边界见 [架构契约](docs/architecture.md)，术语见
[terminology](docs/terminology.md)，当前 UI/UX 工作与验证证据见
[active issue](../spec/issues/2026-08-13-acp-hub-uiux-audit.md)。`ui.md` 仅是重构前
历史基线，不是当前实现说明。

## 常用命令

```bash
# Web
cd web
bun run test
bun run build

# Rust workspace
cd ..
cargo test -p acp-hub-proto
cargo test -p acp-hub-server --lib
cargo clippy --workspace --all-targets -- -D warnings

# 无敏感值地列出 token 记录，或吊销一个 token
cargo run -q -p acp-hub-server -- token list
cargo run -q -p acp-hub-server -- token revoke <token_id>
```

需要自定义目录时，server、token CLI 与 `dev.sh` 使用相同环境变量：

```bash
ACP_HUB_CONFIG_DIR=/path/to/config ACP_HUB_DATA_DIR=/path/to/data ./dev.sh
```

`ACP_HUB_LISTEN_ADDR`/`ACP_HUB_LISTEN_PORT` 会同时决定 server listener、instance
outbound URL 和最终打印的 Web 地址；如需跨主机连接，可用 `ACP_HUB_SERVER_URL`
显式覆盖 instance URL。

当前浏览器部署只支持 loopback 明文 HTTP。不要把 `8456` 直接暴露到公网；远程
部署需要 TLS、Secure cookie 与相应的 non-loopback 安全配置。
