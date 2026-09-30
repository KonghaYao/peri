# Perihelion 领域语言

这里记录会话归属与执行环境的领域用语。

## 会话与工作区

**项目（Project）**：用户查看和组织相关会话的归属单位。一个本地 Git 仓库及其关联 worktree 属于同一项目；独立 clone 默认是不同项目。
_Avoid_: 用当前目录、分支名或远端地址代指项目身份。

**执行工作区（Workspace）**：会话实际使用的一份文件树。Git 主工作树、每个 linked worktree，以及非 Git 项目的目录，分别构成执行工作区。
_Avoid_: 与项目、Cargo workspace 或整个编辑器窗口混用。

**执行目录（Execution directory）**：会话解析相对路径、启动工具与发现局部项目指引的目录，可以是执行工作区的子目录。
_Avoid_: 用项目根目录替代会话原本的执行目录。

**会话（Session / Thread）**：具有独立身份与连续历史的工作记录；打开它的终端位置不决定它的归属。
_Avoid_: 将一个文件系统路径或一个进程等同于一个会话。

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
