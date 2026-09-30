# P2 文件系统依赖：实现方式与难度评估

状态：2026-09-30 OAuth 数据库持久化、Resources 本地/远端 provider 与独立 credentials MCP 接线已完成；bootstrap 集成与 middleware 定向测试通过，实际云端、真实 OAuth 网络授权及多实例 refresh 仍未验证。验证证据见下文，不等同于全部风险验收关闭。

当前裁决：统一地址需求废弃，相关 URI/VFS、地址 resolver、跨环境输出 locator 与同步改造建议已删除。Session ID 恢复与机器 env 分区已实施，见 [核心改动清单](2026-09-30-session-id-environment-core-change.md)，不再列为待建地址体系。

范围来自 [文件系统依赖清单](2026-09-30-filesystem-dependencies-inventory.md)：MCP OAuth 凭据迁至既有配置数据库中的 `mcp_oauth_credentials` 表，覆盖本地 SQLite 与既有远端后端。MCP response cache、插件缓存与插件体系继续暂缓、保持落盘，不修改其准入、失效或生命周期。落盘日志、存储后端与 TUI 本地状态免除整改。

实施裁决：直接切换既有数据库的新表，不迁移旧 JSON、不做旧存储兼容或文件回退，也不双写。删除 `FileCredentialStore`；旧 token 不读取，用户需要重新授权，旧文件不自动删除。不新增私有凭据库、数据库配置、路径配置、HOME 查询、文件锁、用户认证模型或 schema 版本号。

## 当前结论

**凭据持久化复用既有配置数据库，Resources 提供窄端口，OAuth 经独立受信 bootstrap MCP 消费。** 不把 token 当作 session 数据或 response cache，也不把共享数据库自动等同于共享授权。逻辑 scope 固定为 `principal_id = local` 与初始化时缓存的 machineID，不建立用户认证模型；困难主要在完整接线、共享库权限、刷新竞争与动态连接清理，而非建表。

| 方向 | 最小切片 | 难度 | 状态 / 边界 |
| --- | --- | --- | --- |
| OAuth 数据库持久化 | 既有本地/远端 DB 新表，Resources provider，独立 credentials MCP 与 OAuth pool 注入 | 中 | 实现完成；bootstrap 编译/集成与 remote、middleware 定向验证通过；不新增 schema 版本号或数据库，真实部署及网络授权仍未验证 |
| 共享数据库的风险边界 | 核对既有部署访问授权、secret 保护与刷新竞争 | 中高 | 逻辑 scope 不是安全多租户隔离；不以本轮引入认证、加密或 CAS |
| MCP response cache、插件缓存与体系 | 保持现有实现 | 暂缓 | 不进入本轮迁移 |

## OAuth：已落地实现与边界

- `peri-acp-types/src/oauth_credentials.rs` 公共端口/请求类型、Resources 本地与远端 provider、`mcp-packages/credentials/src/{lib,server,client}.rs` 已落地；`peri-acp/src/host/assemble.rs` 将 Resources provider 建成 credentialsClient 并注入 OAuth pool。
- TUI 面板另有独立生产 pool；`peri-tui/src/app/mod.rs::spawn_mcp_init` 在 init 前从同一 `services.session_resources.oauth_credentials()` getter 建立 bootstrap MCP client 并注入，与 ACP host 共享既有 backend，不新增文件存储或数据库；入口见 [TUI panel 生命周期索引](../../docs/code-index/peri-tui.md)。此处记录接线实现，不声称已完成面板端到端授权验证。
- `FileCredentialStore` 已删除，新表保存完整 rmcp `StoredCredentials` 序列化载荷，不复刻文件格式、锁或旧版本检查；`PerServerCredentialStore` 和 `OAuthFlowManager` 消费注入能力。
- `initialize.rs`、`reconnect.rs`、`client_oauth.rs` 与 `dynamic/staged_connection.rs` 的授权、重连及动态清理已接入 credentialsClient；middleware auth_store、OAuth 与 dynamic 定向回归通过，结果见验证证据。
- 静态 server key 必须包含 endpoint 与授权配置身份，不能只用展示名称；动态键还绑定 session、connection incarnation 与 server 身份，避免旧连接的清理删除新授权。

## 数据库与 scope 实现

表名 `mcp_oauth_credentials`，建在已配置的本地/远端数据库中，不改 `threads` 或 session 父子关系：

| 字段 | 语义 |
| --- | --- |
| `principal_id` | 本轮固定为 `local`，只是逻辑 namespace，不引入登录或用户授权 |
| `machine_id` | 复用 Resources 初始化时缓存的 machineID，不在凭据请求中查询 HOME、重读文件或重建机器身份；不是用户身份 |
| `server_key` | 配置来源与服务器/授权配置的稳定身份；不能只用展示名称。动态键还要含 session/incarnation/connection 身份 |
| `credentials_blob` | 完整 `StoredCredentials` 序列化载荷；保留 client registration、token 等原有字段，不只保存 access token |
| `updated_at` | 诊断及更新时间，不是跨实例 refresh 互斥保证 |

唯一键为 `(principal_id, machine_id, server_key)`。provider 绑定 `local` 与缓存 machineID，所有 load/save/clear 都使用同一完整键；list/clear-all 仅作用于该 scope，请求方不提供任意 principal/machine。server URL 在这里仅参与已有 OAuth 服务身份，不引入统一地址体系。

该 scope 只防止逻辑误用，不提供安全多租户或同一用户跨机器共享授权；未来如需改变授权与环境 scope，须另立契约，本轮不建立用户身份系统。

表由 Resources 本地/远端 provider 在既有数据库幂等初始化，保持现有 schema 版本与会话序列化契约；不引入 schema 升版或旧数据迁移。公共类型集中在 `peri-acp-types/src/oauth_credentials.rs`，Resources 持有 SQL 与完整 scope，OAuth 消费侧只接收行为接口；不公开 session 私有 pool，不把 token 写进通用 Config MCP，也不新增 middleware 直接执行 SQL 的依赖。

已落地数据流：部署选定已有 Resources provider → 注入 `mcp-packages/credentials` 服务 → 通过独立受信 bootstrap MCP `CustomRequest` 建立 credentialsClient → host 将 credentialsClient 注入 OAuth pool → rmcp credential adapter。credentials 服务不自行解析 DB、路径或 HOME，不引入额外认证机制；部署持有关闭权与信任边界。这不要求独立进程，也不引入统一地址设计。

credentialsClient 必须独立于尚未授权的目标 toolpool，不能在该池发现凭据服务，否则形成“取 token 先连目标、连目标先取 token”的循环。bootstrap 不可得、关闭或 provider 出错时明确失败，不回落文件存储。

数据库写入使用既有后端能力，不新增文件锁或分布式锁。事务只能保护持久写入，不能保证多个实例的网络 refresh 串行，也不能防止旧请求覆盖重授权后的凭据；本轮不提供 CAS，多实例刷新/注销协同仍是风险，不作为已解决能力。

## 新实现与动态生命周期

既有配置数据库是唯一凭据读写权威，删除旧文件存储实现及构造点，不保留导入器、迁移标记、兼容层或 JSON/文件回退。数据库失败如实上报，不输出凭据正文。

动态凭据仍只服务对应连接 incarnation。沿现有 MCP owner 的显式 rollback/close 生命周期等待清理；同步 Drop 只作为有定义的兜底，异步删除完成前不能报告已清理。崩溃残留的回收策略另外验收，不改成永久跨 session 授权。

## 密钥保护与验收边界

- **SQLite 并不自动提供 secret 隔离。** 原文件的权限保护不能在迁移后丢失，数据库、WAL/SHM、备份及故障输出都要纳入权限与泄露检查。
- 既有共享数据库若可被多个人直接读取，逻辑 `local + machineID` scope 不阻止读表，明文 token 的保密性依赖部署访问授权、数据库/WAL/备份保护与受信 bootstrap；本轮不另建私有库、用户认证或自创加密协议，不能据此宣称多用户凭据安全。
- bootstrap 与远端数据库访问授权仍由部署保障，须遵守 `ARC-SECRET-001`；日志、错误、遥测与测试不得出现真实 token，日志落盘机制本身不整改。
- rmcp service 的 debug 日志会打印 `CustomRequest`/Result 原始载荷；`mcp-packages/credentials/src/client.rs` 将整个 credentials 独立 worker runtime 的执行包在 `tracing::subscriber::with_default(NoSubscriber::default(), ...)` 内，隔绝该 worker 的 SDK 原始载荷日志。production main runtime 不受影响；这是凭据通道的定向日志隔离，不是停用全局日志，也不替代共享库授权或 secret 保护验收。
- 待验收目标：本地/远端 load/save/clear/list/clear-all 及重启恢复，machine/endpoint/授权配置 scope、bootstrap 独立性与失效、refresh/重授权竞争、动态关闭/回滚/崩溃残留、只读/数据库失败及 secret 脱敏；不新增旧 JSON 迁移或兼容用例。此处列目标，不声称已有测试或已通过。

## 暂缓参考：MCP cache 与插件（本轮不改）

### MCP response cache

- `peri-middlewares/src/mcp/resource_cache.rs::McpResourceCache` 保存 content 与协调 state；`get_versioned` 联合处理 version/TTL/epoch，`ticket` 与 `put_ticket_versioned` 防止失效后的迟到响应重新落盘；本地短临界区有跨进程锁，RPC 不持锁。它不是普通 get/set KV。
- `mcp/client/cache.rs::persistent_cache_allowed_for` 对 dynamic connection 禁用持久缓存，并按配置中的 OAuth、headers、URL query、stdio env 进行敏感性准入；tools 缓存还要求 cache version。`mcp/resource_tool.rs` 的内容绑定/digest 验证与缓存提交不能被简单的 RPC-success→save 取代。
- 推荐先把后端变成 disabled/memory/local 的部署注入，业务准入与失效规则不复制。只有确需跨实例复用时，才外置提供原子失效与条件提交的 cache capability；把 read/write 移到远端不保留临界区语义，会引入竞态。
- **待验证的安全缺口，不是已观察到泄密：** `mcp/client_oauth.rs::start_oauth_flow` 支持没有显式 OAuth 配置时使用默认 OAuth，而缓存准入读取配置字段；实际认证状态与持久缓存准入可能不一致。应先定向复现，并以实际认证传输状态判断敏感性。相关路径仍会读取缓存资源列表，需一起核对。

### 插件缓存与制品

- `peri-acp/src/host/requests/plugin.rs::handle_search/search_marketplace_plugins` 从插件端口拿路径后自行扫描目录/manifest；已有 `peri-acp-types/src/plugin.rs::PluginManagerPort` 与 `peri-middlewares/src/host_ports.rs`，把搜索结果接口收回插件领域是小切片，不必先重写整个 Plugin MCP。
- `peri-middlewares/src/plugin/config.rs` 区分 marketplace checkout 与安装版本目录；`plugin/installer/install.rs` 将制品复制到版本目录并保存 install_path。版本可能来自声明、sha 前缀或时间戳，不能当作完整内容 digest 校验。
- `plugin/installer/uninstall.rs::cleanup_orphaned_plugins` 保护安装记录引用后清理孤儿版本；活跃会话/frozen 引用是否已完整参与 GC，本轮未穷尽。执行资源不能套用 response cache 的 TTL/任意驱逐。

Config MCP 可复用独立启动/部署注入的组织方式，但 `peri-acp-types/src/configuration.rs::ConfigurationRequest` 没有 credential、目录搜索、锁/CAS、artifact 生命周期语义。不要把 token JSON 硬塞进通用 Config MCP；其 TCP adapter 也没有认证/TLS，只适用于受信部署通道，见 `mcp-packages/config/CLAUDE.md`。

## 验证证据（主代理反馈，2026-09-30）

- MCP bootstrap `cargo check` 通过；这是该范围的编译证据，不是全 workspace 或真实授权验收。
- 主代理修正 shutdown 生命周期后，bootstrap MCP 集成测试重跑 4/4 通过，替代此前 3/4 的中间结果。
- ACP host 定向 worktree 装配测试 1/1 通过；这是宿主装配证据，不代替 TUI panel 端到端、实际云端或真实 OAuth 网络授权验证。
- `cargo check -p peri-tui` 通过，覆盖 ACP 依赖与面板装配的编译；不是面板端到端验证。
- Resources local/remote 既有 backend credentials providers 已落地；local 定向测试 8/8、remote 10/10 通过。
- local schema 扩大回归最初 25 passed、3 failed；两项新表清单断言按已批准契约补正后各自独立通过，保留原结构及数据断言。另一项旧执行准入测试仍期待 `BindingMissing`，与 session ID 恢复语义不一致，本轮未修改或独立验证；不宣称该扩大套件全通过。
- expanded remote 测试为 99 passed、1 failed、21 ignored；失败为 `session_child_guard_test` 未初始化 machine，主代理判定是无关故障，仍保留失败记录，不宣称该套件全通过。
- middleware 定向测试：auth_store 17/17、OAuth 34/34、dynamic 最新 46/46 通过，包含显式异步关闭及失败不虚报成功；以上为协调过程验证记录。worker 的 NoSubscriber 隔离已核对源码，不额外声称完成生产环境泄露验证。

## 限制与当前未完成项

- 实际云端部署、真实 OAuth 网络授权与多实例 refresh 未验证；数据库事务与逻辑 scope 不代表安全多租户或 CAS 刷新已解决。旧数据迁移不在范围内。
- 未进行远端多用户、跨进程崩溃、Windows 权限或数据库备份泄露验证。
- compact 文件回读尚未移除；仍应删除回读而不是改为 MCP 回读，历史工具调用记录及纯摘要元数据保留，见文件系统依赖清单。
- OAuth 运行时与存储实现已完成，bootstrap 集成及 middleware 定向回归通过；MCP cache、插件与既有日志落盘机制保持不动，credentials worker 单独隔离 SDK 载荷日志。
