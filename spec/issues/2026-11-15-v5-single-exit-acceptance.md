# v5 builtin 单出口：设计门槛与验收（#409）

> 状态：v5 单出口已是宿主默认装配并有宿主级 in-process wire 回归（`mcp::builtin_runtime_tests::wave3::v5_*`）；旧 policy 键与 CLI 单实例 serving 路径已退役，`PERI_MCP_BUILTIN=legacy` 与旧 `.mcp.json` 条目映射作为回滚 adapter 保留到 M4 截止决定。Sandbox workspace 代理端到端、真实首个模型请求、E2E replay 与跨进程 owner 验收仍未执行。
>
> 基线：`origin/main` `fa297dd7b`（#428 v5 async 默认已并入）之上的收尾分支；裁决 [#409](https://github.com/KonghaYao/peri/issues/409)；静态审计见 [2026-11-15-v5-builtin-single-exit-audit.md](2026-11-15-v5-builtin-single-exit-audit.md)。
>
> 设计事实源：[MCP v5 单出口目标设计](../../docs/design/mcp-v5-single-exit.md)；配置规则见 [配置权威面](../../docs/design/configuration-authority.md)；关闭规则见 `ARC-CAPABILITY-CLOSURE-001`；命名与工具规则见 `ARC-TOOLS-001`。本 spec 只记录实施范围、验收矩阵与未验证项，不复制设计正文。

## 1. 范围与门禁

1. P0 设计门槛：修订设计契约，写明 v5 单出口目标、兼容期与回滚，并把验收矩阵写入 active spec。
2. P0 阶段不改运行时代码；P2/P3 实现 standalone router 与 `mcp-start --capability`；P4/P5 把单出口切换为宿主默认装配、补齐关闭矩阵与 v4 回滚 adapter，不改数据库表结构或既有会话配置。
3. #409 §10.1（用户裁决）已完成；§10.2–§10.4 的落点与状态见 §2。
4. router、能力开关/降级与代理接缝可按 crate 级测试描述为已实现；宿主默认装配已切换并有 in-process wire 回归。在 §3 的真实 Sandbox / 首个模型请求 / E2E 验收完成前，不得把对应维度写成验收通过。

## 2. #409 批准条件与设计门槛

| #409 条 | 要求 | 落点 | 状态 |
| --- | --- | --- | --- |
| §10.2 | 明确取代 v4「最小隔离单位 = builtin 实例」条款，并记录到 `docs/design/` | [mcp-v5-single-exit.md §1.2](../../docs/design/mcp-v5-single-exit.md#12-取代的-v4-条款)、`mcp-adaptation-v4-part-1.md` 文首取代关系 | 设计门槛完成 |
| §10.3 | 给出能力开关 presence/absence 验收矩阵与迁移回滚路径 | 本 spec §3、§4、§5；设计 §4、§7 | 设计门槛完成；运行未验证 |
| §10.4 | 补齐 workspace 代理契约与 wire-level 测试清单 | 设计 §2.4；本 spec V5-W | 契约完成；wire 测试未执行 |

设计门槛的文档同步项：

1. v4 目标设计中「builtin 实例最小隔离单位」的长期目标已标记为被 v5 取代；v4 六实例保留为兼容期路径。
2. `ARC-CAPABILITY-CLOSURE-001` 已增加 v5 目标段；现行六实例规则仍描述兼容期行为，不把目标写成现状。
3. `ARC-TOOLS-001` 已增加 v5 命名与 `(capability, original_name)` 授权目标；外部 MCP 的 `mcp__<server>__<tool>` 规则不变。
4. `configuration-authority.md` 已增加 v5 双入口、优先级、扩展键占位与旧键退役规则；术语使用 `McpConfigFile` / `ConfigurationSnapshot`。
5. `acp-mcp-control.md`、`middleware-system.md`、`workflow-mcp.md` 已注明 v5 关系与不变边界。

## 3. 验收矩阵

六个维度来自静态审计；每个维度必须同时覆盖「设计契约已写明」与「运行行为已验证」两件事。当前只有前者可勾选。

| ID | 维度 | 必须观察的行为 | 证据要求 | 状态 |
| --- | --- | --- | --- | --- |
| V5-C | 六种能力逐一开/关 | `workspace` / `web` / `artifact` / `cron` / `monitor` / `workflow` 分别在 enabled、disabled、degraded 三态下检查：`tools/list`、direct/deferred、资源/订阅、静态提示、ToolSearch、子 Agent/workflow 继承、宿主任务、runtime authorization 一致；disabled/degraded 时各面不可发现且无法绕过服务端授权调用。 | 逐能力 presence/absence 测试或真实 wire 记录；首个模型请求工具面；面板/诊断快照。 | 部分：`v5_closure_matrix_covers_every_capability` 覆盖六能力关闭后的 wire 工具、required 工具、typed bridge 与运行时拒绝；宿主任务面与真实首个模型请求未执行 |
| V5-G | 配置组合 | CLI > `.mcp.json` > 默认；未知能力、冲突扩展、旧 middleware 键、旧逐实例配置、外部同名 server 的成功映射或 typed error 可解释来源；旧键不得 silent no-op。 | 配置解析单测 + 首请求能力面；错误包含来源与键名。 | 部分：`peri-config` 解析、六条目映射与旧 policy 键退役告警测试通过；真实首请求能力面未执行 |
| V5-W | 代理 wire | 并发 request id 不串线；取消、进度 token、任务 id/owner、通知、资源订阅与读取、`skills/list`、`skills/get`、`image/read` 透传；上游断线不拖垮其他能力；重连不误接旧任务；资源 URI 保持原名。 | wire-level 测试，不得只用 `tools/list` 通过代替；覆盖并发、取消、断线、重连与三个 workspace 扩展方法。 | 部分：`UpstreamProxy` mock-peer wire 与 router 代理测试通过；Sandbox upstream 端到端与宿主代理接线未验证 |
| V5-L | 降级与生命周期 | 单能力初始化失败仍保留其他能力；readiness、授权、目录与诊断使用同一运行时能力投影；warning 在会话诊断与 MCP 面板/快照可见；全部能力失败才 fail-closed；能力 panic 不终止 transport；关闭有界收敛且无遗留任务或订阅。 | 故障注入测试 + 诊断/面板投影断言 + 关闭后任务表/订阅表检查。 | 部分：router 降级/panic/全失败 fail-closed 与宿主关闭收敛有 crate 测试；面板/诊断投影与故障注入未全量执行 |
| V5-N | 命名、审批与历史 | 新 deferred 名为 `mcp__peri__*`；direct 保持原名；旧 transcript 名只做非执行归一；审批按 `(capability, original_name)`；v5 路由不执行 builtin 的 `mcp__<capability>__*` 旧名，v4 路径（`PERI_MCP_BUILTIN=legacy`）继续执行其旧名直到 M4；外部 server 的同形名称不受影响；同出口原始名冲突/跨能力重名/`peri` 被用户抢占在加载期拒绝。 | 工具桥、ToolSearch、审批与历史加载测试；v5 路由负向调用 builtin 旧名必须失败，v4 路径旧名仍可执行，外部同形名称照常可调用。 | 部分：工具桥、ToolSearch、审批与加载期保留名单测通过；真实 transcript 与外部同形 server wire 未执行 |
| V5-M | 迁移与回滚 | 六实例旧会话行为不变；新会话可在影子/默认路径间独立选择；外部 stdio/HTTP/ACP MCP 不受影响；切换前后首个模型请求、冻结配置能力集与运行时能力投影可对账；回滚只影响新会话，不热切换已冻结会话。 | 新旧会话对照记录；冻结配置能力集与运行时投影快照；回滚后旧会话 transcript 与工具面不变。 | 部分：默认切换与 `PERI_MCP_BUILTIN=legacy` 回滚路径有测试；新旧会话对照、观察窗口与外部路径全量回归未执行 |

### 3.1 逐能力 presence/absence 明细

每个能力至少覆盖下表；`workspace` 另需覆盖资源面，`workflow` 另需覆盖任务与继承面。

| 能力 | enabled 必须可见/可调用 | disabled 或 degraded 必须不可见/不可调用 |
| --- | --- | --- |
| `workspace` | direct 文件/终端工具；`workspace://` 资源与订阅；技能、`agent://`、`peri-instruction://`、`peri-meta://` 资源按冻结准入读取 | 上述工具、资源、订阅全部消失或服务端拒绝；技能/指令不得回落宿主读盘；`.peri/meta` 段落覆盖 warn 后保持内置段落 |
| `web` | direct `WebSearch` / `WebFetch` 或声明中的模型面工具；对应静态提示 | 工具、prompt 声明、ToolSearch 索引与执行入口全部消失 |
| `artifact` | artifact 工具与发布能力 | 工具、资源与执行入口全部消失 |
| `cron` | cron 工具；Cron 资源；宿主 tick/后台任务 | 工具、资源、订阅、tick 与宿主任务全部停止 |
| `monitor` | monitor 工具；Monitor 资源；轮询/后台任务 | 工具、资源、订阅、轮询与宿主任务全部停止 |
| `workflow` | workflow 工具；`workflow://` 资源与订阅；workflow agent 继承面与任务信道 | 工具、资源、订阅、继承面、消费者与宿主任务全部消失；不得只隐藏工具但仍保留消费者 |

能力依赖必须按注册表声明验证：依赖被关闭时，dependent 能力必须同步消失或安全失败；不得从完整 pool 重新引入已关闭能力。

## 4. 配置与命名验收要点

1. **双入口**：`.mcp.json` 的 `mcpServers.peri` 与 `peri mcp-start` 参数都是一等入口；standalone 不读取完整 Peri settings 装配。
2. **优先级**：命令行 > `.mcp.json` > 内置默认；冲突来源必须可解释。
3. **扩展键**：只允许 server 条目的命名空间扩展；键名未裁决，本阶段不编码、不预占。顶层 `capabilities` 不得用作配置键。
4. **旧键**：旧 middleware 能力键已退役；加载期告警丢弃，不得 silent no-op。旧逐实例 `.mcp.json` 条目仍确定性映射为能力开关。
5. **保留名**：`peri` 是宿主 builtin 保留名；用户配置不得用 `command`/`url` 覆盖宿主 builtin 身份并继承其审批白名单。standalone 的 `.mcp.json` `mcpServers.peri` 是客户端启动条目，不是宿主覆盖。
6. **路由**：执行与授权按 `(capability, original_name)`；模型面名字只用于入口解析；旧名不得作为执行别名。
7. **观测身份**：事件、日志、工具回执与 Standard Observation 必须保留 capability 来源，不能只保留 `peri` server 名。

## 5. 迁移与回滚验收

迁移阶段与检查点：

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| M1 设计门槛 | 取代关系、配置权威、关闭矩阵、命名/授权、故障域与验收矩阵落入设计/spec | 完成（仅文档） |
| M2 代理与影子 | 先完成 workspace 代理 wire 测试与 capability router；v4 六实例保留，回滚开关只对新会话开启 v5 | 完成：router、能力开关/降级、task 归属、workspace 代理接缝与宿主 `build_host_router` 已接线 |
| M3 切换 | 单出口成为新会话默认；v4 注入降级为兼容 adapter；旧 transcript 归一；旧键映射或报错 | 完成：默认注入 `peri`；`PERI_MCP_BUILTIN=legacy` 保留六实例回滚（M4 截止）；旧 `.mcp.json` 逐实例条目映射为能力开关；旧 policy 键退役 |
| M4 清理 | 进入条件：观察窗口通过、回滚支持正式截止。内容：删除六实例注入、独立 runtime/client 条目与旧 policy 键；保留外部 MCP/ACP 路径。删除前须确认无仍需冷恢复的 v4 会话，否则保留只读 v4 adapter。M4 后不再承诺 v4 装配或旧名执行 | 部分完成：旧 policy 键删除、CLI 单实例 serving 路径删除；六实例注入与 `PERI_MCP_BUILTIN=legacy` 作为回滚 adapter 保留（截止点见 §5.1） |

M4 是回滚支持截止点：M4 开始前必须记录截止决定并确认旧会话处置；若仍有需要冷恢复的 v4 会话，保留只读 v4 adapter，不得删除。M4 后 flag-off 回滚路径与旧名执行为退役状态。

### 5.1 已退役项与保留 adapter（2026-11-16）

已退役（本分支）：

1. 旧 MetaHarness 能力键（`WebMiddleware` / `ArtifactMiddleware` / `CronMiddleware` / `MonitorMiddleware` / `WorkspaceMiddleware` / `WorkflowMiddleware`）：加载期按未知键告警丢弃，不再映射为能力开关；能力名是唯一开关身份。
2. `peri mcp-start <instance>` 的独立单实例 serving 路径：只保留 CLI adapter，把实例名映射到同一单出口能力并告警；`mcp-start peri --capability ...` 是规范形式。

保留的兼容 adapter（截止点均为 M4 截止决定）：

1. `PERI_MCP_BUILTIN=legacy` 六实例注入：回滚与旧会话观察窗口所需，默认路径不经过。
2. 旧 `.mcp.json` 逐实例条目 → 能力开关映射：配置 adapter；新会话不写回六条目。
3. 旧 transcript 工具名归一：只用于展示/匹配，不是执行别名。
4. `mcp-start <instance>` 命令行映射：不启动独立 server，只解析为 capability 选择。

迁移与回滚验收项：

1. **beta flag**：v5 新装配只对新会话生效，flag 关闭时走 v4 六实例兼容路径（仅到 M4 前）；flag id 与注册表条目在实现期按 `beta-flags.md` 引入，本阶段不预占。
2. **能力集模型**：新会话冻结配置能力集并记录来源 revision 供诊断；运行时能力投影由该集合减去失败能力派生，用于目录、ToolSearch、资源/订阅、继承面、宿主任务、readiness、诊断与 runtime authorization；恢复只通过显式重连、新会话或冷恢复，冷恢复不持久化降级状态；子 Agent 复用；冷加载/恢复不重新解析、不隐式换身份。
3. **旧配置 adapter**：v4 逐实例 `.mcp.json` 条目确定性映射为能力开关；映射失败为 typed error；新会话不写回六条目。
4. **旧 transcript**：v5 路由只把旧名用于展示/匹配归一；`original_tool_name_of_effective` 不用于执行路由；归一失败报错。v4 路径继续执行其 `mcp__<instance>__<tool>` 名，直到 M4。
5. **回滚**：只对新会话选择旧装配（仅到 M4 前）；已冻结会话不得热替换 pool、重写历史或重新发布已关闭实例。
6. **观察窗口**：切换默认前记录新旧首个模型请求、readiness、面板/诊断、资源/订阅、宿主任务与关闭矩阵的对照；窗口时长、样本量与退出条件在切换前写入本 spec，不写死在设计文档。
7. **外部路径**：外部 stdio/HTTP/ACP MCP 的注册、启停、工具名与授权不变；单出口只收敛 builtin。
8. **回滚支持截止**：M4 开始即截止；截止决定必须在 M4 前写入本 spec，并确认无仍需冷恢复的 v4 会话；M4 后不再承诺 v4 装配、flag-off 回滚或旧名执行。

## 6. 未验证与未决项

### 6.1 本轮未验证

1. 单出口 router、能力开关/降级、task 归属与 workspace 代理接缝已实现并有 crate 级测试；已接入宿主默认装配，宿主 in-process wire 覆盖六能力聚合与逐能力关闭；未验证真实 Sandbox 上游与首个模型请求。
2. 代理的 mock-peer wire 测试已覆盖并发 request id、取消透传、progress token、task id 映射、资源读取/订阅与断线降级；未运行 Sandbox upstream 或 router + host 端到端 MCP wire 实验，断线清理与重连后的订阅恢复仍未验证。
3. 未验证新旧会话隔离、观察窗口与回滚截止决定；`PERI_MCP_BUILTIN=legacy` 回滚路径有 crate 测试。
4. 未验证跨进程 `taskOwner`、task id 归属或 workspace 上游重连。
5. 未验证能力级降级在首个模型请求、面板、诊断三处的一致性。

### 6.2 未决项

| 项 | 状态 | 关闭条件 |
| --- | --- | --- |
| `.mcp.json` server 条目的能力扩展键名 | 未裁决 | 按 MCP 生态复核后裁决；标准客户端忽略不影响连接 |
| 能力/工具声明表的类型名与字段名 | 实现期裁决 | 语义冻结为 capability；可保留 `instance` 字段名 |
| 聚合 handler 的 crate 落点 | 已裁决（实现期） | `peri-middlewares::mcp::builtin::router`；复用现有 builtin dispatch 与 `UpstreamProxy` |
| workspace upstream descriptor 来源、授权与生命周期 | 未裁决 | wire 门槛验证；Sandbox actor 拥有生命周期 |
| task id 编码 | 未裁决 | 全局唯一或稳定能力前缀；出口内可唯一判定 |
| beta flag id 与注册表条目 | 实现期裁决 | 按 `beta-flags.md` 引入并记录 |
| 观察窗口参数 | 实现期裁决 | 切换默认前写入本 spec |
| 代理重连/超时/重试策略 | 未裁决 | 首阶段不做自动热恢复；验证断线清理与旧任务隔离 |

## 7. 关闭条件

本 spec 只有在以下条件全部满足后才能关闭或压缩为历史：

1. §3 的 V5-C / V5-G / V5-W / V5-L / V5-N / V5-M 均有运行证据，且证据覆盖首个模型请求、ToolSearch、资源、订阅、继承面、宿主任务与授权面。
2. 每个能力的 enabled / disabled / degraded 三态均有 presence/absence 记录；不存在「列表隐藏但可调用」的中间态。
3. 旧名只能做非执行归一；旧配置与旧 middleware 键均确定性映射或 typed error。
4. 迁移期新旧会话隔离、冻结配置能力集、运行时能力投影与回滚均有记录；外部 MCP/ACP 路径未受影响。
5. 清理期完成后再按 `DOC-HISTORY-001` 更新稳定事实源并删除过程文档；未完成项不得提前删除本 spec。
