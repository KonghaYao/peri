# 跨平台与受限环境兼容性待办

**状态**：Open

**关联 P0**：[文件系统身份与 Git 探测阻断会话](2026-09-17-p0-workspace-validation-blocks-input.md)。首次发送、inode 身份门槛、无 Git 普通目录、`git init` 后登记模式冲突和重关联缺口由该 issue 跟踪整体简化；本文件继续保留独立兼容性待办。

**范围**：TUI → ACP 启动与输入、工作区发现和 SQLite、配置保存、Plugin 子进程、PTC / Workflow artifact。依据本地代码及两个独立 subagent 审计交叉核对；这是有界审计，不能代替全部平台验收。

用户要求移除工作区身份对目录创建时间的依赖，并检查体系兼容性。创建时间修复的现行契约见 [工作区身份设计](../../docs/design/session-workspace-identity.md)。本文件只保留后续需要实施或验证的项目。

## 优先处理

| 项目 | 证据与触发条件 | 影响与待办 |
| --- | --- | --- |
| 首次发送的准备阶段计入 10 秒回执期限 | `peri-tui/src/kit/steer_consumer.rs::spawn_steer_consumer` 的 timeout 覆盖整个 `execute`，其中还要 `ensure_session`、等待 operation gate 和 snapshot。代码可确认计时边界；尚未注入慢启动复现 | 慢 host 可能在 enqueue RPC 之前拒绝输入。分开准备阶段与已发送请求的期限，保留取消与入队不确定状态语义；用持 gate 的确定性回归证明没有丢稿或重复入队 |
| Windows artifact 只读取 HOME | `peri-js-runtime/src/artifact.rs::NpmArtifactProvider` 与 `peri-workflow/src/runner/artifact.rs::workflow_prefix`，原生 Windows 中调用默认 provider / prefix 且未由上层注入 HOME 时，仅设置 USERPROFILE 无法获得安装目录。代码可确认此分支；未运行 Windows | 使用统一的跨平台 home 解析，覆盖未设置 HOME、USERPROFILE 有效的原生 Windows 环境；先确认后续 artifact 安装与执行均成功 |
| Plugin URL 安装缺少取消与等待上限 | `peri-middlewares/src/plugin/installer/install.rs::install_plugin` 的 `spawn_blocking` 内同步 `git.output()`，没有 timeout 或进程 owner | 网络/认证挂起会让任务及 git 持续存在。按共享进程生命周期管理取消、杀树及 wait，增加隔离假 git 的挂起/取消测试 |
| Marketplace 超时没有回收子进程 | `peri-middlewares/src/plugin/marketplace/fetch.rs` 对 git/npm `Command::output` 包 timeout，没有 `kill_on_drop` 或 ProcessTree | 超时返回不能证明进程已停止。增加终止与 wait，验收超时后子进程及后代均退出 |

## 依赖与数据边界

- **Git 是普通目录启动的隐式依赖。** `peri-resources/src/sessions/sqlite_store/discovery.rs::git` 在找不到 Git 时返回 discovery error，即使最终可能是非 Git 目录也无法发现工作区。先明确安装依赖和可用性提示；不能简单把所有 `NotFound` 当作非 Git 仓库，否则会改变真实仓库身份。验收最小 Linux 环境中有 Git / 无 Git 的普通目录与仓库目录。
- **旧 Git 命令能力未覆盖。** `discovery.rs` 使用 `rev-parse --path-format=absolute`。尚未运行旧 Git，不能宣称具体最低版本或已复现失败。需确定支持版本并以旧版本 fixture 验证，或使用有契约的兼容发现路径。
- **配置临时文件名冲突。** `peri-acp/src/provider/store.rs::save_to` 固定使用 `settings.json.tmp`。多个进程同时保存同一路径可能互相覆盖临时内容或 rename 失败；需核对跨进程写入契约并做并发测试，不能仅凭原子 rename 宣称并发安全。
- **Plugin 的非 UTF-8 路径 panic。** URL 安装对 `cache_dir.to_str().unwrap()`；Unix 下非 UTF-8 插件缓存路径会触发 panic（发生在 blocking task，join 层会转为安装错误，不能称整个应用必然崩溃）。改用 Path / OsStr 参数并补路径回归。
- **Windows npm 入口差异。** Workflow 直接调用 `npm` / `npx`，PTC 已区分 `npm.cmd` / `npx.cmd`。这是需要 Windows 实测的条件风险，检查真实命令解析后再决定统一入口，避免未经验证宣称 CreateProcess 一定失败。

## 明确限制与未验证项

2026-09-17 第二轮 subagent 检查补充（主 agent 已核对相关代码；以下尚未完成场景实测）：

- **stdio 路径契约不一致。** `peri-acp/src/host/stdio/mod.rs::assemble_stdio_config` 对 canonicalize 结果使用 `to_string_lossy`，资源层则明确拒绝非 UTF-8。即使入口是 UTF-8 字符串，symlink 目标仍可能包含非 UTF-8 字节。需要验证这种情况下应明确报错而非替换字符后继续装配配置。
- **配置替换可能改变权限。** `peri-acp/src/provider/store.rs::save_to`、`peri-tui/src/sync/writer.rs` 使用默认权限创建临时文件再 rename，没有继承已有目标的权限。目标原为 `0600`、临时文件不存在且 umask 允许组/其他用户读时，替换后的模式可能更宽；是否实际可被其他用户读取还取决于父目录权限/ACL。应以隔离配置文件验证。`FilesystemThreadStore` 仅用于测试，不把它作为生产存储漏洞证据。
- **Windows worktree 路径比较待验收。** `discovery.rs::discover` 使用 `Path` 相等比较 Git membership，`git_command_path` 会处理 verbatim 前缀，但不能据此证明大小写与所有 UNC 表示都一致。需要原生 Windows fixture；不得无条件小写所有路径，因为文件系统可能启用大小写敏感。

- 工作区路径目前要求 UTF-8，ACP 字符串边界也受此约束；这是明确限制。若扩展支持，先设计无损路径契约，不能以有损转换造成身份混淆。
- SSH / tmux 下 Kitty 图形关闭属于已有降级策略；远程剪贴板、无 DISPLAY/Wayland、Unicode 和终端 resize 仍需组合实测。
- 当前 schema 的损坏表结构、原生 Windows、旧 Git、慢 host 与真实 A800 尚未完成系统验收。
- 审计已排除一项误报：Workflow preflight 实际设置了 `kill_on_drop(true)`，不能报告为“取消后完全不杀子进程”。是否需要有界执行与整棵进程树契约应另按验证器实际行为评估。

## 验收原则

- 每项修复提供能触发原行为的测试，使用临时 HOME / 数据库 / 工具 fixture，不修改真实用户状态。
- 平台测试与静态推断分别记录；macOS 通过不能代替 Windows / Linux 通过。
- 补齐对应 code-index / 现行设计事实源；全部待办关闭后删除本过程文档，历史由 Git 保留。
