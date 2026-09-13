# local-mcp-server

`local-mcp-server` 是独立的 MCP server，在一个本机进程中提供 Peri 的七个工具：
`Read`、`Write`、`Edit`、`Glob`、`Grep`、`folder_operations` 和
`Bash`。支持 stdio、Streamable HTTP、MCP `2026-07-28` modern 请求，以及
`2025-11-25` legacy initialize 握手。

## 执行与权限

server 以启动它的当前用户身份直接运行，不提供沙箱、容器、网络隔离、文件系统隔离或
权限降级。`--workspace` 是六个文件工具的能力边界，不是安全边界。`Bash` 是普通的
`bash -c` 子进程，工作区是其 cwd；它可以访问当前用户能访问的工作区外文件和网络。
只应让可信调用方处理可信输入。HTTP bearer token 只用于认证和任务归属，不能隔离用户。
需要隔离时，请在本产品之外使用你管理的容器、虚拟机、低权限用户或独立主机。

stdio 握手后的 stdin EOF、SIGINT 和 SIGTERM 会在退出前回收自有后台 Bash 任务
（TERM 后等待两秒再 KILL）。SIGKILL、断电和进程崩溃无法执行清理，可能留下子进程。

## 构建与启动

本目录是独立 Cargo workspace，不属于 Peri 根 workspace；要求 Rust 1.88+、edition 2021，
不依赖 Peri crate。

```bash
cd side-projects/local-mcp-server
cargo build
cargo build --release
cargo build --offline       # 依赖已下载后使用
```

stdio：

```bash
target/release/local-mcp-server \
  --transport stdio \
  --workspace /abs/path/to/workspace
```

stdout 只承载逐行 JSON-RPC，日志走 stderr。握手后关闭 stdin 正常退出；握手前关闭的退出码
为 4。发布二进制为 `target/release/local-mcp-server`。

MCP 客户端接入示例（将二进制和工作区路径替换为本机绝对路径）：

```json
{
  "mcpServers": {
    "local": {
      "command": "/abs/path/local-mcp-server",
      "args": ["--transport", "stdio", "--workspace", "/abs/path/to/workspace"]
    }
  }
}
```

Streamable HTTP：

```bash
target/release/local-mcp-server \
  --transport http --bind 127.0.0.1:8765 \
  --workspace /abs/path/to/workspace
```

默认绑定 `127.0.0.1:0`，端口 0 由系统分配。客户端通常 POST 到 `/mcp`，请求使用
`Content-Type: application/json` 并接受 `application/json, text/event-stream`；
成功响应是 SSE。modern 请求必须提供 `MCP-Protocol-Version`、`Mcp-Method`，以及
适用时的 `Mcp-Name`；header 与 body 不一致返回 `400` 和 `-32020`。

HTTP 非回环绑定必须同时指定 `--allow-non-loopback` 和 token 来源。token 只能来自环境变量
或文件（`--token-env` 与 `--token-file` 互斥），不能放在命令行：

```bash
export LOCAL_MCP_TOKEN="$(cat ~/.config/local-mcp/token)"
target/release/local-mcp-server \
  --transport http --bind 127.0.0.1:8765 \
  --token-env LOCAL_MCP_TOKEN --workspace /abs/path/to/workspace
```

客户端发送 `Authorization: Bearer $LOCAL_MCP_TOKEN`。缺失或错误凭证返回 `401`；
token 值不会写入日志。默认 Host 白名单为回环名称和实际绑定地址，可用
`--allowed-host` 替换。Origin 默认不校验；配置 `--allowed-origin` 后，缺失 Origin
仍放行，未列出的 Origin 拒绝。

## 配置

`--help` 是权威选项列表：

| 选项 | 默认值 | 作用 |
| --- | --- | --- |
| `--transport <stdio\|http>` | `stdio` | 传输类型 |
| `--workspace <PATH>` | 当前目录 | 文件工具根与 Bash cwd |
| `--bind <ADDR>` | `127.0.0.1:0` | HTTP 监听地址 |
| `--allowed-host <HOST>` | 回环白名单 | Host 白名单，可重复 |
| `--allowed-origin <ORIGIN>` | 不校验 Origin | Origin 白名单，可重复 |
| `--max-body-bytes <N>` | `4194304` | HTTP body 上限，范围 1 字节至 64 MiB |
| `--token-env <VAR>` | 无 | token 所在环境变量 |
| `--token-file <PATH>` | 无 | token 文件 |
| `--allow-non-loopback` | 关闭 | 允许非回环绑定 |
| `--task-ttl-secs <N>` | `3600` | 终态任务保留秒数 |
| `--task-retention <N>` | `100` | 终态任务最多保留数 |

`RUST_LOG`（默认 `info`）控制 stderr 日志。workspace 根在启动时 canonicalize；
无效根、body/token 配置、非正任务限制和不安全的非回环配置以退出码 2 拒绝启动。

进程退出码：`0` 正常结束，`2` 用法或配置错误，`4` 传输失败，`70` 内部错误。
任务回收失败会作为进程错误返回；具体退出边界见 [入口实现](src/main.rs) 和
[错误定义](src/error.rs)。

## 工具契约

`tools/list` 固定返回上述七个名称；`reading` 仅在调用时别名为 `Read`，
`Shell` 仅在调用时别名为 `Bash`。`sandbox://tasks` 和
`sandbox://tasks/{task_id}` 是冻结的资源 URI 字面量，名称不代表隔离。

| 工具 | 必填输入 | 主要语义 |
| --- | --- | --- |
| `Read` | `file_path` | 读取文件或列目录；32 MiB 文件上限；按行和总量截断 |
| `Write` | `file_path` | 原子写事务；支持 `content`、`from_draft`、`append` |
| `Edit` | `file_path`、`old_string`、`new_string` | 原子替换；旧文本默认必须唯一 |
| `Glob` | `pattern` | 工作区内 glob；扫描和输出有界，超量结果落盘 |
| `Grep` | `pattern` | 工作区内搜索；支持内容、文件、计数、上下文、类型和深度选项 |
| `folder_operations` | `operation`、`folder_path` | `create`、`list`、`exists`、`deep_scan` |
| `Bash` | `command` | 合并 stdout/stderr；前台超时后可提升为后台任务 |

文件工具拒绝工作区外路径和符号链接逃逸，写入使用同目录临时文件加原子替换。根内硬链接
仍可能暴露工作区外 inode；根路径替换为另一个真实目录时，Grep 可能读到该路径内容，
而其他文件工具仍使用启动时锚定的根。Grep 会复核候选路径并对符号链接换向 fail-closed，
但解析到打开之间仍有文件系统竞态窗口。

Bash 的 `timeout` 单位为毫秒，前台默认 15 秒，`0` 表示不超时，最大按 600 秒处理；
输出上限为 65000 字节或 2000 行，超量结果写入工作区 `.local-mcp/logs/` 并返回 head/tail。
Glob、Grep 和目录扫描也限制内联输出，完整结果可能写入 `.local-mcp/artifacts/`。
这些日志是工作区内普通文件，同一主体的其他连接可用 Read 读取；任务句柄仍按 owner 归属
控制。modern HTTP 请求忽略 `Mcp-Session-Id`，身份绑定 TCP 连接，换连接不可见任务；legacy 会话复用 session ID
时可跨连接读取。当前配置只有一个 bearer token，因此是单主体部署。

## 已知限制与验证

- HTTP body 无法解析时由传输层返回 `415` 纯文本；协议错误使用 JSON-RPC。
- 每条连接首个请求同时出现 `Content-Length` 与 `Transfer-Encoding` 时返回 `400`；
  后续 keep-alive 请求不做这项自建判定。请求头超过 8 KiB 时交给 hyper，首字节后 5 秒
  仍未读完请求头则关闭连接。
- 绝对路径必须使用 workspace 的 canonical 拼写，建议使用相对路径。
- 已在 macOS arm64 验证；Linux 和 Windows 尚未验证。

目录中的 `src/` 是实现，`tests/` 是单元与集成测试，`tests/fixtures/` 是被测试和
工具目录读取的 JSON schema/协议输入。常用检查：

```bash
cargo fmt --check
cargo check --tests
cargo clippy --all-targets -- -D warnings
cargo test --lib
cargo test --tests
cargo test --doc
```

维护时按 [代码索引](../../docs/code-index/local-mcp-server.md) 定位入口。上述命令在本目录执行；
根仓库的 `cargo test --workspace` 不包含本项目。
