# 架构文档脱离扫描：契约、实现与现状标记对齐

- 状态：**Active / P1 已修复并通过定向回归，11 项关键架构说明已更新；P2 实现缺口与 P3 保留**。
- 扫描日期：2026-10-07；读取基线为 `b504ad04` 及当时共享工作树，不是纯提交快照。
- 授权范围：初始扫描与记录；2026-10-07 用户追加授权更新关键架构说明并处理 P1。仅修 A1 的实现，更新 P2 中的关键说明，不实施 A2/A3 或 P3，不改变已批准目标，不 commit。
- 执行：5 个并行 subagent 分主题只读核查，主 Agent 核查总览、配置边界、链接及关键调用链。显式 `max` 参数被当前模型接口拒绝，实际使用继承模型配置；不声称执行了 max 模式。
- 初始严重等级：**P0 0 项、P1 1 项、P2 13 项、P3 10 项，共 24 项**。以下保留原分级和发现身份，逐项标注处理状态。
- 当前剩余：**P1 0 项、P2 实现缺口 2 项、P3 10 项**。P1 和 11 项 P2 说明已处理，不据此宣称全部运行时部署验收完成。
- 问题类型是独立维度：3 项实现违反契约、21 项文档/职责/路由漂移；跨文档 schema 与诊断策略问题各合并为一项。

## 判断口径与覆盖

按 `STD-INDEX-002` 区分现状、规则和目标：代码存在不证明目标完成，目标未完成也不自动构成文档过时。文档标为现行设计或明确声称当前行为时，才与当前实现直接比较；已批准目标中的明确约束，不能仅改写文档来掩盖实现缺口。

分工覆盖 `docs/design/` 的 32 份主题设计及 README，并按风险点读取对应 standards、模块指引、code-index、active spec、实现和相邻测试源码：

| 核查组 | 主题 |
| --- | --- |
| 主 Agent | architecture、configuration-authority、索引状态、链接与依赖门 |
| Agent/消息 | meta-harness、message-transcript、micro-compact、rcra-message-activation、model-adapters、system-prompt、system-reminder |
| ACP/存储 | peri-acp-protocol、session-id-environment、storage-v2-machine-workspace-session、session-async-tasks、interaction-brokers |
| MCP/工具 | middleware-system、tool-system、dynamic-mcp、mcp-adaptation-v4-part-1、mcp-multiplexing、mcp-cache |
| TUI | tui-acp-data-flow、tui-chat-workbench、tui-streaming-markdown-performance、tui-subagent-activity、user-input-queue、command-system |
| SDK/编排 | workflow、ultra-adlc、serverless-execution-identity、time-runtime、git-watch-middleware、meta-control |

下述文档/源码行号记录初始扫描时的修复前证据，不是新增稳定架构规则。已处理项的现行入口可能已移动或删除；本轮实施与验证见下节，未处理项仍须重新核对共享工作树。

## 本轮实施与验证（2026-10-07）

- **P1 A1**：删除 `PoolAppToolDispatcher` 及宿主直接调用 bridge/签发 lease 的分支；保留输入与连接准入校验，无 canonical 审批上下文时记录结构化原因并返回 `PolicyDenied`。取消在准入前返回 `Cancelled`。
- **功能边界**：现行生产宿主 `peri/mcp/invoke` 不再重跑工具，含 UI 重开后的调用；模型签发的 canonical lease 与既有 App 续调用路径保留。恢复宿主调用必须另行接入同一审批能力，不能回落 pool 直调。
- **回归**：真实内存 MCP server 的调用计数为 0，拒绝后不产生新 lease/turn；ACP relay 的 policy denial 返回错误，不构造成功工具投影。新增拒绝与取消测试先失败（原实现错误为 UpstreamProtocolError/UnknownServer），修复后通过。
- **文档**：同步 B1/B2/B3/B4/B5/B6/B7/B9/B10/B11/B18，区分能力图与依赖门、持久事实与易失句柄、SDK registry 与 KV 占位，并修正 schema、恢复 cwd、wire、broker、输入投影、诊断 scope、Workflow 启动和配置权威。未降低 A2/A3 的契约要求。
- **规则与路由**：新增 `ARC-MCP-APPS-001` 固化既有审批要求及 fail-closed 路径，更新 Middleware 代码索引，删除旧 dispatcher 路由；模块指引已核对，不修改已有用户变更。

| 已执行验证 | 结果 |
| --- | --- |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- mcp::apps` | 23 passed，包含宿主 invoke、lease 生命周期与 profile |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::mcp_apps::tests` | 9 passed，包含 policy denial 不发布成功投影 |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- effective_dispatcher` | 1 passed，canonical 嵌套分派与 pinned catalog 回归 |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- permission::` | 65 passed，现行审批/拒绝/来源策略回归 |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --doc` | 0 failed，3 ignored，无实际运行的 doc example |
| 受影响 Rust 文件 rustfmt 检查 | 通过；3 个文件均低于 1000 行 |
| Design 本地 Markdown 目标路径检查 | 33 文件、138 链接、0 缺失；不验证 anchor |
| `bash scripts/check-layer-imports.sh` | 22 规则，0 违规 |

共 98 项定向单元测试通过；未运行全 workspace、E2E、真实远端或托管 WASM 验收。检查基于共享工作树，包含其他线程未提交的依赖变更，不把结果外推为纯基线或全仓通过。

## 严重等级与定级边界

本节是本次扫描的风险判断，不新增仓库全局分级标准，也不复用 testing.md 的测试优先级含义。按当前证据支持的影响和范围定级，不将“实现问题”自动设为高等级，也不将“文档问题”自动设为低等级。

- **P0（紧急）：0 项。** 本轮未取得全局不可用、不可恢复数据损坏或正在发生的严重安全事件证据；静态扫描不证明这些事件绝不存在。
- **P1（高）：1 项。** 已证实调用路径的授权安全边界缺失。
- **P2（中）：13 项。** 功能契约、可诊断性、wire 可用性，或关键安全/部署/领域说明存在实质差异。
- **P3（低）：10 项。** 局部职责、交互、算法和导航描述失真，尚未证实更高等级的实际影响。

原 A/B 编号仅保留作发现身份，不代表严重等级。引用的既有 P0 issue 保持原定级；本报告对其某个子问题独立评估，不自动继承或改写父任务级别。全部行为影响仍为静态核查，未完成运行时复现。

## P1 — 高：授权安全边界缺失（1 项）

已存在绕过承诺审批入口的调用分支，影响授权安全边界，应优先修复。结论限定于宿主签发 lease，尚无实际越权事件或利用复现。

### P1-01. MCP Apps 宿主签发 lease 的调用分支绕过审批 seam（A1）

- 处理状态：已修复并通过定向回归；删除 pool 直调 dispatcher，缺少 canonical 审批上下文时拒绝宿主 invoke。
- 类型：实现违反契约；需修实现并补行为回归，不能仅改文档。
- 文档：`docs/design/mcp-multiplexing.md:228`、`docs/design/mcp-multiplexing.md:366` 要求 App 工具调用进入 canonical invocation、Permission/HITL，不得直调低层 MCP。
- 实现：`peri-middlewares/src/mcp/apps_invoke.rs:139` 创建 `PoolAppToolDispatcher`；其 `dispatch` 在 `peri-middlewares/src/mcp/apps_invoke.rs:72` 直接 `bridge.invoke`。`peri-middlewares/src/mcp/tool_bridge.rs:661` 将 dispatcher 写入 lease，后续 `peri-middlewares/src/mcp/apps_relay.rs:232` 使用该 dispatcher。
- 影响：**限定宿主签发分支**。lease 归属、白名单、generation 和 cancellation 校验不等于每次调用审批；模型路径可携带 canonical dispatcher，不据此否定所有 Apps 调用。
- 修复与验收：宿主分支绑定具备审批能力的 canonical dispatcher；缺少审批上下文时拒绝。构造宿主 lease，拒绝审批后断言 MCP server 零调用，保留现有 lease 失效测试。

## P2 — 中：功能契约、诊断与关键架构误导（13 项）

可能改变模型上下文、丢失诊断、导致客户端调用失败，或误导安全语义、部署与关键领域实现；未证明存在 P1 级授权绕过或不可恢复损害。实现缺口与文档漂移逐项标注，不通过类型推导严重等级。

### P2-01. Micro 的默认 Preserve 声明没有进入实际保留策略（A2）

- 处理状态：保留，未修实现；不在本次追加授权范围。
- 类型：实现违反契约；需修实现并补行为回归，不能仅改文档。
- 契约：`docs/design/micro-compact.md:221`、`docs/design/micro-compact.md:226` 声称默认 Preserve 并回退工具声明；`peri-acp-types/src/tools.rs:668` 明确承诺未显式标注工具不被压缩。
- 实现：`peri-agent/src/agent/compact_v2/planner.rs:191` 只读配置 map 和旧黑名单，未命中时返回可压缩；`peri-acp-types/src/compact.rs:290` 的默认 map 为空。全仓 Rust 检索 `context_retention()` 仅命中 trait 定义，map 写入仅见测试构造。
- 影响：未列入 map/黑名单的工具，其旧轮次长成功结果可能被压缩；工具默认声明不构成实际保护。此项针对结果保留，不是历史输入压缩。
- 修复与验收：接入单一权威 retention 策略，覆盖默认 Preserve、显式可压缩、StateBearing、错误结果与配置覆盖。验收必须经过实际工具目录到 provider-facing projection，不能只手工给 planner 填 map。

### P2-02. Workflow Node 日志的级别和正文在消费点丢失（A3）

- 处理状态：保留，未修实现；不在本次追加授权范围。
- 类型：实现违反契约；需修实现并补行为回归，不能仅改文档。
- 文档：`docs/design/workflow.md:145` 要求 Node log 按 level 映射 tracing；`docs/standards/architecture-contracts.md:159` 要求诊断保真。
- 实现：`peri-workflow/src/runner/message_loop.rs:168` 的 log 分支不读取级别/正文，只输出固定 debug 文案 `workflow node log received`。
- 影响：Node 上报 error/warn 也只剩 debug 信号；提高日志等级不能恢复已经丢失的正文。
- 修复与验收：恢复级别和实际诊断映射，独立保持长度与终端控制字符约束；用合成数据验证 error/warn 在默认级别可见、正文和原因保留。归入既有 `spec/issues/2026-10-06-p0-agent-internal-error-diagnostic-loss.md` 的诊断范围，不新增与其竞争的规则。

### P2-03. ACP 生命周期表混用领域字段与真实 wire 字段（B5）

- 处理状态：关键说明已更新；未以文档同步代替相关运行时部署验收。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/peri-acp-protocol.md:28`、`docs/design/peri-acp-protocol.md:30`、`docs/design/peri-acp-protocol.md:31` 列出 `session_id`、`source_session_id`、`new_session_id`。
- 证据：`peri-acp/src/host/requests/session_restore.rs:79`、`peri-acp/src/host/requests/session_lifecycle.rs:602`、`peri-acp/src/host/requests/session_lifecycle.rs:631` 读取 `sessionId`。`peri-acp/src/host/requests/session_control/reopen_test.rs:187` 使用该字段，`:196` 读取响应的 `sessionId`。
- 影响/建议：照表实现客户端会缺少必需字段或读错返回值。按 handler/序列化 DTO 更新 wire 表，仅列真正接受的别名。

### P2-04. 交互设计仍保留已删除的 StdioBroker 和默认全批准（B6）

- 处理状态：关键说明已更新；未以文档同步代替相关运行时部署验收。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/interaction-brokers.md:29`、`docs/design/interaction-brokers.md:60` 仍列 StdioBroker/旧 context 入口，随后称默认所有审批批准、问答空答案。
- 证据：`peri-acp/src/host/stdio/mod.rs:3` 说明删除旧 StdioContext；`peri-acp/src/host/prompt.rs:134` 共用 AcpTransportBroker，`peri-acp/src/broker/transport_broker.rs:47` 默认 Forward，`:265` 对有原因的问答取消返回 Unanswered。
- 影响/建议：误述审批安全边界。删除旧实现路由，区分显式 auto-approve 模式和 transport 默认转发，保留 Unanswered 与生命周期取消差异。

### P2-05. 恢复设计仍承诺请求 cwd 不匹配就拒绝（B4）

- 处理状态：关键说明已更新；未以文档同步代替相关运行时部署验收。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/peri-acp-protocol.md:42`、`docs/design/session-id-environment.md:181` 仍称请求 cwd 被作期望校验。
- 证据：`peri-acp/src/host/workspace.rs:569` 的检查入口将请求参数命名为 `_expected`，使用保存的 bound workspace；`docs/standards/architecture-contracts.md:14` 明确请求 cwd 不作为按 ID 恢复门槛。
- 影响/建议：客户端不能依赖该参数获得目录 mismatch 拒绝。同步说明保存 binding、机器环境及实际目录的执行准入验证仍保留；不要把按 ID 恢复改成请求路径认领。

### P2-06. 多处“当前 schema14”已落后于 schema17（B3）

- 处理状态：关键说明已更新；未以文档同步代替相关运行时部署验收。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/session-id-environment.md:237`、`docs/design/serverless-execution-identity.md:126`、`docs/code-index/peri-resources.md:64`、`docs/code-index/peri-resources.md:90` 仍报当前版本或最终迁移为 14。
- 证据：`peri-resources/src/sessions/canonical.rs:27` 为 17；`peri-resources/src/sessions/sqlite_store/schema.rs:238` 写入 17。`spec/issues/2026-10-07-workstate-schema17-redesign.md:13` 要求保留 schema17 格式。
- 影响/建议：误导兼容和迁移排障。当前版本路由到 canonical 常量，不机械删除 v12/v14 的历史迁移含义，不把版本修正当作 Work 性能优化闭环。

### P2-07. Micro 文档仍声称压缩历史工具输入（B7）

- 处理状态：关键说明已更新；未以文档同步代替相关运行时部署验收。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/micro-compact.md:108`、`docs/design/micro-compact.md:114`、`docs/design/micro-compact.md:126` 保留参数压缩描述。
- 证据：`peri-agent/src/agent/compact_v2/planner.rs:270` 不生成 input 投影；`peri-agent/src/agent/compact_v2/projection.rs:366`、`peri-agent/src/agent/compact_v2/projection.rs:399` 保留 canonical 输入，遵循 `docs/standards/architecture-contracts.md:165`。
- 影响/建议：可能诱导恢复已禁止行为。删除现行输入压缩叙述，区分 legacy directive 解码与当前 renderer 政策；测试现已按职责拆分，`peri-agent/src/agent/compact_v2/projection_render_test.rs` 的 `legacy_tool_input_projection_preserves_selected_long_fields_and_tool_use` 明确验证保留原参数。

### P2-08. 诊断保真已获批准，但多份文档继续承诺全链路脱敏（B9）

- 处理状态：关键说明已更新；未以文档同步代替相关运行时部署验收。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/code-index/peri-model.md:83`、`docs/code-index/peri-tui.md:208`、`docs/design/system-reminder.md:304`、`docs/design/tui-acp-data-flow.md:190`、`docs/design/tui-chat-workbench.md:134`、`docs/design/ultra-adlc.md:245`、`docs/design/dynamic-mcp.md:569`、`docs/design/dynamic-mcp.md:616` 仍有绝对不输出 secret 或统一脱敏承诺。
- 证据：`docs/standards/architecture-contracts.md:159` 和诊断 active issue 已批准运行时诊断不做内容脱敏。`peri-model/src/anthropic/mod.rs:87` 保留 Debug 内容，`peri-tui/src/kit/acp_events/agent.rs:13` 展示实际错误。Reminder `docs/design/system-reminder.md:205` 与 Dynamic MCP `docs/design/dynamic-mcp.md:255` 自身已表达新政策，旧条款造成内部冲突。
- 影响/建议：会引导重新遮蔽诊断或错误地将日志当无凭据数据管理。统一诊断、主动收集/交付凭据、模型受众、secretRef 存储和日志访问控制的不同 scope；不得将保真扩大成无条件记录/广播成功业务数据，也不得删除认证、权限、限长或控制字符约束。

### P2-09. SDK 的执行权威仍被简化为 ManagedAgents KV claim（B10）

- 处理状态：关键说明已更新；未以文档同步代替相关运行时部署验收。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/serverless-execution-identity.md:150` 将 AtomicManagedAgentKv 作为执行权协调权威。
- 证据：`npm-packages/@peri-sdk/src/execution/session-execution.ts:42` 创建 admission/coordinator，`npm-packages/@peri-sdk/src/execution/admission-service.ts:42` 使用 SqliteExecutionRegistry，`:70` 按持久实例、attempt 和 entered evidence 准入。`docs/code-index/peri-ts-sdk.md:3` 已记录持久 ExecutionRegistry 权威。
- 影响/建议：只共享 KV 不足以解释完整 RCRA 准入需求。区分对象/会话占位和执行准入，说明 registry、generation 与 proof provider；不因此宣称跨实例接管已验收。

### P2-10. Workflow 启动仍被写成 bunx 优先、自动 npx fallback（B11）

- 处理状态：关键说明已更新；未以文档同步代替相关运行时部署验收。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/workflow.md:136`、`docs/design/workflow.md:563` 保留旧启动与兜底说明。
- 证据：`peri-workflow/src/runner/artifact.rs:101`、`peri-workflow/src/runner/artifact.rs:106` 校验 artifact 后使用 node；仅测试或 `PERI_WORKFLOW_ALLOW_NPX_FALLBACK=1` 允许固定版本 npx，否则 SpawnFailed。
- 影响/建议：影响离线部署、网络依赖和失败排查。删除 bunx 路径，明确生产默认 fail-closed 和显式 opt-in 边界。

### P2-11. 总览混用了能力提供方向和 Rust 依赖方向（B1）

- 处理状态：关键说明已更新；未以文档同步代替相关运行时部署验收。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/architecture.md:11` 定义箭头为提供方向，`:20` 画 Middleware → Agent，又在 `docs/design/architecture.md:36` 宣称未声明边一律禁止并由 crate 依赖门验证。
- 证据：`peri-middlewares/Cargo.toml:9` 实际依赖 Agent；`peri-agent/Cargo.toml:26` 的依赖中没有 Middleware。`scripts/import-exemptions.conf:137` 允许 middlewares 消费 agent，`:143` 禁止 agent 反向消费 middlewares。ACP 的合法宿主装配依赖另在 `peri-acp/Cargo.toml:15` 明确保留，也不完整出现在总览图中。
- 影响/建议：同一图无法同时作为能力注入图和完整 crate 白名单。明确两类图的语义，依赖合法性路由到标准及实际门禁，标明部署装配与接口注入的允许边；不要为迎合旧图反转现有依赖。

### P2-12. 总览的易失 Task 定义与持久任务事实、目标引用混在一起（B2）

- 处理状态：关键说明已更新；未以文档同步代替相关运行时部署验收。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/architecture.md:78` 无状态区分地引用待重构异步任务设计，`:190` 又称 Task 为易失投影，`:215` 将后台任务笼统定义为不持久化。
- 证据：`docs/design/session-async-tasks.md:3` 明确是待重构目标；`docs/design/rcra-message-activation.md:30` 区分持久领域和内存投影。当前 `peri-acp-types/src/session_resources/work.rs:503` 已包含持久 work、task bindings、delegations 与 obligations，`peri-resources/src/sessions/work.rs:20` 定义持久 state 表，`peri-resources/src/sessions/sqlite_store/session_data/work.rs:135` 事务化写入。
- 影响/建议：将易失运行句柄/registry、持久任务绑定与处理义务、目标 Task 目录分别命名并标记状态。持久事实存在不证明独立任务目录已完成；load 不自动复活旧 execution，也不等于未知副作用和交付责任可以丢弃。

### P2-13. MCP cache 配置权威仍被分配给 loader（B18）

- 处理状态：关键说明已更新；未以文档同步代替相关运行时部署验收。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/mcp-cache.md:7`、`docs/design/mcp-cache.md:50`、`docs/design/mcp-cache.md:79` 将规则校验/关闭优先合并归 loader。
- 证据：`peri-middlewares/src/mcp/config.rs:11` 使用 core 类型，`:417` 委托 core 合并，`:458` 调用 snapshot.mcp_with_plugins；初始化在 `peri-middlewares/src/mcp/initialize.rs:246` 选用快照。
- 影响/建议：容易制造第二份规则。按输入 provider → peri-config 规则/快照 → middleware overlay → pool 不可变策略说明职责。

## P3 — 低：局部边界、算法与维护导航失真（10 项）

主要影响维护理解、局部交互说明与定位；当前证据未显示这些文档偏差本身导致运行失败、安全失效或数据损坏。后续若取得行为影响证据可重新定级。

### P3-01. Transcript 的绝对不可变表述遗漏合法输入准备（B8）

- 处理状态：保留，未处理；P3 不在本次追加授权范围。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/message-transcript.md:13` 称内容不可原地修改、变更必须产生新 ID。
- 证据：`peri-agent/src/session/transcript.rs:561` 支持稳定 ID 替换，`peri-agent/src/agent/stages/middleware_runner.rs:157`、`peri-agent/src/agent/stages/middleware_runner.rs:174` 在输入准备后 reconcile；`docs/standards/architecture-contracts.md:153` 已批准此例外。
- 影响/建议：按旧原则新建 ID 会破坏合法输入身份和批次关联。限定普通历史/Compact 不变原则，显式说明 before_agent/before_input 的输入准备例外及错误后 reconcile。

### P3-02. Meta 数据访问仍路由旧 ThreadStore/SQLite-only seam（B12）

- 处理状态：保留，未处理；P3 不在本次追加授权范围。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/meta-control.md:5`、`docs/design/meta-control.md:34`、`docs/design/meta-control.md:115` 仍描述 SqliteThreadStore/ThreadStore 调用链和旧命令面。
- 证据：`peri-tui/src/cli_meta.rs:304` 经 open_deployment/session_resources/load_session_meta；`peri-tui/src/main.rs:489` 支持 session-store locator 并与 db-path 互斥，`:170` 已有 meta machines。
- 影响/建议：维护者会在旧接口扩展查询。更新 deployment seam 与命令路由，保留只读、显式 session 选择和禁止业务写入约束。

### P3-03. Cache coverage 的样本失效和告警时机仍是旧规则（B13）

- 处理状态：保留，未处理；P3 不在本次追加授权范围。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/tui-acp-data-flow.md:126` 要求 missing/zero 清除样本、回合终态前最多一次告警。
- 证据：`peri-tui/src/kit/acp_notifier/session_update.rs:268` 对缺失不发新样本，零值为有效样本；`peri-tui/src/kit/acp_events/turn.rs:63` 每个有效样本即时检查，遵循 `docs/standards/architecture-contracts.md:77`。
- 影响/建议：旧文档诱导丢有效样本或恢复终态集中告警。同步缺省/零值/不一致三种情况与逐样本告警，旧测试名称不能替代当前行为验证。

### P3-04. TUI notifier 被误写为完全无 UI 副作用（B14）

- 处理状态：保留，未处理；P3 不在本次追加授权范围。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/tui-acp-data-flow.md:65`、`docs/design/tui-acp-data-flow.md:183` 称只解码转发、统一由 bridge 写状态。
- 证据：`peri-tui/src/kit/acp_notifier.rs:267` 发布 commands，`:278` 同步发布 plan，`:284` 分支更新 spinner；`docs/code-index/peri-tui.md:131` 已说明这些同步发布。
- 影响/建议：混淆纯 decoder、同步发布和 transcript reducer 的边界，错误迁移会改变顺序。按事件类别标明真实 owner 与 session 校验位置，不为了符合旧文档搬实现。

### P3-05. VIEW_MODELS 独占写入声明遗漏本地 presentation 更新（B15）

- 处理状态：保留，未处理；P3 不在本次追加授权范围。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/tui-acp-data-flow.md:96` 称只有 bridge canonical publish 可替换 atom。
- 证据：`peri-tui/src/kit/message_area/entry_nav.rs:302` 本地折叠更新展示快照/generation，`peri-tui/src/kit/acp_events/render.rs:150` 从已发布 generation 接续；`docs/design/tui-streaming-markdown-performance.md:48` 已记录共享版本纪律。
- 影响/建议：canonical transcript 权威不等于展示快照单写者。说明合法 presentation mutation、锁和 generation，不恢复独立版本计数或删除本地折叠。

### P3-06. Command 文档把已删除的 kind 反推继续标为现状（B16）

- 处理状态：保留，未处理；P3 不在本次追加授权范围。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/command-system.md:157` 仍路由 input_area.rs，称从 SKILL_NAMES/MCP_SKILL_NAMES 反推 kind。
- 证据：`peri-tui/src/kit/input_area/popup.rs:76` 只消费 AVAILABLE_SLASH_COMMANDS，`:103` 直接复制 entry.kind；相邻测试见 `peri-tui/src/kit/input_area_test.rs:482`、`peri-tui/src/kit/input_area_test.rs:531`。
- 影响/建议：产生重复技术债任务。删除旧现状括注和路由，不能因此把 Command 其他明确未落地目标标为完成。

### P3-07. 用户队列只允许 Queued 取回的描述遗漏未领取发布（B17）

- 处理状态：保留，未处理；P3 不在本次追加授权范围。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/user-input-queue.md:41`、`docs/design/user-input-queue.md:64` 只列 Queued。
- 证据：`peri-tui/src/kit/steer_queue.rs:36` 允许 Queued/Dispatching，`peri-tui/src/kit/steer_queue/view_test.rs:206` 覆盖 published-unclaimed 的取回资格；目标领取/撤回边界见 `docs/design/rcra-message-activation.md:262`。
- 影响/建议：按文档收紧会删除合法入口。区分草稿取回与发布后未领取撤回，最终资格以服务端原子裁决为准，按钮可用不保证撤回成功。

### P3-08. ToolSearch 索引失效仍描述 version/count 旧算法（B19）

- 处理状态：保留，未处理；P3 不在本次追加授权范围。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/tool-system.md:149` 使用 content_version/cached_prompt_version/数量判断重建。
- 证据：`peri-middlewares/src/tool_search/middleware.rs:45` 生成 name/description/schema fingerprint，`:114` 比较它；`:60`、`:77` 另建立请求专用索引。
- 影响/建议：误导同数量内容变化及 schema 更新排查。区分共享索引失效与请求绑定索引/resolver；验证入口为 `peri-middlewares/src/tool_search/middleware_test.rs:549`。

### P3-09. Middleware inventory 遗漏展开实例和关键 hook（B20）

- 处理状态：保留，未处理；P3 不在本次追加授权范围。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/middleware-system.md:132`、`docs/design/middleware-system.md:134` 仅列 MCP before_agent/before_model 和 ToolSearch before_agent，遗漏 Dynamic MCP 实例。
- 证据：`peri-middlewares/src/assembly/mcp.rs:20` 在 MCP 槽位展开 DynamicMcpMiddleware；`peri-middlewares/src/mcp/middleware.rs:892` 有启动闸门；`peri-middlewares/src/tool_search/middleware.rs:164` 有目录重绑 hook。
- 影响/建议：无法解释真实准入与 Reason 目录更新。区分槽位和实例，稳定职责链接具体符号，不继续复制易过时的完整 inventory。

### P3-10. 工具“无状态、并发互不干扰”的普遍承诺不成立（B21）

- 处理状态：保留，未处理；P3 不在本次追加授权范围。
- 类型：文档/职责/路由漂移；按适用契约修正文档，不因此改变已批准目标。
- 文档：`docs/design/tool-system.md:10` 对所有工具作无可变状态和并发独立保证。
- 证据：`peri-middlewares/src/tools/todo.rs:45` 持有 Arc/Mutex TodoState，`:201` 全量更新列表；`peri-middlewares/src/middleware/todo.rs:56` 将同源状态交给工具。
- 影响/建议：同步锁不能证明业务状态互不干扰。写清无状态工具、session/provider 状态引用和覆盖型并发语义；**本项不证明跨 session 状态泄漏**。

## 初始扫描检查与局限

- 本地 Markdown 链接检查：扫描 33 份 `docs/design/*.md`，解析 133 个本地 Markdown 链接，目标文件缺失 0。仅检查目标路径存在，不检查 heading anchor、代码标记中的源码路径或语义有效性；因此不与 B6 的失效代码路由矛盾。
- `bash scripts/check-layer-imports.sh`：22 条规则，违规边 0。通过仅证明当前门禁未报告违规，不证明总览图、规则覆盖或运行时职责完全正确。
- 初始扫描未运行 Cargo/SDK 测试、E2E、真实 Turso、WASM/远端接管复现或性能采样，未读取用户数据库。扫描条目中的测试引用是源码证据；追加实施的实际测试结果单列于本轮验证表。
- 已核对历史加载 P0：load 只恢复历史、冻结上下文和正常续聊，不默认复活旧 execution；可靠交付、未知副作用和 owner 对账责任仍保留。
- 不计入漂移：明确标注目标/待重构的 RCRA、异步任务、MCP part-1、Command 其余目标、Workbench/SubAgent 未验收项、托管 Workers/跨实例接管，以及 schema17 剩余性能工作。
- 初始扫描仅新增本 active issue；追加实施只修改本任务源码、测试与文档，保留已有其他工作树变更。静态定点核查不等于全量安全审计或全部设计正确。

## 剩余范围与关闭条件

1. **P1 已处理**：A1 缺少审批上下文时拒绝，定向回归通过；不等同于安全恢复宿主重跑功能，后者需另行接入 canonical 审批。
2. **P2 实现保留**：A2 保留策略接线与 A3 Workflow 日志丢失未修改，仍需行为回归，不能通过降低契约消除差异。
3. **P3 保留**：局部职责、交互、算法与旧入口未处理，不在本次追加授权范围。
4. 后续获授权处理剩余项时核对实现、测试、design、standards、模块指引和 code-index；全部稳定结果归位并验收后删除本过程文档，历史由 Git 保留。本报告当前不关闭。
