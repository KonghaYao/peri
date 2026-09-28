# System MCP 工具原名注入与冲突处理计划

状态：已实施并通过自动化与真实调用验证，待用户验收。

## 已确认目标

- system MCP 工具向模型注入时使用原始工具名，不附加 `mcp__<server>__` 前缀。
- 名称冲突不使启动失败：记录 warning，保留先注入的工具，后来的冲突项不覆盖它。
- 原始 MCP wire tool name、server/connection 身份与模型可见名称分开保存，调用仍抵达正确实例。
- 普通非 system MCP 的前缀规则不在本次修改范围。

## 本次范围

按本轮开始时说明的默认范围，保留 `system_mcp_tools` 筛选，仅修改入选工具名称：

- `system_mcp=true` 保留首轮 readiness 闸门。
- `system_mcp_tools` 决定 direct 集合，空数组仍仅要求 ready。
- Cron/LSP 当前 deferred 语义保持；未入选项仍走既有发现/执行路径。
- 普通 MCP 的命名与调用协议保持。

## 变更涉及的边界（实施前审计）

| 入口 | 当前事实 | 实施要求 |
| --- | --- | --- |
| `peri-middlewares/src/mcp/tool_bridge.rs` | 名称、Apps allowed tools 等消费 effective name | 模型名与目标身份分离，Apps 使用最终获准目录 |
| `peri-middlewares/src/mcp/system_tools.rs` | 必需工具检查与碰撞失败混合 | 缺失/schema 错误继续失败；冲突改 warning + first-wins |
| `peri-middlewares/src/mcp/middleware.rs` | startup 与工具更新依赖来源/名称 | 显式区分发现成功、获准注入、因冲突被跳过 |
| `peri-agent/src/session/exec/stage_builder/tools.rs` | 同名 insert 后入覆盖 | 改为统一首次准入规则 |
| `peri-agent/src/session/tool_catalog.rs` | 启动目录检查及来源识别依赖前缀 | 使用显式来源，避免原名 system 工具消失或再次被判冲突失败 |
| `peri-middlewares/src/permission/mod.rs` | 部分外部工具保守审批依赖 `mcp__` 前缀 | 使用绑定来源；外部同名 Write/Bash 不能冒充 builtin 获得权限 |
| builtin 注册表、prompt、hooks、Subagent、Workflow、ACP/事件 | 多处依赖 effective name 或工具归一 | 沿实际消费者同步，保证提示、权限、执行与展示一致 |

连接池含 HashMap，异步连接完成先后不是稳定的注入顺序；不能直接遍历连接池决定获胜者。

## 实施步骤

1. 明确稳定准入顺序：已进入会话目录的工具优先；system server 沿 readiness 的确定排序，同一 server 沿 `tools/list` 顺序。同名工具在相反连接完成顺序下须有同一胜者。
2. 建立工具来源与调用目标的显式投影，保留 server、原 wire name 与实例身份。只在模型边界选择原名，不改变远端调用参数。
3. 在唯一目录准入边界落实 first-wins；统一主工具表、startup 更新、Subagent/Workflow 视图与 Apps 的获准映射。来自同一身份的正常刷新可以替换自身对象，不能让冲突败者借刷新接管名称。
4. warning 只记录冲突名及获胜/被跳过来源，不记录配置、凭据或工具参数。按现有名称解析规则检查大小写和别名冲突，不能在执行时产生另一套胜者。
5. 同步 required 工具状态：发现成功但冲突跳过不能再次触发“必需工具缺失”；真实缺失、schema 无效、初始化失败仍按原契约失败关闭。
6. 权限、只读过滤、参数别名、事件分类按实际绑定身份处理，不通过裸名字推断可信 builtin。同步 prompt 中工具引用及锁定测试；不增加隐式旧名字执行别名。
7. 更新对应模块指引、代码索引与 system MCP 契约，再运行自动化和真实 `./dev.sh -p` 验收。

## 验收矩阵

- 首轮模型请求含预期原名，调用真实 MCP wire 原始工具成功；普通 MCP 仍使用前缀。
- core/system、system/system 同名冲突均 warning，先入者保留，只有胜者产生副作用。
- 改变连接完成顺序、重连和刷新不会改变既有绑定或重复注入。
- 必需工具被重名遮蔽不会二次报启动失败；工具确实缺失、schema 错误和 readiness 失败仍拒绝启动。
- 大小写、别名、外部工具名恰为 Read/Write/Bash 时，不绕过审批或只读限制。
- 主 Agent、Subagent、Workflow、搜索/执行与 Apps 对同一名称看到相同调用目标。
- 当前已修复的 bare 能力、Cron 调度、错误恢复、超时和取消不回退。

优先回归入口：`system_tools_test.rs`、`tool_bridge_test.rs`、`client/readiness_test.rs`、`session/tool_catalog_test.rs`、`stage_builder/tools_test.rs`、权限测试，以及 ACP `mcp_v4_startup_test.rs` / `mcp_v4_wire_fixture_test.rs`。过滤执行必须核对非零用例与最终退出状态。

## 分工建议

- A：bridge 名称/来源投影、system tools 与 Apps 映射。
- B：统一目录准入、稳定顺序、冲突状态与刷新语义。
- C：权限和消费者审计、prompt/文档同步、真实 MCP 集成验证。

先共同冻结身份与准入接口，再并行修改独立文件；所有代码按本仓库规模限制和验证规则执行。

## 实施边界与验收证据

- 已选 system 工具使用原名，初始收集等待整批 system server 的本代发现证据，再按 server 名与 `tools/list` 顺序准入；冲突同时覆盖名称、大小写及别名。
- 执行、Apps、审批、提示声明和子 Agent 白名单使用绑定来源；外部同名工具不会继承 builtin 审批例外、参数别名或声明。Auto 分类输入和缓存区分外部 server/wire 身份。
- ToolSearch 的刷新和代理解析保留已获准的外部同名工具；子 Agent 混合 wildcard 列表维持既有显式白名单语义。
- 真实 `./dev.sh -p` 已完成 Write、Edit（无需先读）、Read、Grep、Glob、folder_operations 和 Bash 调用。首次发现 MCP overview 仍泛称前缀调用，修正后再次运行，Read 与新提示均通过。
- print bare/exit/background、MCP host policy/isolation、canonical invocation、PTC 和 workspace registration 集成测试共 31 项通过。
- MCP 专项 614 项通过，包含两项真实 120 秒超时/取消用例。全量复跑中跳过这两项已验证的耗时用例。
- 合并上游前的 workspace 单元测试 6919 项通过、0 失败、36 项保持仓库既有 ignored；严格 `cargo clippy --workspace --all-targets -- -D warnings`、格式、拼写和依赖边界检查通过。
- 压缩提交基点为 `16683af8`；随后合并 `origin/main` 的 Compact 修复 `eb35ce89`，保留报告摘要/排除与原名工具路径提取两组行为。合并后 Agent / ACP 完整单元测试 1659 项与 print 集成测试 14 项通过，包含补齐的原名与历史前缀名混合路径提取回归；本轮日志在 `/tmp/peri-main-merge-20260928/`。
- 验收日志位于本机 `/tmp/peri-system-mcp-20260928/`；用户验收完成前保留此 active issue，稳定契约见 `ARC-TOOLS-001` 与 `ARC-HITL-001`。
