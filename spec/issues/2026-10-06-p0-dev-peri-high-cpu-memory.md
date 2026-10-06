# P0：dev Peri 进程 CPU 与内存占用异常

**状态**：Open；第一轮小范围止血已实施并通过定向回归，待用户现场验收。全量账本根治、事故会话数据规模及内存归因仍未闭环。
**优先级**：P0（用户指定）。
**创建 / 现场采集**：2026-10-06（Asia/Shanghai），约 13:50:59–13:51:33。
**范围**：本仓库 dev TUI 进程资源异常；用户已授权指挥 subagent 进行少量修复后交由用户验收。本轮仅做窄门禁检查与旧状态编码复用，不迁移 schema，不删除历史证据；完整账本重构仍待确认。
**来源**：用户报告一个 dev Peri 进程内存与 CPU 都非常高，要求快速提取情况，随后要求记录为 P0 事故。

## 现场事实

| 项目 | 观测 |
| --- | --- |
| 目标进程 | PID `38338`，命令 `target/debug/peri` |
| 工作目录 | `/Users/konghayao/code/ai/peri-v4p3` |
| 启动链 | PID `38332` 的 `/bin/bash ./dev.sh` → PID `38338` |
| 启动时间 / 运行时长 | 2026-10-06 13:34:56 +0800；采集时约 16 分钟 |
| `ps` CPU | 多次观测为 59.8%、100.4%、100.3% |
| `top` CPU | 首帧 0.0% 不作为窗口 CPU 结论；后续两帧 64.3%、88.4% |
| `ps` RSS | 1,188,160 / 1,000,976 / 1,070,384 KiB，约 0.95–1.13 GiB |
| `sample` physical footprint | 579.1 MiB；进程历史峰值 840.8 MiB |
| 线程 | `top` 显示 15 个线程，后续采样帧显示 1 个 running |
| 子进程 | PID `38547`，Bun execution sidecar；CPU 0.0%，RSS 38,768 KiB |
| 打开的存储 | `~/.peri/threads/threads.db` 为 3,525,701,632 字节（约 3.28 GiB）；WAL 为 49,934,432 字节（约 47.6 MiB） |
| 打开的日志 | `.tmp/agent-tui.2026-10-06`，`lsof` 观测大小 23,005,907 字节 |

RSS 与 macOS physical footprint 是不同统计口径，不直接比较，也不据此宣称内存泄漏。数据库、WAL 和日志大小是文件大小，不代表进程把它们全部加载进内存。

## 栈与日志证据边界

- 对 PID `38338` 执行 `sample 38338 3 1`，采集约 3 秒调用栈。
- 非等待的栈顶热点包括 `serde_json::read::SliceRead::skip_to_escape`、JSON 字符串转义序列化、`memmove`，以及 tracing / memchr 相关操作；采样也出现 ratatui 文本、段落和字素处理路径。
- 栈顶计数包含多线程等待样本，不能把计数直接当作整进程 CPU 百分比，也不能仅凭符号认定某一业务调用链是根因。
- 日志尾部可见 `perf.render`：`items=208`、`rebuilds=0`、`slots=208`、`logic_lines=230`、`vp_lines=41`；示例 `frame-total` 为 244–320 μs、`gen=3600`。这是日志中该埋点的观测，不代表完整终端帧开销，也尚未独立验证每条日志的 PID 归属。
- 初次提取时仅确认资源异常现场；后续调查结论见下节，不能把初始热点符号直接当作业务归因。

## Subagent 根因调查（2026-10-06）

两个 subagent 分别只读调查存储 / 后台读取和 TUI / tracing，主 agent 核对关键调用栈与源码。未改业务代码、数据库，未运行构建或测试。14:03:20–14:03:26 再次查询时 PID `38338` 已不存在；本次调查未终止它，退出时间与原因未知，不能据此认定恢复。

### 主要 CPU 热点与放大机制

初始 `sample.txt` 的实际祖先链：

```text
ACP execution → Agent run_reason → commit_response
→ WorkMutationBarrier::commit_execution_transition / commit
→ SessionResources::apply_work_mutation → SqliteSessionData::write_work
→ mutation_effects → encode<WorkState>
→ works → WorkRecord.reason_request → ReasonRequest 字符串序列化
```

同一 worker 分支的包含计数为 `commit_response=880`、`write_work=690`、`mutation_effects=623`、请求字符串序列化分支约 `600`，该线程总样本为 `1535`。这是祖先 / 子分支的包含计数，不是互斥百分比，也不是调用次数。

采样与当前源码共同支持以下机制：

- `peri-resources/src/sessions/sqlite_store/session_data/work.rs` 的 `write_work` 读取并解码整份状态；`peri-acp-types/src/session_resources/work/reducer.rs` 的 reducer 克隆状态；`peri-resources/src/sessions/work/effects.rs` 的 `mutation_effects` 序列化旧状态与接受后的新状态，并为事务 effect 保留字符串副本。
- `peri-model/src/protocol/prepared.rs` 的 `checkpoint` 包含 provider 请求 body；`peri-agent/src/agent/stages/work_pipeline.rs` 的 `request_checkpoint` 将其序列化为字符串，存入 `ReasonRequest.serialized_request`。外层 WorkState 编码会再次扫描并转义这些字符串。
- `peri-acp-types/src/session_resources/work/processing.rs` 中旧 work 进入 Settled 时仍保留请求，并创建 successor。历史请求会继续参与后续全量状态处理，不需要同一事件重复执行即可放大成本。
- `peri-resources/src/sessions/resources/gate.rs` 的 `check_owned_work` 和 `peri-acp/src/host/continuation.rs` 的 `publish_inbox_work` 即使 `WorkQuery.limit=1` 仍读取全量状态；后台通知在 wake 或两秒定时器后再次检查。这两类读取路径也出现在初始采样中。

**调查结论**：初始窗口的主要 CPU 热点位于内嵌大请求的全量 WorkState 提交及重复读取，成本随保留的请求 / 状态规模放大。事务 guard、恢复数据与业务幂等语义仍需保留，不能用直接删除 checkpoint 或跳过一致性检查作为修复。

### TUI / tracing 反证

- 主线程 `1535` 个样本中，`1492` 个处于 Tokio park / 条件变量等待（约 97.2%）；仅 `43` 个进入 TUI future。该窗口不支持 TUI 绘制是主要 CPU 来源，不排除其他时段的短时重建。
- JSON 链中部分底层符号末尾带 `tracing_core`，但祖先是 WorkState 编码，不是 tracing Event dispatch。真正日志事件分支只见少量样本，不能按符号后缀误归因。
- 当前 telemetry 使用直接 `RollingFileAppender`；此处不支持“异步日志队列积压”解释。日志 metadata 也只说明共享日志输出，不证明目标 PID 的 CPU 原因。

### 补查的数据规模与内存限制

存储 subagent 于 14:08:54 后使用 SQLite `mode=ro` / `query_only` 查询纯大小元数据，未输出正文或原始会话 ID：`session_work_state` 当时有 `12,197` 行，按 rowid 倒序取最后三条插入记录的状态长度为 `9,731,051 / 52,527,586 / 660` 字节，约 `9.28 MiB / 50.09 MiB / 660 B`。

这些结果证明当前库存在数十 MiB 的单行 WorkState，但**尚未关联到 PID 38338 的事故会话，也不是 13:51 的现场状态快照**。不能把约 50 MiB 状态直接宣称为本次事故数据规模。

全量解码、深拷贝、旧 / 新编码与事务参数副本提供了内存峰值放大的机制；TUI 正文、渲染行和 Markdown 缓存也可能同时驻留。但无事故会话大小、堆归属或释放曲线，尚不能解释全部 RSS，也不能确认内存泄漏。通知队列积压、revision 重试等仅是待验证候选，未确认发生；后台检查有等待与有限退避，未证明忙循环。

当前工作树有未提交改动，二进制 mtime 为 13:32:58 +0800。存储关键源码 mtime 早于该时间、符号链相符，但 mtime 不能证明完整构建来源；TUI 当前源码尤其不能直接等同于事故二进制。

## 原始证据与处置

本机证据目录：`/tmp/peri-dev-38338-20261006-135110`。

- `process.txt`：目标、父进程与 sidecar 的启动信息及资源快照。
- `process-final.txt`：末次目标进程资源快照。
- `top.txt`：连续三帧资源采样。
- `sample.txt`、`sample-command.txt`：完整调用栈及采样命令输出。
- `lsof.txt`：工作目录、二进制、打开文件及连接。
- `log-tail.txt`：当时日志最后 150 行。
- `resource-followup.txt`、`sample-followup-command.txt`：后续复采失败及进程已不存在的证据。
- `SHA256SUMS.txt`：初始 sample / top / lsof / log-tail 文件的 SHA-256。

这些文件位于本机临时目录，非仓库 fixture；清理后不能依赖其仍可读取，需要重新采样。文件可能包含会话或本机路径信息，对外分享前须检查并脱敏。本次未停止、重启或修改目标进程，未修改业务代码。

## 后续调查与验收

- [x] 从完整采样定位 JSON 热点的上层业务调用链，区分 TUI 主线程与后台 worker 成本，并纠正 tracing 符号误归因风险。
- [ ] 关联事故 session，仅补取状态 / 请求字节大小与 work 数量；不得把后续其他记录当作事故快照。
- [ ] 分别观测空闲、流式输出、历史加载与切换会话时的 CPU 时间差分、RSS / footprint 走势，确认可复现触发条件。
- [ ] 评估大请求 / 结果与小型可变状态分离、未决检查与通知的窄读取；保留恢复、幂等、事务 guard 与可靠投递契约，方案需单独确认。
- [ ] 量化同负载下存储中间副本、TUI 缓存与队列内存，区分正常驻留、峰值放大和泄漏。
- [ ] 根因确认后按 `docs/standards/testing.md` 补充用户可观察行为及生命周期回归，修复一类问题而非仅压低当前进程指标。
- [ ] 在可比工作负载下重新采样，报告 CPU 和内存改善及局限；不能以测试通过或命令启动代替现场验收。

相关历史线索：[2026-09-27 TUI 流式发布 CPU P0](2026-09-27-p0-tui-streaming-view-rebuild-cpu.md)、[2026-08 历史月志](../history/2026-08.md)中的 SQLite 后台轮询资源风暴。仅作为检索入口，不代表本次事故与其根因相同。

## Astra 修复方案（待批准，2026-10-06）

来源：用户指定 `gpt-6-astra` subagent，重新核对当时源码与[移除默认执行恢复 P0](2026-10-06-p0-remove-session-runtime-state-restoration.md)。本节是完整待批准方案，不是已实现行为；方案产出时未改业务代码、未迁移数据库、未运行构建或测试，后续小范围修复见末节。另一 P0 的快速修复已实施，但旧 work 隔离与未知副作用迁移仍待闭环，实施前须重核共享工作树。

### 推荐与取舍

**推荐：不可变大载荷独立保存 + 工作账本按记录读写 + 当前执行与历史事实分离。先独立交付窄读取止血，再完成根治。**

| 方案 | 收益 | 局限 / 成本 |
| --- | --- | --- |
| A：窄读取，减少临时副本 | gate、投递去重与部分后台检查立即减负，契约变化较小 | 全量提交仍随历史规模增长，只能止血 |
| B：载荷外置，保留整体 metadata JSON | 消除历史大字符串的重复转义 | metadata、投递、invocation 等仍增长，整体 clone / 重写仍存在 |
| C：载荷外置，领域记录分表，事务增量提交 | 热路径成本围绕本次相关记录与新载荷，修复同类增长问题 | 需要 schema 迁移、reducer / 查询接口调整，以及本地 / 远端一致性验证 |

A 是第一阶段；C 是目标。B 可作为迁移内部步骤，不作为长期终点。降低轮询频率、压缩或扩大缓存不能代替根治。`peri-resources/src/sessions/work.rs` 的 `command_effects` 也编码完整原命令，不能只移走 `WorkRecord.reason_request` 而留下 journal 正文热点。

### 职责、接口与存储

维持 `SessionResources → SessionDataPort → SQLite / Turso` 依赖方向，不新增文件缓存或通用对象存储框架。

- `peri-acp-types` 维护身份、状态转换、资格、冲突与回执规则的单一权威；存储负责查询筛选，不复制领域规则。
- `peri-resources` 封装有界读取、payload 保存、原子提交、迁移与持久化不确定性；Agent 只获取获准 work 所需正文。
- SDK 负责当前执行身份与合法准入，ACP 做查询 / 通知适配；不借此在 Peri 增加执行 lease、owner CAS 或 fencing。

将通用全量读取的热路径调用改为目的明确的接口；以下名称为草案：

| 接口 | 数据范围 |
| --- | --- |
| `pending_mutation_status(scope)` | 根及后代范围的未决命令存在性和有限身份信息，不返回正文 |
| `query_work_availability(context)` | revision、合法候选摘要、阻塞原因；携带准入来源 / observer 分界 |
| `load_work_execution(work_id, admission)` | 指定执行涉及的 batch、预算、invocation 与 payload 引用 |
| `load_work_evidence(id)` | 显式诊断 / 对账所需的指定历史证据 |
| `apply_work_mutation(command)` | 稳定原命令身份、类型化回执，保留确定 / 不确定语义 |

存储结构建议：小型 session head 保存 revision、接纳序号和 limits，继续关联现有 control；work、batch、budget、invocation、delivery、obligation、admission、binding 按稳定身份独立保存。请求、响应、工具输入 / 结果和大投递正文进入同一 Store 的不可变 payload，记录类型、编码版本、长度与摘要，业务记录和新 command journal 保存引用。首版不做跨会话全局去重，不自动 GC。

payload 引用必须稳定；重试不能生成不同引用而改变原命令身份。摘要用于完整性和命令内容绑定，不替代访问授权。canonical transcript 保持消息投影职责，与交付义务的原子关联不能丢失。

### 增量提交与一致性

建议数据流：

```text
准备本次载荷 → 登记原命令及载荷 → 读取相关事实
→ 共用 reducer 生成类型化变更集
→ 原子提交变更 / 投影 / 义务 / 回执 → 确认命令 → 发布提示
```

- 不再 clone 全会话历史，禁止分表后重新全表组装 WorkState。命令登记、领域提交、确认的阶段语义不能被随意合并。
- 首版保留 session 级 revision 串行化，以小型 revision guard 代替整份 work JSON 比较；所有相关写入口必须推进对应版本。
- control guard 必须覆盖现有 guard 的全部语义；generation 若不覆盖所有变化，保留小型 control 全值比较或完整存储版本，不能只比较 lifecycle。
- 父会话绑定、终态义务回执、事件身份、canonical message 一致性检查仍在事务内完成。guard 未命中必须使整个提交失败，不能将零行更新认作成功。
- 本地和远端复用领域变更规则；远端继续使用 `QualifiedMutation` 和原 operation identity。现有按 `rejected_statement: Some(4)` 分类竞争的耦合改为具名 effect 分类。
- Applied 丢 ACK 按原命令读取回执；Unknown 保留原命令并对账；NotApplied 必须有封闭迟到提交的证据。暂时查不到回执不等于未执行。

### 与默认执行恢复 P0 的共同约束

下列约束贯穿每个实施阶段，不能等到最终阶段才恢复正确语义：

- `load/replay` 只恢复历史消息与 frozen，不默认发模型请求、调用工具或准入旧 execution。未完成状态、同 lifecycle、旧 admission 均不能独立证明当前 live。
- 新输入或合法新投递仅能选择与当前精确准入来源对应的 work，不能因旧 work 排在前面而恢复它；不伪造 Settled / Abandoned 解卡。
- owner 查询、终态接纳、去重、可靠投递对账可独立推进，但不授权重跑旧模型 / 工具。observer floor 只控制提示，不意味着此前义务已履行。
- 保留旧 Required 义务；如何通过显式续聊或独立对账履行它们需批准并测试，新投递丢 wake 后仍需可发现。
- 旧未知业务副作用按 invocation、owner、scope 与资源冲突隔离；真实冲突须明确拒绝，证据不足须报告无法确定，不泛化为永久会话封锁。
- 持久化提交 Unknown 与业务执行 Unknown 分开；不能为解除旧任务阻塞而跳过持久化屏障。窄查询必须保留 `READ_PENDING` 现有根及后代范围。

### 迁移与实施顺序

推荐有排他保证的停写迁移，避免在线双写；不执行任何迁移直到得到授权：

1. 协调所有写入者退出并可靠备份，不能以没看到 PID 证明排他。
2. 按会话分批抽取旧状态、载荷与记录，控制内存预算；超大单行采用受限 / 流式解析或明确中止，不整库加载。
3. 保留身份、revision、接纳顺序、原回执、未决义务和未知状态。旧命令原字节与 digest 不重算，作为版本化不可变证据仅供原命令必要对账。
4. 未决提交无法安全解释或结果不确定时，不标为 reconciled，不丢弃；阻止不安全切换。校验引用、摘要及回执 / 义务关联后原子发布新 schema。
5. 新业务路径只读写新结构，旧表退出业务路径；中断可续迁或回滚未发布结构。旧客户端拒绝新 schema，新版开始写入后不能直接切回旧程序。

迁移工具是有限生命周期工具，不保留长期兼容读写层；尚未结清原命令的历史证据解码须有明确退场条件。历史证据删除 / GC 另行批准。

| 阶段 | 垂直交付 | 关键验收 |
| --- | --- | --- |
| 1：止血 | gate 未决命令存在性查询、按 ID 投递去重 | 根 / 后代范围、内容冲突、Unknown 屏障保留，不读历史正文 |
| 2：载荷与事务切片 | 请求准备 → 响应提交 → 原回执对账贯通本地 / 远端 | 丢 ACK、取消、竞争、迟到提交不重复生效 |
| 3：增量账本 | delivery、invocation、义务、绑定等记录级读写 | 不组装全历史，事件 / 投影 / 义务 / 回执原子性保持 |
| 4：窄通知与执行隔离闭环 | 历史不激活，新输入正常准入，通知不选旧 work | 未知副作用、真实 live 对照、丢 wake、重复 load、迟到结果、关闭 / 取消 |
| 5：迁移与事故验收 | 旧库迁移保真，可比负载资源采样 | 崩溃续迁、版本拒绝、数据完整性，CPU / 内存分别报告 |

### 测试、性能验收与待决项

沿用本地 / 远端既有 work 契约测试，补模型 / 工具实际调用计数，不能只检查 spinner。覆盖丢 ACK、并发发布、mutation ID 内容冲突、Unknown 阻塞、新 facade 对账、事务 guard 失败、迁移中断及可靠投递。

拟议的确定性门槛：gate、通知及去重读取历史 payload 为 **0 字节**；普通状态提交重编码历史 payload 为 **0 字节**；执行只读取明确依赖。分别扩大历史 payload、历史记录、待办规模，记录 CPU 时间、峰值分配、读写字节、SQL 行数、WAL 增量、远端传输量和延迟分布；百分比性能预算须先确定稳定基线，不将拟议收益当实测。

在合成 / 脱敏副本上用 dev profile 复核事故形态、release profile 验证扩展性，不操作用户当前库。空闲、流式、历史加载、切换和清空后的 RSS / footprint 单独验收；存储热点消失不自动证明内存事故闭环。

待批准：记录级结构 / payload 引用 / 接口调整；涉及 wire 时的协议版本与两端升级；旧未知 work 与新准入的冲突规则；旧 Required 义务履行路径；排他迁移窗口及回滚边界。主要风险是遗漏全局不变量、改变旧命令 digest、缩小根屏障、破坏远端 guard 或通知仍选中旧 work。

## 引入时间追溯（2026-10-06）

用户要求快速核查旧架构与引入时间；两个 subagent 分查存储 / 查询和请求 checkpoint / 留存，使用 Git 历史对象交叉确认，未修改业务代码或数据库。以下时间均为 Asia/Shanghai（+0800）。

**首次引入核心机制的提交：`ce4c9b370eee2a1f527e4323e125be313dc1b9ee`，2026-10-06 08:58:20，`refactor(rcra): persist work checkpoints and unified execution admission`。**

该提交同时引入且接入生产 `run_reason`：

- 完整 provider 请求 checkpoint → `ReasonRequest.serialized_request` → `BeginReason` 原命令 journal → `WorkRecord.reason_request`；响应提交 / settle 后仍保留请求。
- 单会话单行 `session_work_state.state_json`；全量读取 / 解码、reducer 状态克隆、旧 / 新状态编码、整份旧 JSON 事务 guard。
- `load_session_work(limit=1)` 先读取完整状态，再截候选；limit 并不约束状态字节读取。

引入目的为持久执行检查点、完整原命令对账、冻结实际请求和保留未知结果证据，不是单纯日志或 TUI 改动。问题在于把这些增长型历史证据纳入高频整体读写的状态，而非保留证据本身。

| 后续提交 | 时间 | 对已有机制的影响 |
| --- | --- | --- |
| `9a6526aea1e7d0b8617b5caade63bc644eac7d81` | 13:06:28 | 草稿 input_json 暂存 / 发布和 workflow 子执行准入扩大状态及变更面；不是核心 checkpoint / 全量状态的首次引入 |
| `67abb92b5f84aa1ba239768348cd57c1238212ce` | 13:09:39 | 新增后台 inbox 通知任务，启动和 wake 后调用原有全量查询；有等待与有限退避 |
| `17f718e135edb05d191e977d24c58bb5be65d8a5` | 13:16:59 | 增加两秒定时分支，无 mailbox wake 时也可重新读取检查 |

### 旧架构对照与边界

ce4 的父提交为 `873ed575a0031a9ff56d0f0cda60dacb68dd53d9`（2026-10-06 07:03:58）。父版本没有该 WorkState / session_work_state 请求持久化链：`run_receive` 从 queue 消费，历史按 `messages` 行追加；模型路径使用 `generate_reasoning_with_observed_body`，未将完整请求累积到 durable work 状态。

旧版本已有小型 `ControlState` JSON guard，不能概括为“以前没有 JSON guard”；可以确认旧路径没有本次“累积完整请求 → 全量 WorkState clone / 重编码 / guard”的具体机制，不能宣称旧架构不存在其他资源问题。

证据入口：ce4 及父版本的 `peri-model/src/protocol/prepared.rs`、`peri-agent/src/agent/stages/{reason,work_pipeline,receive}.rs`、`peri-acp-types/src/session_resources/work/processing.rs`、`peri-resources/src/sessions/work{.rs,/effects.rs}`、`peri-resources/src/sessions/sqlite_store/session_data/{work,history}.rs`；后续后台放大见各提交的 `peri-acp/src/host/continuation.rs` diff。可用 `git show <commit>:<path>` 复核，不按当前 WIP 归因。

历史证据确认了机制引入时间，未量化各提交对事故 CPU / RSS 的贡献，也未证明事故二进制精确对应哪一提交。调查时工作树继续变化，当前 binary 已重建，不能用其 mtime 或 HEAD 代替原事故构建来源。提交时间线另保存在本机 `/tmp/peri-p0-introduction-commit-timeline.txt`。

## 第一轮小范围止血（已实施，待用户验收）

用户授权：指挥修复，可派 subagent，先少量修复后由用户验证。两个代码 worker 分别负责以下不重叠写集，主 agent 负责整合、定向验证与文档：

- 将内部 mutation gate 的未决命令检查替换为存在性查询；本地 / 远端保持同一根及后代范围，不读取全量 WorkState，未知提交仍阻断。
- 已有状态的提交复用同一快照读取的原始 JSON 作为旧状态 guard 输入，避免再次编码历史请求；缺失状态仍构造并编码原有初始状态。保留新状态编码、全部事务 guard、原命令 journal 与回执协议。

本轮不承诺资源事故闭环：reducer 全量 clone、新状态编码、后台通知的全量读取仍未根治，完整 schema / 载荷分离迁移不在本轮范围。不得用单测通过宣称 CPU / RSS 改善，真实效果由用户在同工作负载下验证。

### 实际交付与验证

- 两个 adapter 的 `SessionDataPort::has_pending_work_mutations` 使用共用根 / 后代 `SELECT EXISTS`；本地通过门面验证正常写入、Unknown 阻断、reconciled 解锁与 DB 错误，远端以真实 SQLite transport 验证相同存在性查询及不读取大状态。
- `mutation_effects` 接收同快照的旧 JSON 所有权，避免旧状态重编码以及额外持有旧 JSON 副本；远端从 `read_batch` 结果移出原 String，不再 `to_owned`。仅写路径保留 raw，缺失状态初始语义与全部 guard / 回执协议保持。
- 定向审查未发现 P0 / P1 正确性问题；审查指出的远端大字符串复制已移除。未进行真实 Turso 网络部署验收、完整工作区测试或资源收益实验。

2026-10-06 执行结果：

| 命令 | 结果 |
| --- | --- |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- pending_work` | 7 passed |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- sessions::work::effects::tests` | 5 passed |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- sessions::remote::session_work::tests` | 9 passed，含 3 个上述 pending 测试 |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --test durable_work_contract` | 18 passed / 1 ignored；ignored 为由父测试驱动的进程崩溃 fixture |

验证过程中修正了新测试的动态 SQL（SQLx 0.9 要求字面量）以及远端 fixture 初始化假设；最终远端窄查询测试仅依赖既有 work fixture，不扩张到无关会话创建契约。无业务断言放宽、无 schema 变更、未操作用户数据库、未提交 Git。

用户现场步骤：先妥善结束或取消当前任务，再用 `./dev.sh` 重新编译并启动，在相同长会话下显式发起任务；对比空闲、流式与工具结束提交时的 CPU / RSS，验证新消息与工具结果仍完整。历史加载本身不应恢复旧执行。若资源仍高，保留新 PID 与调用栈再推进下一轮，不据此跳过 Unknown 安全屏障或清理历史证据。
