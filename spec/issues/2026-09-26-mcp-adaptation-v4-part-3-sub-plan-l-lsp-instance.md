# MCP adaptation v4-part-3 sub-plan L：LSP builtin 实例

> 主计划唯一事实源：`spec/issues/2026-09-26-mcp-adaptation-v4-part-3-plan.md`。本文件只规划 `lsp` 实例，不修改代码；执行 worktree 为 `/Users/konghayao/code/ai/peri-v4p3`。
>
> 同步端口/中间件契约已由主 agent 裁决 **A30（LSP 同步契约）** 冻结，本文件的端口形状、错误类型、cwd 来源、实现者矩阵与超时登记以 A30 为准。

## 覆盖范围表

| 项 | 本文覆盖 | 施工归属 | 验收落点 |
| --- | --- | --- | --- |
| 契约与声明 | IF-P3-01、IF-P3-02；A4/A5/A20 | L-01；契约层由 C-01 单一写入 | §8 第 3/4/12 行 |
| builtin handler | IF-P3-08、IF-P3-12；A6/A21 | L-01 | §8 第 2/5/15 行 |
| 同步端口与薄中间件 | IF-P3-09；A7/A23/A24；**A30 全部 7 条** | L-02、L-03 | §8 第 8/10/21/23 行 |
| pool 作用域与生命周期 | A11/A22；R19/R25 | H-03/H-04 接线，L-02/L-03 提供 seam | §8 第 13/15/17 行 |
| 明确不做 | 配置热更新、`peri-lsp` 协议行为重构、workspace、多余 MCP notification | 本波非目标 | 主计划 §11 |

## 1. 冻结前提与边界

- 本波只有一个 LSP builtin MCP 实例：实例名 `lsp`，传输为 `TransportConfig::Builtin`；宿主不再把 `LSP` 作为 middleware 工具适配器。
- 组合根构造唯一的 `Arc<LspServerPool>`，注入 builtin 上下文；handler 与薄同步中间件消费同一 pool，不复制状态，不通过最后一个 `Arc` 的释放触发关闭（A1）。
- `LspServerPool::new` 是惰性构造：构造 pool 不启动 language server；工具面门控只看 `has_servers()`，不得等待进程 ready（A6/A21）。
- `server_info` 只声明 `tools` capability；不新增 `resources`、订阅或 server→client MCP notification（IF-P3-12/A16）。
- `LSP` 仍是一个 deferred 工具：不进入首个 LLM 请求的 direct 工具表，不进入 `system_mcp_tools`，搜索/执行迁移以裸名 XOR effective name 判断（A4/A5/A20）。
- 同步链路遵循 A30：端口唯一、无默认实现、path 入参、typed `LspSyncError`、同一调用内 didChange→didSave、失败只 debug 降级。

## 2. L-01：声明表与 builtin handler

### 2.1 声明表条目（只提供内容，不并行写契约文件）

契约层 owner 已归并为 C-01 单一写入；L-01 只提供以下逐字段内容给 C-01，不直接与其并行修改 `peri-acp-types/src/builtin_mcp.rs`：

| 字段 | `lsp` 条目冻结值 |
| --- | --- |
| 实例 `name` | `"lsp"` |
| 实例 `instance` | `"lsp"` |
| `policy_key` | `"LspMiddleware"` |
| `system_mcp` | `Some(true)` |
| `system_mcp_tools` | `Some([])`，不提升 direct、不做 required 校验 |
| 工具数量 | 1 |
| 工具原名 | `LSP` |
| 工具 `effective_name` | `mcp__lsp__LSP` |
| 工具 `direct` | `false` |
| 工具 `prompt_declaration` | `None` |
| 保留名 | 不改；`lsp` 已在 `BUILTIN_RESERVED_INSTANCE_NAMES` |

实现/测试约束：

- `LSP_TOOLS` 只声明 `LSP`；effective name 必须与 `effective_tool_name()` 及字面量测试一致。
- `system_mcp_tools == []` 不表示实例未 ready；1R 仍须完成 transport、初始化、能力协商及 `tools/list`。
- `prompt_declaration: None` 保证声明段零变化；既有 web/artifact 模板声明不得被这条修改。
- `MIDDLEWARE_TOOL_NAMES` 不加入 `LSP`；`MIDDLEWARE_NAMES` 只保留链槽位名并由其他任务迁键，`LspMiddleware` 同时作为 builtin policy key 保留。

### 2.2 `LspMcpServer` 结构与接口

目标文件：`peri-middlewares/src/mcp/builtin/lsp.rs`；形状按 wave 1 handler 先例：

```rust
pub(crate) struct LspMcpServer {
    tools: Vec<Arc<dyn BaseTool>>,
    pool: Arc<LspServerPool>,
}
```

构造与 handler 行为：

1. 构造参数接收已完成配置合并的 `Arc<LspServerPool>`；构造点必须晚于 global/plugin LSP 配置合并。
2. 构造时只做一次 `pool.has_servers()` 快照：真则 `tools = vec![Arc::new(LspTool::new(Arc::clone(&pool)))]`，假则 `tools = vec![]`。不在每次 `list_tools` 重新读取配置。
3. `list_tools` 返回该快照；无配置时返回空列表，但 builtin 实例仍 ready。
4. `call_tool` 复用共享 `list_tools_of` / `invoke_tool_call`，以 IF-D14 处理未知工具、成功、业务失败和取消/超时；不得另写结果格式化或错误映射。
5. `LspTool::definition()` 是 schema 与描述来源（`peri-middlewares/src/lsp/tool.rs`）；十个 operation 的 enum、required `operation`、`file_path`/位置/查询字段均从现有 `parameters_schema()` 进入 `rmcp::model::Tool`。不在 handler 中复制 schema。
6. LSP 业务执行复用 `LspTool::invoke`；`formatters.rs` 仍是唯一结果格式化位置，handler 不复制 `format_definition_result`、`format_references` 等逻辑。
7. `get_info` 使用共享 `server_info("peri-lsp-mcp", ...)` 等既有助手形态，仅声明 `tools`；不得覆写 `discover`。
8. 配置热更新是显式非目标：配置变化须重建 host/runtime 上下文并按生命周期重连，不向现有 handler 注入可变配置表。

### 2.3 门控与构造顺序

| 时点 | 必须成立的事实 | 禁止的替代判断 |
| --- | --- | --- |
| 配置加载 | global 与 plugin LSP 配置已合并，disabled server 已由 `LspServerPool::new` 过滤 | 只看 plugin 配置；在合并前构造 handler |
| pool 构造 | `has_servers() == !生效 servers 表.is_empty()`；不启动进程 | `is_ready()`、`active` 或任一 server 子进程状态 |
| handler 构造 | 按 `has_servers()` 快照 `tools` | 每次请求动态增删工具、等待 server ready |
| MCP ready | 实例 ready 与工具数解耦；无配置也可 ready | 用空 `tools/list` 判定实例未 ready |

W0 已实测：注入一个最小配置但不拉起 language server 时，LSP 裸名已经可见；该事实必须由测试保留。

### 2.4 端到端超时不等价（F2 登记）

- `LspTool::timeout()` 返回 `None`（`peri-middlewares/src/lsp/tool.rs:266-268`），而 builtin 桥 `mcp/tool_bridge.rs:52` 的 `TOOL_CALL_TIMEOUT`（120s）在 `:274` 包裹 `peer.call_tool`。
- 因此「复用 `LspTool`」**不等于**端到端 timeout 语义等价：长跑 LSP 请求由桥层 120s 截断并转为 typed 超时错误，原 middleware 路径没有该截断。
- 处置：本 sub-plan 在 §6.2 增加延迟夹具用例，验证超时与取消收敛；并把该差异登记到 acceptance 的语义变更（R20 同节）。

## 3. L-02：`LspPoolPort` 冻结契约（A30）

### 3.1 单端口形状（冻结字面）

不新增 `LspSyncPort` 或第二个 LSP 端口。在 `peri-acp-types/src/ports.rs` 的既有 `LspPoolPort` 上新增三个方法，**一律不给默认实现**（缺省实现会让 mock 静默 no-op，制造假绿）：

```rust
fn ready_for(&self, path: &std::path::Path) -> bool;            // 同步、廉价；只判断该文件是否有可用 ready server
async fn did_change(&self, path: &Path, text: &str) -> Result<(), LspSyncError>;
async fn did_save(&self, path: &Path) -> Result<(), LspSyncError>;
```

- 参数是**文件系统 path**，不是 URI；`path → file:// URI` 转换留在实现内部（沿用 `peri-lsp/src/uri.rs::path_to_uri` 的既有做法）。
- `ready_for` 是**读文件之前的唯一前置判定**：false ⇒ 薄中间件不得读文件、不得发送任何通知。
- 端口只做 LSP 协议工作：按扩展名路由 server、转换 URI、调用既有 `LspClient::did_change` / `did_save`（`peri-lsp/src/client/documents.rs:31` / `:54`）；端口不读文件、不持有 capability root、不解析工具输入。
- `peri-lsp::LspServerPool` 实现端口；不重构 `peri-lsp` 的十个 operation 或 formatter 行为。

### 3.2 `LspSyncError`

- 定义在 `peri-acp-types`（与端口同 crate），最小变体集即够用：`NoServer`（无路由/无 ready server）与 `Protocol`（协议或 JSON-RPC 失败，可用 `String` 承载已脱敏原因）。
- `Debug` / `Display` 不得泄露 env、凭据、token 或未登记路径信息；错误文本只进 debug 日志，不进模型面工具文本。
- 消费侧冻结：薄中间件对任一 `Err` 只 `tracing::debug!` 降级，`after_tool` 恒 `Ok(())`，绝不影响工具结果。

### 3.3 调用语义与已知限界

| 语义 | 冻结值 |
| --- | --- |
| 顺序 | 同一调用内严格 `did_change` → `did_save` |
| 前序失败 | `did_change` 失败**仍尝试** `did_save`（沿用现行 `lsp/middleware.rs:91-96` 行为，不做行为变更） |
| 失败传播 | 两个错误分别 debug 记录；不返回 `after_tool` 错误、不改写工具结果、不阻断后续工具 |
| 并发 | **不要求**跨并发调用的全局串行（保持现状）；显式登记为已知限界，留待 wave 3 评估 |
| 热更新 | 不支持；端口不做配置变更感知 |

### 3.4 读文件边界

- 文件读取属于 `LspSyncMiddleware`（它拥有工具名与调用输入）；顺序是：匹配工具名 → 解析路径 → `ready_for` → 读文件 → 端口 `did_change`/`did_save`。
- 端口接收 path 与文本，只负责协议；端口不得自行 `read_to_string`，不得新增 capability root。
- `LspTool::invoke` 内既有的查询前 `didOpen` 读取行为属于工具自身查询语义，不是本同步中间件的替代实现；本波不复制或重写它。

### 3.5 实现者矩阵（逐条：改动 + 断言）

| 文件 | 改动 | 必须断言 |
| --- | --- | --- |
| `peri-lsp/src/pool.rs` | `impl LspPoolPort for LspServerPool` 增 `ready_for`（`server_for_file` + `is_ready`）、`did_change`、`did_save`（内部 `path_to_uri` 后转发 `LspClient`） | 既有 `pool::tests::test_has_servers` / `test_empty_config` 仍绿；新增 `pool::tests::port_ready_for_reflects_routed_server_state`：无路由/未 ready ⇒ `false` 且 `did_change` 返回 `Err(NoServer)`；有路由且 ready ⇒ `true` |
| `peri-lsp/src/pool_test.rs` | 端口替身 `StubPool` 显式补齐三个新方法（不得留默认实现） | 新增 `pool::tests::port_did_change_and_did_save_route_paths`：替身用 `AtomicUsize` 计数，断言 didChange 在 didSave 之前被调用且计数为 1/1（**以调用计数断言，不以「未报错」代替**） |
| `peri-acp/src/host/requests_test.rs` | `MockLspPool`（`:1495-1508`）补齐三个方法并加调用计数；改造既有 `test_delete_active_session_shuts_down_lsp_pool`（`:1512-1595`）——A11/A22 后 session/delete **不再**关闭 host pool | 新断言形态改为 `test_delete_active_session_does_not_shutdown_host_lsp_pool`：`shutdown_calls == 0`、会话已移除、host pool 句柄仍被 host 持有（`Arc::strong_count`/句柄存在）；三个新方法在该路径计数为 0 |
| `peri-acp/src/host/stdio/run_server_integration_test.rs` | `RecordingLspPool`（`:381-401`）补齐三个方法；改造 session 级 pool 生命周期断言（`:837` load / `:878` resume / `:915` fork / `:1022` 无配置不建池）为 host 级语义 | 新增 `host::stdio::run_server_integration_tests::test_host_lsp_pool_survives_session_lifecycle`：多个 session 共享同一 pool（指针相等/创建计数为 1）、session 关闭后 pool 仍在、host shutdown 才关闭；`RecordingLspPool` 记录 shutdown 调用计数并断言恰 1 次 |

- 其他 `LspPoolPort` impl/消费点（装配面 downcast、`host/shutdown.rs` 收集逻辑）必须同步编译通过并补实；不得创建平行 trait。

## 4. L-03：`LspSyncMiddleware` 与关闭矩阵

### 4.1 职责边界（cwd 来自 hook state）

目标文件：`peri-middlewares/src/lsp/middleware.rs`。删除/替换当前活体 `LspMiddleware` 的工具面，保留一个薄 `LspSyncMiddleware`：

- 构造签名冻结为 `LspSyncMiddleware::new(port: Arc<dyn LspPoolPort>)`：**cwd 取自 `after_tool` 的 `AfterToolState`（`StateView::cwd()`）**，主计划 §3 IF-P3-09 / §8 第 8 行冻结；A30 第 4 条只禁止假定 hook 能拿到 `ToolContext`，不禁止消费 hook 自身的 state 参数（`LspSyncMiddleware::new(port, cwd)` 的装配期注入形态已被裁决否定，见 §7）。
- `name()` 返回 `LspSyncMiddleware`；仍占用 `ChainSlot::Lsp`。
- `after_tool` 只匹配 `Write`、`Edit`（保留既有匹配范围）；对 `LSP`、`mcp__lsp__LSP` 及其他工具立即 `Ok(())`。
- 路径解析规则：`tool_call.input["file_path"]` 为绝对路径则原样使用，为相对路径则相对 `state.cwd()` 解析为绝对路径，再交给端口（不以进程 cwd 兜底）。
- 顺序：路径缺失/非字符串 ⇒ debug 并 `Ok(())`；`port.ready_for(&abs) == false` ⇒ debug 并 `Ok(())`（**不读文件、不发端口**）；随后 `tokio::fs::read_to_string` 读取，失败 debug 并 `Ok(())`。
- 读成功 ⇒ 依次 `did_change(&abs, &text)`、`did_save(&abs)`；任一 `Err` 单独 debug，恒 `Ok(())`，不改写原工具结果。
- `LspSyncMiddleware` 不实现 LSP 工具面、不构造 `LspTool`、不启动 server，不承担 `LspMcpServer` 的 ready/handler 生命周期。
- `LspSyncMiddleware: false` ⇒ 不读文件、不调用端口；`LspMiddleware: false` ⇒ 工具面与同步目标同时关闭；两者都非物理销毁（A7/A24）。

### 4.2 从活体结构迁移的步骤

1. 在 L-01 handler 就绪后，把 `LspMiddleware::collect_tools` 的 LSP 工具构造移除；工具只由 `LspMcpServer` 暴露。
2. 将当前 `LspMiddleware::after_tool`（`middleware.rs:62-98`）的 Write/Edit 匹配、`ready` 判定、文件读取、didChange→didSave 顺序逻辑收敛到 `LspSyncMiddleware`，改为消费 `LspPoolPort`，不保留第二份 pool。
3. 删除 `LspMiddleware::new` / `from_configs` 的工具面构造路径；仅保留同步 middleware 的 `new(port, cwd)` 构造器。
4. `assembly/lsp.rs` 的 `add_lsp` 不再注册 `LspMiddleware` 或临时 pool，只在配置合并完成后用同一 host pool 构造同步中间件；builtin 上下文把同一 pool 交给 LSP handler。
5. `assembly.rs` 的 `ChainSlot::Lsp` 分支改为 `LspSyncMiddleware`；不得删除 `ChainSlot::Lsp`，不得新增 `ChainSlot::Cron`。
6. 改造既有 middleware 测试为同步测试；旧的 `collect_tools` 测试迁到 `mcp::builtin::lsp` handler 测试，避免把 middleware 重新当成能力面。

### 4.3 两关闭键交叉矩阵

| `LspMiddleware` policy | `LspSyncMiddleware` policy | LSP builtin 工具 | Write/Edit 同步 | 是否读文件/发端口 | 实例/pool/readiness |
| --- | --- | --- | --- | --- | --- |
| 开 | 开 | 可见/可调用 | 开 | 是，didChange→didSave | 保留/ready |
| 开 | 关 | 可见/可调用 | 关 | 否 | 保留/ready |
| 关 | 开 | 不可见/不可调用 | 关 | 否；关闭实例时同步目标也必须关闭 | 保留/ready |
| 关 | 关 | 不可见/不可调用 | 关 | 否 | 保留/ready |

`LspMiddleware: false` 是 builtin 实例的工具面/同步目标关闭键，不是物理析构；`LspSyncMiddleware: false` 只关闭同步。两者均不停止 pool、handler task、language server，也不改变 builtin readiness（A7/A24）。物理关闭只由显式 instance close/host shutdown 生命周期操作负责。

## 5. A11/A22：pool 从 per-session 改为 per-host

### 5.1 逐文件改造点

| 文件/消费面 | 当前事实 | 目标改造 |
| --- | --- | --- |
| `peri-acp/src/host/requests/session_lifecycle.rs` | `session/new`、load/resume/fork 路径及 `create_session_lsp_pool`（`:491-496`）创建 per-session pool；`close_owned_session` 在 `session/delete` 调 `pool.shutdown()`（`:921-923`） | 删除 session 级构造 helper 与 `SessionState.lsp_pool` 生命周期消费；session/delete 不再 shutdown LSP；session state 只持有 host pool 的共享句柄 |
| `peri-acp/src/host/prompt.rs` | 从 session state 取 `lsp_pool`（`:294`、`:314`），并把 `lsp_servers`/`lsp_pool` 放进每 turn `SessionContext`（`:526-527`） | 改为从 deployment/host 级上下文取得同一 pool；同步中间件由装配面用会话 cwd 构造（不再声称经 `ToolContext` 传参）；不按 turn/session 重建 pool |
| `peri-middlewares/src/assembly/lsp.rs` | `create_session_lsp_pool(cwd, configs)`（`:42-56`）；`add_lsp`（`:58-90`）可 downcast 后构造 `LspMiddleware`，无 pool 时临时构造 | 工厂改为 host 级单次构造（root = host cwd），配置合并后注入 `BuiltinInstanceContext.lsp`；`add_lsp` 只装配 `LspSyncMiddleware::new(port)`，不创建临时工具实例（L-03 已落机械换名，门控重排归 H-03） |
| `LspPoolPort` 消费点 | `peri-acp/src/host/shutdown.rs:43-56` 收集 session pools 去重后关闭（`:125-133`）；`session_lifecycle.rs` 关闭单 session pool | host 配置持有唯一 pool；host shutdown 直接对该 pool 做有界 shutdown；删除 session/delete 关闭路径；所有 mock/recording impl 按 §3.5 补实并计数 |
| `peri-agent/src/session/factory.rs` / executor context | 字段与注释仍是 session 级 `lsp_pool`（`:298-300`） | 随 H-03/H-04 改为 host deployment 投影；若保留字段，必须明确是 host-shared handle，不能由 session 创建/销毁 |

### 5.2 shutdown、退化与 wave 3 依赖

- 责任人：H-04 在 host 组合根/`peri-acp/src/host/shutdown.rs` 收口；必须关闭 host 唯一 pool 管理的全部 language server，执行有界等待并断言全部 client/task 收敛、无 orphan。
- `session/delete` 只释放 session 资源，不关闭 host pool；多个 session 共享同一 `Arc` 不得导致重复关闭或提前关闭（断言见 §3.5）。
- 单 cwd 场景：`root_uri = host cwd`，行为逐字等价。多 cwd 场景：所有 session 共享 host root，明确登记为功能退化；不得在本波通过隐式临时 pool 或配置热更新补偿。
- 必须同步写入 acceptance 的语义变更记录、用户文档/代码索引，并注明 wave 3 依赖：通过 `ToolContext` → MCP 调用上下文恢复 per-session root/多 cwd 隔离。本 sub-plan 不实现 wave 3。

## 6. 测试清单与验收映射

> 命令纪律（F3/F4）：过滤串必须指向**真实存在**或**明确标注新增**的测试函数；集成测试用 `cargo test -p <crate> --test <target> -- <filter>` 形态；`0 tests` 与「只命中旧用例」都算失败。本计划阶段不执行 cargo。

### 6.1 契约、门控、同步与生命周期

| 测试函数（模块路径） | 状态 | 关键可观察量 | 验收行 |
| --- | --- | --- | --- |
| `builtin_mcp::tests::tools_are_non_empty_and_unique_per_instance` | 既有（C-01 修改） | `lsp` 条目 `direct=false`、`prompt_declaration=None`、effective name `mcp__lsp__LSP` | §8-4、§8-12 |
| `mcp::builtin::lsp::tests::list_tools_follows_configured_server_set` | 新增 | 有配置但未启动进程时报 `LSP`；空配置时空表；实例仍 ready | §8-5 |
| `mcp::builtin::lsp::tests::call_tool_reuses_lsp_tool_and_shared_result_mapping` | 新增 | schema/十个 operation 来自 `LspTool`；成功/失败走 IF-D14，不重复 formatter | §8-2、§8-23 |
| `mcp::builtin::lsp::tests::handler_snapshots_tools_after_config_merge` | 新增 | 合并前不得构造；合并后快照；配置变更不热更新 | §8-5 |
| `mcp::builtin::lsp::tests::server_info_declares_tools_only` | 新增 | capabilities 只有 `tools`，无 resources/subscriptions | §8-2、§8-15 |
| `pool::tests::test_has_servers` / `pool::tests::test_empty_config` | 既有 | `has_servers()` 只反映生效配置表，不反映 server ready | §8-5 |
| `pool::tests::lsp_pool_port_downcast_roundtrip` | 既有 | 同一 pool `Arc::ptr_eq`；新方法经端口可达 | §8-8、§8-13 |
| `pool::tests::port_ready_for_reflects_routed_server_state` | 新增 | 无路由/未 ready ⇒ `ready_for == false` 且 `did_change == Err(NoServer)`；ready ⇒ true | §8-8 |
| `pool::tests::port_did_change_and_did_save_route_paths` | 新增 | 替身调用计数 1/1、didChange 先于 didSave | §8-8 |
| `lsp::middleware::tests::write_sync_orders_change_then_save` | 新增（已落地） | `Write`/`Edit` 读最新文件；端口记录顺序严格 didChange、didSave；计数断言 | §8-8 |
| `lsp::middleware::tests::not_ready_skips_read_and_notifications` | 新增（已落地） | `ready_for == false` ⇒ 不发端口任何通知；配合 `not_ready_with_missing_path_skips_file_access` 断言 ready 门禁先于**文件访问**（读调用计数 0） | §8-8、§8-10 |
| `lsp::middleware::tests::change_error_still_attempts_save` / `read_failure_degrades_to_ok` / `missing_or_non_string_file_path_skips_port` / `non_write_tools_ignored` | 新增（已落地） | 端口 typed error 仅 debug 且仍尝试 save；读失败与非 Write/Edit、缺 path 一律恒 `Ok(())` 且不改写原工具结果 | §8-8、§8-23 |
| `lsp::middleware::tests::relative_path_resolved_against_state_cwd` | 新增（已落地） | 相对 `file_path` 按 `state.cwd()` 解析；绝对路径原样；进程 cwd 兜底必红 | §8-8、§8-13 |
| `assembly::tests::production_chain_has_only_lsp_sync_slot` | 新增 | `ChainSlot::Lsp` 只挂 `LspSyncMiddleware`；无 LSP 工具 middleware | §8-21 |
| `host::requests::tests::lifecycle_cases::test_delete_active_session_does_not_shutdown_shared_host_lsp_pool` | 改造（原 `test_delete_active_session_shuts_down_lsp_pool`） | session/delete 后 `shutdown_calls == 0`，host pool 仍在 | §8-13、§8-15 |
| `host::stdio::run_server_integration_tests::test_host_lsp_pool_survives_session_lifecycle` | 改造（原 session-scoped 断言） | 多 session 共享同一 pool（创建 1 次）；session 关闭后 pool 仍在；host shutdown 恰关闭 1 次 | §8-13、§8-15 |
| `host::mcp_v4_wave2_baseline::wave2_baseline_lsp_tool_visible_when_server_configured` | 既有（迁移后重跑） | W0 门控对照；迁移后改以 `mcp__lsp__LSP` 出现在搜索面（裸名 XOR effective name） | §8-3、§8-5、§8-16 |
| `host::mcp_v4_wave2::lsp_sync_close_cross_matrix` | 新增（新夹具模块） | 两关闭键四格矩阵：只关同步时工具仍可用且不读/发；关实例时工具与同步均关；pool/ready 保留 | §8-10 |
| `host::mcp_v4_wave2::multi_cwd_degradation_and_host_shutdown` | 新增（新夹具模块） | 单 cwd root 等价；多 cwd 共享 host root 有明确证据；host shutdown 后全部 server/task 结束 | §8-13、§8-15 |

### 6.2 超时与取消收敛（F2）

| 测试函数（模块路径） | 状态 | 断言形态 |
| --- | --- | --- |
| `mcp::tool_bridge::tests::builtin_tool_call_surfaces_timeout_error_after_bridge_deadline` | 新增 | 延迟夹具不返回 ⇒ 断言桥层在 `TOOL_CALL_TIMEOUT` 到期后返回 typed 超时错误（文案含秒数、不含路径/凭据），且调用 future 已 drop。机制优先 `tokio::time::pause()` + `advance`；若该 bridge 路径不支持虚拟时钟，则改为断言「deadline 可注入/缩短」的测试专用路径；两者都不可行时退化为仅断言语义①并在 acceptance 登记原因，禁止真实等 120s |
| `mcp::builtin::lsp::tests::delayed_fixture_converges_without_orphan_on_cancel` | 新增 | 取消令牌触发后：①夹具侧取消信号到达（oneshot 计数）；②调用在有限步内返回；③无 orphan（fixture task join 成功或 dropped 计数符合预期） |

统一断言形态（三条缺一不可）：**typed 错误/取消事实** + **夹具侧信号或调用计数** + **无 orphan 证据**；不得用「未 panic / 未报错」代替。

## 7. 与代码冲突项

| # | 事实 | 状态 | 处置 |
| ---: | --- | --- | --- |
| 1 | 任务书给定基线 `b1651aee`，实际 worktree HEAD 为 `a81e0ba66c014f8a05ae05298f9eee9f94e6ffaf`（分支名一致） | **已由主计划 v3 头部采信 `a81e0ba6`（非 A30）** | 行号以「符号名 + repository-relative 完整路径」为准、行号仅作辅助标注；acceptance 记录最终基线 |
| 2 | `after_tool` 无 `ToolContext` 参数（`peri-agent/src/middleware/trait.rs:142-148`） | **已裁决（A30 第 4 条 + 主计划 §3 IF-P3-09）** | 冻结为 `LspSyncMiddleware::new(port)`，cwd 取自 hook state 的 `StateView::cwd()`；已删除全部「中间件持有 `ToolContext`」表述（§4.1） |
| 3 | 现行 `LspMiddleware` 自持具体 pool 并同时暴露工具面（`lsp/middleware.rs:21-98`） | **已裁决（A7/R15；A30 第 4 条）** | 拆为 builtin handler + 薄同步中间件，旧 `collect_tools` 与临时 pool 一并删除（§4.2） |
| 4 | `LspPoolPort` 当前仅 `as_any`/`shutdown`（`peri-acp-types/src/ports.rs:340-346`） | **已裁决（A30 第 1/2 条）** | 新增无默认实现的 `ready_for` / `did_change` / `did_save` 与 `LspSyncError`（§3） |
| 5 | pool 现为 per-session（`assembly/lsp.rs:42-56`、`session_lifecycle.rs:181`/`:628`/`:1171`、`peri-agent/src/session/factory.rs:298-300`） | **已裁决（A11/A22）** | 迁 per-host，删除 session 级构造与 session/delete 关闭（§5） |
| 6 | host shutdown 从 session 集合收集 pool 去重后关闭（`host/shutdown.rs:43-56`、`:125-133`） | **已裁决（A11/A22；A30 第 5 条）** | 改为 host 唯一 pool 直接有界关闭；两条既有断言按 §3.5 改造 |
| 7 | 组合根仍建立宿主 cron tick（`host/assemble.rs:299-325`） | **未裁决** | 非 LSP 范围，由 C/H 任务处理；L-01/L-03 只消费 H-04 的 host context |
| 8 | 主计划 §6 V-03 使用 `host::mcp_v4_wave2::wave2_baseline_first_request_and_deferred_summary`，实际既有模块为 `host::mcp_v4_wave2_baseline`（`peri-acp/src/host/mod.rs:74-75`） | **未裁决** | 既有基线以真实模块路径为准（`host::mcp_v4_wave2_baseline::*`）；新增 host 夹具另建 `host::mcp_v4_wave2` 模块，二者不混用 |
| 9 | 现行同步直接用 `file_path` 交给 `path_to_uri`，相对路径按**进程 cwd** 绝对化（`lsp/middleware.rs:83`、`peri-lsp/src/uri.rs:27-31`） | **已裁决（A30 第 4 条）** | 改为按 `state.cwd()` 解析；进程 cwd ≠ 会话 cwd 时属可观察差异，须在 acceptance 语义变更节登记（§6.1 已加断言） |
| 10 | `LspClient::did_change` / `did_save` 入参为 URI 字符串，定义在 `peri-lsp/src/client/documents.rs:31` / `:54`（不在 `client.rs`） | **已裁决（A30 第 1 条）** | 端口对外收 path；URI 转换留在 `LspServerPool` 实现内部（§3.1） |

## 8. 施工顺序 + 提交切分

> 每步只列验证命令；本次撰写不执行。所有过滤串均为既有真实函数或明确标注「新增」；集成测试用 `--test <target>` 形态（F3/F4）。

1. **契约内容与 handler**：C-01 写入 `LSP_TOOLS`/实例条目；L-01 新增 `mcp/builtin/lsp.rs`（共享助手、`LspTool`、配置非空快照、capability 声明）并挂 `lsp_test.rs`。验证：`cargo test -p peri-acp-types --lib -- builtin_mcp::tests::tools_are_non_empty_and_unique_per_instance`（既有，改造）；`cargo test -p peri-middlewares --lib -- mcp::builtin::lsp::tests::list_tools_follows_configured_server_set`（新增）。
2. **单端口合流**：L-02 按 §3.1 改 `LspPoolPort`（无默认实现）与 `LspServerPool` 实现，同步补 `pool_test.rs` 替身与计数断言。验证：`cargo test -p peri-lsp --lib -- pool::tests::lsp_pool_port_downcast_roundtrip`（既有）；`cargo test -p peri-lsp --lib -- pool::tests::port_ready_for_reflects_routed_server_state`（新增）；`cargo test -p peri-lsp --lib -- pool::tests::port_did_change_and_did_save_route_paths`（新增）。
3. **薄同步与槽位**：L-03 把活体 `LspMiddleware` 拆为 `LspSyncMiddleware::new(port)`（ready_for → 读 → didChange → didSave → 恒 `Ok(())`，cwd 取 `state.cwd()`）；H-03 让 `ChainSlot::Lsp` 只挂同步中间件。验证：`cargo test -p peri-middlewares --lib -- lsp::middleware::tests::write_sync_orders_change_then_save`（已落地）；`cargo test -p peri-middlewares --lib -- lsp::middleware::tests::not_ready_skips_read_and_notifications`（已落地）；`cargo test -p peri-middlewares --lib -- assembly::tests::production_chain_has_only_lsp_sync_slot`（待 H-03 新增）。
4. **host pool 与关闭**：H-04 将 pool 迁为 per-host，重排「配置合并 → pool → builtin context → handler」，删除 session/delete 关闭，host shutdown 有界关闭全部 server；按 §3.5 改造两条既有断言。验证：`cargo test -p peri-acp --lib -- host::requests::tests::test_delete_active_session_does_not_shutdown_host_lsp_pool`（改造）；`cargo test -p peri-acp --lib -- host::stdio::run_server_integration_tests::test_host_lsp_pool_survives_session_lifecycle`（改造/新增）；`cargo test -p peri-acp --lib -- host::mcp_v4_wave2::multi_cwd_degradation_and_host_shutdown`（新增）。
5. **超时/取消与交叉验收**：补 §6.2 两条延迟夹具用例、两关闭键矩阵、direct XOR 与 IF-P3-12 隔离证据；S-05 登记多 cwd 退化、超时语义差异与 wave 3 依赖。验证：`cargo test -p peri-middlewares --lib -- mcp::tool_bridge::tests::builtin_tool_call_surfaces_timeout_error_after_bridge_deadline`（新增）；`cargo test -p peri-middlewares --lib -- mcp::builtin::lsp::tests::delayed_fixture_converges_without_orphan_on_cancel`（新增）；`cargo test -p peri-middlewares --test mcp_isolation_contract -- distinct_instances_keep_distinct_pool_entries_and_handle_identity`（既有集成目标真实函数）；`cargo test -p peri-acp --lib -- host::mcp_v4_wave2::lsp_sync_close_cross_matrix`（新增）。

建议提交切分：`L-01 lsp builtin handler` → `L-02 lsp pool sync port` → `L-03 lsp sync middleware` → `H-03/H-04 host assembly and lifecycle` → `V/S acceptance and docs`。每次提交只包含对应 owner 文件；契约层 `builtin_mcp.rs` 不与 L-01 并行写入。
