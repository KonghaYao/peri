# P0：文件系统身份与 Git 探测阻断会话创建与发送

**状态**：Open（登记模式冲突、无 Git 目录建会话、准入探测成本、目录搬迁 / 替换的登记可用性、绑定失败文案与输入路径的原因提示、准备阶段与受理回执的期限拆分、Git 命令版本兼容、三条路径的探测边界、既有保护链路复核、仓库布局端到端验收、旧版 Git 的 common dir 推导与慢响应实测、慢 Git 的端到端验收、真实旧版 Git 2.4.12 二进制验收（含一处大小写分类缺陷的修复）、准入内重复完整发现的收敛、绑定执行目录的文本形式统一、会话建立期间的用户可见状态与 Git spawn 的有限退让重试（`7f90c11d`，第 17 条事后补记）已于 2026-09-19 完成，验收条件 9 项全部勾选；简化目标 3 的重新论证已按代码证据补完、简化目标 5 已给出按「新会话语义」收口的处置建议，两项的收口本身仍需产品决策，本 issue 未关闭）
**优先级**：P0（用户指定；2026-09-19 依据本机确证由 P1 升级）
**类型**：可用性缺陷 / 设计简化
**创建日期**：2026-09-17
**检查范围**：2026-09-17 审计创建时间依赖移除及其调用链；2026-09-19 在本机 macOS 取证「目录登记模式变化」导致的确定性阻断。本报告不代表已发布版本行为。

## 问题描述

用户在 SSH 终端启动新版 Peri，第一次发送「你好」即出现 `Input was not accepted. Your draft has been kept.`，输入仍保留在编辑区。2026-09-17 没有机器日志，无法从该提示单独确定失败环节。

2026-09-19 在 macOS 本机取得确定性证据：**普通目录在被 Peri 登记之后执行 `git init`，该目录就永久无法创建会话**，错误为 `WorkspaceError::NeedsRelink`（当时文案为 `workspace identity changed; explicit relinking is required`，已于「修复记录」第 5 条改写），而产品没有对应的恢复入口。启动时的 `ensure_session` 与随后的每次输入都经过同一条准入链路，所以失败表现为「输入未被接收」。

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
| **目录登记模式变化后两个查询不再对称** | 当时 `resolve_workspace_impl` 的两个查询各按 `locator OR object_identity`、`root OR root_identity` 匹配（现已改为两者同时命中的组合键）：「目录 ↔ 仓库」转换时前者改值、后者不变 | 普通目录 `git init`（或仓库移除 `.git`）后，该目录每次建会话都返回 `NeedsRelink`，且无恢复入口；2026-09-19 本机实测确认。**已修复**（`ff3a1391`：同一目录对象的布局变化复用原登记，见「修复记录」） |
| 普通目录也依赖 Git 可执行文件 | `peri-resources/src/sessions/sqlite_store/discovery.rs::discover` 先调用 `git rev-parse --is-inside-work-tree`；`git` 的 spawn 失败直接返回错误（`7f90c11d` 之后：`NotFound` 仍直接降级为目录模式，其余 spawn 失败退让 10/20ms 重试至 3 次，用尽后仍报 `Git could not be executed`，见「修复记录」第 17 条） | 未安装 Git 的精简 Linux / 容器不能建立普通目录会话。**已修复**（`7d59a7b9`：spawn 返回 `NotFound` 时降级为目录模式并记 `git_answered=false`；端到端见「修复记录」第 2 条） |
| Git 发现依赖一组命令和输出约定 | `discover` 曾使用 `--path-format=absolute`（上游文档记为 Git 2.31 引入）、`--absolute-git-dir`（2.13）与 `worktree list --porcelain -z`，并按特定英文 stderr 前缀识别非仓库 | Git 版本、权限或命令行为差异可能成为普通会话阻塞。**已部分修复**（2026-09-19：不再使用版本相关选项，相对输出按 cwd 还原，`worktree list` 不支持 `-z` 时退回换行分隔，见「修复记录」第 7 条；真实旧版 Git 2.4.12 二进制已实测——四种目录组合均可建会话，但实测暴露出一处大小写敏感的分类缺陷并已修复，见第 13 条；2.4.12 之前的版本未测） |
| 相同解析重复完整发现 | `workspace.rs::resolve_workspace_impl` 先 `discover`，又在 `BEGIN IMMEDIATE` 内 `Discovery::revalidate`；后者再次 `discover` | 成功路径执行两轮发现，每轮最多五类 Git 命令，失败分支会提前返回；写事务持有期间仍等待外部进程。放大慢盘 / 慢 Git 对同库写入的影响，具体延迟未测量。**已修复**（2026-09-19：事务内只复核关键文件对象，写事务外才做完整快照复核；第 3 条把探测移出写事务，第 14 条再收敛为一次准入一轮完整发现——现行值是仓库模式 3 次 Git 调用、已有会话复核 1 轮；本行原先写的「两轮各 3 条」漏同步，当时的状态变更记录只声明同步了验收条件 3 / 6 与第 9 条，见「修复记录」第 3、14 条） |
| 持久化身份同时绑定路径和文件对象 | `resolve_workspace_impl` 要求 project locator / identity 一致，workspace 则比较完整 discovery；`ObjectIdentity` 当前仍含 device/inode 或 Windows volume/file index | 路径可用不意味着身份通过；同一登记上的布局变化按各自证据复核。**已部分修复**（2026-09-19：登记键改为组合键后，新对象或新位置单独登记，不再被旧登记挡成 `NeedsRelink`；旧绑定仍失败关闭，见「修复记录」第 4 条） |
| 要求重关联，但没有可达的重关联操作 | `peri-acp-types/src/workspace.rs::WorkspaceError::NeedsRelink` 曾要求 explicit relinking；全仓静态入口检查未发现对应 Resources 公共操作、ACP request 或 TUI 流程，尚未做运行时流程验收。现行设计 §5.4 明确初始交付不提供该功能 | 同一文件对象搬到新路径，或原登记路径被新的文件对象替换，可能被阻塞，而当前 UI / ACP 缺少对应恢复入口；历史仍可只读，不等于可以继续执行。**已部分修复**（2026-09-19：当前可访问目录可建立新会话继续工作，改动记录见「修复记录」第 4 条；文案已改为指出该前进路径、输入路径会复述失败原因，见第 5 条；把已有会话改指到新位置的入口仍未提供） |
| 首次输入准备阶段与回执共用期限 | `peri-tui/src/kit/steer_consumer.rs::spawn_steer_consumer` 曾用 10 秒 timeout 包整个 `execute`，其中包括 `ensure_session` | 准备过慢可能在 enqueue RPC 前拒绝输入。**已修复**（2026-09-19：准备与会话已发出请求的回执改为各自计时，慢准备仍被受理、准备超时按未受理恢复原稿，见「修复记录」第 6 条） |

## 设计判断

问题在于这些机制在用户主路径上的职责和失败范围过大。目录可访问、会话应在哪个目录执行、Git 项目如何分组、是否已有另一个进程执行同一会话，是不同的问题。当前完整工作区发现和文件对象登记将它们绑在同一准入链路中，某一项证据缺失就可能阻断基本使用。

inode 本身并非错误 API，但「要使用会话就必须证明目录仍是同一底层文件对象」比用户需要的目录执行语义更强。去掉 birthtime 后保留其余模型，只降低了一个平台门槛，还增加了 schema 迁移成本，并未证明整体取舍合理。自动测试通过只能说明实现符合当前契约，不能替代对契约本身可用性的检查。

当前拒绝搬迁 / 重建并非单个漏写的分支，而是原设计主动选择的限制；错误文案要求重关联，交付范围却没有该操作，使限制成为没有产品内出路的失败。本次取证进一步说明，即使目录对象**完全没有变**，只要 Git 布局变化（`git init`），准入同样失败——这已超出「搬迁 / 替换」的原设定范围。

同样，SQLite 写锁不能冻结外部文件系统。`revalidate` 与提交之间仍有变化窗口，设计也明确不隔离运行中的外部目录改动。重复探测提供时点一致性检查，并不构成完整执行期间的文件系统隔离，不能据此无限扩大它的准入成本。

需要重新评估文件对象身份是否应进入持久化主路径，以及 Git 识别是否应只服务 Git 相关能力。具体替代模型尚未实施，本报告不将「按路径静默覆盖历史绑定」当作已批准方案。

## 简化目标与保留边界

1. **拆开基本目录执行与 Git 增强能力。** 定义无 Git、旧 Git、Git 拒绝读取时的普通目录使用行为；不能把所有探测错误悄悄解释成「不是仓库」，造成历史归属变化。
2. **把 Git 布局变化当作正常演进。** 同一目录对象的 `git init` / 移除 `.git` 不应使该目录失去可用性；项目身份演进需要可预期地迁移或并存，而不是硬拒绝。
3. **重新论证并削减 inode / file ID 的持久化与硬拒绝。** 以用户可理解的保存目录、显式选择和恢复行为验收；不再为了维持现有实现而不断扩展平台身份探测。（部分实施：登记键与绑定复核仍使用文件对象证据，但「同一路径只允许一个登记」的硬拒绝已解除，见「修复记录」第 4 条；持久化主路径是否继续携带这份证据已按代码证据重新论证，见下文「简化目标 3 与 5 的重新论证」，结论是保留但限制用途、不扩展平台探测——收口仍需产品决策）
4. **限制外部探测成本。** 避免在 SQLite 写事务中运行完整 Git 发现；减少同一次准入的重复检查，给准备阶段独立、可取消的期限与可见状态。（调用位置与次数已收敛，见「修复记录」第 3 条；一次准入至多执行一次完整发现、准入内其余检查只复核已记录证据，见第 14 条，且跨层门禁断言窗口内至多一轮发现；同一个目录不再因文本形式不同被重复解析，见第 15 条；准备阶段已有独立期限并可被应用关闭取消，见第 6 条；准备期间的用户可见状态已实施，见第 16 条——会话建立全程状态栏显示「正在准备会话」，端到端实测该提示可见 1019ms（注入等待每次 300ms 的一轮发现），建立结束即消失。第 15 条实测里「提交后前 10 秒保持不变」的那段窗口就是这次建立本身：等待中的输入与启动期的建立共用同一次状态，不再是一段没有反馈的等待）
5. **提供可完成的恢复操作。** 遇到目录变化应说明影响，并提供实际可达的恢复 / 选择路径；不能只提示一个没有产品入口的「显式重关联」。优先比较简化后的目录选择与明确新会话语义，不预设必须再增加一整套保留所有 ID 的重关联框架。（部分实施：当前可访问目录按新会话语义得到新登记，用户可继续工作，见「修复记录」第 4 条；绑定失败文案已改为指出该前进路径、输入路径已复述失败原因，见第 5 条；把已有会话改指到新位置的入口仍未提供——按下文「简化目标 3 与 5 的重新论证」的建议，该项按「已由新会话语义解决」收口，不补重定位框架，收口本身待产品决策）
6. **保留必要的数据与执行契约。** 历史可读、执行 cwd 明确、不同工作区的配置和权限不串用、同一会话不被两个 owner 并发执行、取消后资源正确收尾。这些要求不因简化文件身份识别而自动取消。

涉及现行 `ARC-WORKSPACE-001` 和 [工作区身份设计](../../docs/design/session-workspace-identity.md) 的调整，应在实施时同步事实源。本报告是变更需求与检查证据，不直接改写现行设计为已实现的新保证。

## 简化目标 3 与 5 的重新论证（2026-09-19，按代码证据，未改代码）

简化目标 3 要求「重新论证并削减 inode / file ID 的持久化与硬拒绝」，简化目标 5
要求「提供可完成的恢复操作」。第 4、5 条已经把硬拒绝改成可继续工作的新登记，把
失败文案指向前进路径；本节回答剩下两个问题：这份文件对象证据现在还值不值得留在
持久化主路径上，以及目录变化后用户能不能走完一条路。

### 证据用在哪些地方（代码事实）

| 位置 | 用途 | 不一致时的用户可见结果 |
| --- | --- | --- |
| `workspaces` 登记键 `(root, root_identity)`、`projects` 复用键 `(locator, object_identity)`（`workspace.rs::resolve_workspace_impl`） | 区分「同一路径上的新目录对象」与「同一对象的新路径」 | 都不命中已登记行 → 按实际打开的目录建新登记；已有会话继续按各自证据复核（第 4 条） |
| `Discovery::reassert_key_objects`（写事务内与准入内的复核，`discovery.rs:556`） | 提交前 / 执行前确认 root 与 Git common / private 目录仍是登记时的对象 | `NeedsRelink`：该会话不能在当前目录继续执行，历史仍可读，用户在当前目录建新会话即可继续 |
| `discovery.rs::object_identity`（发现的一部分） | 取对象证据本身 | 非 Unix / 非 Windows 目标返回 `Unsupported`、元数据不可读返回 `Unavailable`，两者都建不了会话（当前发布目标均在覆盖范围内） |

### 论证

1. **这份证据只回答「同一路径上的两个时刻是不是同一个目录对象」，没有第二用途。**
   去掉它，登记键就退回路径：`rm -rf` 后重建的同名目录会被当成原对象，旧会话会
   安静地在另一棵文件树上继续执行——这正是设计禁止的静默改绑，也与「历史归属
   可解释」冲突。保留它，代价只是「同路径的新对象得到一条新登记」，用户仍然能
   在打开的目录里工作。
2. **第 4 条已经把这份证据从「硬拒绝」改成「新登记」。** 布局变化（`git init` /
   移除 `.git`）不再触发拒绝，对象替换与换位得到新登记；仍会拒绝的只剩「用已有
   会话在已变化的目录上继续执行」，以及「Git 不可用时无法证明的布局变化」。
   前者是简化目标 6 的直接体现（执行 cwd 明确、不同工作区不串用），不是可省掉的
   门槛；后者是第 2 条特意保留的失败关闭方向。
3. **不再扩展探测面。** 现有实现只比较同一路径上两个时刻的对象，不用 inode 证明
   复制、重建或历史路径复用（设计 §3.2 已写死这条边界）。本轮没有新增任何平台
   探测，后续也不应为兼容更多文件系统而扩展它。
4. **已知代价（未实测，不宣称已解决）**：网络文件系统或容器覆盖层上对象证据可能
   不稳定。登记边界上不稳定只会多出一条登记，无功能损失；运行时复核不稳定会得到
   `NeedsRelink`，即与「目录被替换」相同的用户可见结果——路径依然可达（在当前
   目录建新会话）。这是本模型的代价，记录在此，不实测不结论。
5. **简化目标 5 的处置建议：按「新会话语义」收口，本轮不建重定位框架。** 设计
   §5.4 已把「初始交付不提供重定位 / 重关联」写成决定：产品给的是「历史保持可读
   ＋ 在当前可访问目录建新会话」，而这条路径在同一窗口可达——`/clear` 就是
   `submit_consumer.rs::handle_clear_submit` 里无条件的新会话，失败提示也已复述
   服务端原因并指出该前进路径（第 5 条）。本 issue 的简化目标 5 原文同样把这条
   列为首选（「优先比较简化后的目录选择与明确新会话语义，不预设必须再增加一整套
   保留所有 ID 的重关联框架」）。因此建议：把该项按「已由新会话语义解决」收口，
   而不是补一个重定位入口；若要保留 `WorkspaceId` 做身份迁移，属于设计 §5.4 末尾
   写明的后续工作，不在本 issue 范围。

### 需要用户决策的两点

1. **运行时复核是否继续使用文件对象证据。** 保留＝现状：目录被替换后旧会话只能在
   原目录可用，用户走新会话；去掉＝同一路径上的任意目录都被当作原会话的执行目录，
   历史绑定会随路径静默漂移。**建议保留**，理由是第 2 条。
2. **是否在提示里点名可达操作。** 现行协议层文案是
   `session directory changed; this session cannot continue here: start a new
   session in the current directory`，没有点名 `/clear`——协议错误文案不适合宣称
   某个 TUI 命令。若要点名，应加在 TUI 的 `steer-session-unavailable` 文案上，
   属产品文案决策，本轮未改。

## 验收条件

- [x] 普通目录登记后执行 `git init`（以及已登记仓库移除 `.git`）仍能创建会话并发送输入；历史绑定不被静默改绑或隐藏。（2026-09-19 修复，见「修复记录」）
- [x] 无 Git 的普通目录能新建会话并成功发送一次输入；无重复入队，草稿状态正确。（2026-09-19 修复，见「修复记录」第 2 条）
- [x] 新会话、已有会话、历史只读访问分别验证，不让 Git 或目录身份检查不必要地传播到其他能力。（2026-09-19：新会话见「修复记录」第 3 条的调用次数与持锁状态，计数已由第 14 条收敛为一轮完整发现；已有会话与历史只读访问见第 9 条——复核恰好一轮观测且全部在写事务外，列表 / 消息 / frozen / 绑定在读路径上 0 次 Git 调用，登记目录删除后仍可读）
- [x] 目录移动、备份恢复 / 文件对象变化、普通目录执行 `git init`、Git 管理目录变化有明确且可完成的用户操作；历史不被静默改绑或隐藏。（2026-09-19 修复，见「修复记录」第 4 条：搬迁与同路径替换各有单元测试，搬迁另有真实 TUI 端到端用例；备份恢复按「同路径新对象」路径覆盖，未单独实测）
- [x] 主仓库、linked worktree、独立 clone、子目录和 symlink 场景仍得到正确的执行目录与项目展示。（2026-09-19：新增真实 TUI 用例 `e2e/tests/scenarios/workspace-worktree-layouts.test.ts` 覆盖子目录 / linked worktree / 独立 clone 三种布局的执行目录与项目归属，见「修复记录」第 10 条；主仓库与 symlink 由 resources 单元测试 `test_worktree_main_linked_subdirectory_and_clone_identity`、`test_worktree_symlink_discovery_reuses_identity_but_binding_escape_is_rejected` 覆盖——进程 cwd 由 `getcwd` 给出物理路径，TUI 层看不到符号链接代理，该场景无法在 TUI 层观测）
- [x] Git 缺失、旧版本、权限拒绝、慢响应分别验证；记录实际调用次数和等待阶段，避免把静态最坏预算写成实测耗时。（缺失见「修复记录」第 2 条端到端；权限拒绝与调用次数见第 3 条假 Git 判别用例——当时的计数是目录模式 2 轮、仓库模式 6 次 Git 调用、已有会话复核 2 轮，全部在写事务外，现行为第 14 条收敛后的目录模式 1 轮、仓库模式 3 次、已有会话 1 轮；旧版本见第 11 条（假 Git 拒绝新选项）与第 13 条（真实 Git 2.4.12 二进制：仓库、普通目录、子目录与现代 Git 建的 linked worktree 四种组合均可建会话，并修掉实测暴露的大小写分类缺陷）；慢响应为实测时间线——假 Git 每次调用固定等待 400ms 时准入总耗时 2.938s、首次到末次调用 2.148s、6 次调用全部记录为写锁空闲，挂起 Git 实测 5.003s 后以类型化超时错误结束。30s = 6 次 × 5s 是第 14 条之前的静态上限（现为 15s），不是实测耗时；同一条链路在真实 TUI 上的慢 Git 实测见第 12 条——输入到回复 2.489s、窗口内 6 次发现调用、无重复入队，第 14 条后同一用例的窗口内调用降到 3 次、输入到回复 1550ms）
- [x] 事务内没有无界或重复的外部探测；慢准备、取消和超时不造成输入丢失或重复执行。（前半见「修复记录」第 3 条；后半见第 6 条：慢准备仍被受理且只入队一次、准备超时恢复原稿且不发送入队请求、准备期间取消不产生投递，均为 consumer 级回归；回执超时的「未知结果按同身份重试」沿用既有用例 `test_steer_uncertain_receipt_retries_identical_command_and_input`）
- [x] 身份模型调整保留已有消息、frozen snapshot、绑定关系和执行状态；冲突处理可理解、可恢复。（「可理解」2026-09-19 实施：绑定失败文案指出可完成的下一步，输入路径复述服务端原因而非表述为输入被拒，见「修复记录」第 5 条；「可恢复」按第 4 条的新会话语义覆盖；保留性证据见第 8 条：schema 4→5 迁移前后逐字节比对绑定 / 执行状态与线程行 / 消息，并对 frozen snapshot 做存储层读取。把已有会话改指到新位置的入口仍未提供，属设计 §5.4 的明确限制）
- [x] 简化后继续通过错误 cwd、配置/权限隔离、跨进程 owner 竞争及 dirty 状态保护测试。（2026-09-19 复核：错误 cwd 见 `legacy_adoption_rejects_changed_cwd_child_and_lost_native_binding`；配置 / 权限隔离见 `worktree_new_resources_use_the_target_directory`、`worktree_scheduled_approval_uses_session_permission_and_rejects_closed_owner`、`test_update_config_refreshes_existing_owner_environments`；跨进程 owner 竞争见 `test_worktree_execution_competes_across_processes_and_crash_remains_dirty`、`test_worktree_execution_child_process`；dirty 保护见 `test_worktree_dirty_reset_held_stale_and_exact_generation`、`test_worktree_cancelled_mutation_remains_dirty_and_cannot_publish_clean`、`test_worktree_clean_waits_for_admitted_mutation_before_releasing_os_ownership` 及 ACP 侧 `test_workspace_dirty_recovery_original_load_and_frozen`、`test_dirty_reset_then_failing_reload_keeps_store_state_and_original_id`、`test_dirty_reset_without_initialize_is_rejected_without_store_effect`。全量 `cargo test -p peri-resources --lib` 138 项、`cargo test -p peri-acp --lib` 678 项通过）

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
| 2026-09-19 | — | Open（部分修复） | agent | 绑定失败文案改为指出可完成的下一步，输入路径在会话未能建立时复述服务端原因而非表述为输入被拒（见「修复记录」第 5 条） |
| 2026-09-19 | — | Open（部分修复） | agent | 准备阶段与受理回执分开计时：慢准备不再被 10 秒回执预算判成输入被拒，准备超时按未受理恢复原稿；勾选验收条件第 7 项（见「修复记录」第 6 条） |
| 2026-09-19 | — | Open（部分修复） | agent | Git 发现去掉版本相关选项：位置解析不再用 `--path-format=absolute` / `--absolute-git-dir` 并按 cwd 还原相对输出，`worktree list` 不支持 `-z` 时退回换行分隔（见「修复记录」第 7 条） |
| 2026-09-19 | — | Open（部分修复） | agent | 补齐 schema 4→5 迁移的保留性断言：迁移前后逐字节比对线程行 / 消息（含 frozen snapshot）并做存储层读取，变异检验通过；勾选验收条件第 8 项（见「修复记录」第 8 条，生产代码未变） |
| 2026-09-19 | — | Open（部分修复） | agent | 三条路径的探测边界补实测断言：历史只读访问 0 次 Git 调用（登记目录删除后仍可读），已有会话复核恰好一轮观测且全在写事务外；勾选验收条件第 3 项（见「修复记录」第 9 条，生产代码未变） |
| 2026-09-19 | — | Open（部分修复） | agent | 复核简化后的既有保护链路：错误 cwd、配置 / 权限隔离、跨进程 owner 竞争与 dirty 状态保护用例全部通过（resources 138 项、acp 678 项）；勾选验收条件第 9 项（本轮无代码改动） |
| 2026-09-19 | — | Open（部分修复） | agent | 新增仓库布局端到端用例：真实 TUI 上验证子目录 / linked worktree / 独立 clone 的执行目录与项目归属，变异检验通过；勾选验收条件第 5 项（见「修复记录」第 10 条，生产代码未变） |
| 2026-09-19 | — | Open（部分修复） | agent | 旧版 Git 的 common dir 改为读 `commondir` 文件推导（不再请求 Git 2.5 引入的 `--git-common-dir`），未知选项回显按不兼容处理，`worktree` 子命令缺失时只跳过交叉核对；慢 Git 实测等待时间线与单次超时边界，勾选验收条件第 6 项——验收条件 9 项至此全部勾选（见「修复记录」第 11 条） |
| 2026-09-19 | — | Open（部分修复） | agent | 慢 Git 补端到端验收：真实 TUI / ACP 路径上每次 Git 调用固定等待 300ms，输入到回复 2.489s、6 次发现调用、无重复入队，观测仍为仓库模式（见「修复记录」第 12 条，生产代码未变） |
| 2026-09-19 | — | Open（部分修复） | agent | 真实旧版 Git 2.4.12 二进制验收：仓库、普通目录、子目录与现代 Git 建的 linked worktree 均可建会话；实测暴露大小写敏感的分类缺陷（旧版 `fatal: Not a git repository…` 被判成类型化发现错误，普通目录在那个版本上完全建不了会话），已修复并补回归用例，真实二进制变异检验通过（见「修复记录」第 13 条） |
| 2026-09-19 | — | Open（部分修复） | agent | 准入内不再重复完整发现：新增 `reassert_session_binding` / `BindingCheck::Recorded` / `expect_directory`，一次准入至多一次完整发现（`session/new` 6 轮 → 1 轮，慢 Git 端到端窗口内发现调用 6 → 3、输入到回复 2489ms → 1550ms），跨层用例加上限断言（见「修复记录」第 14 条） |
| 2026-09-19 | — | Open（部分修复） | agent | 绑定复核的执行目录改为 `binding_cwd`，与登记解析返回同一文本形式：同一个目录不再因 `join("")` 产生的尾分隔符被当成换了目录重复解析（新增文本形式断言，变异检验通过；见「修复记录」第 15 条） |
| 2026-09-19 | — | Open（部分修复） | agent | 补上会话建立期间的用户可见状态：`new_session_under_gate` 全程置位 `SESSION_PREPARING`、`Drop` 清除，状态栏显示「正在准备会话」，建立结束即消失；端到端实测提示可见 1019ms，两处变异检验（不置位 / 不清除）在单元与 e2e 两层都被抓到（见「修复记录」第 16 条） |
| 2026-09-19 | — | Open（部分修复） | agent | 按代码证据完成简化目标 3 的重新论证，并给出简化目标 5 按「新会话语义」收口的处置建议；同时把验收条件 3 / 6 与第 9 条里已被第 14 条取代的调用计数同步为现行值（本轮未改代码，「简化目标 3、5」的收口仍需产品决策） |
| 2026-09-19 | — | — | agent | 独立核实的实测环境观察：冷启动后按默认并行执行 `cargo test -p peri-resources --lib`，单次 5 秒 Git 预算在负载下被击穿，出现 7 例 `Git discovery timed out`（139 passed / 7 failed）；同一命令串行（`--test-threads=1`）与热态并行均为 146 passed / 0 failed。反复核对后判定波动来自测试装置（假 Git 每次调用都要启动测试二进制）与机器负载的叠加，未观察到生产路径回归 |
| 2026-09-19 | — | — | agent | 据独立核实回填：补记 `7f90c11d` 为「修复记录」第 17 条并同步状态行；修正「已确认事实」里 spawn 的机制描述（第 98 行）与准入调用计数（第 100 行）；标注第 14 条引用的 `run-2026-09-19T07-51-47` 产物缺失；校正 `peri-resources` 测试计数推导链（第 14、15 条）。未改状态、优先级、验收勾选与其它既有结论 |

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

- Git 版本差异（旧版 `--path-format=absolute` 等）、权限拒绝、慢响应仍未实测；验收条件第 6 项待办。（版本差异部分见「修复记录」第 7 条；仍无真实旧版 Git 二进制证据）
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
- 仍没有把已有会话改指到新位置的入口：用户可完成的是「在当前目录建立新会话」，旧会话保持只读历史且执行失败关闭。该前进路径的文案见第 5 条。
- 同路径替换（删除重建）只做了单元测试，未做真实 TUI 端到端：运行中的 TUI 其进程 cwd 已被删除，`getcwd` 语义与登记语义无关，不适合作为该场景的端到端入口。
- 备份恢复（restore）按同路径替换路径覆盖，未单独实测。

### 2026-09-19：绑定失败文案与输入路径的原因提示（第 5 条）

**范围**：只改用户可见的失败文案与输入路径的提示语义。不新增重关联入口、不改变绑定复核强度与 `NeedsRelink` 的触发条件、不动 `steer_consumer` 包住整个 `execute` 的 10 秒期限（验收条件第 7 项后半仍未实施）。

**根因**：`WorkspaceError::NeedsRelink` 的文案要求 `explicit relinking`，而产品没有该操作；TUI 输入路径又在任何失败下都只显示 `steer-input-rejected`。2026-09-19 本机日志里的 84 次 `code=-32010` 都对应这条通用提示，用户无法区分「输入被拒」与「会话根本没建立」。

**改动**：

- `peri-acp-types/src/workspace.rs`：`NeedsRelink` 文案改为 `session directory changed; this session cannot continue here: start a new session in the current directory`，只陈述事实与用户实际可完成的下一步；恢复路径（`session-restore-failed`）本就展示错误正文，因此该文案同时改善恢复失败的提示。
- `peri-tui/src/kit/steer_consumer.rs`：`execute` 返回 `SteerFailure { error, stage }`，`SteerStage` 区分会话建立（`Prepare`，含 `ensure_session` 内部完成的初次快照）与入队（`Admit`）；`failure_notice` 在 `Prepare` 阶段复述服务端给出的原因（新 key `steer-session-unavailable`），入队被拒与回执不明沿用原两条结论。
- `peri-tui/locales/{en,zh-CN}/main.ftl`：新增 `steer-session-unavailable`（`会话未能建立：{ $error }。原稿已保留。`）。
- e2e：三个工作区场景原有断言只挡 `Input was not accepted`；提示拆分后该断言不再覆盖会话建立失败，补挡 `Session could not be established`。

**回归测试**（修复前失败）：

| 验证 | 结果 |
| --- | --- |
| `peri-acp-types`：`test_needs_relink_message_states_a_reachable_next_step` | 修改前失败：`文案不得要求产品中不存在的操作：workspace identity changed; explicit relinking is required`；修改后通过，`cargo test -p peri-acp-types --lib` 423 项全绿 |
| `peri-tui`：`test_failure_notice_distinguishes_preparation_from_admission` | 新增：准备阶段复述服务端原因，入队被拒 / 未知回执保持原结论 |
| `peri-tui`：`test_steer_session_unavailable_notice_is_translated_in_both_locales` | 未加 FTL key 时失败（`en 缺少 steer-session-unavailable 文案`），两份 FTL 补齐后通过 |
| `peri-tui`：`test_steer_initial_new_session_failure_recovers_unsubmitted_draft` 与其 snapshot 变体 | 未加文案时失败（`准备会话失败必须说明服务端给出的原因`）；编写断言时同时纠正一处模型错误：初次快照 RPC 在 `ensure_session` 内，两种准备失败都属准备阶段 |
| `cargo test -p peri-tui --lib -- steer` | 54 项通过 |
| `cargo test -p peri-tui --lib` | 1634 通过 / 2 失败：两个 macOS 剪贴板用例（`kit::input_area::image::tests::macos::clipboard_*`）在全量并行运行下争用系统 pasteboard，单独运行 5 项全过，与本改动无关 |
| `cargo fmt --all -- --check` | 无格式差异 |
| `cargo clippy -p peri-tui -p peri-acp-types --all-targets -- -D warnings` | 被 `peri-tui/src/kit/popups/dirty_recovery_test.rs`（`2c243a9d` 的在途工作，本改动未触及）的 4 个 lint 挡住；屏蔽 `clippy::bool_assert_comparison`、`clippy::clone_on_copy` 后本改动无告警 |
| E2E `workspace-git-init` / `workspace-no-git` / `workspace-directory-moved` / `steer-queue-live` | `run-2026-09-19T05-05-35`、`05-05-47`、`05-06-03`、`05-06-15` 各 1/1 通过（12s / 15s / 9s / 14s，串行、无重试） |

**事实源同步**：[工作区身份设计](../../docs/design/session-workspace-identity.md) §5.4（失败原因与前进路径随错误呈现；会话未建立的失败发生在输入受理之前，其提示不得表述为输入被拒）；`docs/code-index/peri-tui.md` 待发送投影行（`steer-session-unavailable`）。

**遗留**：

- 把已有会话改指到新位置的入口仍未提供（设计 §5.4 明确初始交付不提供）：用户可完成的是在当前目录建立新会话。
- 启动时 `entry.rs` 的首次建会话失败仍只写日志；用户在第一次发送时才看到原因。「准备阶段可见状态」属简化目标第 4 项剩余部分，未实施。
- 未新增覆盖该文案的真实 TUI 端到端用例：现有场景都在成功路径上，失败路径需要注入会话建立失败。

### 2026-09-19：准备阶段与受理回执分开计时（第 6 条）

**范围**：只处理已确认事实「首次输入准备阶段与回执共用期限」，即验收条件第 7 项后半与简化目标第 4 项中的期限部分。不改变输入身份、命令去重、未知回执的同身份重试、取消语义与 `NeedsRelink` 判定；不新增准备期间的用户可见状态（简化目标第 4 项剩余部分仍未实施）。

**根因**：`peri-tui/src/kit/steer_consumer.rs::spawn_steer_consumer` 用单个 `RECEIPT_TIMEOUT`（10 秒）包住整个 `execute`，其中包含 `ensure_session`——它要在同一窗口内完成工作区发现、服务端建会话、等待 operation gate 与初次快照。准备慢于 10 秒时，失败发生在 enqueue RPC 之前，却按「已发出请求」的结论收尾：输入从未发出，用户却读到输入未被受理。

**改动**（`peri-tui/src/kit/steer_consumer.rs`）：

- 新增 `PREPARE_TIMEOUT = 60s`，与 `RECEIPT_TIMEOUT = 10s` 分开：`execute` 拆为 `prepare`（准备首会话）与 `admit`（核对实例身份、发送请求、落定回执）两段，各自计时。准备阶段不再被回执预算提前放弃，已发出的请求也不因准备耗时被误判。
- `prepare` 只在「会话尚未绑定且是入队命令」时运行；其失败（含超时，`-32603 session preparation timed out`）标记 `SteerStage::Prepare`，走既有「确定未受理」路径恢复完整原稿。
- consumer 的 `select!` 只保留 shutdown 取消：期限不再兼作取消入口，准备超时的输入不会留在无法撤回的 Submitting 状态（TakeBack 只接受 Queued）。

**回归测试**（`peri-tui/src/kit/steer_consumer_test.rs`，`start_paused = true` 配真实常量，修复前失败）：

| 验证 | 结果 |
| --- | --- |
| `test_slow_session_preparation_is_not_capped_by_receipt_deadline` | 服务端 `session/new` 慢于 `RECEIPT_TIMEOUT`（+5s）但仍在准备期限内应答：输入照常入队、只入队一次、无失败提示，队列行保持可见 |
| `test_session_preparation_timeout_recovers_draft_without_admission` | 服务端不应答 `session/new`：提示为准备阶段结论（含 `timed out`）而非 `steer-input-rejected`，不发出 enqueue；`recover` 返回完整原稿（多行 + 图片引用） |
| `test_shutdown_during_preparation_keeps_unadmitted_input_identity` | 准备期间关闭：不发出任何请求，未受理输入保持原身份，不变成已发送或可重复投递 |
| 变异验证 | 临时恢复「10 秒包住整个 `execute`」后三个用例全部失败（准备超时用例因结论被写成输入被拒而失败），回退后全绿 |
| `cargo test -p peri-tui --lib -- kit::steer_consumer::tests` | 9 项通过 |
| `cargo test -p peri-tui --lib` / `--doc` | 1639 通过 / 0 失败；doc 无用例 |
| `cargo fmt --all -- --check` | 无格式差异 |
| E2E `workspace-directory-moved` / `workspace-git-init` / `workspace-no-git` / `steer-queue-live` | `run-2026-09-19T05-21-52` 4/4 通过（50s，串行、无重试） |

**事实源同步**：[用户待发送队列](../../docs/design/user-input-queue.md)（准备与已发出请求的回执各自计时）；`docs/code-index/peri-tui.md` 待发送投影行；[兼容性待办](2026-09-17-platform-compatibility.md) 删除该行并在关联 P0 指向本条。

**遗留**：

- 准备期间的用户可见状态仍未提供：慢 host 上输入停留在 Submitting，60 秒内用户看不到「正在建立会话」的进度，也无法主动撤回（TakeBack 只接受 Queued）。该可见状态是简化目标第 4 项的剩余部分。
- 60 秒是常量取舍而非测量结果：取有界值是避免无期限等待把输入留在无法撤回的状态；未在慢 host 上实测准备耗时的分布，也未区分发现 / 建会话 / gate 各段的耗时占比。

### 2026-09-19：Git 发现去掉版本相关选项（第 7 条）

**范围**：只解除「旧版 Git 因未知选项而无法完成发现」这一通道，即已确认事实「Git 发现依赖一组命令和输出约定」。不改变文件对象身份模型、登记键、`NeedsRelink` 判定与 `git_answered` 语义，也不新增 Git 版本提示或降级开关。**未运行真实旧版 Git**：最低支持版本仍未确定，本条的旧版证据是拒绝这些选项的假 Git 脚本。

**根因**：发现的每条命令都只认新版选项——`rev-parse --path-format=absolute`、`--absolute-git-dir`，以及 `worktree list --porcelain -z`。把未知选项当致命错误的 Git（本机 Git 2.39 对未知选项退出 129、stderr 打印 `usage:`；`rev-parse --bogus` 则退出 0 并把未知参数当修订回显，因此不能用退出码判断 rev-parse 的选项支持）会在这里直接失败：目录其实在仓库中，用户拿到的却是 `Git location discovery failed`，与无 Git、权限拒绝同属「建不了会话」这类 P0 表现。

**改动**（`peri-resources/src/sessions/sqlite_store/discovery.rs`）：

- `observe_with_git`：仍只启动一次 `rev-parse`，参数改为 `--show-toplevel --git-common-dir --git-dir`；`git_paths` 对非绝对输出先与 cwd 组合再 canonicalize。本机实测（Git 2.39）同一命令的输出形式随 cwd 变化：主仓库根给 `.git`，子目录的 `--git-common-dir` 给 `../../.git`，而同一子目录的 `--git-dir` 反而给绝对路径——两类输出混排，只有逐个判断才能还原。
- 新增 `git_worktree_listing`：优先 `worktree list --porcelain -z`；仅当非零退出且 stderr 含 `usage:` 时退回 `--porcelain`（换行分隔）。新增 `is_usage_error` 承担该判定；真实失败不退回，仍报 `Git worktree membership is inconsistent`。成员判定按调用方给出的字节分隔符切分后做完整路径精确比对，退回只改变分隔符。

**回归测试**（`discovery_test.rs`，修复前失败）：

| 验证 | 结果 |
| --- | --- |
| `test_worktree_legacy_git_discovers_repository_without_version_options` | 假 Git 把 `--path-format=*` 与 `-z` 当未知选项（打印 `usage:`、退出 129），其余转交真实 Git。修复前主仓库即失败于 `Git location discovery failed`；修复后主仓库 / linked worktree / linked worktree 子目录的 `Discovery` 与真实 Git 的观测逐一相等，且调用日志不含 `--path-format`、同时含 `--porcelain -z` 与退回后的 `--porcelain` |
| `test_worktree_listing_failure_is_not_retried_as_legacy_git` | 假 Git 让 `worktree list` 以退出 128 失败：报错不变，且 `worktree list` 只被调用 1 次（非用法错误不触发退回） |
| `test_worktree_git_paths_resolve_relative_output_against_cwd` | 子目录观测的 `root` / `common_dir` / `private_dir` 与仓库根观测相等，且断言为字面路径 `<repo>/.git`；变异检验（去掉按 cwd 还原）后该用例失败于 `Unavailable` |
| `cargo test -p peri-resources --lib` | 134 项通过（改动前 131 项） |
| `cargo test -p peri-acp --lib` | 678 项通过 |
| `cargo clippy -p peri-resources --all-targets -- -D warnings`、`cargo fmt --all -- --check` | 无告警、无格式差异 |
| E2E `workspace-git-init` / `workspace-no-git` / `workspace-directory-moved` | `run-2026-09-19T05-44-11`、`05-44-41`、`05-45-01` 各 1/1 通过（13s / 17s / 10s，串行、无重试） |

**事实源同步**：[工作区身份设计](../../docs/design/session-workspace-identity.md) §3.1（命令契约、按 cwd 还原相对输出、`-z` 退回条件）；`docs/code-index/peri-resources.md` 工作区身份行；[兼容性待办](2026-09-17-platform-compatibility.md)「旧 Git 命令能力未覆盖」条目。

**遗留**：

- 仍无真实旧版 Git 证据：本机只有 Git 2.39，最低支持版本、各选项的实际引入版本与旧版在真实仓库中的行为都未验证；现有用例只能证明「不依赖这些选项」这一性质。要宣称支持版本，需要装旧版二进制或在 CI 里准备旧版 fixture。
- 慢响应与等待阶段已由第 11 条实测；本条只处理版本差异。
- 退回路径的代价未量化：换行分隔无法表示含换行的路径（该情形下成员判定会精确比对失败并报错，不会静默误判），但也没有覆盖用例构造这样的路径。

### 2026-09-19：schema 4→5 迁移的保留性断言（第 8 条，生产代码未变）

**范围**：只补齐验收条件第 8 项中「身份模型调整保留已有消息、frozen snapshot」的断言。不改变迁移实现、登记键与绑定语义；生产代码无改动。

**缺口**：`test_version4_upgrade_relaxes_registration_keys_and_preserves_rows` 原本只比对 `projects` / `workspaces` / `session_bindings` / `execution_runs` 的行内容，并检查 `load_messages` 条数为 1；它没有比对 `threads` 行本身，因此没有覆盖迁移重建登记表时最可能连带丢失的 `frozen_context`。4→5 的实现会 `DROP TABLE projects` / `workspaces` 并在提交前做 `foreign_key_check`，历史是否原样保留此前靠「迁移不触碰这两张表」推断，没有断言。

**改动**（`peri-resources/src/sessions/sqlite_store/schema_test.rs`：测试与文档注释）：

- 迁移前记录 `history_bytes`（`threads` 全列 + `messages` 行），迁移后用只读连接再次记录并逐一比对：`frozen_context`、`cached_context`、`config`、`message_count` 等列与消息行都在其中。
- 增加存储层读取断言：迁移后 `store.load_frozen_snapshot` 仍返回 `frozen-owner-state`，把不变量表述为「可读取的 frozen」而不只是「字节恰好相同」。

**验证证据**：

| 验证 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib -- test_version4_upgrade_relaxes_registration_keys_and_preserves_rows` | 通过（0.42s） |
| 变异检验 | 在迁移里临时加 `UPDATE threads SET frozen_context = NULL`：该用例失败于「迁移不得丢失 frozen snapshot」（`left: None` / `right: Some("frozen-owner-state")`），回退后通过 |
| `cargo test -p peri-resources --lib` | 134 项通过 |
| `cargo fmt --all -- --check` | 无格式差异 |

**遗留**：

- 只覆盖 4→5 这一条迁移路径；2→3、3→4 的历史保留由各自用例覆盖，未逐条比对 `threads` 行。
- 未覆盖真实二进制在旧库上的端到端行为：`legacy-history-upgrade` e2e 断言 schema 版本与列表可见性，不比对 frozen 内容。

### 2026-09-19：三条路径的探测边界断言（第 9 条，生产代码未变）

**范围**：补齐验收条件第 3 项的实测证据。不改变发现、登记与复核实现；生产代码无改动。

**缺口**：「新会话」的调用次数与持锁状态在第 3 条已有断言，另两条路径没有：历史只读访问（列表 / 消息 / frozen / 绑定）此前只有「不要求目录可用」的表述，没有「不调用 Git」的证据；已有会话恢复执行前的绑定复核（`session/load` → `validate_expected`）没有记录调用次数，重复发现一类回归只能靠读代码发现。

**改动**（`peri-resources/src/sessions/sqlite_store/workspace_test.rs`：两个子进程模式 + 两个断言用例，复用第 3 条的假 Git 与日志装置）：

- `test_worktree_history_access_never_probes_git` + 子进程 `test_worktree_history_access_child`：假 Git 语境下完成一次准入并写入一条历史，随后删除登记目录，再在子进程里分别以读写与只读方式打开存储，读绑定、meta、消息、frozen 与项目 / 精确目录两种列表；父进程比对假 Git 日志行数不变。子进程必须成功且 stdout 含 `running 1 test`，否则用例只覆盖了「子进程提前返回」的空断言。
- `test_worktree_bound_session_validation_probes_once` + 子进程 `test_worktree_bound_session_validation_child`：真实 Git 前置下完成准入，再在子进程里执行 `validate_session_binding`；日志尾部必须恰好 3 条（一轮观测），且这 3 次调用时写锁都空闲。

**验证证据**：

| 验证 | 结果 |
| --- | --- |
| 历史只读访问 | 0 次 Git 调用；登记目录已删除仍读到 1 条绑定、1 条消息与两种范围的列表条目（读写与只读打开各一遍） |
| 已有会话复核 | 恰好 3 次调用（一轮观测），全部记录为写锁空闲 |
| 变异检验（历史路径） | 在 `list_scoped_threads_impl` 里临时加 `discovery::observe("/")`：用例失败于「历史只读访问不得调用 Git：调用次数 2 → 6」，回退后通过 |
| 变异检验（已有会话） | 在 `validate_session_binding_impl` 里临时重复一次完整复核：用例失败于「已有会话复核只应观测一轮（三条命令）」`left: 6` / `right: 3`，回退后通过 |
| `cargo test -p peri-resources --lib -- --test-threads=1` | 138 项通过（新增 4 项） |
| `cargo clippy -p peri-resources --all-targets -- -D warnings`、`cargo fmt --all` | 无告警、无格式差异 |

**ACP 侧静态核对**（未新增运行时用例）：`session/list` 只调用 `list_scoped_threads`，`session/metadata` 只调用 `load_meta` 与 `load_session_binding`，两者都不经 `validate_session_binding` / `resolve_workspace`；`session/load` 经 `acquire_for_load` → `validate_expected` 复核绑定，即上面第二项实测的路径。

**遗留**：

- 「新会话」的调用次数证据沿用当时第 3 条的计数（目录模式 2 轮 / 仓库模式 6 次 Git 调用），本条未重复测量；第 14 条把一次准入收敛为一轮后，现行计数是目录模式 1 轮、仓库模式 3 次。
- 旧版 Git、权限拒绝与慢响应当时仍未实测，「等待阶段」只有持锁状态这一项证据；慢响应与等待阶段已由第 11 条补上。

### 2026-09-19：仓库布局的端到端验收（第 10 条，生产代码未变）

**范围**：补齐验收条件第 5 项的用户可见证据。不改变发现、登记与绑定实现；生产代码无改动。

**缺口**：主仓库 / linked worktree / 独立 clone / 子目录的身份与执行目录此前只有 resources 单元测试证据（`test_worktree_main_linked_subdirectory_and_clone_identity` 等）。单元测试断言的是 `resolve_workspace` 的返回值，没有经过 ACP `session/new` 与 TUI 启动路径，也没有断言「项目展示」所依赖的事实——线程浏览器按 `project_id` 分组、项目定位取 common dir。

**改动**：新增 `e2e/tests/scenarios/workspace-worktree-layouts.test.ts`（本地 SSE 模型端点，无真实凭据、无外部 judge），在真实 TUI 上依次启动三个布局，每次都走完整 `session/new` 准入：

- **子目录**（`<repo>/sub`）：执行目录是子目录本身（`threads.cwd`），工作区根是仓库根，`relative_cwd = sub`，观测快照的 `common_dir` 是仓库 `.git`。
- **linked worktree**（`git worktree add`）：工作区根是 worktree 路径且 `relative_cwd` 为空，`project_id` 与子目录会话相同、`workspace_id` 不同，`common_dir` 仍指向主仓库 `.git`。
- **独立 clone**：工作区根是 clone 路径，`project_id` 与源仓库不同，`common_dir` 在 clone 自己身上。
- 收尾断言：项目 2 个（仓库 + clone）、工作区 3 个、三个会话的历史 cwd 分别是各自的启动目录。

**验证证据**：

| 验证 | 结果 |
| --- | --- |
| E2E `workspace-worktree-layouts.test.ts` | `run-2026-09-19T06-01-42` 1/1 通过（11s，串行、无重试） |
| 变异检验 | 把 `Discovery::project_locator` 从 common dir 改为 private dir：`run-2026-09-19T06-03-21` 失败于「同一仓库的 worktree 属于同一项目」（两个不同的 `project_id`），回退后 `run-2026-09-19T06-04-08` 通过 |
| 既有 resources 单元测试（138 项） | 通过，含主仓库 / 子目录 / clone / symlink 身份用例 |

**遗留**：

- symlink 场景无法在 TUI 层观测：启动进程的 cwd 由 `getcwd` 给出物理路径，符号链接代理在 `session/new` 之前已被解析，该场景仍由 resources 单元测试覆盖。
- 新用例未加入 L0：当前 L0 串行运行已约 7.5 分钟，本用例需要三次 TUI 启动；它只在 L2 / release 全量中运行。

### 2026-09-19：旧版 Git 的 common dir 推导与慢响应实测（第 11 条）

**范围**：补上验收条件第 6 项余下的两部分——common directory 的推导不再请求新版选项，以及慢响应 / 等待阶段的实测。不改变登记键、绑定复核、`NeedsRelink` 判定与目录模式语义。

**缺口**（承接第 7 条遗留）：第 7 条把位置解析改成 `rev-parse --show-toplevel --git-common-dir --git-dir`，仍在请求一个 Git 2.5 才引入的选项；慢响应完全没有实测，30s（两轮 × 三条命令 × 5s 单次超时）只是静态上限。

**改动**（`peri-resources/src/sessions/sqlite_store/discovery.rs`）：

- `observe_with_git` 只请求 `rev-parse --show-toplevel --git-dir`（两个位置共用一次进程启动）；新增 `common_directory`，按 [gitrepository-layout](https://git-scm.com/docs/gitrepository-layout) 的语义读 `$GIT_DIR/commondir` 文件（相对内容按 `$GIT_DIR` 解析）推导 common directory，文件不存在时 common 与 private 相同。linked worktree 的该文件由 `git worktree add` 写入、主工作树没有它——本机 Git 2.39 实测：linked worktree 内容为 `../..`，canonicalize 后与 `rev-parse --git-common-dir` 一致。
- `git_paths` 拒绝以 `--` 开头的位置行：`rev-parse` 把不认识的选项当普通参数回显到 stdout（本机 Git 2.39 实测退出码 0），不拦住就会被当成相对路径拼在 cwd 下，用「目录不可用」掩盖版本问题。
- `git_worktree_listing` 拆为返回 `WorktreeMembership` 的 `git_worktree_membership`：子命令或 `--porcelain` 整体缺失（`is_missing_command`，或退回后仍是用法错误）时不做成员交叉核对——位置已由 `rev-parse` 回答，跳过的是交叉核对，真实失败（权限、损坏仓库、非零退出）仍原样上报。

**回归测试**：

| 验证 | 结果 |
| --- | --- |
| `test_worktree_common_dir_matches_git_reported_locations` | oracle：主仓库根、含空格子目录、linked worktree 三种 cwd 下的 private / common 与真实 Git 的 `rev-parse --git-dir` / `--git-common-dir` 逐一相等（linked worktree 的 common 是主仓库 `.git`） |
| `test_worktree_path_discovery_rejects_unknown_option_echoes` | 复现旧版回显行为（stdout 出现 `--git-common-dir`，退出码 0）：报类型化 `DiscoveryError`，不被当成相对路径 |
| `test_worktree_ancient_git_needs_no_common_dir_option_or_worktree_command` | 把 `--git-common-dir` / `--path-format` / `--absolute-git-dir` 当未知选项、把 `worktree` 当未知子命令的假 Git 下，观测与真实 Git 相等 |
| `test_worktree_slow_git_wait_is_measured_per_call_outside_the_write_lock` | 假 Git 每次调用前固定等待 400ms：准入成功，6 次调用（两轮 × 三条命令）全部记录为写锁空闲；实测准入总耗时 2.938s、首次到末次调用 2.148s，等待阶段按到达时刻落在具体命令上（两条 `rev-parse` 与一条 `worktree list` 各一轮） |
| `test_worktree_hanging_git_ends_within_the_call_budget` | `exec sleep 600` 的假 Git：实测 5.003s 后以 `Git discovery timed out` 结束，没有走到 30s 静态上限，也不降级为目录模式 |
| 变异检验（common dir） | `common_directory` 改为直接返回 private dir：oracle 用例失败于 `left: …/repository/.git/worktrees/linked-tree` / `right: …/repository/.git`，回退后通过 |
| 变异检验（调用次数） | `worktree list` 成功分支临时多发一次调用：慢响应用例失败于 `left: 8` / `right: 6`，第 3 条的调用次数用例同时失败，回退后通过 |
| 变异检验（超时预算） | 单次超时预算临时改为 100ms：挂起用例失败于「实测 103.97ms 短于单次预算：没有真正触发超时路径」，回退后通过 |
| `cargo test -p peri-resources --lib` | 143 项通过（改动前 141 项） |
| `cargo test -p peri-acp --lib` | 678 项通过 |
| E2E `workspace-git-init` / `workspace-worktree-layouts` / `workspace-no-git` | `run-2026-09-19T06-23-17`、`06-24-05`、`06-24-16` 各 1/1 通过（7s / 8s / 17s，串行、无重试）：真实 TUI 上「普通目录 `git init`」「子目录与 linked worktree 同项目分组」「无 Git 目录建会话」三条路径都经过新的 common dir 推导 |
| `cargo clippy -p peri-resources --all-targets -- -D warnings`、`cargo fmt --all` | 无告警、无格式差异 |

**事实源同步**：[工作区身份设计](../../docs/design/session-workspace-identity.md) §3.1（命令契约改为两个位置、common dir 由 `commondir` 推导、未知选项回显按不兼容处理、单次调用超时预算、`worktree` 子命令缺失时的交叉核对行为）；`docs/code-index/peri-resources.md` 工作区身份行；[兼容性待办](2026-09-17-platform-compatibility.md)「旧 Git 命令能力未覆盖」条目。

**遗留**：

- 仍未运行真实旧版 Git（本机只有 2.39）：最低支持版本未确定。`commondir` 文件在各旧版中是否都存在同样只在文档与假 Git 层面验证；若某个版本不写该文件，linked worktree 会各自成项目（不阻断使用，但项目分组退化），宣称支持版本前需实测或查证。（真实旧版二进制由第 13 条补上：Git 2.4.12 实测，`commondir` 在新版 Git 建的 linked worktree 上存在但旧版 Git 本身读不了该 worktree，按普通目录降级）
- 实测是本机单次运行的值（macOS，含每次调用约 100ms 的进程启动与 SQLite 探测开销），不是跨机器的性能保证；两轮共 6 次的静态上限 30s 仍然只是预算。
- 慢响应的端到端由第 12 条补上：TUI / ACP 全链路用同一个慢 Git 脚本实测，输入到回复 2.49s、窗口内 6 次发现调用、无重复入队。

### 2026-09-19：慢 Git 的端到端验收（第 12 条，生产代码未变）

**范围**：补上第 11 条遗留的「慢响应只在 resources 层验证，没有走 TUI / ACP 路径」。验收条件第 7 项要求「慢准备不造成输入丢失」，这是跨层结论，单进程单元测试不能代表（[testing](../../docs/standards/testing.md)）。生产代码无改动——第 11 条的每次调用超时预算与第 6 条的准备 / 回执分开计时由已有实现提供。

**改动**：

- 新增 `e2e/tests/scenarios/workspace-slow-git.test.ts`（已入 L0）。PATH 用 `env -i` 构造：`/usr/bin`、`/bin` 逐项软链过去但跳过 `git*`，再写一个每次调用先记录参数、固定等待 300ms、最后 `exec` 真实 Git 的 `git` 脚本；被测目录是真实仓库（`git init`）。
- **前提守卫**：用例自己先在该 PATH 下跑一次 `git rev-parse --is-inside-work-tree`，断言耗时 ≥ 300ms。没有这一步，PATH 未被应用时用例会静默退化成普通 Git 路径——`workspace-no-git` 已记录过这个退化方式（tmux `-e PATH=` 会被会话 shell 覆盖）。
- 观测窗口以调用日志的字节偏移为界：输入前记录偏移，收到模型回复后读取新增行，按到达顺序区分发现调用（`rev-parse --is-inside-work-tree`、`rev-parse --show-toplevel`、`worktree list`）。

**验证证据**：

| 验证 | 结果 |
| --- | --- |
| E2E `workspace-slow-git` | `run-2026-09-19T06-29-28` 1/1 通过（16s）。实测：输入到回复 **2489ms**，窗口内 Git 调用 7 次（其中发现调用 6 次、正好两轮三条命令），注入等待每次 300ms；屏幕上没有 `Input was not accepted`，也没有会话未能建立的提示 |
| 同上（数据库因果） | 登记后 1 project / 1 workspace / 1 binding；观测为仓库模式（`common_dir`、`private_dir` 均非空，`root` 是仓库根），说明慢 Git 仍被识别为 Git，而不是降级成目录模式；一次输入对应 1 次模型请求、user / assistant 各 1 条 |
| 同上（版本契约） | 全部调用中不含 `--path-format` / `--git-common-dir` / `--absolute-git-dir`：第 7、11 条的兼容约束在真实 TUI 路径上同样成立 |
| 变异检验（超时预算） | 单次预算临时改为 100ms（小于注入的 300ms）：用例失败于 30s 内收不到模型回复（发现超时 → 会话未建立），回退后复跑通过（2489ms） |
| E2E L0 全量（`npm run e2e:l0`） | `run-2026-09-19T06-32-22` **11/11 通过**（6m52s，并发 1、无重试、flake 预算 0）：新场景 17s，同层 `workspace-git-init` 10s、`workspace-no-git` 13s、`workspace-directory-moved` 7s，此前偶发超时的 `plugin-uninstall-no-freeze` 本轮也通过 |

**事实源同步**：`e2e/config/tiers.mjs` 的 L0 文件列表（与同类工作区场景同层）。无需改动设计与 code-index——本轮没有实现变更。

**遗留**：

- 端到端与 resources 层用的是同一个常量语义，但两边各自维护注入值（Rust 用例 400ms、E2E 300ms）；没有把「单次调用预算」暴露成可配置项，因此无法用真实慢 Git 逼近 5s 边界。要覆盖边界仍需在 resources 层做（第 11 条已覆盖挂起与超时）。
- E2E 的耗时断言是下界（≥ 注入等待），不是耗时回归门禁：机器变慢不会失败，只有慢 Git 没有被真正用上或会话被阻断才会失败。

### 2026-09-19：真实旧版 Git 2.4.12 的二进制验收与大小写分类缺陷（第 13 条）

**范围**：把「旧版 Git」的证据从假 Git 脚本升级为真实二进制，并修掉实测暴露出的缺陷。不改动发现命令集合（第 11 条的两条 `rev-parse` 加一次 `worktree list`）、登记键、调用次数与 `git_answered` 语义。

**方法**：本机源码构建 Git 2.4.12，把它放进 PATH shim，**并作为该 PATH 里唯一的 `git`**；被测 Peri 用 `env -i` 显式构造环境启动（与 `workspace-no-git` 同一手法）。构建与下载步骤见下方「取证方法」。

**实测发现（真实缺陷）**：Git 返回的「不是仓库」文案随版本变化——2.4.12 在非仓库目录回答 `fatal: Not a git repository (or any of the parent directories): .git`（大写 `N`），在它读不了的 linked worktree 里回答 `fatal: Not a git repository: <gitdir>`；2.39 回答小写 `not`。`observe_with_git` 用大小写敏感前缀 `fatal: not a git repository (` 判定「不是仓库」（该判定由 `275fa850d` 引入），于是旧版 Git 下**普通目录**与**旧版读不了的 linked worktree** 都被判成 `DiscoveryError("Git rejected repository discovery")`：那个版本的用户在任何非仓库目录都建不了会话，症状与本次 P0 完全相同——草稿留在编辑区、`projects` / `workspaces` / `session_bindings` 三表全空。假 Git 用例（第 11 条）按新版小写文案模拟，覆盖不到这个差异。

**改动**（`peri-resources/src/sessions/sqlite_store/discovery.rs`）：新增 `is_not_a_repository`，前缀按 `eq_ignore_ascii_case` 比较。放宽的只是大小写而非匹配范围——真实拒绝（权限不足、`dubious ownership`、仓库损坏）的文案不含该前缀，仍原样上报为类型化发现错误。

**回归测试**（`discovery_test.rs`）：

| 验证 | 结果 |
| --- | --- |
| `test_worktree_legacy_not_a_repository_wording_is_directory_mode`（新增） | 两种旧版文案都必须得到目录模式（`git_answered = true`、root = cwd、common / private 为 null）。修复前失败于 `旧版文案 … 不应报错：workspace discovery failed: Git rejected repository discovery` |
| `test_worktree_git_rejection_is_not_directory_mode`（既有） | `dubious ownership` 仍上报类型化发现错误：证明放宽没有扩大到真实拒绝 |
| `cargo test -p peri-resources --lib -- sessions::sqlite_store::discovery` | 17 项通过 |

**端到端证据（真实 Git 2.4.12，临时用例跑完即删，未入库）**：

| 场景 | 修复后 | 变异检验（把分类改回大小写敏感并重编译真实 binary） |
| --- | --- | --- |
| 旧版 `git init` 建的仓库 | 通过（2.0s）：仓库模式，`common_dir == private_dir == <repo>/.git`，建会话并发输入收到回复，1 project / 1 binding、模型请求 1 次 | 通过（仓库里 Git 正常退出，不经过该前缀） |
| 普通目录（非仓库） | 通过（2.0s）：目录模式，`root` = 真实路径，common / private 为 null，会话与回复正常 | **失败**：30s 内收不到回复，草稿留在编辑区，三表全空 |
| 现代 Git 建的 linked worktree，交给只有旧版 Git 的 Peri | 通过（2.1s）：旧版 Git 回答 `Not a git repository: <gitdir>`，按目录项目降级，会话可用、1 个绑定 | **失败**：同上 |
| 旧版 `git init` 建的仓库，Peri 在子目录（`<repo>/sub`）启动 | 通过（`run-2026-09-19T09-31-04`，1/1）：仓库模式，`root` 是仓库根，`common_dir` 与 `private_dir` 都是 `<repo>/.git`；绑定 `relative_cwd = sub`、`threads.cwd` 是子目录本身；建会话、发输入并收到回复，1 project / 1 绑定。该版本没有 `worktree` 子命令（实测 `git: 'worktree' is not a git command`），成员交叉核对按「不支持」跳过 | 不适用：本行观测为仓库模式，不经过「不是仓库」前缀 |

第 4 行是复核补测（`run-2026-09-19T09-31-04`，1/1 通过）；它的计时口径是文件时长（含 binary 构建检查与 PATH shim 构造），不是单次会话时延。其余三行来自首次取证，未记录 run id。变异检验用真实二进制证明了失败与修复都发生在生产路径上，而不是测试装置的产物；恢复改动后复跑 3/3 通过。

**结论与限制**：

- Git 2.4.12 可用：仓库、普通目录、子目录、现代 Git 建的 linked worktree 四种组合都能建会话。这是**已验证的版本**，不是「最低支持版本」——2.4.12 之前的版本仍未测。
- 旧版 Git 读不了现代 Git 建的 linked worktree 是正确的降级（设计 §3.3：Git 明确回答「不是仓库」即目录项目）：该项目分组退化为目录项目，但会话可用，符合「简化目标」第 1 条「不能把所有探测错误悄悄解释成不是仓库」的反面要求——这里是 Git 的真实回答。
- 真实旧版二进制不进 e2e 常规用例：仓库没有环境变量开关或条件跳过先例，且该二进制需手工构建、机器上不存在，留在 L2 / release 门禁会变成环境依赖。可复用的方法记在下面；缺陷本身由单元回归用例（两种文案）守住。

**取证方法（可复用）**：

```bash
# 1) 构建真实 Git 2.4.12（无需 OpenSSL；NO_ICONV 不能加，compat/precompose_utf8.c 依赖 iconv）
curl -LO https://www.kernel.org/pub/software/scm/git/git-2.4.12.tar.gz
tar xf git-2.4.12.tar.gz && cd git-2.4.12
make NO_GETTEXT=1 NO_TCLTK=1 NO_PERL=1 NO_PYTHON=1 NO_CURL=1 NO_EXPAT=1 \
     NO_OPENSSL=1 NO_APPLE_COMMON_CRYPTO=1 prefix=/tmp/git-old/install install
/tmp/git-old/install/bin/git --version   # git version 2.4.12

# 2) 端到端：PATH shim 里只放这一个 git（其余条目从 /usr/bin、/bin 软链并跳过 git*），
#    用 `env -i` 启动 target/debug/peri，前提守卫断言 `git --version` 含 2.4.12。
#    参考 e2e/tests/scenarios/workspace-no-git.test.ts 的 shim 与 env -i 构造。
```

**事实源同步**：[兼容性待办](2026-09-17-platform-compatibility.md)「Git 是普通目录启动的隐式依赖」与「旧 Git 命令能力未覆盖」两条（前者在本次核对时已与现行实现不符，一并更正）；[工作区身份设计](../../docs/design/session-workspace-identity.md) §3.1 补一句「不是仓库」的判定按 stderr 前缀忽略大小写，以及放宽范围止于大小写。`docs/code-index/peri-resources.md` 的工作区身份行只登记入口函数（`resolve_workspace` 等）与整体契约，分类实现是 helper 级细节，该行未涉及、本次未改。

### 2026-09-19：准入内不再重复完整发现（第 14 条）

**范围**：只处理「同一次准入的重复完整发现」。第 3 条把探测移出写事务后，一次准入仍会按环节重复跑完整发现（每轮三条 Git 命令）：计数来自临时插桩实测——`session/new` 6 轮（18 次调用）、`session/load` / `resume` 约 4 轮、prompt 轮 2 轮、`session/fork` 2 轮完全相同的复核。不改变身份模型、登记键、绑定校验强度与失败关闭语义；简化目标第 3、5 项与第 4 项的准备期可见状态仍未实施。

**根因**：准入链路上每个环节各自做一次「权威复核」，而每次权威复核都重新执行完整 Git 发现。这些环节判定的是同一件事（本会话的执行目录仍是登记时那个目录），却把 Git 的等待逐次叠加到同一次准入上；慢 Git / 慢盘下用户看到的就是「准备很久」。

**改动**（准入级规则：**一次准入至多一次完整发现**，准入内其余检查只复核已记录证据——SQL 关系加关键文件对象，不启动外部进程）：

- `peri-acp-types/src/store.rs`：新增 `reassert_session_binding`（默认 `Unsupported`），与 `validate_session_binding` 并列；文档写明后者是**准入级动作**、一次准入只应调用一次。
- `peri-resources/src/sessions/sqlite_store/workspace.rs`：新增 `reassert_session_binding_impl`（只走 `validate_session_binding_on`）；`create_bound_thread_impl` 不再为子线程重跑完整发现（比对的是已记录的父绑定）；`execution.rs::acquire_execution_lease_impl` 的先行复核改用 `reassert_session_binding_impl`。
- `peri-acp/src/host/workspace.rs`：新增 `BindingCheck::{Full, Recorded}`；`validate_expected`（准入入口）与 `reassert_expected`（同一次准入内）分派到两个 store 方法；新增 `expect_directory`——用 `canonicalize` 比对调用方给出的目录，不再为比较去解析登记（那会多跑一轮完整发现并顺手登记一个与本次执行无关的目录）；`acquire_for_load_with` 取得所有权后的第二次复核改为 `Recorded`；`finish_session_end` 与协议读请求保持 `Full`。
- 调用点：`requests/session_lifecycle.rs`（`handle_new` 以 `resolve_workspace` 为本次准入的唯一发现；响应装配 `admission_identity` 用 `Recorded`；`handle_fork` 第二次取得用 `reacquire_for_load`）、`requests/legacy_session.rs`（期望目录比对改走 `expect_directory`）、`host/prompt_dispatch.rs`（等锁前的先行检查 `Recorded`，取得 prompt 锁后的权威检查 `Full`）、`host/continuation.rs`（派发前先行检查 `Recorded`）。

**验证证据**：

| 验证 | 结果 |
| --- | --- |
| `test_worktree_registration_probes_filesystem_outside_write_lock` | 目录模式一次准入的观测轮次 2 → **1** |
| `test_worktree_repository_registration_keeps_git_calls_bounded` | 仓库模式一次准入的 Git 调用 6 → **3** |
| `test_worktree_bound_session_validation_probes_once` | 已有会话的准入复核 6 → **3** |
| `test_worktree_slow_git_wait_is_measured_per_call_outside_the_write_lock` | 常量 6 → 3；总耗时断言改为「逐次调用的间隔 ≤ 注入等待 + 2s 开销」加跨度下界，起进程时间不再计入单次判定 |
| `cargo test -p peri-resources --lib` | 145 项通过（测量时的中间值，计数链见第 15 条注与本轮核实） |
| `cargo test -p peri-acp --lib` / `cargo test -p peri-acp-types --lib` | 678 项 / 423 项通过 |
| `cargo clippy -p peri-resources -p peri-acp -p peri-acp-types --all-targets`、`cargo fmt --all` | 无告警、无格式差异 |
| E2E `workspace-slow-git` | 原引用的 `run-2026-09-19T07-51-47` 在本机 `e2e/results/` 中已不存在——该条引用的 21 个 run 里只有它缺失，其余 20 个仍在；同一用例现存产物为 `run-2026-09-19T09-07-17`（1/1 通过，13s）与 `run-2026-09-19T06-29-28`（1/1 通过，16s，第 14 条之前），两者只留通过与否和耗时、不记录窗口内调用次数，因此「窗口内发现调用 6 → **3**（正好一轮三条命令），输入到回复 2489ms → **1550ms**（注入等待每次 300ms）」这组数此后只能由用例自身的上限断言与单元用例支撑；用例新增上限断言「一次准入至多一轮发现」，把「不得为同一个目录重复解析」变成跨层门禁 |

**遗留**：

- 「哪个调用点用 `Full`、哪个用 `Recorded`」没有 ACP 层单元测试：`peri-acp` 测试没有 Git 假脚本基础设施，跨 crate 计数不划算，因此这条判断由跨层 e2e 的轮次上限守住（单进程单元测试不能代表准入时序，见 [testing](../../docs/standards/testing.md)）。
- subagent 派生路径（`peri-agent` spawn 时调 `validate_session_binding`）保持原样：一次派生一次发现，未纳入准入级去重。
- 临时插桩（`PERI_DIAG_DISCOVERY` 回溯、ACP 请求耗时日志、TUI `session_context` 耗时日志）与临时观测用例在取证完成后已删除，不进仓库。

### 2026-09-19：绑定复核的执行目录文本形式与登记一致（第 15 条）

**范围**：修掉「同一个目录两种文本形式」导致的重复解析。不改变目录身份证据、绑定关系与失败关闭语义。

**根因**：`validate_session_binding_on` 用 `root.join(&binding.cwd_relative_to_workspace)` 还原执行目录，而 `join("")` 会追加分隔符——工作区根的绑定还原出 `/a/b/`，`resolve_workspace` 解析出的是 `/a/b`。`Path` 比较看不出差别（既有测试因此一直是绿的），但按字符串比较目录的调用方会把它当成换了目录：TUI 的线程列表按目录缓存 `ResolvedWorkspace`（`SlowSnapshotRefresh::list_workspace`），`session/new` 的响应投影（`project_execution_cwd` → `ACTIVE_EXECUTION_CWD`）带回 `…/slow-repository/` 后与启动时的 `…/slow-repository` 不等，缓存被清空并重新发起一次 `peri/session_context`（by cwd）——同一个目录在同一次准备窗口里被解析两遍。

**实测证据**（临时用例，每次 Git 调用固定等待 2s，已删）：提交「PREPARE_OBSERVATION_INPUT」到回复共 **16.4s**、窗口内 Git 调用 14 次；临时插桩记录 4 次完整发现，其中两次 `resolve_workspace_impl` 的 `cwd` 分别打印为 `…/slow-repository` 与 `…/slow-repository/`，后者所在的线程列表刷新日志是 `refresh_threads needs_probe=true`——即缓存因文本形式变化被清空。

**改动**（`peri-resources/src/sessions/sqlite_store/workspace.rs`）：新增 `binding_cwd(root, relative)`——相对路径为空时返回工作区根本身，否则 `root.join(relative)`；绑定复核、`list_scoped_threads_impl` 的执行目录投影与 `validate_resolved_on` 的不变量检查统一走它。

**验证证据**：

| 验证 | 结果 |
| --- | --- |
| `test_worktree_binding_cwd_text_matches_registration_without_trailing_separator`（新增） | 工作区根与子目录各建一个绑定，`validate_session_binding` / `reassert_session_binding` 返回的执行目录文本必须与 `resolve_workspace` 一致（`Path` 相等不够——断言的是 `to_str()`）；修复前失败于 `/…/.tmpvbgrer/` 与 `/…/.tmpvbgrer` |
| 变异检验 | 把 `binding_cwd` 退回无条件 `root.join(relative)`：新用例失败（上一条的左值），恢复后通过 |
| `cargo test -p peri-resources --lib` | 计数链（本轮核实校正）：143（第 11 条）→ +1 `02ad6a7b`（旧版文案用例）→ +1 `7f90c11d`（spawn 退让重试用例）→ +1 本条用例 → HEAD 实测 **146 项通过**。原记的「145 项」是 `7f90c11d` 时的中间值（`02ad6a7b` 已含在内），「第 14 条之后合入的 `02ad6a7b` 又新增一条…现为 146 项」漏了本条用例的 +1 |

**遗留**：TUI 侧仍按字符串比较目录（`cwd != slow.list_cwd` 时清缓存）；本轮把生产方的文本形式统一，没有把调用方的比较改成路径比较。（本轮实测里「提交后前 10 秒状态栏没有反馈」的准备期可见状态已由第 16 条实施。）

### 2026-09-19：会话建立期间的用户可见状态（第 16 条）

**范围**：只补「准备期间没有任何可见状态」。不改变准备期限、失败恢复语义，也不改准入链路本身。

**根因**：会话建立（工作区发现 + `session/new` + 初次快照）期间，用户的输入既不在待发送队列（还没入队）、也还没有发出请求（还没到 enqueue），因此队列面板与消息区都投影不到它，状态栏也保持不变——第 15 条的实测里这段窗口超过 10 秒，第 11 秒才因执行目录投影被清空而显示为 `Initializing…`，两段都不是「正在准备」。慢 Git / 慢盘会把这段窗口拉得更长，而这正是用户最需要知道「在推进」的时候。

**为什么放在 client 而不是输入端**：建立由启动期的 `ensure_session` 发起（`kit/entry.rs:442` 的 spawn），首次输入只是等待同一次建立——`ensure_session` 在 operation gate 上排队，等的是正在建立的那个 future。把状态放在消费者一侧会漏掉「还没有输入」的启动窗口，而在会话已存在时又会给出瞬时假状态；放在建立过程本身，则无论建立由谁发起（启动、首次输入、`/clear`、切换模型）都恰好覆盖，等待方也不必自己再投影一次。

**改动**：

- `peri-tui/src/acp_client/client/session.rs`：新增 `PreparingSessionGuard`——进入置位、`Drop` 清除，在 `new_session_under_gate` 开头进入，覆盖整段建立过程（含关闭旧会话、`session/new`、初次快照）。选 `Drop` 而非逐分支清理：`session/new` 的 future 会被准备超时与应用关闭直接丢弃，逐分支清理会漏掉取消路径，把提示留在状态栏上。
- `peri-tui/src/kit/atoms.rs`：新增 `SESSION_PREPARING`（默认 false）；`src/kit/status_bar.rs`：`StatusBarRow1` 订阅它，`preparing_label` 只在置位时向该行追加一段（`loading` 色），不改动其余段落与折行规则；双语 `locales/*/main.ftl` 新增 `statusbar-preparing`（`Preparing session…` / `正在准备会话…`）。
- `peri-tui/src/kit/steer_consumer.rs`：注释写明可见状态由 client 拥有，消费者不重复投影。
- `docs/design/user-input-queue.md`（准备阶段段落）与 `docs/code-index/peri-tui.md`（会话建立期间的用户可见状态行、StatusBar 组件行、ACP client 索引）同步事实源。

**验证证据**（`cargo test -p peri-tui --lib` 1640 项、`cargo fmt --all -- --check`、`cargo clippy -p peri-tui --all-targets -- -D warnings` 均通过）：

| 验证 | 结果 |
| --- | --- |
| E2E `tests/scenarios/workspace-slow-git.test.ts` | 状态栏提示自启动后 **2134ms** 起可见 **1019ms**（注入等待每次 300ms 的一轮发现），建立结束即消失；实测值取自用例自身的轮询，并用「可见时长 ≥ 注入等待」作为下限断言，避免把「恰好抓到一帧」当成证据；终屏断言会话可用后不再含该提示 |
| E2E 单用例跑批 | `run-2026-09-19T09-07-17` 1/1 通过（13s），首轮无失败 |
| E2E 全量 L0（`--tier l0 --no-interactive`） | `run-2026-09-19T09-08-06` 11/11 通过（5m47s，串行、无重试，flake 预算 0）——状态栏新增的这一段没有影响任何既有用例的首屏文案断言 |
| 单元回归（`kit::steer_consumer`，9 项） | 建立请求发出时已置位；入队前已清除；准备超时清除；应用关闭丢弃准备中的 future 后清除 |
| 变异检验：`new_session_under_gate` 不置位 | 单元 2 例失败（"会话建立请求发出后，准备阶段必须对用户可见"）；e2e 失败于 `Text "Preparing session" not found` |
| 变异检验：`Drop` 不清除 | 单元 3 例失败（超时/取消/入队后仍置位）；e2e 失败于「准备提示在会话建立后仍然可见」 |

**遗留**：

- 会话 **load**（`session/load`、启动恢复的 load）不置位该状态：切换由面板或弹窗发起，那些界面本身可见；被 load 挡住的输入仍只有既有的 loading 投影。是否统一两种「会话尚未可用」的窗口未决。
- 状态只是进程内 atom：headless / print 路径不渲染，也不受影响；e2e 只在 TUI 上观测。

### 2026-09-19：Git 启动的瞬时失败按有限次数退让重试（第 17 条，事后补记）

**范围**：只处理「Git 子进程启动失败被直接判成 Git 不可执行」这一条路径。不改变发现命令集合、调用次数、`git_answered` 语义与失败关闭方向。**事后补记**：改动由 `7f90c11d` 提交，发生在原 16 条记录之后、当时未登记；本条按独立核实补记，不是本轮新改动。

**根因**：`git()` 对任何非 `NotFound` 的 spawn 失败直接返回 `Git could not be executed`。负载下 `Command::spawn` 会瞬时失败（EAGAIN / ENOMEM 等资源不足，此时没有子进程被启动），一次系统抖动就变成一次用户可见的工作区发现失败；提交说明记录 CI（ubuntu-latest）曾在 `test_worktree_path_discovery_rejects_unknown_option_echoes` 上报出该失败。

**改动**（`peri-resources/src/sessions/sqlite_store/discovery.rs`）：

- 新增 `GIT_SPAWN_ATTEMPTS = 3`（`discovery.rs:185`）与 `spawn_git`（`discovery.rs:191`）：`NotFound` 是确定性的环境事实（Git 未安装），不重试，仍由调用方按「Git 不可用」降级为目录模式；其余 spawn 失败退让 10/20ms 后重试，用尽次数仍按原语义上报 `Git could not be executed`。
- 重试不改变语义：spawn 失败意味着没有子进程被启动，不存在重复执行；发现命令集合、调用次数与超时预算都不变（超时仍按类型化发现错误结束、不重试，与设计 §3.1 一致）。

**回归测试**（`discovery_test.rs`）：

| 验证 | 结果 |
| --- | --- |
| `test_worktree_git_spawn_failure_recovers_within_retries`（`discovery_test.rs:130`，新增） | 首次 EACCES、退让期间放开权限，发现必须在同一次调用里恢复；提交说明记录变异检验为「尝试次数改成 1 时失败、改回 3 通过」 |
| `test_worktree_git_permission_denied_is_not_directory_mode`（既有） | 权限拒绝仍上报类型化发现错误，不因重试改变结论 |
| `cargo test -p peri-resources --lib` | HEAD 实测 146 项通过（含本条用例；串行与热态并行各一次，见「状态变更记录」的环境观察） |

**遗留**：

- 本机（macOS）不强制 ETXTBSY，瞬时启动失败无法在单机上确定性复现，用例以权限位恢复为复现手段，断言只看结果、不依赖退让时长；退让时长是常量取舍，不是实测分布。
- CI 上该 flake 是否因此消失未复测。设计 §3.1 只写「超时按类型化发现错误结束，既不重试也不降级为目录模式」，与本条的启动失败退让不冲突，但该节没有记录这条重试；本次回填只限本 issue 文件，未改设计文档。


### 2026-09-19：P1 旧库漏迁移登记键，误标版本 5 后持续阻断目录登记

**本项状态：Fixed（自动验证与独立业务复核通过）**。本 issue 的其他待办与整体验收状态不变。

| 日期 | 原状态 | 新状态 | 操作人 | 说明 |
| --- | --- | --- | --- | --- |
| 2026-09-19 | Open | Fixed | agent | 补齐 schema 2/3 直升及已误标 5 的登记键迁移；正常库与受影响库统一升级到 6 |

**复现**：schema 2/3 仅完成身份载荷转换就被标成 5，跳过原先只对 4 执行的组合键迁移。
关闭并重开后，移动已登记目录再建会话仍报 `UNIQUE constraint failed: projects.object_identity`；
版本号已经是 5，后续开库无法补迁移。独立临时数据库复现中 2/3 失败、4 对照成功。

**修复**：`schema.rs` 将 2–5 全部迁移到 6；2/3 先完成既有 revision / 身份载荷转换，
再与 4/5 一样在同一事务重建登记表。健康 5 和误标 5 均执行一次，之后重开不再重建。
逐行保留 ID、binding、历史/frozen 和 execution generation/dirty，不扫描旧目录，
不把旧会话重绑到新目录；外键引用损坏时整次回滚，保留旧版本与全部数据。

**验证**：

- 新增回归修复前：2/3/误标 5 均在移动目录时命中唯一约束错误；4 只在目标版本 6 的断言失败。
- `cargo test -p peri-resources --lib`：152 passed，0 failed。新增六项覆盖 2/3/4/误标 5
  升级后跨关闭/重开，在移动目录和原位置的新目录分别新建会话并取得执行 lease；
  旧绑定保持且拒绝在替换目录执行；健康 5 的两类组合登记完整保留；损坏引用完整回滚。
- `cargo build --workspace`：通过（macOS 链接器提示现有 unwind 表过大，不影响退出状态）。
- 独立 subagent 仅复核业务合理性：通过，未发现 P0/P1 业务遗漏；未进行代码正确性审查。
- 设计 §8 与资源代码索引同步到 schema 6；既有升级 E2E 从源码读取目标版本，无须修改常量。

**未验证**：Windows 运行时与完整 E2E 本轮未执行。
