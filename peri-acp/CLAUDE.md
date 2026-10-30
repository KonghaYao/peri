# peri-acp

## Scope

`peri-acp` 负责 ACP 服务层：session 生命周期、prompt 构建、Agent 装配入口与事件映射/发送；不实现 TUI 组件。中间件链序蓝本位于 Agent 层（见 `../peri-agent/CLAUDE.md`），具体装配位于 `../peri-middlewares/src/assembly.rs`；本层构造宿主装配上下文。Langfuse 观测由 `peri-controller` 的事件流旁路消费者承担（见 `../peri-controller/`），本层仅在事件协议化前分支调用 bridge（`src/event/forwarder.rs`）。

## 数据流

`ACP request → SessionManager → frozen session data / prompt → Agent 层 session 工厂（装配）→ run_react_loop → ExecutorEvent → event mapper / event sink → SessionUpdate 或扩展通知 → client`。Langfuse 经 `peri-controller` 的 `LangfuseBridge`（事件流旁路消费者）在协议化前分支消费事件；不改变客户端事件路径。

## 任务路由

| 任务 | 优先读取 |
| --- | --- |
| session、事件、Prompt、工具、中间件、secret | `../docs/standards/architecture-contracts.md` |
| Rust、async 与 doc tests | `../docs/standards/rust.md` |
| 测试位置与覆盖要求 | `../docs/standards/testing.md` |
| middleware 具体链顺序 | `../peri-middlewares/CLAUDE.md` 与 `../peri-agent/src/session/factory.rs` |
| TUI 通知消费 | `../peri-tui/CLAUDE.md` 与 `../docs/standards/tui.md` |

不通过导入扩展默认上下文；需规则时按表显式读取。

## 稳定不变量

- ACP 只驱动当前进程内执行，不提供持久 execution admission、Work/control 协议或冷恢复，不要求 SDK reverse admission，不向工具继承执行 token。session/load 与 resume 仍校验 Store 访问模式、持久化状态、binding、保存目录与 frozen；不能执行的环境明确失败，独立 history RPC 保持可读。显式 close intent、prompt 排空、SessionEnd、MCP/task 当前资源生命周期与普通关闭的持久化结清保持独立。

- 配置定义、typed 解析、provider/profile 默认值、分层合并与差异保存归 `peri-config`；`provider/{config,store}.rs` 仅 re-export。正常 `peri_config::settings::ConfigSource` 持有 `ConfigurationSystem`，固定 scope/layout，并提供 snapshot、显式 reload 与 CAS 保存；布局或校验失败不得误写全局层。见 `../peri-config/CLAUDE.md`。
- 配置输入经独立 bootstrap `peri-mcp-config` 采集，不依赖待配置的 session 工具池，不新增 daemon/model 工具；环境来自选中的 source provider，不读计算宿主 fallback。核心不反向依赖 ACP 业务实现。
- 宿主创建的 MCP pool 在初始化前绑定同一 `ConfigSource` snapshot；host Langfuse 使用该快照的 typed projection，LLM adapter 消费 core `ResolvedProvider`。保存保留同文件兄弟域；失败不 publish，显式 reload 不热替换已初始化 pool 或已冻结 session prefix。
- workspace 资源输入使用 `snapshot.resources().disable_bundled_skills`，不重新读取全局关闭位；技能回归 fixture 必须使用与生产 source 一致的选中 global 路径。需要新值时显式 reload 并重新取得 snapshot，旧 pool 固定旧 Arc，没有 hot watcher。lenient 无 authority 仅临时可读，不可写。
- `ConfigSource::save(expected_revision, &PeriConfig)` 返回 accepted snapshot；持久 configOptions / update_config 必须 candidate → 验证 → 保存成功 → 从 accepted snapshot 发布，失败不更新 live provider / agent cache、不 notify、不返回成功。revision 在编辑开始捕获，不得提交时换成最新版；远程 wire 的基线 token 仍需审计。
- `SessionManager` 在每条 session/new、load、resume 或 fork 路径注册 session caps；发送扩展事件前按该 session 的 caps 门控。
- MCP 运行控制统一走 `peri/mcp/control` 与当前连接协商的 `peri.mcpControlV1`；按显式 session 定位已建立环境，禁止未知 session 回落启动池。`mcp/list` 投影服务端可管理性，控制业务与清理 owner 归 pool；不提供模型动态注册工具。契约见 `../docs/design/acp-mcp-control.md`。
- `session/load` 无 `cursor` = 初始 load、带合法 `cursor` = 历史页：模式在进入生命周期路由前由 `host/history_page.rs::classify` 唯一解析，锁策略与响应后 setup 都由模式决定。历史页不取执行生命周期锁、不做绑定 / MCP 接纳 / 广播 / 续跑，也不在全局 sessions 锁内做存储 I/O；未协商却带分页字段即拒绝，不降级为初始 load。扩展能力按**当前连接**的 `ConnectionContext::caps` 判定，不读会话级 registry（重连后能力重新协商）。
- `SessionState::structure_version` 只在 rewind 持久化提交**成功**时 `+1`（恰好一次，失败分支不得触碰）：它让已发出的历史游标在存储委派之前失效，客户端处置与「锚点被删」同为 `-32012`。回放投递失败使 `session/load` 整体失败（`-32603`，不返回半回放成功，也不跑成功路径后处理）：已发布的会话态保持完整并留给下次 load 经 `AlreadyLive` 原位修复。初始 load 的代次（客户端可选或服务端生成）经 `_meta.periLoadGeneration` 同时出现在成功响应与本次全部重放（含 reminder 出口），门控仍是 `caps.replay`。
- 新增 `ExecutorEvent` 或 ACP 扩展事件时，覆盖发射、ACP mapper/forwarder、caps 门控（如适用）和客户端消费；不能只增加枚举或单一发送点。
- 给 Hub/Web 的事件投影必须从 canonical event 映射为版本化 allowlist DTO；不得复用包含消息、路径、输出或错误正文的 TUI 私有 `event_json`。`peri.agentActivity` 是该安全摘要面，legacy `peri.agentEvent` wire 保持独立兼容。
- session 创建时构建并复用 frozen 数据；Prompt 与 SubAgent 不得在会话中途重读导致前缀漂移。`env` 正文独立冻结，模型桥接层在所有 contribution 后追加并填入当前执行 session ID；V3 快照保存该字段，旧主提示词不改写；旧会话派生执行经 `prepare_derived_environment` 从冻结输入准备 env，V3 显式缺席与 V1 外部指令拒绝边界保持。
- `{{available_agents}}` 目录行只从资源面投影渲染；`description` 单行化、长度有界并标注 `untrusted metadata`，不得改变系统指令。默认职责交接与直接执行例外由 `11_subagent` 策略定义，支持探索后交接实现的串行委派；`multitask` 加载后升级为全委派。角色完整职责经 `agent://.../agent.md` 资源读取。
- ACP `peri.instructions` 只在 `session/new` 准入，作为独立 frozen 字段持久化；内部身份重渲染与外部正文组合遵循 `ARC-EXTERNAL-INSTRUCTIONS-001`。
- 生产中间件顺序以 Agent 层 session 工厂的链序蓝本为事实源（`../peri-agent/src/session/factory.rs` 的 `production_blueprint`），未经完整验证不得重排。
- Langfuse 事件只经 `peri-controller` 的 `LangfuseBridge` 统一映射进入 tracer（协议化前分支，不参与业务链路）；日志、错误和遥测不得泄露 secret。
- stdio/MPSC transport 的 pending request 由 router 统一持有：response、caller cancellation 与 terminal close 至多结算一次；终止结算当前和后续请求，连接静默不引入隐式 timeout（ARC-TRANSPORT-001）。
- `--bare` 会话仅装配 builtin `workspace` MCP 池，保留基础文件/终端及后台任务能力；跳过用户插件、外部 MCP 和 settings hooks。`PERI_MCP_BUILTIN=off` / `0` 仍显式关闭注入。
- builtin MCP 实例上下文须在 `McpClientPool::run_initialize` 前装配，后置注入会被拒绝。生产 Workspace 的后台 Bash 由 MCP 自行持有；会话侧任务投影与取消路由的目标设计见 `../docs/design/session-async-tasks.md`。
- Cron scheduler 属于会话环境：builtin 工具、宿主端口与 session bridge 使用同一份实例；共享会话注册表时只共享 continuation 入口，不覆盖 scheduler。部署的 tick 开关传入会话池，由 builtin supervisor 唯一驱动并随会话关闭。
- Host 后台任务由 non-Clone `HostTaskOwner` 持有，config/task 只持 weak `HostTaskSpawner`；MCP concrete owner 属 middlewares，ACP config 只能持 `peri-acp-types::ports::McpTaskOwnerPort`，禁止直接依赖 concrete type。transport EOF 关闭准入后取消并 drain local/manager 会话 ID 并集，再在锁外关闭 MCP。Host drain 或 MCP service-close report 超时必须报告 `Incomplete` 并保持 Closing，不得当作已经 join/Closed（ARC-HOST-SHUTDOWN-001）。
- 会话 setup（`session/new` / `load` / `resume` / `fork`）里的 `mcpServers` acp 型声明在响应写入后由 `host/requests/acp_mcp.rs` 受理（`attach_session_servers`）；客户端须先拿到会话结果，受理顺序不能提前到响应之前。会话级服务持有声明（每个会话一个 MCP 池），入站 `mcp/message` 通知按连接级 `AcpMcpRegistry` 的唯一归属定位会话，跨会话 serverId 重名在 setup 前以 `CODE_UNAVAILABLE` 拒绝；未知或已关闭的 server 返回 `-32001`，内层 MCP 错误码原样透传；会话终结在 MCP 池关闭前调 `AcpMcpServerPort::close_session`（幂等）。发现是后台的：不阻塞会话建立，失败留在 MCP 池状态面；协议层无 `mcp/connect` / `mcp/disconnect`（ARC-MCP-ACP-001）。

## 目标命令

```bash
cargo check -p peri-acp
cargo test -p peri-acp --lib
cargo test -p peri-acp --lib -- host::task_scope
cargo test -p peri-acp --lib -- acp_mcp
cargo test -p peri-acp --lib -- history_page
cargo test -p peri-acp --lib mapper
cargo test -p peri-controller --test langfuse_e2e
cargo test -p peri-acp --doc
```

## Verify

- session/caps 改动：运行相关 crate 测试，并人工检查所有创建、加载、恢复、fork 入口均在 session 就绪后注册 caps；`session/load` 分页改动另跑 `cargo test -p peri-acp --lib -- history_page`，并确认分页路径未取得生命周期锁、未在全局 sessions 锁内做存储 I/O。
- 事件改动：运行 mapper 测试，并人工沿服务端发送点到 TUI/stdio 客户端检查新增事件覆盖；现有 mapper 测试不自动证明全链路完整。
- Prompt、middleware 或 Langfuse 改动：按 `ARC-FROZEN-001`、`ARC-MIDDLEWARE-001`、`ARC-SECRET-001` 逐项核对。
- 配置规则改动：先读 `../peri-config/CLAUDE.md` 与 `../docs/design/configuration-authority.md`；领域回归在 core，ACP 验证消费与装配接线。插件生命周期、hook 格式及存储/执行 credentials 仍遵守专属能力边界。
- 触碰 builtin `workspace` 资源面的测试（agent / 技能 / 指令 / meta 文档）：用例依赖 `PERI_MCP_BUILTIN` **默认态**，而该进程级 env 由开关组用例在 `#[serial]` 临界区内改写 ⇒ 读侧必须同键 `#[serial]`（`TEST-HERMETIC-001` 读侧闭合；漏标时并行窗口内池无 `workspace` 句柄，资源面静默缺席，症状是内容断言拿到 `None`/空）。
