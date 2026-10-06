# P0：dev Peri 进程 CPU 与内存占用异常

**状态**：Open（2026-10-06）。根因机制已定位于 `ce4c9b37` 引入的全量 WorkState 读写；两轮窄读取止血已提交，用户现场反馈 CPU 暴涨明显减少（2026-10-06，非受控对比）；账本根治未批准，内存归因未闭环。复核：两个已知回归失败在当前工作树仍复现。
**优先级**：P0（用户指定）。**范围**：本仓库 dev TUI 进程资源异常；不做 schema 迁移，不删历史证据。

## 问题

1. **CPU**：ACP 执行提交路径全量读写内嵌大请求的 WorkState，成本随保留的历史请求规模放大。现场 PID `38338`（`target/debug/peri`，`./dev.sh` 启动，运行约 16 分钟）`ps` CPU 59.8–100.4%；打开的 `~/.peri/threads/threads.db` 3.28 GiB、WAL 47.6 MiB。
2. **机制**（采样 + 源码）：`commit_response → WorkMutationBarrier::commit → apply_work_mutation → write_work → mutation_effects → encode<WorkState>`。完整 provider 请求经 `checkpoint` 存为 `ReasonRequest.serialized_request`，响应提交 / settle 后仍保留；读取侧 `gate.rs::check_owned_work` 与 `continuation.rs::publish_inbox_work` 即使 `limit=1` 也读全量状态，旧 work 进 Settled 后仍作为历史参与全量处理。
3. **引入时间**：`ce4c9b37`（2026-10-06 08:58，persist work checkpoints and unified execution admission）首次引入；放大来自 `9a6526ae` / `67abb92b` / `17f718e1`。父提交 `873ed575` 无该链。引入目的是持久检查点与对账，问题在于把增长型证据纳入高频整体读写。
4. **内存**：全量解码、深拷贝、旧 / 新编码与事务副本解释了峰值放大机制；无事故会话大小、堆归属与释放曲线，**不确认泄漏**。
5. **数据规模**：`session_work_state` 12,197 行，末三条状态 9.28 / 50.09 MiB / 660 B；**未关联事故会话，非事故快照**。

## 证据边界（约束结论用）

- RSS 与 `sample` physical footprint（579.1 MiB，历史峰值 840.8 MiB）口径不同，不直接比较。
- 采样栈计数是包含计数且含线程等待样本，不是互斥百分比或调用次数；TUI 主线程 1492/1535 样本在 park，不支持"绘制是主因"。
- JSON 链符号后缀 `tracing_core` 的祖先是 WorkState 编码，非 Event dispatch；telemetry 用同步 `RollingFileAppender`，不支持"异步日志积压"。
- 原始证据目录 `/tmp/peri-dev-38338-20261006-135110`（含 SHA256SUMS）非仓库 fixture，可能含会话与本机路径，分享前须脱敏。

## 当前状态

- 事故进程 14:03 前自行消失，退出原因未知，**不视为恢复**。
- 用户现场反馈（2026-10-06）：CPU 暴涨现象明显减少；未按验收步骤做同负载对比采样，不作为闭环验收，内存侧无现场结论。
- 源码复核（2026-10-06）：两轮止血已移除 gate 的全量状态读取（`work.rs::HAS_PENDING` 递归 EXISTS）、消息级去重对整份 WorkState 的反序列化（`READ_DELIVERY` 单条 delivery）与提交时的旧状态重编码（`effects.rs` 复用 `current_json`）。
- 两轮止血已提交（见归档）；无受控 CPU / RSS 收益测量、无 Turso 网络验收、未跑完整 workspace 测试。
- 复核失败：`remote_work_concurrent_publish_...` 在 `session_work_test.rs:322` 仍 `PersistenceUncertain/Unknown`；`durable_act_handoff_...` 在 `durable_work_contract.rs:666` 仍 `Rejected { InvalidTransition }`。归属未经隔离基线证明。
- 内存告警埋点已提交 `30a4942f`（`peri-tui/src/app/service_registry.rs` + `service_registry_test.rs`）。

## 未完成

- [ ] 关联事故 session，仅补取状态 / 请求字节大小与 work 数量。
- [ ] 观测空闲、流式、历史加载与切换会话的 CPU 时间差分及 RSS / footprint 走势，确认可复现触发条件。
- [ ] 量化存储中间副本、TUI 缓存与队列内存，区分正常驻留、峰值放大与泄漏。
- [ ] 用户现场验收：同负载重采样，报告改善与局限。
- [ ] 两轮止血未覆盖的根治项（2026-10-06 复核仍成立）：reducer 全量 clone（`work/reducer.rs`）、新状态全量编码（`work/effects.rs::UPDATE_STATE`）、事务 guard 仍以整份旧 JSON 作参数比较（`GUARD_STATE`）、后台 wake 与 2 s 定时全量读取（`continuation.rs` → `read_work` → `READ_STATE`）、`load_session_work(limit=1)` 先读整份状态、每条命令整份编码入库（`work.rs::command_effects`）。
- [ ] 阶段 2–5：载荷与事务切片、增量账本、窄通知与执行隔离闭环、迁移与事故验收。
- [ ] 根因确认后按 `docs/standards/testing.md` 补行为与生命周期回归；修复一类问题而非压低当前指标。
- [ ] 两个回归失败的归属判定或修复。
- [ ] 与《移除默认执行恢复 P0》的共同约束闭环：旧 work 隔离、旧未知副作用迁移，实施前重核共享工作树。
- [ ] 确认内存告警在现场生效（`peri.mem` 可在日志中检出）。

## 待批准

- 记录级结构 / payload 引用 / 接口调整。
- wire 协议版本与两端升级；旧未知 work 与新准入的冲突规则；旧 Required 义务的履行路径。
- 排他停写迁移窗口与回滚边界（迁移未获授权，不得开始）。

## Astra 方案要点（待批准，未实施）

推荐：不可变大载荷独立保存 + 工作账本按记录读写 + 当前执行与历史事实分离。A（窄读取止血，已部分落地）→ C（载荷外置 + 分表增量提交，目标）；B 仅作迁移内部步骤。降低轮询频率、压缩或扩大缓存不代替根治。

- 维持 `SessionResources → SessionDataPort → SQLite / Turso` 依赖方向；`peri-acp-types` 为领域规则单一权威，`peri-resources` 负责有界读取、原子提交与迁移，不在 Peri 增加 lease / owner CAS / fencing。
- 接口草案：`pending_mutation_status(scope)`、`query_work_availability(context)`、`load_work_execution(work_id, admission)`、`load_work_evidence(id)`、`apply_work_mutation(command)`。
- 增量提交：不再 clone 全会话历史，禁止分表后重新全表组装；保留 session 级 revision 串行化、control guard 全部语义、事务内义务 / 回执 / 一致性检查，guard 未命中须整体失败。
- 确定性门槛：gate、通知与去重的历史 payload 读取 0 字节；普通状态提交重编码历史 payload 0 字节。
- 阶段：1 止血（已部分实施）→ 2 载荷与事务切片 → 3 增量账本 → 4 窄通知与执行隔离 → 5 迁移与事故验收。

## 证据入口

- 代码：`peri-resources/src/sessions/{work.rs,work/effects.rs,sqlite_store/session_data/work.rs}`、`peri-acp-types/src/session_resources/work/{reducer.rs,processing.rs}`、`peri-model/src/protocol/prepared.rs`、`peri-agent/src/agent/stages/work_pipeline.rs`、`peri-acp/src/host/continuation.rs`。
- 历史：`git show ce4c9b37:<path>` 与父版本 `873ed575` 对照；提交时间线另存本机 `/tmp/peri-p0-introduction-commit-timeline.txt`。
- 相关 issue：《移除默认执行恢复 P0》、2026-09-27 TUI 流式发布 CPU P0（仅检索入口，根因未证实相同）。

## 已完成归档

- 现场采集 2026-10-06 13:50–13:51：`ps` / `top` / `sample` / `lsof` / 日志尾部 + SHA256SUMS，证据目录见上节。
- 根因调查：两个只读 subagent 分查存储与 TUI / tracing，主 agent 核对调用栈与源码，产出机制链与反证；未改代码与数据库，未运行构建。
- 引入时间追溯：定位 `ce4c9b37` 及三个放大提交，父版本对照确认机制为首次引入。
- 第一轮止血 `e628113f`：mutation gate 改为存在性查询（本地 / 远端共用根及后代 `SELECT EXISTS`），提交复用同快照原始 JSON 作旧状态 guard。定向测试 7 + 5 + 9 passed，契约 18 passed / 1 ignored。
- 第二轮止血 `83ed1e44`：新增 `WorkDeliveryQuery` 与 `load_work_delivery`，消息级 inbox 去重不再反序列化整份 WorkState。定向测试 16 项 passed。
- 内存告警 `30a4942f`：复用 `ProcessResourceMonitor` 的 2 s 采样，RSS > 200 MiB 告警、60 s 节流，target `peri.mem`，仅 TUI 进程生效。
