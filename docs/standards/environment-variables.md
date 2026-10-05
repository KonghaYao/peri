# Peri 环境变量控制表

本表是 **Peri 主产品及随附运行组件的具名环境变量权威入口**；TUI 专属变量的权威明细独立放在 [TUI 环境变量控制表](tui-environment-variables.md)。现行行为仍以代码和契约测试为准；修改变量的名称、取值、优先级、默认值或作用范围时，须在同一变更中更新对应表。这里不收录测试夹具、构建脚本、`side-projects/`、SDK 示例的专用变量，也不把任意传给子进程的变量误认为 Peri 开关。

### ENV-CATALOG-001

- **Scope**：Peri 主程序、运行时 crate 和随附 MCP 组件中读取的具名环境变量。
- **Rule**：新增、删除或改变环境变量控制行为时，同步更新本表或 TUI 专属表中的作用、有效值、默认/优先级及消费入口；其他文档只保留任务相关说明并链接相应表，不维护第二份完整清单。名称由部署方动态指定的变量按“动态入口”说明，不逐个登记具体名称。
- **Verify**：检索生产代码的 `std::env::var`、`var_os`、`get("...")`、Clap `env =`、`ENVIRONMENT_KEYS` 及相关常量，对照本表；`git diff --check`。

## 配置来源与优先级

`peri-config` 的具名配置变量通过 configuration MCP 数据面按名称读取；本地默认部署读本机进程环境，远端配置部署读该 provider 的环境，不能假定总是计算进程的 OS 环境。TUI 启动时还会从 Peri settings 的 `config.env` 和 Claude settings 的 `env` 向进程注入**尚不存在**的字符串变量；已有进程变量不会被覆盖。以下各行的“环境变量”均指相应消费入口实际读到的值。模型环境组合指定初始 provider 与档位，显式会话模型选择仍由会话配置控制；Langfuse 的环境值覆盖对应 settings 字段；MCP cache 合并 global、project 与环境设置，任一处为 false 即关闭，否则任一处为 true 即开启。

## 模型与 Agent

模型凭据与 API 地址来自 settings 中配置的 provider。`ANTHROPIC_*`、`OPENAI_*` 不再控制 Peri 的模型连接；Claude settings 导入和独立 E2E judge 的同名输入不属于 Peri 模型选择。

| 变量 | 控制什么；有效值与缺省行为 | 消费入口 |
| --- | --- | --- |
| `MODEL_PROVIDER` | 与 `MODEL_TYPE` 同时设置；值为 settings 中精确匹配的 provider ID，选中该 provider 的凭据与 endpoint。只设置其中一个或 ID 无效时模型配置不可用。 | `peri-config/src/provider.rs` |
| `MODEL_TYPE` | 与 `MODEL_PROVIDER` 同时设置；值为 `fable`、`opus`、`sonnet` 或 `haiku` 档位，从所选 provider 的 models 映射选模型（`fable` 映射缺失时沿用该 provider 的 `opus`）；该档位无模型时配置不可用。未设置这一对时沿用 active profile。 | 同上 |
| `DISABLE_COMPACT` | **存在即生效**：关闭自动 compact，且把 micro compact 阈值设为 `1.0`；`0` 也会触发。 | `peri-acp-types/src/compact.rs` |
| `DISABLE_AUTO_COMPACT` | **存在即生效**：关闭自动 compact；`0` 也会触发。 | `peri-acp-types/src/compact.rs` |
| `COMPACT_THRESHOLD` | 覆盖自动 compact 阈值；只接受 `0.0` 到 `1.0` 的有限可解析数字，非法值保留配置值。 | `peri-acp-types/src/compact.rs` |

Compact 的三项环境变量在宿主装配时覆盖 `config.compact`；Agent 执行链只消费装配结果。

## MCP、工具与会话

| 变量 | 控制什么；有效值与缺省行为 | 消费入口 |
| --- | --- | --- |
| `PERI_MCP_CACHE` | MCP 响应缓存开关；`true/false`、`1/0`、`on/off`，忽略首尾空白和大小写；非法值报配置错误。环境值不强制覆盖文件中的 false；全部缺省时启用。 | `peri-config/src/mcp.rs` |
| `PERI_MCP_BUILTIN` | 全局禁止默认注入内建 `web`、`artifact`、`cron`、`workspace` MCP 实例；`off` 或 `0` 关闭注入，缺省开启，其他值告警后仍开启。关闭 `workspace` 还会移除内建文件/终端/资源能力；`--bare` 也受此开关影响。 | `peri-config/src/mcp.rs`、`peri-middlewares/src/mcp/builtin/mod.rs` |
| `PERI_MCP_APPS` | stdio ACP 的 MCP Apps deployment profile；支持 MCP Apps 的客户端启动 Peri 时携带此变量，**存在即启用**，空串或 `0` 也启用；缺省关闭。Peri 提供 ACP relay，客户端负责 Apps UI。 | `peri-acp/src/host/stdio/mod.rs` |
| `PERI_ASK_USER_TIMEOUT_SECS` | AskUser 等待 ACP 客户端回答的超时秒数；无效或缺省为 300 秒，`0` 为无限等待。适用于 TUI 和 stdio 共用 broker。 | `peri-acp/src/broker/transport_broker.rs` |
| `PERI_MACHINE_ID` | 当前机器的 session environment 身份覆盖；必须为 UUID，解析后规范化，首次初始化后进程内缓存；缺省使用本机 `~/.peri/machine-id`。 | `peri-resources/src/sessions/machine.rs` |
| `PERI_ARTIFACTS_URL` | Artifact MCP 实例 `env` 中的上传服务地址；缺省使用内置公共服务。Peri 宿主只透传实例环境。 | `mcp-packages/artifact/src/client.rs` |
| `PERI_ARTIFACTS_TOKEN` | Artifact MCP 实例 `env` 中的上传 token；缺省使用公共服务协议标识。宿主不解释、不记录值。 | 同上 |
| `PERI_WORKFLOW_ALLOW_NPX_FALLBACK` | 固定版本 Workflow artifact 不可用时，值**恰为 `1`**才允许 npx 后备路径；缺省不允许（测试构建例外）。 | `peri-workflow/src/runner/artifact.rs` |

支持 MCP Apps 的客户端应在启动 stdio ACP 子进程时传入 `PERI_MCP_APPS=1`，例如 `PERI_MCP_APPS=1 peri acp`。Artifact 连接信息经 `mcpServers.artifact.env` 传给内建实例，例如 `{"mcpServers":{"artifact":{"env":{"PERI_ARTIFACTS_URL":"${ARTIFACT_ENDPOINT}","PERI_ARTIFACTS_TOKEN":"${ARTIFACT_SECRET}"}}}}`；`${…}` 由 MCP 配置加载器展开。外部 Artifact MCP 子进程使用自己的 MCP `env` 配置，不需要 Peri 宿主解释这些字段。

## Langfuse 与日志

Langfuse 只有 public key 和 secret key **都存在**时才启用。以下环境值覆盖同名 settings 值；数值解析失败则保留 settings 或默认值。队列等容量的最终运行约束还由 Controller/Client 执行。

| 变量 | 控制什么；缺省值 | 消费入口 |
| --- | --- | --- |
| `LANGFUSE_PUBLIC_KEY` | Langfuse public key；缺省无。 | `peri-config/src/observability.rs` |
| `LANGFUSE_SECRET_KEY` | Langfuse secret key；缺省无。 | 同上 |
| `LANGFUSE_BASE_URL` | Langfuse 服务地址；缺省 `https://cloud.langfuse.com`。 | 同上 |
| `LANGFUSE_TRACE_SAMPLING` | Trace 采样比例；解析后夹到 `[0,1]`，缺省 `1.0`。 | 同上 |
| `LANGFUSE_ERROR_SPAN_ALWAYS` | 错误 span 是否始终保留；仅 `false`（不区分大小写）或 `0` 为 false，其余为 true；缺省 true。 | 同上 |
| `LANGFUSE_BATCH_MAX_EVENTS` | 单批最大事件数；缺省 50。 | 同上 |
| `LANGFUSE_BATCH_QUEUE_CAPACITY` | 事件队列容量；缺省 1024。 | 同上 |
| `LANGFUSE_BATCH_MAX_IN_FLIGHT` | 同时发送的批次数；缺省 2。 | 同上 |
| `LANGFUSE_BATCH_MAX_EVENT_BYTES` | 单事件字节预算；缺省 512 KiB。 | 同上 |
| `LANGFUSE_BATCH_MAX_BYTES` | 单批字节预算；缺省 4 MiB。 | 同上 |
| `LANGFUSE_BATCH_MAX_QUEUE_BYTES` | 整个队列的字节预算；缺省 16 MiB。 | 同上 |
| `LANGFUSE_BATCH_FLUSH_INTERVAL` | 批次定时发送间隔（秒）；缺省 10。 | 同上 |
| `LANGFUSE_USER_ID` | 自定义观测 user 维度；缺省无。 | 同上 |
| `RUST_LOG` | tracing filter 指令；缺省 `info`，并将 MCP/plugin/rmcp 模块设为 `warn`。 | `peri-agent/src/telemetry/subscriber.rs` |
| `RUST_LOG_FORMAT` | 恰为 `json` 时输出 JSON 日志；其他值使用普通格式。 | 同上 |
| `RUST_LOG_FILE` | 日志文件路径/名前缀；缺省 `~/.peri/logs/<service>`，按天轮转。 | 同上 |

## 操作系统与进程输入

下列是多个组件使用的 OS/进程环境输入，不等同于 Peri 业务开关。

| 变量 | 控制什么；有效值与缺省行为 | 消费入口 |
| --- | --- | --- |
| `HOME` | 用户目录/历史等路径的操作系统输入；影响缓存、插件、主题、历史等目录，具体使用依平台和组件而异。 | `peri-tui/src/kit/input_history.rs` 等 |
| `USERPROFILE` | `HOME` 不可用时 TUI 历史和部分文件路径的回退用户目录；具体使用依平台和组件而异。 | `peri-tui/src/kit/input_history.rs`、`mcp-packages/workspace/src/image.rs` |
| `PATH` | 查找 Node/npm/npx、shell 工具和可执行文件；动态 MCP 子进程按其 allowlist 继承。 | 相关进程启动入口 |
| `MALLOC_CONF` | jemalloc 配置；已存在时 Peri 不覆盖，缺省时 TUI 启动入口注入自己的配置。 | `peri-tui/src/alloc_config.rs` |

## 动态环境变量入口

以下机制允许配置决定变量**名称**，因此没有可穷举的固定名字；具体名字由部署或用户配置维护：

| 入口 | 语义与边界 | 代码入口 |
| --- | --- | --- |
| Peri `config.env`、Claude settings `env` | TUI 启动时注入字符串值，已存在的进程变量优先。注入后是否控制 Peri 取决于本表中的消费方或外部子进程。 | `peri-tui/src/main.rs` |
| MCP 配置的 `${VAR}`、hook 的 `allowed_env_vars` | 按配置引用或白名单读取变量，用于命令、参数、HTTP header 等；这不会为每个 `VAR` 新增 Peri 全局开关。 | `peri-middlewares/src/mcp/config.rs`、`peri-middlewares/src/hooks/variables.rs` |
| `--session-store-token-env=<NAME>` | 按 CLI 给定名称读取远端 Session Store 凭据；文档只传名称，不登记 secret 值。 | `peri-resources/src/sessions/remote/credentials.rs` |
| 子进程运行环境 | MCP、hook、Workflow 和工具进程各自按配置或 allowlist 接收环境；Peri 注入的 `CLAUDE_*`、`GIT_OPTIONAL_LOCKS`、`TERM` 等是子进程输入，不是启动 Peri 的用户开关。 | 对应进程构造器 |

密钥值不得写入本表、日志或示例；见 [ARC-SECRET-001](architecture-contracts.md#arc-secret-001)。
