# P0：文件系统身份与 Git 探测阻断会话创建与发送

**状态**：Open（登记模式冲突、无 Git 目录建会话、准入探测成本与目录搬迁 / 替换的登记可用性已于 2026-09-19 修复；简化目标其余项与其余验收项待实施）
**优先级**：P0（用户指定；2026-09-19 依据本机确证由 P1 升级）
**类型**：可用性缺陷 / 设计简化
**创建日期**：2026-09-17
**检查范围**：2026-09-17 审计创建时间依赖移除及其调用链；2026-09-19 在本机 macOS 取证「目录登记模式变化」导致的确定性阻断。本报告不代表已发布版本行为。

## 问题描述

用户在 SSH 终端启动新版 Peri，第一次发送「你好」即出现 `Input was not accepted. Your draft has been kept.`，输入仍保留在编辑区。2026-09-17 没有机器日志，无法从该提示单独确定失败环节。

2026-09-19 在 macOS 本机取得确定性证据：**普通目录在被 Peri 登记之后执行 `git init`，该目录就永久无法创建会话**，错误为 `WorkspaceError::NeedsRelink`（`workspace identity changed; explicit relinking is required`），而产品没有对应的恢复入口。启动时的 `ensure_session` 与随后的每次输入都经过同一条准入链路，所以失败表现为「输入未被接收」。

上一轮发现工作区登记强制读取目录创建时间，已在本地移除此依赖；用户进一步指出：「在 fs 的处理上已经过度设计了，inode 都出来了；然后 git 可能会有阻碍」。本次取证确认后者成立：基本会话能力被文件对象身份和 Git 布局探测绑定，一次正常的 `git init` 就足以阻断。

期望：用户在正常可访问的目录中能够创建会话、发送输入、读取历史。Git 项目聚合、worktree 识别与底层文件系统能力的缺失，不应无区分地阻断这些基本操作。

## P0 依据与影响边界

- **已确证的可复现阻断**：`~/code/ai/llm-mock` 在 `git init` 之后每次启动都失败，没有任何恢复入口，用户端只看到「输入未被接收」。详见「本次取证」。
- 触发条件在日常开发中平凡且不可逆：任何用过的普通目录执行 `git init`，或已登记仓库被移除 `.git`。冲突按 Git toplevel 判定，命中后**整棵子树**都无法建立会话。
- 失败发生在发送主路径第一步（`session/new` → `resolve_workspace`），阻断的是「能否使用产品」，不是展示差异。
- 代码确认无 Git 时普通目录的工作区发现也会失败；已有登记在目录移动、替换或 Git 布局变化后重新校验，可能触发身份拒绝。
- 证据边界：本次确证覆盖 macOS 本机与 `git init` 方向；`rm -rf .git`、目录搬迁、inode 复用导致的误命中由同一比较结构推断，未逐一实测。未观察到消息丢失、数据库损坏或所有环境均不可用。

## 本次取证（2026-09-19，macOS 本机）

### 结论

`/Users/konghayao/code/ai/llm-mock` 先前以**普通目录**身份登记：project 的 `locator` 与 `object_identity` 都指向目录本身。2026-09-19 09:48:03 该目录出现 `.git` 之后，`resolve_workspace_impl` 的两个查询不再同时命中，每次 `session/new` 都返回 `NeedsRelink`；该目录此后无法建立任何会话。

### 证据链

| 时间（本机） | 事件 | 证据 |
| --- | --- | --- |
| 09:47:34、09:47:40 | 在 `~/code/ai/llm-mock` 成功建会话（当时为普通目录） | `threads.db` 中该目录的 thread 记录 |
| 09:48:03 | `~/code/ai/llm-mock/.git` 创建 | `stat -f '%SB' .git` 与 `.git/HEAD` 同秒 |
| 09:48:57 | 首次失败：`kit: initial session creation failed … NeedsRelink` | `~/.peri/logs/agent-tui.2026-09-19` |
| 09:51:09 | 在 `~/code/ai/perihelion` 启动成功 | 同日志；thread cwd `…/perihelion` 与 workspace 绑定一致 |
| 09:51:52 / 09:52:12 / 09:52:58 | 同一目录再次失败 | 同日志 |
| 09:52 起 | 84 次 `user input command failed code=-32010` | 同日志，`peri_tui::kit::steer_consumer` |

### 代码机制

`peri-resources/src/sessions/sqlite_store/workspace.rs::resolve_workspace_impl` 先 `discovery::discover`，再在 `BEGIN IMMEDIATE` 事务内比对两张表。`git init` 改变的是「项目」，不改变「工作区目录对象」：

| 查询 | 旧登记 | `git init` 之后 | 结果 |
| --- | --- | --- | --- |
| `projects WHERE locator = ? OR object_identity = ?` | locator=`/Users/…/llm-mock`，identity={device, 目录 inode} | locator=`/Users/…/llm-mock/.git`，identity={device, `.git` inode} | 两边都不命中 → 新建 project |
| `workspaces WHERE root = ? OR root_identity = ?` | root=`/Users/…/llm-mock`，root_identity={device, 目录 inode} | 与旧登记完全相同 | 命中旧行，但 `project_id` 与新建的不一致 → `Some(_) => Err(NeedsRelink)` |

只读 SQL 复核（未改写数据库）：

```bash
# projects：按当前 locator 查询 → 空，说明会新建 project
sqlite3 -readonly ~/.peri/threads/threads.db \
  "SELECT id, locator FROM projects WHERE locator = '/Users/konghayao/code/ai/llm-mock/.git';"
# workspaces：按 root / root_identity 查询 → 命中旧行 b013f6ec（project 056f6fed）
sqlite3 -readonly ~/.peri/threads/threads.db \
  "SELECT id, project_id, root FROM workspaces WHERE root = '/Users/konghayao/code/ai/llm-mock' OR root_identity = '{\"device\":16777231,\"inode\":139676947}';"
```

对照：同样在本机、几乎同一时刻，`~/code/ai/perihelion`（Git 模式登记与现状一致）启动成功。因此这不是 Git 探测能力、权限或性能问题，而是登记模式与实际模式冲突。

### 影响范围

对 9 条已登记 project 逐条比对「登记模式 vs 当前实际模式」，只有 `~/code/ai/llm-mock` 冲突；其余一致，包括 Git 模式的 `perihelion`、`open-mcp-market` 等，以及普通目录登记的 `/Users/konghayao/code/ai`、`/private/tmp/peri-print-probe`。

冲突使用 Git toplevel 判定，因此受影响的是该目录**及其全部子目录**：在 `~/code/ai/llm-mock` 的任意子目录启动同样失败，直到登记被修复。上述扫描脚本可用于判定其它机器上的受影响目录。

### 取证方法（可复用）

```bash
sqlite3 -readonly ~/.peri/threads/threads.db "SELECT locator FROM projects;" | while read -r loc; do
  if [[ "$loc" == *"/.git" ]]; then
    cur=$(git -C "${loc%/.git}" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
    [ "$cur" = "$loc" ] && echo "OK   | git模式 | $loc" || echo "CONFLICT | git模式登记但实际=$cur | $loc"
  else
    inside=$(git -C "$loc" rev-parse --is-inside-work-tree 2>/dev/null)
    [ "$inside" = "true" ] && echo "CONFLICT | 目录模式登记但现在在仓库中 | $loc" || echo "OK   | 目录模式 | $loc"
  fi
done
```

### 证据限制

- 用户运行实例为 `~/.peri/agent-v3.17.0/peri`（2026-09-18 构建）；本报告基于当前工作区代码。工作区中 `discovery.rs` 与 `workspace.rs` 未被其它在途改动触及，但未对已安装二进制做逐字节比对。
- 未实测 `rm -rf .git`、目录搬迁与 inode 复用导致误命中的场景，它们由同一比较结构推断。
- 复核为只读，未改写数据库；未验证草稿在失败路径上的持久化细节，仅确认 TUI 保留原稿且未入队。

## 已确认事实

| 事实 | 代码入口与证据 | 用户影响 |
| --- | --- | --- |
| 首次建会话先登记完整工作区身份 | `peri-acp/src/host/requests/session_lifecycle.rs::handle_new` 先 `resolve_workspace`，再 `create_bound_thread`、取得 lease 和验证 cwd | 发送第一句话前就必须通过 FS / Git / SQLite 登记链路 |
| **目录登记模式变化后两个查询不再对称** | `resolve_workspace_impl` 的 `projects` 按 `locator OR object_identity` 匹配，`workspaces` 按 `root OR root_identity` 匹配；「目录 ↔ 仓库」转换时前者改值、后者不变 | 普通目录 `git init`（或仓库移除 `.git`）后，该目录每次建会话都返回 `NeedsRelink`，且无恢复入口；2026-09-19 本机实测确认。**已修复**（`ff3a1391`：同一目录对象的布局变化复用原登记，见「修复记录」） |
| 普通目录也依赖 Git 可执行文件 | `peri-resources/src/sessions/sqlite_store/discovery.rs::discover` 先调用 `git rev-parse --is-inside-work-tree`；`git` 的 spawn 失败直接返回错误 | 未安装 Git 的精简 Linux / 容器不能建立普通目录会话。**已修复**（`7d59a7b9`：spawn 返回 `NotFound` 时降级为目录模式并记 `git_answered=false`；端到端见「修复记录」第 2 条） |
| Git 发现依赖一组命令和输出约定 | `discover` 使用 `--path-format=absolute`、`--absolute-git-dir`、`worktree list --porcelain -z`，并按特定英文 stderr 前缀识别非仓库 | Git 版本、权限或命令行为差异可能成为普通会话阻塞；旧版本失败尚未实测 |
| 相同解析重复完整发现 | `workspace.rs::resolve_workspace_impl` 先 `discover`，又在 `BEGIN IMMEDIATE` 内 `Discovery::revalidate`；后者再次 `discover` | 成功路径执行两轮发现，每轮最多五类 Git 命令，失败分支会提前返回；写事务持有期间仍等待外部进程。放大慢盘 / 慢 Git 对同库写入的影响，具体延迟未测量。**已修复**（2026-09-19：事务内只复核关键文件对象，写事务外才做完整快照复核；一次准入的 Git 调用由两轮各 5 条降为两轮各 3 条，见「修复记录」第 3 条） |
| 持久化身份同时绑定路径和文件对象 | `resolve_workspace_impl` 要求 project locator / identity 一致，workspace 则比较完整 discovery；`ObjectIdentity` 当前仍含 device/inode 或 Windows volume/file index | 路径可用不意味着身份通过；同一登记上的布局变化按各自证据复核。**已部分修复**（2026-09-19：登记键改为组合键后，新对象或新位置单独登记，不再被旧登记挡成 `NeedsRelink`；旧绑定仍失败关闭，见「修复记录」第 4 条） |
| 要求重关联，但没有可达的重关联操作 | `peri-acp-types/src/workspace.rs::WorkspaceError::NeedsRelink` 要求 explicit relinking；全仓静态入口检查未发现对应 Resources 公共操作、ACP request 或 TUI 流程，尚未做运行时流程验收。现行设计 §5.4 明确初始交付不提供该功能 | 同一文件对象搬到新路径，或原登记路径被新的文件对象替换，可能被阻塞，而当前 UI / ACP 缺少对应恢复入口；历史仍可只读，不等于可以继续执行。**已部分修复**（2026-09-19：当前可访问目录可建立新会话继续工作，改动记录见「修复记录」第 4 条；把已有会话改指到新位置的入口仍未提供） |
| 首次输入准备阶段与回执共用期限 | `peri-tui/src/kit/steer_consumer.rs::spawn_steer_consumer` 用 10 秒 timeout 包整个 `execute`，其中包括 `ensure_session` | 准备过慢可能在 enqueue RPC 前拒绝输入；目前是代码确认的边界与条件风险，未注入慢启动复现 |

## 设计判断

问题在于这些机制在用户主路径上的职责和失败范围过大。目录可访问、会话应在哪个目录执行、Git 项目如何分组、是否已有另一个进程执行同一会话，是不同的问题。当前完整工作区发现和文件对象登记将它们绑在同一准入链路中，某一项证据缺失就可能阻断基本使用。

inode 本身并非错误 API，但「要使用会话就必须证明目录仍是同一底层文件对象」比用户需要的目录执行语义更强。去掉 birthtime 后保留其余模型，只降低了一个平台门槛，还增加了 schema 迁移成本，并未证明整体取舍合理。自动测试通过只能说明实现符合当前契约，不能替代对契约本身可用性的检查。

当前拒绝搬迁 / 重建并非单个漏写的分支，而是原设计主动选择的限制；错误文案要求重关联，交付范围却没有该操作，使限制成为没有产品内出路的失败。本次取证进一步说明，即使目录对象**完全没有变**，只要 Git 布局变化（`git init`），准入同样失败——这已超出「搬迁 / 替换」的原设定范围。

同样，SQLite 写锁不能冻结外部文件系统。`revalidate` 与提交之间仍有变化窗口，设计也明确不隔离运行中的外部目录改动。重复探测提供时点一致性检查，并不构成完整执行期间的文件系统隔离，不能据此无限扩大它的准入成本。

需要重新评估文件对象身份是否应进入持久化主路径，以及 Git 识别是否应只服务 Git 相关能力。具体替代模型尚未实施，本报告不将「按路径静默覆盖历史绑定」当作已批准方案。

## 简化目标与保留边界

1. **拆开基本目录执行与 Git 增强能力。** 定义无 Git、旧 Git、Git 拒绝读取时的普通目录使用行为；不能把所有探测错误悄悄解释成「不是仓库」，造成历史归属变化。
2. **把 Git 布局变化当作正常演进。** 同一目录对象的 `git init` / 移除 `.git` 不应使该目录失去可用性；项目身份演进需要可预期地迁移或并存，而不是硬拒绝。
3. **重新论证并削减 inode / file ID 的持久化与硬拒绝。** 以用户可理解的保存目录、显式选择和恢复行为验收；不再为了维持现有实现而不断扩展平台身份探测。（部分实施：登记键与绑定复核仍使用文件对象证据，但「同一路径只允许一个登记」的硬拒绝已解除，见「修复记录」第 4 条；持久化主路径是否继续携带 inode 仍未重新论证）
4. **限制外部探测成本。** 避免在 SQLite 写事务中运行完整 Git 发现；减少同一次准入的重复检查，给准备阶段独立、可取消的期限与可见状态。（调用位置与次数已收敛，见「修复记录」第 3 条；独立可取消的准备阶段期限未实施）
5. **提供可完成的恢复操作。** 遇到目录变化应说明影响，并提供实际可达的恢复 / 选择路径；不能只提示一个没有产品入口的「显式重关联」。优先比较简化后的目录选择与明确新会话语义，不预设必须再增加一整套保留所有 ID 的重关联框架。（部分实施：当前可访问目录按新会话语义得到新登记，用户可继续工作，见「修复记录」第 4 条；提示文案与把已有会话改指到新位置的入口仍未提供）
6. **保留必要的数据与执行契约。** 历史可读、执行 cwd 明确、不同工作区的配置和权限不串用、同一会话不被两个 owner 并发执行、取消后资源正确收尾。这些要求不因简化文件身份识别而自动取消。

涉及现行 `ARC-WORKSPACE-001` 和 [工作区身份设计](../../docs/design/session-workspace-identity.md) 的调整，应在实施时同步事实源。本报告是变更需求与检查证据，不直接改写现行设计为已实现的新保证。

## 验收条件

- [x] 普通目录登记后执行 `git init`（以及已登记仓库移除 `.git`）仍能创建会话并发送输入；历史绑定不被静默改绑或隐藏。（2026-09-19 修复，见「修复记录」）
- [x] 无 Git 的普通目录能新建会话并成功发送一次输入；无重复入队，草稿状态正确。（2026-09-19 修复，见「修复记录」第 2 条）
- [ ] 新会话、已有会话、历史只读访问分别验证，不让 Git 或目录身份检查不必要地传播到其他能力。
- [x] 目录移动、备份恢复 / 文件对象变化、普通目录执行 `git init`、Git 管理目录变化有明确且可完成的用户操作；历史不被静默改绑或隐藏。（2026-09-19 修复，见「修复记录」第 4 条：搬迁与同路径替换各有单元测试，搬迁另有真实 TUI 端到端用例；备份恢复按「同路径新对象」路径覆盖，未单独实测）
- [ ] 主仓库、linked worktree、独立 clone、子目录和 symlink 场景仍得到正确的执行目录与项目展示。
- [ ] Git 缺失、旧版本、权限拒绝、慢响应分别验证；记录实际调用次数和等待阶段，避免把静态最坏预算写成实测耗时。
- [ ] 事务内没有无界或重复的外部探测；慢准备、取消和超时不造成输入丢失或重复执行。（前半 2026-09-19 修复，见「修复记录」第 3 条；慢准备 / 取消 / 超时未验证）
- [ ] 身份模型调整保留已有消息、frozen snapshot、绑定关系和执行状态；冲突处理可理解、可恢复。
- [ ] 简化后继续通过错误 cwd、配置/权限隔离、跨进程 owner 竞争及 dirty 状态保护测试。

## 与前一轮修复的关系

本地 birthtime 移除已通过 macOS resources 113 项、Linux 将 `statx` 设为 ENOSYS 后 resources 113 项，以及 ACP 输入链路 9 项测试；schema 3 升级回归覆盖原 ID / binding、历史、frozen、dirty execution 与重开复用。这些是前一轮补丁的证据，不是本 P0 已完成的证据。

本轮新增本机取证（`git init` 场景）与只读复核，没有据此实施新的身份模型，也没有取得 A800 / 原生 Windows 的运行证据。关联项目：

- [兼容性待办](2026-09-17-platform-compatibility.md)：Windows HOME、插件进程回收、配置并发写等独立风险；不全部纳入本 P0 的关闭条件。
- [Worktree 身份原始验收](2026-09-12-worktree-session-identity.md)：当前硬拒绝规则的来源，应与本次可用性目标一起重新评估。
- [3.15 历史不可访问 P0](2026-09-16-p0-315-history-inaccessible.md)：相关历史背景，不将本次问题与已修复症状混为一项。

## 2026-09-17 审计范围与证据限制

三个 subagent 分别检查发送链路、身份模型和跨模块 FS / Git 假设；主 agent 核对关键源码并撰写报告。报告与补丁审查相互独立：补丁没有新的实现阻断问题，不代表沿用的产品契约合理。

额外发现已并入兼容性待办：stdio 的有损路径转换、原子替换未保留既有文件权限、Windows 路径表示差异。它们不是此次 A800 故障的已证实原因。

未采纳审计中的过强断言：未实际运行旧 Git，不能断言具体版本必然怎样失败；网络文件系统 inode 是否不稳定需实测；没有重关联入口不等于数据已丢失；测试专用 `FilesystemThreadStore` 的实现不能作为生产会话存储缺陷证据。本轮未运行性能测量或新增故障注入测试，验收矩阵仍是待办。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
| --- | --- | --- | --- | --- |
| 2026-09-17 | — | Open | agent | 按用户要求登记 P1，继续并行审计，由主 agent 汇总报告 |
| 2026-09-19 | P1 | P0 | 用户 | 用户在 macOS 本机确证：普通目录 `git init` 后该目录永久无法创建会话，错误 `NeedsRelink`，且无恢复入口 |
| 2026-09-19 | — | — | agent | 完成本机取证：定位到 `projects` / `workspaces` 两个查询在登记模式变化后的不对称；只读 SQL 复核并扫描全机冲突目录 |
| 2026-09-19 | — | Open（部分修复） | 用户 | 用户要求派出 subagent 对抗根因后实施修复；按测试驱动完成登记模式冲突修复，本 issue 的整体简化目标仍未实施 |
| 2026-09-19 | — | Open（部分修复） | agent | 补充无 Git 路径的真实 TUI 端到端验收（勾选验收条件第 2 项），生产代码未变；并同步旧库 e2e 的 schema 版本期望 |
| 2026-09-19 | — | Open（部分修复） | agent | 完成准入探测成本收敛：事务内不再执行外部进程，一次准入的 Git 调用由两轮各 5 条降为两轮各 3 条（见「修复记录」第 3 条） |
| 2026-09-19 | — | Open（部分修复） | agent | 解除登记键的硬拒绝：同一路径的新文件对象与同一对象的新路径各自登记，旧绑定按各自证据复核；勾选验收条件第 4 项（见「修复记录」第 4 条） |

## 修复记录

### 2026-09-19：登记模式冲突（本机 macOS）

**范围**：只解除「同一目录对象在 Git 布局变化后不可用」这一确定性阻断。不改变文件对象身份的持久化、不新增重关联入口、不调整准入链路的外部探测成本；本 issue「简化目标与保留边界」全部 6 项与其余验收条件仍未实施。

**改动**（`peri-resources/src/sessions/sqlite_store/`）：

- `discovery.rs`：发现结果改为 `Observation { discovery, git_answered }`。`git_answered` 记录 Git 是否真正回答过（包括明确回答「不是仓库」）；spawn 失败或可执行文件缺失得到的是不完整目录观测。`revalidate` 只比较其中的 `discovery`，既有复核语义不变。
- `workspace.rs::resolve_workspace_impl`：`root` 路径与 `root_identity` 同时命中已登记行时判定为同一目录对象，复用原 `project_id` 与工作区 ID，只在原行刷新 `discovery` 快照；仅当观测完整（`git_answered`）才允许覆盖，否则仍返回 `NeedsRelink`。只命中路径或只命中对象身份的判定保持不变。

不更换 `project_id` 的原因：`session_bindings` 以 `(workspace_id, project_id)` 复合外键引用 `workspaces`，改指向会让已有绑定成为悬空引用；TUI 的项目范围又按 `project_id` 过滤，改指向会把历史移出用户当前项目。

**回归测试**（`workspace_test.rs`，修复前均失败、修复后通过）：

- `test_worktree_directory_gaining_repository_keeps_registration`：目录登记 → `git init` → 同一工作区仍可解析，新会话可建，历史绑定不变；此前登记的**子目录**会话不并入仓库工作区，仍按原快照拒绝。
- `test_worktree_repository_losing_git_keeps_registration`：已登记仓库 → 移除 `.git` → 注册继续可用，子目录重新各自成区。
- `discovery_test.rs` 补充 `git_answered` 证据断言；`test_worktree_git_availability_change_preserves_binding_boundary` 覆盖「Git 不可用不得降级目录模式」方向。

**验证证据**：

| 验证 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | 122 项通过（另见「遗留」的既有偶发用例） |
| `cargo test -p peri-acp --lib` | 678 项通过 |
| `cargo clippy -p peri-resources --all-targets -- -D warnings` | 无告警 |
| `cargo fmt --all -- --check` | 无格式差异 |
| E2E `tests/scenarios/workspace-git-init.test.ts`（新增，已入 L0） | 修复前 `run-2026-09-19T02-50-52` 失败于 `/clear` 建会话（绑定停在 1）；修复后 `run-2026-09-19T02-51-49` 通过，终稿树复跑 `run-2026-09-19T02-59-42` 通过（11s）。用例走真实 TUI：普通目录建会话发消息 → `git init` → `/clear` 建新会话再发消息 → 重启后再次建会话发消息，并核对项目/工作区 ID 不变、3 个绑定同属一个工作区、观测快照已刷新为仓库模式 |
| E2E `tests/smoke/steer-queue-live.test.ts`（受影响面回归） | `run-2026-09-19T02-55-59` 通过，无重试 |
| 真实 binary 复现消除 | 隔离 HOME 与数据库、以用户实际目录结构（本机 `llm-mock` 的登记副本）运行真实 `peri`：修复前同一状态每次启动 `NeedsRelink`；修复后 `kit: initial session created` 正常，项目/工作区 ID 与历史绑定不变，`discovery` 刷新为仓库模式 |

**遗留**：

- `test_worktree_dirty_reset_held_stale_and_exact_generation`（曾 6 次运行出现 2 次偶发失败）与 `test_worktree_execution_child_process`（子进程 lease，曾失败于「generation required」）都属在途 dirty 恢复改动，本 issue 未处理其内容。该改动已由他方提交为 `2c243a9d`；2026-09-19 复跑 `cargo test -p peri-resources --lib` 122 项全绿，两者均通过。
- E2E `tests/scenarios/legacy-history-upgrade.test.ts` 断言 `PRAGMA user_version` 写死为 3，未随 schema 版本 3→4（`51f1bbe4`）同步而失败，与本 issue 无关；已改为从 `schema.rs` 读取当前版本，`run-2026-09-19T03-42-11` 通过（见「修复记录」第 2 条）。
- 无 Git 环境（Git 可执行文件缺失）下的端到端输入已实测通过（`run-2026-09-19T04-07-31`），验收条件第 2 项已勾选；Git 版本差异、权限拒绝与慢响应仍未实测。
- 本轮未实测 `rm -rf .git` 与目录搬迁在真实 TUI 下的组合；「简化目标」中的探测成本削减已由「修复记录」第 3 条实施，但未做性能测量。

### 2026-09-19：无 Git 路径的端到端验收与旧库 e2e 期望同步（第 2 条，生产代码未变）

**范围**：把上一轮只有 resources 层单元测试证据的「无 Git / 普通目录」结论补成真实 TUI 端到端证据，勾选验收条件第 2 项；同时修掉一条与本 issue 无关、但会连坐 L0 的旧库 e2e 期望。生产代码无改动——`discovery.rs` 中 spawn `NotFound` 降级为目录模式的实现由 `7d59a7b9` 提供。

**改动**：

- 新增 `e2e/tests/scenarios/workspace-no-git.test.ts`（已入 L0）。用 `env -i` 显式构造进程环境，PATH 只挂一个 shim 目录：把 `/usr/bin`、`/bin` 逐项软链过去，跳过所有 `git*`；用例开头以 `command -v git` 必须失败作为前提守卫。
- `e2e/tests/scenarios/legacy-history-upgrade.test.ts`：`PRAGMA user_version` 期望值改为解析 `peri-resources/src/sessions/sqlite_store/schema.rs` 中的 `PRAGMA user_version = N`，不再写死字面量（设计 §8：写打开在同一事务内升级，只读打开不升级）。

**验证证据**：

| 验证 | 结果 |
| --- | --- |
| E2E `tests/scenarios/workspace-no-git.test.ts` | `run-2026-09-19T04-07-31` 3 项通过（14s）。① 无 Git 建会话、发送输入、收到模型回复；空回车不重复入队（模型请求数仍为 1，消息各 1 条）；`common_dir` / `private_dir` 为 null，1 project / 1 binding；同一会话第二次输入正常。② 重启后同目录再建会话：仍 1 project / 1 workspace，2 个绑定同属该工作区。③ 判别用例：cwd 在 Git 仓库子目录内但 PATH 无 git 时，`discovery.root` 等于 cwd 本身、不推断仓库关系 |
| E2E `tests/scenarios/legacy-history-upgrade.test.ts` | `run-2026-09-19T03-42-11` 通过（44s），`user_version` 断言随 `schema.rs` 当前版本变化 |
| E2E L0 全量（`--tier l0 --no-interactive`） | `run-2026-09-19T04-07-56` 8/9 通过（7m28s）：`workspace-git-init` 10s、`workspace-no-git` 13s、`legacy-history-upgrade` 45s 均通过。唯一失败项 `tests/panels/plugin-uninstall-no-freeze.test.ts` 为本轮唯一新增文件之外的既有用例，见「遗留」 |

**方法说明（可复用）**：tmux 的 `-e PATH=` 不会到达会话 shell——会话为 login bash，`/etc/profile` 的 path_helper 会重置 PATH。早先版本的 no-Git 用例因此在本机静默退化成 Git 模式（真实 git 可见），用例看似通过却没有覆盖目标路径。因此「无 Git」必须由命令自身用 `env -i` 保证；上面第 ③ 项判别用例即用于守住这一前提。

**遗留**：

- Git 版本差异（旧版 `--path-format=absolute` 等）、权限拒绝、慢响应仍未实测；验收条件第 6 项待办。
- L0 唯一失败项 `tests/panels/plugin-uninstall-no-freeze.test.ts` 在全量运行中 90s 超时（用例自设 timeout），单独复跑 48s 通过。两者都不经过本 issue 的发现 / 绑定链路，判定为负载下的慢启动抖动，本 issue 未处理；若要闭环 L0 门禁需另有记录。

### 2026-09-19：准入探测移出写事务并收敛 Git 调用（第 3 条）

**范围**：只处理「相同解析重复完整发现」与「写事务内执行外部探测」两项已确认事实。不改变文件对象身份模型、登记唯一性、`NeedsRelink` 语义与绑定校验强度；简化目标第 3、5 项与其余验收条件仍未实施。

**改动**（`peri-resources/src/sessions/sqlite_store/`）：

- `discovery.rs`：三个 `rev-parse` 位置（`--show-toplevel` / `--git-common-dir` / `--absolute-git-dir`）合并为一次 `git_paths` 调用，输出按参数顺序解析；行数与请求不符时返回类型化 `DiscoveryError`，不把错位的位置当成根目录。新增 `Discovery::reassert_key_objects`：只复核 cwd 规范路径与 root / common / private 三个已记录的目录对象身份，不启动任何外部进程。
- `workspace.rs`：`resolve_workspace_impl` 提交前的复核改用 `reassert_key_objects`；事务内的 `validate_resolved_on` 同样只复核关键文件对象；完整快照复核移到事务外，由新增的 `revalidate_registered_observation_on` 承担（`validate_resolved` 与 `validate_session_binding_impl` 在 SQL 校验后调用）。

效果：一次准入的 Git 调用从「两轮各 5 条命令、其中一轮在 `BEGIN IMMEDIATE` 内」变为「两轮各 3 条命令、全部在写事务之外」。

**验证证据**（`workspace_test.rs`、`discovery_test.rs` 新增用例，修复前失败）：

| 验证 | 结果 |
| --- | --- |
| `test_worktree_registration_probes_filesystem_outside_write_lock` | 假 Git 每次被调用时用独立连接尝试 `BEGIN IMMEDIATE`（`busy_timeout=0`）并把结果写进日志；目录模式一次准入记录 2 次调用，全部为 `free`（写锁空闲），登记产生 1 条 binding |
| `test_worktree_repository_registration_keeps_git_calls_bounded` | 仓库模式一次准入 6 次调用（两轮 × 3 条命令），全部为 `free` |
| `test_worktree_key_object_reassertion_rejects_changed_objects` | 事务内复核的判别用例：`.git` 被移除、根目录被新文件对象替换 → `NeedsRelink`；cwd 消失 → `Unavailable`；未变化时通过 |
| `test_worktree_git_missing_mid_discovery_is_not_directory_mode` | 合并调用后 Git 中途不可用 → 类型化 `DiscoveryError`，不降级为目录模式 |
| `cargo test -p peri-resources --lib` | 127 项通过（改动前 122 项） |
| `cargo test -p peri-acp --lib` | 678 项通过 |
| `cargo clippy -p peri-resources --all-targets -- -D warnings`、`cargo fmt --all -- --check` | 无告警、无格式差异 |
| E2E `workspace-git-init` / `workspace-no-git` / `steer-queue-live` | `run-2026-09-19T04-33-54` 3/3 通过（38s，串行、无重试） |

**遗留**：

- 未测量慢盘 / 慢 Git 下的实际等待时间；本条第 3 项的断言是「调用次数」与「调用时的持锁状态」，不是耗时。
- 「准备阶段独立、可取消的期限」（`steer_consumer` 用 10 秒包住整个 `execute`）属验收条件第 7 项后半，本轮未处理。

### 2026-09-19：登记键改为组合键，解除搬迁 / 替换的硬拒绝（第 4 条）

**范围**：解除「同一路径上的新文件对象」与「同一文件对象的新路径」被登记唯一约束与旧登记挡成 `NeedsRelink` 的阻断，让用户在可访问目录继续建立新会话。不改变：文件对象证据仍进入持久化主路径与绑定复核；已有 binding 不自动改写；不新增重关联入口；不做性能测量。

**根因**：原登记表用单列唯一表达身份——`projects.locator` / `workspaces.root` 各自唯一，`resolve_workspace_impl` 又用 `root = ? OR root_identity = ?` 查询。于是「同一路径上的另一个文件对象」在路径上撞唯一约束，「同一对象的新路径」命中旧行却路径不符，两者都只能返回 `NeedsRelink`；而产品没有重关联入口，用户没有可完成的下一步。

**改动**：

- `schema.rs`：`SchemaState::Version4` + `relax_registration_keys`。schema 4→5 在事务内重建 `projects` / `workspaces`，把单列唯一约束换成组合键 `UNIQUE(locator, object_identity)` / `UNIQUE(root, root_identity)`（`UNIQUE(id, project_id)` 与 `session_bindings` 复合外键保持不变），逐列复制行内容，`user_version = 5`。
- `schema.rs::init_schema`：重建要被引用的父表执行 `DROP TABLE`，而 SQLite 对父表的隐式删除会立即检查外键——实测 `PRAGMA defer_foreign_keys = ON` 挡不住（`sqlite3` 3.51.0 复现），该 PRAGMA 也只在事务外生效。因此重建路径改为：同一连接上事务外 `foreign_keys = OFF` → 事务内迁移并在提交前 `PRAGMA foreign_key_check` 补齐校验（有悬空引用即回滚）→ 恢复 `foreign_keys = ON`（错误路径同样恢复）。其余升级路径不变。
- `workspace.rs::resolve_workspace_impl`：工作区查询改为 `root = ? AND root_identity = ?` 的组合命中；未命中即为该位置建立新登记（新 `WorkspaceId`）。项目只在 `locator = ? AND object_identity = ?` 同时一致时复用，因此 Git linked worktree 换位后 common directory 未变仍属原项目，不相关的同名副本各自成项目。旧行、旧绑定、执行状态与历史都不改写。

**回归测试**（`workspace_test.rs` 与 `schema_test.rs` 新增用例，修复前失败）：

| 验证 | 结果 |
| --- | --- |
| `test_worktree_replaced_directory_registers_new_workspace_keeps_old_history` | 目录被删除并在同路径重建：旧会话 `validate_session_binding` → `NeedsRelink` 且绑定字段未变，历史仍在该项目列表可见；新会话在新登记上建立成功 |
| `test_worktree_moved_directory_registers_new_path_keeps_old_history` | 目录整体改名：旧会话 → `Unavailable`，历史保留；新路径单独登记，执行 cwd 为新路径 |
| `test_worktree_moved_linked_worktree_reuses_project_registers_new_workspace` | `git worktree move` 后：新工作区独立、`project_id` 复用原项目（common directory 未变） |
| `test_worktree_registration_reuses_exact_object_and_keeps_rows_unique` | 同一 `(root, root_identity)` 仍然唯一，重复登记被约束拒绝 |
| `test_version4_upgrade_relaxes_registration_keys_and_preserves_rows` | schema 4 库升级到 5：升级前同路径第二个文件对象被单列唯一拒绝；升级后旧登记 / 绑定 / 执行状态字节不变，组合键允许新对象登记、仍拒绝同组合重复，孤儿工作区仍被外键拒绝 |
| `cargo test -p peri-resources --lib` | 131 项通过 |
| `cargo test -p peri-acp --lib` | 678 项通过 |
| `cargo clippy -p peri-resources --all-targets -- -D warnings`、`cargo fmt --all -- --check` | 无告警、无格式差异 |
| E2E `tests/scenarios/workspace-directory-moved.test.ts`（新增，已入 L0） | 真实 TUI：普通目录建会话发消息 → 退出 → 目录整体改名 → 新位置启动建会话发消息成功；2 project / 2 workspace / 2 binding，旧 binding 与项目 locator、工作区 root、旧 thread 的 cwd 均未被改写，历史仍在 |
| E2E `workspace-git-init` / `workspace-no-git` / `legacy-history-upgrade` 受影响面回归 | 3 文件 12 项全部通过（68s），`legacy-history-upgrade` 的 `user_version` 断言随 `schema.rs` 自动跟随为 5 |

**事实源同步**：[工作区身份设计](../../docs/design/session-workspace-identity.md) §3.2 登记裁决（组合键与新对象 / 新位置各自登记）、§3.3 情景规则、§5.4 位置重定位（可完成的前进路径）、§8 单库存储（当前 schema 5 与重建路径）；`ARC-WORKSPACE-001` Rule 与 `docs/code-index/peri-resources.md` 同步。

**遗留**：

- 文件对象证据（device / inode 或 Windows volume / file index）仍留在登记与绑定复核的持久化主路径；本条第 4 条只解除了它的硬拒绝，没有重新论证是否保留（简化目标第 3 项的剩余部分）。
- 仍没有把已有会话改指到新位置的入口：用户可完成的是「在当前目录建立新会话」，旧会话保持只读历史且执行失败关闭。提示文案尚未说明这一步。
- 同路径替换（删除重建）只做了单元测试，未做真实 TUI 端到端：运行中的 TUI 其进程 cwd 已被删除，`getcwd` 语义与登记语义无关，不适合作为该场景的端到端入口。
- 备份恢复（restore）按同路径替换路径覆盖，未单独实测。
