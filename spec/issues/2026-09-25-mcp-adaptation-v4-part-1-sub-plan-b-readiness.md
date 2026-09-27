# MCP adaptation v4-part-1 — sub-plan B：System MCP 启动准入

> **主 plan v2 覆盖（优先于本文）**：见 [`2026-09-25-mcp-adaptation-v4-part-1-plan.md`](2026-09-25-mcp-adaptation-v4-part-1-plan.md) §5。本文与主 plan 冲突处一律以主 plan 为准，涉及本文件的具体覆盖：**R6（最重要，推翻本文 B-04）** — 不再把 `peri-agent/src/agent/stages/mod.rs:881-886` 的 `before_agent` Err 从 warn 降级改为全局传播（全仓库 6 个既有中间件的 `before_agent` 可返回 Err，其中多属允许的软失败），改为新增专用启动闸门 hook `before_react_start` + `StartupState`（主 plan §3 IF-M4）；**R1**（`initialize.rs` 由 A-02b 先完成再移交 B-02）、**R2**（4 处 `unwrap_or_default()` 的修复归 B-02）、**R12**（B-04 依赖 B-05，v1 顺序反了）。本文的 `DiscoveryEvidence` 需求由主 plan IF-M3 冻结为硬要求。

## 1. 元信息

- 日期：2026-09-25；状态：**实现规划，未实施、未运行 Cargo 验证**。
- 主责验收契约 **2**；衔接 A 的契约 1、C 的契约 3/4，遵守契约 7。整个 workflow 只实施 **1–4、7**，不是五个 MCP 实例真实迁移。
- 权威目标：`docs/design/mcp-adaptation-v4-part-1.md:33-65,222-234`，已完整读取 244 行。以下行号为侦察时代码基线，不表示目标已实现。
- 依赖 A：严格配置解析、可信且完整的 System MCP requirement 集合、加载失败不得当作空配置；新增 timeout 字段由 A 持有。依赖 C：`build_typed_tool_bridges`、`prepare_system_tools`、`SystemToolError`，接口见 `spec/issues/2026-09-25-mcp-adaptation-v4-part-1-sub-plan-c-injection.md:68-164`。
- 可验证产出：首次有效 Receive 后存在 fail-closed 屏障；transport/协商/tools discovery/必需工具检查未完成，不放行 Compact/Reason/Act；失败或超时成为 prompt fatal error；普通 MCP 不成为连接等待条件；首个真实 Reason 使用已验证工具快照。
- **必须改 `peri-agent`。** 首批 `before_agent` 当前吞掉错误，且工具 catalog 在该 hook 之前构造；单改 McpMiddleware 不足以交付。
- **B 独占 `peri-middlewares/src/mcp/middleware.rs`；C 不拥有该文件，只提供函数，由 B 接线。** B 还拥有任务表列出的 Agent loop、middleware 接口/runner、catalog 文件。A/C 不得同时编辑；新增配置字段导致 B 文件中的 literal 变化也由 B 合并。
- 本次唯一写入物为本文；所有验证命令都是未来实施后的命令，未执行。

## 2. 「1R 阶段」定位论证

### 2.1 生命周期与候选调用链

`peri-agent/src/middleware/trait.rs:16-52` 列出 session `on_session_start/end`、输入 `on_user_prompt`、初始化 `before_agent`、每批输入 `before_input`、每次模型调用 `before_model/after_model`、工具 batch/tool、`after_agent`、compact、通知与 error hooks。补充定义：`first_turn_reminder` 在 `:78-93`，`before_reason_catalog` 在 `:145-154`，`on_session_start` 在 `:224-231`。

| 候选 hook | 实际证据 | 判断 |
| --- | --- | --- |
| `on_session_start` | trait 注释称 session/new 后、loop 前（`trait.rs:226`）；chain `chain.rs:240-249` 用 `?`。但全仓库检索 `run_on_session_start(` 只有此定义，没有生产调用；middlewares 中也未找到该 hook 实现 | **排除**。不能把注释当作执行证据；不是“证实会日志降级”，而是当前没有可依赖的生产调用链。补接会引入 session/new/load/resume/subagent 生命周期工作 |
| `first_turn_reminder` | chain `chain.rs:280-292` 用 `?`；executor `session/exec/executor_helpers/v2_execute.rs:369-391` 捕获 Err 仅 warn；只在首个模型可见用户 turn 触发 | **排除**。错误被吞、已有历史与 continuation 不覆盖，语义是通知而非准入 |
| 首批 `before_agent` | `agent/stages/mod.rs:874-894` 在首次有效 Receive 后、Compact 前；chain `chain.rs:68-78` 对每个 middleware 交错调用 `before_agent → before_input`；runner `middleware_runner.rs:62-82` 原样返回 Result，Err 也 reconcile | **选定，但必须修正 loop 对 Err 的消费**。当前 `mod.rs:885` 仅 warn，然后仍进入 Compact/Reason |
| `before_input` | 首批跟随各 before_agent；后续非空批次由 runner `:84-98` 调用，`mod.rs:887-894` 错误会终止 | 排除主门禁：首批仍被同一 warn 分支吞掉；每批输入也不是一次初始化语义 |
| `before_reason_catalog` / `before_model` | `reason.rs:22-40`：refresh → map swap → catalog hook `?` → model hook `?` → pin；`mod.rs:911-923` 传播 Reason stage 错误 | 能阻止 LLM，但已进入 Compact/Reason，晚于第一 Receive 启动边界。仅保留目录重绑/运行中防御，不代替主门禁 |

**结论：“1R”落实为 `run_react_loop` 首次有效 Receive 后、进入 Compact 前，由链内 `McpMiddleware::before_agent` 执行的启动准入屏障。** 这不是“尚未调用 run_react_loop 函数”：Receive 先接纳输入才符合当前 RCRA。等待期间可以已经发 TurnStarted、消费输入，但不能进入 Compact、Reason、Act 或调用模型。空队列/已取消/no-op keepgoing 不产生可启动模型工作，无须准入。

### 2.2 首批语义，不是每个 ReAct iteration

- 每次 `run_react_loop` 新建 `LoopState::default()`（`stages/mod.rs:677-680`）；`:879-880` 的 `before_agent_has_run` 限制一次，后续仅 before_input。它是**每次 loop 执行的首批**，不是每个 iteration，也不是 session 永久仅一次。
- trait `:66-75`、chain `:68-88`、`stages_test.rs:1048-1177,1186-1230` 相互印证。`mcp/middleware.rs:427` 的“每轮”注释须改成“每次 Agent 执行的首批”。
- 当前文档没有 ARC-MW-001 编号；用户引述内容实际在 **ARC-MIDDLEWARE-CAPABILITY-001**（`docs/standards/architecture-contracts.md:116-120`）：首批交错初始化/输入准备、后续仅输入准备、不可持 guard 跨外部 await。不得为 MCP 重排 `session/factory.rs:95` 的 production blueprint。

### 2.3 Err 如何让整个 prompt 失败

现有执行链：ACP `host/prompt.rs:579-590` 注册 PromptHandle → `host/prompt_handle.rs:62` 调 `run_session_loop` → Agent `session/exec/executor.rs:217` → `v2_execute.rs:350-391` 入队输入/reminder，`:397-418` 发 TurnStarted 并调 loop → `Receive → before_agent → Compact → Reason → Act`（`stages/mod.rs:672-677,874-923`）。

B 修改 `stages/mod.rs:879-886`：首批 runner Err 与后续输入 Err 一样，`Interrupted → LoopResult::Interrupted`，其它 → `LoopResult::Error(error)`；初始化与目录提交成功后才置 flag。不能只改 chain/runner，它们已返回 Err；不按 middleware 名称字符串特判 MCP，也不新增 fail-open 开关。

后续现有投影可复用：

1. `classify_loop_terminal`（`v2_execute.rs:635-705`）：MiddlewareError 为 fatal，`ok=false`、`failure=Some`、`TurnStatus::Error`。当前通用 fatal 的 `TurnErrorKind` 是 **LlmFailure**（`:704`），本次不借机重做这个遗留分类。
2. `ExecutionFailure::from_agent_error`（`peri-acp-types/src/session/execution.rs:92-122`）将它投影为 `kind=Internal`；`AgentError::user_facing_message` 对 MiddlewareError 走 `other.to_string()`（`error.rs:267-306`）。所以安全、明确的文案必须在 MCP 边界构造，ACP 不会替任意 MCP cause 自动脱敏。
3. `v2_execute.rs:543-591` 用同一 safe public message 发 `AgentExecutionFailed`，再唯一 TurnEnded，保留 drain/done 收尾。
4. ACP `host/prompt.rs:613-649` 先采纳历史/清理 cancel token，再 `prompt_wire_response`；`:122-128` 对 failure=Some 返回 Err；`:39,59-109` 为 **JSON-RPC `-32000`，`data={"kind":"internal"}`**，无 status/内部 cause。
5. ARC-EVENT-001（`docs/standards/architecture-contracts.md:48-51`）：canonical 用户错误是 **session/prompt error response**。`AgentExecutionFailed` 仅 capability-gated 兼容事件，不能以 TUI 通知代替失败响应。

### 2.4 必要的目录发布配套

- `session/exec/stage_builder.rs:456-466` 在 1R 前完成 collect_tools、build_session_tool_view、register_tool_catalog、StageContext。
- before_agent 可访问 local_tools（`middleware/capabilities.rs:51-67`），但只改 working map 会被 `reason.rs:23-30` 的 refresh/swap 覆盖。
- `SessionToolCatalog.base_tools` 当前不可变（`session/tool_catalog.rs:98-103`），refresh 按 dynamic generation 短路（`:181-195`）。B 必须增加明确的准入后静态 MCP 更新接口，不可盼 collect_tools 自动重跑。
- 选择默认无贡献的同步 `startup_tool_update` hook：只返回本次准入的整批静态 MCP bridge；不重跑整链 collect、不覆盖非 MCP 有状态工具。runner 在首批整链成功/reconcile 后原子更新 catalog 与 working map；ToolSearch 仍在原 before_reason_catalog 重绑。
- 动态冲突目录也需同步：`stage_builder/tools.rs:48-60` 在初始装配注册，而 `mcp/dynamic/registry.rs:112-132` 对重复 register 是 no-op。B-06 必须重验/更新晚到静态工具的碰撞目录，不能以旧目录接受冲突 load。C 不承担此宿主接线。

## 3. 事实基线

### 3.1 全量状态机与 ready 证据缺口

| 层 | 全量状态 | file:line |
| --- | --- | --- |
| ClientStatus | `Connected`、`Failed(String)`、`Disconnected`、`Disabled`、`Uninitialized`；**无 Connecting/Reconnecting 变体** | `peri-middlewares/src/mcp/client/types.rs:11-20`，标签穷尽映射 `client/status.rs:34-41` |
| McpInitStatus | `Pending`、`Initializing { connected,total }`、`Ready { total }`、`Failed(String)` | `client/types.rs:22-29` |
| OAuthStatus | `None`、`Authorized`、`NeedsAuthorization`，授权状态不等于 ready | `client/types.rs:31-41`；`status.rs:119-152` 的 NeedsAuthorization 同时写 ClientStatus::Failed |
| pool lifecycle | `0=Open → 1=Closing → 2=Closed`；Closing 禁任务与连接提交 | `client.rs:51-53`；`lifecycle.rs:166-220,329-332` |
| service wrapper | `Default`、`Channel`、`Shared` 是运行服务容器；`Closing`、`Closed` 无有效 peer；test-only `Controlled` 无协议 peer | `service.rs:65-93,140-193` |
| task owner / task | owner `Open/Closing/Closed`；task `Running/Stopping/Finished`；不是协议状态 | `task_scope.rs:72-84` |

转换和关键事实：

1. new_pending 是空 clients/configs + Pending + initialized=false（`client.rs:106-148`）。**空 map 不证明“无 System MCP”**，配置在 async 初始化内才装载。
2. initialize_config 先注册 configs、置 Initializing（`initialize.rs:82-94`），当前按配置串行连接（`:96-97`）。禁用→Disabled（`:98-118`）；transport 构造/启动、握手失败/超时→Failed（`:120-160,270-290`）。连接中的无 handle server 被 `all_server_infos` 合成为 Uninitialized（`status.rs:183-231`）。
3. `serve_client_auto` 成功才表示 rmcp lifecycle 完成。缺省 Auto，显式 2026-07-28 用 Discover、不回退 legacy（`client/transport.rs:13-56`）；client capabilities 在 `service.rs:199-211`，server 协商结果在 `peer.peer_info()`。
4. 初次连接 `initialize.rs:218-225` **tools/list、resources/list 错误 unwrap_or_default**，随后 `:240-258` 仍 commit Connected。test-only initialize 同样退化（`:490-529`）。所以 Connected 不能证明 discovery 或必需工具校验成功。
5. tools/list 清单存 `McpClientHandle.tools: Vec<rmcp::model::Tool>`（`types.rs:83-103`）；`get_tools` missing 返回空列表（`client.rs:181-187`），不能作完成证据。`list_all_tools_cached` 按协商 cache-version 返回缓存或 `peer.list_all_tools()`（`client/cache.rs:254-285`），没有本层整体 timeout。
6. 全部待 OAuth 可 pool Ready{0}；部分失败但至少一台成功也可 Ready（`initialize.rs:295-335`）。mark_initialized 只启通知（`status.rs:240-275`）。`client.rs:251-267` snapshot 将它映射 initPhase=ready；这不是 System ready。
7. reconnect 先关闭旧 service 再删 handle（`reconnect.rs:43-61`），没有 Reconnecting；窗口可能先见旧 Connected、后合成 Uninitialized。tools/list 用 `?`（`:213-219`）但失败可能仅返回 Err，没有 Failed handle；成功 commit 新 Arc（`:230-255`）。真实文件是 `mcp/reconnect.rs`，不存在 `client/reconnect.rs`。
8. OAuth 是独立提交路径：工具发现错误返回，成功 commit（`client_oauth.rs:175-229`）。remove 删除 handle/config（`lifecycle.rs:116-127`）；disable 替换 Disabled（`:129-164`）；shutdown 置 Disconnected 并清 peer（`:224-237`）。所有路径都需使 readiness evidence 失效/更新。

### 3.2 McpMiddleware 现有语义

- collect_tools（`middleware.rs:366-385`）从 **deployment tool_pool** 同步构建 bridge，再加 resource/discover；projection pool 不等价于 tool_pool（`:31-37,64-69`；`assembly/mcp.rs:26-70`）。
- first_turn_reminder（`:390-425`）仅 connection summary 入队，不等待。
- before_agent（`:439-444`）仅 ensure_discovery 后 Ok；这是 Skills/commands 发现，不是 initialize/tools/list 准入；其执行体按 handle identity 去重/spawn（`:113-178`）。
- before_model（`:449-454`）仅 drain 状态通知。
- prewarm_discovery（`:204-217`）供 session/new 预热，未连接 server 空跑；attach_connection_notifier（`:219-259`）是覆盖式单 notifier，以连接文本触发发现，弱引用 pool。**不得复用 UI notifier 作可靠 readiness watch**。
- McpTaskOwner 只提供 keyed 准入/abort/join：key `task_scope.rs:30-40`，spawn `:272-323`，stop `:326-351`；Finished 不是协商成功证据。

### 3.3 timeout 与配置

真实 McpServerConfig 定义在 `peri-acp-types/src/plugin.rs:41-77`，`mcp/config.rs:9-12` 仅 re-export。已有 command/args/env/url/headers/oauth/disabled/protocolVersion/subscriptions/source；**没有连接 timeout 或 System ready timeout 配置**。protocolVersion 只选 lifecycle。

检索整个 mcp 目录的 `Duration::from_secs` 后确认：

| 值 | 用途 | file:line |
| --- | --- | --- |
| 10s / 30s | stdio / HTTP connect | `client.rs:102-103`；`initialize.rs:128-135`；`reconnect.rs:69-76` |
| 5s | service/process shutdown | `client.rs:104`；`lifecycle.rs:29,79-83` |
| 120s | tool call、resource read、OAuth callback | `tool_bridge.rs:46`；`resource_tool.rs:40`；`callback_server.rs:10,43-52` |
| 30s | agent resource read、skill resource read / skills list | `agent_registry.rs:17`；`skill_discovery.rs:77,81` |
| 30s | Dynamic MCP drain | `dynamic/registry.rs:80` |
| 300s / 120s | Apps binding/raw-result TTL，不是启动 timeout | `apps.rs:17-18` |
| shutdown + 1s | staged cleanup | `dynamic/staged_connection.rs:259` |
| 1/2/4s，3 次 | subscription retry backoff，不是 readiness | `client/subscription.rs:67-69,193-201` |
| 2s / 5s / 60s | 测试保护时间或 cache fixture TTL | `client/transport_test.rs:49,83,146`；`client/service_test.rs:80,98,104,136`；`client_test.rs:581`；`resource_cache_test.rs:19` |

没有可复用的 pool“等待全部依赖 ready”的配置上限；McpTaskOwner shutdown 等待 tracker join（`task_scope.rs:208-219`）也不是 startup timeout。

**决策：新增每个 server 的 `system_mcp_timeout: Option<u64>`，JSON key 同名，单位毫秒，缺省 30000，合法范围 1..=600000，由 A 负责 serde/验证/透传。** 现有 transport 10s/30s 保留；启动外层从 1R 入场统一计时，不在 initialize/list/C 校验之间重置。多个 system 并发等待各自 deadline，不能串行相加。manifest 未知时用 30000ms bootstrap 上限；Loaded 且 system 集合为空立即通过，不等普通 transport。manifest 的 Loaded 不是 System Ready。

### 3.4 测试脚手架

- `middleware_test.rs:12-52,64-100,395-475` 手工插 Connected + peer=None，适合通知/负向状态测试，**不能证明 System ready**。
- `client/service_test.rs:31-85` 使用 duplex + JSON-RPC 假 server + serve_client_auto 得到真实 RunningService；DelayedClose（`:9-29`）仅延迟 close。可同样用 oneshot/Notify 闸住 initialize/list。
- `client/transport_test.rs:5-55,60-92` 验证 Auto fallback 与显式版本不回退，覆盖两种 handler。
- `initialize_test.rs:3-25` 有 Node stdio 假 server；`:63-95` 直接注入合并配置和真实 owner/spawner，不读取用户配置。新增 transport 层首选 Rust 内存 fixture；完整宿主装配复用 Node fixture或 loopback HTTP，明确 Node 前置条件。
- Agent `stages_test.rs:9-18,1186-1230` 可构造真实 Session/StageContext、计数 LLM/hook；ACP `host/executor_flow_test.rs:1-8,83,1450-1485` 有完整装配、sink 与 failure/terminal/done 顺序断言。
- 已有 futures/tokio/parking_lot/rmcp（`peri-middlewares/Cargo.toml:14-23,34-43`）；Agent dev tokio 有 test-util（`peri-agent/Cargo.toml:33-35`）。不增生产库；middlewares 单包测试不能假设启用了 paused clock，使用明确闸门与短 timeout。

## 4. 接口冻结

以下为拟新增 API，不是现存实现。A/C 开工前须确认 cross-plan 接口，不能并行编辑共享文件。

### 4.1 A ↔ B：配置清单

- A 提供 `system_mcp`、`system_mcp_tools` 与上述 `system_mcp_timeout`，保留原始 server identity，配置错误不退化空集合。
- B 的 pool manifest 状态为 `Pending / Loaded / Failed`，完整 configs 一次性发布后才 Loaded。run_initialize 消费 A 的严格 Result，失败写 Failed 并唤醒 waiter。
- A 拥有 plugin/config 类型及 loader；B 拥有 initialize 调用点。`config.rs:83-85` 的 unwrap_or_default 与 `:238-245` 合并时 warn/empty 必须由 A消除 system 相关静默退化，否则本契约不可成立。

### 4.2 连接/协商完成接口

新增 `mcp/client/readiness.rs`，由 B 的 client.rs 声明/re-export，不占 C 的 mcp/mod.rs：

```rust
pub(crate) struct SystemMcpRequirement {
    pub server: String,
    pub required_tools: Vec<String>,
    pub timeout: std::time::Duration,
}
pub(crate) struct NegotiatedSystemMcp {
    pub requirement: SystemMcpRequirement,
    pub handle: std::sync::Arc<McpClientHandle>,
    pub generation: u64,
}
impl McpClientPool {
    pub(crate) async fn await_system_connections(
        self: &std::sync::Arc<Self>,
        cancel: &peri_agent::agent::AgentCancellationToken,
        started_at: tokio::time::Instant,
    ) -> Result<Vec<NegotiatedSystemMcp>, SystemReadinessError>;
}
```

返回 handle.tools 即完整工具清单；成功只表示 transport + rmcp lifecycle + 能力信息 + tools/list 完成，**还不是 System ready**。要求本 generation 的发现成功记录、有效 peer/peer_info、未关闭 service、当前 Arc identity；拒绝旧句柄与 peer=None。证据仅由成功生产提交路径生成，不能用任务 Finished/空 tools 猜测。

采用独立 watch revision：先 subscribe 再检查，变化后重读；不使用 UI notifier，不持 parking_lot guard 跨 await。initialize/reconnect/OAuth/disable/remove/shutdown 都更新证据/revision。已知失败立即 Err；未知/连接中只等待到 deadline；不主动弹 OAuth、不自动跳过或重试。

System tools/list 使用本次 live `peer.list_all_tools()`，不以历史 cache 代替启动健康证据；required=[] 仍完成此 round-trip，清单可为空。普通 MCP 保持 cache 策略。必要健康检查限定为 lifecycle、peer 存活、成功 list；不额外强制 ping/resources/skills/subscription 或执行有副作用工具。

### 4.3 B ↔ C：最终检查与 candidate

定义在 middleware.rs：

```rust
pub(crate) struct SystemReadySnapshot {
    pub negotiated: Vec<NegotiatedSystemMcp>,
    pub bridges: Vec<super::tool_bridge::McpToolBridge>,
}
impl McpMiddleware {
    pub(crate) async fn await_system_ready(
        &self,
    ) -> Result<SystemReadySnapshot, SystemReadinessError>;
}
```

该函数完成全部 MCP/必需工具检查，返回**待目录提交的 candidate**；不发外部 ready。执行顺序：

1. before_agent 最前捕获 started_at，调用 tool_pool.await_system_connections，不以 projection pool 作依赖事实源。
2. 用 C 的 `build_typed_tool_bridges(&self.tool_pool)` 构建整批静态 bridge；`prepare_system_tools(typed, &required)`，其中 required 为 `BTreeMap<String, Vec<String>>`，来自 negotiated requirement，包含空数组。
3. 构建前后核对 system Arc/generation、open、cancel/deadline；代际变化返回 ConnectionChanged，不无限重试。C 同步检查返回后也比较 deadline，防止未 yield 的校验漏掉超时。
4. C 返回整批静态 bridges（required direct、普通 deferred），不能再 extend 旧 bridge；所有检查成功才保存 candidate，失败不保存部分结果。C 读取清单事实为同代 `NegotiatedSystemMcp.handle.tools`，不再次通过 get_tools 猜测完成。
5. 无 system 时直接走普通 collect_tools，不产生 startup update。每个新 loop 重验，不把整个 session 永久缓存为 ready；普通工具仍保留每个 turn 的收集机会。
6. C 接口保持：`prepare_system_tools(Vec<McpToolBridge>, &BTreeMap<String, Vec<String>>) -> Result<Vec<McpToolBridge>, SystemToolError>`。C 不负责网络等待，B 接线调用并包装 RequiredTools。

### 4.4 错误类型与安全文案

在 client/readiness.rs 定义 crate 可见 thiserror 类型：

```rust
pub(crate) enum SystemReadinessError {
    ConfigurationUnavailable,
    ConfigurationFailed,
    PoolClosed,
    Disabled { server: String },
    AuthorizationRequired { server: String },
    ConnectionFailed { server: String },
    NegotiationIncomplete { server: String },
    ToolDiscoveryFailed { server: String },
    ConnectionChanged { server: String },
    Timeout { server: String, timeout_ms: u64 },
    Cancelled,
    RequiredTools { source: crate::mcp::system_tools::SystemToolError },
    CatalogPublicationFailed,
}
```

| 变体 | 稳定 Display |
| --- | --- |
| ConfigurationUnavailable | `System MCP 启动失败：配置清单在 30000ms 内未就绪` |
| ConfigurationFailed | `System MCP 启动失败：配置加载或校验失败` |
| PoolClosed | `System MCP 启动失败：连接池正在关闭或已关闭` |
| Disabled | `System MCP "{server}" 启动失败：服务器已禁用` |
| AuthorizationRequired | `System MCP "{server}" 启动失败：需要完成授权` |
| ConnectionFailed | `System MCP "{server}" 启动失败：transport 或协议初始化失败` |
| NegotiationIncomplete | `System MCP "{server}" 启动失败：缺少有效协议协商证据` |
| ToolDiscoveryFailed | `System MCP "{server}" 启动失败：tools/list 失败` |
| ConnectionChanged | `System MCP "{server}" 启动失败：连接代际已变化，请重试本次输入` |
| Timeout | `System MCP "{server}" 启动超时（{timeout_ms}ms），未发布 ready` |
| Cancelled | `System MCP 启动已取消` |
| RequiredTools | `System MCP 启动失败：{source}`，source 为 C 固定模板 |
| CatalogPublicationFailed | `System MCP 启动失败：工具目录发布被拒绝，未发布 ready` |

保留 C 的 source 链；server/tool 展示必须转义控制字符、限长并清洗敏感形态，不输出 env/headers/URL/协议 payload/schema 默认值。底层失败只保留阶段/安全类别，不把 Failed(String) 原文嵌入 reason，更不记录未经清洗的 cause。

Middleware 边界：Cancelled → `AgentError::Interrupted`；其它 → `AgentError::MiddlewareError { middleware: "McpMiddleware".into(), reason: safe_display }`。可见前缀为 `Middleware error: McpMiddleware - ...`（`peri-acp-types/src/error.rs:40-41`）。timeout 不是 cancel，不可映射 Interrupted。

### 4.5 Agent 目录提交接口

Agent 不导入 MCP concrete 类型。新增 DTO 定义于 `session/tool_catalog.rs`：

```rust
pub struct StartupToolUpdate {
    pub tools: Vec<std::sync::Arc<dyn BaseTool>>,
    pub required_names: Vec<String>,
}
// Middleware trait，默认 None。
fn startup_tool_update(&self) -> Option<StartupToolUpdate>;
// 提交成功后收口准入事实；默认 Ok(())，失败仍阻止后续 stage。
fn on_startup_tools_committed(&self) -> AgentResult<()>;
// 本次失败时清除候选；默认空实现，不发布 ready。
fn discard_startup_tool_update(&self);
// MiddlewareChain，对上述接口按链序转发。
pub(crate) fn collect_startup_tool_updates(&self) -> Vec<StartupToolUpdate>;
pub(crate) fn run_on_startup_tools_committed(&self) -> AgentResult<()>;
pub(crate) fn discard_startup_tool_updates(&self);
// SessionToolCatalog，同步、fallible、先验证后原子提交。
pub fn replace_static_mcp_tools(
    &self,
    update: StartupToolUpdate,
) -> Result<std::sync::Arc<SessionToolCatalogSnapshot>, CatalogRefreshError>;
```

McpMiddleware 仅有 candidate 时返回整批 prepared bridge + required effective names；无 system 返回 None。runner 在首批整链成功/reconcile 后：合并更新（禁止重复 provider 的冲突目标）→ catalog 提交 → working map 替换 → committed 回调 → 返回 Ok。任何失败 discard candidate、返回 Err，不进入 Compact。committed 回调再次检查 deadline/cancel/generation，并记录本次准入；若此时失败，该 turn 永不启动，已提交的内部 catalog 随本次 StageContext 丢弃，不发 ready、不写宿主共享工具表。

catalog 使用一个内部状态锁同步 base/published；先验证来源/alias/原 tool_filter/dynamic overlay/required_names，再提交，不能先改 base 后失败。StaticMcp 替换不得覆盖 core/middleware 或已发布动态身份；必需项被策略过滤或动态遮蔽则拒绝，不提升权限。子 agent 的 tool_filter 事实源见 `session/subagent/v2_bridge.rs:260-271`。

`CatalogRefreshError` 新增 `InvalidStartupSource`、`RequiredToolUnavailable`、`StartupRegistrationRejected`，固定安全文案；runner 映射为前述目录发布 MiddlewareError。动态碰撞注册以 catalog 构造时注入的同步回调接入：

```rust
pub type StartupCatalogRegistration = std::sync::Arc<
    dyn Fn(Vec<peri_acp_types::dynamic_mcp::DynamicMcpCatalogTool>)
        -> Result<(), CatalogRefreshError> + Send + Sync
>;
```

由 stage_builder/tools.rs 捕获已有 deployment/session_id，调用 register_catalog；registry 锁内验证现有 live entries 和候选 static catalog，再替换，occupied 不再直接 no-op。catalog 完成本地所有 fallible 校验后调用注册回调，回调成功后的本地提交不再失败；锁序 catalog→registry，回调不得重入 catalog。无回调的子 agent 沿用 session capability 与自身 filter，不改父 session 的注册目录。

### 4.6 ready 真值表与发布边界

| 状态/证据 | 协商/发现成功 | 本次 System ready | 动作 |
| --- | --- | --- | --- |
| manifest Pending/未知 | 否 | 否 | 有界等待 manifest；错误不能当空集合 |
| Loaded，无 system=true | 不要求 | 不建立 System ready；准入可通过 | 立即继续，普通 pending/failed 不阻塞 |
| system 无 handle/Uninitialized/connecting/reconnecting | 否 | 否 | 等待或已知失败 Err，绝不默认 true |
| Failed，含 initialize 错误 | 否 | 否 | ConnectionFailed/ToolDiscoveryFailed |
| Failed + NeedsAuthorization | 否 | 否 | AuthorizationRequired |
| Disabled | 否 | 否 | Disabled，不跳过 |
| Disconnected | 否 | 否 | 已知显式 reconnect 中等新代；否则 ConnectionFailed |
| Connected，但无有效 peer/info/本代成功记录，或 service 关闭 | 否 | 否 | 等待尚未完成 discovery 或 NegotiationIncomplete，不能信 status |
| 协商成功、tools/list 未结束 | 否 | 否 | 等待到 deadline |
| tools/list Err/解析错误 | 否 | 否 | ToolDiscoveryFailed，不转空 Vec |
| list 成功，必需工具检查未完成 | 是 | 否 | 调 C，不发布 |
| C 缺工具/schema/visibility/collision Err | 是 | 否 | RequiredTools |
| required=[]，list 成功 | 是 | catalog 提交且最终复核后是 | 不增加 direct 工具 |
| C 成功、catalog/策略/冲突注册拒绝 | 是 | 否 | CatalogPublicationFailed，无部分发布 |
| 所有依赖同代、open、未超时/取消，C+catalog+最终复核成功 | 是 | 是 | 放行后续 stage |
| pool Closing/Closed、代际变更、deadline/cancel | 否或失效 | 否 | 明确错误/Interrupted |
| 仅 McpInitStatus::Ready 或 initialized=true | 未知 | 否 | 不是准入证据 |

ready 的线性化点为目录提交后 committed 回调的最终复核。连接 Connected 通知只能表示连接，不能改名当 ready。含 System MCP 的 legacy aggregate `McpInitStatus::Ready` 不得早于依赖验证；保留 discovery 完成与准入完成两个事实，仅两者满足才更新 pool.init_status **和外部 status_tx**。普通-only 的原面板语义保留。已失败/超时的 prompt 不得被迟到后台成功翻成成功；之后的新 prompt 可以重新准入。全局连接显示与每次 prompt 准入不可相互替代。

## 5. 任务表

任务间文件不重叠；每行含该任务生产与测试文件。估算是未来实现规模。B-01/02/03 与 B-04/05/06 按依赖串行集成，不能局部绿色就宣告完成。

| Task ID | 标题 | 目标文件 | 改动摘要 | 验证命令（未来） | 预估 diff 规模 | 文件冲突面 |
| --- | --- | --- | --- | --- | --- | --- |
| B-01 | 连接证据与等待 | `peri-middlewares/src/mcp/client.rs`；`client/readiness.rs`、`client/readiness_test.rs`（新增）；`client/lifecycle.rs`；`client/status.rs` | manifest/发现证据、watch、await_system_connections、typed error；失效/代际/open 检查；维护 status sender | `cargo test -p peri-middlewares --lib mcp::client` | 300–500 行 | B 独占，模块声明在 client.rs，不碰 C 的 mcp/mod.rs |
| B-02 | 初始/重连/OAuth 严格发现 | `peri-middlewares/src/mcp/initialize.rs`；`initialize_test.rs`；`reconnect.rs`；`client_oauth.rs` | 消费 A 严格 Result；原子 manifest；system 并发优先推进，避免普通慢连接排队；live list 错误/超时不 default；同代证据提交；test initialize 不保留第二套宽松逻辑 | `cargo test -p peri-middlewares --lib mcp::initialize`；`cargo test -p peri-middlewares --lib mcp::client` | 200–350 行 | A 不修改 initialize.rs；若原 A 已占用，父计划先移交 |
| B-03 | McpMiddleware 闸门/C 接线 | `peri-middlewares/src/mcp/middleware.rs`；`middleware_test.rs` | await_system_ready、C 调用、candidate/update/commit/discard、collect_tools 替换；保留 discovery/通知；安全错误 | `cargo test -p peri-middlewares --lib mcp::middleware` | 250–450 行 | **B 独占，C 不拥有 middleware.rs**；C 仅提供函数 |
| B-04 | 首批 Err 终止 loop | `peri-agent/src/agent/stages/mod.rs`；`stages_test.rs` | 首批 Err 返回 LoopResult，Interrupted 分类，成功才置 flag；一次/链序回归 | `cargo test -p peri-agent --lib agent::stages` | 80–150 行 | **必须改 peri-agent，B 独占** |
| B-05 | startup catalog 原子提交 | `peri-agent/src/middleware/trait.rs`；`middleware/chain.rs`；`agent/stages/middleware_runner.rs`；`middleware_runner_test.rs`；`session/tool_catalog.rs`；`tool_catalog_test.rs` | DTO/hook/runner、static base 更新、filter/alias/dynamic overlay、required 检查与 commit/discard | `cargo test -p peri-agent --lib middleware_runner`；`cargo test -p peri-agent --lib tool_catalog`；`cargo test -p peri-agent --doc` | 250–420 行 | B 拥有上述全部 Agent 文件；C 不碰，不导入 MCP concrete 类型 |
| B-06 | 动态冲突目录配套 | `peri-agent/src/session/exec/stage_builder/tools.rs`；`peri-middlewares/src/mcp/dynamic/registry.rs`；`dynamic/registry_test.rs` | 注入注册回调；重复注册重验更新而非 no-op；保持 runtime 隔离/过滤，不改实例语义 | `cargo test -p peri-middlewares --lib mcp::dynamic::registry`；`cargo test -p peri-agent --lib session::exec` | 100–180 行 | B 独占，父计划如有 dynamic 工作需串行合并 |
| B-07 | 宿主/ACP 验收 | `peri-acp/src/host/executor_flow_test.rs`；`host/prompt_test.rs` | 真 assembler + controlled transport + counting model；错误/成功工具视图、终态、wire allowlist | `cargo test -p peri-acp --lib host::executor_flow`；`cargo test -p peri-acp --lib host::prompt` | 180–300 行 | B 仅拥有这些测试；不改 ACP 生产投影，不与 C system_tools_test 重叠 |

A 单独持有 timeout serde/严格加载/合并/hash/透传与其测试；C 单独持有 tool_bridge/system_tools 与 mcp/mod.rs 模块声明。配置新字段导致上表文件中的 literal 变化由 B 处理，不能成为 A 越界修改理由。

## 6. 验证计划

本次未运行 cargo build/test/check。以下为实施后的测试；遵循 `docs/standards/testing.md:12-19`，fixture 局部定义，不建共享 mock 框架。

### 6.1 MCP transport seam

位置：`peri-middlewares/src/mcp/client/readiness_test.rs`。采用 duplex + serve_client_auto 真实 SDK，JSON-RPC server 仅替换外部依赖；oneshot 确认请求到达再控制响应，不靠 sleep 猜时序。

| 测试函数 | 可观察断言 |
| --- | --- |
| `system_ready_waits_for_initialize_and_tools_list` | 卡 initialize，再只放 initialize 卡 list；两阶段 waiter pending、无 ready；list 成功只得到 negotiated，尚非最终 ready |
| `system_ready_rejects_initialize_error` | initialize JSON-RPC error → Failed evidence/ConnectionFailed，无 Connected/ready；回归显式 Discover 不 fallback |
| `system_ready_rejects_tools_list_error_instead_of_empty` | list Err/畸形结果，空 required 也 Err，不出现默认空列表成功 |
| `system_ready_timeout_is_terminal_without_fallback` | 不响应 initialize/list，deadline 返回错误，无快照；迟到释放不能改变已失败结果 |
| `system_ready_rejects_connected_without_protocol_evidence` | 旧 Connected+peer=None fixture 被拒绝，未知状态不 ready |
| `system_ready_ignores_non_system_pending_and_failed` | manifest 已 Loaded，ordinary pending/failed 不等待；混合场景仅 system 完成即可通过连接屏障 |
| `system_ready_does_not_treat_unloaded_manifest_as_empty` | new_pending 空 map 不放行，Loaded(empty) 才通过；加载失败明确 Err |
| `system_ready_rechecks_generation_and_pool_close` | reconnect/disable/shutdown 或提交前换代，无旧 Arc ready |
| `system_ready_parallel_deadlines_do_not_accumulate` | 多个 timeout 从同一入场时间计，最早失败及时终止，不依 HashMap 顺序叠加 |

位置 `mcp/initialize_test.rs`：

- `system_initialization_is_not_queued_behind_ordinary_transport`：普通 server 卡实际 transport，system 仍收到 initialize/list 并完成；不能只手工写 Pending 代替真实并行初始化。
- `system_initialization_never_emits_aggregate_ready_before_validation`：观察实际 status/watch/notifier，list 已成功但 C/catalog 未提交，无含 system 的 Ready；ordinary-only 保持原行为。

### 6.2 必需工具与 middleware seam

位置 `mcp/middleware_test.rs`，运行真实 before_agent 与 C 函数，声明来自 runtime list。

- `system_mcp_required_tool_check_must_finish_before_ready`：握手/list 成功但缺必需工具，RequiredTools(MissingTool)→MiddlewareError；candidate/目录未 ready。不能以 Connected 阳性 fixture 代替检查。
- `system_mcp_invalid_schema_aborts_startup`：实际 list 返回非法 schema，C InvalidSchema，无部分 direct、无模型调用。
- `system_mcp_empty_required_still_waits_for_connection`：required=[] 仍等待 initialize/list；成功 direct 增量 0，失败仍 Err。
- `system_mcp_false_does_not_block_before_agent`：false/缺省各一 case，ordinary 永不响应但 before_agent 可结束，skill discovery 可异步进行。
- `system_mcp_cancelled_startup_never_publishes_ready`：取消→Interrupted，与 timeout fatal 区别；不关闭 deployment pool/其它 session 任务。
- `system_mcp_gate_uses_tool_pool_not_projected_pool`：projection 伪 Connected 而 deployment pending/failed，仍阻塞/失败；成功身份来自 tool_pool。

### 6.3 Agent RCRA 与实际工具视图 seam

位置 `peri-agent/src/agent/stages/stages_test.rs`：

- `before_agent_error_stops_before_compact_and_reason`：真实 loop 返回 MiddlewareError；Receive 已接受输入，但 Compact/Reason/Act observer 和模型计数为 0，后续 middleware 不执行。
- `before_agent_interrupted_is_not_fatal_error`：Interrupted 分类正确，模型计数 0。
- `before_agent_runs_once_per_loop_after_receive`：工具回合/后续输入不重跑初始化，保留链序与附件转换。

位置 `middleware_runner_test.rs` / `session/tool_catalog_test.rs`：

- `startup_catalog_update_survives_first_reason_refresh`：初始 catalog 无 required，提交后运行真实 run_reason，LLM 入参含 required effective name，普通仍 deferred。
- `startup_catalog_update_is_atomic_and_preserves_policy`：schema/策略/dynamic 遮蔽或注册拒绝，整批失败，旧表不部分修改、不扩大权限。
- `startup_catalog_update_keeps_non_mcp_tools_and_dynamic_generation`：Search/Execute/SubAgent 有状态对象不被覆盖，动态 overlay/generation 保持原事实源。
- `startup_commit_rechecks_deadline_and_generation`：C 成功后、catalog 提交期间超时/换代，committed 回调失败，loop 仍未进入 Compact，不发布 ready。

### 6.4 宿主与用户可观察 seam

位置 `peri-acp/src/host/executor_flow_test.rs`：

- `system_mcp_startup_failure_has_fatal_prompt_terminal`：ProductionChainAssembler + 实际 MCP fixture，initialize/list/missing-tool/timeout 矩阵；PromptResult.ok=false、failure Internal、安全明确文案、模型 0；AgentExecutionFailed→TurnEnded(Error)→done 各一次，不残留 loading。
- `system_mcp_ready_reaches_first_model_with_required_tools`：全部 gate 放行后，第一次 ModelRequest.tools 出现 required effective names，无需 ToolSearch；普通 deferred 不直接出现，空 required 不新增 direct。
- `ordinary_mcp_pending_does_not_delay_prompt_model`：ordinary 永久 pending，模型仍到达；不因 pool connecting 把普通 prompt 超时失败。

位置 `host/prompt_test.rs`：

- `system_mcp_middleware_error_projects_standard_acp_error`：通过真实 ExecutionFailure::from_agent_error 与 prompt_wire_response，断言 -32000、data.kind=internal、无 status/diagnostic/raw cause、message 含失败类别；cancel 保持成功 Cancelled response。
- `system_mcp_error_projection_contains_no_transport_secrets`：动态构造非真实凭据的危险形态，wire 不含 URL query/header/env/schema payload，不以“调用脱敏函数”代替输出断言。

**通过标准：** transport 失败矩阵、首批 Err 传播、首次 Reason catalog、宿主 prompt error 四层全部通过，才能声称验收 2 完成。仅静态表、mock Connected、单个 hook 单测或 ACP 纯投影测试都不充分。最后执行目标测试及 git diff --check，报告实际结果；局部绿色不等于五实例迁移完成。

## 7. 风险与未知

1. **首批吞 Err 已确认。** B-04 不可省略；全链初始化 Err fatal 会暴露其它 middleware 过去被掩盖的失败，要回归附件/skills/preload，不因回归压力保留 MCP fail-open。
2. **无真实 server 的测试可行。** 现有 duplex 已跑真实 rmcp lifecycle；ready=释放 initialize+list，failed=协议 error/关闭 transport，timeout=持 stream 不响应。本次新增 gated fixture 未编译；测试必须 join 假 server，并按 pool begin-close→owner abort/join→pool shutdown 清理，不留 orphan tasks。
3. **A 接口尚待确认。** 已读取 C 的规划并采用其函数/错误名；A 文档在相关侦察时尚未出现，timeout 单位/范围、严格 loader Result 是跨 plan 依赖，不是假称已经协商一致。
4. **catalog 冲突面明确扩大。** static base 不可变、dynamic 重复注册 no-op、child filter 均有证据。B-05/06 是必要配套，父计划必须分配上述文件；不能模糊写“由 C 接线”。锁序 catalog→registry，注册回调不可重入 catalog；实现时必须审查父/子与多 session 并发。
5. **ready 的层次。** legacy aggregate Ready 不是 System gate；本计划要求独立发现证据、candidate、catalog commit 和最终复核，含 system 的外部 Ready 不得提前。pool.init_status 与 status_tx 都需统一更新。已成功的其它 session 不代表当前 prompt 已准入。
6. **配置未发布窗口。** host `assemble.rs:451-464` async spawn Initialize，可能先等 activation；不能从空 pool 放行。manifest Pending 有上限；false-only 在配置 Loaded 后不等待 transport。若父计划要求连配置读取也零等待，需 A 将可信 manifest 前置到宿主构造，并另分配 host/TUI 文件；本文不假称现有接口已支持。
7. **普通 server 间接阻塞。** initialize 当前串行（`:96-97`），只做 waiter 会被普通慢连接拖住；B-02 必须优先并发推进 system。单 prompt timeout 不 abort deployment 共享初始化；旧 prompt 的迟到结果不得发布其 ready。
8. **协议与健康范围。** 不固定唯一握手版本，保留 Auto/Discover；空 required 仍要求 live list；resources/skills/subscription 不自动升级为必需条件。新增 server-specific 健康检查需要另外声明，不猜测工具、不执行副作用探活。
9. **持续健康不是本契约。** 同代启动原子发布不保证远端随后永不掉线。新 loop 重验、提交前换代拒绝、工具调用沿用既有失败路径；不以一次准入宣称长期健康。
10. **运行时核实缺口。** 本次禁止 Cargo，故新 API 可编译性、fixture 调度稳定性、真实外部 server 兼容性均待实施验证。状态/调用链结论来自源码，未将静态侦察冒充运行结果。

## 8. 非目标

- 不迁移五个真实 MCP，不新增它们的 transport/process/凭据/capability root，不宣称验收 5/6 完成。
- 不改 Permission/HITL/Hook/SubAgent/Workflow/Goal/PTC 架构，不绕过 allow/disallow、effective name 或 dynamic policy。
- 不重排 blueprint，不新接 on_session_start，不把通知 hook 当准入 hook，不新增 Agent 对 peri-middlewares 的依赖。
- 不把普通 MCP 改为阻塞启动，不把 ordinary discovery 失败升级为 system failure，不强制 OAuth UI/skills/subscription 就绪。
- 不造通用重试/健康监控框架，不用 shutdown timeout 或 protocolVersion 代替启动配置，不把未知/空清单/任务结束/旧缓存当 ready。
- 本次只写本文，不写生产/测试代码、其它文档，不运行 cargo build/test/check，不提交 git。
