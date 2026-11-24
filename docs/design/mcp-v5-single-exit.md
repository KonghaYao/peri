# MCP v5：builtin 单出口目标设计

> 状态：已批准目标设计（2026-11-15，Issue #409）。v5 单出口 `peri` 已是宿主默认装配；
> CLI 单实例 serving 路径已收敛为能力映射 adapter，六个独立 builtin server 仅在
> `PERI_MCP_BUILTIN=legacy` 回滚路径保留到 M4 截止决定。旧 `*Middleware` 策略键
> 仍确定性映射为能力开关（兼容 adapter，M4 删除），不能 silent no-op。
> Sandbox workspace 的宿主内代理接线仍是兼容路径（见 active spec 未验证项）。
> 运行时代码、协议测试与迁移状态以代码、契约测试和
> [active spec](../../spec/issues/2026-11-15-v5-single-exit-acceptance.md) 为准。
>
> 规范性用语：**必须**表示不可违反的契约，**不得**表示禁止行为，**可以**表示实现选择。本文使用三种状态标记：
>
> - **现行**：当前 `origin/main` 的代码行为，由代码与契约测试证明。
> - **兼容期**：v4 六实例路径在迁移期间仍可用，用于未迁移会话与回滚；不是长期目标。
> - **目标（v5）**：已裁决的单出口契约；standalone 与宿主默认切换均已实现；仍待验证的闭环见 active spec，不把未验证部分写成现状。
>
> 术语：配置权威使用 `McpConfigFile`（`.mcp.json` 的 typed 解析结果）与 `ConfigurationSnapshot`（配置权威快照）；本文不使用旧 issue 中的其他配置类型名。

## 1. 目标与取代关系

### 1.1 目标

v5 把 builtin MCP 从「六个独立 server 实例」收敛为「一个对外出口 + 一个 capability router」：

1. 每个会话环境只注入一个 builtin MCP server，名称为 `peri`。
2. `peri` 只有一个对外 transport、一次 initialize/discover、一份聚合 `tools/list` / `resources/list`、一个生命周期与一个关闭边界。
3. `workspace` 由 `peri` 代理：Sandbox Actor 暴露的上游连接不是第二个模型可见 server。
4. 出口内部按 capability 路由工具、资源、订阅、任务与宿主注入；能力之间不互相调用。
5. capability 是唯一开关边界：`workspace`、`web`、`artifact`、`cron`、`monitor`、`workflow` 六项能力各自声明工具、资源、开关、宿主注入、执行绑定与状态根。
6. 关闭或降级能力必须同时关闭其 direct/deferred 工具面、资源与订阅、静态提示、ToolSearch、子 Agent/workflow 继承面、宿主任务与 runtime authorization。

### 1.2 取代的 v4 条款

本设计取代 [MCP 适配清单（v4-part-1）](mcp-adaptation-v4-part-1.md) 中下列**长期目标**条款：

1. 「隔离单位是 builtin MCP 实例」以及由该条款推导出的逐实例 transport、client pool 与独立 server task 要求。
2. 六实例作为 builtin MCP 目标形态的表述；六实例在迁移期仅保留为**兼容期**路径与回滚路径。
3. `ARC-CAPABILITY-CLOSURE-001` 中以六实例、旧 middleware 键和 workflow 物理关闭特例作为唯一长期关闭模型的表述；该契约已同步增加 v5 目标段，现行段继续描述兼容期。

不取代的部分：

1. 外部 stdio/HTTP/ACP MCP 的隔离、生命周期与控制规则不变；单出口只收敛 builtin。
2. `system_mcp` 的启动依赖语义不变；v5 中 `peri` 是唯一 builtin system MCP 条目。
3. [Workflow as MCP](workflow-mcp.md) 的工具、资源、订阅与任务信道语义不变；变化只在暴露出口、模型名与关闭粒度。
4. [ACP MCP 运行控制](acp-mcp-control.md) 的边界不变：builtin `peri` 仍不可经 ACP restart/stop，能力开关不属于 ACP 运行控制。

## 2. 目标架构

### 2.1 单出口

**目标（v5）**：

1. 对外目录中只有一个 builtin server 条目 `peri`；其 `source` 为 builtin 身份，`system_mcp = true`，由宿主注入，不作为用户可覆盖的普通 server。
2. 出口只有一份对外 transport 与一个 server task。不按 capability 分别协商连接；`tools/list`、`resources/list`、`resources/read`、订阅（0728 `subscriptions/listen`，legacy `resources/subscribe` 等）与受支持的 Tasks 扩展方法由 router 聚合。
3. workspace 代理的上游连接只属于 `peri` 内部，不产生第二个模型可见 endpoint，也不改变客户端看到的资源 URI。
4. 对外单 transport 不等于共享可变状态：每个 capability 保留独立 handler 状态、凭据、资源根、任务归属与关闭逻辑。
5. `mcp-packages/*` 仍是能力实现的组织方式；运行时实例划分与 crate 划分解耦。聚合 handler 只做路由、生命周期、代理与错误隔离，不复制业务规则。

**现行（v5 默认）**：宿主默认只注入 `peri` 一个 builtin 条目（`source = Builtin`，`system_mcp = true`），
由 capability router 聚合六项能力；workspace 能力在出口内同进程服务。`PERI_MCP_BUILTIN=legacy`
保留六实例逐实例 transport / task 的回滚路径（M4 截止），`PERI_MCP_BUILTIN=off` 是零注入 kill switch。

### 2.2 能力注册表

**目标（v5）**：`peri-acp-types::builtin_mcp` 从「实例 / 工具」表升级为「能力 / 工具」表；声明表的语义事实源是该表。现行 `BUILTIN_MCP_INSTANCES` 的类型名与字段名可在实现期调整，但以下语义必须保留：

| 声明项 | 内容 |
| --- | --- |
| 工具 | 原始名、模型面名、`direct`、`model_visible`、prompt 声明、context retention |
| 资源 | 资源 URI、内容形态、默认订阅与通知归属 |
| 开关 | 能力级开关来源与旧键映射；不提供工具级开关 |
| 宿主注入 | `_meta["peri/taskOwner"]`、后台任务归属、恢复查询 |
| 执行绑定 | Harness 同进程 / Sandbox MCP descriptor / 远端；绑定是实现字段，不改变能力身份 |
| 隔离 | 凭据、状态根、资源根与任务归属为 capability 私有 |

能力注册表是关闭矩阵、路由、审批、readiness 与诊断的单一声明源。能力之间的依赖必须在注册表中显式声明；依赖能力被关闭时，dependent 能力必须同步关闭或安全失败，不得由实现按名称推断。

### 2.3 路由与任务归属

**目标（v5）**：

1. 工具按 `(capability, original_name)` 唯一定位；模型面名字只在入口做一次解析，执行与授权使用经校验的二元组。
2. 单出口内工具原始名必须全局唯一；重复是加载期错误，不沿用跨 server 的 first-wins 冲突策略。
3. 资源按 URI 归属到 capability；订阅与通知由 router 统一管理。
4. 统一 task registry 保存 capability 归属；task id 必须能在出口内唯一判定，采用全局唯一或稳定能力前缀，具体编码在实现期裁决。
5. `tasks/get|cancel` 与通知中的 `taskIds` 只按该 registry 定位；未知 task id 返回确定性错误，不猜测能力。
6. 能力错误不得让 router 丢失其他能力的在途请求；取消、进度与超时按原请求归属透传。

### 2.4 workspace 代理

**目标（v5）**：`workspace` 在 Sandbox Actor 暴露 HTTP MCP descriptor；`peri` 作为代理连接该 upstream。代理契约：

1. JSON-RPC 请求/响应映射：request id、`tools/call`、`resources/*`、`subscriptions/listen`，以及 workspace 现有扩展方法 `skills/list`、`skills/get`、`image/read`。
2. 取消与任务：`notifications/cancelled`、`tasks/get|cancel`、progress token、通知中的 `taskIds`。
3. 资源 URI 保持现状（`workspace://` 等），不因合并改名；旧任务的归属与隔离不得因代理而丢失。
4. 出口管理上游连接，不接管 Sandbox 生命周期；Sandbox 启停仍归 Sandbox actor。
5. workspace 上游不可用时只降级 workspace；不得回落宿主读盘，也不得把上游 HTTP 连接计为第二个模型可见 server。
6. 上游断线后不得把旧 task id 误接到新连接；重连清理与超时语义必须在 wire 层验证。
7. 代理层的 wire-level 测试是实现的首个门槛；只用 `tools/list` 通过不能代替协议验收。

**未裁决**：descriptor 的来源、连接权限与生命周期，以及代理的并发关联、重连与超时细节，在 wire 门槛实现前裁决并记录到 active spec。

#### 2.4.1 lazy 远端 workspace（宿主显式 opt-in）

cold 代理宿主（Bun/Swarm）在真实后端启动前只宣告工具面（`initialize` /
`tools/list`），资源面要真实后端才有。该形态由宿主**显式声明** `lazy_workspace`
（`HostAssemblyInput` / `AcpServerConfig`，默认 `false`；会话环境把它原样传给
会话 MCP 池的 `set_lazy_workspace`），唯一 opt-in 方是 `peri-swarm` 的 `AcpChild`；
TUI / print / stdio 与普通 ACP 客户端一律 `false`。

约束：

- 默认（`lazy_workspace = false`）：远端 workspace 保持既有语义——启动期探测
  `resources/list`，`WorkspaceRemote` 的发现失败拒绝初连
  （`fail_workspace_resource_discovery`）。
- `lazy_workspace = true` 且 workspace 来源是 `WorkspaceRemote` 时，启动发现只冻结
  transport / initialize / 能力协商 / `tools/list`：`resources/list`、指令/覆盖面
  读取、技能模板探测与宿主侧 `shell_task_list` / `workspace/gitBranch` 都延后到
  首个真实 `tools/call`。首个调用置位 `workspace_remote_started`，随后只做一次
  启动后 `resources/list` 重发现；失败只降级告警，不改变连接。
- 延迟只对 lazy 远端 workspace 生效；builtin `workspace`、普通 MCP 与非 lazy 的
  `WorkspaceRemote` 仍在启动期探测。

### 2.5 故障域与关闭

**目标（v5）**：

| 故障 | 处理 |
| --- | --- |
| capability handler 返回错误 | 只影响该次调用；router 保持 server 与其他能力可用 |
| capability panic | 在 router 边界收敛为内部错误（`catch_unwind` 或等价隔离点），不得拖垮 transport |
| workspace 上游断连 | 只降级 workspace；清理其订阅与在途任务；其他能力继续；不回落宿主读盘 |
| 出口 transport 断连 | 唯一全能力故障域：所有能力不可用，按会话关闭/重连路径有界清理，不得宣称能力级隔离 |
| 能力降级 | 不留下工具、资源、订阅、静态提示、继承面或宿主任务；warning 在诊断与面板可见 |

关闭 `peri` 时按 capability 有界关闭其宿主任务（cron tick、monitor 轮询、shell 任务等），沿用现有 bounded converge 语义；不得遗留 detached 执行。

### 2.6 Dynamic MCP 按需加载（外部 server）

**现行**：`dynamic: true` 的外部 server 只登记受控配置，首次显式 Loader 调用前不启动进程、不联网、不建立连接。每个配置投影为一条 deferred 宿主工具 `LoadMCP__<规范键>_<身份摘要>`；模型经 `ToolSearch` 发现、`ToolInvoke` 执行，工具名不含地址、headers、env 或凭证，调用不接受任何参数。

用户入口：插件面板把已安装插件加入下一次任务的待提交上下文，composer 只显示插件 ID 标签；提交时选择与文本、附件同批经 `AgentText` / 输入队列随 `input_id` 冻结。客户端只提交插件 ID，不提交指引、URL、凭证或 Loader 绑定；Host 在 Reason 准备阶段按当前会话的插件元数据与 pool 投影生成 model-only 指引并给出 Loader 名称，slash 命令复用同一过程。选择只表达使用意图，不触发连接、不修改权限；选择未安装或没有可见 Loader 的插件不产出指引。

1. 加载走与 initialize/reconnect 相同的连接、发现与提交路径：只有 transport、initialize 与真实成功的 live `tools/list` 全部完成，才允许提交 `Connected`。失败、需要 OAuth 或会话目录无法验证时报告错误，不伪造 ready，不重放业务操作。
2. 每个 server slot 最多一个在建 attempt；并发调用共享同一结果。单个等待者退出不影响其他等待者；全部退出或显式 stop/unload/会话关闭时取消 attempt。
3. stop/unload 先取消 attempt、撤销调用门并撤下目录，再排空与关闭；`stopped`、配置版本或取消状态不匹配的迟到连接一律拒绝提交，不能复活已停止或已卸载的 server。
4. 失败 attempt 在收口时关闭未提交的 service/process；重复 load/close 不累积连接、进程、任务或保留 owner。Loader 成功不产生额外授权：新工具在下一轮 Reason 进入目录后仍走既有审批与 runtime authorization。
5. 子 Agent、fork 与 workflow 沿用各自冻结配置与目录过滤；继承配置不触发首次加载，已连接能力能否复用仍由既有会话准入决定。

**验证入口**：`cargo test -p peri-middlewares --lib -- mcp::control::load_attempt_tests`、`mcp::control::dynamic_cleanup_tests` 与 `mcp::loader::tests` 覆盖并发单次加载、失败共享、停止/卸载竞争、保留计数、授权未就绪与下一轮目录；`cargo test -p peri-agent --lib -- session::exec::executor::plugin_guidance` 覆盖选择合并、指引文本与投递。用户闭环由 `peri-e2e` 的 P01 场景覆盖：选择插件后提交任务，模型只经 `ToolSearch` / `ToolInvoke` 调用 Loader，下一轮搜索到业务工具 schema 并执行真实本地 HTTP MCP，同会话复用连接，关闭后 socket 归零；录制基线为 Ubuntu 24.04，随 `.github/peri-e2e.rev` 在 CI 回放。

## 3. 配置权威

**现行**：普通会话的 MCP 配置由 `ConfigurationSnapshot` 投影，`McpConfigFile` 解析 `.mcp.json` 的 `mcpServers`；builtin 默认层由宿主注入一个 `peri` 条目，能力集合来自 `.mcp.json` 旧逐实例条目的确定性映射（`disabled` → 能力关闭）与会话冻结关闭集。`PERI_MCP_BUILTIN` 由 `mcp::builtin_enabled`/overlay 单一解释：缺省 v5、`off` 零注入、`legacy` 回滚六实例。`peri mcp-start peri --capability ...` 在配置加载前分派，standalone 不读取完整 settings。

**目标（v5）**：单出口有两个一等配置入口，不把 `ConfigurationSnapshot` 作为 standalone 的唯一权威：

1. **`.mcp.json`**：使用顶层 `mcpServers`，单出口是其中一个 server 条目；`command` / `args` / `env` / `url` / `headers` 保持 MCP 标准语义，Peri 既有 `disabled` 等字段语义不变。
2. **`peri mcp-start` 参数**：控制本次 standalone 服务启用的能力、transport 与 workspace；它不读取完整 Peri 配置装配，也不是整个 `peri` 进程的全局开关。

宿主内建条目与 standalone 的 `.mcp.json` 条目使用同一 server 契约，但身份不同：宿主条目由宿主注入且不可被同名用户条目覆盖；standalone 条目由通用 MCP 客户端启动 `peri mcp-start`，不构成对宿主 builtin 的接管。

约束：

1. 优先级为命令行 > `.mcp.json` > 内置默认；冲突必须能解释来源。
2. 不新造顶层 `capabilities` 对象；MCP 的 `capabilities` 属于 wire 协议协商，复用会混淆配置与协议协商。
3. Standalone 优先读取 `mcp-start --capability`；未提供时可从工作区 `.mcp.json` 的 `mcpServers.peri.x-peri.capabilities` 读取非空字符串数组。`all` / `none` 各自互斥，不能与能力名共用；`x-peri` 是 Peri 的暂用配置扩展。
4. 未知能力名、非法组合、能力子配置冲突是加载期 typed error，不 warning 后忽略。
5. 旧 middleware 策略键（`WebMiddleware` 等）不再是能力开关身份，但保留为确定性映射 adapter：出现时映射到对应能力（`false` 关闭），M4 清理时删除；不得 silent no-op。v4 逐实例 `.mcp.json` 条目同样由配置 adapter 确定性映射为 capability 开关。
6. `PERI_MCP_BUILTIN` 是部署级开关：缺省 / `on` / `v5` 走单出口，`legacy` / `v4` 走六实例回滚（M4 截止），`off` / `0` 零注入；不承担逐能力配置。
7. v4 的逐实例 `.mcp.json` 条目在迁移期由 adapter 确定性映射为单出口能力开关；新会话不得写回六条目。
8. 宿主内的 builtin 单出口由宿主注入，能力开关来自会话配置与既有映射，不受 `mcp-start` 影响；standalone 与宿主共用能力注册表，但不读取完整 settings。

状态说明：`mcp-start` 的 `--capability` 参数语法已实现；standalone 已读取暂用扩展 `mcpServers.peri.x-peri.capabilities`。宿主默认装配已切换为单出口 `peri`，`PERI_MCP_BUILTIN=legacy` 保留六实例回滚（M4 截止）；旧 `*Middleware` 策略键保留为确定性映射 adapter。验收矩阵见 active spec 的配置组合维度。

## 4. 能力关闭矩阵

配置关闭使用**冻结配置能力集**（会话装配时固定，冷恢复复用）；运行期降级在其上派生**运行时能力投影**（移除失败能力，仅在显式重连、新会话或冷恢复时恢复）。目录、ToolSearch、资源与订阅、继承面、宿主任务、readiness、诊断与 runtime authorization 都必须使用运行时能力投影；关闭能力的配置事实仍以冻结集为准。两者都必须满足下表：

| 能力面 | 关闭（配置/部署） | 降级（运行期失败） |
| --- | --- | --- |
| direct tools | 不进入模型 tools；不注入 `system_mcp_tools` | 同关闭 |
| deferred 索引与 resolver | 不进入 `tools/list`、ToolSearch 索引与执行 resolver | 同关闭 |
| 资源与订阅 | `resources/list` 不出现；读取返回 not-found/unavailable；不建立订阅 | 同关闭；清理既有订阅，不继续推送旧通知 |
| 静态提示与示例 | 不注入该能力的 prompt 声明与示例 | 同关闭 |
| ToolSearch | 索引、描述、代理执行都不含该能力 | 同关闭 |
| 子 Agent/workflow 继承 | `parent_tools`、workflow agent 工具列表不含该能力；不得从完整 pool 重新引入 | 同关闭 |
| 宿主任务 | 不启动；已有任务有界取消/收敛 | 不留下新的宿主任务；既有任务按关闭语义收敛 |
| runtime authorization | 按运行时能力投影拒绝该能力的工具调用与资源读取，不能只靠隐藏目录 | 同关闭；按同一投影拒绝 |
| readiness/诊断/首个请求 | 同一运行时能力投影；面板、warning、首个模型请求一致 | 同一运行时能力投影；warning 可见 |

上表的 direct/deferred、资源与订阅、静态提示、ToolSearch、继承面与宿主任务均按运行时能力投影计算；冻结配置集只决定配置关闭、授权上限与冷恢复基线。

附加约束：

1. 只隐藏 catalog/UI、只移除 middleware 实例或只过滤 `tools/list` 都不算关闭；必须做到「不可发现且不可调用」。
2. 关闭 `workspace` 能力时，其资源面一并关闭：技能、Agent 定义（`agent://`）、项目指令（`peri-instruction://`）与 MetaHarness 段落覆盖（`peri-meta://`）不可发现、不可读取；宿主不得回落磁盘读取本地技能或 `.peri/meta`。段落覆盖按可选降级：warn 并保持内置段落，不阻塞会话创建。
3. 关闭 builtin 能力不得误杀外部同名 MCP；关闭规则只按 builtin 来源身份生效。
4. 关闭一个能力不得占用合法 core/外部工具的名称；过滤应先于候选重名合并。
5. 降级能力不得留下「列表隐藏但可调用」的中间态；服务端授权与目录投影使用同一运行时能力投影。
6. 全部能力失败才 fail-closed；部分能力失败允许能力级降级。配置错误仍是加载期 hard fail，不退化为运行期 warning。

## 5. 命名、审批与授权

**目标（v5）**：

1. deferred 工具的模型面名为 `mcp__peri__<tool>`；不再保留 `mcp__<capability>__<tool>` 作为长期名。
2. direct 工具维持原名注入（如 `WebSearch`、`WebFetch`、`artifact`、`Read`），走 `system_mcp_tools` 的 wire 原名，不属于 `mcp__` 命名空间。若后续要求 direct 前缀化，单列迁移波次，不扩大本阶段范围。
3. 旧 transcript、用户 allowlist、prompt 文案与测试字面量只做展示/匹配归一；映射失败必须报错。旧前缀不是执行别名，不能用于调用。
4. `original_tool_name_of_effective` 的历史归一继续保留，但只用于旧记录与显示/过滤匹配，不用于执行路由。
5. 执行路由与审批按 `(capability, original_name)` 绑定，不按 server 名或模型面名字反推；服务端必须复核授权，不能信任模型声明的身份。
6. 观测身份保留 capability：事件、日志、工具回执与 Standard Observation 字段都必须能区分能力来源。
7. 单出口内原始名重复、跨能力重名、无法唯一判定归属，以及用户自定义配置抢占保留名 `peri`，均为加载期确定性失败；不得静默覆盖或 first-wins。
8. 外部 MCP 继续使用 `mcp__<server>__<tool>`；外部同名 server 不得被 builtin 关闭规则误杀，也不得借 `peri` 名称继承 builtin 审批白名单。

**现行（v5 默认）**：deferred 工具经工具桥生成 `mcp__peri__<original>`，direct 工具保持原名；`peri` 的 wire `tools/list` 使用原始名，模型面命名只由工具桥生成。**兼容期**：`PERI_MCP_BUILTIN=legacy` 回滚路径与既有 v4 会话继续执行 `mcp__<instance>__<tool>` 名。**v5 路由**不执行 builtin 的旧实例名，旧名仅用于历史显示与过滤匹配；外部 server 的同形名称不受影响。M4 后 v4 路径与旧名执行一并退役。

## 6. 降级、readiness 与可观察性

**目标（v5）**：

1. 运行期能力初始化或连接失败只降级该能力并发出 warning；其他能力继续服务。配置错误仍在加载期失败。
2. `readiness` 与 `system_mcp_tools` 按运行时能力投影判定；部分能力可用即保持 server ready，全部失败才 fail-closed。
3. 首个模型请求的工具面、MCP 服务快照/面板、warning 与会话诊断必须反映同一运行时能力投影；不得以隐藏 catalog 代替取消订阅或撤销宿主任务。
4. warning 不能只写 tracing；必须出现在会话诊断与 MCP 面板/服务快照中，供用户判断能力缺失。
5. 首阶段不做会话内自动重试或热恢复；降级能力只通过显式重连、新会话或冷恢复重新建立运行时能力投影。冷恢复不持久化降级状态，也不恢复旧执行状态。
6. 观测字段至少能回答：哪个 capability、哪个失败阶段（初始化/连接/调用）、当前是 ready/degraded/failed，以及是否仍有宿主任务或订阅。

## 7. 迁移、兼容期与回滚

### 7.1 兼容期与回滚语义

迁移阶段、批次、观察窗口与验收勾选只写 [active spec](../../spec/issues/2026-11-15-v5-single-exit-acceptance.md)，不回填设计。设计冻结以下语义：

1. 迁移期 v4 六实例路径必须保留，可由部署级 `PERI_MCP_BUILTIN=legacy` 为新会话选择；它是兼容期与回滚路径，不是目标设计。
2. 默认装配已切换为 v5 单出口；v4 路径由 `PERI_MCP_BUILTIN=legacy` 显式选择（部署级回滚），M4 截止。
3. 切换后旧配置必须可确定性映射或 typed error；旧 transcript 名只做非执行归一。
4. 清理期（M4）只能在观察窗口与验收完成后开始；M4 启动即回滚支持结束，之后不得再承诺 v4 装配或旧名执行。清理前必须确认无仍需冷恢复的 v4 会话；否则保留只读 v4 adapter，不得删除。清理不得改变已冻结会话，也不得删除外部 MCP/ACP 路径。

### 7.2 回滚开关、冻结配置集与运行时投影

1. 默认（`PERI_MCP_BUILTIN` 未设置 / `on` / `v5`）走单出口；`legacy` / `v4` 走 v4 六实例兼容路径；`off` / `0` 是零注入 kill switch。开关是进程级部署选择，只影响**新会话装配**，不做会话内热生效；同一进程内不能为新会话逐个选择，但冷恢复按该会话冻结的模式装配。
2. v4 路径持续到回滚支持截止（M4 清理前），M4 后与旧名执行一并退役。
3. 新会话冻结其**配置能力集**，并记录来源 revision 供诊断；同一会话及其子 Agent 复用冻结集合，不在 turn 中途重新解析。运行期失败由该集合派生运行时能力投影。
4. 冷加载/恢复复用已冻结的配置能力集，并按启动结果重新建立运行时能力投影。冷恢复是允许的恢复事件：不持久化降级状态、不恢复旧执行状态、不改变会话身份或历史；不得因当前配置变化隐式换身份、重写历史或热替换 pool。
5. 已存在的 v4 会话保持原装配与原模型名，不回写；回滚只改变新会话的装配选择（进程级），不改变已冻结会话。

冻结装配模式随 `FrozenContext` 写入版本化 `FrozenSnapshotV4`；旧 V1–V3 快照按其创建时的六实例装配解释。恢复时先把模式绑定到会话 MCP pool，再加载当前配置并应用对应 overlay；运行中的 pool 不受部署开关变化影响。此字段复用现有 frozen blob，不新增数据库列。

### 7.3 旧配置 adapter 与旧名称

1. v4 逐实例 `.mcp.json` 条目必须确定性映射为单出口能力开关；映射规则与来源必须可解释。
2. 旧 middleware 策略键必须确定性映射或在加载期报错；不得 silent no-op，也不得出现「键仍存在但不再生效」的中间态。
3. 旧 transcript 名称只做展示/匹配归一；归一失败报错，不做模糊匹配。旧名不是执行别名。
4. 旧实例的 `source = Builtin` 身份与保留名规则继续用于识别旧配置；用户自定义同名 server 不得冒充 builtin。
5. 新会话不写回六个实例条目；旧配置文件在迁移完成前保持可读。

### 7.4 观察窗口与回滚

1. 切换默认前，必须在观察窗口内对比新旧路径：首个模型请求的工具面、readiness、面板/诊断、资源与订阅、宿主任务与关闭矩阵。窗口时长、样本量与退出条件在 active spec 中记录。
2. 观察期内保留 v4 adapter；出现阻断缺陷时，只对新会话切回旧装配，不热切换已冻结会话。
3. 回滚不得重写旧 transcript、不得重新发布已关闭实例、不得把旧名变成执行别名。
4. 观察窗口结束后才进入清理期（M4）；M4 开始即回滚支持截止。清理前必须证明旧配置可映射或可报错、旧历史可读、无仍需冷恢复的 v4 会话，外部 MCP/ACP 不受影响；否则保留只读 v4 adapter，不得删除。

## 8. 验收与未决项

### 8.1 验收口径

验收矩阵分六维：六种能力逐一开/关、配置组合、代理 wire、降级与生命周期、命名/审批/历史、迁移与回滚。矩阵、逐能力 presence/absence 明细与未验证项记录在 [active spec](../../spec/issues/2026-11-15-v5-single-exit-acceptance.md)。本设计只冻结契约，不宣称任何运行项已通过。

验收不得以 `tools/list` 通过、代码存在或单个单测替代以下证据：

1. 每个能力的启用/关闭/降级在首个模型请求、ToolSearch、资源、订阅、继承面、宿主任务与授权面上的 presence/absence。
2. workspace 代理的并发 request id、取消、进度、任务、通知、断线隔离，以及 `skills/list` / `skills/get` / `image/read` 扩展方法。
3. 配置优先级、旧键映射/报错与外部同名 server 的不误杀。
4. 迁移期新旧会话隔离、冻结配置能力集、运行时能力投影与回滚。

### 8.2 未决项

| 项 | 状态 | 说明 |
| --- | --- | --- |
| Standalone 能力扩展最终键名 | 未裁决 | 当前实现读取 `mcpServers.peri.x-peri.capabilities`；是否长期采用 `x-peri` 待生态复核 |
| 能力/工具声明表的类型名与字段名 | 实现期裁决 | 语义已冻结；可保留 `instance` 字段名以减少机械改动，但语义改为 capability |
| 聚合 handler 的 crate 落点 | 已裁决（实现期） | `peri-middlewares::mcp::builtin::router`：复用 `BuiltinServerHandler`、`StandaloneFactory` 与 `UpstreamProxy`，不新增 crate |
| workspace upstream descriptor 来源、授权与生命周期 | 未裁决 | 由 Sandbox Actor 提供；代理 wire 门槛必须验证 |
| task id 编码 | 未裁决 | 全局唯一或稳定能力前缀；必须能唯一判定归属 |
| 回滚开关 | 已裁决（实现期） | `PERI_MCP_BUILTIN=legacy`（M4 截止）；不新增 beta flag |
| 观察窗口参数与退出条件 | 实现期裁决 | 记录在 active spec；不在设计写死数字 |
| 代理重连/超时/重试策略 | 未裁决 | 首阶段不做自动热恢复；wire 门槛验证断线清理与旧任务隔离 |

运行验收的未验证项、证据要求与关闭条件只记录在 [active spec](../../spec/issues/2026-11-15-v5-single-exit-acceptance.md)，不回填设计。
