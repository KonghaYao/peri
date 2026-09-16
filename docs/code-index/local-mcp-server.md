# local-mcp-server 代码索引

独立项目，入口为 `side-projects/local-mcp-server/Cargo.toml`，不属于根 Cargo workspace。
使用与配置见 [项目 README](../../side-projects/local-mcp-server/README.md)；行为以实现与契约测试为准。

## 数据流

`ServerCli → main::run → stdio/HTTP → SandboxServer → InProcessExecutor → 文件工具/TaskRegistry`。
两种传输共用工具执行器；任务注册表持有 Bash 生命周期，退出时回收自有任务进程组。
项目以当前用户权限执行，不提供沙箱隔离；文件工具的工作区约束不限制 Bash 命令权限。

## 速查表

下列路径均相对于 `side-projects/local-mcp-server/`。

| 我想做什么 | 主文件与入口 | 验证入口 |
| --- | --- | --- |
| 改 CLI、配置校验 | `src/config.rs`：`ServerCli::into_config`、`Config::validate` | 同文件配置测试 |
| 改启动、信号与退出顺序 | `src/main.rs`：`run`、`serve_stdio`、`serve_http` | `tests/e2e_stdio.rs`、`tests/e2e_http.rs` |
| 改协议接线与工具目录 | `src/mcp/server.rs`：`SandboxServer`；`src/mcp/catalog.rs` | `tests/schema_tools.rs`、`tests/schema_errors.rs` |
| 改 stdio 生命周期 | `src/transport/stdio.rs`：`serve_stdio_with_shutdown` | `tests/transport_stdio.rs`、`tests/e2e_stdio.rs` |
| 改 HTTP 身份、请求校验与关闭 | `src/transport/http.rs`：`serve_http`、`HttpServer`；`src/auth/mod.rs` | `tests/transport_http.rs`、`tests/http_identity.rs`、`tests/http_tools_wire.rs` |
| 改工具执行分派 | `src/runtime/executor.rs`：`InProcessExecutor`；`src/wire.rs`：`ToolExecutor` | `tests/fs_executor.rs`、双传输 E2E |
| 改文件路径、事务与草稿 | `src/capability/root.rs`：`RootDir`；`src/tools/fs/` | `tests/fs_capability.rs`、`tests/fs_write.rs`、`tests/fs_edit.rs` |
| 改搜索与输出限制 | `src/tools/grep/`、`src/tools/fs/glob.rs`、`src/output/mod.rs` | `tests/grep_delivery.rs`、`tests/grep_search.rs`、`tests/fs_glob.rs` |
| 改 Bash、任务归属与回收 | `src/tasks/registry.rs`：`TaskRegistry::close`；`src/tasks/bash.rs` | `tests/tasks_registry.rs`、`tests/bash_lifecycle.rs`、双传输信号回归 |

## 维护边界

- `tests/fixtures/schemas/` 被生产工具目录编译嵌入，也被契约测试读取；这些 JSON 是产品依赖。
- `tests/fixtures/protocol/` 与 `tests/fixtures/fs/` 是实际测试输入；不能按扩展名作为生成物清理。
- `sandbox://tasks` 是兼容资源 URI，内部历史类型名也不代表隔离能力。
- 构建与验证命令统一维护在项目 README；测试范围遵循 [testing.md](../standards/testing.md)。
- 审计记录、临时脚本和日志不属于产品源码；根工作区通过不能替代本项目的验证。
