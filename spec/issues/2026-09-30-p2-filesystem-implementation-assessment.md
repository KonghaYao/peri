# P2 文件系统依赖：实现方式与难度评估

状态：2026-09-30 源码级实施可行性调研，未实施；不是已批准的完整设计或工期承诺。

范围来自 [文件系统依赖清单](2026-09-30-filesystem-dependencies-inventory.md)：统一地址、MCP 凭据/响应缓存和宿主插件缓存扫描。落盘日志免除；SQLite 后端无需整改；TUI 本地状态免除。本轮只改文档，未运行原型、跨进程实验或远端 E2E。

最新裁决：插件系统与 MCP 缓存均推迟，保留现有落盘实现及权限、认证与缓存准入；后端注入、插件扫描收口与远端迁移都不进入当前实施队列，不作为核心问题整改的前置条件。下文相关方案仅保留为后续调研参考。

## 结论与难度

以下难度是基于现有调用边界、状态与安全约束的定性判断，不是实测工时。

| 方向 | 推荐最小实现 | 难度 | 不能省略的部分 |
| --- | --- | --- | --- |
| 工作区/执行环境定位契约 | 工作区身份 + 执行环境定位 + 相对 cwd，由 provider 解析实际路径 | 中 | 持久元数据、旧会话处理、显示路径与执行路径分离 |
| 跨机器执行与恢复 | 按执行环境路由 Workspace MCP，环境不可达时明确失败 | 高 | 受信准入、绑定校验、代际/lease、取消与恢复，不回落本机 |
| 输出引用跨实例恢复 | 稳定环境/制品身份与 provider 的持久索引 | 中高 | 生命周期、授权、过期/删除、环境消失后的行为 |
| 全面 URI/VFS 化 | 文件/目录/shell/LSP/缓存/同步统一 resolver/provider | 很高 | 触及大量既有契约；URI 本身不提供隔离与权限 |
| MCP response cache | 先支持 disabled/memory/local 后端注入 | 仅禁用低；完整注入中 | version、TTL、epoch、digest 与认证准入单一维护 |
| OAuth credential | 独立 credential store 注入，动态实例可先用 memory | 中；远端持久恢复高 | 命名空间、失效/重授权、刷新竞争、旧代清理 |
| ACP 插件搜索扫描 | 收回 PluginManagerPort，ACP 只消费类型化结果 | 低；远端接入中 | 结果与错误语义、取消、插件来源身份 |
| 插件制品跨环境驻留 | 安装/搜索/GC 与 artifact provider 一起驻留执行环境 | 高 | 制品被执行资源引用，不能按响应缓存任意驱逐 |

**当前推荐：先推进执行环境绑定与 Workspace provider 路由，不先做全面 VFS；插件与 MCP 缓存继续落盘，不参与本轮迁移。** 三类缓存的独立边界仅作为将来重新启动迁移时的参考。

## 统一地址：建议从执行绑定切入

已确认的基础：`peri-acp-types/src/workspace.rs::ResolvedWorkspace` 已有 project/workspace 身份、`cwd/root: PathBuf` 和相对 cwd；`SessionBinding::from_workspace` 固化 workspace 身份与相对目录。它们可作为迁移起点，但不等于已有跨机器地址解析。`peri-acp-types/src/session_store.rs::SessionStoreLocator` 是存储 locator 的既有先例，不应因此要求更换 SQLite 后端。

候选最小契约应区分：逻辑 workspace、执行环境、相对 cwd、显示路径、环境内实际路径。地址由部署/provider 解析；计算核心只持身份和相对地址。现有 Read/Write 等文件参数可先保持其工具环境语义，不要求第一批把每个工具参数全部改成 URI。这是实施建议，不是已实现契约。

执行环境身份须能跨实例持久保存，不能直接使用 workspace ID 或活跃实例的 generation 替代。`peri-acp/src/host/workspace.rs::expect_directory` 当前在宿主 canonicalize，`host/prepared.rs::PreparedSessionInputs::resolve_configuration` 按本地目录解析配置，`peri-agent/src/session/exec/executor_helpers/v2_execute.rs::V2ExecuteRequest` 仍传递 `cwd: String`；最小类型切片为中等难度，贯通这些入口与恢复链路的定位方案整体为高难度。

现有 builtin 并不是现成远端 provider：`peri-middlewares/src/mcp/builtin/runtime.rs::spawn_builtin_link` 使用本地 duplex，`builtin/dispatch.rs::builtin_server_handler` 构造本地 Workspace；`builtin/mod.rs::reserved_name_takeover` 禁止同名 command/url 接管，`workspace_io.rs::McpWorkspaceFileReader::request` 要求 builtin 来源。远端部署须建立明确获批的 provider 与信任入口，不能仅把保留的 workspace 配置改成 URL。

不能仅把 `cwd: PathBuf/String` 改为 URI 字符串：宿主目录检查、session 创建/恢复、子任务继承和工具实例选择仍需对应的环境解析与准入。跨环境不可达时应错误或只读恢复，不能拿旧绝对路径在宿主尝试执行。路径规范化也必须在对应环境内完成，不能对远端 URI 调用本地 canonicalize。

输出引用是独立的小切片：`mcp-packages/workspace/src/output_store.rs::OutputStore` 当前生成实例 UUID，URI→文件映射驻内存，文件虽然保留于工具环境，实例重建后仍不能凭旧 URI 恢复映射。稳定环境/制品 locator、持久索引与授权须一起设计；只增加 URI scheme 不解决此问题。

全面 VFS 只在第二个真实跨环境用例确有统一文件操作需求时评估。URI 不等于 cwd 沙箱；当前绝对路径与 symlink 行为不能在迁移中被悄悄改成另一套权限规则。

## 缓存：分成授权、响应复用、执行资源三类

### OAuth credential

- `peri-middlewares/src/mcp/auth_store.rs::FileCredentialStore` 使用全文件锁、版本检查、临时文件/同步/原子替换，Unix 文件权限为 0600；`PerServerCredentialStore` 已对接 rmcp credential 端口，但仍绑定具体文件实现。构造方包括 `initialize.rs`、`reconnect.rs`、`client_oauth.rs`，不能只换一个入口。
- 静态 key 当前按 server name；远端共享时应明确用户/租户、endpoint 与授权主体，不可直接把同名 server 的凭据混在一起。动态连接的 `dynamic/staged_connection.rs::DynamicOAuthCredentialGuard` 使用 session/incarnation/server key，关闭与回滚在同步 Drop 清理；跨网络删除、崩溃残留与过期回收必须重新定生命周期。
- 推荐先注入 credential store，保留本地实现；动态实例若不要求崩溃后恢复，可先用 memory。确需持久恢复时，再建设带 owner/lease 与安全通道的专用 capability。正常关闭清除不等于崩溃后必然无残留。

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

## 设备同步：不作为第一批前置改造

`peri-tui/src/sync/protocol.rs::SyncPackage/FileEntry/McpItem` 当前表达版本、相对路径/内容和全局/项目配置，项目条目没有 workspace 身份。`scanner.rs::scan_mcp` 从发送方 cwd 读取，`writer.rs::write_sync_items` 向接收方 cwd 写入；这不是跨设备 workspace 身份映射。

TUI 本地存储免除。先完成执行环境/工作区映射；确需跨设备恢复项目绑定时，再单独扩展 payload 身份与版本验收，不先重写加密传输。写入验收仍要保留 `writer.rs::validate_and_resolve` 与 `channel_flow/staging.rs` 的路径防护、暂存及回滚。此建议未做混合版本 E2E。

## 当前核心实施顺序与验收

1. 执行环境 locator 与持久 binding：同 workspace 在不同环境、旧会话加载、相对路径与显示路径分离；明确环境不可达的处理。
2. Workspace provider 路由与受信准入：关闭/换代/重连、取消与旧 lease、子任务继承、断连不回落本机；不允许用户配置冒充受信 builtin。
3. 输出 locator/索引的小型恢复切片：provider 重建、文件过期、环境不存在、无权读取及同 ID 错环境。

缓存后端注入、插件扫描收口与 artifact 外置全部暂缓；设备同步仍不作为第一批前置改造。缓存认证准入的疑点只是未验证风险，不是已确认漏洞，也不据此扩大当前迁移范围。

## 限制与当前未完成项

- 难度来自静态调用链与契约评估，未做工期、性能或生产数据测量，不给人日承诺。
- 未执行跨进程锁、崩溃恢复、远端断连、Windows 权限或混合设备版本验证；同进程新实例缓存复用测试不能视为独立 OS 进程实验。
- compact 回读尚未移除：`peri-agent/src/agent/compact_v2/full.rs::full_compact_inner → collect_reinject_v2 → read_file_with_budget` 仍从宿主读取 skills/recent files。应删除回读而不是下沉为 MCP 回读；历史工具调用记录及纯摘要元数据应保留，本轮没有修改 compact 代码。
- 落盘日志明确免除，无需整改；P2 本轮只有调研，无运行时代码改动。
