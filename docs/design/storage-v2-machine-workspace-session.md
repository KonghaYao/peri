# 存储 v2：Machine → Workspace → Session

> 状态：已批准目标设计；压缩后的 schema 11 已接入，真实远端验收待执行。当前行为以代码、契约测试和
> [Session ID / 机器环境设计](session-id-environment.md)为准。实施与验收记录见
> 进度与验收记录：[2026-10 月志](../../spec/history/2026-10.md)（2026-10-02 条目）。
>
> 后续变更（2026-10-08）：开发期形状（v12–v19，从未正式发布）引入的
> `legacy_execution_registrations` 已删除，其 `project_id`/`root_identity`/`discovery` 并回
> `workspaces` 归属行；`session_bindings.workspace_id` 收敛为会话归属行 ID
> （`threads.workspace_id`），不再存在独立的执行登记 id 空间。开发期各代在合并进 main 时
> 压缩为紧接正式基线的 **schema 11**（契约 `peri.session.store/v5`，唯一升级来源是 v2 的
> 10|11）；下文的「执行登记 UUID」「两个 id 不同」读作压缩前形状的历史形态，现行语义见
> [Session ID / 机器环境设计](session-id-environment.md) §3.2 与
> [active plan](../../spec/issues/2026-10-08-remove-legacy-execution-registrations-plan.md)。
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
| `workspaces` | `id`, `machine_id`, `path`, `path_source` | `id` 为 UUID 主键；`machine_id` 必须引用 Machine；`UNIQUE(machine_id, path)`；`path_source` 记录 discovered / derived_legacy / unverified |
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

### 沿用现有 `workspaces` 表

现有 `workspaces` 表保存 `project_id`、发现出的 `root`、`root_identity` 和
`discovery`，唯一键是 `(root, root_identity)`。本机用它登记和复核 Git/worktree
位置；`session_bindings.workspace_id` 引用它，列表用它还原执行目录。旧远端会话库
创建同形状的表，但旧执行登记曾只驻留本机，远端旧表通常没有这些登记行。
因此它可以作为 v2 Workspace 的**同一张表**继续使用，但现有唯一键、登记查询
和行含义必须迁移：增加 `machine_id`、`path`、`path_source`，以
`(machine_id, path)` 为归属键；原 `root`/`root_identity`/`discovery` 不再决定
Workspace 身份，也不能作为多个历史 Session 共用的不可变执行证据。远端也要
保存 Machine 与 Workspace 归属行，不能继续把该表视为空壳；v2 远端行不能
被旧 `project_id/root_identity/discovery` 非空约束阻止创建。

现有 Git worktree 登记的 `root` 与 v2 所需的 Git Workspace.path 对应，因此
一个 worktree 中多个启动 cwd 应继续引用**同一** Workspace ID。迁移按
`(旧 workspace_id, Session 的 machine_id)` 分组：同一旧 ID 若跨 Machine 使用，
必须拆为不同的 v2 Workspace；同一 Machine/path 若有多个旧文件对象登记，
必须收敛为一个 v2 Workspace。按 `(machine_id, path)` 排序处理目标键：优先
保留尚未被其他目标键占用的最小旧 UUID；没有可用旧 ID 时分配新 UUID。同一个
旧 UUID 不得出现在两个 v2 Workspace 中，并在同一事务中重映射相关 thread。
`ThreadId`、消息与历史不变。

现有 `SessionBinding` 中的 `workspace_id` 是旧执行登记的引用。v2 以
`threads.workspace_id` 为归属的单一权威；绑定需要 Workspace ID 时从 thread
读取，不长期维护可漂移的第二份归属。`session_bindings` 必须保存**每条新 Session
不可变的版本化执行发现快照**：创建时的 root、文件对象身份、Git private/common
directory 证据及执行目录相对路径。旧本机绑定只能复制迁移时登记的**最后观测值**：
旧实现可能原地更新 `workspaces.discovery`，故不能把该值冒称为 Session 创建时证据。
旧绑定标明 `evidence_origin=legacy_last_observation`，按旧环境规则重新复核；无法
证明原对象连续性时只读历史。新 Session 使用 `evidence_origin=creation_snapshot`。
旧远端迁移若没有远端保存的执行发现快照，快照明确缺失并保留原
`session_bindings.workspace_id` 执行登记 UUID；该 Session 仅可按 ID 读取历史，
不得用当前同名目录、本地旧 SQLite 登记或新的文件系统观测补造执行资格。
已有远端快照的旧 v2 Session 即使执行登记 UUID 与 `threads.workspace_id` 不同，
仍可按其远端快照、当前文件系统/Git、远端 Machine/path 归属重新复核。
当前形状不再保留这份双 ID：绑定行的 `workspace_id` 在迁移中改写为会话归属行 ID，
改写前逐行校验绑定记录的根就是归属行路径，任一行不成立即拒绝升级。

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

## 4. 旧数据迁移

写打开升级 v2 时，在一个可回滚的迁移边界内建立 Machine、Workspace 与 Session
归属；旧 Session ID、消息 ID、消息顺序、父子链、flags、frozen/inherited context
和归档以外的元数据保持不变。所有旧 Session 的 `archived` 置为 `false`，包括
现有 `hidden` 子 Session。迁移不得把旧 `hidden` 当作用户归档意愿。

1. 已记录 `session_environments.machine_id` 的根 Session 使用该机器 ID；子 Session
   沿根 Session 的机器身份和 Workspace 归属，发现父子身份冲突时拒绝迁移，不猜测。
   尚无该行的旧本机数据先按现行迁移规则归属本安装的 Machine；来源未知的远端
   数据按下面的占位身份处理。
2. 已有本机绑定的根 Session 使用其原 `workspaces.root` 作为候选
   Workspace.path：Git 会话的该值是所属 worktree 根，目录模式的该值是原目录。
   旧登记未持久化“Git 确认不是仓库”与“Git 不可用”的区别，因此目录模式
   迁移后的 `path_source` 必须是 `unverified`，不能伪称已确认为非 Git。
   远端旧库的 `workspaces` 通常为空；对其已有绑定，按保存的
   `threads.cwd` 和经校验的 `session_bindings.relative_cwd` 反推出候选 root，
   再拼接验证与保存 cwd 完全一致；同一 `(旧 Workspace ID, Machine ID)` 组内
   的各会话还必须得出同一 root。跨操作系统路径须按从保存文本可确认的
   来源格式解析，不能用迁移机器的路径规则猜测；格式无法确认时使用保存 cwd
   作 `unverified` 历史归组。保存的 cwd 始终
   是 Session 的执行目录，不能被 root 覆盖。未绑定旧会话先以保存 cwd 匹配
   可证实的旧登记；若目录仍可访问，可重新发现 Git worktree 根。没有可靠
   根证据时同样以保存 cwd 作 `unverified` 历史归组，不因此授予执行资格。
   不得用当前终端 `pwd` 或当前机器覆盖旧归属。
3. 旧远端的 `legacy:{store_id}` 或其他来源未知的环境不能冒充本机 UUID。
   为其创建 `identity_kind=legacy_unknown` 的 Machine 占位行，保留原标识；
   这些历史可按 ID 读取，但不会因本机出现同名路径而获得执行资格。
4. 保存 cwd 或旧 root 无法可靠表示为绝对路径，或归属关系损坏时，
   迁移必须明确失败并回滚；不得把数据丢入当前 Machine、空路径或随机 Workspace。
   目录暂时不存在本身不改变保存路径，也不妨碍只读历史。

现有 `mcp_oauth_credentials` 以 `(principal_id, machine_id, server_key)` 为键，
没有可证明的 Workspace 归属。v2 迁移不得把一条旧凭证复制给该机器的所有
Workspace，也不得由首次访问的 thread 隐式认领。v2 迁移在事务内清理旧
机器级凭证，不把旧值带入新 Workspace 键；用户需在所需 Workspace 重新授权。
迁移失败须回滚清理，不留下仍可被 v2 路径读取的机器级回退。

迁移成功后，每个 thread 都有 Workspace ID，旧 `session_environments` 表在同一
迁移事务内删除。机器归属由 `threads.workspace_id → workspaces.machine_id` 唯一推导；不得长期维护两份
可独立修改的机器归属。原 `session_bindings` 中的 Project/worktree 信息若仍服务
执行准入，应保留为执行事实，不再充当 Session 的归属权威。v2 保留
`threads`、`messages.thread_id` 和现有 Session ID；不做表名与协议的无关重命名。
v2 使用当前形状（schema 11）；旧库形状升级路径和远端事务能力见 §6.2。

## 5. 边界与例子

| 情景 | v2 结果 |
| --- | --- |
| 机器 A、B 都从 `/repo` 打开 | 两个 Machine、两个 Workspace；路径相同不合并 |
| 机器 A 分别从 Git worktree `/repo`、`/repo/src` 打开 | 同一个 Workspace，path 为 `/repo`；Session 各自保留原 cwd |
| 同一 Git 项目的主工作树 `/repo` 与 linked worktree `/feature` | 两个 Workspace，分别以各自 worktree 根为 path |
| 机器 A 从两个不同的非 Git 目录打开 | 两个 Workspace，各自 path 为启动 cwd |
| 机器 A 再次从 `/repo` 打开 | 复用原 Workspace ID，创建新的 Session ID |
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
| `current_machine` / `list_machines` | 当前部署身份 / 分页 | 返回 Machine ID、name、当前标记及 Workspace 摘要；只读列表不创建身份或表行 |
| `rename_machine` | Machine ID、非空展示名 | 只改变 name，不改变 ID/归属；未知 ID 失败 |
| `resolve_workspace` | 可信当前 Machine ID、启动 cwd、发现结果 | 按 `(machine_id, path)` 原子查找或创建；返回 Workspace ID、path、path_source、实际执行 cwd；无法证明 Git 时标 `unverified` |
| `create_session` / draft / fork / child | 已解析 Workspace 或父 Session 身份 | 根/独立 fork 写 `threads.workspace_id`，child 继承父值；归属与现有创建事实同提交，不可提交 null 或异机 Workspace |
| `load_session` / `list_sessions` | ThreadId / Machine、Workspace、归档状态及游标 | ID 读取不附加当前环境条件；列表只在数据端过滤已发布且非空的根 thread，携带 Workspace/Machine 展示投影 |
| `set_archived` | ThreadId、目标 bool | 只允许已发布根 thread；幂等更新 archived，不变更内容活动时间、消息、执行资格或 child 行 |
| `oauth_credentials_for_workspace` | 经可信 Session/Workspace 准入得到的 WorkspaceId | 返回只作用于该 Workspace 的凭证端口，端口方法不再接收 WorkspaceId；缺失/不可信归属拒绝 |

ACP 新的机器列表、重命名、Workspace 列表和归档操作使用版本化 Peri 扩展；
现有 `session/new/load/resume/fork/delete` 的 Session ID 和标准 wire 语义不改。
TUI 从这些扩展展示当前机器、Workspace 分组与普通/归档列表，不在客户端
重新实现 Git 发现或凭证作用域判断。跨 env 只读历史仍可打开，但不因此获得
该 Workspace 的 OAuth 凭证、MCP pool 或工具执行能力。

### 6.2 数据表与写入顺序

canonical DDL 在 `peri-resources/src/sessions/canonical.rs` 保持一份，本机 SQLite
和远端用相同表名及列语义。下表只列 v2 改动；现有消息、frozen、继承区、
父链与配置列不因本设计改名。

| 表 | v2 改动 | 数据约束 |
| --- | --- | --- |
| `machines` | 新增 `id`, `name`, `identity_kind` | `id` 主键；known ID 为规范 UUID；name 非空且可改；legacy_unknown 不可作为本机当前身份 |
| `workspaces` | 复用现表，新增 `machine_id`, `path`, `path_source`；旧 `project_id/root/root_identity/discovery` 的执行证据职责迁出 | `id` UUID 主键，`machine_id` 非空引用 machines，`path` 非空，`UNIQUE(machine_id,path)`；远端可创建无本机文件证据的行 |
| `threads` | 增加 `workspace_id`, `archived` | workspace_id 非空引用 workspaces；archived 非空默认 false；`parent_thread_id` 及其他列保留 |
| `session_bindings` | 历史列名 `workspace_id` 保留（压缩前形状存**执行登记 UUID**），保存版本化执行发现快照与 `evidence_origin` | 逻辑归属只通过 `threads.workspace_id` 读取；旧远端缺证据不伪造，新创建必须有完整快照。当前形状该列值收敛为会话归属行 ID，迁移逐行改写并 fail-closed 校验归属与路径，类型层不再有 `execution_registration_id` 双 ID |
| `legacy_execution_registrations`（仅本机迁移辅助） | 压缩前形状的旧登记 UUID → 最后观测的执行发现值 | 迁移把证据并回 `workspaces` 归属行后 `DROP`；当前形状不存在此表，也不再需要它是登记权威 |
| `mcp_oauth_credentials` | machine_id 改为 workspace_id | 主键 `(principal_id,workspace_id,server_key)`；不能通过 machine_id 或 server 名兜底查找 |

迁移版本从压缩前形状（正式基线 ≤10；本机先补齐到 V10 形状）推进到当前
schema 11；远端 `peri_store_meta.schema_version` 取同一版本常量，存储契约标签
从 `peri.session.store/v2`（10|11）推进到 `v5`。开发期曾分多代推进（schema 12
建立存储归属、14 移除执行账本、19 删除执行登记表并把绑定收敛到归属行），
合并进 main 时压缩为**一条**迁移，中间代 12..19 不再被接受。远端升级是一条
受管原子批：在旧 schema/contract/store 身份与对象 SQL 快照守卫下搬运全部历史
行、重建受影响表、清理旧凭证，最后把版本与 contract 同批推进。远端批结果
未知时重读身份、形状和数据验收摘要，不能把超时当成功。新 writer 不接受旧
schema 继续写入；只读打开按已识别形状使用对应版本的 SQL 投影与 decoder 读取，
不做迁移。写打开先检查版本、必需表列、外部依赖与旧数据完整性，再在同一可回滚
边界完成 DDL、数据回填、索引和版本推进。本机 `session_environments` 回填并入
这条搬运事务（压缩前曾是 `migrate_schema` 与 `migrate_environments` 两次提交）；
SQLite 的表重建在事务外处理外键开关并在提交前运行外键检查；远端用受支持的
托管原子批守卫旧版本及 store 身份，响应未知不得当作迁移完成。失败时旧数据和
版本保持原样；不能先清旧凭证再发现会话归属无法迁移。

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

### 6.3 旧数据回填的判定表

| 旧记录 | Workspace.path 来源 | Machine 来源 | 执行证据 |
| --- | --- | --- | --- |
| 本机有绑定及登记 | 旧登记的 root；同 worktree 子目录不拆分 | root Session 的 environment；child 继承 | 在改写旧登记前复制最后观测值，标 `legacy_last_observation`；不能声称创建时证据 |
| 远端有绑定但无登记 | 保存 cwd 减去已校验 relative_cwd；组内逐条交叉核对 | 保存的 environment | 保留旧绑定 UUID；缺远端快照时只读历史，不自动补造 |
| 无绑定但有可信本机登记（仅本地模式） | 保存 cwd 可证实的所属 root | 保存的 environment | 不伪造过去的快照；按本地 legacy 接纳规则执行；远端不接纳 legacy |
| 无法证实 Git 根的旧记录 | 保存 cwd，`path_source=unverified` | 已知原 env；未知来源为 legacy_unknown | 历史可读；不因归组获得执行资格 |

表中「执行证据」列记录压缩前形状的迁移规则；当前形状绑定 UUID 统一改写为会话归属行 ID，
不再保留旧绑定 UUID，缺快照的记录仍按只读历史处理。

历史路径校验与当前宿主平台无关：接受 POSIX 绝对路径、Windows 盘符绝对路径和 UNC 路径，拒绝相对路径；不读取当前文件系统，也不由历史路径推导执行资格。

同一旧 Workspace UUID 跨 Machine 使用时按 Machine 拆分，同一 Machine/path 的
多个旧对象登记按路径合并；合并或拆分都不改 Session ID。先保存每个旧 UUID 的
只读执行证据映射，再迁移读取旧
`session_bindings.workspace_id` 时必须先完成旧证据复制与分组映射，随后才能
以 `threads.workspace_id` 成为唯一归属。旧机器级 OAuth 凭证无 Workspace
归属，和旧 schema 一起清理，不复制到任一 Workspace。

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
  新 Session 保存新快照。
- Turso 写打开、只读打开及冷恢复在全新或损坏本地 SQLite 的 HOME 均不访问本地库。
- 旧本机库、旧远端库、跨 Machine 共用旧 Workspace UUID、Git 不可用与缺目录：
  迁移保存 ThreadId/消息/frozen，归档全 false；不确定归属或证据保持只读。
- 旧远端由异机先升级：原机器仍能按 ID 读取历史；远端已有快照才可执行复核，
  缺快照的记录保持只读。
- 两阶段草稿、零消息、child、归档/取消归档、按 ID 加载分别保持列表和历史
  契约；归档不改变运行资格。
- 身份文件丢失后，列表如实显示新旧 Machine，只有显式切换并重启才使用旧 ID；
  未确认前不复用旧 Workspace 凭证。
