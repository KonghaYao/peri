# P0：TUI 流式发布重复处理历史工具卡，且发布调度与投影状态混用

**状态**：已修复，待现场性能验收；测试与对抗审查见末节。
**优先级**：P0（用户指定）。
**创建 / 最后核查**：2026-09-27（Asia/Shanghai）。
**范围**：按后续授权完成 S1–S4 修复、回归测试与提交；保留事故时证据，不把测试计数等同于现场 CPU 收益。
**来源**：本机两个 debug TUI 在活跃 subagent 流式输出期间 CPU 暴涨。

## 结论与证据边界

以下为修复前的事故机制；源码、合成诊断和现场采样共同支持：

```text
历史回放：Running/Preview → 工具完成，但 committed 仍保留 Preview
  → 每次发布重新从 committed 组装，重新折叠为 Collapsed
  → 重复 clone / format / 全文 hash / 共享节点 COW
subagent 每个 chunk 在 handler 内直接发布
  → 放大上述历史处理成本
消息区每次构建仍扫描全部 slots；generation 驱动的 effect 可再次唤醒渲染
  → 增加主线程负担
```

还发现一条独立的正确性缺陷：主 Agent handler 的 `push_acp_state()` 会提前物化 ViewModel 并清掉 scheduler 使用的 dirty 标记，使主 Agent chunk 返回 `PublicationIntent::None`。所以不能再宣称「主 Agent 流式已正常受到 50 ms 合帧保护」；也不能仅删除 subagent 直推就认定修复完成。

| 判断 | 核查结果 |
| --- | --- |
| 回放完成的工具卡持续产生折叠失配 | 已证实：无用户覆盖时，回放开始设 Preview，结束保留 Preview，而终态策略要求 Collapsed |
| 失配使稳定历史反复 clone/hash/COW | 已证实：pass 只修改临时快照；64 张回放卡重复发布 20 次产生 1,280 次折叠写入 |
| subagent Streaming/Block chunk 逐条立即发布 | 已证实，且属于修复前设计许可，不是当时代码偏离设计 |
| 主 Agent 50 ms 调度在真实 handler 链路正常工作 | 被诊断反例推翻：文本与推理各 100 个 chunk，分别 100 次投影、0 次发布 |
| 现场所有失配都来自 replay、共有约 800 张卡 | 未证实：采样不能反推出卡片数与来源占比；用户覆盖也能造成重复变换 |
| 修复后总 CPU / 每次 publication 已与历史长度脱钩 | 未验证，也不是仅修复 fold + cadence 就能保证的性质 |

修复已同步 [流式 Markdown 性能设计](../../docs/design/tui-streaming-markdown-performance.md)：子流改为首块立即、后续合帧；主 Agent 发布状态与 projection dirty 分离。以下根因章节描述修复前实现。

## 现场数据（保留原观测口径）

### 进程与 CPU

两个进程均由本仓库 `./dev.sh` → `cargo run -p peri-tui` 启动，二进制为 `target/debug/peri`。事件与日志持续增长，不能称为空闲自旋。

| 项目 | PID 21485 | PID 68130 |
| --- | --- | --- |
| 原观测时运行时长 | 1:45:26 | 21:35 |
| `ps` 瞬时 %CPU | 172.9 | 155.2 |
| 10 秒 CPU 时间差分 | +15.98 s，约 160% | +15.16 s，约 152% |
| 线程分布 | 主线程约 50% + 一条 worker 约 98% | 主线程约 48% + 一条 worker 约 98% |

共享日志 `.tmp/agent-tui.2026-09-27` 原窗口增长约 341 KB/s。原日志尾部事件带 subagent source ID；两份 sample 落在 `handle_reasoning_chunk` 直接调用 `push_view_models` 的路径，与 subagent 分支吻合。共享日志不能独立给出每个 PID 的完整事件归属或发布次数。

### 采样

原 PID 21485：3 秒窗口、热点线程 1557 个样本。下表是同一调用分支的包含计数，不是可相加的互斥 CPU 百分比，也不是整进程 CPU 占比。

| 调用分支 | 样本 | 相对该线程样本 |
| --- | --- | --- |
| `handle_reasoning_chunk` | 1549 | 99% |
| `push_view_models` 中主热点分支 | 1251 | 80% |
| `apply_fold_pass` 中主热点分支 | 995 | 64% |
| `TuiToolCard::recompute_hash` 中主热点分支 | 796 | 51% |
| `tui_hash_str` 中主热点分支 | 790 | 51% |

原 PID 68130 的 2 秒 sample 也命中同链路。2026-09-27 15:40:04 +0800 再对 PID 68130 采样 3 秒，仍出现同一路径（对应主分支计数 1894 / 1696 / 1326 / 1061 / 1055）。这些计数用于确认热点仍存在，不用于比较不同窗口的性能收益。

本机原始文件：`/tmp/p21485.sample.txt`、`/tmp/p68130.sample.txt`、`/tmp/peri-cpu-review-68130.sample.txt`。这些是临时证据，非仓库 fixture；文件失效后需重新采样。

### 消息区构建次数与成本

原 120 KB 日志窗口覆盖 0.338 秒，包含 112 次 `frame-total`，即两进程合计约 330 次 **MessageArea 构建/秒**；该埋点不覆盖完整终端帧，不等于 publication 或屏幕实际刷新次数。

| 埋点 | 原样本耗时 / 元数据 |
| --- | --- |
| `hash+detect` | 567 µs；items=2555，rebuilds=0 |
| `concat` | 604 µs；slots=2555，logic_lines=3282，vis_rows=3282 |
| `viewport` | 372 µs；vp_lines=11 |
| `frame-total` | 1418–1596 µs；gen 示例为 111435 |

`rebuilds=0` 只说明该次消息区 slot 缓存无需重建，不说明整个界面或所有构建都没有视觉变化。后续 07:42:42–07:43:11 UTC 的约 2 MB 日志尾部中，4274 次 `hash+detect` 有 2197 次 rebuilds=0、2077 次 rebuilds=1，不能用单条零重建样本推断全部窗口。

`push_acp_state()` 仅在 snapshot 不等时赋值；当前锁定的 ratatui-kit 0.10.3 写 guard 仅发生可变解引用时通知，故并非每个 chunk 都通过 ACP_STATE 再唤醒一帧。`run_auto_follow` 依赖 generation，且无条件写多个响应式哨兵，是另一条可重复唤醒路径；心跳、动画和交互也会触发构建。不能用「330 ÷ 40」证明未经合帧的发布量。实际 publication cadence 须独立计数。

## 根因

### P0-1：回放完成态未归一化，派生折叠结果又不复用

事实源：`peri-tui/src/kit/acp_events/tool.rs::{handle_replay_tool_started,update_committed_tool_card}`、`render.rs::push_view_models`、`fold.rs::apply_fold_pass`（修复前位于 render.rs）、`tui_render_unit/{fold,tool_card}.rs`。

1. 回放开始将卡片存入 `committed`，fold 为 `fold_for_status(Tool, Running) = Preview`。
2. 回放结束写入输出并令 `is_running=false`，但保留 `fold: card.fold`。无覆盖时新的目标为 `fold_for_status(Tool, Completed|Error) = Collapsed`。
3. `push_view_models` 从 `committed.clone()` 组装快照；clone 本身是 im::Vector 结构共享，不能描述为全历史正文深拷贝。pass 对失配项 clone、重算 hash，再向临时 vector set；set 可触发共享 chunk 的 COW，连带复制邻近条目。
4. pass 没有回写 `committed` 或保留可复用的历史折叠投影，所以下次又对同一失配重做。
5. hash 把输入、输出、presentation 等字段格式化后全文计算；成本不仅取决于卡片数，也取决于失配卡片携带的内容体积。

正常实时工具完成走 `build_tool_card`，按当前状态取 fold；此前 `perf_probe_test::build_state` 用实时工具生命周期构造历史，未覆盖 replay 的 Preview 残留。用户覆盖是另一条需覆盖的失配来源；不能把 replay 解释成唯一来源。

### P0-2：subagent 直接发布发生在 scheduler 之前

事实源：`acp_events/streaming.rs::{handle_text_chunk,handle_reasoning_chunk}`、`acp_events/mod.rs::dispatch_for_bridge`、`acp_bridge.rs::PublicationScheduler`。

已路由到 subagent 组的 Streaming/Block chunk 直接调用 `push_view_models`，随后 generation 变化让 dispatcher 返回 Immediate。**昂贵工作已经发生**，scheduler 无法事后合帧；generation 分支是「已经发布」的结果，不是直推的起因。None 模式不走这次直推；BG 组不存在的路由也不同，不能推广为所有带 agent_id 的事件都发布。

现行设计许可这一行为，问题是事件率与历史处理成本相乘。50 ms fixed deadline 机制确实存在，但不是所有 publication 的严格 20/s 全局上限：首块、边界、终态等 Immediate 均是例外，1 秒工具时长刷新也有直推入口。

### P0-3：缓存投影 dirty 被误作待发布状态

事实源：`render.rs::push_acp_state`、`acp_types/current_turn/projection.rs::{view_models,sync_cache}`、`current_turn.rs::has_unprojected_changes`。

```text
主 Agent append → cache_dirty=true
  → handler 尾部 push_acp_state
  → 为 view_count 调用 current_turn.view_models()
  → sync_cache，cache_dirty=false（仅物化缓存，并未写 VIEW_MODELS）
  → dispatch_for_bridge 检查 has_unprojected_changes=false
  → 返回 None，scheduler 没有 deadline，也没有首 chunk publication
```

这是投影与发布生命周期混用。即便去掉这次隐式读取，也必须防止明确读取、诊断或未来消费者再次消掉待发布事实。该问题不能解释现场 subagent 的逐 chunk CPU 热点，但会使「仅删除 subagent 直推」的方案丢失可见更新，因此纳入同一修复链。

### 放大项：全 slots 扫描与重复 effect 通知

`message_area/mod.rs` 每次构建收集 hash/动画状态、检查 slots 并组装 prefix index，仍有 O(N) 元数据工作。`message_area/scroll/auto_follow.rs::run_auto_follow` 对多个哨兵无条件赋值，可在 publication 引起的构建后再次通知。修正 cadence 能减少触发次数，但不自动消除单次扫描或历史驻留；有界历史窗口仍是独立任务。

## 已执行的确定性诊断

本轮之前已在现有 bridge 测试模块临时挂载三个诊断用例，使用真实 replay handler、dispatcher 与 scheduler。命令 `cargo test -p peri-tui --lib cpu_review_ -- --nocapture --test-threads=1`：exit 0，3 passed / 0 failed。测试断言的是**当前缺陷行为**，不是修复后的通过证据。

| 用例 | 输入与控制 | 观察 |
| --- | --- | --- |
| `cpu_review_replay_fold_repeated_writes` | 64 张回放完成卡，各 1 KiB 合成输出；预热后 push 20 次 | FoldPassWrites=1280；committed 卡片仍 Preview。只把合成输入 fold 校正后，同样 20 次 push 为 0 |
| `cpu_review_subagent_bypasses_scheduler` | 已存在 child 组；固定 scheduler 时间连续 100 个 reasoning chunk | 100 次 Intermediate publication、100 次 generation 增长、无 pending deadline |
| `cpu_review_main_chunk_projection_consumes_dirty` | 空 live turn；text/reasoning 分别 100 个 chunk；默认 Streaming | 每组 100 次 projection、0 次 publication，intent 全为 None；推进 deadline 也无发布 |

测试入口已删除、源文件恢复；诊断源暂存 `/tmp/peri-cpu-review-tests.rs`。该命令在入口删除后不会再命中测试，实施时应把场景转成正式回归并确认实际执行数量。它们证明机制，不证明现场失配规模、release 收益、BG/Block/None 全矩阵或真实终端体验。

`PerfCounters`、`PerfCounter`、`observe_perf` 均受 `#[cfg(test)]` 控制，不能在已有 debug/release 进程中直接开启。现场要测 publication/失配次数须另加受控诊断构建或埋点；需按 PID/session 分开，仅记计数、类型、长度，不记录用户内容。

## 解决方案与实际落地

采用「发布状态独立 + 主/子流统一调度 + 历史折叠投影复用」。以下保留方案要求，具体落地取舍见实施记录。方案目标是解除当前重复大内容工作并控制中间发布频率，不宣称本轮让全部 TUI 工作变成 O(增量)。

### S1：分离投影状态与发布状态，明确发布边界

- `CurrentTurn.cache_dirty` 只管 canonical → owned VM 缓存。bridge 单独持有待发布 revision 与已发布 revision，或等价的显式状态；**普通投影读取不得推进已发布状态**。
- 发布失效覆盖 current_turn、committed 同长度替换、phase、折叠覆盖及其他影响 VIEW_MODELS 的输入；不能只以字符串长度或 current_turn dirty 判定。reset/session 使用独立 epoch，避免旧 pending 与新 revision 混用。
- handler 负责 canonical mutation 和显式 publication intent；bridge 的统一发布函数负责取投影、派生快照、写 VIEW_MODELS 并在写入成功后标记 revision 已发布。去掉依赖 generation 差值推断意图的协议，generation 仅标识成功发布的快照。
- handler 的终态与交互保留同步 `publish_barrier`，显式 `Published` 通知 scheduler 结算，保证发布先于 drain/replay 副作用；1 秒工具时长刷新与 receiver close 纳入 scheduler。同步 session/reset 与折叠 UI 原有 owner/顺序路径保留，不迁移为异步请求。Immediate 必须发布 committed-only 变化。
- `push_acp_state` 只消费已取得的 projection 元数据或低成本维护的条目数，不为 view_count 隐式物化全文；publication 时获取一次 projection 并复用。计数必须与真实 VM 条目数一致，不能把 canonical segment 数直接当作 view_count。

### S2：主 Agent 与 subagent 的中间 publication 共用 50 ms fixed deadline

**已改变此前 subagent 策略**：Streaming 下主/子流后续 chunk 使用同一个 bridge-local pending deadline。subagent Block 保留「不检测子流 Markdown 边界」的现有语义，但中间更新也受 50 ms 合帧；主 Agent Block 保留 Markdown 边界发布；None 不因 chunk 产生中间 publication。实施时同步更新现行性能设计与 code-index 的对应条款，不静默改变契约。

- 所有 chunk 仍立即按顺序接收入 canonical，不丢字、不以节流限制接收。后续 chunk 不推迟既有 deadline；没有新 chunk 时 deadline 也必须 flush。
- 首个可见 text/reasoning 块、tool/SubAgent/message 边界、交互请求、主/子终态与中断为明确的 Immediate barrier。首块身份按 stream occurrence/message 区分，不能用主 Agent 文本是否为空判断每个 subagent chunk；同 agent_id 恢复运行须区分 occurrence。
- 一次 barrier 发布最新合法快照，包含此前可发布的待定更新并结算 pending；子任务结束不能被误当成整个 session terminal。取消、主 terminal、reset、session 切换、receiver close 与 shutdown 遵守各自既有 archive/所有权规则，旧 deadline 不得重写终态或新 session；shutdown 不写 UI。
- 频率验收只约束普通 Deferred publication。固定存续的 streams、无新 barrier 时，任意长度 T 的窗口中 Deferred 数不超过 `ceil(T/50ms)+1`；Immediate 按原因单独计数。大量新 stream/barrier 可以突破总 20/s，因此不能以总发布量断言严格 20/s。
- 不选择「保留逐 chunk 直推，只优化 hash」：它仍让 atom 通知、布局扫描和 effect 随事件率无上限增长；不选择「仅删除直推」：S1 的发布状态缺陷必须同时解决。

### S3：消除 replay 失配，并复用稳定历史折叠投影

- replay 完成时通过唯一策略 `fold_for_status` 生成正确的基础终态 fold；默认 fold 不从临时 UI 覆盖反向写回。复用现有业务规则，不再复制 Completed/Error 决策表。
- 在 BridgeState 所有的派生缓存中复用**已折叠的 committed 部分**；canonical committed 与纯视觉 fold override 分开。按 committed 结构身份、phase 和 fold-overrides 内容失效；折叠函数依赖变化时同步键，禁止每个 chunk 全文 hash 历史来判断缓存是否有效。
- committed 未变、折叠输入未变时直接结构共享已折叠历史，不重新遍历/克隆/哈希历史卡片。current_turn 仍按当前状态处理；历史真实变更或覆盖变更可低频重建历史投影，追加优化可后续做，不要求第一步引入任意位置增量更新框架。
- reset、session 切换、rewind/缩短、同长度 replay 工具更新、归档、override 添加/移除均须正确失效；缓存只保留本 bridge 的必要版本，不能按 revision 无限累积旧快照。分组和 todo 顺序沿用原流水线，禁止直接缓存最终 grouped 快照后拼尾巴破坏跨边界分组。
- 不直接把整个 fold pass 回写 committed：会把 UI override、reasoning 状态/时长冻结混入基础状态，且无法自然覆盖 current_turn 缓存；不采用只有卡片 epoch 而仍全历史扫描的方案来宣称 O(1)。

### S4：减少无变化的 effect 通知，量化剩余成本

`run_auto_follow` 的哨兵值仅变化时通知；纯 effect 内部记账在不依赖重新渲染时可无通知写入，真实 scroll offset、anchor/follow 改变仍须触发渲染。覆盖流式吸底、用户上滚暂停跟随、resize、交互锚点及 session/reset。

本期保留分组差异检测、todo 定位、消息区 hash 扫描和 slot prefix index 的必要全量工作，不承诺 `push_view_models` 总成本与 N 无关。S3 的「稳定历史折叠无扫描/无正文工作」与这些 O(N) 元数据阶段分开测量；后续全局增量布局或有界历史另立契约。

## 验收与实施顺序

正式回归已落在 `peri-tui/src/kit/publication_test.rs`；下表保留验收目标，已执行范围与未验证项见末节。正式测试不得仅直接调用 `append_text + scheduler.accept`，也不得用会补发快照的 `dispatch_and_notify` 替代生产 dispatch 链路。

| 范围 | 必须证明的行为 |
| --- | --- |
| 主 Agent 发布 | 默认 Streaming 的首 text/reasoning 可见，后续 chunk 在固定 deadline 发布；提前调用 projection / push_acp_state 不吞更新；无后续事件也 flush |
| 多流与模式 | 主/子混流、多个 child、text/reasoning、Streaming/Block/None、BG 有组/无组、同 ID resume；chunk 内容完整且顺序不变，None 不因 chunk 意图泄漏中间发布 |
| 时间与 barrier | 可控时钟；首块/边界 Immediate 与 Deferred 分别计数；持续到达不延期、停止到达不挂起；barrier 后旧 deadline 不重复发布 |
| 生命周期 | committed-only 同长度替换、replay 完成、归档、主/子终态、取消、交互、receiver close/reset 竞争、session 切换、shutdown；无旧会话污染和 loading 残留 |
| 折叠重复成本 | replay 与实时两类历史，成功/失败、短/长输出、手动 override 与恢复默认；预热后稳定历史 FoldPassWrites=0、历史工具 hash 调用=0、历史折叠访问数=0；真实变化仍立即生效 |
| 缓存失效与等价 | overwrite/rewind/长度不变更新、phase 与 override 添加/移除、跨 session 相同 ID；在相同输入/受控时间点比较未分组 fold 与最终 grouped 快照的完整字段，忽略发布次数导致的 generation 差异 |
| 渲染 | 相同 generation/几何/滚动状态不因记账哨兵重复通知；真实滚动和视觉变化仍渲染，现有选择/复制/锚点契约不变 |
| 性能 | N=100/1000/现场规模，分别增加历史条数和历史正文字节；记录事件数、投影数、Deferred/Immediate 原因、fold 访问/写入/hash 字节、分组/布局扫描量及各阶段耗时；区分 debug 与 release |

时间测量不替代操作计数。「N=1000 与 N=100 耗时只差常数倍」不能证明复杂度与 N 无关；P0 验收要求稳定历史折叠工作确实为零，而剩余 O(N) 阶段如实报告。整体收益需同类负载现场复测，不能预设 CPU 降幅；原 09-18 release 事故不是本次 replay×subagent 缺陷在 release 下已复现的证据。

相关命令：`cargo test -p peri-tui --lib`、`cargo clippy -p peri-tui --all-targets -- -D warnings`、`cargo fmt --all --check`、`git diff --check`；实施改动若触及跨 crate 契约再按 testing/architecture 标准扩大范围。已执行结果见实施记录；现场与 release 性能矩阵仍待验收。

## 关联与文档路由

- [前序长会话 CPU issue](2026-09-18-p1-long-session-pins-one-cpu-core.md)：同一 push_view_models 热点；其实时生命周期基准未覆盖回放失配。
- `2026-09-04-tui-long-markdown-streaming-cpu.md`：较早的合帧建议；scheduler 存在不等于当前完整生产链路正确。
- 工作区另有 `2026-09-27-long-thread-history-window.md` 有界历史窗口提案：处理驻留和可见窗口，不能替代本 issue 的发布状态、重复折叠和 cadence 修复。
- 已同步 `docs/design/tui-streaming-markdown-performance.md` 与 `docs/code-index/peri-tui.md` 的实际发布、历史缓存和测试入口。

## 对抗验证

方案先成文，再由独立 subagent 挑战证据、状态边界、失效规则、模式/生命周期反例及验收可执行性。审查报告：[adversarial_review.md](../../docs/experiment-tui-streaming-publication/adversarial_review.md)。首轮 CONCLUSION_WEAKENED 揭示时长冻结、同步发布顺序、模式 pending 和 effect 依赖风险；实现吸收后，收尾为 CONCLUSION_STANDS（仅限机制与实现方向）。审查者没有运行最终测试；测试证据由主执行者提供。

## 状态变更记录

| 日期 | 状态 | 记录 |
| --- | --- | --- |
| 2026-09-27 初报 | Open | 登记两个进程的 CPU、采样和日志证据；提出重复折叠与 subagent 高频发布 |
| 2026-09-27 文档复核 | Open | 确认 subagent 立即发布是现行设计许可，replay 为确定失配来源 |
| 2026-09-27 诊断与方案 | Open | 加入三个确定性诊断，撤回主 Agent 合帧已生效、帧数等于 publication、运行时可直接启用测试计数器等判断；提出 S1–S4，待对抗验证与实施 |

## 实施记录与验证结果（2026-09-27）

- 独立 `unpublished` 与显式 intent 替代 dirty/generation 推断；`view_model_count` 不物化 VM。主/子后续流式 chunk 共用 fixed deadline，空 chunk 不消耗首块；同步 terminal/local-loading-reset barrier 先发布再执行后续副作用，并清 pending。
- 模式热切换按既有事件读取：Streaming pending 在模式改变时失效；None→Streaming 在下个非空 chunk 恢复，主 Block 仍遵守 Markdown boundary。无事件的纯配置改变不保证立即发布；replay 无条件 pending 保留。
- replay terminal fold 归一化；历史缓存持有源共享节点，覆盖同长度更新、替换/缩短与 override 移除。im inline 小向量分支使用值比较，覆盖表比较成本随覆盖条数变化，不宣称整个发布 O(1)。
- canonical trailing 时长在终态/归档前一次冻结，保留空 reasoning placeholder；auto-follow 纯记账无通知写入，effect 加入 loading/reset 依赖。BG_LIVE_DETAIL 仍有独立逐 chunk 明细写入，剩余 group/todo/layout 扫描仍为 O(N)。

正式测试使用真实 handler → dispatcher → scheduler：主/子 text/reasoning 各 100 chunks 在同一时刻仅首块发布，deadline 再发布完整内容；主流首块只投影一次，明确 projection 读取不会吞更新。N=100/1000、每卡 4 KiB replay 历史预热后重复发布 20 次，历史 fold 访问/写入与工具 hash 调用/字节均为 0，包含 override 并验证移除恢复、同长度更新和冷重建等价。此为确定性操作计数，不是 wall-time benchmark。

- 完整 TUI lib 测试：`CARGO_INCREMENTAL=0 cargo test -p peri-tui --lib -- --test-threads=1`，1683 passed / 0 failed / 7 ignored。先前沙箱限制本机 socket/剪贴板导致环境失败；沙箱外完整复跑通过。
- 对抗审查后的发布回归：`CARGO_INCREMENTAL=0 cargo test -p peri-tui --lib publication -- --test-threads=1`，15 passed / 0 failed（11 个新增用例和 4 个既有相关用例）。
- `cargo clippy -p peri-tui --all-targets -- -D warnings`、`cargo fmt --all --check`、`cargo test -p peri-tui --doc`（0 个 doctest）与 `git diff --check` 均通过。
- 未验证：修复二进制同类现场 CPU 复采、release 性能矩阵与真实终端交互体验。暂不关闭现场验收，不给出 CPU 降幅。
