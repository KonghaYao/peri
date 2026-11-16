# #409 审计：builtin MCP 单出口的实施门槛

**状态**：只读审计；尚未获得实施授权。

**验收**：设计门槛与验收矩阵见 [2026-11-15-v5-single-exit-acceptance.md](2026-11-15-v5-single-exit-acceptance.md)；本文只保留审计证据，不承担实施状态。

**基线**：`KonghaYao/peri#409`（2026-11-15 已裁决）及 `origin/main` `9d27a634e`。

**范围**：接替解决方案审计；不修改运行时、不改变既有会话配置。

## 结论

目标已裁决，但不能直接把六个 server 配置合并为 `peri` 后切默认。当前代码的实例身份同时承担注入、隔离、关闭、工具命名、资源和任务归属；缺少 workspace 上游代理以及跨能力的封闭授权。`#409` §10.2–10.4 尚未验收，原 issue 也明确不构成实现授权。先完成设计修订、代理 wire 契约和关闭矩阵，再进入影子实现。

## 核对证据与风险

1. **旧合同和目标冲突（阻断）**：`docs/design/mcp-adaptation-v4-part-1.md` 仍要求每实例独立 transport / client pool；`docs/standards/architecture-contracts.md` 的 `ARC-CAPABILITY-CLOSURE-001` 仍把六实例、旧 middleware 键和 workflow 物理关闭特例写成现行规则。不能仅改注入代码而保留旧契约，也不能把尚未实现的 v5 写作现状。先把获批目标写入设计，把迁移期兼容与当前规则区分。
2. **workspace 代理尚无入口（阻断）**：`peri-middlewares/src/mcp/builtin/{runtime,dispatch,standalone}.rs` 在宿主内建 handler，`runtime.rs` 明确每实例单独 duplex/task；当前 `origin/main` 未找到 workspace upstream descriptor 的接线。需先定 Sandbox Actor descriptor 的来源、连接权限和生命周期，以及断线时的 session-local 降级，不把上游 HTTP 连接误算成第二个模型可见 server。
3. **关闭不能只做列表过滤（高）**：`peri-acp-types/src/builtin_mcp.rs::BUILTIN_MCP_INSTANCES` 把 `instance`、`policy_key`、direct/deferred 名称绑定在一起；`peri-middlewares/src/mcp/builtin/mod.rs::apply_builtin_overlay` 逐实例注入，`closed_instances` 消费旧键。重构时按同一冻结有效能力集约束 `tools/call`、资源读/订阅、静态提示、ToolSearch、subagent/workflow 继承、后台任务与审批；`workspace` 资源不可得不得回落宿主读盘。外部同名 MCP 不能被 builtin 的关闭规则误杀。
4. **命名与授权交叉（高）**：deferred 旧名 `mcp__<instance>__*` 将改为 `mcp__peri__*`；direct 仍用原名。执行路由必须由经校验的 `(capability, original_name)` 唯一定位，且授权在服务端复核，不能只凭模型面名字反推。旧 transcript 仅展示/匹配归一，不能作为执行别名；未知名、跨能力重名或用户自定义 `peri` 抢占均应确定性失败。
5. **双配置入口仍有实施空位（高）**：`peri-tui/src/cli_mcp_start.rs` 当前只接受单个 instance，且在整机配置加载前分派；`.mcp.json` 的配置权威由 `docs/design/configuration-authority.md` 定义。需要为 standalone 单独解析 `mcpServers.peri` / CLI 参数，并明确 `--capability` 与条目扩展字段的精确语法、优先级、source 与错误类型；不得把整个 Peri settings 装配引入 standalone，也不能让旧键 silent no-op。扩展键名在 #409 尚未定稿，不宜提前编码。
6. **全出口故障域（高）**：能力 handler 错误/panic、workspace upstream 断连和出口 transport 断连必须分别处理；只在前两者隔离/降级，出口断连则整个服务不可用。`readiness`、服务快照/面板、warning 和首个模型请求应反映同一有效能力集；全部能力失败才 fail-closed。不要以隐藏 catalog 代替取消旧订阅或撤销宿主任务。

## 建议的实施顺序（待授权）

1. **设计门槛**：修订 v4 隔离条款和配置权威，写明 v5 目标、现行兼容期与回滚语义；确定配置扩展键、能力依赖闭包、`peri` 名称冲突规则和运行期降级事件。把 §10.2–10.3 的验收矩阵纳入 active spec。
2. **wire 门槛**：先定义并验证 workspace upstream 代理，覆盖请求 ID 并发关联、cancel、progress token、task id/owner、`tasks/get|cancel`、资源读/订阅与通知透传、断线清理和超时。保留 workspace 原资源 URI 和旧任务的隔离，不用仅 `tools/list` 通过代替协议验收。
3. **影子装配**：能力注册表作为单一声明源；单 `peri` handler/router 和一个对外 transport；各能力 handler 状态、凭据、资源根和生命周期仍独立。仅对新会话以 beta flag 选新路径，旧六实例保持可回滚；外部 MCP/ACP 路径不变。
4. **切换和清理**：对比新旧首个模型请求、任务/资源与关闭矩阵后再切默认；旧配置确定性映射或报错，旧历史名只做非执行归一；观察窗口后删旧六实例注入。回滚只对新会话选择旧装配，已冻结会话不得隐式换身份、重写历史或热替换 pool。

## 验收矩阵

| 维度 | 必须观察的行为 |
| --- | --- |
| 六种能力逐一开/关 | enabled 时 `tools/list`、direct/deferred、资源/订阅与调用一致；disabled/degraded 时各面不可发现且无法绕过服务端授权调用；宿主任务与子 Agent/workflow 继承同判。 |
| 配置组合 | CLI > `.mcp.json` > 默认；未知能力、冲突扩展、旧键、旧逐实例配置、外部同名 server 的成功映射或 typed error 可解释来源。 |
| 代理 wire | 并发请求 ID 不串线；取消/进度/任务 ID/owner/通知/订阅透传；上游断线不拖垮其他能力；重连不误接旧任务。 |
| 降级与生命周期 | 单能力初始化失败仍保留其他能力，warning 在会话诊断与面板可见；全部失败 fail-closed；能力 panic 不终止 transport；关闭有界收敛且无遗留任务。 |
| 命名、审批与历史 | 新 deferred `mcp__peri__*`、旧 transcript 归一、direct 原名、审批按能力+wire 原名；旧前缀不能执行；同名冲突加载期拒绝。 |
| 迁移与回滚 | 六实例旧会话行为不变；新会话影子/默认可独立选择；外部 stdio/HTTP/ACP MCP 不受影响；切换前后首个模型请求和冻结能力面可对账。 |

## 本轮未验证

未运行 Sandbox upstream 或端到端 MCP wire 实验；未验证 beta flag 接线与跨进程 owner 透传。上述是基于 issue 和 `origin/main` 的静态审计，不宣称代理已实现或性能乘数已被实测消除。
