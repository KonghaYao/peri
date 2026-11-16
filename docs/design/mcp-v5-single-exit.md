# MCP v5：builtin 单出口目标设计

> 状态：已批准目标设计（2026-11-15，Issue #409）；尚未实现。运行时代码、协议测试与迁移状态以代码、契约测试和 [active spec](../../spec/issues/2026-11-15-v5-single-exit-acceptance.md) 为准。
>
> 规范性用语：**必须**表示不可违反的契约，**不得**表示禁止行为，**可以**表示实现选择。本文使用三种状态标记：
>
> - **现行**：当前 `origin/main` 的代码行为，由代码与契约测试证明。
> - **兼容期**：v4 六实例路径在迁移期间仍可用，用于未迁移会话与回滚；不是长期目标。
> - **目标（v5）**：已裁决但尚未实现的单出口契约。不得把目标写成现状。
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
2. 出口只有一份对外 transport 与一个 server task。initialize/discover 只发生一次；`tools/list`、`resources/list`、`resources/read`、`resources/subscribe`、`tasks/*` 由 router 聚合。
3. workspace 代理的上游连接只属于 `peri` 内部，不产生第二个模型可见 endpoint，也不改变客户端看到的资源 URI。
4. 对外单 transport 不等于共享可变状态：每个 capability 保留独立 handler 状态、凭据、资源根、任务归属与关闭逻辑。
5. `mcp-packages/*` 仍是能力实现的组织方式；运行时实例划分与 crate 划分解耦。聚合 handler 只做路由、生命周期、代理与错误隔离，不复制业务规则。

**现行**：六实例各自一条 duplex 与独立 task；默认层把它们写成六个 `mcpServers` 条目；`workspace` 的 7 个工具用原名直连，其余实例使用 `mcp__<instance>__<tool>`。**兼容期**：该路径保留，供未迁移会话与回滚使用。

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

## 3. 配置权威

**现行**：普通会话的 MCP 配置由 `ConfigurationSnapshot` 投影，`McpConfigFile` 解析 `.mcp.json` 的 `mcpServers`；builtin 默认层由宿主注入，`PERI_MCP_BUILTIN` 由 `mcp::builtin_enabled` 单一解释。`peri mcp-start` 在配置加载前分派，当前只接受单个 instance，standalone 不读取完整 settings。

**目标（v5）**：单出口有两个一等配置入口，不把 `ConfigurationSnapshot` 作为 standalone 的唯一权威：

1. **`.mcp.json`**：使用顶层 `mcpServers`，单出口是其中一个 server 条目；`command` / `args` / `env` / `url` / `headers` 保持 MCP 标准语义，Peri 既有 `disabled` 等字段语义不变。
2. **`peri mcp-start` 参数**：控制本次 standalone 服务启用的能力、transport 与 workspace；它不读取完整 Peri 配置装配，也不是整个 `peri` 进程的全局开关。

宿主内建条目与 standalone 的 `.mcp.json` 条目使用同一 server 契约，但身份不同：宿主条目由宿主注入且不可被同名用户条目覆盖；standalone 条目由通用 MCP 客户端启动 `peri mcp-start`，不构成对宿主 builtin 的接管。

约束：

1. 优先级为命令行 > `.mcp.json` > 内置默认；冲突必须能解释来源。
2. 不新造顶层 `capabilities` 对象；MCP 的 `capabilities` 是 `initialize` 协议字段，复用会混淆配置与协议协商。
3. 能力选择优先放在 `mcp-start` 参数。若必须持久化到 `.mcp.json`，只能放在该 server 条目的命名空间扩展下，且标准客户端忽略它不影响连接。扩展键名在实现前按 MCP 生态复核后裁决，本文不预占。
4. 未知能力名、非法组合、能力子配置冲突是加载期 typed error，不 warning 后忽略。
5. 旧 middleware 策略键（`WebMiddleware` 等）不再作为能力开关；出现时必须确定性映射到 capability 开关或显式 typed error，不得 silent no-op。
6. `PERI_MCP_BUILTIN` 只保留为部署级 kill switch（全部 builtin 关闭），不承担逐能力配置。
7. v4 的逐实例 `.mcp.json` 条目在迁移期由 adapter 确定性映射为单出口能力开关；新会话不得写回六条目。
8. 宿主内的 builtin 单出口条目由宿主注入，能力开关以 `mcp-start` 参数为准；standalone 与宿主共用能力注册表，但不重新绑定整个 settings 装配。

状态说明：以上是目标契约，当前 `mcp-start` 的参数语法与扩展键尚未实现或裁决；验收矩阵见 active spec 的配置组合维度。

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

**现行**：deferred 名按实例分裂，workspace direct 工具用原名；旧名归一函数存在。**兼容期**：v4 路径（beta flag 关闭的新会话与既有 v4 会话）继续执行其 `mcp__<instance>__<tool>` 名。**v5 路由**不执行 builtin 的旧实例名，旧名仅用于历史显示与过滤匹配；外部 server 的同形名称不受影响。迁移完成（M4）后 v4 路径与旧名执行一并退役。

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

1. 迁移期 v4 六实例路径必须保留，可被新会话选择；它是兼容期与回滚路径，不是目标设计。
2. 新路径先以 beta flag 只对新会话开启；切换默认前不得删除 v4 adapter。
3. 切换后旧配置必须可确定性映射或 typed error；旧 transcript 名只做非执行归一。
4. 清理期（M4）只能在观察窗口与验收完成后开始；M4 启动即回滚支持结束，之后不得再承诺 v4 装配或旧名执行。清理前必须确认无仍需冷恢复的 v4 会话；否则保留只读 v4 adapter，不得删除。清理不得改变已冻结会话，也不得删除外部 MCP/ACP 路径。

### 7.2 beta flag、冻结配置集与运行时投影

1. 新装配由专用 beta flag 选择；flag id 在实现期按 [Beta Flag 系统](beta-flags.md) 注册，本文不预占。flag 只影响**新会话**，不做会话内热生效。
2. flag 关闭时新会话仍走 v4 六实例兼容路径；这是迁移期回滚路径，不是目标设计。该路径持续到回滚支持截止（M4 清理前），M4 后 flag 与 v4 路径一并退役。
3. flag 开启时新会话冻结其**配置能力集**，并记录来源 revision 供诊断；同一会话及其子 Agent 复用冻结集合，不在 turn 中途重新解析。运行期失败由该集合派生运行时能力投影。
4. 冷加载/恢复复用已冻结的配置能力集，并按启动结果重新建立运行时能力投影。冷恢复是允许的恢复事件：不持久化降级状态、不恢复旧执行状态、不改变会话身份或历史；不得因当前配置变化隐式换身份、重写历史或热替换 pool。
5. 已存在的 v4 会话保持原装配与原模型名，不回写；回滚只改变新会话的选择，不改变已冻结会话。

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
| `.mcp.json` server 条目的能力扩展键名 | 未裁决 | 只允许 server 条目命名空间扩展；实现前按 MCP 生态复核，本文不预占 |
| 能力/工具声明表的类型名与字段名 | 实现期裁决 | 语义已冻结；可保留 `instance` 字段名以减少机械改动，但语义改为 capability |
| 聚合 handler 的 crate 落点 | 未裁决 | `peri-mcp-common` 或新建 `peri-mcp-builtin`，实现前裁决 |
| workspace upstream descriptor 来源、授权与生命周期 | 未裁决 | 由 Sandbox Actor 提供；代理 wire 门槛必须验证 |
| task id 编码 | 未裁决 | 全局唯一或稳定能力前缀；必须能唯一判定归属 |
| beta flag id 与注册表条目 | 实现期裁决 | 按 beta-flags.md 引入，不预占 id |
| 观察窗口参数与退出条件 | 实现期裁决 | 记录在 active spec；不在设计写死数字 |
| 代理重连/超时/重试策略 | 未裁决 | 首阶段不做自动热恢复；wire 门槛验证断线清理与旧任务隔离 |

运行验收的未验证项、证据要求与关闭条件只记录在 [active spec](../../spec/issues/2026-11-15-v5-single-exit-acceptance.md)，不回填设计。
