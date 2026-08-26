# ACP Host 双轨统一（stdio 并入 run_acp_server）

状态：设计稿 v2（经对抗审查修订，2026-08-17）｜范围：peri-acp host 层

> **修订记录**：v1 经对抗性审查（advisor/opus）发现 5 处失真：①将「统一 transport」误判为「统一 host 语义」；②Handler 映射表误报 `execute-command` 覆盖；③遗漏 prompt 错误/取消/通知序列差异；④把「字段超集」当作「行为超集」；⑤依赖删除计划不成立。v2 逐项修正。

## 1. 背景与目标

`peri-acp` 的 host 层存在两套并行的 ACP 服务实现：

- **TUI/notify 路径**：`run_acp_server`（`host/mod.rs`）→ `handle_request`（`host/requests.rs` + `requests/` 模块组）+ `dispatch_prompt_turn`（`host/prompt.rs`），经 `AcpTransport` 抽象收发（mpsc）。
- **stdio 路径**：`run_acp_stdio`（`host/stdio/mod.rs`）→ agent-client-protocol 框架的 13 个 typed handler（`host/stdio/session/*`），经 `StdioContext` 持有状态。

同一份「装配 → 会话状态 → 请求处理 → prompt 执行 → 通知」被实现两遍，已发生双向重复问题（MCP skill 发现预热先后漏 TUI 侧、stdio 侧）。目标：**合并为单一路径**——stdio 与 TUI 共用 `run_acp_server` + `AcpServerConfig` + `SessionState`，transport 多态。

## 2. 现状盘点（证据，已校正）

| 维度 | TUI/notify | stdio | 判定 |
|---|---|---|---|
| 装配 | `assemble_server_config`（三路径共用注释） | `init_stdio_context`（复制逻辑） | stdio 未切 |
| 会话状态 | `SessionState` | `SessionInfo` | 类型超集，**行为非超集**（§6） |
| 请求处理 | `requests/` + Value 返回 | typed handler + typed resp | 双份 |
| prompt 引擎 | `run_prompt`（39KB，recall/continuation/bgResults） | `prompt_exec::run`（25KB，frozen replay/session_start_source） | 双份、状态机不同（§7） |
| 线协议 transport | `AcpTransport`（mpsc） | agent-client-protocol `Stdio` | stdio 待切 `transport/stdio.rs` |
| initialize builder | `dispatch::build_initialize_response` | 同一函数 | **共享**（`dispatch/init.rs:21`） |
| 命令广播 | `build_available_commands_update` | 同一函数 | **共享**（`dispatch/commands.rs`） |
| 命令执行协议 | `dispatch/execute_command.rs`（ReAct loop 内命令执行，非独立 RPC method） | 同一函数 | **共享** |

**关键定位（v1 失真修正）**：`transport/stdio.rs` 只是 **JSON-RPC 传输层**（信封/分帧/id 匹配/pump，`transport/stdio_test.rs` 已有基础信封测试），**不包含** initialize 状态机、typed 输入校验、session lifecycle 副作用、prompt 响应、通知顺序、legacy cancel、EOF 清理等 **host 语义**。这些语义全部由 `run_acp_server` 侧承载——统一 ≠ 「换 transport」，而是「stdio 业务处理整体切换到 run_acp_server 的 handler」。

`transport/stdio.rs` 现状：`send_request/send_notification/recv/send_response` 齐全（`transport/stdio.rs:134-201`）；id 转换非保真（`Value::Number(n) → i64`，`224-229` 越界压 0）；pump 退出依赖 channel 关闭的间接语义（`121-124`）；**零集成测试**（有 envelope 单测，无 stdin pump/stdout/并发/EOF/initialize 集成测试）。

> **后续闭环（2026-08-24）**：上段保留统一工程启动时的历史基线；当前 `transport/stdio.rs` 已完成 id 域收口与生命周期闭环。reader EOF/error、outbound write/flush failure 会终止 router 并以稳定 `Transport closed` 结算当前/后续 request；caller cancellation 同步注销 pending；连接静默仍无隐式 timeout。MPSC pair 复用同一 terminal 生命周期，任一方向关闭会对称终止，并保留已经转发到 incoming queue 的消息。事实源为 ARC-TRANSPORT-001 与 transport 相邻测试。

## 3. 目标架构

```
TUI  │ mpsc transport  ──┐
                         ├─→ run_acp_server(AcpServerConfig, SessionState)
stdio│ StdioTransport  ──┘        ├─ requests/（控制面 RPC）
                                  ├─ notify.rs（通知 + session/cancel）
                                  └─ prompt.rs（dispatch_prompt_turn：session/prompt 专用分支）
```

删除：`host/stdio/session/*` handler 全套、`StdioContext`、`SessionInfo`、agent-client-protocol typed handler 层。

## 4. Handler 映射表（卧底复核后）

| stdio handler | 统一后处理位置 | 差异点 |
|---|---|---|
| initialize | `requests.rs` "initialize"（`session_lifecycle::handle_initialize`） | 共用同一 builder + `set_pending_caps`；统一路径由 run_acp_server 自身保证 initialize 先于 session/new（**需补顺序违反测试**） |
| session/new | "session/new" | stdio 专有：LSP pool（并入 SessionState）、`prewarm_mcp_discovery`（并入 handle_new） |
| session/list | "session/list" | — |
| session/prompt | **`run_acp_server` 专用分支** → `dispatch_prompt_turn`（非 handle_request） | 见 §7 |
| session/set_mode | "session/set_mode" | — |
| session/set_config_option | "session/set_config_option" | — |
| session/cancel（通知） | `notify.rs` "session/cancel" | **语义不同**：stdio 仅 `token.cancel()`；notify 侧检查 writer lease + continuation 武装。合并后取消语义以 notify 为准，需验证 IDE 取消行为不回归（§10 决策点 2） |
| session/close | "session/close" | 错误传播策略差异（stdio 部分失败返回成功 vs 统一返回 `AcpError`）——按统一语义收口 |
| session/resume | "session/resume" | LSP pool 语义并入 |
| session/load | "session/load" | 同上 |
| session/fork | "session/fork" | 同上 |
| session/delete | "session/delete" | 同上 |
| session/update_config | "session/update_config"（`requests.rs:47`） | — |
| **legacy `{"type":"cancel"}`**（`stdio/transport.rs:37-49` debug hook） | 无对应 | **必须显式移植到 `transport/stdio.rs` 的 pump**，并明确与标准 `session/cancel` 的并存语义（§7/§10 决策点 3） |

**不存在的覆盖点（v1 失真修正）**：`execute-command` **不是独立 RPC method**——`dispatch/execute_command.rs` 是命令执行协议解析层（ReAct loop 内调用，两路径共享），stdio 13 个 typed handler 中也没有 ExecuteCommandRequest。v1 映射表中相关表述删除。

统一后 stdio **新增扩展能力**（非回归承诺的一部分，见 §9「四标准」）：`workflow/*`、`plugin/*`、`session/rename`、`session/rewind`、`marketplace/*`、`mcp/*`。

## 5. 装配统一

`stdio/init.rs` 改为构造 `HostAssemblyInput` → `assemble_server_config`（与 TUI `launch.rs:148` 同构）：

| HostAssemblyInput 字段 | stdio 取值 |
|---|---|
| provider | 现有 `LlmProvider::from_config` → env |
| peri_config | `Arc<RwLock<PeriConfig>>`（现有 `provider::load()`） |
| config_source | `provider::ConfigSource::load()`（新构造） |
| permission_mode | main.rs 传入 |
| thread_store | 现有 `open_thread_store_with(db_path)` |
| cwd / bare=false / drive_cron_tick=false | 现有 |

`StdioContext` 过渡期收窄为 `{ cfg: AcpServerConfig, sessions: RwLock<HashMap<String, SessionState>> }`；handler 字段引用 `ctx.xxx` → `ctx.cfg.xxx`（19 字段，`cron_scheduler`/`mcp_pool` 由 Arc 变 Option 需 `as_ref().unwrap()`；`parking_lot::RwLock` 与 `tokio::sync::Mutex` 差异单独处理——**不能标「行为零变化」**，标「同实现仅换引用」）。

## 6. 会话状态统一（v2：从「字段超集」改为「状态机兼容性审计」）

`SessionInfo` 删除，stdio 改用 `SessionState`（`host/mod.rs:66-93`）。**字段类型同构 ≠ 生命周期语义同构**，以下每项单独审计创建/读取/更新/清理位置：

| 关切点 | 差异与处理 |
|---|---|
| `cancel_token` | stdio 写、notify 侧粒度（写锁 + lease 检查）；统一后以 notify 语义为准 |
| `agent_pool` | stdio 侧 `mem::take` 取出、执行后归还（`prompt.rs:84-92`）；TUI 侧由 `dispatch_prompt_turn` 统一管理——**取出/归还/异常路径是否一致需逐分支核对** |
| `lsp_pool` | 两者同构；统一后由 handle_new/close/delete + run_acp_server 退出钩子（`host/mod.rs:473-483`）管理 |
| **writer lease** | notify 侧 cancel 检查 `state.lease.is_writer("default")`；stdio session 创建时必须正确建立 lease，否则 stdio 合法取消会被静默忽略 |
| `recall_items` / `continuation_*` / epoch | stdio 当前不消费；统一后 prompt 路径会读写——**「默认值即可」不成立**，必须核对初始值（`recall_items: Vec::new()`、`continuation_armed: false`）与续跑/取消交互 |
| `history` | 两条 prompt 路径对 cancel/error 后增量保留、compact 替换、泄漏 prepend 清理的处理不同（§7.4）——以 TUI `run_prompt`（`host/prompt.rs:646-724`）为准，逐分支对齐 |

结论：迁移对象是 **session 状态机**，不是 Rust struct。批 2 的 cancel/close/delete 语义对齐是本批核心验收项。

## 7. Prompt 引擎合并（最大风险块，v2 差异矩阵扩充）

差异清单（每项是独立对齐任务，禁止以「都走 executor」一带而过）：

| # | 能力 | stdio `prompt_exec::run` | TUI `run_prompt` | 合并方向 |
|---|---|---|---|---|
| 1 | frozen 重放 | 有（`params.frozen`） | 有（`SessionState.frozen`） | 以 TUI 为准，stdio 语义对照 |
| 2 | history replay | 有（`params.history`+len） | 从 SessionState 取 | 对齐 |
| 3 | `session_start_source` | 有（`startup`/`None`，`prompt.rs:53-73`） | **待核对是否消费**（v2 未决） | 若 TUI 未消费，按 stdio 语义补入 `dispatch_prompt_turn` |
| 4 | recall 注入（take/clone 语义） | 无 | 有（`prompt.rs:132-138、756-767`，continuation 特判） | stdio 获得新行为，**属行为扩展** |
| 5 | continuation/epoch | 无 | 有（scheduler + `continuation_armed` + epoch） | 同上；**取消后自动续跑行为变化需 IDE 侧确认** |
| 6 | bgResults / tool-result 注入 | 无 | 有（`prompt.rs:85-89` synthetic tool_use/tool_result 注入） | stdio 将看到额外 synthetic tool 消息，行为变化 |
| 7 | prediction | 有 | 有 | 对齐 |
| 8 | cancel/error 后 history 持久化、compact 替换、泄漏 prepend 清理 | 独立实现 | `prompt.rs:646-724`（增量持久化 + `history_replaced_by_compaction`） | 以 TUI 为准逐分支对齐 |
| 9 | event sink / 通知序列 | `StdioEventSink`（typed `SessionUpdate`，经 `ConnectionTo<Client>`） | `TransportEventSink`（`AcpTransport::send_notification`） | **通知时点/顺序/响应先后需 wire 级对齐**（§8） |
| 10 | legacy `type:cancel` 全 session cancel | `transport.rs:37-49` debug hook（遍历全部 session token） | 无 | 移植到 `transport/stdio.rs` pump；与标准 `session/cancel`（按 sessionId + lease + continuation）**并存语义需定义** |
| 11 | prompt response 与 update 的并发/排序 | typed responder 串行 | transport writer mutex | 对齐后加顺序断言 |

合并原则：以 `run_prompt`（更全、TUI 产线使用）为基座，stdio 独有能力（#3、#10）并入后删除 stdio 引擎。**prompt 是最后迁移项**（§9 批 3），迁移后先手动全场景验证再删旧引擎。

## 8. 线协议验证计划（v2 修正）

> **批 0 已落实（2026-08-17）**：下方 1-3 全部完成——§8.1/8.2 → `transport/stdio_test.rs`（当时记录 id 与 EOF 后 pending 挂起的历史基线，后续分别完成 id 域收口与 ARC-TRANSPORT-001 生命周期闭环）；§8.3 wire capture → `host/unify_wire_baseline_test.rs`（AvailableCommandsUpdate / SessionInfoUpdate 双路径逐字段一致 + initialize 基线）。结果见 §9「批 0 完成清单」。

- **现状校正**：`transport/stdio.rs` 已有基础信封测试（`transport/stdio_test.rs:1-39`：response/request/notification envelope + id conversion），但**缺集成测试**（pump/stdout/并发/EOF/非法 JSON/initialize 顺序）。
- 批 1 前必须补齐：
  1. stdin pump + stdout 写入、request/response 配对、并发报文交错、EOF、write failure、非法 JSON；
  2. id 保真：agent-client-protocol 的 `RequestId` 语义 vs `transport/stdio.rs:224-229` i64 截断（越界/字符串 id）——**要么保真要么显式对齐 ACP 约束**；
  3. **wire capture 对照测试**：typed stdio 路径与统一路径各自产出 initialize/new/update/prompt 通知的最终 JSON，断言逐字段一致（外层 `{sessionId, update}`、`sessionUpdate` 判别字段、`_meta`、camelCase/snake_case、空字段省略规则）。
- 兼容点判定：initialize builder 同源（`dispatch/init.rs`，OK）；notification 同源但**未被测试证明**（v1 过渡断言，纳入 #3 wire capture 测试）。

## 9. 分批执行计划（v2 重排）

**「行为零变化」拆为四独立标准**（v1 自相矛盾修正）：
- A. 标准 ACP wire 兼容（客户端看到的信封/字段不变）
- B. 现有 stdio 功能兼容（freeze/replay/LSP/prediction/session_start_source/type:cancel）
- C. TUI 行为不回归
- D. 统一后新增扩展能力（workflow/plugin/rewind 等——**明确的增强，不是零变化**）

| 批次 | 内容 | 验证标准 |
|---|---|---|
| 批 0 | 补 `transport/stdio.rs` 集成测试（§8.1/8.2）+ 现有 typed stdio 与统一路径的 **wire capture 对照测试**（§8.3，双路径并存期先建立基线） | 测试绿；基线锁定（**已完成**，见下方「批 0 完成清单」） |
| 批 1 | 装配统一（§5，`StdioContext` 收窄）+ 字段引用迁移 | `cargo check` + stdio 测试绿（**标「同实现仅换引用」，不标行为零变化**） |
| 批 2 | 控制面迁移（**逐方法**，按对抗建议顺序）：initialize/list → set_mode/set_config_option → new/load/resume/fork/close/delete → cancel（先定 lease/continuation 语义）→ update_config；每步 typed handler 与 handle_request **并存**（适配器） | 每方法 wire capture 对照 + stdio 测试迁移通过 |
| 批 3 | prompt 引擎合并（§7 #1-11 逐项）+ legacy `type:cancel` 移植 + 删除 `stdio/session/*`、`StdioContext`、`SessionInfo` | 手动全场景验证（freeze/replay/lsp/prediction/cancel/通知顺序）后才删旧引擎；clippy `-D warnings` |
| 批 4 | 测试迁移收尾（`create_test.rs` 17KB）+ 文档事实源（`docs/top-level.md` §7/§8、code-index）+ 依赖清理 | `cargo test --workspace`（**已完成**，见下方「批 4 完成清单」；`create_test.rs` 的 load/resume/fork LSP 池断言已随批 3 Step 5 迁入 `run_server_integration_test.rs`） |

每批独立提交；批 2 后 stdio 不再新增 typed handler；批 3 是唯一不可逆临界点（缓存 wire capture 基线作为回滚锚）。

### 批 0 完成清单（2026-08-17，基线已锁定）

- [x] **可测性重构**（`transport/stdio.rs`）：新增 `StdioTransport::from_reader_writer<R, W>`（注入 reader/writer，`tokio::io::duplex` 驱动测试）；`new()`/`Default` 仍绑定进程 stdin/stdout，行为不变。零业务逻辑改动。
- [x] **stdio 集成测试**（`transport/stdio_test.rs`，14 条含既有信封单测）：stdin 逐行解析三态（Request/Notification/Response，未匹配 Response 转发）；`send_request` id 配对 + `send_response` Ok/Err 两形态 + `send_notification` stdout 写入形态；并发 5 报文交错 + 乱序 id 匹配；非法 JSON/空行跳过（不中断解析）；EOF → pump 退出、`recv()` 返回关闭；`send_request` 无内置超时（挂起语义）；stdout 断裂 → `-32603 broken pipe` 失败语义；**id 保真问题以测试标注**（结论见 §10 决策点 7）。
- [x] **wire capture 基线**（`host/unify_wire_baseline_test.rs`，5 条）：`AvailableCommandsUpdate` 经 notify 路径（真实驱动 `send_available_commands_update` + MockTransport 捕获）与 stdio 路径（`SessionNotification` 序列化）最终 wire JSON **逐字段一致**——外层 `{sessionId, update}` 结构、`sessionUpdate: "available_commands_update"` 判别字符串、`_meta` 省略规则（未协商 skillNames 时 update 级省略；条目级 periKind/periLevel 恒有）、camelCase 命名；`SessionInfoUpdate` 同法（含 title 缺省省略规则，updatedAt 掩码归一）；`build_initialize_response` wire 基线（顶层 `{agentCapabilities, authMethods, protocolVersion}`、`_meta.peri.*` 回显、session 生命周期能力齐全）。两通知路径确认共享 `dispatch::build_available_commands_update` / `dispatch::build_initialize_response`，本轮锁定的是**外层封装一致**。
- [x] 验证：`cargo test -p peri-acp --lib`（447 全绿）+ `cargo clippy -p peri-acp --all-targets -- -D warnings`（零警告）+ `cargo fmt --check` 通过。

### 批 4 完成清单（2026-08-17，收尾）

- [x] **broker 提问超时兜底恢复（批 3 未决风险 1）**：`parse_ask_user_timeout` / `ask_user_timeout` 归位 `broker/transport_broker.rs`（语义归口 broker）；统一构造点 `host/prompt.rs` run_prompt 内 `AcpTransportBroker::new(...)` 恢复 `.with_timeout(ask_user_timeout())`。env 语义与旧 stdio `host/stdio/context.rs` 完全一致：缺失/非法 → 默认 300s，`0` → None。单测 2 条（纯逻辑四形态 + env 接线 serial）。注：统一构造读 env 后，TUI 不设 env 时也获得 300s 默认兜底（旧 TUI 为 None）——TUI 本地客户端恒响应，实际交互无感知；TUI 显式设 env 会获得对应超时（显式配置的合理语义，构造点注释已标明）。
- [x] **id 越界压 0 收口（§10 决策点 7 落地）**：`transport/stdio.rs` pump 对入站 Request/Response 的域外 id（小数 / u64 溢出 i64 / null / bool 等）**拒绝该消息**（`tracing::warn` + 丢弃该行，pump 不中断，与非法 JSON 行处理语义一致），不再压 0 转发；id 为 null 的 Request 按 JSON-RPC 2.0 §2.2 视为通知（`IncomingMessage::Notification`）。`send_request` 侧（id 由内部 `RequestId` 生成恒合法）不改。测试：`test_request_id_domain_validation` + `test_pump_rejects_out_of_domain_ids_and_null_id_becomes_notification`（原压 0 测试改写）。
- [x] **依赖评估与清理**：`parking_lot` 的 `send_guard` feature 移除（批 2 为 stdio typed handler 层 `await_holding_lock` 规避引入，批 3 删除 adapter 后全 workspace 无 `send_guard`/`SendGuard` 引用；`cargo clippy -p peri-acp --all-targets -- -D warnings` 无跨 await 持锁告警，`cargo check --workspace` 通过）。**用量收敛评估结论：`agent-client-protocol`（15 文件）/`agent-client-protocol-schema`（6 文件）仍保留**——prompt/notify/requests/dispatch/broker/unify_wire_baseline_test 持续使用（决策点 1）。peri-acp 其余依赖（tokio/serde/parking_lot/dashmap 等）逐一核有用例，无因批 2-3 删除而闲置的依赖。遗留未删：`StdioEventSink`（`session/event_sink.rs:425`）与 `ConnectionTo<Client>` 的 `RequestTransport` impl（`transport/mod.rs:87`）为批 3 后无调用方的死代码残留，后续单独清理。
- [x] **文档事实源更新**：`docs/code-index/peri-acp.md`（服务入口/速查表 transport 行/broker 行/通知基线行/stdio 部署行 → 批 3 后单一路径事实；`StdioEventSink` 行保留——代码仍存在）；`docs/top-level.md` §7 增补部署单元事实（stdio 部署单元 = `run_acp_stdio(StdioInput)` → `run_acp_server`）。过时注释清理：`transport/stdio_test.rs`（id 压 0 基线 → 域外 id 拒绝基线）、`host/unify_wire_baseline_test.rs`（批 2 `StdioNotifyTransport` / `prompt_exec.rs` 引用 → Value 直发 + schema typed 参照线事实）。
- [x] 验证：`cargo check -p peri-acp`、`cargo test -p peri-acp --lib`（479 全绿，unify_wire_baseline_test 7 条 + host/stdio `run_server_integration_test` 7 条保持绿）、`cargo clippy -p peri-acp --all-targets -- -D warnings`（零警告）、`cargo fmt -p peri-acp --check`、`cargo check --workspace` 全部通过。

## 10. 风险与决策点（v2 修订）

1. **依赖不能因删 stdio 而删**（推翻 v1 风险 1 的预设）：`agent-client-protocol`/`agent-client-protocol-schema` 在**统一路径**中大量使用——`host/notify.rs`（`SessionUpdate`）、`host/prompt.rs`（`PromptResponse`/`StopReason`）、`host/requests/session_lifecycle.rs`（schema 类型）、`dispatch/init.rs`、`dispatch/commands.rs`。删除 stdio typed handler 后仅是 `Client`/`ConnectionTo`/`Responder`/typed handler 的 `agent-client-protocol` crate 用量收敛，是否可删 `transport/mpsc.rs` 不含依赖（已核实独立）。依赖边界调整放批 4，单独评估。
2. **session/cancel 语义合并**：stdio（直接 token.cancel，无 lease/continuation）→ notify（lease 检查 + continuation 武装）。需确认 IDE 客户端取消行为（是否依赖「取消即停止无续跑」）。
3. **legacy `type:cancel`**：与标准 `session/cancel` 并存语义（session 范围、权限、竞态）必须明确定义并纳入 wire capture。
4. **prompt 引擎合并回归面**：最大风险点；批 3 前完成 §7 全部差异对齐，合并后先手动验证。
5. **`session_start_source`**：批 2 前核实 `host/prompt.rs` 是否消费；未消费则按 stdio 语义补入。
6. **新增能力是行为扩展**（D 标准）：IDE 客户端可能面临意外新通知/新 RPC，需在发布说明标注。
7. **id 保真结论（批 4 已落地收口）**：agent-client-protocol-schema 的 `RequestId` = `Null | Number(i64) | Str(String)`（`rpc.rs:42`，JSON-RPC 2.0 §5）；`transport::types::RequestId` = `String | Number(i64)`（无 Null）——内部域已覆盖可表示子集。批 0-3 对域外值（小数、u64 溢出 i64、null、bool 等）**静默压 0**（`as_i64().unwrap_or(0)`），非保真且与合法 id 0 存在碰撞风险（router 配对/宿主 `send_response(0, ...)` 将响错对象）。**最终落地（批 4）**：`transport/stdio.rs` pump 对入站 Request/Response 的域外 id **拒绝该消息**（`tracing::warn` + 丢弃该行，不中断 pump，与非法 JSON 行处理语义一致），不再压 0 转发；id 为 null 的 Request 按 JSON-RPC 2.0 §2.2 视为通知（`IncomingMessage::Notification`——客户端无兴趣于对应响应）。`send_request` 侧（id 由内部 `RequestId` 生成恒合法）不改。风险面评估：仅影响协议违规输入（合法 IDE 客户端 id 恒为整数/字符串），不触及合法报文；`StdioTransport` 出站 id 恒为域内 Number。测试锁定：`stdio_test.rs` 的 `test_request_id_domain_validation` / `test_pump_rejects_out_of_domain_ids_and_null_id_becomes_notification`（原压 0 测试改写）。EOF 后 pending 挂起是当时保留的历史基线；2026-08-24 已由 ARC-TRANSPORT-001 闭环，测试改为 `test_pending_request_fails_after_eof`，并覆盖 read error、writer failure、caller abort 与 blocked writer/mutex waiter。
