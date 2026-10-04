# WASM 接入的主线边界收敛

状态：主线边界已合入 WASM 分支；Node 与本地 `workerd` 的 ACP、模型、持久会话和重启恢复回环已重新通过。受信远端 Workspace 的部署接线仍待验证；托管 Workers 不在本轮验收范围。

## 目标与边界

在主线收敛原生与受限部署共用的 Host、Session、MCP 和 ACP transport 边界，为后续合入 `peri-wasm`、构建补丁及 SDK transport 提供稳定入口。本轮不接管具体 WASM 部署实现，也不把本地 `workerd` 验证扩大为托管环境结论。

按 `ARC-TOOLS-001`、`ARC-CAPABILITY-CLOSURE-001`、`ARC-HOST-SHUTDOWN-001`、`ARC-WORKSPACE-001`、`ARC-TURSO-STORAGE-001`、`ARC-MCP-ACP-001`、`ARC-TRANSPORT-001` 和 `ARC-MIDDLEWARE-001` 验收；现行行为以主线代码及契约测试为准。

## 工作项

### 1. Host 可选能力

在部署装配点提供 Cron、LSP、builtin MCP、插件发现、settings hooks 等实际可用能力所需的输入或句柄。会话 Host 消费这些能力，ACP 请求路由与 Agent 规则不按目标平台分叉。区分部署持有的关闭权与会话级资源所有权；保留原生关闭顺序和中间件链序。

验收：原生工具、hook、Cron、LSP 行为不变；受限 Host 能完成 session/new、load、prompt、cancel、close；缺席能力不进入工具目录、首轮模型工具列表、提示词或子任务继承面；排空后才消费部署关闭权。

### 2. 远端数据与执行环境组合

保留 `SessionResources` 的单一业务门面和 `LocalExecutionPort` 接缝。远端数据打开后，由部署提供的工厂创建执行端口；Native 使用真实目录观测，WASM 后续注入虚拟工作区观测。不得把目标平台条件放入 session 读写规则。明确虚拟工作区 ID、发现快照和执行准入与持久归属的关系。

验收：本地 SQLite 与 Native Turso 的读写、归属、冷恢复和关闭契约保持；注入受限执行端口后，固定 machine UUID 与 workspace root 可恢复同一 Session；machine/root 变化、缺失快照或执行资格时保留只读历史并拒绝执行。进程内 lease 不替代 Store CAS 与外部工具 fencing。

### 3. MCP 能力准入

凭证存储、stdio 子进程和 builtin handler 分别表达可用性。匿名 HTTP MCP 在无凭证存储时仍可建连；显式 OAuth 无凭证能力时给出确定的不可用状态，不静默匿名或发起无效授权流程。stdio 不可用在初始化与重连时一致处理。远端 Workspace MCP 仍由受信配置来源指定，其工具以当次 `tools/list` 为准。

验收：覆盖匿名 HTTP、显式 OAuth 缺凭证、stdio 缺席、builtin 缺席及受信远端 Workspace；发现、首轮模型工具面和执行均来自真实连接；ACP 承载 MCP 的归属、错误码和关闭契约不变。

### 4. ACP 原始帧桥

将分支 `peri-acp/src/transport/wire_bridge.rs` 移植为基于现有 MPSC transport 的适配器。桥只转换 JSON-RPC 帧，不实现 ACP 方法或第二份 dispatcher。

验收：字符串/整数 request ID、正反向请求与错误码、通知、cancel 越过未完成请求、显式关闭对双向 pending 的结算及 Drop 对内部反向 pending 的结算均有真实 wire 测试；符合 `ARC-TRANSPORT-001`。

## 实施顺序与待裁决项

帧桥可独立落地；Host、Session、MCP 按模块分别实现并在主线集成验收。具体 WASM 调用方与部署脚本已在分支适配并合并。SDK Yjs 投影单独评审。

- **跨部署恢复**：本轮保证相同虚拟工作区身份的 WASM Host 重启恢复；Native 已创建会话是否可由 WASM 接管尚无批准契约。本轮不声称跨部署接管，实施时不得意外改变 Native ID 或快照规则。
- **远端 Workspace 来源**：受信配置的具体入口须按现行 Host/全局配置权威复核；项目或插件配置不得冒充保留实例名。若主线缺少可证明受信的入口，保留为后续部署接线条件，不降级来源限制。

交付前核对受影响的 standards、模块指引、code-index 与测试路由，记录命令终态和目标平台未覆盖面。未经要求不 commit。

## 本轮落地与验收记录

- Host 装配显式传入 builtin、stdio、Cron、LSP、插件和 settings hooks 可用性；部署关闭集随会话冻结，缺席的 builtin 不进入首轮工具或敏感工具提示。受限 Host 行为测试覆盖创建、prompt、关闭、冷加载；原生 Host 与 stdio 集成回归通过。
- `RemoteWorkspaceEnvironment::virtual_workspace` 将稳定 machine UUID 与 root 传入远端数据和执行端口；虚拟发现快照使用 `virtual-v1`，身份或快照不匹配时保留历史并拒绝执行。SQLite wire 的远端冷恢复及只读退化测试通过；本机 SQLite 公共门面跨进程契约测试通过。
- MCP 准入覆盖匿名 HTTP 实际握手、显式 OAuth 缺凭证、builtin、插件与 stdio 缺席；远端 Workspace 的**受信配置来源和目标部署接线仍待验证**，本轮不将其记为已验收。
- `WireBridge` 复用 MPSC transport；56 项 transport 测试覆盖帧桥正反向请求、ID、错误、通知、取消期间转发及 pending 结算。
- `build --locked --workspace`、`fmt --all --check`、层级导入检查通过。目标 crate 的 Clippy 完成且这次改动无新增提示；仓库既有提示使 `-D warnings` 未通过。文件尺寸检查仅剩 13 个既有超限测试文件。合并后 `peri-wasm` Emscripten release 构建、Node ACP/远端 MCP/生命周期回环及 Wrangler 本地 `workerd` 回环通过；均使用真实本地 sqld 与模拟模型服务。合并时补齐远端草稿创建的 owner 准入，并将共用 schema 版本推进至 13；Resources 库测试验证远端升级。本轮未验证托管 Workers。
