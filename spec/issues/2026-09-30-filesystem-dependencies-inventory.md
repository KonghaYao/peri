# Peri 仓库文件系统依赖清单

状态：待办清单；已处理项在 5.3 各保留一行，未完成项与未验证内容保留，历史过程及测试明细查 Git。

- 范围：整个仓库的 Rust crates（`peri-agent` 即原 `peri-core`，已于 2026-09 更名）；**排除 `mcp-packages`**——它们本身就是文件系统工具环境，是未来文件系统耦合的落点，不是本清单的对象
- 关联：`2026-09-27-v4-cloud-architecture-audit.md`（存算分离方向）、`peri-agent/src/lib.rs`（计划引入 feature gates）

## 一、用户裁决（本文方向锚点）

1. **范围与落点**：文件系统耦合的清单覆盖全仓库；`mcp-packages` 除外，且**是未来耦合的移动目标**（工具执行环境）。
2. **cwd / 统一地址（P2）**：cwd 与多设备统一寻址（URI 化，第四节）列为 **P2**；仍是大工程（此前「单独立项」表述以本次定级为准）。
3. **存储（无需整改）**：SQLite 与 turso 后端及 `--session-store env:<VAR>` 已支持，属**安全依赖**，不列入待办等级表。
4. **配置数据面（P3）**：配置文件下沉为一个**配置数据面**——所有外部来源汇总于此，再被其他方消费；数据面自身依赖文件系统，依赖也可以来自 workspace；最小化迁移。
5. **插件体系（P3）**：插件相关单独设计为 **Plugin MCP**（可能落 Workspace 内），替代当前重型依赖。
6. **shell（P1）**：执行与输出持久化归工具执行环境；**workflow 与 PTC 是特例**（本地 JS 执行环境自管理，另行处置）。
7. **工具 fs（P1）**：本清单中的工具执行依赖已下沉，不代表 P2/P3 配置、地址与缓存也已迁移。
8. **TUI 免除**：TUI 相关（客户端本地状态、主题、web-pty 等）**免除定级**。
9. **遥测**：落盘日志是**特例**；Langfuse 服务端上报效果仍需端到端验证。
10. **compact**：不应读取 skill 文件——保留历史工具调用记录即可；同机制的文件回读（recent files）一并评估。`compact_v2/full.rs` 的文件读取应移除。

## 二、全仓库总览

未改动 crate 的点数保留 2026-09-30 扫描快照；已改动 crate 标为「未重统计」，不把旧计数冒充现状（模式含 `std::fs` / `tokio::fs` / `dirs_next` / `temp_dir` / `current_dir`，排除 `*_test.rs`）：

| crate | 点数 | 主要用途 | 处置方向 |
| --- | --- | --- | --- |
| `peri-acp` | 未重统计 | 全局配置路径（P3）、rewind 文件回退、工作区 canonicalize、插件缓存（P2）、`/etc/os-release` | 宿主控制面收口；canonicalize 见第四节（统一地址 P2） |
| `peri-acp-types` | 0 | 仅 `PathBuf` 类型（契约数据） | 保持；路径类型不是 feature 边界 |
| `peri-agent` | 未重统计 | 存储桥、compact 文件回读、日志 | compact 回读移除；存储无需整改；日志特例 |
| `peri-controller` | 0 | — | — |
| `peri-middlewares` | 未重统计 | 插件/MCP 管理、settings、MCP 执行环境适配 | P2 缓存 + P3 配置数据面与 Plugin MCP |
| `peri-model` | 0 | — | — |
| `peri-process` | 0 | 进程树所有权（OS 依赖，非 fs） | 随 shell feature 处置 |
| `peri-resources` | 32 | SQLite 存储、Filesystem 测试后端、`~/.peri` 配置路径、turso 远端 | 存储安全依赖无需整改；配置路径随 P3 配置数据面处置 |
| `peri-runtime` | 0 | — | — |
| `peri-theme` | 2 | 主题文件读取 | TUI 相关，**免除** |
| `peri-tui` | 85 | 设备间同步、keystore、输入历史、插件 CLI、更新、主题下载 | TUI 相关，**免除**；同步协议随统一地址（P2）演进 |
| `peri-web-pty` | 2 | 终端 cwd、pty I/O | **标记**：TUI 的包，未来可能删除（TUI 相关免除） |
| `peri-workflow` | 24 | workflow engine artifact 安装/发布、journal、脚本 | **特例**（第 3.5 节） |
| `peri-js-runtime` | 23 | PTC（`@peri-code/ptc`）artifact 安装与隔离 | **特例**（第 3.5 节） |
| `langfuse-client` | 0 | — | — |

## 三、分类清单

### 3.1 剩余执行依赖 / compact 移除 / 日志

| 位置 | 用途 | 具体行为 | 处置 |
| --- | --- | --- | --- |
| `mcp-packages/common/src/shell_executor.rs` | 后台 shell 执行目录 | 执行环境仍用本地 cwd 字符串 | 随统一地址（P2，第四节） |
| `agent/compact_v2/full.rs:418-434` `read_file_with_budget` | Full compact 时把 skills 与 recent files 内容读回上下文 | `std::fs::read_to_string` | **移除**（用户裁决）：保留历史工具调用记录即可 |
| `agent/compact_v2/full.rs:455-461` `resolve_path` | 相对路径按 cwd 转绝对 | `Path::join` | 随回读移除 |
| `telemetry/subscriber.rs:29-92` | 运行日志滚动落盘 `~/.peri/logs` | `dirs_next`、滚动文件 | **特例**：落盘日志按特例处理 |

### 3.2 宿主控制面（配置数据面 P3 / 插件 / MCP）

方向：配置类文件系统依赖**下沉为配置数据面（P3）**——所有外部来源汇总于此，再被其他方消费；数据面自身依赖文件系统，依赖也可以来自 workspace；此迁移可最小化。插件体系按 **Plugin MCP（P3）** 方向单独设计（可能落 Workspace 内），替代当前重型依赖。

| 位置 | 用途 | 等级 / 处置 |
| --- | --- | --- |
| `peri-acp/src/provider/store.rs`、`provider/config.rs` | 全局配置 `~/.peri/settings.json`（含进程级路径重定向）与配置模型 | **P3** 配置数据面 |
| `peri-middlewares/src/settings.rs` | 全局 `disableBundledSkills` 配置读取适配器（不读技能内容） | **P3** 配置数据面 |
| `peri-middlewares/src/hooks/loader.rs` | hook 配置读取与 canonical 来源去重 | **P3**：保留 symlink 同文件判定，避免用户 settings 被当作项目来源重复加载；随配置数据面处置 |
| `peri-middlewares/src/mcp/config.rs` | MCP 服务器配置读取 | **P3** 配置数据面 |
| `peri-middlewares/src/plugin/{config,loader,install_counts}.rs`、`installer/*`、`marketplace/fetch.rs`、`host_ports.rs`（插件端口） | 插件安装 / 卸载 / marketplace / 计数（当前重型依赖） | **P3** Plugin MCP 方向：单独设计（可能落 Workspace 内） |
| `peri-acp/src/host/requests/plugin.rs:512-526` | 插件缓存目录扫描（`read_dir` + manifest 读取） | **P2**：暂不在考虑范围 |
| `peri-middlewares/src/mcp/{auth_store,resource_cache}.rs` | OAuth 凭证落盘、跨进程资源缓存（advisory file lock） | **P2**：暂不在考虑范围 |
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

方向：**TUI 相关免除定级**（用户裁决）；TUI 是本地客户端，本地状态用本地文件合理。但**同步协议当前以文件路径布局为中心**（协议要求 `/` 分隔符，Windows 分隔符陷阱记录在 `sync/scanner.rs` 源码注释），跨设备身份问题见第四节（统一地址 P2）。扫描清单中的 `~/.claude/plugins/cache/` 属 **P2** 插件缓存范围（暂不处理）。

### 3.5 特例：workflow 与 PTC（本地 JS 执行环境）

| 位置 | 内容 |
| --- | --- |
| `peri-workflow/src/runner/artifact.rs`（~20 点） | `~/.peri` 下 npm 包 `@peri-code/workflow` 的版本校验、原子发布、安装与命令准备；`journal.rs` 持久化；`tool.rs`/`tool/preflight.rs`/`cli.rs` 写脚本与 artifact |
| `peri-js-runtime/src/artifact.rs`、`artifact/install.rs`（23 点） | npm 包 `@peri-code/ptc` 的安装、隔离（rename/quarantine）、`~/.peri` 安装目录 |

这两者是"本地 node 执行环境"的自管理（安装/发布/隔离/进程 spawn），其 fs 依赖与 JS 运行时绑定。按用户裁决列为**特例**：不进入本轮定级处理序列，处置方式另行决定（宜随工具执行环境整体迁移）。

### 3.6 存储后端（peri-resources，安全依赖，无需整改）

- **无需整改**：本机 SQLite 与 turso 远端及 `--session-store env:<VAR>` 已支持，存储属于安全依赖，不列为改造项。
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

### 5.1 待办等级表（仅保留未完成且需要整改的项）

| 等级 | 项 | 现状位置 | 处置 |
| --- | --- | --- | --- |
| **P2** | cwd / 多设备统一地址（URI 化） | 第四节 | 统一地址方案；大工程，按 P2 推进 |
| **P2** | MCP / 插件缓存 | `peri-middlewares/src/mcp/{auth_store,resource_cache}.rs`、`peri-acp/src/host/requests/plugin.rs:512-526`（另：3.4 同步扫描的 `~/.claude/plugins/cache/` 项） | 暂时标记 P2，**不在考虑范围内**（TUI 侧插件 CLI / 面板归 TUI 免除） |
| **P3** | 配置数据面 | `peri-acp/src/provider/{store,config}.rs`、`peri-middlewares/src/settings.rs`、`mcp/config.rs`、`hooks/loader.rs` 等配置外部来源 | **未完成**：配置仍在宿主多处读写；需汇总为配置数据面供其他方消费，数据面自身可依赖文件系统或 workspace，最小化迁移 |
| **P3** | 插件体系 → **Plugin MCP** | `peri-middlewares/src/plugin/*`、`installer/*`、`marketplace/fetch.rs`、`host_ports.rs` | 单独设计为 Plugin MCP（可能落 Workspace 内），替代当前重型依赖 |

### 5.2 等级外裁决

| 裁决 | 项 | 处置 |
| --- | --- | --- |
| 无需整改 | 存储后端（本机 SQLite / turso） | 既有后端切换能力满足本清单要求，安全依赖，不列入待办等级表 |
| 免除 | TUI 相关（`peri-tui` 本地状态与插件 CLI / 面板、`peri-theme`、`peri-web-pty` 已标记可能删除） | 本地客户端，**免除定级**；同步协议随统一地址（P2）演进 |
| 特例 | workflow / PTC artifact 管理 | 本地 JS 执行环境自管理，另行处置（宜随工具执行环境整体迁移） |
| 特例 | 落盘日志（`~/.peri/logs` 滚动日志） | 按特例处理，不列为改造项（`telemetry/subscriber.rs`） |
| 移除 | compact 文件回读（skills / recent files） | 移除，保留历史工具调用记录（`compact_v2/full.rs`） |

### 5.3 已处理（每项一行）

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

- 输出资源 URI 绑定当前 Workspace 实例，实例关闭/重建后不保证旧 URI 可读；输出文件保留于工具环境，跨实例恢复与统一地址仍属 P2。
- Workspace 仍使用本地路径，不构成 cwd 沙箱；跨设备 URI 方案未实施，绝对路径/symlink 行为保持现状。
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
- P2 统一地址、P3 配置数据面与 Plugin MCP 未做本轮实施拆解与工作量评估
- cwd / 统一地址（P2）仅到方向层面；未形成 spec 契约
- metrics → Langfuse 未做端到端上报验证（本地无 Langfuse 凭据，未观察 Langfuse 服务端落库；含指标归属到活跃 turn trace 的服务端表现）
