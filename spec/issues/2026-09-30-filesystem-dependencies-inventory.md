# Peri 仓库文件系统依赖清单

状态：待办清单；已处理项在 5.3 各保留一行，未完成项与未验证内容保留，历史过程及测试明细查 Git。

- 范围：整个仓库的 Rust crates（`peri-agent` 即原 `peri-core`，已于 2026-09 更名）；**排除 `mcp-packages`**——它们本身就是文件系统工具环境，是未来文件系统耦合的落点，不是本清单的对象
- 关联：`2026-09-27-v4-cloud-architecture-audit.md`（存算分离方向）、`peri-agent/src/lib.rs`（计划引入 feature gates）

## 一、用户裁决（本文方向锚点）

1. **范围与落点**：文件系统耦合的清单覆盖全仓库；`mcp-packages` 除外，且**是未来耦合的移动目标**（工具执行环境）。
2. **机器 env 分区（P2，核心改动）**：execution_environment_id 标记机器位置，在数据库中区分 env；采用 Session ID 恢复，路径仅作过滤/展示/工具定位；统一地址需求已废弃。目标见 [核心改动清单](2026-09-30-session-id-environment-core-change.md)。
3. **存储（无需整改）**：SQLite 与 turso 后端及 `--session-store env:<VAR>` 已支持，属**安全依赖**，不列入待办等级表。
4. **配置数据面（P3，已完成本清单范围）**：ACP settings、MCP 配置与 hooks/开关来源 I/O 统一到独立配置 MCP；宿主保留类型校验、来源优先级与业务投影，插件体系整体迁移另列。
5. **插件体系（P3，推迟）**：保留现有落盘安装、缓存与扫描；Plugin MCP 迁移暂不推进，不作为核心存算分离工作的前置条件。
6. **shell（P1）**：执行与输出持久化归工具执行环境；**workflow 与 PTC 是特例**（本地 JS 执行环境自管理，另行处置）。
7. **工具 fs（P1）**：本清单中的工具执行依赖已下沉，不代表 OAuth 凭据、MCP 缓存及 P3 插件体系也已迁移。
8. **TUI 免除**：TUI 相关（客户端本地状态、主题、web-pty 等）**免除定级**。
9. **遥测**：落盘日志**免除，不整改**；Langfuse 服务端上报效果仍需端到端验证。
10. **compact**：不应读取 skill 文件——保留历史工具调用记录即可；同机制的文件回读（recent files）一并评估。`compact_v2/full.rs` 的文件读取应移除。
11. **OAuth 与缓存分开**：MCP OAuth 凭据采用 SQLite 新表方向，不迁移旧 JSON、不兼容或回退旧存储，需重新授权；response cache 与插件缓存仍推迟，保留落盘，不扩大本轮改动。
12. **恢复机制（核心改动，已实施）**：移除 session 文件锁及前端 dirty 恢复弹窗；root session 以 ID 恢复，不按目录或 owner 认领，父子关系保持。不用分布式锁替代。

## 二、全仓库总览

未改动 crate 的点数保留 2026-09-30 扫描快照；已改动 crate 标为「未重统计」，不把旧计数冒充现状（模式含 `std::fs` / `tokio::fs` / `dirs_next` / `temp_dir` / `current_dir`，排除 `*_test.rs`）：

| crate | 点数 | 主要用途 | 处置方向 |
| --- | --- | --- | --- |
| `peri-acp` | 未重统计 | rewind 文件回退、工作区 canonicalize、插件缓存（P2）、`/etc/os-release` | 配置 I/O 已下沉；ID 恢复/env 分区已实施；统一地址需求废弃 |
| `peri-acp-types` | 0 | 仅 `PathBuf` 类型（契约数据） | 保持；路径类型不是 feature 边界 |
| `peri-agent` | 未重统计 | 存储桥、compact 文件回读、日志 | compact 回读待移除；存储无需整改；落盘日志免除 |
| `peri-controller` | 0 | — | — |
| `peri-middlewares` | 未重统计 | 插件/MCP 管理、MCP 执行环境适配 | P2 缓存 + P3 Plugin MCP |
| `peri-model` | 0 | — | — |
| `peri-process` | 0 | 进程树所有权（OS 依赖，非 fs） | 随 shell feature 处置 |
| `peri-resources` | 32 | SQLite 存储、Filesystem 测试后端、`~/.peri` 配置路径、turso 远端 | 存储安全依赖无需整改；目录定位不等于配置正文 I/O |
| `peri-runtime` | 0 | — | — |
| `peri-theme` | 2 | 主题文件读取 | TUI 相关，**免除** |
| `peri-tui` | 85 | 设备间同步、keystore、输入历史、插件 CLI、更新、主题下载 | TUI 相关，**免除**；同步协议不纳入本轮改造 |
| `peri-web-pty` | 2 | 终端 cwd、pty I/O | **标记**：TUI 的包，未来可能删除（TUI 相关免除） |
| `peri-workflow` | 24 | workflow engine artifact 安装/发布、journal、脚本 | **特例**（第 3.5 节） |
| `peri-js-runtime` | 23 | PTC（`@peri-code/ptc`）artifact 安装与隔离 | **特例**（第 3.5 节） |
| `langfuse-client` | 0 | — | — |

## 三、分类清单

### 3.1 剩余执行依赖 / compact 移除 / 日志

| 位置 | 用途 | 具体行为 | 处置 |
| --- | --- | --- | --- |
| `mcp-packages/common/src/shell_executor.rs` | 后台 shell 执行目录 | 执行环境仍用本地 cwd 字符串 | 保留执行环境本地路径；统一地址需求废弃 |
| `peri-agent/src/agent/compact_v2/full.rs` `read_file_with_budget` / `collect_reinject_v2` | Full compact 时把 skills 与 recent files 内容读回上下文 | `spawn_blocking` 内调用 `std::fs::read_to_string`，仍在主路径使用 | **未完成、待移除**（用户裁决）：保留历史工具调用记录，不应改成 MCP 回读 |
| `peri-agent/src/agent/compact_v2/full.rs` `resolve_path` | 相对路径按 cwd 转绝对 | `Path::join` | 随回读移除 |
| `peri-agent/src/telemetry/subscriber.rs` | 运行日志滚动落盘 `~/.peri/logs` | `dirs_next`、滚动文件 | **免除，不整改** |

### 3.2 剩余宿主控制面（Plugin MCP P3 / 缓存 P2）

方向：配置数据面已处理（5.3）；插件体系按 **Plugin MCP（P3）** 方向单独设计（可能落 Workspace 内），替代当前重型依赖，缓存 P2 保留。

| 位置 | 用途 | 等级 / 处置 |
| --- | --- | --- |
| `peri-middlewares/src/plugin/{config,loader,install_counts}.rs`、`installer/*`、`marketplace/fetch.rs`、`host_ports.rs`（插件端口） | 插件安装 / 卸载 / marketplace / 计数（当前重型依赖） | **P3** Plugin MCP 方向：单独设计（可能落 Workspace 内） |
| `peri-acp/src/host/requests/plugin.rs:512-526` | 插件缓存目录扫描（`read_dir` + manifest 读取） | **P2**：暂不在考虑范围 |
| `peri-middlewares/src/mcp/auth_store.rs` | OAuth 凭证文件存储与动态清理 | **P2**：评估 SQLite 新表；凭据主体与 machine 分区须分开 |
| `peri-middlewares/src/mcp/resource_cache.rs` | 跨进程资源缓存（advisory file lock） | **P2 / 推迟**：继续落盘，本轮不处理 |
| `peri-tui/src/cli_plugin.rs`、`kit/panels/plugin/data.rs` | 插件 CLI 与面板 | **免除**（TUI 相关） |
| `peri-agent/src/resources.rs` | 存储装配桥（`Option<PathBuf>` → `peri-resources`）；默认 `~/.peri/threads/threads.db` | 安全依赖，无需整改 |

### 3.3 剩余工具环境依赖

| 位置 | 用途 | 等级 / 处置 |
| --- | --- | --- |
| `peri-web-pty/src/{lib.rs,ws_handler/io.rs}` | 终端 cwd、pty I/O | **标记 / 免除**：TUI 的包，未来可能删除 |

### 3.4 客户端本地状态（peri-tui；TUI 相关免除）

| 位置 | 用途 |
| --- | --- |
| `sync/{scanner,packer,sender,receiver,writer}.rs` | 设备间加密同步（r2-encrypted-transfer v1）：扫描配置类文件（`~/.peri/settings.json`、`~/.claude/settings.json`、`~/.claude/skills/`、`~/.claude/plugins/cache/`、`~/.mcp.json` 与 **`{cwd}/.mcp.json`**）打包传输 |
| `sync/channel_flow/staging.rs` | 同步暂存目录 `~/.peri/staging-<channel_hash>`（同卷 rename 提交） |
| `sync/{keystore,device,device_cli}.rs` | 私钥存储（OS keyring → 0600 加密文件）、设备管理 |
| `kit/input_history.rs`、`update.rs`、`kit/panels/theme/download.rs`、`kit/image_safety.rs`、`app/setup_wizard/mod.rs` | 输入历史、版本自更新、主题下载、图片安全、设置向导 |

方向：**TUI 相关免除定级**（用户裁决）；TUI 是本地客户端，本地状态用本地文件合理。但**同步协议当前以文件路径布局为中心**（协议要求 `/` 分隔符，Windows 分隔符陷阱记录在 `sync/scanner.rs` 源码注释），统一地址需求废弃，同步不作为后续地址改造待办。扫描清单中的 `~/.claude/plugins/cache/` 属 **P2** 插件缓存范围（暂不处理）。

### 3.5 特例：workflow 与 PTC（本地 JS 执行环境）

| 位置 | 内容 |
| --- | --- |
| `peri-workflow/src/runner/artifact.rs`（~20 点） | `~/.peri` 下 npm 包 `@peri-code/workflow` 的版本校验、原子发布、安装与命令准备；`journal.rs` 持久化；`tool.rs`/`tool/preflight.rs`/`cli.rs` 写脚本与 artifact |
| `peri-js-runtime/src/artifact.rs`、`artifact/install.rs`（23 点） | npm 包 `@peri-code/ptc` 的安装、隔离（rename/quarantine）、`~/.peri` 安装目录 |

这两者是"本地 node 执行环境"的自管理（安装/发布/隔离/进程 spawn），其 fs 依赖与 JS 运行时绑定。按用户裁决列为**特例**：不进入本轮定级处理序列，处置方式另行决定（宜随工具执行环境整体迁移）。

### 3.6 存储后端（peri-resources，安全依赖，无需整改）

- **无需整改**：本机 SQLite 与 turso 远端及 `--session-store env:<VAR>` 已支持，存储属于安全依赖，不列为改造项。
- `sessions/filesystem.rs`：`FilesystemThreadStore`——非默认后端，测试用途。
- `config/mod.rs`：`~/.peri`、`~/.peri/settings.json` 目录/locator 入口，不读取配置正文。

### 3.7 零 fs 依赖

`peri-model`、`peri-controller`、`peri-runtime`、`langfuse-client`、`peri-process`（进程树/Job Object，OS 非 fs）；`peri-acp-types` 无 fs I/O，仅有 `PathBuf` 类型（skills / hooks / workspace / plugin / session_store / lsp）。

## 四、Session ID 与机器 env 分区（已实施）

已实施 ID 恢复、持久 machine ID 与 env 查询 scope、不升版本的幂等数据回填，以及 session 文件锁/dirty 弹窗移除；算法、并发限制与未覆盖验证见 [设计](../../docs/design/session-id-environment.md) 和 [核心改动清单](2026-09-30-session-id-environment-core-change.md)。

## 五、用户等级表

### 5.1 待办等级表（仅保留未完成且需要整改的项）

| 等级 | 项 | 现状位置 | 处置 |
| --- | --- | --- | --- |
| **P2** | MCP OAuth 凭据 SQLite 新表 | `peri-middlewares/src/mcp/auth_store.rs` 及 OAuth 构造/动态清理入口 | 尚未实施；直接替换文件存储，不做旧数据迁移或兼容；主体与环境分区不等同于共享 token |

P2 的实现方式与难度见 [实施可行性调研](2026-09-30-p2-filesystem-implementation-assessment.md)；调研不代表已实施。MCP 缓存与插件体系已移至暂缓项，不作为当前核心工作的前置条件。

#### 暂缓项（保留落盘，不列入当前实施队列）

| 原等级 | 项 | 处置 |
| --- | --- | --- |
| P2 | MCP 响应缓存与插件缓存 | 保留现有落盘实现、权限与缓存准入；后端注入及远端迁移推迟 |
| P3 | 插件体系 / Plugin MCP | 保留现有安装、扫描及制品生命周期；插件系统迁移推迟 |

### 5.2 等级外裁决

| 裁决 | 项 | 处置 |
| --- | --- | --- |
| 无需整改 | 存储后端（本机 SQLite / turso） | 既有后端切换能力满足本清单要求，安全依赖，不列入待办等级表 |
| 免除 | TUI 相关（`peri-tui` 本地状态与插件 CLI / 面板、`peri-theme`、`peri-web-pty` 已标记可能删除） | 本地客户端，**免除定级**；同步协议不纳入本轮改造 |
| 特例 | workflow / PTC artifact 管理 | 本地 JS 执行环境自管理，另行处置（宜随工具执行环境整体迁移） |
| 免除 | 落盘日志（`~/.peri/logs` 滚动日志） | 不整改、不纳入迁移范围（`peri-agent/src/telemetry/subscriber.rs`） |
| 待移除 | compact 文件回读（skills / recent files） | 尚未移除；删除文件回读，保留历史工具调用记录（`peri-agent/src/agent/compact_v2/full.rs`） |

### 5.3 已处理（每项一行）

- **配置数据面**：ACP settings、MCP 配置、hooks 与 builtin 开关统一经独立 `peri-mcp-config` MCP 通道读写/探测，共享全局路径与原子保存，无宿主文件回落，保留类型校验、分层与来源优先级。
- **MCP 截断输出**：bridge/resource 成功、错误及恢复输出均经 Workspace `output/store` 持久化，返回实例绑定 `peri-output://` 资源 URI 与工具环境 Read 路径，不可得时明确未保存且不回落宿主。
- **归因分支探测**：`before_agent` 改走 Workspace `workspace/gitBranch`，git 执行、超时与进程树清理下沉工具环境，宿主仅消费分支值。
- **Shell 本地执行边界**：进程、tee 与输出持久化移至 `mcp-packages/common`，Agent 仅保留生命周期和 `ShellExecutor` 注入端口，移除其 `peri-process` 依赖并将 `libc` 留在测试。
- **图片附件**：读取、大小与 MIME 签名校验下沉 Workspace MCP `image/read`，宿主不读盘，关闭/断连/换代不回落本机。
- **归因文件快照**：写前/写后正文改走 Workspace MCP `workspace/readText`，按 tool-call 身份隔离暂存。
- **LSP 正文同步**：正文改走 Workspace MCP，保留 ready gate、didChange → didSave 顺序及 change 失败后仍尝试 save。
- **`skillsDir`**：配置字段、别名、loader、自定义全局根和宿主插件目录预过滤已删除，其他来源及 builtin 开关保留，旧键仅可能作为不被消费的未知 JSON 透传。
- **metrics 出口**：删除本地 JSONL 落盘，改用 Langfuse event 上报；无 Langfuse 时丢弃指标。
- **metrics trace 归属**：按 sid 关联活跃 turn trace，无活跃 trace 时回退独立 root trace。
- **`peri-agent` → `sqlx`**：删除未使用的直接依赖，存储层传递依赖保持不动。

### 5.4 未完成边界

- compact 的 `full_compact_inner → collect_reinject_v2 → read_file_with_budget` 仍执行宿主文件回读；本轮仅核对状态，未修改 compact 代码。
- 配置 MCP 的远端部署认证/TLS 与跨设备 locator 属部署/P2 后续工作；插件 manifest、安装与 marketplace 生命周期仍在 Plugin MCP P3，未扩为本轮范围。
- 输出资源 URI 绑定当前 Workspace 实例，实例关闭/重建后不保证旧 URI 可读；输出文件保留于工具环境，跨实例恢复不在本轮范围；统一地址需求废弃。
- Workspace 仍使用本地路径，不构成 cwd 沙箱；统一地址需求废弃；绝对路径/symlink 行为保持现状。
- 图片只做 MIME 签名判断而非完整解码；请求取消不保证已进入 OS 的 blocking I/O 立即结束。

## 六、Cargo 依赖与清理

| 项 | 现状 | 建议 |
| --- | --- | --- |
| `dirs-next` | 分散于 peri-agent、peri-resources、peri-tui、peri-workflow、peri-js-runtime、peri-acp | 可收敛为宿主注入的目录参数；低优先 |
| `peri-agent` → `peri-resources` | 仅 `resources.rs` 声明边 | 可选项化/由宿主编排（feature 或纯注入） |
| `tempfile`(dev) | 测试夹具 | 保留 |
| `peri-resources` 传递 | `sqlx`（SQLite 驱动）、`tokio::fs`（FilesystemThreadStore）、`turso_serverless`（远端）、`dirs-next` | turso 远端为既有能力，提及即可 |

## 七、未验证

- 未做编译期 feature 拆分实验
- 未核对 `peri-tui/src/sync` 协议全量字段（仅扫描 fs 触点与 staging 语义）
- 未跑全库/E2E、Windows 原生执行或 120 秒 ignored 用例
- OAuth SQLite 新表已做源码级可行性评估，尚未实现；旧数据迁移明确不做，MCP cache 与 Plugin MCP 继续暂缓
- Session ID / env 核心实现与定向契约测试已迁移；未做远端多机器端到端部署验证
- metrics → Langfuse 未做端到端上报验证（本地无 Langfuse 凭据，未观察 Langfuse 服务端落库；含指标归属到活跃 turn trace 的服务端表现）
