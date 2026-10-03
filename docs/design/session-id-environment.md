# 会话身份、工作区归属与恢复

> 状态：现行设计；核心路径已实施，并发与端到端验收仍有未完成项。
>
> Scope：会话身份、机器环境分区、工作区发现与登记、执行绑定与恢复入口。
> 进度与验收见 [核心改动清单](../../spec/issues/2026-09-30-session-id-environment-core-change.md)。
> Machine → Workspace → Session 归属与 schema 12/13 迁移见 [存储 v2 设计](storage-v2-machine-workspace-session.md)；
> 执行所有权与 Workspace fencing 目标见 [Session 异步任务统一入口](session-async-tasks.md)。
> 术语见 [领域语言](../../CONTEXT.md)；冻结与生命周期约束遵循 [架构契约](../standards/architecture-contracts.md)（ARC-WORKSPACE-001）。
> 本设计取代旧工作区设计中以目录绑定、文件锁和根执行 owner 限制恢复的规则；父子关系与运行资源生命周期保持。

## 1. 会话身份与机器归属

Session ID 是唯一会话身份，不改为路径、机器地址或 env + path 复合主键。父子关系仍由 parent ID 关联，根通过父链确定。cwd、项目根、工作区路径与 `SessionBinding` 保留定位和展示语义，不是按 ID 恢复的归属凭证。

机器归属由 `threads.workspace_id → workspaces.machine_id` 唯一推导，不维护第二份 env 归属表或可独立漂移的过滤依据。`ThreadScope::Environment(machine_id)` 显式按 `workspaces.machine_id` 过滤列表；它是显式 scope，不表示所有列表已默认按当前机器筛选。按 ID 读取不隐式追加 env/cwd 条件，同名路径不构成跨 env 执行依据。

机器环境身份由 `peri-resources/src/sessions/machine.rs` 提供：

- 默认首次生成随机 UUIDv4，持久化到 `$HOME/.peri/machine-id`；后续启动读取同一文件，不用硬件指纹、PID 或连接地址。
- `PERI_MACHINE_ID` 可覆盖默认值，必须能解析为 UUID，解析后规范化；覆盖值不写入默认文件。进程通过 `OnceLock` 缓存身份，初始化后改环境变量不会切换身份。
- 首次创建在同目录使用 `create_new` 临时文件，Unix 模式为 `0600`，写入后 `sync_all`，再通过 hardlink 原子发布。若目标已存在，读取已发布值；最后尽力删除临时文件。已有文件损坏或不可读时返回错误，不静默重生成。
- 身份稳定性依赖保留该文件或一致的 override。复制 HOME/身份文件或复用 override 会共享 env；删除文件会产生新 env。它表达配置的机器环境身份，不证明硬件唯一性。

旧库与远端的来源未知归属使用占位 Machine 身份（`identity_kind=legacy_unknown`），不得与真实机器 ID 混同，也不因打开而迁移归属；写打开执行 schema 12/13 迁移，来源假设、失败回滚与只读行为见 [存储 v2 设计](storage-v2-machine-workspace-session.md)。只读打开不做回填；归属行缺失时不能据此声称机器归属完整。

## 2. 项目、工作区与执行绑定

会话已有独立 `ThreadId`，需要补齐的是项目身份和执行绑定。路径用于定位文件，不能同时承担项目归属、会话身份和执行所有权。

同一个本地 Git 仓库的主工作树与 linked worktree 共用 `ProjectId`；每份文件树具有独立 `WorkspaceId`。会话固定绑定一个工作区及其中的执行目录。打开列表、切换终端或从另一 worktree 恢复，不会改写这个绑定。项目列表默认聚合本项目的会话，允许按工作区过滤；列表中显示工作区和实际执行目录，分支只是可变化的展示信息。项目聚合不扩大任何会话的文件访问权限。

| 概念 | 身份 / 数据 | 不变量 |
| --- | --- | --- |
| 项目 | `ProjectId`，opaque ID | 一个本地仓库实例；同 remote 的独立 clone 默认不同 |
| 执行工作区 | `WorkspaceId`，opaque ID | 一份主工作树、linked worktree 或非 Git 文件树；归属键为 Machine + path |
| 会话 | 现有 `ThreadId` | 历史、消息、目标与运行归属继续使用原 ID |
| 执行绑定 | `SessionBinding` | 项目、工作区、工作区内相对目录与绑定版本 |
| 执行所有者 | Store 的 `epoch + nonce`；进程内 lease | 跨进程写 fence 与本地活跃执行、关闭收尾分离 |

不再增加与 `ProjectId` 重叠的 RepositoryId。身份范围限于同一台主机的同一 Peri 存储；跨主机、跨数据库合并和仓库克隆身份传播不在本设计范围。`WorkspaceInfo` 携带 machine ID、路径与路径来源（discovered / derived_legacy / unverified），`ResolvedWorkspace` 区分归属身份与本机发现登记；存储侧约束见 [存储 v2 设计](storage-v2-machine-workspace-session.md)。

`SessionBinding` 的持久化字段为：

```text
schema_version
project_id
workspace_id
cwd_relative_to_workspace
```

binding 不可变，协议中的 `revision` 保持常量 `1` 以兼容已有客户端，不作为数据库字段或并发控制依据。执行代次由 Store 的执行 owner 记录管理；进程内 `SessionExecutionLease` 管理本实例的活跃执行、写入效果结清与关闭收尾，不提供跨进程互斥。

`ThreadMeta.cwd` 保留为创建时目录和 legacy 证据，禁止通过普通 metadata 更新改写绑定。有效执行目录由绑定与已验证位置派生；兼容协议中的 `cwd` 是这个结果的投影。项目/工作区登记、位置更新和 binding 写入由同一个持久化 owner 管理，通过专用事务接口维护项目与工作区的关系，不能散落在 metadata JSON 中各自解释。

消息、compact flags、frozen snapshot 与列表摘要继续各守现有边界。列表允许投影小型身份字段，但不得加载消息正文、冻结 blob 或重新计算内容大小。

## 3. 工作区发现与身份登记

### 3.1 Git 事实

使用 Git 的机器可读入口取得事实，不根据 `.git` 是文件还是目录推测路径布局：

```text
git -C <cwd> rev-parse --is-inside-work-tree
git -C <cwd> rev-parse --show-toplevel --git-dir
git -C <cwd> worktree list --porcelain -z
```

两个位置来自同一次 `rev-parse`，输出按参数顺序每行一个；行数与请求不符即视为输出
不可信，不猜位置。发现不使用 `--path-format=absolute` 与 `--absolute-git-dir`：
更早的 Git 把它们当未知选项按用法错误退出，会让普通仓库被判成无法发现。common
directory 也不请求 `rev-parse --git-common-dir`，而是读 Git 自己写入的 `commondir`
文件：该文件的语义就是 `$GIT_COMMON_DIR`，`--git-common-dir` 是它的投影；
linked worktree 的该文件由 `git worktree add` 写入，主工作树没有它，两个位置相同。
代价是 `--git-dir` 的默认输出可能是相对路径，且同一命令在不同 cwd 下的输出形式
不同（主仓库根给相对 `.git`，子目录的 `--git-dir` 反而给绝对路径），因此位置先与
cwd 组合再 canonicalize，不能按宿主进程的 cwd 解释。`rev-parse` 把不认识的选项当
普通参数回显到 stdout，因此位置行以 `--` 开头即按不兼容的 Git 处理并报类型化错误，
不能当成相对路径拼在 cwd 下，用「目录不可用」掩盖真正的版本问题。

common directory 用于发现同仓库关联，private Git directory 用于区分 checkout；
linked worktree 的这两者不同，主工作树通常相同。这些 Git 语义来自
[git-worktree](https://git-scm.com/docs/git-worktree#_details) 与
[git-rev-parse](https://git-scm.com/docs/git-rev-parse#_options_for_files)。

命令使用参数数组、显式 cwd 和有界执行，不拼接 shell。每次调用有固定超时预算，超时
按类型化发现错误结束，既不重试也不降级为目录模式。发现过程隔离继承的 `GIT_DIR`、
`GIT_WORK_TREE`、`GIT_COMMON_DIR` 等会改写仓库选择的环境变量；不能修改用户 Git
配置来使探测成功。解析支持带空格的路径，worktree list 优先 NUL 分隔；旧版 Git 不
认识 `-z` 时按用法错误退回换行分隔，退回只改变分隔符，成员判定仍按完整路径精确
比对，真实失败（权限、损坏仓库）不触发退回；`worktree` 子命令或 `--porcelain`
整体不存在的旧版 Git 不做成员交叉核对（位置已由 `rev-parse` 回答），这属于证据不足
而非「不是仓库」，真实失败仍原样上报。路径按所在文件系统 canonicalize，不统一
小写、不使用 lossy 转换生成身份。

「不是仓库」的判定按 stderr 前缀比较完成，忽略大小写：该文案随版本变化，按大小写
敏感匹配会把旧版 Git 下的普通目录判成类型化发现错误，使这些目录完全无法建立会话。
放宽的只是大小写，不是匹配范围：权限不足、unsafe repository 与损坏仓库的文案不含
该前缀，仍按真实失败原样上报。

### 3.2 登记裁决

由 `peri-resources` 持有本地登记表，分配 opaque ID。Git 路径是发现线索，不是
ID 本身；不把路径 hash 永久当成项目 ID，也不往受版本控制文件或 Git 管理目录
写 Peri 身份标记。

登记记录保存 canonical locator、平台文件对象识别信息和登记代际。登记键是
(canonical locator, 该位置的文件对象证据) 组合，两者同时命中才复用原登记：
路径相同而文件对象已被替换，或同一文件对象出现在新路径，都不是同一次登记。
inode / file ID 只能作为一致性证据，不能证明任意复制、重建或历史路径复用。
文件对象身份使用 Unix device/inode 或 Windows volume/file index；不依赖 creation
time，也不降级为 mtime/ctime 或路径等同。

新对象或新位置不继承旧身份，也不被旧登记挡住：登记表允许同一路径有多个文件
对象、同一对象出现在多个路径，各自得到新的 `ProjectId` 与 `WorkspaceId`，
执行 cwd 就是用户实际打开的目录。旧登记、旧绑定与历史保持原样，引用它们的
会话继续按各自登记证据复核并失败关闭，不静默改绑、不隐藏历史。项目（而非
工作区）只在定位与对象证据同时一致时复用，例如 Git linked worktree 换位后
common directory 未变仍属原项目；不相关的同名副本各自成项目。证据不足返回
`NeedsRelink`，不自动合并。

工作区身份取 canonical root 路径加上该目录自身的文件对象证据；Git 布局是同一
目录的派生观测，`git init`、移除 `.git` 或重建其管理目录都属于正常演进。同一
目录对象在同一路径再次解析时复用原项目与工作区 ID，只在原行内刷新观测快照：
执行 cwd、项目归属和已有绑定都不移动。可以覆盖已登记快照的观测必须来自 Git 的
真实回答；Git 不可用时得到的是不完整目录观测，仍按证据不足拒绝。

第一次登记与 binding 写入使用唯一约束、事务和竞争失败后重读 winner，避免
两个宿主同时为同一已验证工作区分配不同有效身份。Git 探测在事务外进行，提交
前复核关键文件对象与关联关系；期间发生变化则放弃该次结果。

完整发现是准入级动作：一次准入（一次 ACP 请求，或一次 prompt 轮）至多执行一次
Git 发现，其余检查复用已记录证据——SQL 关系加关键文件对象身份，不启动外部
进程。目录被替换、被换位或 Git 位置消失仍会在这些检查里失败；重复发现不带来新
证据，只会把 Git 的等待（慢盘、慢 Git、Git 缺失时的探测）叠加到同一次准入的
每一步上。已有绑定的准入复核（`session/load`、prompt 轮、`workflow/resume`）在
准入入口执行一次完整发现，其后的身份读取、二次确认与执行所有权取得只复核已
记录证据。

### 3.3 情景规则

| 情景 | 行为 |
| --- | --- |
| 主树与 linked worktree | 同项目，不同工作区 |
| 同 worktree 的不同子目录 | 同项目、同工作区，各会话保留原子目录 |
| 指向同目录的符号链接 | canonicalize 后复用登记；保留原路径用于展示/诊断 |
| 相同 remote、branch 或 commit 的独立 clone | 分别登记项目，不推断同一身份 |
| 嵌套仓库、submodule | 使用 cwd 所属的最近 Git 仓库，不上卷到 superproject |
| 非 Git 目录 | 创建目录项目与工作区；不猜测任意父目录是项目根 |
| 已登记目录随后出现或移除 `.git` | 同一目录对象仍取原工作区，刷新观测快照；执行 cwd 与历史绑定不变 |
| Git 可执行文件缺失 | 新发现使用 cwd 目录模式，不推断仓库关系；已有 Git 绑定仍须匹配原发现快照，缺少证据时拒绝执行 |
| Git 权限不足、unsafe repository、损坏或探测中途失效 | 类型化探测错误，不能伪装为非 Git 项目 |
| bare repository | 可作为 linked worktree 的仓库锚点；bare 目录本身不可作为执行工作区 |
| worktree 删除或目录暂时不可用 | 保留身份和历史，位置标记不可用，阻止执行 |
| 删除后同路径重新创建 | 不继承旧会话绑定；新对象单独登记，可在该目录建立新会话 |
| 目录（含仓库）整体搬迁到新路径 | 旧绑定按原登记证据复核并失败关闭，历史保留；新路径单独登记，不自动改绑或改指旧 ID |
| 整仓复制、导入或 Git 管理目录重建 | 不承诺透明识别，不以 remote 相同证明身份；新位置单独登记并可建立新会话，旧绑定保留历史 |

发现结果区分 Git 工作区、目录工作区、不可用、需重关联与探测错误；失败经
`WorkspaceError` 的类型化分支（`DiscoveryError` / `Unavailable` / `NeedsRelink`）
上报，不折叠成「目录不可用」。Git 确认“不是 Git 仓库”，或 Git 返回
executable-not-found 时，可以建立目录项目；后者仅表示未启用 Git 发现，不声称
目录中没有仓库。两种目录模式均只使用 cwd 及文件对象身份，不推断任意父目录归属。
Git 探测一旦开始成功，后续失败不能降级为目录模式。

已有绑定在每次准入的入口复核一次完整发现快照（准入内的其余检查只复核已记录
证据，见 §3.2）；Git 安装状态变化不能改写项目/工作区身份。
非 Git 目录随后初始化 Git，或已登记仓库移除 `.git` 时，该目录仍是同一工作区：
复用原项目与工作区 ID 并刷新观测快照，已有会话继续可执行。子目录会话不因根
目录的布局变化被并入或改绑，仍按各自登记快照复核。Git 不可用不构成「该目录
已不是仓库」的证据，不得据此覆盖已登记的仓库布局。

## 4. 恢复与执行分开

ACP `requests/session_restore.rs::prepare_existing` 按 ID 读取保存的会话，先经 `session_resources.inspect_availability` 判定 env 与执行可用性；仅可执行时才做 legacy 冻结/接纳并读取持久 frozen，避免准备阶段读取异机路径。请求 cwd 不再是准入条件，目录不存在、旧 dirty 代际或另一实例恢复不构成文件锁/owner 认领门槛。只读恢复不要求 frozen 存在，也不启动 workflow/LSP；只读标记经 `sessionWorkspaceV1` 身份载荷下发，未协商的连接同样按只读准入但拿不到标记，`require_owner` 在写入/执行时仍是确定拒绝。可执行路径的缺失、损坏或未知 frozen 与存储错误仍按既有契约报告，legacy 使用保存 cwd 而非当前终端 cwd。

执行准入复核保存的执行目录，而不是以请求或期望 cwd 重新决定归属：一次准入执行一次完整发现复核（`validate_expected`），准入内后续检查只复核已记录证据（`reassert_expected`）。相对目录重新 canonicalize 后必须仍属于原工作区，并复核最近 Git 仓库；目录组件变为 symlink 或新嵌套仓库时不能只凭字符串前缀通过。请求环境与保存绑定的环境不匹配时返回 `ExecutionBindingMismatch`，不因项目相同就放行。

显式 load/resume/fork 可接纳未绑定根会话：从保存的绝对 `ThreadMeta.cwd` 发现并验证当前工作区，请求 cwd 仍只作期望校验，不使用当前终端目录兜底。接纳在写事务中重读 cwd、根关系与绑定，验证解析结果仍有效，原子插入 binding 并只在 frozen 缺失时保存兼容快照；缺失快照的配置、语言、MetaHarness 与插件目录均从保存 cwd 发现，不沿用启动项目，也不提前装配执行资源。已有快照先校验并保持原样，并发竞争复用赢家，失败或中断不得只提交其中一项。旧数据未保存目录对象身份，接纳以保存 cwd 和当前可验证身份为依据；缺目录或非绝对 cwd 保留只读历史。

实际执行由 `SessionResourcesImpl::execution_availability` / `acquire_execution` 与 ACP `require_owner` 控制：`execution_availability` 在执行/工具准入时检查保存 cwd 是否为实际目录，不把目录探测作为历史读取的条件；env 不匹配或保存 cwd 缺失/不可用时，ACP 以只读方式恢复历史，不装配可执行的会话环境。`sessionWorkspaceV1` 响应可携带 `read_only`，执行请求由宿主拒绝；不悄悄在当前机器同名绝对路径执行，也不自动把 session 迁入当前 env。这是执行准入，不是 Session ID 的访问授权。

取得执行准入后，重读 binding、重新验证位置与运行恢复状态，再恢复 frozen、构建有效环境和资源，最后提交 live state。装配失败必须收回本次新建资源，不能泄漏 LSP/MCP 或后台任务。热态复用必须校验同一 binding 和环境；冷态不从请求重新决定 cwd。metadata、Git 定位、资源装配或 frozen 恢复失败，都不能提交新的 active session。已有 `ARC-SESSION-LOAD-001` 的同步 reservation 和 operation gate 保留；新旧 session 切换只有在目标提交时才公布有效目录和 active identity，失败必须恢复真实可用的原状态或明确 NoSession。

目录缺失时，历史通过只读查询/回放路径仍可访问。这条路径不得创建执行资源、回填 frozen、启动 continuation 或取得写 lease；标准可执行 load 则明确失败。

session 执行 sidecar 文件锁及 TUI dirty/owner 恢复确认已移除；不再启用 `peri.sessionRecoveryV1` 协商或发出 `peri/session_reset_dirty`；caps 字段/序列化键仍保留为 false，不表示恢复机制仍存在。普通工具授权、删除等确认弹窗保留。`SessionExecutionLease` 与进程内 mutation gate 管理当前实例的活跃执行、写入效果结清及关闭收尾；跨进程单执行 owner 由 Store 的 `epoch + nonce` CAS 提供，并在业务写入事务内校验（ARC-WORKSPACE-001）。不把重开当成前次未知写入已结清的证明。同 ID 创建重试只在绑定、冻结快照及父链等不可变事实一致时接纳，不覆盖已有内容；无绑定但已有冻结快照的记录拒绝自动 legacy 接纳，避免掩盖绑定损坏。取消、子任务关系、MCP/LSP 关闭与事务仍需遵守自身生命周期。

## 5. 执行环境装配

会话的 shell、Read/Edit/Write、@mention、PTC、hooks、skills、项目指引、MCP、LSP、Workflow 和 SubAgent 都从同一个已解析的 session environment 取得执行目录。宿主启动目录只用于创建默认新会话及初始列表选择。

Host 可以共享 transport、全局配置来源与确定可共享的服务；项目相关配置、插件发现结果、hook groups、命令目录和资源句柄必须按会话执行环境装配。缓存的 key 必须包含真实环境与配置身份，不能只包含 `ProjectId`。同项目工作区之间不合并 `.mcp.json`、局部 settings、权限或可写目录。

不能只修 `SessionState.cwd`，仍把宿主启动目录的 project hooks / plugins / MCP 注入恢复后的会话。跨工作区会话选择开放前，必须完成这些消费者的路径一致性验收；无法装配正确环境时返回错误，禁止部分成功。

已有全局和 session 权限策略照常生效。目录、项目身份与 Git 信任状态不是授权凭证；恢复到另一个项目环境不会获得浏览窗口或兄弟 worktree 的额外权限。

## 6. 会话生命周期

### 6.1 new

验证请求目录（本次准入唯一的完整发现与登记）→ 发现/登记项目与工作区 → 形成 binding → 创建 thread → 取得执行准入（只复核已记录证据）→ 在该环境构建并持久化 frozen snapshot → 装配会话资源 → 发布 session。中间失败不得留下可执行的半成品；new/frozen 写入失败继续遵守现有补偿规则。

### 6.2 fork 与跨工作区续作

普通 fork 仅在 source 的已提交历史达到完整工具往返边界、且没有未完成执行时复制；由源 owner 在生命周期 gate 下给出一致性快照，不能边执行边复制当前 Vec。它保持 source binding，产生新 `ThreadId`，并精确继承 source frozen snapshot，继续满足 `ARC-FROZEN-001`。请求不同 cwd 不得偷偷创建“旧前缀、新目录”的会话；返回 binding mismatch。

跨工作区续作定义为独立、显式的新环境操作：新 `ThreadId`、新 binding、目标环境重新冻结，历史以带来源和截止点的快照复制；不得改写旧消息中的绝对路径，亦不得把旧工具结果当作目标工作区已执行的事实。跨环境的说明持久化为可信上下文。它不继承进行中的工具、审批、队列、cron、Workflow、子 Agent 或旧运行句柄。

初始交付不提供该操作。普通 fork/load 不承担它的兼容别名；将来实现时应独立定义 wire capability 与历史投影验收，不修改普通 fork 的冻结语义。

### 6.3 位置重定位

初始交付不提供重定位或重关联操作：没有把已有 binding 改指到新位置或新对象的入口。目录移动、移除后重建、Git 管理目录身份变化让原绑定返回 `NeedsRelink` 或 `Unavailable`，保留历史，不自动修改 binding 或 frozen。用户可完成的前进路径是在当前可访问目录建立新会话：该目录按 §3.2 得到新登记，旧会话与历史保持只读可查。该绑定失败的原因与这条前进路径随错误一并呈现，不提示产品中不存在的操作；会话未能建立的失败发生在输入受理之前，其提示不得表述为输入被拒。后续显式重定位若要保留 `WorkspaceId`，必须在无执行 owner 时校验 Git 关联和文件对象证据，并让位置更新与执行准入共享线性化点。

当前在 new/load/resume/fork 和新 prompt 准入时验证目录。运行中的外部 `git worktree move/remove` 不触发自动迁移，也不承诺隔离任意外部文件系统改动。因此活动执行期间应保持其工作区位置稳定；下一次准入发现变化必须拒绝。

## 7. 查询、TUI 与 ACP 兼容

查询入口：`list_sessions(ThreadScope)` 按 scope 在 SQL 层过滤，`load_session(session_id)` / `resume_session(session_id)` 按 ID 读取历史与元数据（恢复语义见 §4）。提供统一的强类型 scope：`Environment(machine_id)`、`Project(ProjectId)`、`Workspace(WorkspaceId)`、`ExactDirectory(WorkspaceId, relative_dir)` 与显式 `All`。筛选使用稳定排序 `(updated_at, ThreadId)` 与 cursor 分页；索引覆盖项目/工作区及排序。列表条目的 `binding` 可为空表示未绑定历史，`effective_cwd` 只用于展示，可执行 load 必须独立验证。

普通 TUI 列表默认为 Project，允许 Workspace 筛选。选择其他工作区的会话时，客户端从宿主取得已验证的有效执行目录，再发起恢复；宿主仍独立校验 binding。界面在提交成功后显示这个目录，@mention、路径展示和相关文件视图同步切换。hooks、插件与 MCP 展示取当前会话环境。TUI 本地配置面板仍编辑宿主启动配置，必须显示“宿主配置”及实际保存路径；它不代表当前工作区的会话配置。

`-c` 默认选择当前工作区、当前相对目录的最近会话，避免用户在新 worktree 启动时自动接管另一 worktree。`-r <id>` 显式选会话，遵循保存的 binding；启动 cwd 不覆盖它。查询失败与没有候选必须分开呈现，失败不能无声变成新会话。

标准 `session/list` 的 `cwd` 继续表示精确目录过滤，不把旧字段改释为项目过滤。项目/工作区 scope 和身份投影经版本化 Peri capability 扩展；未协商时保留标准字段。请求 ID 不匹配保存的环境时，对旧客户端也返回明确错误，不能兼容错误执行。

新 TUI 列表经 ACP → Controller → Resources 查询，复用同一 scope 语义，不在客户端再次实现 Git 身份算法。既有直接 ThreadStore 列表入口在这条路径落地后退出对应调用；不保留两套可独立漂移的归属判断。

## 8. 存储打开与版本边界

默认读写使用 `~/.peri/threads/threads.db`，`--db-path` 可选择显式路径；schema 版本记录在 `PRAGMA user_version`，当前为 13（v12 建立 Machine/Workspace/Session 归属，v13 增加持久执行 owner），版本与迁移细节以 [存储 v2 设计](storage-v2-machine-workspace-session.md) 与代码索引为准。

只读 metadata 打开不创建数据库、不升级 schema、不登记或绑定；缺失的默认库按空历史处理，损坏与不兼容 shape 返回错误。启动时写打开失败（schema 锁被占、库文件或 WAL 不可写）降级为只读打开并记 warning：进入与历史浏览不受影响，但降级不假装可写——新会话与目录登记在进入 SQL 前按 `ReadOnlyStore` 失败。写打开走到版本判定时，本构建不认识的 schema 与 `user_version` 返回 `UnsupportedDatabaseSchema` / `UnsupportedSchemaVersion`，且不降级；升级前必须停止所有旧 writer，不支持新旧二进制混用同一库。schema 版本号不是对不遵守协议的旧 writer 或外部 SQLite writer 的访问控制。

## 9. 保证边界

- hardlink 发布只解决初次身份文件的完整可见性与竞争创建，不代表整个系统全面并发安全。文件系统须支持同目录 hardlink；目录创建未强制私有权限，已有身份文件权限/符号链接未额外校验，非 Unix 不提供 `0600` 保证。未同步父目录，也未保证崩溃后无残留临时文件或身份目录项持久性。
- Store 的 `epoch + nonce` CAS fence 只提供跨进程单执行写 owner，不构成工具副作用去重、多用户授权或前代已正常收尾的证明；同一 session 仍可被多个实例以只读方式恢复。SQLite 事务与进程内 lease 不提供跨实例/跨机器的续写冲突策略，更不保证工具副作用不重复；相关验收仍见 active spec。
- Session ID 可查找性不是认证，env 分区不是多用户权限隔离；机器 ID 文件与 override 都不是安全凭证。数据库/服务访问授权独立，不能仅靠列表过滤或执行只读标记宣称安全。
- 本设计不承诺任意缺路径/缺快照/坏库均能完整恢复，也不宣称只读界面替代所有存储写权限检查。全面 URI/VFS、插件与 MCP 缓存迁移不是本次前置条件。
