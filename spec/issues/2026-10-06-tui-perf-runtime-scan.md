# TUI 性能专项：事件循环、后台轮询与任务生命周期

- 日期：2026-10-06。
- 状态：active / 静态扫描与 Astra 审计完成，行为计量与修复验收待实施。
- 范围：kit entry、notifier/bridge、service/workflow snapshots、effects、动画、鼠标/选择、面板刷新、空闲 timer、队列背压及任务取消。
- 本轮交付：仅新增本 issue；不修改源码、不清理工作树、不联网、不启动或停止用户进程。
- 定级：没有新增 P0；Astra 将本报告 R1–R7 均定为 P2，确认机制不等于现场高 CPU/RSS 主因。

最终裁决见 [Astra 审计 §5/§6](2026-10-06-tui-perf-astra-audit.md)。R2/R5/R6 由扫描建议 P1 下调 P2；R5 的正常 CLI 退出长期泄漏推断被驳回，存在 runtime drop 与显式 transport close。R6 唯一承接内存 M5 的队列/请求背压及 gap single-flight，R5 共用任务 owner 承接退出。R4 属于 kit render-demand 边界，Ctrl+C 退出是 raw 事件不 render 的例外；R7 已按最新 steer dirty 重核。下文是扫描证据，不以旧行号或初步推断覆盖审计。

## 1. 证据口径与去重

事实源为扫描时的 **当前 dirty 工作树**，不是 HEAD，也不是现场二进制。已读取 `docs/standards/index.md`、`tui.md`、`rust.md`、`architecture-contracts.md`、`testing.md`、`documentation.md`、`peri-tui/CLAUDE.md` 和 `docs/code-index/peri-tui.md`。本文件记录调查证据，不新增架构权威规则；实施仍遵守 ACP 边界、精确 cancel 身份及终态不能丢失的契约。

`peri-tui/Cargo.toml:41` 声明 ratatui-kit 的 semver 依赖；实际锁定版本是 `Cargo.lock:4503` 的 **0.10.3**。以下以本机可读的 registry 源码追踪，不使用相邻开发 checkout 代替已锁定依赖：

`/Users/konghayao/.cargo/registry/src/rsproxy.cn-e3de039b2554c837/ratatui-kit-0.10.3/`

下文 `KIT/...` 表示该目录下的相对路径，便于辨认本仓库与依赖的职责；依赖未修改。行号是此次扫描定位点，后续实施应重新核对。

### 已有 issue 的归属

| 既有 issue | 当前复核与本 issue 边界 |
| --- | --- |
| `2026-10-06-p0-dev-peri-high-cpu-memory.md` | 主要跟踪 runtime/store 工作账本与载荷读取。本 issue 只登记 TUI 的轮询生产端、调度/生命周期边界，不重报存储全量读取为 TUI 根因，也不因该 issue 标为 P0 就继承其优先级。 |
| `2026-09-27-p0-tui-streaming-view-rebuild-cpu.md` | 当前 bridge 有 single-pending publication scheduler，不能再用旧的逐 chunk 全树发布描述现状。 |
| `2026-10-06-tui-streaming-render-redundancy.md` | 按其“已实施/未完成”章节复核；不重复登记正文/推理 COW、Markdown 尾部解析、缓存拷贝、逐 VM 全帧扫描和 running slot content/chrome 分离。这里登记的是让这些成本在无内容变化时发生的 **触发源**。 |

具体纠正：`kit/acp_bridge.rs:529`、`:545` 是 deadline 驱动；`:564` 的秒级分支只在 running Bash 时请求刷新，静止时不无条件发布 VM。`kit/message_area/mod.rs:265` 使用逐帧缓冲复用，`:351` 使用当前 `build_slot_wrap_map`；不沿用旧 issue 的已失效行号或“每帧总是重新构建所有 wrap map”结论。`kit/service_snapshot.rs:382`、`:456` 已有 equality-gated atom publication，不能说所有服务 atoms 每 2 秒都写入。

### 主 agent 提供的现场样本（仅旁证）

采样来自本 repo 的 `target/debug/peri` 三个进程，时间窗口为 **2026-10-06 20:30:09–20:30:12 +0800**：

`/tmp/peri-tui-perf-20261006-2nsZNM/sample-{9500,55171,49449}.txt`

- `sample-9500.txt:63` / `:308` 出现 `Tree::render`；`:419` 出现 `selection::build_wrap_map`。
- `sample-55171.txt:63` / `:220` 出现 `Tree::render`；`:112` 出现 `build_wrap_map`；`:974` 出现 `service_snapshot::tick_once`。
- `sample-49449.txt:64` / `:322` 出现 `Tree::render`；`:390` 出现 `build_wrap_map`；`:2170` 出现 `service_snapshot::tick_once`。

这些只证明旧二进制采样窗口中相应路径被采到，不证明是空闲状态、由哪个 timer/事件唤醒、占据多少互斥 CPU 时间或发生任务泄漏。**二进制早于 dirty 修改，不能据此认定当前实现仍有旧 wrap-map/渲染行为。** 栈包含计数不可加总为 CPU 百分比，等待栈也不等于 CPU 消耗。现场完整 CPU、RSS、进程状态与版本对应汇总由主 agent 负责；本文件不另造主因或改善数字。

## 2. 发现摘要

| ID | 建议优先级 | 发现 | 静态置信度 / 现场归因 |
| --- | --- | --- | --- |
| R1 | P2 | Workflow 常驻轮询、相同快照仍通知，且异步响应未校验会话边界 | 高 / 未证明 |
| R2 | P2 | History 已加载页每 2 秒从首页重取；隐藏面板仍刷新服务投影 | 高 / 只有旧 binary 的 tick 栈旁证 |
| R3 | P2 | 空闲仍有 100ms spinner 检查与 5s 强制全树心跳 | 高 / 未证明 |
| R4 | P2 | raw 事件无论是否改变 UI 都触发 update+draw；节流不等于渲染限频 | 高 / 只有旧 binary 的 render 栈旁证 |
| R5 | P2 | 转发/子提交 task 缺统一 owner/join；正常 CLI runtime drop 反证长期泄漏推断 | 高（owner 缺口）/ 未证明长期残留 |
| R6 | P2（压力验证） | 两级事件队列与提交 fan-out 没有容量/并发背压，合并 M5 | 高（结构）/ 中（增长条件），现场积压未测 |
| R7 | P2 | Steer 5s 对账接受相同 revision 仍写状态，失败去重集合生命周期无清理 | 高 / 未证明 |

## 3. 逐项证据与验收

### R1 — Workflow 无变化轮询/通知，以及迟到响应跨会话发布

- **精确可达链**：`kit/entry.rs:429` 无面板可见性条件地启动 `spawn_workflow_poll` → `kit/workflow_snapshot.rs:95` 的 2s interval → `:104` 取当前 session → `:107` 请求 `workflow/list_runs` → `:112` 全量反序列化 → `:128` 无条件替换 `WORKFLOW_SNAPSHOT`。显示消费为 `kit/panels/workflow.rs:33` / `:34` 的 atom 订阅及完整快照 clone。
- **事实**：`WorkflowSnapshot`/run 类型已可比较，但写入处不判等；没有 active panel/caps gating；无 session 时也写 `Some(empty)`。`KIT/src/reactive_handle.rs:299` / `:310` 在可变解引用 guard Drop 时置 changed 并 wake，不比较新旧值。显示 Workflow 面板时相同值仍能唤醒树；面板隐藏且没有 subscriber 时，不能把 atom 写入直接等同于一帧重绘。
- **频率/增长**：响应及时且调度未拖慢时约每 2s 一次，首次 tick 立即；单任务串行等待 RPC，不是每 2s 并发堆出一个新请求。数据成本随 run、phase、agent 数及字符串载荷增加。面板打开时无变化通知叠加每帧 clone；无会话/隐藏面板时仍有 polling task 的构造/写入工作。
- **边界缺口**：取到 sid 后 await；返回时未重新检查 sid/reset epoch。A 的请求挂起期间切到 B，A 响应仍可覆盖共享 snapshot；这是静态可构造竞态，不宣称现场已发生。停止响应后 task 的 shutdown 问题另归 R5。
- **置信度/影响**：上述行为高置信；CPU 主因未验证。冗余 RPC/解析/通知及 Workflow 面板短时显示错误会话结果。
- **验证**：mock transport 连续返回相同快照，分别测试无 session、隐藏/打开面板、A→B 切换、断连；计数 RPC、解码、atom mutation、树更新。延迟 A 的响应直至 B 已发布，再释放 A。
- **验收**：相同有效快照不增加可见投影 mutation；迟到旧会话响应不能覆盖新会话；隐藏/不支持 Workflow 时不持续拉完整 run 树（若仍需保活，明确频率/数据预算）；恢复面板与终态仍及时可见。错误须保留日志/错误状态，不能仅以空快照掩盖。

### R2 — 服务轮询全量重建与 History 页数放大

- **精确可达链**：`kit/entry.rs:329` → `kit/service_snapshot.rs:94` 2s interval 或 `:106` execution cwd 变化 → `:122` `tick_once` → `:223` 到期 refresh → `:401` `refresh_threads` → `:428` 重置 cursor → `:430` 遍历 `list_page_count` → `:437` / `:439` 逐页 RPC 并重新组成列表。
- **增长来源**：`kit/panels/thread_browser.rs:625` / `:635` 根据已加载页请求下一页，`THREAD_LIST_PAGE_COUNT` 是全局 atom（`kit/atoms.rs:345`）。service tick 不检测 History 是否打开。page_count 为 P 且仍有这些页时，每个刷新重复读取前 P 页，不是只拉新页；每周期处理的 metadata 数随已加载页数增长。实际存在页数/早停 cursor 才决定 RPC 数，不能把 P 一概当成已发送数。
- **其他全量源**：`service_snapshot.rs:193` 每 tick `list_tasks` 再派生 cron jobs；`:281` → `service_snapshot/session_services.rs:38` / `:40` 并发完整 `plugin/list`、`mcp/list`；`:299` 每 tick `session/metadata`。`service_snapshot.rs:245` 的 thread 扫描间隔为 2s，不是 30s；当前 session 标题没有独立低频 deadline。文件与 memory 已在 `:250` / `:263` 门控为 30s，不能报告为每 2s 重新扫盘，但 `:258` / `:273` 仍每 tick clone 慢频缓存。
- **事实/假设**：equality gate（`:456`）抑制最终 write，不消除前面的 RPC、解码、Vec/String 构造、clone 与 O(n) equality。静态确定生产端重复工作；服务端实际数据库/网络成本归对应模块计量，不能推定调用都读巨大 ledger。
- **频率/影响**：通常约 2s，cwd 变更可额外触发；刷新耗时和 Delay 行为会改变实际频率。无服务变化时仍做近似 O(已加载线程 + cron + 插件/MCP + 慢频缓存) 的工作；多个 TUI 各自重复。列表浏览越深，隐藏后仍可能持续放大后台消耗。置信度高；仅有旧 binary tick 栈可达旁证，无当前耗时数据。
- **验证**：相同会话数据，分别加载 1/10/更多实际存在页并关闭 History；用 mock 请求计数记录每周期方法、页数、解码条数和 copied bytes；同时记录终端更新次数以区分“无写入”和“无工作”。覆盖 cwd 变更及慢 RPC，不触真实远端。
- **验收**：增页只增量请求缺失页面，未变已加载页不每周期重放；隐藏面板无持续全页拉取。当前会话状态与 cron/MCP 真正变化仍按明确新鲜度预算展示；不靠延长所有轮询或削弱 ACP 环境语义止血。慢频缓存未变化时不重复大载荷 clone/比较，或给出可核对的有限成本设计。

### R3 — 静止状态仍被动画检查与 watchdog 唤醒

- **精确可达链**：`kit/entry.rs:235` 心跳 task → `:239` sleep 5s → `:240` 写 `RENDER_HEARTBEAT` → `kit/app_shell.rs:31` 常驻订阅 → `KIT/src/render/tree.rs:84` state wait 返回 → `:80` render → `:52` update 整树及 `:69` draw。另有 `entry.rs:257` spinner task → `:267` 每 100ms sleep → `:268` 读取 loading。
- **事实**：spinner 非 loading 时不写 heartbeat，但 task 仍每约 100ms 醒来查 atom；注释“非 loading 避免 CPU 唤醒”不能作为无唤醒事实。loading 时即便在等待 HITL/网络、终端失焦或动画不在可见区域，仍按帧变化写根 heartbeat；没有上述可见性/焦点门控。5s watchdog 独立于 loading 始终写入。秒级 bridge tick 另在 `acp_bridge.rs:532` / `:558` 检测 reset、`:564` 搜索 running Bash，静止时不发布 VM。
- **频率/增长**：约 10/s 检查，空闲强制心跳约 1/5s；loading 心跳约 10/s（运行时调度可使其更慢）。强制 render 的每次成本随当前树/历史规模增长；bridge 的工具/子 turn 搜索随当前 turn 的工具/子树增长。不同 timer/事件可以合并唤醒，不能把频率相加等同于实测 FPS。
- **置信度/影响**：高置信存在冗余唤醒；未验证其对 CPU 的占比，不把 10 个轻量 read/s 称为高 CPU 主因。watchdog 有窗口切换恢复目的，不能直接删除而不验证替代。
- **验证**：空会话、长历史 idle、loading 无新增输出、HITL 等待、失焦分别计数 timer poll、heartbeat mutation、update/draw；用虚拟时间验证状态切换启停，不做 wall-clock 易抖断言。
- **验收**：无可见动画时不保留 100ms 周期检查；有动画时仅以实际帧变化和可见需求调度；watchdog 的恢复能力有行为回归，空闲整树 render 有独立、明确且可测试的预算。timer 取消后 task 必须可收尾，不降低 cancel/reset 可达性。

### R4 — 无变化 raw 事件也必定整树 render，鼠标节流不控制 FPS

- **精确可达链**：`kit/entry.rs:545` fullscreen → `KIT/src/render/tree.rs:85` / `:86` 等 state 或 terminal event → `:97` dispatch → `:100` 无条件 continue → `:80` render（包含 update + draw）。`KIT/src/render/tree.rs:55` 每帧重建输入注册表。
- **事实**：事件被 Ignored、命中不变，或滚轮仅累计 throttle pending，都不阻止下一次 render。`kit/message_area/handlers.rs:331` 的 hover handler 对非 Moved/命中不变不写状态（`:346` 比较），最终仍 Ignored；框架不使用该结果判定是否有渲染需求。`kit/panel_scroll.rs:158` 的 due flush 与现有消息区节流限的是状态落地，不是 kit loop 每事件一帧。选择/拖拽应保持事件顺序与 pointer capture，不能粗暴丢事件。
- **频率/增长**：只要终端送入并被循环取出的 raw 事件就会重复此路径；Moved/滚轮事件率由终端和用户输入决定，无统一固定 Hz。每次帧仍至少进入 `kit/message_area/mod.rs:269` 的 VM 扫描以及组件协调/draw；这部分扫描优化归旧 streaming issue 的 F5，不重报其实现成本。
- **置信度/影响**：框架行为高置信；当前用户现场事件率未知。输入风暴可能以无变化事件率放大渲染 CPU，抢占实际 chunk 展示/交互响应时间；不是无输入时的自旋证据。
- **验证**：在本地 harness 注入大量不命中/同目标 Moved、重复无效按键、面板和消息区节流滚轮，分别数 raw events、有效 mutation、update/draw；比较无变化事件与真实选区拖动。额外验证退出型/纯副作用 handler 不写 State 时仍能退出。
- **验收**：无变化事件不逐个导致全树 update/draw；合并拖动/滚轮时最后位置、滚动落点、click/Up/Down、capture 与 resize 不丢；终止/退出型 handler 无需依赖偶然 atom write 才生效。应从事件循环 dirty/render-demand 契约解决，不只在应用 handler 中减少 write。

### R5 — UI task owner 不完整，取消 token 不等于已退出

- **确定缺口**：`kit/entry.rs:370` 建 `LOCAL_EVENT_TX` 并存入全局 OnceLock → `:374` mini bridge task 只 `recv().await` / send，没有 shutdown 分支、也没有保留 JoinHandle。退出 `:553` cancel 不会唤醒它；全局强 sender 活着时它可在同一 runtime 中继续等待到进程/runtime 退出。此任务通常 parked，**不是 CPU 自旋**，也不能据此推定单次启动无限新增 task。
- **fan-out 可达链**：`entry.rs:419` → `kit/submit_consumer.rs:58` → `:73` 为每次 SubmitRequest detach 子 task → `:74` await `handle_submit`。子 task 没有继承 shutdown token、JoinSet 或独立句柄；父 consumer 停止不意味着已发出的 prompt/control 子任务停止。`submit_consumer.rs:274` 的 model 切换也是独立 task。
- **in-flight await 可达链**：`entry.rs:429` → `workflow_snapshot.rs:103` tick 分支内 `:110` RPC await；shutdown 只在外层 select，因此连接未断但 RPC 不响应时不能仅靠 token 停止。`kit/hitl_response.rs:66` / `:69` 同样在 receive 分支内 await handler。`acp_client/client/requests.rs:22` 直接 await transport，`peri-acp/src/transport/mod.rs:36` 明确 connected silence 无内建 timeout。
- **收尾事实与反证**：`entry.rs:418` 等句柄只是 `_handle` 局部绑定，没有统一 drain；退出是 `:553` cancel 后 `:556` teardown。`launch.rs:263` 的 deployment shutdown 和 `acp_client/client.rs:119` 的 explicit close 会帮助结束 pending RPC，因此不能说所有 await 在真实退出后永久泄漏。`service_snapshot.rs:119`、`steer_consumer.rs:73` 已对采样/执行单独 select shutdown；`submit_consumer.rs:439` 的 cancel RPC 已有 timeout，也不能泛称所有消费者无取消预算。
- **频率/增长条件**：每个被接受的 submit 一个 child；connected silence/很慢响应时，活跃 child 随未完成请求数增长。mini bridge 是每次装配的驻留任务缺口，正常 CLI 进程结束会由 runtime 回收；同 runtime 退出 TUI、服务模式复用、故障测试等场景才暴露 owner 生命周期不闭合。
- **置信度/影响**：结构高置信；真实泄漏数量/CPU 未测。可能保留 client、请求 payload 与 pending 状态；退出时无法核对 UI tasks 是否静止，迟到错误还可能执行 `submit_consumer.rs:76` 清全局 loading。协议 cancel Agent 与停止本地 observer 是不同语义，不能以 abort 本地 future 伪造 Agent 已取消。
- **验证**：mock 保持连接但不响应 workflow/HITL/prompt；只 cancel UI token，观察各 task 完成信号；再 explicit close 对比；同时测正常退出、面板卸载/会话切换与 mini bridge 的 receiver 关闭。用任务完成/owner drop 与 pending registration 计数，不只测函数返回。
- **验收**：所有 UI 派生任务有明确 owner、取消与有界 join/drain；mini bridge 不依赖进程退出才能停；in-flight observer RPC 在 shutdown 时可释放，server 执行任务按 ACP 契约独立结算；退出后无晚到 UI 写入，pending 请求注册清理有断言。资源/host Incomplete 应明确报告，不能以发出 cancel 当完成。

### R6 — 事件链路与 Submit fan-out 缺少背压预算

- **精确可达链**：`acp_client/client.rs:160` notification unbounded channel → `acp_client/client/pump.rs:155` / `:447` 发送通知 → `kit/acp_notifier.rs:79` recv/DTO 转换 → `:140` 向 bridge unbounded send（`kit/entry.rs:368`）→ `kit/acp_bridge.rs:575` 单条消费 → `:637` dispatch / `:641` scheduler。submit 入口是 `entry.rs:341` unbounded channel → `submit_consumer.rs:73` 无并发准入的 task fan-out。
- **事实/假设**：50ms publication scheduler 合并的是状态发布，不是入队时合并/背压，也不限制两级队列字节或每个输入事件的解码/ingest 工作。单 task 消费、同步转换和持 operation gate 发布构成有限服务率。容量无限是事实；只有生产率 λ 长时间大于消费率 μ 才有队列增长，约随积压事件/字节增长；本轮没有测 λ、μ 或实际 queue len。输入 UI 还有交互门控，不能声称普通用户任意按键一定无限 fan-out。
- **频率/影响**：受 ACP chunk/后台事件及已接纳提交速率驱动，不是固定 timer；慢渲染/大 payload/发布 gate 竞争时更易积压。可能放大内存与消息/终态延迟，短 burst 也可能在结束后需要较长 drain。没有证据支持当前现场 CPU 高必然由 backlog 导致。
- **置信度**：结构高、增长风险中；P1 以压力验证为优先，不按未量化 RSS 标 P0。
- **验证**：mock 可控速率与大小的文本、工具、后台事件；阻塞/减慢 bridge publication，观察两级 queue events/bytes、最大 live submit 数、终态可见延迟、shutdown drain。覆盖顺序、session epoch 过滤及 terminal 与 chunk 相邻的 burst。
- **验收**：事件/字节与提交并发都有明确有限预算和超载语义；不能静默丢终态/交互/工具身份或打乱事件顺序。若采用批量/合并，文本完整且 session 隔离；超载须可观察；停止生产后积压收敛到零，cancel/退出不等待无限普通事件 backlog。

### R7 — Steer 相同版本对账仍通知；失败去重集合随会话操作保留

- **精确可达链**：`kit/entry.rs:412` → `kit/steer_consumer.rs:15` / `:57` 5s reconciliation interval → `:63` 无 retry 时生成 `refresh_command` → `:124` / `:126` active session 且功能 enabled 才请求 → `:244` 全量 user input snapshot → `:246` `establish_session_snapshot` → `kit/steer_state.rs:433` 可变写 → `:435` `accept_snapshot`；`:91` 只拒绝 revision 更旧/不同 generation，`:102` 相同 revision 也替换 snapshot。
- **事实**：即使没有 queued input、snapshot 内容不变，enabled+active session 仍可周期 Refresh。`:259` 的 `bind_command_generation` 还有一次 `STEERS` 可变访问。是否实际唤醒具体组件取决于 atom subscriber；不能把每一次 write 都直接当一次 draw。同 revision 不变短路缺失是静态事实，不等于服务端每次都读完整 SessionData。
- **次级增长**：consumer 的 `steer_consumer.rs:56` `warned: HashSet` 在 `:93` 记录失败的非 Refresh command ID，直到 consumer 退出未清理/按 epoch reset。它不随每个成功输入增长，也不随纯 Refresh 失败增长；增长条件是同一 TUI 生命周期中越来越多不同失败命令。retry queue 的重试有 timeout/epoch 条件（`:100`），不能宣称无延迟 tight loop。
- **频率/影响**：正常每约 5s，RPC/执行耗时可拉长；snapshot 处理成本随队列条目与原稿文本增长；warned 集合随不同失败 ID 数增长。置信度高，现场主因未测；无变化状态通知与有条件长期内存残留。
- **验证**：enabled/disabled、有/无 active session，连续同 revision、较新 revision、generation/epoch 变化、失败和 Unknown receipt；数请求、mutation、wake、warned 容量。测试 Unknown 必须仍按原 command 对账，禁止为了减轮询重投新身份。
- **验收**：同有效 revision/内容且无待结算命令时不增加投影 mutation；对账保持受理未知状态的恢复语义与明确 freshness；去重集合有终态/epoch 生命周期或容量预算。是否以事件替代 Refresh 需完整断连/重连验证，不能移除 reconciliation 后丢恢复能力。

## 4. 覆盖记录与暂不升级的疑点

- **effects**：`kit/app_shell.rs:69` / `:84` 是依赖驱动退出，`:100` 为 mount-only panic hook；`kit/message_area/mod.rs:510` / `:535` 的 auto-follow 依赖包含 generation/几何/epoch，`scroll/auto_follow.rs:252` / `:271` 有增长判定；没有据此建立“每帧 effect 无条件自激”的结论。输入恢复、popup、steer queue、image overlay effect 按各自状态变化执行；后续用 mutation 来源计数验证，不凭 effect 存在判高 CPU。
- **鼠标选择/面板**：消息区 slot wrap cache 使用当前修改后的实现；选择 wrapping 的算量归 streaming issue。`kit/panel_overlay.rs:49` 注册全局 scroll handler，`:52` 每 render 尝试 due flush；`kit/panel_scroll.rs:182` 使用 `try_write_no_update`，不能写成确定的 render 写 atom 自激。其空扫描/handler 重注册由 R4 的无变化帧触发覆盖。
- **图片 hover**：`kit/message_area/handlers.rs:34` 300ms debounce；`:377` 每次新目标 spawn sleeper，`:376` weak gate、`:383` generation check 已防迟到写。快速目标变化仍有暂存 sleeping task，约随“目标切换率 × 300ms”增长，正常不是永久泄漏；不另标 P1。若保留 task owner 后续统一管理，可覆盖卸载/退出 cancel。
- **插件搜索**：`kit/panels/plugin/discover_handler.rs:225` detach launch；`search_request.rs:31` 在 hook 借用冲突时按 `:45` 1ms sleep 重试。只有 `:35` completion 返回未消费结果才进入等待，RPC 无响应本身不会触发 1ms delivery retry。`discover.rs:62` Drop 及取消 ticket 已提供生命周期门控；目前仅列低置信 CPU 风险观察点，不宣称 1kHz 常驻轮询或无限泄漏。可用人工保持借用的本地测试量化，需有证据再升级。
- **取消**：显式用户 cancel 的 command/attempt 定位已有当前代码与契约测试，R5 不改 Agent 的 cancel 结算权威。service 和 steer 已有 inner cancellation select；秒级 bridge tick 使用 Skip，service/workflow 使用 Delay，不登记默认 Burst catch-up 风暴。

## 5. 后续验证计划与关闭条件

1. 固定已包含 dirty 修改的构建身份（git revision + dirty diff 摘要、lock、profile、binary hash、启动参数）。旧 `sample` 只作取样入口提示，不作该构建修复前基线。
2. 本地可控负载矩阵：空 idle / 长历史 idle / Workflow 开关 / History 已加载多页后关闭 / 相同 Steer revision / loading 无输出 / 鼠标无变化风暴 / ACP 事件 burst / connected silence + shutdown。
3. 优先计量可观察行为：per-source wake、atom mutation、update/draw、RPC 方法/页/解码条目/bytes、queue depth/bytes、live tasks、退出完成信号。计数要轻量，不记录正文、密钥或连接参数；当前 bridge 的测试计数不能冒充生产完整计量。
4. 测试分层：纯逻辑/virtual-time 调度与生命周期契约用 local mock；显示正确性与现场 CPU/RSS 由主 agent 按同构建/负载手工核验，不新增截图视觉框架。测试不连接真实模型/远端。后续 Rust 定向命令使用 `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- <精确模块过滤词>`，确认实际命中测试，必要时按 `e2e/CLAUDE.md` 走交互门禁。
5. 本轮未运行 Cargo/benchmark，未新增埋点；只进行静态读取及主 agent 提供样本检索。关闭需 R1–R7 各自行为验收或有证据撤销，且不回归协议终态、交互、滚动/选择、恢复与退出。现场占比未知的发现允许降级/排除，不允许用“issue 已写”或“cancel 已发送”替代完成。

覆盖限制：本轮是上述主路径的定向静态扫描，不是全部组件、面板异步操作或依赖内部任务的穷举审计；未证明每个 effect 都无冗余写入，也未审计全部外部进程/图片 worker 生命周期。这些未覆盖项留给后续 Astra 审计，不影响已列证据的独立验证。

文档路由核对：仅新增 active spec，未变架构/模块入口/规则，无需改 standards、CLAUDE 或 code-index；现有源码和 issue 的 dirty 修改均保持原状。
