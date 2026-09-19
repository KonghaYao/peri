# 跨平台与受限环境兼容性待办

**状态**：Open

**关联 P0**：[文件系统身份与 Git 探测阻断会话](2026-09-17-p0-workspace-validation-blocks-input.md)。首次发送、inode 身份门槛、无 Git 普通目录、`git init` 后登记模式冲突和重关联缺口由该 issue 跟踪整体简化；本文件继续保留独立兼容性待办。原「首次发送的准备阶段计入 10 秒回执期限」已修复（P0 修复记录第 6 条：准备与已发出请求的回执各自计时，`PREPARE_TIMEOUT` 独立、慢准备仍被受理），本表不再列出。

**范围**：TUI → ACP 启动与输入、工作区发现和 SQLite、配置保存、Plugin 子进程、PTC / Workflow artifact。依据本地代码及两个独立 subagent 审计交叉核对；这是有界审计，不能代替全部平台验收。

用户要求移除工作区身份对目录创建时间的依赖，并检查体系兼容性。创建时间修复的现行契约见 [工作区身份设计](../../docs/design/session-workspace-identity.md)。本文件只保留后续需要实施或验证的项目。

## 优先处理

| 项目 | 证据与触发条件 | 影响与待办 |
| --- | --- | --- |
| Windows artifact 只读取 HOME | `peri-js-runtime/src/artifact.rs::NpmArtifactProvider` 与 `peri-workflow/src/runner/artifact.rs::workflow_prefix`，原生 Windows 中调用默认 provider / prefix 且未由上层注入 HOME 时，仅设置 USERPROFILE 无法获得安装目录。代码可确认此分支；未运行 Windows | 使用统一的跨平台 home 解析，覆盖未设置 HOME、USERPROFILE 有效的原生 Windows 环境；先确认后续 artifact 安装与执行均成功 |
| Plugin URL 安装缺少取消与等待上限 | `peri-middlewares/src/plugin/installer/install.rs::install_plugin` 的 `spawn_blocking` 内同步 `git.output()`，没有 timeout 或进程 owner | 网络/认证挂起会让任务及 git 持续存在。按共享进程生命周期管理取消、杀树及 wait，增加隔离假 git 的挂起/取消测试 |
| Marketplace 超时没有回收子进程 | `peri-middlewares/src/plugin/marketplace/fetch.rs` 对 git/npm `Command::output` 包 timeout，没有 `kill_on_drop` 或 ProcessTree | 超时返回不能证明进程已停止。增加终止与 wait，验收超时后子进程及后代均退出 |

## 依赖与数据边界

- **Git 是普通目录启动的隐式依赖。** `peri-resources/src/sessions/sqlite_store/discovery.rs::git` 曾在找不到 Git 时返回 discovery error，即使目录本来不是仓库也无法发现工作区。2026-09-19 已改为：spawn 报 `NotFound` 时按目录模式降级并记 `git_answered = false`——这种不完整观测不得改写已登记的 Git 布局；其余 spawn 失败（如权限拒绝）仍报类型化错误，不能当作非仓库（P0 修复记录第 2 条，端到端见 `e2e/tests/scenarios/workspace-no-git.test.ts`）。仍缺最小 Linux 环境（有 Git / 无 Git 的普通目录与仓库目录）验收与面向用户的安装依赖提示。
- **旧 Git 命令能力未覆盖。** `discovery.rs` 曾使用 `rev-parse --path-format=absolute`（上游文档记为 Git 2.31 引入），旧版 Git 会以用法错误退出并使发现失败。2026-09-19 已改为只用更早版本也认得的选项（见 P0 修复记录第 7、11 条）：位置解析只请求 `--show-toplevel --git-dir` 并按 cwd 还原相对输出，common directory 改由 Git 自己写入的 `commondir` 文件推导（不请求 Git 2.5 引入的 `--git-common-dir`），`worktree list` 在不支持 `-z` 时退回换行分隔、子命令整体缺失时跳过成员交叉核对，未知选项回显按不兼容处理。第 13 条已用真实 Git 2.4.12 二进制验收：仓库、普通目录、子目录、现代 Git 建的 linked worktree 四种组合都能建会话；实测同时暴露并修复了一处大小写敏感的分类缺陷（旧版 `fatal: Not a git repository…` 被误判成类型化发现错误，普通目录在那个版本上完全建不了会话）。2.4.12 是已验证版本，不是最低支持版本——更早版本仍未测。
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
- 当前 schema 的损坏表结构、原生 Windows、慢 host 与真实 A800 尚未完成系统验收（旧 Git 已由 P0 修复记录第 13 条用真实 2.4.12 二进制验收）。
- 首次输入的准备期限现为 60 秒常量（P0 修复记录第 6 条），是避免无期限等待的取舍而非测量结果；慢 host 上工作区发现 / 建会话 / operation gate 各段耗时占比与超时发生率仍需实测。
- 审计已排除一项误报：Workflow preflight 实际设置了 `kill_on_drop(true)`，不能报告为“取消后完全不杀子进程”。是否需要有界执行与整棵进程树契约应另按验证器实际行为评估。

## 验收原则

- 每项修复提供能触发原行为的测试，使用临时 HOME / 数据库 / 工具 fixture，不修改真实用户状态。
- 平台测试与静态推断分别记录；macOS 通过不能代替 Windows / Linux 通过。
- 补齐对应 code-index / 现行设计事实源；全部待办关闭后删除本过程文档，历史由 Git 保留。
