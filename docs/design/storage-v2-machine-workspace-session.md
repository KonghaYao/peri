# 存储 v2：Machine → Workspace → Session

> 状态：已批准目标设计。本机 schema 12 与远端存储契约 `peri.session.store/v6` 已接入；
> 真实远端验收待执行。当前行为以代码、契约测试和
> [Session ID / 机器环境设计](session-id-environment.md)为准。
>
> Scope：会话数据的机器分区、工作区身份、会话归属与归档。本文是 v2 数据结构的
> 目标权威；不改变按 Session ID 读取历史与执行准入分离的现行原则。Session
> 就是现有 thread：同一条记录、同一个 `ThreadId`，不另建一套会话实体。

## 1. 领域结构

```text
Machine: machines (id, name)
  └── Workspace: workspaces (id, machine_id, path)
        └── Session = thread: threads (id, workspace_id, archived, ...)
              ├── messages (thread_id) / flags
              ├── frozen and inherited context
              └── child threads (parent_thread_id)
```

`Machine` 表示一份稳定的机器环境身份。`id` 沿用当前持久 machine ID：本机首次
生成的 UUID 或有效的 `PERI_MACHINE_ID` override；它不是硬件指纹、用户身份、
授权凭证或执行锁。`name` 是用户可修改的展示名，例如“我的电脑”，修改名字不
改变 ID、Workspace 或 Session 的归属。首次登记时默认名为“我的电脑”；同一
共享库可以有多条 Machine 记录，展示名允许重复。

### 本机 Machine ID 的稳定算法与识别

本机 ID 使用**持久随机身份**，不从主机名、MAC 地址、硬盘序列号、Git 路径或
数据库中的候选 Machine 行计算。算法只有一个权威入口：

1. 部署显式设置 `PERI_MACHINE_ID` 时，校验并规范化 UUID，将其作为本进程的
   Machine ID；override 不改写默认身份文件。
2. 未设置 override 时，读取 `$HOME/.peri/machine-id` 并校验 UUID。文件存在但
   损坏或不可读时返回错误，不生成另一身份掩盖问题。
3. 文件不存在时生成一个 UUIDv4，按现有原子发布规则写入该文件；并发初始化
   读取同一个胜出值。后续启动始终读取它。Machine 表中的行以此 ID 查找或
   创建，已存在的展示名不得被默认名覆盖。

因此稳定性依赖保留身份文件或显式使用同一 override；它是一次生成、随后读取
的算法，不声称能从硬件重新算出丢失的旧 ID。不同机器复制该文件或复用同一
override 会共享 Machine ID，须由部署方避免；重装系统或换 HOME 丢失文件会
产生新 ID，旧 Workspace 和 thread 不自动迁移。

命令入口：`peri meta machines` 只读显示当前 ID、已登记机器和 Workspace 路径；
`peri machine adopt <旧 ID>` 预览候选，确认所有活跃 Peri 执行已经停止后，以
`--current <当前 ID> --apply --confirm-no-active-executions` 原子替换身份文件并退出。
该确认由操作人负责；命令以当前 ID 再次校验文件，拒绝未知或来源不明的目标。

用户不需要记住 UUID：Machine 查询面必须显示“当前机器”的 `name` 和完整或
可复制的 ID；Machine 列表同时显示名称、短 ID、是否当前机器和其 Workspace
路径，允许用户重命名。当前标记只由本地身份文件或 override 与表中 ID 精确
匹配得出，不能用名称、hostname、同名路径或“库里只有一台机器”推断。

若身份文件丢失，首次启动仍按上述算法生成新 ID；用户确认某条旧 Machine 才是
本机时，提供**显式切换本机身份**操作：展示当前新 ID、待采用的旧 ID 及其
Workspace 路径，在没有活跃执行的边界原子替换身份文件，并要求重新启动后
读取确认。进程内已缓存的 ID 不能热切换。不得通过打开 Session、机器名相似
或路径相同自动认领，不得采用 `legacy_unknown` 占位身份。切换的是本机 ID，
不批量改写旧 Workspace、thread 或凭证的归属。设置了
`PERI_MACHINE_ID` 时，应更改该显式部署值；写身份文件不会覆盖正在生效的
override。

`Workspace` 是**某个 Machine 上的一份 Git worktree，或一个非 Git 启动目录**。
打开 Git 仓库中的任意子目录时，`path` 取**该 worktree 自身的根路径**，不是
子目录 `pwd`，也不是多个 linked worktree 共享的 Git common directory 或主工作树
路径；打开非 Git 目录时，`path` 取启动 `pwd`。新登记沿用现有发现流程得到
规范化的绝对路径，不以有损字符串生成身份。同一 `(machine_id, path)` 只对应
一个 Workspace，其 `id` 是创建时分配的 UUID，后续打开同一键复用该 ID。
不同机器上的同一路径是不同 Workspace；同机一个 worktree 内的不同子目录共享
Workspace，linked worktree 各自有 Workspace。Project 可聚合多个 worktree，
但不替代此唯一键。Git 无法回答时不能声称该目录是非 Git 目录；此时按当前
cwd 建立 `path_source=unverified` 的 Workspace，后续 Git 恢复若发现另一个根，
新 Session 使用经证实的根 Workspace，旧 Session 不自动改绑或合并。

`Session` 与现有 `thread` 是同一个持久实体，`ThreadId` 就是 Session ID；
v2 继续使用 `threads` 表，不创建 `sessions` 表，也不重命名 `messages.thread_id`
或 `parent_thread_id`。每条 Session 有独立且稳定的 ID，
必须有一个 `workspace_id`；通过 Workspace 可找到其 Machine。Session ID 仍是
历史读取的唯一会话身份，Workspace ID 不参与按 ID 恢复的认领或授权。根 Session
仍保存创建时的原始执行 cwd；在 Git worktree 中它可以是 Workspace.path 下的
子目录，不因归组而改成根目录，之后也不随启动位置变化。子 Session
保留自身保存的执行目录与父 Session ID，继承父 Session 的 Workspace ID；
fork 出的独立 Session 按其创建目标的 Workspace 归属。归档不会改变父子关系。

Workspace 也是会话相关持久能力数据的隔离边界。同一 Workspace 的多个 thread
共享该 Workspace 的 MCP 鉴权；不同 Workspace 即使处于同一 Machine、连接同一
MCP server，也不能读取或覆盖彼此的凭证和私有状态。Git worktree 内不同子目录
因共享 Workspace ID 而共享鉴权；linked worktree 因 Workspace ID 不同而隔离。

## 2. 持久化不变量

| 实体 | 权威字段 | 约束 |
| --- | --- | --- |
| `machines` | `id`, `name`, `identity_kind` | `id` 主键；`name` 仅展示；`identity_kind` 区分真实机器身份与迁移时来源未知的占位身份 |
| `workspaces` | `id`, `machine_id`, `path`, `path_source`，以及诊断列 `project_id`、`identity`、`discovery` | `id` 为 UUID 主键；`machine_id` 必须引用 Machine；`UNIQUE(machine_id, path)`；`path_source` 记录 discovered / derived_legacy / unverified；诊断列只描述当前占用该路径的对象，不参与执行准入 |
| `threads`（Session） | `id`, `workspace_id`, `parent_thread_id`, `archived`, 现有会话元数据与上下文 | `id` 主键；`workspace_id` 非空并引用 Workspace；`archived` 非空，默认 `false` |
| `session_bindings` | 现有 Project/relative cwd 与版本化执行发现快照 | 保存每条 Session 创建或旧库迁移时的执行证据；可缺失的旧远端证据不得凭空补造 |
| `mcp_oauth_credentials` | `principal_id`, `workspace_id`, `server_key`, 凭证正文与更新时间 | 凭证属于 Workspace；同 Workspace 的 thread 共享，同机其他 Workspace 不可见 |
| `messages` | `message_id`, `thread_id`, payload 与 flags | 消息只归属一个 thread，现有顺序、内容和 compact 语义保留 |

`archived` 只表示用户将 Session 从普通列表移入归档。创建时为 `false`；用户可将
其改为 `true`，也可取消归档。归档不删除消息、不终止运行、不改变执行资格，
也不修改 `updated_at` 所表达的会话内容活动时间。普通列表只列未归档的、
`message_count > 0` 的根 Session；归档列表只列已归档的、`message_count > 0`
的根 Session。未发布草稿和已发布但仍为零消息的 Session 都不进入这两个列表，
但可按 ID 读取已发布会话。归档操作只对根 Session 开放；子 Session 随父会话
浏览，不独立归档。现有 `hidden` 是子 Agent 展示规则，不是归档状态，
迁移时不得把 `hidden=true` 转成 `archived=true`。

`identity_kind=known` 的 Machine ID 必须是规范 UUID；`legacy_unknown` 仅用于
旧库来源未知的记录，其原标识不得与真实机器 ID 混同。展示名不参与唯一性。

### `workspaces` 与 `session_bindings`

`workspaces` 在本机与远端是同一张表，归属键为 `(machine_id, path)`；行上的
`project_id`、`identity`、`discovery` 只描述当前占用该路径的对象，用于项目复用与
诊断，不决定 Workspace 身份，也不参与执行准入。`threads.workspace_id` 是会话归属的
单一权威；`session_bindings.workspace_id` 与它保持同一归属行 ID，不再有独立的执行
登记 ID 空间。

`session_bindings` 保存每条新 Session 不可变的版本化执行发现快照（创建时的 root、
文件对象身份、Git private/common directory 证据及执行目录相对路径），
`evidence_origin=creation_snapshot`。旧本机绑定迁移时只能复制最后观测值并标
`legacy_last_observation`，不得冒称为创建时证据。发现快照只作诊断与兼容数据：
本机准入只要求归属行存在、机器一致且 cwd 可用；远端组合仍要求快照存在以定位 root，
identity/Git 观测字段不参与比较；virtual 远端沿用快照 root / project 校验。旧远端
缺快照的记录明确保持缺失，仅可按 ID 读取历史，不得用当前同名目录、本地旧登记或新的
文件系统观测补造执行资格。

选择 Turso locator 是持久化后端的全量切换：Machine、Workspace、Session、消息、
绑定、执行发现快照与 Workspace 级 OAuth 只读写远端库。运行时可以读取机器 ID
文件、访问工作目录并由 `peri-sdk` 管理执行唯一性；写打开和只读打开均不得打开、创建、
升级或查询本地 SQLite（包括 `~/.peri/threads/threads.db`）。本地 locator 才使用
本地 SQLite 及其迁移路径。

机器、Workspace 与 Session 的归属关系是持久事实：普通 load、resume、列表查询
和更改归档状态都不得重写 `machine_id`、`path` 或 `workspace_id`。需要跨机器或
跨路径继续工作时，应创建新归属的 Session，并显式复制/继承所需历史；不能把
旧 Session 静默移到当前 Workspace。保存目录不可用或机器身份不同，不阻止
按 Session ID 读取历史；实际执行仍按现行环境准入规则判断。

### Workspace 数据与鉴权边界

v2 的持久 MCP OAuth 凭证以 `(principal_id, workspace_id, server_key)` 为唯一键；
其中 Workspace ID 是必须参与每次查询的隔离维度。`server_key`
仍区分 endpoint、OAuth 配置及动态连接代际，不能只按 MCP server 显示名合并。
当前固定的 `principal_id=local` 不能替代 Workspace ID。读取、保存、清除、列举和刷新凭证都必须
限定在**同一个** Workspace。清除全部凭证也只清当前 Workspace，不得清整台
Machine。Workspace ID 从已保存、已校验的 Session 归属或可信 Workspace
装配上下文取得，不能由 MCP 工具参数或客户端自由文本指定。凭证端口在装配时
绑定单个 Workspace，端口方法只接收 server key；同一个全局凭证客户端不得
不经作用域绑定直接注入多个 Workspace 的 MCP pool。

同一 Workspace 的多个 root/child thread 可以复用已保存的 OAuth 凭证；归档
Session 不撤销 Workspace 凭证。连接、OAuth 待完成流程、token refresh 和
可复用的 MCP client 若跨 thread 共享，也必须按 Workspace ID 及 server 身份
分区；跨 Workspace 不得借同名 server、相同 URL 或相同机器 ID 命中已有连接。
ACP 客户端声明的 MCP server 仍按声明它的 Session 隔离，不能仅因两 Session
同属 Workspace 就共享该客户端连接或工具可见性。Workspace 归属不替代每次
工具调用的权限审批，也不是多用户访问控制。

所有持久 MCP 响应缓存的存储键与读取范围必须包含 Workspace ID，包括标记为
Public 的响应，避免错误分类让数据跨 Workspace 复用；Workspace ID 未确定时
不得读写该缓存。其他
持久 MCP 数据（例如 workspace 专属的 server 状态或授权结果）遵守同一规则。
全局 MCP 配置仍是配置来源，Session transcript/frozen context 仍归 thread，
运行中的工具任务仍归执行生命周期；这些数据不因“Workspace 隔离”而改写
各自的所有权。新增持久数据需显式声明是 Workspace、Session 还是部署级作用域，
不能默认沿用 machine ID 或进程级 pool 作为鉴权范围。

## 3. 创建和查询

打开目录时，先取得当前 Machine ID 并确保其 Machine 行存在，再发现该目录
是否属于 Git worktree：Git 确认时使用发现出的 worktree 根，Git 确认不是仓库
时使用启动 cwd；Git 无法回答时使用启动 cwd 并标记 `unverified`，不把它称为
已证实的非 Git 工作区。以结果作为 Workspace.path 对 `(machine_id, path)` 做原子查找或创建；
并发创建必须返回同一个
Workspace ID。创建根 Session 时将该 Workspace ID 与 Session 元数据在同一
持久化操作中保存，不允许留下无归属的可用 Session。创建子 Session 时继承父
Session 的 Workspace ID，不能只凭子进程当前目录另建 Workspace。

列表以 Workspace ID 为归属过滤，按需再选当前/全部 Machine，以及普通/归档
状态。对不同机器同名路径的查询不得合并。按 Session ID 加载不附加当前
Machine、当前 Workspace 或当前 `pwd` 条件；列表筛选与读取权限是两回事。

## 4. 旧数据升级边界

写打开升级时，在一个可回滚的迁移边界内建立 Machine、Workspace 与 Session 归属；
旧 Session ID、消息 ID、消息顺序、父子链、flags、frozen/inherited context 与归档以外
的元数据保持不变。所有旧 Session 的 `archived` 置为 `false`，包括现有 `hidden` 子
Session；迁移不得把 `hidden` 当作用户归档意愿。

归属回填遵循以下规则：

1. 已记录机器环境的根 Session 使用该机器 ID，子 Session 沿根 Session 的机器身份与
   Workspace 归属；父子身份冲突时拒绝迁移，不猜测。来源未知的远端记录创建
   `identity_kind=legacy_unknown` 的 Machine 占位行并保留原标识，不能冒充本机 UUID。
2. 有本机绑定或可信旧登记时，以登记的 worktree 根/目录作为候选 Workspace.path；
   旧登记未持久化「Git 确认不是仓库」与「Git 不可用」的区别，目录模式一律标
   `path_source=unverified`。只能从保存文本可确认的来源格式解析跨操作系统路径；
   无法确认时以保存 cwd 作 `unverified` 历史归组。
3. 保存的 cwd 始终是 Session 的执行目录，不能被 root 覆盖；不得用当前终端 `pwd`
   或当前机器覆盖旧归属。保存路径不是绝对路径或归属关系损坏时，迁移明确失败并
   回滚，不得把数据丢入当前 Machine、空路径或随机 Workspace。目录暂时不存在不改变
   保存路径，也不妨碍只读历史。
4. 旧机器级 MCP OAuth 凭证没有可证明的 Workspace 归属：迁移在事务内清理，不复制给
   该机器的任一 Workspace，也不由首次访问的 thread 隐式认领；用户需在所需
   Workspace 重新授权。

迁移不读文件系统，因此无法为「没有旧绑定、也没有可引用旧登记证据」的会话伪造快照。
这类会话保留 `unverified` 归组行并按数量告警；首次本机接纳按保存 cwd 的当前观测
重建归属，入口幂等、可重跑，只在原归属行从未被观测过时收敛，已有执行证据或已有绑定
不静默改绑。本机核实不了的历史保持只读、按 ID 可读，拒绝执行时给出可完成的下一步
（在该目录新建会话），不报泛化的绑定数据错误。

迁移成功后每个 thread 都有 Workspace ID，旧环境表在同一事务内删除；机器归属由
`threads.workspace_id → workspaces.machine_id` 唯一推导，不维护第二份可独立修改的
归属。绑定中的 Project/worktree 信息若仍服务执行准入，只作为执行事实保留，不再充当
Session 的归属权威。v2 保留 `threads`、`messages.thread_id` 与现有 Session ID。

## 5. 边界与例子

| 情景 | v2 结果 |
| --- | --- |
| 机器 A、B 都从 `/repo` 打开 | 两个 Machine、两个 Workspace；路径相同不合并 |
| 机器 A 分别从 Git worktree `/repo`、`/repo/src` 打开 | 同一个 Workspace，path 为 `/repo`；Session 各自保留原 cwd |
| 同一 Git 项目的主工作树 `/repo` 与 linked worktree `/feature` | 两个 Workspace，分别以各自 worktree 根为 path |
| 机器 A 从两个不同的非 Git 目录打开 | 两个 Workspace，各自 path 为启动 cwd |
| 机器 A 再次从 `/repo` 打开 | 复用原 Workspace ID，创建新的 Session ID |
| 迁移后无绑定的历史会话再次打开 | 首次本机接纳按保存 cwd 的当前观测重建归属并写绑定（幂等、可重跑）；核实不了保持只读、按 ID 可读；已认领过但绑定记录丢失（有 frozen 无绑定）的会话只补绑定，既有 frozen 字节不回写 |
| 无绑定的子会话再次打开 | 归属继承自父会话：父会话已在本机核实接纳时绑定身份与 frozen 逐字继承（不重建、不另立），父会话未被核实即拒绝并提示先加载父会话 |
| Git 暂不可用时从 `/repo/src` 打开，之后才发现根 `/repo` | 旧 Workspace 保留 `unverified` 的 `/repo/src`，新会话使用 `/repo`；不静默移动旧 Session |
| `/repo` 被删除后同路径重建 | `(machine_id, path)` 仍指同一 Workspace；旧 Session 的执行仍需独立判断环境可用性 |
| Session 被归档 | 普通列表不显示，归档列表显示，按 ID 历史可读 |
| 打开异机 Session | 可按 ID 读取；不会被当前机器同名 Workspace 自动接管执行 |
| 本机身份文件丢失、共享库里有多台 Machine | 产生新 ID；列表展示旧机器供显式恢复，不自动选同名或同路径记录 |
| 同一 Git worktree 的两个 thread 连接同一 MCP server | 共享所属 Workspace 的 OAuth 凭证，仍分别遵守工具权限与会话生命周期 |
| 主工作树与 linked worktree 连接同一 MCP server | Workspace ID 不同，凭证和私有缓存隔离，不沿用对方的 token |

此结构只定义数据身份与归属，不提供跨实例单执行者、并发续写去重或多用户授权。

## 6. 落地接口与数据流

### 6.1 资源端口

`peri-acp-types` 定义小型领域输入/输出，`peri-resources` 的本机/远端 adapter
实现同一行为，不向 ACP/TUI 暴露 SQL、数据库路径或凭证正文：

| 行为 | 输入 | 后置条件 |
| --- | --- | --- |
| 本机当前身份（`current_machine_id`） / `list_machines` / `list_workspaces` | 部署身份 / Machine ID | 返回 Machine ID、name、当前标记及 Workspace 摘要；只读列表不创建身份或表行 |
| `rename_machine` | Machine ID、非空展示名 | 只改变 name，不改变 ID/归属；未知 ID 失败 |
| `resolve_workspace` | 启动 cwd、发现结果 | 按 `(machine_id, path)` 原子查找或创建；返回 Workspace ID、path、path_source、实际执行 cwd；无法证明 Git 时标 `unverified` |
| `create_session` / draft / fork / child | 已解析 Workspace 或父 Session 身份 | 根/独立 fork 写 `threads.workspace_id`，child 继承父值；归属与现有创建事实同提交，不可提交 null 或异机 Workspace |
| `load_session_snapshot` / `load_session_history` / `load_session_meta` / `list_sessions` | ThreadId / `ScopedThreadQuery` | ID 读取不附加当前环境条件；列表只在数据端过滤已发布且非空的根 thread，携带 Workspace/Machine 展示投影 |
| `set_session_archived` | ThreadId、目标 bool | 只允许已发布根 thread；幂等更新 archived，不变更内容活动时间、消息、执行资格或 child 行 |
| `oauth_credentials_for_workspace` | 经可信 Session/Workspace 准入得到的 WorkspaceId | 返回只作用于该 Workspace 的凭证端口，端口方法不再接收 WorkspaceId；缺失/不可信归属拒绝 |

ACP 的 `peri/machines/list`、`peri/workspaces/list` 与 `peri/session/archive` 使用版本化
Peri 扩展；机器重命名目前只有存储端口 `rename_machine`，尚未暴露 ACP 方法。
现有 `session/new/load/resume/fork/delete` 的 Session ID 和标准 wire 语义不改。
TUI 从这些扩展展示当前机器、Workspace 分组与普通/归档列表，不在客户端
重新实现 Git 发现或凭证作用域判断。跨 env 只读历史仍可打开，但不因此获得
该 Workspace 的 OAuth 凭证、MCP pool 或工具执行能力。

### 6.2 数据表与写入顺序

canonical DDL 在 `peri-resources/src/sessions/canonical.rs` 保持一份，本机 SQLite
和远端用相同表名及列语义。下表只列 v2 相关表的现行关键列；现有消息、frozen、继承区、
父链与配置列不因本设计改名。

| 表 | 关键列 | 数据约束 |
| --- | --- | --- |
| `machines` | `id`, `name`, `identity_kind` | `id` 主键；known ID 为规范 UUID；name 非空且可改；legacy_unknown 不可作为本机当前身份 |
| `workspaces` | `id`, `machine_id`, `path`, `path_source`；`project_id`、`identity`、`discovery` 为诊断列 | `id` UUID 主键，`machine_id` 非空引用 machines，`path` 非空，`UNIQUE(machine_id,path)`；远端可创建无本机文件证据的行 |
| `threads` | `workspace_id`, `archived` | workspace_id 非空引用 workspaces；archived 非空默认 false；`parent_thread_id` 及其他列保留 |
| `session_bindings` | `thread_id`, `schema_version`, `project_id`, `workspace_id`, `relative_cwd`, `discovery_snapshot`, `evidence_origin` | 逻辑归属只通过 `threads.workspace_id` 读取；`workspace_id` 与会话归属行同值；旧远端缺证据不伪造，新创建必须有完整快照 |
| `mcp_oauth_credentials` | `principal_id`, `workspace_id`, `server_key` | 主键 `(principal_id,workspace_id,server_key)`；不能通过 machine_id 或 server 名兜底查找 |

升级来源是正式基线 ≤10（本机先补齐到 V10 形状）与 11 的两种形状；当前版本是
schema 12，远端 `peri_store_meta.schema_version` 取同一版本常量，存储契约标签为
`peri.session.store/v6`，可读旧契约为 `v2` 的 10|11 与 `v5` 的 11。11 的当前形状
只缺派生索引表，走只增派生对象的增量迁移；11 的旧登记形状与 ≤10 走搬运批次，
不认识的版本直接拒绝。远端升级是一条受管原子批：在旧 schema/contract/store 身份
与对象 SQL 快照守卫下搬运全部历史行、重建受影响表、清理旧凭证，最后把版本与
contract 同批推进。远端批结果未知时重读身份、形状和数据验收摘要，不能把超时当成功。

新 writer 不接受旧 schema 继续写入；只读打开按已识别形状使用对应版本的 SQL 投影与
decoder 读取，不做迁移。写打开先检查版本、必需表列、外部依赖与旧数据完整性，再在
同一可回滚边界完成 DDL、数据回填、索引和版本推进。旧环境表回填并入这条迁移事务；
SQLite 的表重建在事务外处理外键开关并在提交前运行外键检查；远端用受支持的托管原子批
守卫旧版本及 store 身份，响应未知不得当作迁移完成。失败时旧数据和版本保持原样；
不能先清旧凭证再发现会话归属无法迁移。

形状判定只有一份声明（`sessions/schema_shape.rs`，本机与远端共用）：当前形状
逐表严格比对列序列、NOT NULL 与主键，`mcp_oauth_credentials` 与
`session_close_intents` 允许缺表（前者保持缺失、凭证能力如实上报不可用，后者由写
打开幂等补齐），存在时必须同形；迁移输入带上下界，未知列没有去处即拒绝升级
（fail-closed）。远端在打开时（含只读）先用这份声明做一次只读形状探测，判定不过
即拒绝，不发 DDL。搬运重建 `threads` 与 `session_bindings`：SQLite 与远端执行器
都不能把已有列改成 NOT NULL，两端也不用 `ALTER TABLE ... RENAME`；重建表上不承载
数据的挂载对象（索引、触发器）不随表搬运——索引由迁移末尾整表重放 canonical
集合补回，索引是否齐全由测试断言，不参与形状判定。

新根 Session 的持久化顺序是：取得 Machine → 发现 Workspace.path → 原子查找或
创建 Workspace → 在创建 Session 的同一事务/托管批内确认 Workspace 归属并写入
`threads.workspace_id`、执行快照和原有创建事实。远端保存后由进程内执行端口按远端快照复核绑定；执行所有权由 `peri-sdk` 管理；
保存成功但准入失败继续如实报告，不能回滚已确认的远端数据或认作执行成功。

### 6.3 旧记录回填规则

Workspace.path 优先取旧登记/可证实的 worktree 根；只有保存 cwd 时，扣除已校验的
relative_cwd 后按组内交叉核对；无法证实 Git 根时使用保存 cwd 并标 `unverified`。
Machine 取根 Session 原环境并由 child 继承；来源未知的远端记录使用
`legacy_unknown` 占位身份。执行证据只能复制最后观测值并标
`legacy_last_observation`，不能声称创建时证据；缺远端快照的记录只读历史，不自动
补造。无绑定的本地记录按首次本机接纳的当前观测重建归属（幂等、可重跑），原归属行
已有执行证据或已有绑定时不静默改绑；有 frozen 缺绑定的会话只补绑定（write-once），
子会话绑定身份继承父会话；远端不接纳 legacy。

同一旧 Workspace UUID 跨 Machine 时按 Machine 拆分，同一 Machine/path 的多个旧对象
登记按路径合并；合并或拆分都不改 Session ID，并先完成旧证据复制与分组映射，才以
`threads.workspace_id` 作为唯一归属。历史路径校验与当前宿主平台无关：接受 POSIX
绝对路径、Windows 盘符绝对路径和 UNC 路径，拒绝相对路径；不读取当前文件系统，
也不由历史路径推导执行资格。

### 6.4 MCP 装配与作用域

当前 `OAuthCredentialPort` 方法只带 server key，适合保留为**已绑定 Workspace
的能力**；Resources 按可信 Workspace ID 构造端口，端口持有该 ID 并在每条
SQL 的 `WHERE`/主键中使用。ACP 装配先由 Session 取得 Workspace ID，再构造
其 MCP pool 与凭证客户端；host 级尚未绑定 Session 的 pool 不得读取或刷新
凭证。静态 server 的连接、OAuth flow、refresh 和本地响应缓存要么由 Workspace
专属 pool 持有，要么在共享 pool 的全部键中包含 Workspace ID；不能只替换
数据库主键而继续让进程级连接泄漏旧 token。动态 MCP 仍保留 session/incarnation
身份，并额外受所属 Workspace 限定；ACP 声明的连接仍按 Session 可见。

## 7. 验收情景

- 两台 Machine 使用同一共享库、相同路径：Workspace ID、凭证、缓存互不串用；
  按 ID 历史读取不受影响，异机执行仍被拒绝。
- 同机同 Git worktree 的根与子目录：Workspace ID、OAuth 凭证相同，Session cwd
  各自保持；linked worktree 独立。
- 同机同路径目录对象重建：Workspace ID 不变，旧绑定的执行快照不被新发现覆盖；
  旧 Session 只要 cwd 仍可用就可继续执行，新 Session 保存新快照；项目沿用原
  workspace 行登记值（只有新路径才按 canonical locator 选择/复用项目）。
- Turso 写打开、只读打开及冷恢复在全新或损坏本地 SQLite 的 HOME 均不访问本地库。
- 旧本机库、旧远端库、跨 Machine 共用旧 Workspace UUID、Git 不可用与缺目录：
  迁移保存 ThreadId/消息/frozen，归档全 false；不确定归属或证据保持只读。
- 旧远端由异机先升级：原机器仍能按 ID 读取历史；远端已有快照才可执行复核，
  缺快照的记录保持只读。
- 两阶段草稿、零消息、child、归档/取消归档、按 ID 加载分别保持列表和历史
  契约；归档不改变运行资格。
- 身份文件丢失后，列表如实显示新旧 Machine，只有显式切换并重启才使用旧 ID；
  未确认前不复用旧 Workspace 凭证。
