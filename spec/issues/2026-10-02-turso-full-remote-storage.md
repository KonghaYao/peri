# Turso 全量远端存储切换

状态：实施完成，独立验收通过；真实 Turso 云端验收待凭证。权威语义见 `docs/design/storage-v2-machine-workspace-session.md`。

## 裁决与范围

选择 Turso locator 后，Session、Machine、Workspace、消息、绑定、MCP OAuth 与可持久的执行证据只使用远端会话库。本机可以读取机器 ID 文件、访问工作目录、运行 Git 发现；不得打开、创建、迁移或查询本地 SQLite，包括 `~/.peri/threads/threads.db`。本地 locator 继续使用既有 SQLite adapter。当前 schema 为 14，执行所有权唯一由 SDK 管理，Peri 仅保留 binding/path 校验与任务资源生命周期，不持有执行 lease、Store owner 或 Workspace fencing。WASM 工作树的只读端口分离可借鉴，不能直接移植其 schema 11 或仅只读的实现。

## 实施计划

1. **拆开执行环境校验与 SQLite。** 将 binding/path 校验及未决持久化约束封装在存储无关的执行环境模块；本机 SQLite adapter 继续使用同一实现。为远端组合装配不持有 `SqliteSessionDatabase` 的执行端口，读写和只读访问模式都不访问本地 SQLite，不重新引入执行所有权。
2. **远端 Workspace 归属。** 文件系统/Git 发现仍在本机运行，linked worktree 按其 worktree 根归组。按当前 Machine ID 与规范路径从 Turso 查找已有 Workspace ID；缺失时使用同键稳定生成的 UUID，创建 Session 的远端原子批确认 Machine/Workspace 后写 Session。并发竞争必须收敛到同一 ID，不得因本地登记丢失生成冲突 ID。发现快照可短暂驻留进程内供新 Session 创建，但不能成为持久归属事实。
3. **执行证据与恢复。** 新 Session 的不可变发现快照随绑定保存在 Turso；重启后的准入从远端绑定、Session 归属与保存的快照读取，再对当前文件系统/Git 作复核，不依赖本机登记表。旧远端缺失快照时保留按 ID 只读历史，拒绝凭当前同名目录自动补造执行资格。SDK 管理执行唯一性，Peri 保留未决持久化、cancel、关闭和 fork/child 的生命周期约束；环境复核不授予执行所有权。
4. **移除远端装配的本地库前置条件。** `Resources::open_remote` 与远端组合不计算默认 SQLite 路径，也不调用 `LocalExecution::open*`。远端只读在全新 HOME 中可打开已有 Turso 库；远端写打开也不创建本地 `threads.db`。本机库升级只在选择本地 locator 时发生。
5. **同步事实源。** 更新 v2 权威设计、架构契约、资源 code-index 与旧的远端部署测试说明，删掉“远端需要本机登记库”的现行表述；保留旧本机库的本地模式及其迁移语义。

## 验收

### 实施证据（2026-10-02）

以下为当日验收事实，含后来已删除的 lease/门禁实现，不代表现行执行所有权契约。

- 远端装配只构造 `RemoteSessionData` 与进程内 `RemoteExecution`；本地 SQLite 的默认路径与连接只在本地 locator 分支出现。
- 执行 lease/门禁和文件系统发现分别移到存储无关的 `sessions/execution.rs`、`sessions/discovery.rs`。远端按 `(Machine ID, canonical root path)` 查归属；无行时以受域隔离的 SHA-256 UUIDv8 稳定生成，Session 原子批确认远端归属。
- 冷恢复从远端保存的发现快照复核；旧 v2 绑定 UUID 可与逻辑 Workspace UUID 不同。缺快照、异机、目录对象变化只读历史，拒绝执行；缺快照还拒绝历史追加、元数据修改及 legacy 接纳，不能用当前发现结果补造旧证据。
- `peri-resources --lib` 资源库全量测试：423 passed、0 failed、21 ignored。离线 SQLite 传输模拟测试覆盖损坏本地 `threads.db`、旧 v2 UUID、冷恢复、目录变化、异机和缺快照。真 Turso 凭证未配置，网络回环及跨进程云实验保留为待运行验收。

- 使用隔离的 HOME 与可观测文件系统/SQLite 构造器，远端只读及可写打开都不创建或打开本地 `threads.db`；故意放入损坏本地库也不影响远端历史读取。
- 同机同规范路径多次打开和并发创建收敛到一个远端 Workspace ID；不同 Machine 或 linked worktree 不串归属及 OAuth。
- 新 Session 在进程重启后可由远端快照复核执行；目录对象变化、异机、缺快照或缺路径均只读历史，不能执行。
- 本地 SQLite 模式的 Session 行为与迁移测试仍通过；远端模拟传输的读写、迁移及关闭测试通过。无真实 Turso 凭证时如实标注网络验收未完成。
- `cargo fmt --check`、按范围 Cargo 检查/测试、预提交门禁通过；提交仅包含本任务改动，保留共享工作树原有 WIP。
