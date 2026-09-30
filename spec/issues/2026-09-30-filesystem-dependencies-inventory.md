# Peri 仓库文件系统依赖清单

状态：扫描报告 + 用户等级表 + P1 实施验收。第二、三节保留实施前扫描快照；当前实施状态见 5.3，事实以源码与验证证据为准。

- 范围：整个仓库的 Rust crates（`peri-agent` 即原 `peri-core`，已于 2026-09 更名）；**排除 `mcp-packages`**——它们本身就是文件系统工具环境，是未来文件系统耦合的落点，不是本清单的对象
- 关联：`2026-09-27-v4-cloud-architecture-audit.md`（存算分离方向）、`peri-agent/src/lib.rs`（计划引入 feature gates）
- 修订：v1（仅 peri-agent 扫描）→ v2（全仓库 + 用户裁决融入 + cwd/URI 章节）→ v3（用户等级表 P1–P4 与等级外裁决）→ v4（等级重排：shell / 工具 fs / skillsDir → P1，cwd 统一地址 → P2，插件体系 → P3，TUI 免除，遥测定案）

## 一、用户裁决（本文方向锚点）

1. **范围与落点**：文件系统耦合的清单覆盖全仓库；`mcp-packages` 除外，且**是未来耦合的移动目标**（工具执行环境）。
2. **cwd / 统一地址（P2）**：cwd 与多设备统一寻址（URI 化，第四节）列为 **P2**；仍是大工程（此前「单独立项」表述以本次定级为准）。
3. **存储（P4）**：SQLite 可通过环境变量切换为 turso 解除本地依赖（`--session-store env:<VAR>` 的 `env:` 解引用已支持），属**安全依赖**。
4. **配置数据面（P3）**：配置文件下沉为一个**配置数据面**——所有外部来源汇总于此，再被其他方消费；数据面自身依赖文件系统，依赖也可以来自 workspace；最小化迁移。`skillsDir` 配置删除**单列为 P1**。
5. **插件体系（P3）**：插件相关单独设计为 **Plugin MCP**（可能落 Workspace 内），替代当前重型依赖。
6. **shell（P1）**：shell 相关（执行与输出落盘等）列为 **P1**——抽象或迁移到工具执行环境；**workflow 与 PTC 是特例**（本地 JS 执行环境自管理，另行处置）。
7. **工具 fs（P1）**：工具能力的文件系统依赖列为 **P1，严查**（逐处审计，不直接引用文件系统）。
8. **TUI 免除**：TUI 相关（客户端本地状态、主题、web-pty 等）**免除定级**。
9. **遥测**：落盘日志是**特例**；metrics 出口**已对齐到 Langfuse**——没有 Langfuse 则不落盘，Langfuse 侧用 **event 类型**承接（2026-09-30 定案，取代此前「落盘也可接受」）。
10. **compact**：不应读取 skill 文件——保留历史工具调用记录即可；同机制的文件回读（recent files）一并评估。`compact_v2/full.rs` 的文件读取应移除。

## 二、全仓库总览

生产代码（排除 `*_test.rs`）文件系统调用点数，模式含 `std::fs` / `tokio::fs` / `dirs_next` / `temp_dir` / `current_dir`：

| crate | 点数 | 主要用途 | 处置方向 |
| --- | --- | --- | --- |
| `peri-acp` | 19 | 全局配置路径（P3）、rewind 文件回退、工作区 canonicalize、插件缓存（P2）、`/etc/os-release` | 宿主控制面收口；canonicalize 见第四节（统一地址 P2） |
| `peri-acp-types` | 0 | 仅 `PathBuf` 类型（契约数据） | 保持；路径类型不是 feature 边界 |
| `peri-agent` | 19 | shell 执行与输出落盘（P1）、遥测、存储桥、compact 文件回读 | shell P1；compact 回读移除；遥测：日志特例 / metrics 仅 Langfuse event（无 Langfuse 不落盘） |
| `peri-controller` | 0 | — | — |
| `peri-middlewares` | 104 | 插件/MCP 管理、settings、图片、attribution、LSP 同步 | P1 工具 fs（严查）+ P2 缓存 + P3 配置数据面与 Plugin MCP |
| `peri-model` | 0 | — | — |
| `peri-process` | 0 | 进程树所有权（OS 依赖，非 fs） | 随 shell feature 处置 |
| `peri-resources` | 32 | SQLite 存储、Filesystem 测试后端、`~/.peri` 配置路径、turso 远端 | **P4** 安全依赖：环境变量切换 turso 即解除本地依赖 |
| `peri-runtime` | 0 | — | — |
| `peri-theme` | 2 | 主题文件读取 | TUI 相关，**免除** |
| `peri-tui` | 85 | 设备间同步、keystore、输入历史、插件 CLI、更新、主题下载 | TUI 相关，**免除**；同步协议随统一地址（P2）演进 |
| `peri-web-pty` | 2 | 终端 cwd、pty I/O | **标记**：TUI 的包，未来可能删除（TUI 相关免除） |
| `peri-workflow` | 24 | workflow engine artifact 安装/发布、journal、脚本 | **特例**（第 3.5 节） |
| `peri-js-runtime` | 23 | PTC（`@peri-code/ptc`）artifact 安装与隔离 | **特例**（第 3.5 节） |
| `langfuse-client` | 0 | — | — |

## 三、分类清单

### 3.1 核心执行（peri-agent）：shell P1 / compact 移除 / 遥测

| 位置 | 用途 | 具体行为 | 处置 |
| --- | --- | --- | --- |
| `agent/async_tasks/shell.rs:325-361` `persist_truncated_output_with_ref` | 工具输出截断保留完整内容供模型后续 Read | `std::env::temp_dir()` + `std::fs::write` → `{temp}/peri-tool-output-{uuid}.txt` | **P1**：移工具执行环境 |
| `agent/async_tasks/shell_output.rs`（整模块） | 后台 shell stdout/stderr durable capture（启动即落盘） | `temp_dir`/`current_dir`、`OpenOptions` create_new+0600、`tokio::fs::File`、`remove_file` | **P1**：文件归工具环境工作区 |
| `agent/async_tasks/shell.rs:487-540` `tee_pipe`/`tee_pipe_with_output` | 管道 tee 到日志文件 | `Option<std::fs::File>`、`write_all` | 同上（P1） |
| `agent/async_tasks/manager.rs:298` | 后台 shell spawn 设定工作目录 | `cmd.current_dir(&cwd)` | 随统一地址（P2，第四节） |
| `agent/compact_v2/full.rs:418-434` `read_file_with_budget` | Full compact 时把 skills 与 recent files 内容读回上下文 | `std::fs::read_to_string` | **移除**（用户裁决）：保留历史工具调用记录即可 |
| `agent/compact_v2/full.rs:455-461` `resolve_path` | 相对路径按 cwd 转绝对 | `Path::join` | 随回读移除 |
| `metric/mod.rs:80-146` | 指标事件落盘 `~/.peri/metrics/{date}.jsonl`（按日滚动，mpsc 解耦） | `dirs_next::home_dir` + `tokio::fs` 追加写 | **已实施（2026-09-30）**：仅经 Langfuse event 上报；无 Langfuse 时丢弃，本地 JSONL 写路径已删除 |
| `telemetry/subscriber.rs:29-92` | 运行日志滚动落盘 `~/.peri/logs` | `dirs_next`、滚动文件 | **特例**：落盘日志按特例处理 |

`persist_truncated_output*` 的下游消费方在工具层：`peri-middlewares/src/mcp/{resource_tool,tool_bridge}.rs`、`mcp-packages/web`、`mcp-packages/workspace`——迁移时随工具环境一并搬走。

### 3.2 宿主控制面（配置数据面 P3 / 插件 / MCP）

方向：配置类文件系统依赖**下沉为配置数据面（P3）**——所有外部来源汇总于此，再被其他方消费；数据面自身依赖文件系统，依赖也可以来自 workspace；此迁移可最小化。**`skillsDir` 配置删除（单列 P1）**，取代 `2026-09-29-workspace-mcp-resources-plan.md` F12「保留为宿主适配器」的旧方案（链路：`peri-acp/src/provider/config.rs:189-190` 定义与别名 → `peri-middlewares/src/settings.rs`、`skills/loader.rs` 消费；同函数读取的 `disableBundledSkills` 未被点名）。插件体系按 **Plugin MCP（P3）** 方向单独设计（可能落 Workspace 内），替代当前重型依赖。

| 位置 | 用途 | 等级 / 处置 |
| --- | --- | --- |
| `peri-acp/src/provider/store.rs`、`provider/config.rs` | 全局配置 `~/.peri/settings.json`（含进程级路径重定向）与配置模型 | **P3** 配置数据面 |
| `peri-middlewares/src/settings.rs` | 全局配置读取适配器（不读技能内容） | **P3**；`skillsDir` 读取删除（单列 **P1**） |
| `peri-middlewares/src/mcp/config.rs` | MCP 服务器配置读取 | **P3** 配置数据面 |
| `peri-middlewares/src/plugin/{config,loader,install_counts}.rs`、`installer/*`、`marketplace/fetch.rs`、`host_ports.rs`（插件端口） | 插件安装 / 卸载 / marketplace / 计数（当前重型依赖） | **P3** Plugin MCP 方向：单独设计（可能落 Workspace 内） |
| `peri-acp/src/host/requests/plugin.rs:512-526` | 插件缓存目录扫描（`read_dir` + manifest 读取） | **P2**：暂不在考虑范围 |
| `peri-middlewares/src/mcp/{auth_store,resource_cache}.rs` | OAuth 凭证落盘、跨进程资源缓存（advisory file lock） | **P2**：暂不在考虑范围 |
| `peri-tui/src/cli_plugin.rs`、`kit/panels/plugin/data.rs` | 插件 CLI 与面板 | **免除**（TUI 相关） |
| `peri-agent/src/resources.rs` | 存储装配桥（`Option<PathBuf>` → `peri-resources`）；默认 `~/.peri/threads/threads.db` | 随存储（P4）与宿主装配收敛 |

### 3.3 工具 fs（P1，严查；未来迁往 mcp-packages）

| 位置 | 用途 | 等级 / 处置 |
| --- | --- | --- |
| `peri-middlewares/src/middleware/image/mod.rs:185-196` | 读取本地图片文件（工具输入） | **P1（严查）**：下沉到 Workspace MCP 中的逻辑，或通过 MCP tool 获取数据；不直接引用文件系统 |
| `peri-middlewares/src/attribution/mod.rs:125-185` | git 归因（读工作区文件内容、diff） | **P1（严查）**：逐处审计；随 `mcp-packages` 迁移 |
| `peri-middlewares/src/lsp/middleware.rs:81` | 读文件文本同步 LSP | **P1（严查）**：逐处审计；随 `mcp-packages` 迁移 |
| `peri-middlewares/src/hooks/loader.rs:357` | hook 路径 canonicalize 比较 | **P1（严查）**：逐处审计；随 `mcp-packages` 迁移 |
| `peri-web-pty/src/{lib.rs,ws_handler/io.rs}` | 终端 cwd、pty I/O | **标记 / 免除**：TUI 的包，未来可能删除 |

### 3.4 客户端本地状态（peri-tui；TUI 相关免除）

| 位置 | 用途 |
| --- | --- |
| `sync/{scanner,packer,sender,receiver,writer}.rs` | 设备间加密同步（r2-encrypted-transfer v1）：扫描配置类文件（`~/.peri/settings.json`、`~/.claude/settings.json`、`~/.claude/skills/`、`~/.claude/plugins/cache/`、`~/.mcp.json` 与 **`{cwd}/.mcp.json`**）打包传输 |
| `sync/channel_flow/staging.rs` | 同步暂存目录 `~/.peri/staging-<channel_hash>`（同卷 rename 提交） |
| `sync/{keystore,device,device_cli}.rs` | 私钥存储（OS keyring → 0600 加密文件）、设备管理 |
| `kit/input_history.rs`、`update.rs`、`kit/panels/theme/download.rs`、`kit/image_safety.rs`、`app/setup_wizard/mod.rs` | 输入历史、版本自更新、主题下载、图片安全、设置向导 |

方向：**TUI 相关免除定级**（用户裁决）；TUI 是本地客户端，本地状态用本地文件合理。但**同步协议当前以文件路径布局为中心**（协议要求 `/` 分隔符，Windows 分隔符陷阱记录在 `sync/scanner.rs` 源码注释），跨设备身份问题见第四节（统一地址 P2）。扫描清单中的 `~/.claude/plugins/cache/` 属 **P2** 插件缓存范围（暂不处理）。

### 3.5 特例：workflow 与 PTC（本地 JS 执行环境）

| 位置 | 内容 |
| --- | --- |
| `peri-workflow/src/runner/artifact.rs`（~20 点） | `~/.peri` 下 npm 包 `@peri-code/workflow` 的版本校验、原子发布、安装与命令准备；`journal.rs` 持久化；`tool.rs`/`tool/preflight.rs`/`cli.rs` 写脚本与 artifact |
| `peri-js-runtime/src/artifact.rs`、`artifact/install.rs`（23 点） | npm 包 `@peri-code/ptc` 的安装、隔离（rename/quarantine）、`~/.peri` 安装目录 |

这两者是"本地 node 执行环境"的自管理（安装/发布/隔离/进程 spawn），其 fs 依赖与 JS 运行时绑定。按用户裁决列为**特例**：不进入本轮定级处理序列，处置方式另行决定（宜随工具执行环境整体迁移）。

### 3.6 存储后端（peri-resources，P4 安全依赖）

- **P4 定级**：本机存储为单文件 SQLite（WAL，默认 `~/.peri/threads/threads.db`），同时已支持 turso 远端后端（`turso://` locator，`sessions/remote/*`）；`--session-store env:<VAR>` 的 `env:` 解引用已支持——把 locator 指向环境变量即可**经环境变量切换**为远端，解除本地文件系统依赖。属**安全依赖**：默认本地可用，只需提及，不列为改造项。
- 远端与本地 schema 逐列一致（canonical 单一事实源）。
- `sessions/filesystem.rs`：`FilesystemThreadStore`——非默认后端，测试用途。
- `config/mod.rs`：`~/.peri`、`~/.peri/settings.json` 路径入口（与配置数据面 P3 相接）。

### 3.7 零 fs 依赖

`peri-model`、`peri-controller`、`peri-runtime`、`langfuse-client`、`peri-process`（进程树/Job Object，OS 非 fs）；`peri-acp-types` 无 fs I/O，仅有 `PathBuf` 类型（skills / hooks / workspace / plugin / session_store / lsp）。

## 四、cwd 与多设备寻址：URI 化（统一地址，P2）

### 4.1 现状与问题

- `cwd` 以 `String` / `Arc<str>` / `&Path` 贯穿：会话固化 `meta.cwd`（子会话继承、重启不漂移）、`middleware/state.rs` 暴露 `cwd()`。
- 契约层 `ResolvedWorkspace { project_id, workspace_id, cwd: PathBuf, root: PathBuf, relative_cwd }`（`peri-acp-types/src/workspace.rs:75`）：身份（ID）与路径（PathBuf）并存，`cwd_relative_to_workspace` 已相对化。
- 执行准入依赖本地 fs 语义：`peri-acp/src/host/workspace.rs:668` 用 `tokio::fs::canonicalize` 比较调用方 cwd 与绑定 cwd；`host/prepared.rs:259` canonicalize 启动目录。
- 设备间同步（`peri-tui/src/sync`）以本地路径定位内容：`~/.claude/...` 固定路径之外，项目级 MCP 配置用 **`{cwd}/.mcp.json`**（同步的输入本身依赖本地 cwd）；会话执行绑定（workspace/cwd）跨设备同步后同样无法直接解析。
- 多设备交织（本地 / 远端 SSH / 云 worker）下，"cwd = 本机路径"不成立：路径在不同设备上可能有、可能无、可能语义不同。

### 4.2 已有基础（URI 化的落点）

- `WorkspaceId` / `ProjectId` 身份层与 `cwd_relative_to_workspace` 相对路径已存在——URI 化只需为"路径"补上"执行环境"维度。
- `SessionStoreLocator`（`peri-acp-types/src/session_store.rs:39`）已区分 `LocalPath(PathBuf)` 与 `Locator(String)`（`turso://` scheme）——**存储寻址已有 scheme 先例**，工作区寻址可复用同一模式。
- 执行所有权/代际（execution lease、read-only admission）已按"宿主"建模，与"执行环境"维度天然兼容。

### 4.3 VS Code 的 URI 设计（参考）

VS Code 把一切资源（文件、编辑器、扩展资源、存储）用 `Uri` 寻址，而不是平台路径：

- **三段结构**：`scheme://authority/path`。`file:` 只是其中一个 scheme；远端 workspace 用 `vscode-remote` 寻址，如 `vscode://vscode-remote/ssh-remote+<host>/<path>`、`vscode://vscode-remote/attached-container+<hex(container-id)>/<path>`（官方未成文，社区 issue `microsoft/vscode-remote-release#8764`）。
- **FileSystemProvider**：扩展按 scheme 注册文件系统 provider；核心（`vscode.workspace.fs`）只按 URI 路由读写，不感知资源在本地还是远端。
- **执行位置由扩展种类决定**：UI Extensions 常驻本地，Workspace Extensions 运行在 workspace 所在机器（远端 VS Code Server）；命令调用自动路由到正确一侧。
- **存储也用 URI**：`context.storageUri` / `globalStorageUri` 替代 `~/.vscode` 之类的固定路径。

来源：`code.visualstudio.com/api/advanced-topics/remote-extensions`；`microsoft/vscode-remote-release#8764`。

### 4.4 映射建议（提案，未实施）

- **契约先行**：为"工作区/执行目录"引入 URI 形态的标识（如 `WorkspaceUri { scheme, authority, path }`，与 `SessionStoreLocator` 同族）：
  - 本机：`file:///Users/…/project`
  - 远端设备：`remote+ssh://build-host/srv/project`
  - 云执行环境：`sandbox://worker-abc/workspace/project`
- **执行环境解析归宿主**：URI 到具体设备/进程的解析属 Host/Deployment 装配；Agent 核心只见 URI 与相对路径（`workspace_id` + `cwd_relative_to_workspace` 已是身份锚点）。
- **canonicalize 语义改造**：仅在同一 `scheme+authority` 内做路径规范化比较；跨环境用 `workspace_id` 重解析并对齐，不在本地直接 canonicalize 远端路径。
- **设备间同步**：同步"身份 + 相对路径 + 内容"，绝对路径由各设备解析（当前同步协议以文件布局为中心，这是关键差异点）。
- 渐进落点：1）契约类型与序列化先行；2）`host/workspace.rs::expect_directory` 与 `host/prepared.rs` 改造；3）`Session::cwd` 与子会话继承切换到 URI 语义；4）设备同步协议演进。

## 五、用户等级表

### 5.1 等级表（P1–P4，按用户裁决）

| 等级 | 项 | 现状位置 | 处置 |
| --- | --- | --- | --- |
| **P1** | shell 相关（执行与输出落盘） | 第 3.1 节（`shell.rs` / `shell_output.rs` / tee / manager cwd） | 抽象或迁往工具执行环境（`mcp-packages` 方向） |
| **P1** | 工具 fs（工具能力文件系统依赖） | `middleware/image`、`attribution`、`lsp/middleware`、`hooks/loader` | **严查**：逐处审计；图片下沉 Workspace MCP 或经 MCP tool；随 `mcp-packages` 迁移 |
| **P1** | `skillsDir` 配置链路 | `peri-acp/src/provider/config.rs:189-190` → `peri-middlewares/src/settings.rs`、`skills/loader.rs` | 删除（取代 workspace-mcp 计划 F12 宿主适配器方案） |
| **P2** | cwd / 多设备统一地址（URI 化） | 第四节 | 统一地址方案；大工程，按 P2 推进 |
| **P2** | MCP / 插件缓存 | `peri-middlewares/src/mcp/{auth_store,resource_cache}.rs`、`peri-acp/src/host/requests/plugin.rs:512-526`（另：3.4 同步扫描的 `~/.claude/plugins/cache/` 项） | 暂时标记 P2，**不在考虑范围内**（TUI 侧插件 CLI / 面板归 TUI 免除） |
| **P3** | 配置数据面 | `peri-acp/src/provider/{store,config}.rs`、`peri-middlewares/src/settings.rs`、`mcp/config.rs` 等配置外部来源 | 下沉为**配置数据面**：所有外部来源汇总于此、被其他地方消费；数据面自身依赖文件系统，依赖也可以来自 workspace；**最小化迁移**（`skillsDir` 已单列 P1） |
| **P3** | 插件体系 → **Plugin MCP** | `peri-middlewares/src/plugin/*`、`installer/*`、`marketplace/fetch.rs`、`host_ports.rs` | 单独设计为 Plugin MCP（可能落 Workspace 内），替代当前重型依赖 |
| **P4** | 存储后端（本机 SQLite 文件） | `peri-resources/src/sessions/sqlite_store.rs`（默认 `~/.peri/threads/threads.db`）等 | **安全依赖**：经环境变量切换 turso 即解除本地依赖（`--session-store env:<VAR>` 已支持）；只需提及 |

### 5.2 等级外裁决

| 裁决 | 项 | 处置 |
| --- | --- | --- |
| 免除 | TUI 相关（`peri-tui` 本地状态与插件 CLI / 面板、`peri-theme`、`peri-web-pty` 已标记可能删除） | 本地客户端，**免除定级**；同步协议随统一地址（P2）演进 |
| 特例 | workflow / PTC artifact 管理 | 本地 JS 执行环境自管理，另行处置（宜随工具执行环境整体迁移） |
| 特例 | 落盘日志（`~/.peri/logs` 滚动日志） | 按特例处理，不列为改造项（`telemetry/subscriber.rs`） |
| 移除 | metrics 本地落盘（`~/.peri/metrics/*.jsonl`；出口改 Langfuse event） | 已实施（2026-09-30）：指标只经宿主装配注入的 Langfuse 出口上报，无 Langfuse 时不落盘（`metrics/mod.rs`）；Langfuse 承接见 `peri-controller/src/langfuse/metric_sink.rs` |
| 移除 | compact 文件回读（skills / recent files） | 移除，保留历史工具调用记录（`compact_v2/full.rs`） |

### 5.3 P1 实施与验收（2026-09-30）

本轮只处理等级表中的三项 P1。四个实现方向并行，公共装配、依赖与文档统一集成；P2 URI/缓存、P3 配置数据面/Plugin MCP、P4 存储和等级外 compact 回读不扩入本轮。

| 方向 | 边界与验收 | 状态 |
| --- | --- | --- |
| Shell 执行与输出 | 计算层保留任务生命周期与 `ShellExecutor` 注入契约；进程、tee、完整输出持久化归 `peri-mcp-common::{shell,shell_executor,shell_output}`。本地装配使用 `create_local_task_manager()`，无执行环境的 `TaskManager::new()` 不静默执行本机 shell。 | 本地执行边界完成；远端输出存储另见限界 |
| 图片附件 | `ImageMiddleware` 通过当前会话可见的 builtin workspace `image/read` 读取；格式/大小校验归 provider。不新增模型工具，保留消息身份、原内容块、压缩与附件失败反馈；关闭/断连/换代不回落宿主 fs。 | 完成；provider 8、middleware 15 条定向测试通过 |
| Attribution / LSP / hook 路径 | 逐处分类与迁移。归因保留写前/写后内容；LSP 保留 ready gate 与 change/save 顺序；hook 用户来源排除保留 symlink 同文件语义，不降级为字面路径比较。配置正文读取仍属 P3。 | 实施中 |
| `skillsDir` | 删除配置字段、别名、读取与全局自定义根装配；保留 user/project/plugin/builtin 来源及 `disableBundledSkills`，插件路径不再由宿主预过滤。F12 旧方案已更新。 | 完成；ACP 2、根解析 1、开关 6 条定向测试通过 |

验收以定向契约/回归测试、受影响 crate 编译与依赖门为主，不以全库测试数量或静态函数搬移证明完成。完成后在此登记实际命令和仍未验证的部署场景。

实施限界：

- Shell 的本地执行实现已从计算层移出，Agent 不再依赖 `peri-process`，`libc` 仅留测试。宿主 MCP bridge/resource 的截断仍调用 common helper 在**宿主进程**落盘；本轮没有实现远端输出存储、跨机器 Read 寻址，也不宣称计算/工具可直接跨设备部署。
- hook loader 的 canonicalize 实际用于配置来源去重，避免用户 settings 经 symlink 被项目级再次加载，不是工具正文读取。按逐处审计结果归 P3 配置控制面保留，补 alias 回归；不能为了消除调用点降级为字面路径比较。
- `AppConfig::extra` 原有未知字段透传规则不变：旧 `skillsDir` / `skills_dir` 可能作为不被消费的 opaque JSON 保留，但不再是配置字段、没有 loader 或资源根效果；新默认序列化不生成这些键。不增加旧键清洗 shim。
- Workspace 路径仍是本地工具环境路径，不构成 cwd 沙箱；绝对路径/symlink 沿用既有行为。图片沿用 MIME 签名判断，不做完整解码；取消请求不保证 blocking I/O 立即结束。
- 未跑全库/E2E、Windows 原生执行或 120 秒 ignored 用例；无 Langfuse 凭据的其他并行任务验证不作为本轮 P1 证据。

Shell 定向证据：Agent 49、common 24、terminal 53（另 1 ignored）、MCP wire 3、Web 截断 4 条通过；相关 doc tests 11 条通过（另 2 ignored）。图片 doc-test 筛选命中 0，不计行为覆盖。`git diff --check` 与依赖门通过（20 条规则，0 违规）。

## 六、Cargo 依赖与清理

| 项 | 现状 | 建议 |
| --- | --- | --- |
| `peri-agent` → `sqlx` | 直接依赖已删除（2026-09-30 实施；源码与测试原无引用）；传递依赖 `sqlx` 仍经 `peri-resources` 存在，存储实现保持不动 | 已完成；命令与结果见第七节 |
| `dirs-next` | 分散于 peri-agent、peri-resources、peri-tui、peri-workflow、peri-js-runtime、peri-acp | 可收敛为宿主注入的目录参数；低优先 |
| `peri-agent` → `peri-resources` | 仅 `resources.rs` 声明边 | 可选项化/由宿主编排（feature 或纯注入） |
| `tempfile`(dev) | 测试夹具 | 保留 |
| `peri-resources` 传递 | `sqlx`（SQLite 驱动）、`tokio::fs`（FilesystemThreadStore）、`turso_serverless`（远端）、`dirs-next` | turso 远端为既有能力，提及即可 |

## 七、验证与未验证

已验证（静态核对，2026-09-30 快照）：

- 各 crate 生产代码 fs 调用点经多模式交叉搜索（`std::fs` / `tokio::fs` / `dirs_next` / `temp_dir` / `current_dir`）聚合统计；行号为快照值
- `peri-agent` → `sqlx` 无任何引用；`peri-acp-types` / `peri-model` / `peri-controller` / `peri-runtime` / `langfuse-client` 无 fs I/O
- `peri-agent` → `sqlx` 直接依赖清理（2026-09-30 实施后验证，非快照）：`cargo check -p peri-agent` 通过；`cargo tree -p peri-agent -i sqlx` 仅剩 `peri-resources` 路径，`--depth 1` 顶层无 `sqlx`；`peri-agent` 源码与测试复核无 `sqlx` 引用
- metrics 出口对齐 Langfuse（2026-09-30 实施后验证，非快照）：`cargo test -p peri-agent --lib metrics::`（10 passed：未安装出口丢弃、已安装出口投递 + 500 字符截断 + 身份透传）、`cargo test -p peri-controller --lib langfuse::metric_sink`（4 passed：`event-create` wire 类型、payload/level/身份 metadata、背压丢弃记账）、`cargo check -p peri-acp --lib`（安装点编译通过）、`cargo clippy -p peri-agent -p peri-controller --all-targets` 无告警、`bash scripts/check-layer-imports.sh` 通过；`src/metrics/mod.rs` 已无 `tokio::fs` / `dirs_next` / 文件路径写入
- metrics 事件 trace 归属（2026-09-30 已裁决：挂到当前 turn 的 trace；同日实施后验证，非快照）：`turn_traces.rs` 注册表在 `on_turn_start` 登记 sid→本 turn trace、`on_turn_end` compare-and-remove 清理，`LangfuseMetricsSink` 按指标自带 `sid` 取活跃 trace 归属；无活跃 trace（未开始 / 已结束 / 无 sid）时回退独立 root trace，事件不丢。证据：`cargo test -p peri-controller --lib langfuse::`（129 passed，含注册/清理/多 sid 隔离/回退用例）、`cargo test -p peri-agent --lib metrics::`（10 passed）、`cargo clippy -p peri-agent -p peri-controller --all-targets` 无告警、`bash scripts/check-layer-imports.sh` 通过、改动文件 `rustfmt --edition 2021 --check` 通过
- `persist_truncated_output*`、`init_tracing`、`metrics::emit` 的下游消费面
- VS Code URI 设计要点取自官方文档与社区 issue（链接见 4.3）

未验证：

- 未做编译期 feature 拆分实验
- 未核对 `peri-tui/src/sync` 协议全量字段（仅扫描 fs 触点与 staging 语义）
- 未评估"shell 输出落盘迁往 mcp-packages"的具体接口形态（属后续设计）
- 各等级项（P1 shell / 工具 fs 严查 / `skillsDir`、P2 统一地址、P3 配置数据面与 Plugin MCP、P4 切换）未做实施拆解与工作量评估；等级表是方向记录
- cwd / 统一地址（P2）仅到方向层面；未形成 spec 契约
- metrics → Langfuse 未做端到端上报验证（本地无 Langfuse 凭据，未观察 Langfuse 服务端落库；含指标归属到活跃 turn trace 的服务端表现）
