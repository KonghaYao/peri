# 多 Subagent 并发期间 CPU 偏高：现场调查

**状态**：Open / 阶段 A 已交付，待性能验收（2026-10-07）。未完成受控单 Agent / 多 Agent 对比，不宣布 CPU 已根治。

## 观察

- 用户截图显示多 Subagent 工具执行期间 `CPU 168%`，内存 `168MB`。TUI `ProcessResourceMonitor` 使用 sysinfo 的进程 CPU 指标，允许多核时超过 100%；它不是整机 CPU 百分比。
- 本机发现 PID `98230`（`target/debug/peri`），启动于 18:48:05。18:56:04 和 18:59:25 分别采集一次 5 秒、1 ms 间隔的 macOS `sample`；不能证明该进程就是截图对应进程。
- 18:59:25–18:59:31，进程累计 CPU TIME 从 `7:05.97` 增至 `7:11.34`，增加 5.37 CPU 秒。时间戳仅精确到整秒，采样也有开销；这是该窗口确有 CPU 工作的证据，不作为精确负载或收益测量。
- 两次栈采样均出现 `SqliteSessionData::write_work`、`mutation_effects → encode<WorkState>` 和 `read_snapshot → decode<WorkState>`，也出现模型侧字符串序列化与 TUI 绘制。第一次主线程 3178 个样本中 2986 个在 park；第二次 render loop 包含计数为 237。包含计数不能相加计算互斥占比，不足以宣布唯一主因。
- 对进程打开的 `threads.db` 只读查询最近插入的 10 条 WorkState。六个 18:53:50–18:53:52 创建的子会话共享同一父会话；18:58 左右状态体积约 0.59–1.70 MB，works 为 33–168 条。这些是近期会话，尚未通过进程运行身份把它们与截图关联。
- 随后只读检查这六条状态：每个子会话都有一条 `reasonInFlight`，`reasonRequest.serializedRequest` 为 235734–818879 字符；其余 32–184 条为 `settled`，请求正文字段为空。两个查询在不同时刻执行，不能将记录数当同一快照。

## 机制与边界

`write_work` 在状态变更事务内读取并解码完整状态，再由 reducer 更新，最后 `mutation_effects` 编码完整新状态。终态请求裁剪已生效，但进行中的请求与增长型历史账本仍参与整份状态处理。多个子会话并发执行会增加此路径的工作总量；这是源码与现场栈共同支持的成本机制，尚未量化它相对绘制、模型请求构造和工具执行的占比。

相关问题：`2026-10-06-p0-dev-peri-high-cpu-memory.md`。该问题已批准方案 T（终态裁剪），未批准方案 C（payload 外置 / 记录级存储调整）。本次不扩大授权，不清理或迁移现场数据库，不降低并发或刷新频率掩盖成本。

## 后续验收与待裁决

- [ ] 将执行身份、进程与会话关联，在同模型、上下文和工具工作量下对比单 Agent / 多 Agent。
- [ ] 分别计量请求准备、状态读取/解码、reducer/编码、SQL 提交与 UI 发布/绘制，使用 CPU TIME 增量和时间窗，而不是仅看瞬时 `ps %cpu`。
- [ ] 若状态路径主导，裁决不可变请求 payload 外置及按记录增量账本方案，明确 SQLite / remote 共同契约、恢复和事务语义；不能只优化一个后端。
- [ ] 修复后验证非终态恢复、终态对账、并发子会话身份、取消与迟到结果，再用同负载现场采样验收。

## 证据

原始采样位于本机 `/tmp/peri-subagent-cpu-98230/sample.txt` 和 `sample-2.txt`，不纳入仓库；可能包含本机路径，不应直接公开。源码入口：`peri-tui/src/app/service_registry.rs`、`peri-resources/src/sessions/sqlite_store/session_data/work.rs`、`peri-resources/src/sessions/work/effects.rs`、`peri-acp-types/src/session_resources/work/processing.rs`。

## 后续代码审查与状态回归（2026-10-07）

首次采样仅只读调查。后续检查并实施阶段 A，新增数据库重开后的请求保留回归；不修改生产状态机语义或存储结构、不写现场数据库、不重启用户进程。

### 状态正确性

- 六种 `WorkStage` 仍为 ReasonReady / ReasonInFlight / ActReady / Settled / Blocked / Abandoned；四个终态迁移入口 `commit_reason`、完整 `commit_act`、`abandon`、`settle` 均调用请求裁剪，未发现生产代码绕过这四个入口直接设置终态。
- `ReasonInFlight` 的完整请求检查点保留，恢复会检查身份、batch、生命周期和投影；`ActReady` 恢复使用已提交响应与 invocation 记录，而不是重发旧模型请求。Blocked 不裁剪已有正文；Settled / Abandoned 不恢复成新执行。
- reducer 消费独占 WorkState；拒绝命令时丢弃可能部分更新的状态并清空派生效果。adapter 通过 `accepted_state` 检查 receipt / state 配对，状态、投影、事件与回执仍受事务 guard 约束。Unknown 保留原命令身份供对账，不作为确定成功。
- 新增 `peri-resources/tests/durable_work/request_retention_contract.rs` 三个数据库重开回归：进行中大检查点精确保留；终态裁剪后响应/请求 ID 保留且重放不增添历史；错误请求 ID 的响应被拒绝，旧检查点、状态与历史保持不变。

### 验证结果

全部命令通过仓库补丁脚本并使用 `--locked`：

| 范围 | Cargo 参数 | 结果 |
| --- | --- | --- |
| types 全 lib，含摘要 golden、真实计数、availability 单投影 | `test -p peri-acp-types --lib` | 580 passed |
| SQLite 持久化、崩溃与重开，含新增回归 | `test -p peri-resources --test durable_work_contract` | 29 passed / 1 ignored |
| remote adapter 契约 | `test -p peri-resources --lib session_work` | 20 passed |
| SQL 效果、raw guard 与共享参数 | `test -p peri-resources --lib sessions::work::effects::tests` | 11 passed |
| Agent 全 lib，含 Unknown、取消、冷 journal、大请求与子 Agent | `test -p peri-agent --lib` | 1101 passed |
| ACP 全 lib，含恢复、调度与生命周期 | `test -p peri-acp --lib` | 797 passed |
| resources 全 lib，提交快照隔离验收 | `test -p peri-resources --lib -- --test-threads=1` | 471 passed / 21 ignored |
| middleware 全 lib，基线失败集合对比 | `test -p peri-middlewares --lib` | 1527 passed / 77 failed / 2 ignored；与基线同名 77 项失败 |
| workspace 全目标，原锁文件的隔离提交快照 | `check --workspace --all-targets` | passed |
| 五个相关 crate 的 doc tests | `test -p peri-acp-types -p peri-resources -p peri-agent -p peri-middlewares -p peri-acp --doc` | 11 passed / 5 ignored |

durable 的 ignored 为既有崩溃测试子进程入口，由父测试显式启动。resources 早期全量运行曾遇到 Git discovery 超时，隔离提交快照串行复跑已全部通过。middleware 在隔离基线 `8c8c346a` 上完整复跑，结果同为 1527 passed / 77 failed / 2 ignored，排序后的 77 项失败名称集合相同；没有修改这些既有身份/资源夹具问题来掩盖结果。fmt、typos、22 条依赖边检查、修改 Rust 文件 ≤1000 行、diff 空白与五份修改文档本地链接检查通过。真实 Turso、Emscripten 与 CPU/RSS 尚未验收；mock transport 不冒充线上网络。这是覆盖场景的状态正确性证据，不是无条件保证。

### 性能状态仍为未根治

- SQLite 与 remote adapter 仍读取整份 WorkState，并在接受变更后编码完整新状态；窄读取只限制返回载荷，数据库端仍需解析原 JSON。
- WorkState 仍保留历史 delivery / batch / work / response / invocation 等账本记录，没有本次授权下的历史回收或记录级增量存储。
- 终态裁剪仅移除 WorkRecord 的请求正文；持久原命令 journal 仍保留 BeginReason 原命令用于对账，不应把裁剪当作数据库整体清理。
- 本轮不宣称 CPU 已修复；payload 外置及记录级存储仍待方案裁决与实施。原现场身份关联、性能占比和同负载验收复选项继续保持未完成。

## 阶段 A 实施任务（2026-10-07）

依据 `2026-10-07-workstate-no-schema-optimization-proposal.md`，用户要求更新 issue、派出子 Agent 并由主 Agent 主持修复。本轮范围为 A0 / A1a / A1b / A1c；不改数据库表/列/索引/schema17、不改变持久 JSON 形状、不清历史、不迁移现场库、不降低并发。provider A3、执行窄视图 B、选择性物化 C、SQL patch 与缓存不混入本轮。

- [x] A0：补命令准备与状态阶段诊断/确定性验证，明确 wall time 与 CPU 口径。
- [x] A1a：WorkRecord 元数据借用；借用式 checkpoint 包装；缩短无关快照驻留，SDK 所需快照保留到最后观察。
- [x] A1b：不可变 PreparedWorkCommand 从 barrier/gate 到两个 adapter/reducer 共享一次 canonical 编码与摘要；Unknown 仍持原身份、原完整命令。
- [x] A1c：effects 的 INSERT/GUARD 参数共享 Arc；SQLite 借用绑定，remote 保留驱动边界复制；避免 journal/effects 重复编码；保留 remote guard 语句布局及重试分类；快照只构造一份 availability 投影。
- [x] 验证 schema/wire/摘要字节不变、原始 JSON guard、拒绝回滚、Unknown/崩溃恢复、生命周期、幂等与两个 adapter 契约。
- [x] 隔离合成大载荷与固定动作序列验证：prepare 的 encode/hash 各一次，共享 clone 不增计数；四个 SQL 参数位共享一份命令 buffer；checkpoint 字节/摘要与原实现一致。没有宣称测出端到端 CPU/RSS 降幅。
- [x] 完成跨 crate / 全目标与相关回归，明确 middleware 基线失败和真实网络/跨平台未覆盖边界。
- [ ] 现场 CPU/RSS 同负载验收仍单独保留。

分工：子 Agent 按 types / resources / agent / ACP 测试分成互不重叠的写入范围；主 Agent 负责跨层契约、其他调用方迁移、集成、测试与文档，不覆盖已有共享树修改。

DOC-UPDATE-001：稳定接口与入口已同步 types/resources/agent code-index；本 issue 仅记录证据与未完成验收，不替代领域规则。性能验收未完成，保持 active，不另建完成过程归档。
