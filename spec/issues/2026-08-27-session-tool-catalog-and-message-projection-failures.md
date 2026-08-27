# 长生命周期会话中的工具目录漂移、消息投影失配与失败隔离缺陷

**状态**：In Progress
**优先级**：高
**类型**：缺陷 / 生命周期一致性 / 消息投影 / 错误隔离 / 可观测性
**创建日期**：2026-08-27
**来源**：`.tmp/agent-tui.2026-08-27` 生产日志 + 源码、契约和测试复核

> 本文是本次日志事故的 umbrella active issue。Dynamic MCP panic 与 Prediction 失败都体现了运行时投影缺少稳定边界，但不存在一个能够解释全部异常的单一代码根因。MCP service shutdown、`/cron` 命名冲突等应拆为独立实施项；外部网络与 provider 故障只记录分类和诊断边界，不应进入同一个修复 PR。

## 问题摘要

日志约 80 MB、近 80 万行，覆盖多个进程/会话。去重后的问题如下。

| ID | 现象 | 分类 | 根因状态 | 影响 | 处置 |
| --- | --- | --- | --- | --- | --- |
| P1 | Dynamic MCP catalog 漂移导致 4 次 panic | 产品 bug | 已确定 | 会话后续 turn 无法启动 | P0 修复 |
| P2 | Prediction 报 `tool result has no matching tool name` | 产品 bug | 已确定 | Prediction 确定性失败 | P0 修复 |
| P3 | `RunningService dropped without explicit close()` | 生命周期缺陷 | 缺陷确定，具体 owner 分支未定 | 可能破坏 shutdown 或泄漏资源 | 独立 issue |
| P4 | builtin `/cron` 与 TUI `/cron` 冲突 | 产品命名 bug | 已确定 | TUI command 被拒绝注册 | 独立 issue |
| P5 | skill roots 重复扫描并产生大量 conflict warn | 去重/性能/观测缺陷 | 高概率，等价路径 fingerprint 待补 | 日志污染、重复扫描 | P2/独立 issue |
| P6 | 可选 `.peri/meta` 不存在被记为 WARN | 观测缺陷 | 已确定 | 无功能损害 | P2 |
| P7 | ACP/TUI/subagent 每事件成功日志洪泛 | 观测缺陷 | 已确定 | 淹没有效信号 | P2 |
| D1 | `perf.render` 占日志约 72% | 显式诊断模式 | 已确定 | 日志膨胀 | 非产品 bug；核查启动环境 |
| E1 | MCP gateway refused/timeout | 外部可达性故障 | TCP connect 失败已确定，责任主体未定 | 对应 MCP 不可用 | 非目标 |
| E2 | OpenAI-compatible stream interrupted | provider/transport 故障 | 流未正常结束已确定，外因未定 | 对应 SubAgent 失败 | 非目标；补观测 |
| E3 | WebSearch 失败 | 外部工具/API 或未知工具故障 | 错误正文不足 | 单次工具失败 | 当前降级正常 |

## P1：Dynamic MCP session baseline 在 turn 边界被重算并重复注册

### 现象与日志证据

日志实际包含两个 session、每个 session 两次相同 panic，共 4 次：

```text
dynamic MCP catalog registration must succeed before session startup:
DynamicMcpFailure {
  code: ToolNameConflict,
  phase: Failed,
  safe_summary: "Dynamic MCP session catalog changed after initialization"
}
```

第一组：

- session `01a040da-a083-7112-b176-836239e5b030`
- 首次 panic：日志 `436960-436961`
- 用户重试后再次 panic：日志 `437635`
- TUI 消费 panic：日志 `437137`

第二组：

- session `01a0413b-2f7b-7ae3-ba23-2ff8e8fcfb03`
- 首次 panic：日志 `783002`
- 重试后再次 panic：日志 `788970`
- TUI 消费 panic：日志 `783179`

第一组在 panic 前成功执行 `DynamicMCP.load`（日志 `428698`），随后 `DynamicMCP.status` 成功（日志 `430184`）；下一次用户提交后立即 panic。第二组可以确定两次 stage build 提交了不同 catalog，但现有日志没有 old/new fingerprint、server identity 或 tool diff，不能把具体变化源写死为某个 MCP server。

### 已确认根因

生产 stage 构造在每个 turn 执行：

```text
chain.collect_tools()
→ build_session_tool_view()
→ SessionToolCatalog::new()
→ dynamic_catalog_tools()
→ register_catalog(session_id, tools)
```

代码位置：

- 每 turn 收集工具并创建目录：`peri-agent/src/session/exec/stage_builder.rs:816-836`
- 每 turn 再次注册：`peri-agent/src/session/exec/stage_builder.rs:837-840`
- session 工具 merge：`peri-agent/src/session/exec/stage_builder.rs:223-256`
- 完整 `base_tools` 被转成 Dynamic MCP collision catalog：`peri-agent/src/session/tool_catalog.rs:156-175`

Registry 的实际契约却是 session initialization 一次性冻结：

- 首次保存完整 `Vec<DynamicMcpCatalogTool>`：`peri-middlewares/src/mcp/dynamic/registry.rs:1114-1118`
- 后续要求与首次逐项相等，否则返回 `ToolNameConflict`：`peri-middlewares/src/mcp/dynamic/registry.rs:1127-1137`

注册输入不是冻结配置，而是实时工具投影：

- `McpMiddleware::collect_tools()` 每次调用 `build_tool_bridges`：`peri-middlewares/src/mcp/middleware.rs:318-325`
- `build_tool_bridges` 遍历调用当下已经连接的 client/tools：`peri-middlewares/src/mcp/tool_bridge.rs:261-271`
- Dynamic MCP session projection 被用于构造当前 middleware：`peri-middlewares/src/assembly.rs:503-549`

因此 catalog 会随以下事件变化：

1. 静态 MCP 异步完成连接/discovery；
2. `DynamicMCP.load` 发布 capability；
3. `DynamicMCP.unload` 移除 capability；
4. MCP reconnect 或 tools list changed；
5. session projection 恢复或条件 middleware 改变可见工具。

第一组的确定因果链为：

```text
DynamicMCP.load
→ discovery/build_instance_tools
→ publish_capability
→ 下一 turn 重建 McpMiddleware
→ collect_tools 看见新增动态 bridge
→ dynamic_catalog_tools 得到不同 Vec
→ register_catalog 检测 existing != current
→ DynamicMcpFailure(ToolNameConflict)
→ expect(...)
→ panic
```

对应代码：

- control tool 调 deployment：`peri-middlewares/src/mcp/dynamic/tool.rs:86-109`
- load/discovery/bridge 构建：`peri-middlewares/src/mcp/dynamic/registry.rs:481-528`
- commit/publish：`peri-middlewares/src/mcp/dynamic/registry.rs:529-565`、`:682-705`
- load 阶段真实名称冲突检查：`peri-middlewares/src/mcp/dynamic/registry.rs:609-679`

本事故命中的是“catalog changed”分支，不是 load commit 的真实同名冲突。`ToolNameConflict` 被复用于两种不同语义，错误分类具有误导性。

### 爆炸半径放大因素

Registry 已返回结构化 `Result<(), DynamicMcpFailure>`，但 `stage_builder.rs:837-840` 使用 `expect`，将可处理的 stage/session build error 升级为线程 panic。首次失败后旧 baseline 仍在 registry 中，下一次提交重复生成相同的新 catalog，因此不能自愈。

### 设计约束

不能简单用新 catalog 覆盖旧 catalog。这样会让已经发布的 dynamic capability 与新 baseline 形成 TOCTOU 冲突。必须明确：

- collision baseline 在真正 session initialization 生成一次；
- dynamic capability 不进入 baseline；
- 静态 MCP discovered tools 属于稳定 baseline，还是 runtime capability；
- runtime capability 的 publish/replace/unload 在同一原子冲突仲裁边界完成；
- SubAgent 使用独立 session identity，或只读继承父 baseline，不得用父 `session_id` 注册过滤后的不同 catalog。

该问题与 `ARC-FROZEN-001` 的 session 内稳定方向一致，但现有契约只明确列举 prompt 冻结数据。实现修复时应决定是否补充“session collision baseline”稳定契约，而不能假定文档已经完整覆盖。

## P2：Prediction 固定消息窗口破坏 tool-call pairing closure

### 现象与日志证据

两次确定性失败：

```text
Prediction facade: LLM failed
error=LLM error: tool result has no matching tool name
```

第一次时间线：

- 主 turn 结束、历史 29 条：日志 `13071`
- Prediction 启动：日志 `13101`
- 从历史选择 10 条：日志 `13103`
- 本地调用开始：日志 `13105-13106`
- 同一毫秒失败：日志 `13108`
- ACP 记录 `Prediction fork failed`：日志 `13110`

第二次复现：日志 `22164-22165`。

### 已确认根因

Prediction 机械截取最近 10 条非 System 消息：

- `peri-acp/src/host/mod.rs:876-896`

它没有把以下结构作为原子组：

```text
Ai(tool_calls=[A, B])
Tool(tool_call_id=A)
Tool(tool_call_id=B)
```

窗口可能保留 `Tool(A)`，同时切掉声明 `A` 及工具名的 `Ai`。`execute_prediction` 只是原样 clone 已截取历史，再追加 directive，没有修复配对：

- `peri-agent/src/session/exec/executor/prediction.rs:53-67`

`AgentModelBridge` 在构造 provider request 时先从可见的 `Ai.tool_calls` 建立 `tool_call_id → tool name`，随后转换 `Tool`：

- 映射建立：`peri-agent/src/agent/model_bridge.rs:79-87`
- Tool 查找并报错：`peri-agent/src/agent/model_bridge.rs:109-127`

因此错误发生在 provider 请求发出前，不是网络、模型或 compact 的本次直接故障。

### 必须建立的不变量

对任意发送给 model/provider 的消息投影：

```text
∀ Tool(tool_call_id = X),
∃ Ai.tool_calls 中存在 id = X，且该 call 提供工具名
```

若一个 `Ai` 声明多个并行 tool calls，投影必须：

- 扩大窗口，保留整个 `Ai + all Tool results` 组；或
- 丢弃整个不完整组。

不得只保留部分结果，也不得伪造 `unknown` 工具名。

本次只直接证明 Prediction 的固定窗口缺陷。真正 SubAgent fork、compact、history restore 也缺少统一 closure 验收，应加入防御性测试，但不能写成此次事故的已发生根因。日志里的 “Prediction fork” 只是异步 Prediction task 命名，不是 `Agent(fork=true)`。

## P3：存在绕过显式 MCP pool shutdown 的 RunningService

日志出现：

```text
RunningService dropped without explicit close().
The connection will be closed asynchronously.
```

正常 pool shutdown 已有明确 transaction：pool begin-close → owner abort/join → pool service close，并受 `ARC-HOST-SHUTDOWN-001` 约束。该 rmcp 警告证明至少一个 `RunningService` 没有走显式 `close()`/`cancel()`；但当前日志不含 service/server/pool/deployment identity，无法安全判定具体 owner 分支。

处置：

1. 先为该警告补充 server identity、pool/deployment identity、owner state 和 close reason；
2. 枚举 pool 内、初始化失败中间态、Dynamic MCP deployment、reconnect/replace 等所有 service owner；
3. 独立建立 shutdown issue 和生命周期测试；
4. 不得把 gateway 连接失败直接当作该 warning 的代码根因。

## P4：builtin `/cron` 与 TUI `/cron` panel 命名冲突

日志重复出现：

```text
ui 条目注册冲突（拒绝，不覆盖） error=Conflict { key: "cron" }
```

根因：builtin skill `/cron` 已占用裸名，TUI Cron panel 注册 `ui:cron` 时也要求裸名 `cron`。Registry 的 fail-closed 拒绝覆盖行为正确，产品层分配了冲突名称。

影响：TUI Cron command 入口被拒绝注册。应独立决定用户可见命名/namespace，而不是削弱 registry 冲突检查。

## P5：Skill roots 重复扫描与 warning 洪泛

`peri_middlewares::skills::loader` 产生 585 条 canonical/alias conflict warning，多个 skill 各重复约 45 次。Loader 的“来源优先、后到同名跳过”行为符合既有 Skills 契约；问题是：

- 同一或等价 root 可能从用户、配置、项目、plugin 等入口重复加入；
- session/stage 重建反复扫描并反复报告预期 shadowing；
- 缺少 canonical root fingerprint，现有证据不能断言所有重复都来自完全相同路径。

修复方向：canonicalize/deduplicate roots；同一扫描周期按 `(canonical, source, conflict)` 聚合；预期 shadowing 降至 DEBUG/TRACE；保留真正不可判定冲突的 WARN。

## P6：可选 MetaHarness 目录缺失被错误提升为 WARN

日志重复出现：

```text
meta_harness: scan skipped (cannot read .peri/meta)
error=No such file or directory
```

`.peri/meta` 是可选输入，`NotFound → empty map` 是正常降级。应将 `NotFound` 静默或降为 DEBUG/TRACE；权限、损坏、非目录等其他 I/O 错误继续 WARN。

## P7：事件成功路径日志洪泛

除显式 render timing 外，主要高频 targets 包括：

- `acp.event_sink`
- `tui.acp_notifier`
- `agent.subagent_forwarder`

同一事件跨 Agent → ACP → TUI fan-out 时在多个边界逐条打 DEBUG。现有日志不能证明事件被重复处理，但证明成功路径的观测粒度过细。建议：

- 高频成功事件降为 TRACE、采样或窗口聚合；
- drop、lag、mapping failure、归属错误保留 WARN/ERROR；
- 使用 correlation fields 支持跨边界追踪，避免靠完整逐事件文本诊断。

## 独立外部故障与正常降级

### MCP gateway TCP 不可达

涉及 `ip-as-logo` 与 `openspec`，同一 gateway 在不同时段解析到不同 IP：

- `ConnectionRefused`：日志 `93`、`95` 等；
- connect timeout：日志 `426331-426332`、`717606-717607`、`728966-728967` 等；
- `Transport channel closed` 是 rmcp worker 对底层 connect error 的包装后果。

失败发生在 TCP connect 阶段，尚未进入 MCP lifecycle。因此它：

- 不是 Dynamic MCP catalog bug；
- 不是 `2026-08-27-mcp-auto-protocol-negotiation.md` 的 lifecycle 选择问题；
- 不能通过 modern/legacy fallback 修复。

仓库可改进 backoff、错误去重和用户提示，但远端 gateway、DNS、代理或网络路径责任主体不能仅凭本日志确定。

### OpenAI-compatible stream interrupted

日志中的 `model stream interrupted from openai-compatible` 发生在独立 advisor SubAgent。可确认 SSE 未到达正常 completion，不能确认是 provider、代理、网络 EOF 还是响应格式异常。收到可见 delta 后不自动重试是合理安全策略，避免重复文本或 tool call。

应补：安全的 request correlation ID、EOF/decode/HTTP body/provider termination 分类，以及与 Anthropic 路径对称的 partial-stream 测试。

### WebSearch failure

WebSearch 错误被转换为 `ToolResult(is_error=true)`，随后 Agent 继续 Reason。这是预期工具错误语义，不是 Prediction pairing 缺陷或 stream interruption 的原因。错误 ToolResult 仍必须满足 pairing closure，可作为 P2 测试数据。

### `perf.render` 日志

`perf.render` 约 51 万行、约占日志 72%。代码只有在 `PERI_RENDER_TIMING=1/true` 时启用逐帧 INFO，因此在该诊断模式下属于预期行为。应检查启动环境为何启用；默认关闭的实现本身不是 bug。若要长期采集，应另行设计聚合指标，而不是改变本 issue 的功能修复范围。

## 复现

### Dynamic MCP load 后下一 turn panic

```text
create session
→ first build_stage_context / register baseline
→ DynamicMCP.load(fake server with >= 1 tool)
→ wait Ready
→ submit next user turn
→ second build_stage_context
```

当前：`existing != current → ToolNameConflict → expect → panic`。

期望：第二轮成功，动态 capability 可见，baseline fingerprint 不变，无 panic。

### 静态 MCP 延迟 ready

```text
create session while static MCP disconnected/discovering
→ first stage build
→ static MCP publishes connected tools
→ second stage build
```

期望：不产生 baseline drift；静态工具按明确的 baseline/runtime capability 语义处理；collision arbitration 仍 fail closed。

### Prediction 窗口切入工具组

构造至少 11 条非 System 历史，让最近 10 条的第一条为：

```text
Tool(tool_call_id="call-1")
```

而对应的：

```text
Ai(tool_calls=[{ id: "call-1", name: "WebSearch" }])
```

位于窗口外。

当前：`take(10) → orphan Tool → model_bridge error`。

期望：扩窗补齐完整组，或丢弃整个不完整组；mock model 收到合法 request。

## 实施分期

### 阶段 0：先补失败测试与诊断

1. 加入当前会失败的 Dynamic MCP 多 turn 生命周期测试。
2. 加入 Prediction 窗口切入单工具/并行工具组测试。
3. catalog 注册记录安全 fingerprint、old/new count、added/removed canonical names 和 source category。
4. MCP service drop 记录 server/pool/deployment identity、owner state 和 close reason。

### 阶段 1：消除 panic，建立错误隔离

1. 移除 `stage_builder` 中的 `expect`。
2. 将 `DynamicMcpFailure` 映射成 typed stage/session build error。
3. 本轮进入明确失败终态，TUI 接收普通错误事件，不使用 `PANIC_NOTIFY`。
4. 保持 registry fail closed；不能吞掉 invariant violation。

该阶段仅降低爆炸半径，不算根因修复完成。

### 阶段 2：冻结 Dynamic MCP collision baseline

1. 在真正 session initialization 边界生成一次 baseline。
2. baseline 排除 dynamic MCP capability。
3. 保存于 session-local frozen data 或等价 owner。
4. 同一 main session 只注册一次，后续 turn 复用。
5. session close 清理 registry state。
6. 明确 SubAgent baseline identity/继承规则。
7. 明确静态 MCP 属 baseline 还是 runtime capability；后者在 publish/replace 时原子仲裁。

### 阶段 3：统一 pairing-aware message projection

1. 提供共享 selector/validator，按完整 tool-call group 投影。
2. Prediction 不再直接 `take(10)`。
3. 明确 10 条是可扩展软上限，或对不完整组整组丢弃。
4. fork、compact、history restore 复用 validator 或统一契约测试。
5. 损坏持久化数据返回结构化错误或执行明确修复策略。
6. 保留 `AgentModelBridge` 最终防御检查。

### 阶段 4：拆分独立问题

建立独立 active issues：

1. MCP orphan `RunningService` 绕过显式 shutdown；
2. builtin `/cron` 与 TUI `/cron` panel 冲突；
3. equivalent skill roots 重复扫描；
4. 可选 MetaHarness 缺失被记录为 WARN；
5. per-event success telemetry 淹没日志。

### 阶段 5：Observability 收敛

- 为 catalog drift、stream interruption 和 service owner 增加无敏感信息的结构化字段；
- 将 `Prediction fork failed` 改为 `Prediction task failed` 或 `Prediction history projection invalid`；
- 外部连接错误按 server + endpoint category + cause 聚合，保留重试/backoff 状态；
- 高频成功事件采用 TRACE/采样/聚合。

## 验收标准

### Dynamic MCP

- [ ] 同一 main session 构建 N 个 turn，baseline 注册恰好一次。
- [ ] 不同 session 各自注册一次；session close 后 catalog 被清理。
- [ ] SubAgent 不用父 `session_id` 重注册不同 catalog。
- [x] baseline 不包含 dynamic MCP capability。
- [ ] `load → Ready → next turn` 不 panic，动态工具可见且可执行。
- [ ] `load → next turn → unload → next turn` 全部成功，unload 后工具不可见。
- [ ] 静态 MCP 首轮 disconnected、次轮 ready 不产生 catalog drift。
- [ ] reconnect/tools-list-changed 按选定 capability 语义处理。
- [x] canonical、alias、ASCII case-fold 和 dynamic-vs-dynamic 真实冲突仍 fail closed。
- [x] registry error 不再经 `expect` 变为 panic；TUI 无 `PANIC_NOTIFY`。

### Message projection

- [x] Prediction 窗口不会产生 orphan `Tool`。
- [x] 单 tool call、多并行 tool calls、错误 ToolResult 均保持 closure。
- [x] 窗口同时覆盖“扩窗”或“整组丢弃”的明确预算语义。
- [ ] fork、compact、history restore 对 pairing closure 有契约测试。
- [ ] provider request 发出前 validator 能返回带 message index/tool_call_id 的结构化错误。
- [x] 不伪造未知工具名。

### 生命周期与观测

- [ ] 每个 `RunningService` 都有可识别 owner，并在正常关闭、初始化失败、replace/reconnect、Dynamic MCP unload 路径显式 close/cancel。
- [x] `/cron` 用户入口无命名冲突，registry fail-closed 契约保持不变。
- [x] 等价 skill roots 每扫描周期只处理一次，预期 shadowing 不逐项刷 WARN。
- [ ] `.peri/meta` 的 `NotFound` 不记 WARN，其他 I/O 错误仍可见。
- [ ] 默认日志不逐事件重复记录成功 fan-out；异常仍保留足够 correlation fields。
- [ ] `PERI_RENDER_TIMING` 默认关闭，其启用状态可从启动诊断中确认。

## 当前实现与验证记录（2026-08-27）

实现事实：

- Dynamic MCP registry 对同一 session 的 catalog 采用 first-write-wins，后续 turn 的实时 catalog 不覆盖首次 collision baseline；load/unload 的 capability 不进入该 baseline。stage 构建改为 typed `Result`，注册失败进入安全的普通执行失败事件，不再 `expect` panic。当前 stage 仍会在每个 turn 调用 `register_catalog`，因此“真正 initialization 只调用一次”、SubAgent identity、静态 MCP 晚到与 reconnect 语义仍未完成，相关验收项保持未勾选。
- Prediction 使用 10 条非 System 消息软窗口，按 `Ai + all Tool results` 原子投影；缺失结果、孤立结果和无结果的重复 tool-call ID 声明整组丢弃。每个 Tool 只匹配其之前最近的声明，避免损坏历史中的较晚重复 ID 反向认领结果。`AgentModelBridge` 的最终校验保留，未伪造工具名。
- TUI Cron 面板入口改为 `/cron-list`，裸 `/cron` 留给 builtin skill；command registry 的 fail-closed 行为未放宽。
- skill roots 在单次扫描中按 canonical path 去重并保留首次高优先级来源；跨物理 root 的预期 shadowing 降为 DEBUG，同一 root/builtin 内部真实冲突及 builtin 保留名冲突仍为 WARN。

验证证据：

- `cargo check -p peri-agent -p peri-acp -p peri-middlewares -p peri-tui`
- `cargo test -p peri-middlewares --lib -- mcp::dynamic::registry::tests`：16 passed
- `cargo test -p peri-middlewares --lib -- skills::loader::tests`：28 passed
- `cargo test -p peri-agent --lib session::exec`：58 passed
- `cargo test -p peri-acp --lib prediction_projection`：6 passed
- `cargo test -p peri-acp --lib host::executor_flow_test`：17 passed
- `cargo test -p peri-tui --lib -- kit::ui_command::tests`：10 passed
- `cargo test -p peri-tui --lib -- kit::panel_registry::tests`：23 passed
- `cargo fmt --all -- --check`
- `cargo clippy -p peri-agent -p peri-acp -p peri-middlewares -p peri-tui --all-targets -- -D warnings`

以上只证明列出的局部契约；未执行真实 Dynamic MCP provider/TUI E2E，故不据此勾选“动态工具可见且可执行”或完整 load/unload 多 turn 验收。

## 验证命令

具体测试名在实现时确定，至少运行：

```bash
cargo test -p peri-middlewares --lib -- mcp::dynamic
cargo test -p peri-agent --lib
cargo test -p peri-acp --lib
cargo test -p peri-tui --lib
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

涉及 shutdown 时同时按 `ARC-HOST-SHUTDOWN-001` 的 canonical 测试路由验证；涉及 frozen/tool visibility 时按 `ARC-FROZEN-001`、`ARC-TOOLS-001` 检查完整调用链。

## 非目标

- 不在本 issue 中修复远端 MCP gateway、DNS、代理或网络路径。
- 不把 TCP connect failure 误归因于 MCP protocol negotiation。
- 不对已经产生可见 delta 的 model stream 自动重试。
- 不削弱 command、skill 或 tool registry 的 fail-closed 冲突检查。
- 不通过覆盖旧 Dynamic MCP baseline 绕过 catalog drift。
- 不把 fork/compact 写成本次 Prediction 事故的已证实触发器。
- 不将显式启用的 `PERI_RENDER_TIMING` 逐帧日志定义为默认行为 bug。
