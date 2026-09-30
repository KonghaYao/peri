# P2 文件系统依赖：实现方式与难度评估

状态：2026-09-30 源码级可行性评估；OAuth SQLite 方案尚未实施，不是工期承诺。

当前裁决：统一地址需求废弃，相关 URI/VFS、地址 resolver、跨环境输出 locator 与同步改造建议已删除。Session ID 恢复与机器 env 分区已实施，见 [核心改动清单](2026-09-30-session-id-environment-core-change.md)，不再列为待建地址体系。

范围来自 [文件系统依赖清单](2026-09-30-filesystem-dependencies-inventory.md)：本轮评估 MCP OAuth 凭据迁至 SQLite 新表。MCP response cache、插件缓存与插件体系继续暂缓、保持落盘，不修改其准入、失效或生命周期。落盘日志、SQLite 后端与 TUI 本地状态免除整改。

实施裁决：直接切换 SQLite 新表，不迁移旧 JSON、不做旧存储兼容或回退，也不双写。旧 token 不读取，用户需要重新授权；旧文件不自动删除。

## 当前结论

**可以用 SQLite 新表保存 OAuth 凭据。建议先落本机权限保护的 credential adapter，不把 token 当作 session 数据或 response cache，也不把共享数据库自动等同于共享授权。** 当前 rmcp credential 接口已提供替换入口；困难主要在依赖注入、多用户身份、刷新竞争及动态连接清理，而非建表。

| 方向 | 最小切片 | 难度 | 状态 / 边界 |
| --- | --- | --- | --- |
| 本机 OAuth SQLite 存储 | 新建凭据表，credential port 注入，替换全部文件构造点及清理入口 | 中 | 本轮评估；保持授权流程语义，不新增 schema 版本号 |
| 共享数据库中的个人凭据 | principal 隔离、受控访问或客户端加密、明确刷新协调 | 中高 | 当前没有可复用的用户身份授权模型，不作为单纯建表已解决的问题 |
| MCP response cache、插件缓存与体系 | 保持现有实现 | 暂缓 | 不进入本轮迁移 |

## OAuth：现状与可替换边界

- `peri-middlewares/src/mcp/auth_store.rs::FileCredentialStore` 是待删除的文件实现；新表直接保存完整 rmcp `StoredCredentials`，不复刻文件格式、锁或旧版本检查。
- `PerServerCredentialStore` 已实现 rmcp `CredentialStore::{load,save,clear}`，但内部硬编码 `Arc<FileCredentialStore>`；`OAuthFlowManager` 同样绑定该实现。这是可替换 seam，不是已完成的 adapter 注入。
- `initialize.rs`、`reconnect.rs`、`client_oauth.rs` 与 `dynamic/staged_connection.rs` 均有文件存储构造/使用点，必须统一走部署时注入的凭据能力，不能只改授权成功后的写入。
- 静态凭据按 server name 索引；同名配置更换 endpoint 或授权配置可能复用旧键。动态连接使用 session/incarnation/server 的隔离键，`DynamicOAuthCredentialGuard` 在 rollback、close 与同步 Drop 中调用 `clear_server_blocking`。SQLite 异步接口不能直接塞进现有同步 Drop。

## SQLite 表建议（待实施）

候选表名 `mcp_oauth_credentials`，不改 `threads` 或 session 父子关系：

| 字段 | 语义 |
| --- | --- |
| `principal_id` | 凭据使用主体，由可信部署/登录上下文提供；不能由 session ID 或 machine ID 推导成用户授权 |
| `machine_id` | 本轮默认限定凭据所在安装环境，复用已有持久机器 ID；不是用户身份 |
| `server_key` | 配置来源与服务器/授权配置的稳定身份；不能只用展示名称。动态键还要含 session/incarnation/connection 身份 |
| `credentials_blob` | 完整 `StoredCredentials` 序列化载荷；保留 client registration、token 等原有字段，不只保存 access token |
| `updated_at` | 诊断及更新时间，不是跨实例 refresh 互斥保证 |

建议唯一键为 `(principal_id, machine_id, server_key)`。所有 load/save/clear 都绑定同一完整键，clear-all 仅清当前主体与环境；server URL 在这里仅参与已有 OAuth 服务身份，不引入统一地址体系。

本机安装尚无用户主体模型时，可以由部署提供私有 credential namespace，但必须明确它只是本机单用户前提，不能宣称支持共享库中的个人权限隔离。以后确需同一用户跨机器共享凭据，单独调整环境 scope 与授权规则，不自动取消 machine 分区。

表由独立 credential adapter 幂等 `CREATE TABLE IF NOT EXISTS`，保持当前 schema 10 与会话序列化契约，不添加新的 schema 版本号。adapter 应由 Resources 层或专用能力持有，OAuth 消费侧只接收行为接口；不公开 session 私有 pool，不把 token 写进通用 Config MCP，也不新增 middleware 直接执行 SQL 的依赖。

按内部能力经 MCP 消费的边界，凭据能力应在部署装配时经独立、受信的 bootstrap 通道注入，不依赖尚待 OAuth 授权的目标工具池，否则会形成“取 token 先连目标、连目标先取 token”的循环。可由独立 MCP adapter 封装 Resources 的窄凭据端口，部署继续持有关闭权；这不要求独立进程，也不引入统一地址设计。

SQLite 的事务可替代凭据文件 read-modify-write 的文件锁；保留需要的运行时生命周期管理。它只能保证持久写入原子性，不能保证多个实例对同一 refresh token 发起的网络刷新自动串行，也不能防止旧请求覆盖重授权后的凭据。多实例刷新/注销协同仍需独立契约与条件提交验证。

## 新实现与动态生命周期

SQLite 是唯一凭据读写权威，删除旧文件存储实现及构造点，不保留导入器、迁移标记、兼容层或 JSON 回退。数据库失败如实上报，不输出凭据正文。

动态凭据仍只服务对应连接 incarnation。沿现有 MCP owner 的显式 rollback/close 生命周期等待清理；同步 Drop 只作为有定义的兜底，异步删除完成前不能报告已清理。崩溃残留的回收策略另外验收，不改成永久跨 session 授权。

## 密钥保护与验收边界

- **SQLite 并不自动提供 secret 隔离。** 原文件的权限保护不能在迁移后丢失，数据库、WAL/SHM、备份及故障输出都要纳入权限与泄露检查。
- 如果同一 session 数据库可被多个人直接读取，不建议把个人 OAuth 明文 token 直接并入；仅添加 `principal_id` 查询条件不足以阻止直接读表。可选本机私有凭据库，或经独立授权服务访问，或在已有密钥管理前提下存客户端加密载荷；不要自创加密协议。
- 本轮建议默认先采用本机权限保护边界。共享后端、主体验证、密钥管理尚未实施，不据此宣称多用户凭据安全；遵守 `ARC-SECRET-001`，日志、错误、遥测与测试不得出现真实 token。
- 后续最小验收：load/save/clear 及重启恢复，用户/机器/同名服务器隔离，refresh/重授权竞争，动态关闭/回滚/崩溃残留，只读/数据库失败及 secret 脱敏；不新增旧 JSON 迁移或兼容用例。

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

## 限制与当前未完成项

- 难度为静态调用链与契约判断，未运行 OAuth SQLite 原型、真实授权或并发刷新，不给工期承诺；旧数据迁移不在范围内。
- 未进行远端多用户、跨进程崩溃、Windows 权限或数据库备份泄露验证。
- compact 文件回读尚未移除；仍应删除回读而不是改为 MCP 回读，历史工具调用记录及纯摘要元数据保留，见文件系统依赖清单。
- 本轮仅更新评估与范围清单，未修改 OAuth、MCP cache 或插件运行时代码。
