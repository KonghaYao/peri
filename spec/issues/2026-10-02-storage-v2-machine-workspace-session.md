# 存储 v2：Machine / Workspace / Session 实施与验收

状态：详细设计已完成并经对抗审阅；实现进行中。

权威目标：[存储 v2 设计](../../docs/design/storage-v2-machine-workspace-session.md)。
现行行为仍以代码、契约测试及 [Session ID 环境设计](../../docs/design/session-id-environment.md)为准。

## 实施顺序

1. 契约类型与 Machine 身份查询/显式恢复；新增持久 Machine 记录。
2. 本机 11→12 原子迁移，保存旧执行登记映射，回填 Workspace/Session 归属与归档；
   新建、fork、child 和列表/按 ID 读取改用新事实。
3. 远端 11→12 守卫迁移，保留旧库只读 reader 和异机旧证据补齐路径。
4. Workspace 绑定的 OAuth 端口与 MCP pool 装配，覆盖连接、OAuth 流程和缓存。
5. ACP/TUI 机器、Workspace、普通/归档查询与操作，完成本机及真实远端验收。

对抗审阅确认五个实现门槛：远端 11→12 升级路径、旧 UUID 执行证据映射、
本机单事务、旧 schema 独立只读投影、MCP pool 初始化前的 Workspace 绑定。

当前实现：本机和远端生产写打开已接入 schema 12；`threads.workspace_id` 保存归属，
`ResolvedWorkspace` 区分逻辑 Workspace 与执行登记，11→12 迁移保留旧登记和逐会话
证据。资源门面、ACP 和 TUI 已接入归档，MCP OAuth 与响应缓存已按 Workspace
隔离。`peri-resources` 与 ACP 的库测试已覆盖主要迁移、归属和归档行为。
`peri meta machines` 展示当前 ID 与库内 Machine/Workspace，`peri machine adopt`
先预览目标，再以当前 ID 和无活跃执行确认显式替换本机身份文件，重启后生效。
TUI 浏览器展示当前 Machine。待完成的外部验收是使用真实远端库执行 schema 12
升级与 OAuth 网络回环；当前环境未配置远端凭证。旧 `session_environments` 表仍保留
旧版只读投影和兼容写入，v2 机器归属只从 Workspace 推导。

## 验收范围

- [ ] 本机与远端共用一份 v2 数据契约：Machine 登记、沿用并迁移现有 `workspaces` 表的 `(machine_id, path)` 唯一键、每个 `threads` 行的非空 Workspace ID；不新增 `sessions` 表。
- [ ] Machine ID 继续按 override → 读取身份文件 → 缺失时原子创建 UUIDv4 取得；Machine 行创建不覆盖已有展示名；提供当前机器与机器列表的名称/ID/Workspace 路径投影，以及身份文件丢失后的显式恢复操作，禁止按名称或路径自动认领。
- [ ] Git 子目录归组到所属 worktree 根，linked worktree 分开；非 Git 使用启动 cwd；Git 不可用时标记归属未核实且不静默改绑；新根 Session 创建原子保存归属，子 Session 继承归属，并发创建复用 Workspace ID。
- [ ] `archived=false` 默认值、归档/取消归档、普通/归档列表与按 ID 读取行为贯通资源门面、ACP 和客户端。
- [ ] 旧库迁移保留所有 ThreadId、Session 和历史；本机/远端各自正确推导 worktree 根，旧 Workspace ID 跨机器拆分、同机同路径合并，逐 Session 保留不可变执行证据；异机先升级远端时原机器仍可凭旧绑定 ID 和本机登记一次性补齐证据；旧 directory-only 来源标 unverified；全部旧 Session 归档状态为 false；未知机器来源不被当前机器认领；损坏或无法表示路径时无部分提交。
- [ ] 普通/归档列表都保持 `message_count > 0` 的已发布根会话过滤，不暴露两阶段创建草稿；子会话不可单独归档。
- [ ] MCP OAuth 凭证与所有持久响应缓存按 Workspace ID 隔离；同 Workspace 多 thread 共享凭证，异 Workspace 的 load/save/clear/list/refresh、连接复用及缓存读取均不串用；ACP 声明连接仍按 Session 隔离。
- [ ] 旧 `(principal_id, machine_id, server_key)` 凭证不自动复制或认领到 Workspace；v2 不读取机器级回退，用户在目标 Workspace 重新授权，并验收旧凭证清理。
- [ ] 现有执行绑定、环境准入、跨 env 只读恢复和远端适配与新归属不冲突；移除双重机器归属事实。
- [ ] 同步契约类型、schema 版本、标准、设计索引、代码索引及必要的行为测试；记录本机和真实远端的验证结果与限制。
