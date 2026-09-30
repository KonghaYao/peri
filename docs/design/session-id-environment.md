# Session ID 恢复与机器环境分区

> 状态：现行设计；核心路径已实施，并发与端到端验收仍有未完成项。
>
> Scope：共享后端数据库中的会话身份、机器环境分区与恢复入口。
> 进度与验收见 [核心改动清单](../../spec/issues/2026-09-30-session-id-environment-core-change.md)。
> 本设计取代旧工作区设计中以目录绑定、文件锁和根执行 owner 限制恢复的规则；父子关系与运行资源生命周期保持。

## 身份与环境归属

Session ID 是唯一会话身份，不改为路径、机器地址或 env + path 复合主键。父子关系仍由 parent ID 关联，根通过父链确定。cwd、项目根、工作区路径与 `SessionBinding` 保留定位和展示语义，不是按 ID 恢复的归属凭证。

机器环境由 `peri-resources/src/sessions/machine.rs` 提供：

- 默认首次生成随机 UUIDv4，持久化到 `$HOME/.peri/machine-id`；后续启动读取同一文件，不用硬件指纹、PID 或连接地址。
- `PERI_MACHINE_ID` 可覆盖默认值，必须能解析为 UUID，解析后规范化；覆盖值不写入默认文件。进程通过 `OnceLock` 缓存身份，初始化后改环境变量不会切换身份。
- 首次创建在同目录使用 `create_new` 临时文件，Unix 模式为 `0600`，写入后 `sync_all`，再通过 hardlink 原子发布。若目标已存在，读取已发布值；最后尽力删除临时文件。已有文件损坏或不可读时返回错误，不静默重生成。
- 身份稳定性依赖保留该文件或一致的 override。复制 HOME/身份文件或复用 override 会共享 env；删除文件会产生新 env。它表达配置的机器环境身份，不证明硬件唯一性。

canonical `session_environments(thread_id, machine_id)` 保存归属。新 root 记录当前机器 ID；新 child 从父会话读取 env 并继承。插入 SQL 在父 env 缺失时回退当前机器，正常数据需先完成回填以维持树内一致；按 ID 打开不会改写 env。

## 查询与迁移

```text
list_sessions(ThreadScope::Environment(machine_id)) → 指定 env 的会话列表
load_session(session_id)                           → 按 ID 读取历史与元数据
resume_session(session_id)                         → 恢复历史；另行判定能否执行
```

`ThreadScope::Environment` 在本机 `sqlite_store/workspace.rs` 和远端 `remote/session_sql.rs` 中按 `session_environments.machine_id` 过滤；它是显式 scope，不表示所有列表已默认按当前机器筛选。按 ID 读取不隐式追加 env/cwd 条件，同名路径不构成跨 env 执行依据。

本机写打开经 `sqlite_store/schema.rs::migrate_environments` 在事务内幂等创建表/索引并回填；仍使用 schema 10，不提升 `user_version`。已有 env 不覆盖，缺失 root 按当前机器回填，缺失 child 沿根树继承。这里假设旧本地库属于本安装，不是识别导入来源的机制：复制到本机的无 env 库也会按此规则接纳。

远端写打开经 `remote/session_data.rs` 共用 canonical DDL 与回填 SQL。旧远端无归属 root 使用 `legacy:{store_id}`，child 沿树继承；已有 env 保留，避免将来源未知数据冒充当前机器。该值是迁移占位，不是机器 UUID。只读打开不做回填；env 表/行缺失时不能据此声称已经建立机器归属。迁移不自动发现硬件或移动会话到当前 env。

## 恢复与执行分开

ACP `requests/session_restore.rs::prepare_existing` 经 `host/workspace.rs::check_expected` 按 ID 读取保存的会话定位信息；请求 cwd 不再是准入条件，目录不存在、旧 dirty 代际或另一实例恢复不构成文件锁/owner 认领门槛。恢复先判 env 与执行可用性，仅可执行时才做 legacy 冻结/接纳并读取持久 frozen，避免准备阶段读取异机路径。只读恢复不要求 frozen 存在，也不启动 workflow/LSP；可执行路径的缺失、损坏或未知 frozen 与存储错误仍按既有契约报告，legacy 使用保存 cwd 而非当前终端 cwd。

实际执行由 `SessionResourcesImpl::execution_availability` / `acquire_execution` 与 ACP `require_owner` 控制：`execution_availability` 在执行/工具准入时检查保存 cwd 是否为实际目录，不把目录探测作为历史读取的条件；env 不匹配或保存 cwd 缺失/不可用时，ACP 以只读方式恢复历史，不装配可执行的会话环境。`sessionWorkspaceV1` 响应可携带 `read_only`，执行请求由宿主拒绝；不悄悄在当前机器同名绝对路径执行，也不自动把 session 迁入当前 env。这是执行准入，不是 Session ID 的访问授权。

session 执行 sidecar 文件锁及 TUI dirty/owner 恢复确认已移除；不再启用 `peri.sessionRecoveryV1` 协商或发出 `peri/session_reset_dirty`；caps 字段/序列化键仍保留为 false，不表示恢复机制仍存在。普通工具授权、删除等确认弹窗保留。`SessionExecutionLease`、`execution_runs` 与进程内 mutation gate 仍用于活跃执行、写入效果结清及关闭收尾，不代表跨进程互斥，也不能据其名称恢复旧认领语义。取消、子任务关系、MCP/LSP 关闭与事务仍需遵守自身生命周期。

## 保证边界

- hardlink 发布只解决初次身份文件的完整可见性与竞争创建，不代表整个系统全面并发安全。文件系统须支持同目录 hardlink；目录创建未强制私有权限，已有身份文件权限/符号链接未额外校验，非 Unix 不提供 `0600` 保证。未同步父目录，也未保证崩溃后无残留临时文件或身份目录项持久性。
- 同一 session 可被多个实例恢复，不承诺单执行者。SQLite 事务、现有幂等契约及单 lease 的进程内 gate 不提供跨实例/跨机器续写冲突策略，更不保证工具副作用不重复；相关验收仍见 active spec。
- Session ID 可查找性不是认证，env 分区不是多用户权限隔离；机器 ID 文件与 override 都不是安全凭证。数据库/服务访问授权独立，不能仅靠列表过滤或执行只读标记宣称安全。
- 本设计不承诺任意缺路径/缺快照/坏库均能完整恢复，也不宣称只读界面替代所有存储写权限检查。全面 URI/VFS、插件与 MCP 缓存迁移不是本次前置条件。
