# 架构模块边界与交互合规扫描

状态：审计完成，发现待整改；不是架构验收通过记录。

## 范围与裁决依据

- 日期：2026-09-30；工作树基线 HEAD 为 `d6a0dc3d`，含既有未提交修改。
- 扫描对象：workspace crate 依赖、TUI/ACP/Agent 主链、Middleware 能力接口、MCP 工具桥接、SessionResources 关闭权、Controller/Runtime cancel、Workflow RPC，以及架构门禁。
- 规则事实源：[标准索引](../../docs/standards/index.md)、[架构契约](../../docs/standards/architecture-contracts.md)；按 `STD-INDEX-002` 区分实现违规、文档过时和已批准迁移。
- Session 恢复按[已批准的 Session ID/env 目标](../../docs/design/session-id-environment.md)和[迁移任务](2026-09-30-session-id-environment-core-change.md)裁决，不要求重新加回已批准删除的 owner/文件锁。
- 审计期间工作树持续发生其他修改；测试和扫描对应各自执行时的工作树，不构成一个不可变发布快照的验收。
- 续扫交付核对时 HEAD 已变为 `aed6d883`；本任务没有执行 commit。F4/F5/F6 的三个事实源哈希再次核对未变化，其余现有修改仍保留。
- 本任务不修改生产实现。两项临时 Compact 探针、三项临时 MCP 生命周期探针执行后已移除；门禁探针在 `/tmp` 隔离副本执行。本文件只承载待办及证据，不进入 `docs/design/`。

## 结论

依赖门禁当前返回 20 条规则、0 条违规，但不能据此认定实现合规：已经确认一个跨执行环境的数据访问缺口、一个 canonical 事件边界违规，以及门禁自身的两种漏报。

TUI prompt 主路径经 ACP；hook 的窄能力接口和生产链序有定向测试证据。Session 恢复迁移、cancel 目标接线以及未运行的跨层生命周期用例不能计作完成。

续扫（2026-09-30T10:21:22Z）新增三个 P1：MCP-over-ACP 撤销过程跨会话暴露（F4）、会话关闭取消后重试丢失清理（F5）、未登记桥接任务在 owner/pool 宣告 Complete 后仍存活（F6）。三项均有确定性探针，不是仅凭代码形状推断。

## F1 / P1：Full Compact 重新注入绕过 MCP 与执行环境能力

**规则关联**：根指引的 Resources/MCP 能力边界；`ARC-CAPABILITY-CLOSURE-001`；`ARC-TOOLS-001` 的 session-local 能力与绑定来源要求。

**实际链路**：`full_compact_inner → collect_reinject_v2 → read_file_with_budget → spawn_blocking(std::fs::read_to_string)`。

代码入口：`peri-agent/src/agent/compact_v2/full.rs` 的 `collect_reinject_v2`、`read_file_with_budget`、`extract_recent_files`、`extract_skills_paths`。

观察：

- Agent 从历史 Read 参数或 `[Skill: ...]` 行提取路径，直接读取计算进程所在机器的文件，再构造 Human 消息加入 transcript。
- 该 seam 只接收 transcript、CompactConfig 和 cwd，没有执行环境身份、MCP/resource reader 或当前能力关闭策略。
- Read 候选按工具名字解析，没有核对调用的绑定来源；裸名 `Read` 也会被采纳。它不足以区分受信 workspace 工具与同名外部 system MCP 工具。
- 这不是 frozen system prefix 被修改的问题；新增内容进入 Human 历史，但数据获取仍绕过工具环境边界。

**复现证据**：临时集成探针 `architecture_boundary_probe_20260930` 的两个场景均通过：没有装配 MCP、没有执行 Read、没有提供执行环境 capability，仅构造历史调用/技能标记，就能注入本机文件和本机 Skill 正文。探针证明 seam 的本机 I/O 行为，不等于已完成跨机器部署或完整关闭矩阵实验。

最小重现核心（置于临时 `peri-agent/tests/` 测试文件，并按现有测试依赖导入）：

```rust
use peri_agent::agent::compact_v2::{config::CompactConfig, full::re_inject_v2};
use peri_agent::messages::{BaseMessage, MessageContent, ToolCallRequest};
use peri_agent::session::MessageTranscript;

#[tokio::test]
async fn probe_local_read_without_execution_capability() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("recorded-file.txt");
    std::fs::write(&path, "local-machine-only-marker").unwrap();
    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::ai_with_tool_calls(
        MessageContent::text("historical read"),
        vec![ToolCallRequest::new(
            "historical-call", "Read", serde_json::json!({ "file_path": path }),
        )],
    ));
    let result = re_inject_v2(
        &mut transcript, &CompactConfig::default(), directory.path().to_str().unwrap(),
    ).await;
    assert_eq!(result.files_injected, 1);
    assert!(result.messages.iter().any(|message| {
        message.content().contains("local-machine-only-marker")
    }));
}
```

Skill 场景将历史改为 `BaseMessage::human(format!("[Skill: {}]", path.display()))`，路径采用 `skills/audit-skill/SKILL.md`；未装配资源 provider 仍读取并注入本机正文。

**影响推断**：工具环境与计算进程不在同一机器时，相同绝对路径可能读取错误机器的数据；能力已关闭或外部同名 Read 获胜时，这个路径也缺少相应拒绝依据。尚未做真实跨机复现，不宣称已观察到数据外泄。

待整改及验收：

- [ ] 保留重新注入业务语义，但经会话绑定的 MCP/resource 读取能力获取数据，不回落计算宿主磁盘。
- [ ] 文件候选核对实际绑定来源；同名外部 Read 不得被当作 builtin workspace Read。
- [ ] 无能力、能力关闭、环境不可用时不偷偷读取本机同名路径；定义并验证可选重新注入的降级行为。
- [ ] 加入完整 Compact 路径回归：远端内容与本机同名内容不同、workspace/skills 关闭、外部 Read 同名、取消与读取失败。

## F2 / P2：Agent 仍构造 v1 ExecutorEvent，v2 还开放任意旧事件包裹

**规则关联**：`ARC-EVENT-001` 明确要求 Agent 从 EventBus 发射 v2，禁止 Agent 构造 v1 `ExecutorEvent`，兼容映射只服务协议化边界。

确认的生产路径：

- `peri-agent/src/agent/stages/receive.rs::run_receive` 构造 `ExecutorEvent::SystemReminder`，包入 `StateEvent::ProtocolEvent`。
- `peri-agent/src/session/retry_events.rs::translate_observation` 构造 `ExecutorEvent::LlmRetrying`，直接交 `AgentEventHandler`。
- `peri-agent/src/session/subagent/background.rs` 的完成路径构造 `ExecutorEvent::BackgroundTaskCompleted`，直接写 v1 sender。
- `peri-acp-types/src/event_v2/types.rs::StateEvent::ProtocolEvent` 接受任意 `ExecutorEvent`；`executor_mapping.rs::state_event_to_executor` 原样返回该载荷。

**影响**：表面采用 v2 envelope 不能消除内部 v1 生产；旧事件可以绕过专用 v2 payload 与受限映射。当前发现是明确的架构契约违规，未据此宣称已经发生事件丢失或错序。

待整改及验收：

- [ ] Reminder、retry、后台完成等业务生产点发射专用 v2 事件。
- [ ] 移除任意 `ProtocolEvent(ExecutorEvent)` 通道，协议边界穷尽映射为既有 wire DTO，保持外部兼容义务。
- [ ] 覆盖 root/subagent 身份、后台完成顺序、retry diagnostic、live/replay Reminder 和 terminal 前排空。
- [ ] 增加 Agent 禁止生产 v1 的自动检查；不能把大范围 executor 豁免当作迁移完成。

## F3 / P2：依赖门禁对合法 Rust 导入写法漏报

**规则关联**：[总体分层](../../docs/design/architecture.md)的禁止未声明边，以及架构依赖门的验收目的。

入口：`scripts/check-layer-imports.sh` 的 grep/导入行排除逻辑；`scripts/import-exemptions.conf` 的 import 正则。

在隔离目录复制脚本与规则，向 `peri-runtime/src/audit_fixture.rs` 分别放入以下内容：

| 输入 | 应有结果 | 实际结果 |
| --- | --- | --- |
| `use peri_acp::transport::AcpTransport;` | 拒绝 Runtime → ACP，退出 1 | 退出 1，阳性对照有效 |
| `use ::peri_acp::transport::AcpTransport;` | 拒绝，退出 1 | 退出 0，报告 20 规则 / 0 违规 |
| `extern crate peri_acp as forbidden_acp;` 加 `use forbidden_acp::transport::AcpTransport;` | 拒绝，退出 1 | 退出 0，报告 20 规则 / 0 违规 |

第一种漏报原因：import 正则只匹配 `use peri_*`，而全路径规则把所有 `use` 行排除。第二种漏报原因：没有解析 `extern crate` 别名，也不检查 Cargo 依赖边。

以上是检测器输入实验，不表示当前业务代码已经采用这些写法规避检查；它证明现有绿灯不足以保证未来改动不越层。既有路径子串豁免也不是路径内新增业务逻辑的语义审计。

待整改及验收：

- [ ] 至少覆盖绝对导入、重命名、`extern crate` 别名和 multiline/grouped use，并建立正/负样例回归。
- [ ] 增加基于 `cargo metadata` 的 crate 依赖边检查，区分生产、dev 与平台条件依赖；仍保留获批装配例外的精确范围。
- [ ] 源码检查与 manifest 检查分别报告覆盖面；不将 re-export 包装和字符串匹配视为语义隔离证明。

## F4 / P1：撤销 ACP server 时先丢归属，等待清理期间变成全会话可见

**规则关联**：`ARC-MCP-ACP-001` 要求工具、发现和状态按声明会话隔离；关闭也不能将会话资源临时降格为部署共享资源。

代码入口：`peri-middlewares/src/mcp/client/lifecycle.rs` 的 `remove_acp_servers_for_session`、`remove_server`、`is_visible_to_session`；`client.rs` 的 `get_all_clients_visible_to`。

实际顺序：

1. `remove_acp_servers_for_session` 先从 `acp_owners` 删除 server 归属。
2. `remove_server` 先 await 该 server 的 subscription task 停止，再从 `clients` 移除句柄。
3. `is_visible_to_session` 对没有 owner 的 server 返回 true；在步骤 2 的等待窗口，旧 Connected 句柄被当作所有会话都可见。

**确定性复现**：真实 pool + AcpMcpService + rmcp 桥接建成 s1 的 server，s2 初始三面均不可见。给该 server 注册一个 pending subscription task，在 current-thread runtime 中将 removal future poll 到 stop/join 等待点；此刻 s2 的 `get_all_clients_visible_to`、`build_tool_bridges_visible_to`、`all_server_infos_visible_to` 均出现 s1 server。随后继续 await removal 并正常关闭，测试没有留下后台任务。

探针名：`removing_an_owned_server_temporarily_exposes_it_to_another_session`。

**观察边界**：已证明工具目录与状态投影跨会话暴露；未执行跨会话工具调用，不宣称已经观察到未授权副作用或多用户认证突破。

待整改及验收：

- [ ] 先原子撤销可见的 clients/catalog，再清理；或者保留 owner/closed tombstone 到所有可见句柄移除完成。不得用“owner 缺失即全局可见”表示会话撤销中的资源。
- [ ] 不在隔离判定之外增加第二份会话归属规则；审查单句柄、列表、工具桥接、资源/agent/skill 发现的相同撤销窗口。
- [ ] 回归覆盖 subscription 清理等待、并发 catalog refresh 和取消；s2 在整个撤销期间持续不可见，而不是只断言删除前后。

## F5 / P1：close_session 提前取走唯一清理上下文，取消后重试直接返回

**规则关联**：`ARC-HOST-SHUTDOWN-001` 的取消/重试不丢失 owner 与真实清理证据；`ARC-MCP-ACP-001` 的会话连接完整断开及幂等关闭。

代码入口：`peri-middlewares/src/mcp/acp/session.rs::AcpMcpService::close_session`；调用方 `peri-acp/src/host/workspace.rs::SessionEnvironment::shutdown`。

观察：close 在首个 await 前删除 `state.sessions` 和该会话的全部 connection 路由，将 gateway/server IDs/bridge handles 仅留在本次 future 的局部变量里。随后逐个 await task stop、disconnect 和 pool removal。future 被取消时，后续重试找不到 session record，立即返回；宿主把重复调用当作安全幂等，但实际没有续接原清理。

**确定性复现**：同一 s1 建立两个 ACP server；对端在第一个 `mcp/disconnect` 用显式闸门阻塞。等进入 disconnect 后 drop close future，再调用 close_session：两条入站路由都消失，但 disconnect 总调用数仍为 1，另一个连接没有收到 disconnect；pool 仍把两条句柄标为 Connected，s1 的工具桥接仍有两项。最后单独关闭 pool/owner 回收探针资源。

探针名：`cancelled_session_close_retry_leaves_a_connected_pool_entry`。该探针使用两个 server，避免仅凭单连接的对端确认未知来判断整组清理丢失。

待整改及验收：

- [ ] 路由准入立即撤销，但 cleanup owner 移交给保留的单一 close transaction，不由调用者 future 独占。
- [ ] 取消、并发和重复调用继续观察同一清理；取消不能跳过未处理的连接、池移除或后台任务 join。
- [ ] 区分 Complete/Incomplete；没有完整证据时不得仅因 session record 不存在就表示成功。
- [ ] 回归覆盖两个以上连接、disconnect 阻塞、stop/join 等待、pool removal 等各 await 点；不让一个 caller 的取消破坏后续 host close 重试。

## F6 / P1：桥接 runner 和出站请求任务不归 owner，Complete 后仍持有在途 I/O

**规则关联**：`ARC-HOST-SHUTDOWN-001` 的强 owner、关闭准入、abort/drain 与 Complete 证据；`ARC-TRANSPORT-001` 区分连接静默与 terminal，不允许用通用请求 timeout 替代生命周期结算。

代码入口：`peri-middlewares/src/mcp/acp/session.rs::connect_server` 的 `tokio::spawn(runner.run())`；`peri-middlewares/src/mcp/acp/transport.rs::BridgeRunner::run` 的逐请求 `tokio::spawn`。

观察：两处 spawn 都丢弃 JoinHandle，不登记在 McpTaskOwner。bridge close 只使 runner 的下一次 select 退出；逐请求任务没有监听这个 close。runner 还会直接 await 对端 notify，关闭信号不会中断已经进入的那个 await。

**确定性复现**：真实 attach/桥接发起 initialize，对端闸门保持请求 pending，并用 Drop guard 记录正在执行的 gateway future。close_session 后，按 `pool.begin_shutdown → owner.begin_shutdown/shutdown → pool.shutdown` 收尾；owner 返回 Complete、pool 返回 Complete，但初始化 gateway future 的计数仍为 1。只有释放对端闸门，这个独立请求任务才消失。测试最后等待计数归零，不让“复现任务残留”变成测试自身泄漏。

探针名：`bridge_request_stays_live_after_owner_and_pool_report_complete`。直接复现的是出站 request；notify 的 await 关闭盲点是代码观察，尚未单独做运行时探针。

待整改及验收：

- [ ] runner 与每个出站 request/notify 均纳入可取消、可 join 的连接子 scope，并由 deployment-held owner 持有终态证据。
- [ ] terminal close 关闭后续准入并结算 pending；直到子 scope 已确认 drain，才允许向宿主投影 Complete。
- [ ] 不让 pool 反向持有捕获 pool Arc 的任务，仍保持既有 weak spawner/强 owner 方向。
- [ ] 回归覆盖 initialize、tools/list、tools/call 与 notify 阻塞；关闭后 guard 计数必须为零。保留正常静默连接的语义，不加隐式通用请求期限掩盖残留。

## 已批准迁移与已有治理项

### M1：cancel 目标接线未完成，不误报为未经批准的新架构

`ARC-CANCEL-001` 明确允许 ACP 内部 SessionManager 作为过渡路径。Controller/Runtime 确实有三元组转发接口，但生产注册的 `peri-acp/src/host/prompt_handle.rs::PromptHandle::cancel` 是 no-op；`server_loop` 的 `session/cancel` 仍走 notification/state token 路径。

因此 Controller/Runtime 单测或 `register_session` 的存在不能证明生产 cancel 已经贯通目标链路。需要以真实 ACP 输入验收三元组、`clear_queue`、policy、迟到取消和唯一终态；本轮不要求为迁移另造第二套取消语义。

### M2：Session ID/env 迁移与旧契约测试尚未同步

`cargo test -p peri-resources --test session_resources_contract` 实际为 5 passed / 1 failed：`test_contract_owner_is_exclusive_across_processes` 的子进程取得了旧测试预期互斥的 ownership。

工作树正在移除 session 执行锁；[已批准目标](../../docs/design/session-id-environment.md)明确不保证跨机器/实例单 owner。此失败按迁移任务复核与更新测试，不能作为恢复旧锁的依据，也不能忽略后宣称存储契约全绿。本次未修改该测试或锁实现。

续扫时 `ARC-WORKSPACE-001` 和 Session ID/env 设计已被现有迁移更新为现行实现。上述失败是第一轮命令的历史证据，不是当前工作树重新执行结果；本轮没有重复存储契约测试，不据此断言迁移尚未实现或当前仍失败。

### G1：文档冲突裁决存在已知过时描述

`docs/design/README.md` 仍声明“代码与测试 > standards > design”的统一优先级，与 `STD-INDEX-002` 的现状/目标分离不一致。此项已由[此前云架构审计](2026-09-27-v4-cloud-architecture-audit.md)登记；不重复建立另一份权威规则。

### G2：文件规模门禁仍不通过

扫描开始时 31 个超限文件；2026-09-30T09:47:51Z 附近复扫为 29 个：源码 1、测试 28。唯一源码超限为 `peri-agent/src/agent/stages/mod.rs`，1011 行。期间其他任务拆分了会话相关文件，数量变化不归因于本审计。

按 `STD-SIZE-001`，存量治理单独推进，不能称全库规模达标。本次没有修改这些既有超限文件。

## 契约覆盖矩阵

“局部有证据”不等于整个契约通过；未做专项验收的规则明确保留。

| 契约 | 本轮证据与状态 |
| --- | --- |
| ARC-BOUNDARY-001 | 主 prompt/取消调用走 ACP client/transport；装配例外保留，依赖门不能替代语义检查 |
| ARC-WORKSPACE-001 | 现有迁移已更新标准；第一轮旧 owner 契约失败只作当时证据，本轮未复验，见 M2 |
| ARC-CANCEL-001 | 目标接口存在，生产 cancel 未贯通，见 M1 |
| ARC-OUTPUT-COMPLETION-001 | 未重跑 provider 截断、stream interruption 与真实 CLI exit 用例 |
| ARC-FROZEN-001 | 指引 adapter 只持有冻结字符串；未完整验证冷恢复、fork 与持久化补偿 |
| ARC-COMPACT-001 | Full 子模块 26 测试通过；另有 F1，旧业务测试不证明执行环境隔离 |
| ARC-SESSION-LOAD-001 | 未重跑 TUI reservation 与 shutdown/replay 竞态用例 |
| ARC-EVENT-001 | event_v2 56 测试通过，但 F2 明确违规；映射测试不能证明生产点统一为 v2 |
| ARC-TOOLS-001 | session-local 工具视图与 Reason 发布顺序静态核对；MCP bridge 18 测试通过，F1 的路径来源仍需整改 |
| ARC-CAPABILITY-CLOSURE-001 | assembly 关闭矩阵通过；未覆盖 Compact 重新注入旁路，见 F1 |
| ARC-HITL-001 | 装配关闭面有证据；未重跑完整 broker 门、stale UI 与 permission 网络分类 |
| ARC-STDIO-001 | 统一 host 入口静态核对；未重跑完整 stdio 集成套件 |
| ARC-TRANSPORT-001 | router 13 测试通过；未重跑 stdio/MPSC pump 终止套件 |
| ARC-HOST-SHUTDOWN-001 | 第一轮 task_scope 9 测试通过；续扫 pool client 41、Dynamic close 重试 2 通过，但 MCP-over-ACP F5/F6 有运行时反例 |
| ARC-MCP-ACP-001 | 续扫 service 7、ACP 接线 9 测试通过；F4 撤销隔离与 F5 取消重试有运行时反例，不能宣布完整通过 |
| ARC-WORKFLOW-RPC-001 | 第一轮 rpc 11 通过；续扫 runner 24 通过 / 3 ignored，workflow 生命周期 4 通过；未覆盖共享 JS reader 的全部故障 |
| ARC-KEEPGOING-001 | 本轮未做专项验收 |
| ARC-SERIAL-001 | 工具表稳定排序/BTreeMap 静态核对；未重跑 provider cache boundary 用例 |
| ARC-MIDDLEWARE-001 | assembly 38 测试通过，包含蓝本链序与关闭面 |
| ARC-MIDDLEWARE-CAPABILITY-001 | 能力单测 4、Agent doc tests 10 通过，含 compile-fail 窄能力约束 |
| ARC-SECRET-001 | 未对全仓日志、遥测与错误输出面进行敏感数据专项审计 |
| ARC-MICRO-TOOL-INPUT-001 | 未重跑 Micro input 与 Write/Edit sentinel delta 套件 |
| ARC-PTC-ARTIFACT-001 | env_clear/固定包安装入口静态抽查；未做 artifact/cache/handshake 专项验收 |

## 已执行验证

### 第一轮

| 命令或实验 | 实际结果 |
| --- | --- |
| `cargo metadata --no-deps --format-version 1` | 退出 0；只能证明 manifest 可解析 |
| `bash scripts/check-layer-imports.sh` | 退出 0；20 规则 / 0 违规，覆盖缺口见 F3 |
| `bash scripts/check-file-size.sh` | 退出 1；最后一次为源码 1 / 测试 28 超限 |
| `cargo test -p peri-agent --lib compact_v2::full::tests -- --test-threads=1` | 26 passed |
| `cargo test -p peri-agent --lib middleware::capabilities` | 4 passed |
| `cargo test -p peri-agent --doc` | 10 passed |
| `cargo test -p peri-acp --lib transport::router` | 13 passed |
| `cargo test -p peri-middlewares --lib assembly::tests` | 38 passed |
| `cargo test -p peri-resources --test session_resources_contract` | 5 passed / 1 failed，见 M2 |
| `cargo test -p peri-workflow --lib rpc` | 11 passed |
| `cargo test -p peri-acp-types --lib event_v2` | 56 passed |
| `cargo test -p peri-middlewares --lib mcp::tool_bridge` | 18 passed |
| `cargo test -p peri-acp --lib host::task_scope` | 9 passed |
| 临时 Compact seam 探针 | 2 passed，复现直接本机读取；测试文件已移除 |
| 隔离门禁探针 | 普通导入被拒绝；绝对导入和 extern crate 别名漏报 |

曾执行 `cargo test -p peri-agent --lib re_inject`，实际命中 0 tests；不计为验证证据，已改用命中 26 个测试的完整模块过滤词。

未运行全 workspace build/test/clippy、真实跨机/云恢复、网络 MCP 故障矩阵及全平台进程清理。当前结果只支持上述范围和时点，不支持“全库合规”声明。

### 续扫：MCP 会话隔离、关闭和 Workflow 生命周期

| 命令或实验 | 实际结果 |
| --- | --- |
| `cargo test -p peri-middlewares --lib mcp::acp` | 7 passed，正常 attach/隔离/关闭路径 |
| `cargo test -p peri-acp --lib acp_mcp` | 9 passed，含真实 host setup→工具面→close 接线 |
| `cargo test -p peri-middlewares --lib mcp::client::tests` | 41 passed，含 pool 单一 shutdown transaction 与取消/并发重试 |
| `cargo test -p peri-middlewares --lib test_close_registration_` | 2 passed，Dynamic MCP 取消与 Incomplete 重试 |
| `cargo test -p peri-middlewares --lib workflow::lifecycle_tests` | 4 passed，真实 Node 路径的取消/关闭、resume 参数与管理器不可替换 |
| `cargo test -p peri-workflow --lib runner` | 24 passed / 3 ignored；ignored 用例要求安装 `@peri-code/workflow`，不计为通过 |
| 临时 MCP 生命周期探针 | 3 passed，断言的是 F4/F5/F6 当前故障行为；强化双连接场景后复跑仍 3 passed |

常规用例通过没有推翻探针；两组检查覆盖不同条件。MCP 新探针替换的是协议对端，pool、service、bridge、rmcp 和 task owner 均使用真实实现，无外网请求、无真实模型调用。

本次探针源码已从工作树移除，本机复现留档位于 `/tmp/peri-mcp-boundary-audit.MosgKe/architecture_boundary_probe_20260930.rs`。重新临时放到 `peri-middlewares/tests/` 后运行 `cargo test -p peri-middlewares --test architecture_boundary_probe_20260930 -- --test-threads=1`；这些“故障存在”断言不得作为永久正向契约测试保留，修复时应改为预期隔离/排空断言。

续扫时三个未被本任务修改的事实源 SHA-256：

| 文件 | SHA-256 |
| --- | --- |
| `peri-middlewares/src/mcp/client/lifecycle.rs` | `80839218efcaae642476fafdd1c264f93d31738fbc0465bb0f9802638d45b8ad` |
| `peri-middlewares/src/mcp/acp/session.rs` | `28cf4858a5e60416f55717dd8b38a70a1eb7d59d496ee3655f09dbc627bc48ae` |
| `peri-middlewares/src/mcp/acp/transport.rs` | `c21bbb42f9448e31ad783c9817ab4e8c30840adba7764ce0f79a52ef12abd5c7` |

Workflow 这一轮没有新增已复现违规；注册 run 的取消与 Node/Agent 排空有局部正向证据。但三项 runner E2E 被忽略，持久编排脱离活跃 Harness 的长期目标也不由这四项生命周期测试证明。

## 建议推进顺序

1. 先修 F4：关闭/撤销过程中持续维持会话隔离，不让资源临时变成部署共享。
2. 将 F5/F6 作为同一 MCP-over-ACP 生命周期边界整改：强 owner、单一可重试关闭事务和真实桥接排空证据。
3. 修 F1：确定执行环境读取 seam 和缺席语义，阻断计算宿主磁盘旁路。
4. 修 F2/F3：业务生产点收回 v2，加强依赖门并加入失败样例。
5. 在各自迁移任务中收口 M1/M2；冻结待验收快照，补齐矩阵中的专项测试与 E2E，再给出合规结论。
