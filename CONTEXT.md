# Perihelion 领域语言

这里记录会话归属、执行环境与能力消费的领域用语。

## 会话与工作区

**项目（Project）**：用户查看和组织相关会话的归属单位。一个本地 Git 仓库及其关联 worktree 属于同一项目；独立 clone 默认是不同项目。
_Avoid_: 用当前目录、分支名或远端地址代指项目身份。

**工作区（Workspace；存储 v2 目标）**：某台 Machine 上的一份 Git worktree，或一个非 Git 启动目录；Git worktree 内的不同启动子目录属于同一工作区。
_Avoid_: 与多个 worktree 共享的项目、Git common directory、Cargo workspace 或整个编辑器窗口混用。

**执行目录（Execution directory）**：会话解析相对路径、启动工具与发现局部项目指引的目录，可以是执行工作区的子目录。
_Avoid_: 用项目根目录替代会话原本的执行目录。

**会话（Session = Thread）**：具有独立身份与连续历史的工作记录；Session 与代码中的 thread 是同一个持久实体，共用 `ThreadId`；打开它的终端位置不决定它的归属。
_Avoid_: 将一个文件系统路径或一个进程等同于一个会话。

**机器（Machine；存储 v2 目标）**：由本机持久 UUID 标识的机器环境及其用户可修改的展示名称；一台机器可拥有多个工作区。
_Avoid_: 用机器名称、硬件指纹、当前进程或用户身份代替机器 ID。

**归档（Archived；存储 v2 目标）**：用户对会话列表位置的选择，可撤销，不改变会话历史或运行资格。
_Avoid_: 与子 Agent 的 `hidden`、删除或运行终止混用。

**工作区鉴权范围（Workspace authorization scope；存储 v2 目标）**：同一 Workspace 的会话共享持久 MCP 鉴权，不同 Workspace 的凭证和私有 MCP 数据相互隔离。
_Avoid_: 用机器 ID、MCP server 名、当前进程或单条 Session ID 代替 Workspace ID。

**根会话（Root session）**：父子会话树的根，以 Session ID 标识。获批目标中，持有 ID 可在可访问的数据库内恢复，不以目录、持锁机器或运行实例判定持有权。
_Avoid_: 将根会话的数据库身份等同于某次执行的 owner。

**机器环境（Execution environment）**：路径与会话记录所属的机器位置命名空间，以持久的 execution_environment_id 在数据库中划分 env。
_Avoid_: 用 env ID 代替用户授权、互斥锁、瞬时连接地址或进程身份。

**执行绑定（Session binding）**：会话使用的项目、执行工作区与执行目录记录；获批目标中不承担会话持有权或按 ID 恢复的准入门槛。
_Avoid_: 用路径过滤条件决定会话身份或持有权。

**运行资源所有者（Runtime resource owner）**：负责某次运行及其任务、后台资源收尾的实例；不持有数据库中根会话的永久归属权。
_Avoid_: 将资源生命周期责任等同于恢复认领锁或跨机器单执行者保证。

**位置重定位（Relocation）**：同一执行工作区的文件系统位置发生变化，工作区身份保持不变。
_Avoid_: 与转到另一 worktree 执行混用。

**跨工作区续作（Continue in another workspace）**：从已有会话的历史开始，在另一执行工作区建立具有新环境的会话。
_Avoid_: 将普通 load/resume 或同环境 fork 解释为自动迁移。

## MCP 缓存

**MCP 响应缓存（MCP response cache）**：复用先前取得的工具描述、资源或技能响应，减少重复读取提供方的结果。
_Avoid_: 与已连接实例的当前能力目录、技能注册表、工具执行结果归档或模型提示词缓存混用。

**缓存总开关（Cache master switch）**：约束其管理范围内所有提供方是否允许复用和保存响应缓存的使用策略；允许使用不代表每个响应均可缓存。
_Avoid_: 与关闭提供方连接、撤销工具权限或清理历史缓存混用。

## 配置

**配置来源（Configuration source）**：提供配置输入的环境、文件或显式部署参数；输入的存在不代表其已通过校验或成为有效值。
_Avoid_: 与合并后的有效配置混用。

**有效配置（Effective configuration）**：配置输入经过默认值、来源规则与领域校验后，供业务使用的一份确定配置。
_Avoid_: 与某个配置文件的原始正文、进程环境或 UI 草稿混用。

**配置权威面（Configuration authority）**：Peri 内部负责定义并组装有效配置及其来源解释、变更接纳的唯一权威；业务消费配置，不自行再次决定来源与合并规则。
_Avoid_: 将只提供文件/环境输入的通道、外部来源提供方或任意共享可变 map 称为配置权威面。

**配置作用域（Configuration scope）**：`ConfigurationScope` 中的绝对执行目录与选中全局 settings 路径，界定一份项目配置视图。
_Avoid_: 与 session 身份、权限 scope 或 provider 所在机器环境混用。

**配置快照（Configuration snapshot）**：已采集输入经纯 typed resolver 得到的不可变投影；消费者持有同 scope/revision 的结果，reload 不改变旧快照。
_Avoid_: 与 UI 草稿、session frozen prefix 或可随文件变动自动热更新的 map 混用。

**配置版本（Configuration revision）**：由 scope、来源正文与具名环境内容确定的版本身份，用于解释与更新冲突检查。
_Avoid_: 与递增全局序号、时间戳、秘密值或访问令牌混用。

**编辑基线版本（Edit baseline revision）**：编辑开始时从基线配置快照捕获、随草稿保存并作为 expected revision 提交的 token；保存返回的 accepted snapshot 才是发布依据。
_Avoid_: 提交时读取最新 revision 代替旧草稿基线，或把 public core 的 token 参数等同于延迟 UI / 远程 wire 已接线。

**字节 CAS（Byte compare-and-swap）**：目标正文与预期字节相同才替换；区分文件不存在与空文件，合作写者在比较与替换间共用文件锁。
_Avoid_: 将它等同于多来源原子事务、对不合作编辑器的写隔离或全系统热更新。

配置术语的现行设计见 [configuration-authority.md](docs/design/configuration-authority.md)；插件生命周期、hook、OS 执行环境及存储 locator/credentials 仍遵守专属能力边界。
