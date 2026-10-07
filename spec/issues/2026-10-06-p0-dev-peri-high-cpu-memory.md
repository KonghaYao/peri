# P0：dev Peri 进程 CPU 与内存占用异常

**状态**：Open（2026-10-06）。根因机制已定位于 `ce4c9b37` 引入的全量 WorkState 读写；三轮窄读取止血已提交（含 `b7c6770d`），用户现场反馈 CPU 暴涨明显减少（非受控对比）；实测锁定主成本为 encode/decode（≈2.2 s/mutation，由 `state_json` 体积驱动），方案 T 已批准并在当前工作树实施，方案 C 未获批准；内存归因与现场验收未闭环。两个已知回归失败已判定为测试侧问题并按新语义修正（`5e3d7866`、`02d71bf4`）。
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

- 安全收口（2026-10-06）：撤回工作树中未闭环的 TUI `interrupted` / `Unconfirmed` 状态机及 epoch 回执过滤，恢复原有队列语义；补充换实例后的迟到取回回执恢复测试。保留已批准的终态裁剪与 owned reducer，补齐 `commit_act` / `settle` 裁剪、非终态正文保留和历史记录不变测试。定向验证：TUI steer 70、Work reducer 38、持久化契约 26 passed / 1 ignored，TUI 编译检查通过。未迁移或清理现场数据库；不据此宣称 CPU / 内存事故已闭环。上一轮 Subagent tool 套件 72 项失败仍需独立归因与修复，本轮不扩展该范围。
- 事故进程 14:03 前自行消失，退出原因未知，**不视为恢复**。
- 用户现场反馈（2026-10-06）：CPU 暴涨现象明显减少；未按验收步骤做同负载对比采样，不作为闭环验收，内存侧无现场结论。
- 源码复核（2026-10-06）：两轮止血已移除 gate 的全量状态读取（`work.rs::HAS_PENDING` 递归 EXISTS）、消息级去重对整份 WorkState 的反序列化（`READ_DELIVERY` 单条 delivery）与提交时的旧状态重编码（`effects.rs` 复用 `current_json`）。
- 两轮止血已提交（见归档）；无受控 CPU / RSS 收益测量、无 Turso 网络验收、未跑完整 workspace 测试。
- 失败归属已判定（只读排查 + lldb 运行时证据，均指向测试侧）：`remote_work_concurrent_publish_...`（`session_work_test.rs:322`）为 fixture 问题——同 session 并发未对账命令与 `work.rs::GUARD_COMMAND` 不变量冲突，`session_work_journal.rs::begin_owned_work` 把确定性 `NotApplied` 折叠为 `PersistenceUncertain` 且不重试，测试自 `ce4c9b37` 起 flaky（8 跑 7 败，无争抢时 8/8 通过）；`durable_act_handoff_...`（`durable_work_contract.rs:666`）为过期断言——`44309b13` 有意收紧停止态交棒（`processing.rs:262`）并同步了单测与 `work_dispatch.rs` 调用方，漏改该集成测试。两者修测试，不改生产语义。
- 本轮免迁移窄化（用户已定范围：不做 schema 迁移）：Wave 1 后台 inbox 通知路径改窄查询已提交 `b7c6770d`（`continuation.rs` 的 `load_session_work(limit=1)` 不再反序列化整份状态；本地 / 远端共用 `WorkAvailability` 投影，`peri-resources` 定向 9/9 通过）。Wave 2（提交路径去掉 `reducer.rs` 对 `WorkState` 的整份深拷贝）**经实测否决**：真实样本 clone 仅 8 ms，占单次 mutation 成本 <0.5%，见下节。
- 基线已绿（`peri-resources` 侧四类失败全部判定为测试侧问题并修正，见归档）。
- 内存告警埋点已提交 `30a4942f`（`peri-tui/src/app/service_registry.rs` + `service_registry_test.rs`）。

## 实测成本画像（2026-10-06 20:31–20:41）

样本：会话 `01a110cb-…` 的真实 `state_json`，从 `~/.peri/threads/threads.db` 只读导出，debug profile（与 `./dev.sh` 的 `target/debug/peri` 一致）。样本含本机会话内容，不随仓库保存。

| 环节 | 原始 82.2 MB | 清空 settled 请求正文后 2.85 MB |
| --- | --- | --- |
| decode（快照反序列化） | 493 ms | 31 ms |
| clone（reducer 旧状态深拷贝） | 8 ms | 0.5 ms |
| encode（新状态序列化） | 1699 ms | 53 ms |
| SQLite 写同尺寸（/tmp WAL 基准） | 44–268 ms | 未测（按比例） |

体积构成（同一快照）：`works` 334 条，settled 的 `reasonRequest.serializedRequest` 合计 70.0 MB、`response.serialized` 1.2 MB；请求正文占整份状态 96.5%（清空后实测 82.2 MB → 2.85 MB）。该正文为完整 provider 请求（含 system/tools 渲染），`messages` 表无等价副本（该会话仅 0.98 MB），不可由历史重建。

结论：

- 单次 mutation 主成本是 **encode + decode（合计 ≈2.2 s）**，由 `state_json` 体积驱动；SQLite I/O 与 clone 均非瓶颈，降低轮询频率或扩大缓存只省常数因子。
- 状态随每轮推理线性增长（观测：约 7 分钟内 +7.6 MB），账号越用越慢；一次用户提交 ≥2 个 mutation，单次提交成本约 5 s。
- 根治必须让终态 work 的请求正文离开高频整体读写路径：或裁剪（有损）、或外置（无损，需迁移）。两者均待批准，见下。

## 未完成

- [ ] 关联事故 session，仅补取状态 / 请求字节大小与 work 数量。
- [ ] 观测空闲、流式、历史加载与切换会话的 CPU 时间差分及 RSS / footprint 走势，确认可复现触发条件。
- [ ] 量化存储中间副本、TUI 缓存与队列内存，区分正常驻留、峰值放大与泄漏。
- [ ] 用户现场验收：同负载重采样，报告改善与局限。
- [ ] 两轮止血未覆盖的根治项（2026-10-06 复核仍成立）：新状态全量编码（`work/effects.rs::UPDATE_STATE`）、事务 guard 仍以整份旧 JSON 作参数比较（`GUARD_STATE`）、每条命令整份编码入库（`work.rs::command_effects`）。reducer 全量 clone 已在实测中降级（8 ms，不做专项改造）；后台 wake 与 2 s 定时全量读取、`load_session_work(limit=1)` 先读整份状态已在 Wave 1（`b7c6770d`）消除。
- [ ] 阶段 2–5：载荷与事务切片、增量账本、窄通知与执行隔离闭环、迁移与事故验收。
- [ ] 根因确认后按 `docs/standards/testing.md` 补行为与生命周期回归；修复一类问题而非压低当前指标。
- [ ] 评审本轮排查发现的邻近问题：`session_work_journal.rs:84-91` 把确定性 `NotApplied` 折叠为 `PersistenceUncertain`，而该错误会冻结会话热态并阻塞续写，与 `mutation.rs` 自述的「未决才是不确定」矛盾。属本 issue 范围外，需单独决策。
- [ ] 与《移除默认执行恢复 P0》的共同约束闭环：旧 work 隔离、旧未知副作用迁移，实施前重核共享工作树。
- [ ] 确认内存告警在现场生效（`peri.mem` 可在日志中检出）。

## 待批准

- 根治方案已裁决（2026-10-06，AskUserQuestion）：采用**方案 T 终态裁剪**；**存量记录保留现场、只清新数据**；不做清理前备份。实施落在"进入终态的动作内清除当前 work"（`processing.rs` 的 `commit_reason` / `commit_act` / `abandon` / `settle` 四处），不做全量扫描，因此既有存量字节不变、后续新增不再累积。终态语义：`reason_request` 置 `None`，`request_id` 与 `response` 保留；`ReasonInFlight` / `Blocked` / `ActReady` 不裁剪（对账与恢复依赖正文）。
- 方案 C（payload 外置，无损）保留为日后需要复盘终态请求正文时的升级路径（需新增表与结构字段，属"记录级结构 / payload 引用 / 接口调整"；`deny_unknown_fields` 下旧版本读新数据会拒绝）。
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
- 基线归因与修正（全部判定为测试侧，不改生产语义）：远端并发发布用例改为顺序提交（同 session 不允许两条未对账命令）`5e3d7866`；停止态交棒与 admission finish 两处过期断言按 `44309b13` 新语义重排 `5e3d7866`、`02d71bf4`；远端 schema 形状守卫由「只比 canonical 计数」改为逐条比对完整 DDL 序列 `71e39356`。`durable_work_contract` 现为 26 passed / 0 failed / 1 ignored（ignored 为既有）。
- 上述 schema 判定的依据：`session_schema::initialization_plan()` 的 20 条 = 14 条 canonical v2 DDL（9 表 + 5 索引）+ 6 条 control/work 机制表 DDL；新建路径（`remote/session_schema.rs:32-39`）与迁移路径（`remote/schema_v14_upgrade.rs:31-47`、`sqlite_store/schema.rs:202-242`）都会创建这 6 张表，未发现新建库与迁移库的结构或索引分叉。
- 内存告警 `30a4942f`：复用 `ProcessResourceMonitor` 的 2 s 采样，RSS > 200 MiB 告警、60 s 节流，target `peri.mem`，仅 TUI 进程生效。
