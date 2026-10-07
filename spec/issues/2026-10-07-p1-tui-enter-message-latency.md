# TUI Enter 后消息显示延迟：跨层性能排查（P1）

- **状态**：五个修复 subagent 与主 agent 已完成补丁与隔离回归，等待现场验收；尚未宣称用户那次约 1 秒等待已完成归因。
- **日期**：2026-10-07。
- **优先级**：P1（交互核心路径存在用户可感知停顿；尚无本问题导致数据丢失的证据）。
- **调查基线**：`ce7f3c44`，调查段落行号按该版本；修复验证快照基于 `1570464b` 加本轮工作区补丁。现场日志、数据库持续变化，调查数字均为采样结果。
- **来源**：用户报告本地 Rust TUI 输入后按 Enter，消息约 1 秒才进入消息流；要求 subagent 从不同方向排查，所有发现归入本 issue。

## 范围与结论边界

- 主场景：已有会话空闲、没有既有待发项，普通输入按 Enter 后的可见反馈与正式消息延迟。
- 分开验证：首次创建会话、Agent 忙时正常排队、Stop 后恢复、长会话、附件输入。
- 已知显示机制、观察到的性能异常与本次输入的实际根因必须分别表述。没有同一输入的关联计时，不能拼接不同日志推断完整耗时。
- 不以乐观气泡掩盖执行延迟，不把入队确认伪称为 Delivered，不绕过持久化、SDK 准入或取消/恢复契约。
- 初轮仅调查；用户随后授权 subagent 快速修复，明确 **数据库 schema 不动，SQL 可以改**，并追加完成后单独提交。本轮修改生产代码与回归，但不添加表、列、索引或迁移，不升级 schema，不改用户数据库/配置，不提交或覆盖其他任务的改动。

## 并行调查分工

| 方向 | 调查范围 | 产出 |
| --- | --- | --- |
| 渲染与交互线程 | Transcript、缓存、锁、事件处理及消息区计时范围 | 已完成日志与源码核查，见 LAT-06～10 |
| 持久化 | WorkStore SQL、索引、状态体积、事务与编码成本 | 已完成只读复测及合成索引 A/B，见 LAT-03～05 |
| 客户端与执行准入 | 串行 steer consumer、operation gate、ACP、Bun/TS SDK | 已复现 SDK lost-wake，局部测试 14 pass，见 LAT-11～15 |
| Agent 执行准备 | run_prompt、Mailbox 发布、Receive 与 Delivered 时机 | 已完成源码计数及交付竞态分析，见 LAT-04、16～20 |

## 结论与建议顺序

本轮确认系统不是只有“展示慢”：SDK 新唤醒丢失已用生产 coordinator 隔离复现，待处理命令全局扫描由现场查询计划与合成 A/B 支持；此外有多次全量快照、同步渲染及跨 IO 锁等长尾放大路径。**仍未取得用户这一次输入的关联时间线，不能宣称约 1 秒的唯一根因已定位。**

| 建议顺序 | 发现 | 证据级别 / 后续动作 |
| --- | --- | --- |
| 1 | LAT-11 SDK lost-wake | 已确定性复现；补正式回归并保留合并唤醒 |
| 1 | LAT-20 Delivered 事件过滤竞态 | 高风险静态候选；先用屏障复现，若成立修复事件去重所有权 |
| 2 | LAT-02、06 分段诊断 | 可观测性缺口确认；为每项修复提供同一输入证据 |
| 2 | LAT-03 pending 查询 | 索引建议已被本轮 schema 限制排除；执行无迁移 SQL 优化，保留恢复语义 |
| 3 | LAT-04、16～19 重复读/准备/业务锁 | 调用与锁范围确认；测成本，优先去重，再评估结构性优化 |
| 3 | LAT-07～10 渲染长尾 | 慢间隔与同步路径确认；来源未定，分解锁/解析/调度/flush |
| 3 | LAT-12～15 准入队头阻塞 | 结构或配置确认；隔离 fixture 验证实际等待，不贸然移除生命周期锁 |
| 契约评估 | LAT-01 反馈空窗 | 展示行为确认；与真实性能分别验收，不伪造 Delivered |

以下 20 项包含已证缺陷、静态放大机制与待验证候选，不等于 20 个已复现 bug；P1/P2 为本 issue 内的处理建议，不自动批准跨层设计变更。

## 当前数据流与显示语义

```text
Enter → 清空编辑器 → 本地 Enqueue 命令 → ACP enqueue
      → 持久草稿/发布 → SDK 执行准入 → run_prompt → Agent Receive
      → UserInputDelivered → TUI 消息投影 → 消息区准备 → 终端实际显示
```

实现入口：`peri-tui/src/kit/input_area/submit.rs::commit_input/submit_text`、
`peri-tui/src/kit/steer_consumer.rs::execute/admit`、
`peri-acp/src/host/requests/user_input.rs`、
`peri-agent/src/agent/stages/work_receive.rs`、
`peri-tui/src/kit/acp_events/turn.rs::handle_user_input_delivered`。

[现行队列设计](../../docs/design/user-input-queue.md)明确：写入 canonical transcript 才算 Delivered；入队或交接不能直接作为正式聊天消息。空闲直接提交隐藏待发项，Delivered 后才显示聊天气泡。这是当前契约，不是已经证明的性能解释；若调整反馈形式，必须保留投递状态区分。

此前 `7d475be7` 仅消除空闲输入的短暂待发区闪现，没有证明降低 Enter → Delivered 耗时；`39091849` 修复子进程抢占控制终端，不能据此宣称输入延迟也已修复。Stop 恢复变更已进入调查基线，不把旧实现问题重复认定为现存缺陷。

## 现场基线证据

### B1：消息区存在秒级长尾

- 日志：`.tmp/agent-tui.2026-10-07`；`2026-10-07T09:04:23.487410Z`（北京时间 17:04:23）记录 `transcript=1020538μs`，随后 `frame-total=1020769μs`，`gen=5960`。
- 同一日志最近 10,000 次采样：消息区准备计时中位数 `286μs`，P95 `1006μs`，最大 `1020769μs`。
- 限制：计时不是 Enter → 可见、不是完整终端绘制耗时，也不能单独证明是 CPU 计算而非锁等待/调度暂停；日志有多实例交错，尚未与用户的具体 input ID 关联。

### B2：空结果查询仍扫描全局命令记录

- 只读数据库：`~/.peri/threads/threads.db`，连接使用 `mode=ro` 与 `PRAGMA query_only=ON`。
- 采样时 `session_work_commands` 有 22,222 条记录，只有 mutation 主键自动索引；`READ_PENDING` 查询计划为 `SCAN session_work_commands USING INDEX sqlite_autoindex_session_work_commands_1`。
- 对采样时最近根会话执行相同 SQL 五次，均返回 0 行，耗时依次为 `247.815 / 30.465 / 27.472 / 25.223 / 16.508ms`。这是 Python SQLite 只读复测，不等于 Rust/sqlx 请求的完整耗时，不把第一次结果未经验证称为冷缓存。
- 当日日志有同类查询超过 1 秒的告警，观察到最大 `3.043617834s`；其时间不同于 B1，不能相加。
- `peri-resources/src/sessions/sqlite_store/session_data/work.rs::read_work` 调用 `sessions::work::READ_PENDING`，因此是输入与执行查询链路上的实际工作，不只是无关后台 SQL。

### B3：全量工作状态的体积需按会话区分

- `length(CAST(state_json AS BLOB))` 采样：最近四个根会话约 `1.86–3.58MB`，全库最大单会话 `127,280,468 bytes`。
- 最大状态只读读取五次耗时 `147.134 / 142.849 / 109.553 / 117.187 / 88.074ms`；该会话未证明就是用户当前会话，不代表所有输入都承担这个成本。
- 读取只覆盖 SQLite 查询、数据传递和 Python 字符串构造，不含 Rust 类型反序列化、reducer 与序列化写回。状态增长与重复读写的实际贡献待独立验证。

## 汇总发现

### LAT-01：空闲直接提交有可见反馈空窗（确认的显示行为，不是独立耗时测量）

- 证据：`peri-tui/src/kit/input_area/submit.rs::commit_input` 清空编辑器后走 `steer_state::enqueue`；`steer_state.rs::begin/rows` 抑制直接提交的待发项；`acp_events/turn.rs::handle_user_input_delivered` 才追加正式气泡。
- 现有回归 `steer_state_test.rs::test_steer_idle_submission_skips_queue_until_delivery` 明确断言本地提交与 Dispatching 回执时都没有队列行，首次 Delivered 才允许生成正式消息。它验证去闪现/去重，不验证交互反馈时延。
- 影响：只要执行链路或界面阻塞，已清空的输入既不在编辑器也不在消息区；展示行为会放大实际等待，让用户难以判断已提交还是丢失。
- 处理方向：先消除真实阻塞并测量；若调整交互，明确区分提交中/已受理/已消费，不修改 canonical Delivered 定义。当前设计明确要求隐藏直接待发项，改变展示需同步该契约，不能偷偷把 Queued 当正式气泡。
- 验收：以首次可见反馈与 canonical 消息分别计时；覆盖成功、未知回执、拒绝、generation 切换、重复 Delivered，不靠重复提交实现反馈。

### LAT-02：缺少输入关联的全链路延迟证据（确认的可观测性缺口）

- 现有消息区阶段计时与 SQL slow statement 无法串起单条输入的 Enter → 首次绘制路径；日志多实例交错，当前不能裁决 SQL、准入、Receive 准备与渲染各自贡献。
- 风险：根据单条慢日志或正常帧的中位数归因，容易误修、漏掉锁/调度长尾；优化显示后还可能把执行慢问题隐藏。
- 初轮没有添加生产埋点；本轮已补命令排队、客户端请求、ACP 响应、Store 与渲染阶段计时，仍没有覆盖同一输入的终端 draw/flush 完整闭环。

### LAT-03 / P1：未决命令查询扫描无关历史（查询计划与微基准已验证）

- 证据：`peri-resources/src/sessions/work.rs:108` 仅定义 mutation 主键；`:114` 的 `READ_PENDING`、`:115` 的 `HAS_PENDING` 与 `:110` 的 `GUARD_COMMAND` 同会话未决检查缺少 pending/session 索引。现场计划前者扫描全局主键索引，后二者扫描表。
- 输入路径关联：`sqlite_store/session_data/work.rs:121` 每次 `read_work` 查询 pending；`sessions/resources/gate.rs:177` 的写准入也检查 `HAS_PENDING`。零 pending 仍承担无关会话的历史扫描，不只是当前消息的必要工作。
- 独立复测：Python SQLite `3.51.0`；schema 17，22,863 条命令，全库 3 条 pending。最近更新根会话 5 次空结果耗时 `14.863 / 14.193 / 15.626 / 15.474 / 15.368ms`。与 B2 数字不同是不同时间快照；均使用只读连接及查询截止，不包含 Rust 请求整段时间。
- 合成 A/B：`/tmp` 中 22,222 条合成命令，约 13.49MB，payload 约 500 字符，3 条 pending 在无关会话；每组 7 次，原 SQL 不变。增加以下索引后，`READ_PENDING` 从 `2.759–3.049ms` 到 `0.018–0.061ms`；`HAS_PENDING` 从 `2.335–2.455ms` 到 `0.017–0.028ms`。三处查询转为按 session 搜索部分索引；根/子会话 pending 的 mutation 顺序保持一致，`READ_PENDING` 使用临时排序树。

```sql
CREATE INDEX idx_work_pending_session_mutation
ON session_work_commands(session_id, mutation_id)
WHERE reconciled=0;
```

- 初轮索引建议仅保留为研究证据；**本轮用户禁止 schema 变更，不执行上述 CREATE INDEX 或任何迁移**。实际 SQL-only 实施见后述记录；不能删除 journal、绕过 Unknown 屏障或改变恢复排序。
- 局限：合成数据不包含真实 WorkCommand 结构，不经过 Rust/sqlx/Tokio，不模拟现场 WAL/并发；不能将微基准倍数换算为 TUI 加速比例，也不能认定它单独解释本次 1 秒。

### LAT-04 / P1：全量工作状态多次读取、克隆和重写（机制确认，耗时贡献未测）

- 证据：`peri-agent/src/session/user_input_mailbox/staging.rs:155`、`:232`、`:265` 的首次 enqueue 正常分支至少三次顺序 `durable.load`；内部操作还可能读取，不把“三次”当作端到端全部读取次数。
- Agent 方向展开内部调用：全新 command/input、允许立即发布、无重试/不确定结果的分支有 6 次逻辑完整 work snapshot 加载：初始授权 → StageUserInput 完成投影（`durable.rs:317`）→ 再读 staged → `publish_selection` 准备（`staging.rs:586`）→ Publish 完成投影（`durable.rs:317`）→ 收尾 publication 索引。另有 Stage 与 Publish 两个逻辑写入及 mutation resolve；这是代码计数，不是 SQL 次数、网络次数或时间测量。
- `user_input_mailbox/durable.rs::load` 的 `WorkQuery { limit: 1 }` 只限制候选，不限制完整 state 与 pending 查询；`peri-resources/src/sessions/sqlite_store/session_data/work.rs::read_snapshot` 读取完整 JSON。
- `peri-resources/src/sessions/work.rs:209` 同步 serde 编码，`:212` 解码；`sessions/work/effects.rs:33` 克隆旧 JSON，`work.rs:191` 以整段旧 JSON 作 guard，`effects.rs:104` 全量编码并更新新状态。
- 状态增长来源：`peri-acp-types/src/session_resources/work/user_input.rs` 保留输入 JSON/状态；`work/delivery.rs` 另保存 publication/projection。不能仅因完成就删除，它们有恢复与去重义务。
- 修复建议：先测编解码与读取次数，再评估同一操作内快照复用、针对输入身份的窄读、活跃状态与历史 payload 分离、增量写入。不能重用过期 revision 或新增另一套 reducer；长期结构调整需维护原命令、digest、receipt、input identity、lifecycle 及终态义务。
- 验收：分别增加无关全库命令数与当前会话 state 大小，验证成本来源；同时验证 Unknown、原 ID 重放、跨 lifecycle 恢复与已有库迁移。B3 大状态不是当前输入负载的替代样本，尚无当前 Rust serde/reducer 耗时测量。

### LAT-05 / P2：写事务中的全量处理可能放大竞争（锁范围确认，现场等待未归因）

- 证据：`peri-resources/src/sessions/sqlite_store/session_data/work.rs::write_work` 的非重放成功路径包含 journal、业务效果、ACK 三个 `BEGIN IMMEDIATE` 事务；第二个事务内读取/解码状态、reduce、构造全量效果并提交。
- `sqlite_store/connection.rs` 可写连接池最多 5 连接，不等于 5 个并发 writer；`sessions/resources/gate_work.rs` 另有根级 mutation barrier。具体是 writer、pool、barrier 还是线程调度造成现场延迟，尚无分段结果。
- 修复建议：分别测 barrier/pool/BEGIN 等待、读取、serde/reducer、effects、commit 与 ACK；不要未经测量先扩池或合并三阶段事务，避免破坏 journal/Unknown/恢复语义。
- 解释限制：sqlx 慢语句是墙钟诊断，不等于纯 SQL CPU，也不能未经验证全部算作连接池等待。

### LAT-06 / P1：渲染阶段计时边界不精确（异常确认，计算归属未定）

- 证据：`peri-tui/src/kit/message_area/mod.rs:108` 在计时前获取 VM 读锁；`:135` 开始计时后还做 footer、hooks、atom/cache 锁获取、prepare 和 guard 释放，`:257` 才记 `transcript`。B1 的 1.020 秒不是 `Transcript::prepare` 专属 CPU 时间，初始 VM 锁等待甚至没有包含在该数字中。
- `mod.rs:776` 的 `frame-total` 早于 element 构建、框架 draw 与终端 flush；`vm_cache.rs::trace_phase` 使用单调经过时间，包含线程失去调度的间隔。
- 只读完整日志统计快照：162,009 条 `transcript` 中 153,259 条小于 1ms，160,413 条 `frame-total` 中 139,220 条小于 1ms；二者各 1 条达到 1 秒。多实例交错，generation 不等于实例身份，不能形成单实例分布或与某次 Enter 关联。
- 修复建议：增加 pre-prepare、锁等待、prepare、draw/flush 分段，关联 process/run/thread/frame 身份；注入计算、锁与调度暂停分别验证埋点归属。已观察慢间隔，未独立复现它的来源。

### LAT-07 / P1 候选：VM 锁跨同步派生持有（阻塞语义确认，现场竞争未复现）

- 证据：`message_area/mod.rs:108` 至 `:255` 持 VM 读 guard 执行派生；`kit/acp_events/render.rs:151` 获取同一 VM 写锁，并在持锁期间更新 publication（`:173`）。慢派生可以阻碍消息发布，反向初始读锁等待又不在现有计时内。
- 当前 `Cargo.lock` 锁定 `ratatui-kit 0.10.3`、`generational-box 0.7.10`；本地依赖源码 `ratatui-kit/src/reactive_handle.rs` 的 try borrow 最终进入 `generational-box/src/sync.rs::get_split_ref/get_split_mut`，使用阻塞式 `parking_lot::RwLock::read/write`，不能用 try 命名排除等待。尚未确认慢帧二进制就是这组依赖版本。
- 修复建议：测读/写等待及持锁时间，评估短锁克隆不可变快照后锁外派生；验证 publication/generation 一致性及并发发布。不据此断言组件局部 State 有跨线程秒级竞争，更不能未经证明称为死锁。

### LAT-08 / P1 候选：跳代或布局失效可放大冷历史重建（路径确认，耗时未复现）

- 证据：`message_area/transcript.rs:51` 的 reset/grid/width/theme/language 全局失效；`:59` 只有 publication 相邻 generation 衔接时用 `changed_from`，否则从零回扫；`:92` 对整个变化后缀调用 `ensure_slot`，并非仅处理可见项。
- 暖缓存会命中，不是必然全量解析；`vm_cache.rs::drop_heavy_cache` 淘汰重置 entry 后，冷历史可能被重建、换行并再次淘汰。索引采用持久树，不能把 `Arc::make_mut` 当成全历史深拷贝的证据。
- B1 附近只有 225 slots / 434 行，没有 scanned/updated/evicted、解析字节数或失效原因；不能认定现场确实走了此路径。
- 修复建议：诊断失效原因及冷命中工作量，评估合并跨代 dirty 范围、不可见冷项延迟派生；验收覆盖跳代、主题/宽度变化、超预算历史，保持高度、选择与复制正确。

### LAT-09 / P1 候选：同步 Markdown 与高亮无时间预算（路径确认，秒级成本未测）

- 证据：`message_area/render.rs:204` 完成态正文调用全解析；`markdown/mod.rs` 流式路径重解析 mutable tail，`markdown/boundary.rs` 对复杂结构保守停止冻结；`markdown/code_block.rs:129` 同步 syntect，语法/主题集合还存在惰性首次初始化。
- 长 mutable tail、未闭合 fence、长代码行、冷历史、完成态全解析可能拖长组件同步执行。本次没有相应 payload 大小、miss 或初始化证据，不能认定它们已经造成 1 秒。
- 高亮本身在缓存写锁外执行；查询/插入才取写锁，不能误称“持高亮缓存锁执行 syntect”。
- 修复建议：按预处理/解析/高亮/wrap/内存核算测量，再评估预算化或后台派生及 generation 校验；验收合成长列表/表格、未闭合 fence、长代码行、冷暖缓存与流式完成态，不牺牲终态解析正确性。

### LAT-10 / P1 放大机制：输入分发与同步渲染共用循环

- 证据：本地 `ratatui-kit 0.10.3/src/render/tree.rs::render_loop` 先同步 render，再 await raw event/state change，取得事件后同步 dispatch。慢组件既可能推迟 Enter 接收，也可能推迟已发布消息的真实绘制；机制确认，现场输入关联未验证。
- 未发现普通 Enter 固定等待 1 秒；`peri-tui/src/kit/entry.rs` 的展示心跳不能代替阻塞修复。`input_history.rs::save_history` 已在线程内落盘，不将磁盘写入认定为 Enter 同步阻塞来源；历史复制成本尚未测量。
- 修复建议：避免事件循环上的无界派生，测 raw event/handler/flush 三个边界；隔离 fixture 注入慢派生，分别验证“按键迟收”和“消息迟显”。当前未排除其他组件、调度暂停或终端写阻塞。

### LAT-11 / P1：SDK 合并 activation 吞掉收尾中的新唤醒（生产实现隔离复现）

- 证据：`npm-packages/@peri-sdk/src/execution/coordinator.ts:95` 同 session 已有 active Promise 时直接返回，不记 pending/dirty wake；`:146` query 后跨 await 持久化 observeControl，`:160` 仍基于之前的空快照返回 idle。
- 复现：`bun run -` 直接导入生产 `ExecutionCoordinator` 与 `MemoryExecutionRegistry`，允许 bestEffort；第一次 query 返回空，在 observeControl mutation 上用屏障暂停。发布新 work、调用第二次 notification activation，再释放屏障。
- 结果：两次 activation 得到同一个 Promise，均 idle，`queries=1/executions=0`，新 work 仍存在；主动第三次 activation 后 `queries=3/executions=1`。execute 刻意抛错的 mock 仅计启动，不执行真实 Agent。探针 exit 0；复现脚本见后续验证记录。
- 影响：证明“旧查询后到达的新通知”可以失去即时重新检查，不证明每次 Enter 都命中，也不是固定 1 秒等待。`peri-acp/src/host/continuation.rs:203` 的 2 秒周期发现可在满足 observer floor/可用性条件时重新通知 SDK，可能掩盖缺陷；不是所有 work 都保证靠它恢复。
- 修复建议：为合并 activation 保存 wake generation/dirty，在返回 idle/释放 active slot 前重验并重新查询；保留 ticket/预算/Unknown/单执行约束，不能用无条件并发执行替代。
- 验收：精确控制 query 空结果 → observeControl 暂停 → 新 work notification 交错，无周期扫描或第三次 activation 也执行且只执行一次；覆盖 Stop/Resume 代际与旧 ticket 不复活。

### LAT-12 / P2：后台 Refresh 与用户输入串行，期限还混合不同阶段（结构确认）

- 证据：`peri-tui/src/kit/steer_consumer.rs::start` 每次选中一条命令后等整条 execute 结束再收下一条；Refresh 强制请求 snapshot，因此慢刷新可以把后来的 Enqueue 留在 channel。
- `execute` 的 10 秒 timeout 包住整个 admit，包含 gate/snapshot 等待，不只是已发送 enqueue 的回执；与“准备后等待受理回执”的期限描述边界不完全一致，排查时必须分段。
- 5 秒 reconciliation 是刷新频率，不是输入固定等待；它只查输入 snapshot，不执行 SDK activation，也不能作为 LAT-11 的执行唤醒兜底解释。
- 修复建议：合并/降优先级后台 Refresh，并使已开始的慢刷新不占满输入消费者；仅调整 select 优先级不能解决正在执行的刷新。验收暂停刷新回包再提交输入，检查推进及 generation/幂等边界；真实等待未测。

### LAT-13 / P2：operation gate 跨 RPC 持有并阻塞整条通知 pump（结构确认，未证死锁）

- 证据：`peri-tui/src/acp_client/client/steer.rs::input_request/user_input_snapshot` 跨远程请求持有 gate；`client/pump.rs:216`、`:237` 在 Delivered/RunStarted 通知也等待同一 gate。单 pump 卡在前置通知后无法读取后续 work/available，即使该通知本来使用后台 spawn。
- `client/requests.rs::cancel` 也争用此 gate，跨控制 RPC 等待；Stop 既可能被前序请求阻挡，也可能延迟通知投影。
- 修复建议：保留生命周期线性化，用 reservation/identity token 与回包重验缩短持锁范围；不可直接删除 gate。验收暂停某个 RPC，顺序发送 gated 通知、work/available 与其他会话通知，验证无关工作推进和旧代际隔离。
- 限制：这是队头阻塞路径，不等于 transport response 路由死锁，更不能声称 enqueue 必然等自己的 RunStarted ACK。

### LAT-14 / P2：ACP 普通请求串行且跨 handler 持全局 sessions 锁（结构确认）

- 证据：`peri-acp/src/host/server_loop.rs:38` 普通请求直接 await dispatch；`:442` 获取共享 sessions 锁并跨 `handle_request(...).await` 持有，input 成功后才 schedule mailbox（`:446`）。已启动 execution 访问 sessions 也可能等待（`host/execution.rs:259`）。
- 慢 snapshot/其他生命周期 handler 可挡住后续 input/work query；close/delete 等 prompt lock 有最多 5 秒等待，但这是关闭路径，不是既有 idle 输入固定开销。
- 修复建议：缩小 sessions 临界区，按职责把长等待放入受管理任务，保留每会话生命周期顺序与关闭屏障。验收阻塞无关会话 handler 时另一个会话仍能 input/query；现场阻塞贡献未测。

### LAT-15 / P2 候选：SDK 同步 SQLite busy 等待可能阻塞 Bun（未复现竞争）

- 证据：`npm-packages/@peri-sdk/src/execution/sqlite-registry.ts:16`、`admission-service.ts:44` 配置 `busy_timeout=5000`；同步数据库操作发生竞争时可能挡住事件循环。
- 修复方向：记录每次 registry/ledger 操作和 busy 竞争，用临时库及可控独立 writer 验证；再裁决隔离执行/缩短事务/超时策略。不将 5 秒上限解释成固定耗时，不能未经测量降低超时破坏 Unknown 处理。

### LAT-16 / P2：工作初始化重复读取与观察 admission（调用结构确认）

- 证据：`peri-agent/src/agent/stages/work_boundary.rs` 初始化读 snapshot 后调用 `observe_sdk_run`；`peri-acp/src/host/user_input.rs:158` 的 RunStarted publisher 再观察同一 admission；`user_input_mailbox/sdk_run.rs:38` 在内存“已观察”检查前先加载完整 snapshot。
- `peri-acp/src/host/scheduled_admission.rs::approve_work` 对普通输入也读取 scheduled trigger snapshot，最后可能没有 trigger。已有 admission、无 scheduled trigger 的初始化分支可识别 4 次完整加载；未直接测耗时，不与 LAT-04 相加成固定端到端往返数。
- 修复建议：复用一次已验证 observation/快照供 attach、RunStarted、scheduled 判定；不能凭请求字段跳过真实混合 batch 审批。调用计数回归需覆盖带/不带 cron、旧 generation、ACK/RunStarted 失败与恢复。

### LAT-17 / P2：发布后调度与 inbox hint 有额外检查（读取确认，关键路径占比未定）

- `peri-acp/src/host/user_input.rs::schedule_mailbox` 调用 `publish_next_durable`，先完整读取才判断 Dispatching/active 等阻塞，再读 availability；部分在后台，不必都处于 RPC 关键路径，但可能竞争同一 Store。
- `user_input_mailbox/durable.rs:458` 为已确认 publication 放入 inbox hint；`agent/stages/work_receive.rs:66` 对每条 hint 再 `load_work_delivery` 验证内容/身份/policy。不是重复 publication 写入。
- 修复建议：复用已确认状态，区分可信已发布唤醒与待持久消息；保留冲突校验。用调用计数及可控并发验证额外读取、不重复发布、撤回与 ClaimBatch 竞争。

### LAT-18 / P2：Delivered 前历史复制、完整加载与执行装配（路径确认）

- `peri-acp/src/host/prompt.rs:260`、`:279` 分次获取 sessions 锁并克隆消息/完整 payload；随后构造 workflow executor、模型工厂及端口，再从 Store 读 lifecycle。
- `peri-agent/src/session/exec/executor_helpers/v2_execute.rs:142` 每轮 `load_session_snapshot`，此处消费 inherited/flags；`:400` 恢复历史 transcript 后才进 loop。历史越大，锁内复制、宽读及重复恢复越值得测量，尚无秒级耗时证据。
- 修复建议：测历史读取/复制/装配阶段，评估窄读、锁外不可变快照、常驻复用/延后构造，必须保留 frozen、历史一致性和生命周期边界。
- 排除错误归因：`SessionConfig::new` 是内存构造，不是同步读配置文件；模型对象构造不等于已进行推理。`before_agent/before_input/before_react_start`、Compact/Reason 在 Receive 返回后，不能解释本层尚未 emit Delivered；首轮 reminders 是历史为空场景的例外。Delivered 表示已 claim/投影并镜像到 transcript，不承诺模型已调用或 startup gate 已成功。

### LAT-19 / P2：Agent 业务锁跨持久化与反向 ACK（锁范围确认，实际竞争未测）

- 证据：Mailbox operations mutex 覆盖初始 load/submit/投影；`work_boundary.rs::initialize` 的 state mutex 跨工作快照、entered-ACK、observation、RunStarted；`work_receive.rs` 的 state mutex 跨快照、ClaimBatch 和再加载。ACP provider/config、AgentPool 还有同步锁。
- 修复建议：分别测锁等待/持锁、IO 与反向 ACK，先避免重复工作再裁决临界区缩短；不得取消 operation 串行化或 exact batch/control 冲突判断。未证明死锁或约 1 秒实际竞争。

### LAT-20 / P1 候选：刷新抢先置 Delivered 可导致事件不发（静态交错，未确定性复现）

1. `peri-acp-types/src/session_resources/work/delivery.rs` 的 ClaimBatch 将 projected 置 true。
2. 并发 `refresh_durable` → `project_publications` 在 `peri-agent/src/session/user_input_mailbox/durable.rs:420` 把 Mailbox record 直接投影成 Delivered。
3. `user_input_mailbox.rs::mark_claimed` 仅处理 Dispatching，`mark_delivered` 仅处理 Claimed；Receive 再转换时可能返回空 delivered 集合。
4. `agent/stages/work_receive.rs:233` 只对集合中的 input emit 带内容的 `UserInputDelivered`，可能被过滤；此时 queue snapshot 说 Delivered，但聊天事件未发。

- 风险：不仅慢，还可能缺失实时消息投影；目前只有静态可达路径，未运行交错测试，不认定它解释稳定约 1 秒等待。
- 修复建议：区分“持久投影已交付”与“事件已发射”的身份/去重；不得简单删除过滤导致恢复重放重复发事件。用屏障控制 ClaimBatch 后、mark 前的刷新，验证正常/刷新抢先/重复 Receive/恢复重放均恰好一次；同时覆盖 takeback、取消与生命周期切换。

所有确认项均只证明自身机制或复现，不代表用户这一次 1 秒等待已经完成归因。

## 后续验证与关闭条件

1. 补充同一输入的链路计时：Enter、本地命令排队、会话准备、operation gate 等待、RPC 发出/回执、Store 查询/写入、发布、SDK 准入、Receive、Delivered 转发、TUI 投影、首次真实绘制。
2. 使用 `input_id/command_id/session_id` 与进程/运行身份关联，不记录输入正文、附件内容或推理正文；各进程以自己的单调时钟计算阶段耗时，不能跨进程直接相减单调时钟。
3. 分开统计排队、锁/池等待、SQL、编解码、计算与终端写入；给出样本量、环境、会话规模、P50/P95/P99/最大值及失败次数。
4. 既有空闲会话、首次会话、忙时排队、停止恢复、长会话、附件、并发会话均验证；忙时排队是业务状态，不计为即时消费应承诺的延迟。
5. 覆盖失败/未知回执、重试、generation 切换与重复 Delivered，证明不丢消息、不重复气泡、不把 Queued/Dispatching 伪装为 Delivered。
6. 本轮只允许 SQL 与计算路径优化，验证 `sqlite_master`、schema 版本及全部 DDL 常量不变，并验证并发写入和 SQLite/远端等价行为；减少快照必须保留原子裁决、CAS 与 Unknown 恢复语义。
7. 关闭前必须取得至少一次同一输入的现场或隔离全链路证据，解释观察到的秒级停顿；性能阈值需确认，不能以微基准或“气泡提前出现”替代实际路径验收。

## 初轮调查与验证记录

- 主 agent：只读日志统计、数据库采样与 `EXPLAIN QUERY PLAN`；没有改动现场数据库。
- 存储 subagent：只读现场复测与两段 Python 验证 exit 0；合成库已清理，未编译/修改仓库代码或现场数据库。
- 渲染 subagent：只读日志统计、项目及本地锁定依赖源码核查；未编译/运行渲染测试、未访问用户数据库。
- 客户端/SDK subagent：生产 coordinator 内存屏障探针 exit 0，确定性复现 LAT-11；局部 Bun 测试 14 pass / 0 fail / 104 assertions，371ms。fixture 测试时间不是 Enter 端到端延迟。
- Agent subagent：只读源码计数、锁范围和事件语义分析；未运行 Rust 测试/模型调用或 LAT-20 竞态复现。
- 主 agent 独立复跑本 issue 的 LAT-11 探针：exit 0，结果与 subagent 一致；局部 SDK 测试再次 14 pass / 0 fail / 104 assertions，324ms。
- 初轮文档验证：本 issue 本地链接、20 个唯一发现编号、代码围栏配对与空白检查通过；当时仅新增这个 issue。以下调查记录不是本轮修复的最终验证结果。

局部 SDK 回归命令（在 `npm-packages/@peri-sdk` 下）：

```bash
bun test test/execution-jsonl-duplex.test.ts test/execution-session.test.ts \
  test/execution-registry.test.ts test/execution-sidecar.test.ts
```

初轮没有 Rust 代码变更，因此没有运行 Rust 测试；本轮生产修复与验证记录如下。

## 本轮修复记录（2026-10-07）

五个 subagent 分别负责 SDK、Store SQL、Agent 交付、TUI 客户端与渲染，主 agent 集成 ACP 请求锁范围与回归验证。约束：不修改数据库 schema、迁移、表、列或索引，不连接现场数据库执行写入；保留 canonical Delivered、CAS、Unknown、Stop/Resume 与恢复边界。下表区分已修复缺陷与仍有成本的机制，不把静态候选全标成已解决。

| 发现 | 本轮实现与证据 | 剩余边界 |
| --- | --- | --- |
| LAT-01 | 空闲 Enter 后 composer 显示本地“正在提交…”；宽屏/窄屏、中英文标题回归通过。直接 Pending 行仍隐藏，Delivered 才成为正式消息 | 是真实提交反馈，不是提前伪造 canonical 消息；不能证明执行时延下降 |
| LAT-02 | 命令排队、会话准备、operation gate、快照、请求/回执、ACP input/work 完成与 Store 阶段分别计时，不记录正文 | 未覆盖 Enter 到终端 flush 的全部事件关联；现场 P95/P99 未测 |
| LAT-03 | pending SQL 先过滤 reconciled，`NOT INDEXED` 避免随机主键全索引遍历；恢复子树与 mutation 排序、guard 回归通过 | schema 限制下仍扫表；guard 查询没有测得稳定收益，不能承诺规模无关 |
| LAT-04 | SQLite 快照四条 SQL 合一；远端公开快照四条减为两条、写准备四条合一；空闲 enqueue 完整快照读取六次减为三次 | 完整 WorkState JSON 解码、必要 CAS 与状态写回仍保留；没有做持久数据结构重构 |
| LAT-05 | command JSON 序列化移到写事务前，补事务阶段/字节数诊断 | 事务内必要校验、状态计算、写入与落盘未绕过 |
| LAT-06 | 分开记录准备、VM 等锁、缓存等锁、派生/视口；总阶段改称 `message-body-total` | 不再声称包含完整 frame 或终端 flush |
| LAT-07 | 短 VM 锁内获取 Arc/COW 不可变快照与 publication，锁外派生；旧快照在 reset 后仍稳定 | 原子快照读取仍可能等待短锁 |
| LAT-08 | generation 跳代保留未变冷历史的高度与 key，256 条历史只更新两条变更记录的计数回归通过 | 宽度、主题、全局布局变化仍需真实失效；轻量 key 扫描未删除 |
| LAT-09 | 无图候选正文避免扫描/复制；高亮缓存一次计量、锁外退役；超 256 KiB/block、16 KiB/line 或 4096 行保留纯文本并记录 skipped-budget | mutable tail、完成态解析与 wrap 仍同步；预算只限制高亮，不是硬时间上限 |
| LAT-10 | 通过缩短锁、减少冷历史重建与限制高亮降低共享循环的阻塞来源 | 没有替换 ratatui-kit 输入/渲染调度器；重型同步 Markdown 仍是后续候选 |
| LAT-11 | active activation 保留 dirty wake，旧 idle/可刷新阻塞结果在同一 continuation 重查；屏障测试覆盖 Stop/Resume、Unknown、退役与 drain | 单执行与 drain 上界保留，不重试未决执行 |
| LAT-12 | Refresh 成为受 shutdown 管理的单独后台任务，用户提交不再排在其 RPC 后；屏障回归通过 | 同一输入的提交仍保持必要顺序 |
| LAT-13 | 常规 RPC 首次登记后释放 gate，回执以 client/session/generation/prompt epoch 身份校验；接收与有序投影分离，准入不被投影卡住 | generation 绑定、load 与 Stop 的必要 gate 不删除；普通投影保持 FIFO |
| LAT-14 | input/work IO 只在短 sessions 锁内选定会话环境；snapshot/query 使用 host-owned task；阻塞 query 时另一 snapshot 与全局 sessions 可推进的回归通过 | input mutations 保留接收顺序；其他生命周期/配置请求仍串行，未引入每会话 actor 重构 |
| LAT-15 | registry/admission ledger 初始化后使用连接级 `busy_timeout=0`，运行期 Busy 有界异步退避；只在 BEGIN IMMEDIATE 尚未进入 callback 时重试。真实临时库独立 writer 异步释放后正常应用，callback/commit 失败不重放 | 构造期仍可同步等锁；单条 SQL、JSON 与 FULL commit/fsync 未 worker 化；没有修改 DDL/schema |
| LAT-16 | 相同 SDK admission 观察只读 session control 并复核 active SDK；WorkBoundary 重用已确认快照、ACK 前复核 control | 首次 admission、CAS 和 frozen 初始化仍持久校验 |
| LAT-17 | inbox hint 优先复用已读快照验证身份/内容，缺失才窄读；相同 delivery 冲突仍报错并回滚 | 外部 work available 的真实唤醒与未知回执不能跳过 |
| LAT-18 | ACP prompt 在同一次短锁绑定 cancel 并复制 payload，message 投影在锁外生成 | 首次历史加载、必要 payload 复制与执行装配仍存在；未把重型准备移动到 Delivered 之后 |
| LAT-19 | Receive 单独串行 gate，不再跨 Store IO 持有业务 state 锁；读写 state 使用短锁 | 首次初始化 ACK/RunStarted、frozen 的排他边界仍保留，不能绕过恢复裁决 |
| LAT-20 | projection 与本 mailbox 已通知集合分离，刷新先到 Delivered 后仍通知一次；恢复重建后 replay、重复提交与 generation 回归通过 | 无数据库通知 ACK；新 mailbox 实例允许恢复时再次投影，客户端按身份去重 |

### SQL-only 合成对照

SQLite 3.51.0、22,223 条历史命令、随机主键、11 次交替采样；每种场景比较结果一致，fixture 的 `sqlite_master` 和 schema version 前后不变。`READ_PENDING` 中位数：无 pending **21.251 → 4.745 ms**、只有无关 pending **19.981 → 4.312 ms**、子树 pending **22.615 → 4.997 ms**；`HAS_PENDING` 无 pending **5.436 → 4.348 ms**。guard 没有稳定收益，保留其变更是保持 pending 判定的相同 SQL 形态，不宣称提速。数据不是用户真实 WorkState，也不含 Rust/网络/渲染，不能换算成 Enter 提速倍数。脚本与完整采样留在 `/tmp/peri-lat03-synthetic-20261007.py` 与对应 results JSON，临时文件不作为长期仓库事实源。

### 集成验证

- workspace 的 `Cargo.lock` 同时被其他任务修改，根目录 `--locked` 命令要求更新 lockfile。本轮未覆盖该改动；使用 `1570464b` 的已提交 lockfile 加当前修复源文件建立隔离快照，`--locked --offline` 编译四个 crate 测试通过。快照不纳入外部任务的 request-retention 合约改动。
- 隔离快照默认 `cargo check --locked --offline`、四个 crate 的生产 `clippy --lib -- -D warnings` 与 workspace `cargo fmt --all --check` 通过；逐文件 rustfmt、staged typos、22 条依赖方向检查与 `git diff --cached --check` 通过。提交前逐字节核对暂存 Rust 与验证快照一致；根目录 hook 的 check/clippy 因其他任务的 lockfile 改动改用上述隔离验证，不覆盖该 lockfile。
- TUI 全量 lib：**1681 passed、0 failed、6 ignored**（单线程）；包含输入反馈、gate/Refresh/pump、VM publication、冷历史、高亮预算与 Markdown 内容回归。
- Agent 定向：mailbox **67**、Receive **7**、pipeline **5**、recovery **6** 通过；ACP user-input 定向 **18** 通过；Store work effects **8**、pending-work **13** 通过；durable work 合约 **26 passed、1 ignored**。
- ACP 全量 lib **797 passed、0 failed**；Store 全量 lib **468 passed、0 failed、21 ignored**。一次误跑较早的 ACP 编译产物触发旧 fixture 的重复阻塞，正确的当前产物定向与全量均通过，不据旧产物结果修改生产代码。
- SDK 八个局部 execution 文件独立复跑 **40 passed、0 failed、245 assertions**，包括新增 12 项 busy 回归；`tsc --noEmit` 通过。真实 writer 竞争测试只使用临时库，不触及用户数据库。
- 新提交标题的窄屏预算与高亮 owned-capacity fixture 经集成测试修正；全量 TUI 最终为绿。初次并行运行既有 paste 全局 atom 测试失败，单线程全量通过，未改动 unrelated paste 实现。
- 四条 Store DDL 常量与 SDK registry/admission DDL 对比 HEAD 一致；没有新增 migration 或修改 schema 版本。文件规模检查通过。
- 真实会话 Enter → draw/flush 现场验收仍待执行。因此本 issue 不在本轮提前关闭，也不宣称所有候选机制已消除。

### SDK lost-wake 可复现探针（LAT-11）

以下是调查基线的复现命令，仅使用内存 registry，5 秒超时；最后的 `unknown` 是 execute mock 主动抛错，不运行真实 Agent。断言刻意描述旧缺陷，**修复后预期不再满足旧断言**，正确行为验收改用 `test/execution-wake.test.ts`；不将 `REPRODUCED` 当作修复通过，也不在已修代码上把旧探针失败当回归。

```bash
cd npm-packages/@peri-sdk || exit 1
bun run - <<'TS'
import { ExecutionCoordinator } from "./src/execution/coordinator.ts";
import { MemoryExecutionRegistry } from "./src/execution/memory-registry.ts";

function assertCondition(condition: boolean, message: string): void {
    if (!condition) throw new Error(message);
}

const deadline = setTimeout(() => { throw new Error("probe timed out"); }, 5000);
const registry = new MemoryExecutionRegistry();
const control = {
    lifecycle: 1, revision: 0, controlGeneration: 0,
    status: "active" as const, attempt: null,
};
const instance = {
    instanceId: "lost-wake-probe", generationId: "generation-1",
    proofRoute: { kind: "external" as const, reference: "isolated-memory-probe" },
};
let work: {
    workId: string; revision: number; lifecycle: number; controlGeneration: number;
} | null = null;
let queryCount = 0;
let executionCount = 0;
let signalObserveControl!: () => void;
let releaseObserveControl!: () => void;
const observeControlReached = new Promise<void>((resolve) => {
    signalObserveControl = resolve;
});
const observeControlBarrier = new Promise<void>((resolve) => {
    releaseObserveControl = resolve;
});
let pauseNextObserveControl = true;
const originalApply = registry.apply.bind(registry);
registry.apply = async (command) => {
    if (command.action.kind === "observeControl" && pauseNextObserveControl) {
        pauseNextObserveControl = false;
        signalObserveControl();
        await observeControlBarrier;
    }
    return originalApply(command);
};
const coordinator = new ExecutionCoordinator({
    registry, instance, allowBestEffort: true, maxAttemptsPerWork: 2,
    proofProvider: { proveStopped: async () => ({ status: "unknown" as const }) },
    domain: {
        queryWork: async () => { queryCount++; return { control, work }; },
        execute: async () => {
            executionCount++;
            throw new Error("probe stops before real execution");
        },
        resolveExecution: async () => ({ status: "unknown" as const }),
        resolveWorkCommand: async () => ({ status: "unknown" as const }),
    },
});
const registration = await coordinator.registerInstance("isolated-session", "probe-registration");
assertCondition(registration.status === "idle", "registration failed");
const firstActivation = coordinator.ensureProcessing({ sessionId: "isolated-session", source: "notification" });
await observeControlReached;
work = { workId: "new-work", revision: 0, lifecycle: 1, controlGeneration: 0 };
const immediateWake = coordinator.ensureProcessing({ sessionId: "isolated-session", source: "notification" });
const samePromise = firstActivation === immediateWake;
releaseObserveControl();
const firstResult = await firstActivation;
const wakeResult = await immediateWake;
console.log(JSON.stringify({ phase: "overlapping-wake", samePromise, firstResult, wakeResult, queryCount, executionCount }));
assertCondition(samePromise && firstResult.status === "idle" && wakeResult.status === "idle", "unexpected wake behavior");
assertCondition(queryCount === 1 && executionCount === 0, "expected stale query to suppress new work");
const freshResult = await coordinator.ensureProcessing({ sessionId: "isolated-session", source: "inboxScan" });
console.log(JSON.stringify({ phase: "fresh-activation", status: freshResult.status, queryCount, executionCount }));
assertCondition(freshResult.status === "unknown" && queryCount === 3 && executionCount === 1, "fresh activation did not reach mock execution");
clearTimeout(deadline);
console.log("REPRODUCED: overlapping wake lost; fresh activation found work");
TS
```

预期核心结果：第一次及重叠唤醒均 idle，`samePromise=true/queryCount=1/executionCount=0`；第三次独立唤醒后 `status=unknown/queryCount=3/executionCount=1`。该探针复现窗口，不测真实 TUI 时延，不依赖 2 秒/5 秒兜底。
