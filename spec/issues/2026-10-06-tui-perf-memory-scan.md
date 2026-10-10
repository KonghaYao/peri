# TUI 内存专项：所有权、驻留预算与释放边界

**状态**：Open / 静态调查与 Astra 审计完成，待验证与实施（2026-10-06）。优先级是后续工作排序，不是现场事故归因。
**范围**：仅 TUI 消费端的 BridgeState、atoms、VM、Markdown/message_area、后台详情、历史预览、队列及其异步任务；本轮不修改生产代码、不完整构建。
**基线**：当前工作树，读取期间观察 HEAD 为 `f36d9fb0`；存在大量未提交修改，尤其 Markdown/message_area 已有新优化。下列行号是本轮读取定位，实施前须按符号重核，不能用 HEAD 代替 dirty 源码。

## 1. 证据口径与去重

- 最终裁决见 [Astra 审计 §4/§6](2026-10-06-tui-perf-astra-audit.md)：M1 保留 P1；M2/M3/M4 下调 P2；M5 与 runtime R6/R5 合并为 P2 风险，不另建同因实施项；M6 保留 P2 预算研究。M1 最多保留一个 memo 版本，M2 是可回查详情的保留策略，M3 复制中 Arc/im 部分共享；不确认现场泄漏。审计已复核最新 steer dirty，原扫描行号及初步建议不覆盖最终裁决。
- **源码事实**：可在下面文件/行号直接核对的字段、调用与释放动作；静态可达不代表现场已发生。
- **推断**：由所有权得出的保留/峰值/增长风险；不提供未经计量的倍数、泄漏量或 CPU/RSS 收益。
- **实测**：本轮未做进程、堆、allocator 或 RSS 测量，也未运行性能负载或 Rust 测试。只完成只读源码核对与文档轻量检查。
- 主 agent 的现场记录为 `/tmp/peri-tui-perf-20261006-2nsZNM/observations.txt`。主 agent 补采三个本 repo `target/debug/peri` 的 RSS（原始 KiB 见汇总），未采堆，且二进制早于 dirty 修改。本 issue 不重新汇总、不将该 RSS 与当前源码绑定；现场版本、口径与事故归因由主 agent 负责。
- 正常 transcript/草稿/运行中任务驻留不是泄漏；可达但过期的缓存是保留策略问题，也不据此确认内存泄漏。没有匹配版本的堆归属和释放曲线，不能称 TUI 为事故内存主因。
- [进程 P0](2026-10-06-p0-dev-peri-high-cpu-memory.md) 的 `WorkState` 全量解码、provider 请求、序列化与事务副本属于 backend，明确排除；同进程 RSS 不等于 TUI 分配。
- [流式冗余 issue](2026-10-06-tui-streaming-render-redundancy.md) 是 F1–F10 的唯一实施进度源。本 issue 不重开已修的逐 chunk bubble clone、主快照 assistant 深拷贝、terminal 原文缓冲、高亮命中 clone、详情每帧重新建 parser 等问题。该 issue F4 的历史缓存预算余项仍归原 issue，本文件只提供所有权与生命周期补充。

## 2. 所有权地图（源码事实）

路径约定：`kit/…`、`acp_client/…` 相对 `peri-tui/src/`；`src/acp_client/…` 相对 `peri-tui/`；带 crate 名的路径相对仓库根。区间用于静态核对，不能视为现场调用栈。

| 层/持有者 | 内容与共享方式 | 边界及释放条件 |
| --- | --- | --- |
| ACP client → notifier → bridge | `acp_client/client.rs:160` notification unbounded channel；`kit/entry.rs:368` bridge unbounded channel；`kit/acp_notifier.rs:79-85` 消费并转发 | 消费/drop receiver 才释放队列 payload；会话过滤不是队列清空，见 M5 |
| BridgeState | `kit/acp_events/mod.rs:120-180`：committed `im::Vector`、CurrentTurn、Todo 原始 JSON 等；任务局部拥有 | `kit/acp_bridge.rs:405-422` 重置 committed/current_turn/folded_history 等；shutdown/receiver close 退出任务（`:541-543`、`:575-583`） |
| CurrentTurn | `kit/acp_types/current_turn.rs:30-38,109-123`：独占正文/推理、工具 accumulator、共享 VM 缓存；子 turn 同构 | `reset` 在 `:298-299` 用新实例替换，释放原 capacity；`mark_committed` 的 `:247-252` 使用 clear，保留容器容量，不等于泄漏 |
| FoldedHistory / VIEW_MODELS | `kit/acp_events/fold.rs:7-11,33-39` 缓存 source/folded；`kit/acp_events/render.rs:47-61,148-153` 发布一份当前快照 | im 节点可共享，不是向量个数乘正文；reset 清此局部缓存及 atom，但另有 static 分组缓存（M1） |
| VM payload | `kit/tui_render_unit/unit.rs:12-20`：assistant 为 Arc，user/tool 等为 owned；`bubble.rs:62-80` 显式克隆 bubble 会复制 String | assistant 的 Arc clone 不深拷贝；im COW/工具合并可能复制 owned user/tool，不能宣称全部 VM clone 都零字节 |
| 主消息区渲染 | `kit/message_area/mod.rs:158-161,285-289,343-363` 每个顶层 VM 一 slot，持 Markdown/Line/wrap 缓存；`:382-393` 构造 SlotIndex 共享 Arc | 快照长度缩小时 resize 丢弃尾部 slot；索引不变则按 hash/宽度/主题/语言重建，非按 session ID 全清；容器 capacity 可保留；卸载释放组件状态 |
| 后台全局 atoms | `kit/atoms.rs:659,684-704,751-762`：任务列表、显示列表、身份表、详情表，详情含 stream/tool_cards/nested_units/结果 | 活跃列表、可见行与详情不是同一个生命周期；完成不删除详情；不同 session 边界 clear（M2） |
| 详情组件 | SubAgentDetail 持久 slots；ShellDetail 读取整个详情 map clone | 面板关闭停止渲染并移除组件分支，不能因此推定全局后台 atoms 被清；M3 |
| 历史预览 | 一份 async result String + 全文 owned Lines/Paragraph；独立于 VIEW_MODELS | 成功替换/退出预览/组件卸载释放；依赖更新不是 detached Tokio 请求，M4 |
| 新输入恢复 atom | `kit/steer_state.rs:35-55`：按 session 保存 snapshot/pending/recovered/delivered；含用户原稿/附件 | 与 transcript 的 session reset 不同，刻意保留待恢复输入；见 M6，禁止简单清空 |

### Markdown 当前实现的纠正（不新立重复修复）

- **源码事实**：`kit/markdown/mod.rs:339-357` 生产缓存仅含 chunk_source、稳定 ParsedBlock、稳定 rendered chunks；旧 stable_text/stable_state 受 `cfg(test)` 限制，不计入生产驻留。
- **源码事实**：流式 `:190-191,216-227` 保留稳定前缀原文、parsed blocks、Arc rendered chunks；返回 stable Vec 只克隆 Arc。这些表示不是同一 allocator 对象，但也不能逐层按整篇文本乘固定倍数。
- **源码事实**：`kit/message_area/render.rs:220-262` 稳定 Line 交由 MarkdownLineCache 保存，slot.lines 只为稳定区留空占位；`vm_cache.rs:115-125,128-135` owned Lines 移入 Arc，overlay 再共享 Arc。不能沿用旧“stable Lines 在 slot 中再深拷贝一份”的结论。稳定 rendered segments 与带前缀的 Line cache 仍是不同表示，wrap map/占位行也有结构成本。
- **源码事实**：正文非空且 `started_at == None` 时调用 terminal（`message_area/render.rs:198-209`）；`markdown/mod.rs:239-245` 丢弃 chunk_source capacity、清 stable blocks/chunks。空正文或换成其他 VM 变体的 early return 不等于执行 terminal；slot 按索引复用，需在原 F4 生命周期验证中覆盖“流式 → 空正文/其他变体 → resize/reset”。这是验证缺口，不是已经实测的残留量。
- **源码事实**：高亮 static cache 当前为 32 条和 4 MiB 的估算 payload 双预算（`markdown/code_block.rs:35-88,100-114`），命中返回 Arc（`:119-131`）。预算不含全部 allocator overhead，引用离开 cache 后仍可能由渲染对象持有。跨会话有界复用不是无界增长。
- **推断**：长会话在主消息区为所有顶层 VM 保留 Line/wrap 缓存，空间随已渲染会话规模增长；实际绘制视口并不限制缓存数。其有界窗口/LRU 方案及滚动/选择/复制验收仍归原 F4，避免重复 issue。

## 3. 新增发现与可验证假设

### M1 — 全局工具分组 memo 持有 reset 前整段快照（审计 P1）

- **源码事实 / 调用链**：`dispatch → push_view_models → group_successful_tools`；`kit/acp_events/render.rs:247-255,287` 的 static `TOOL_GROUP_CACHE` 持 `input` 和 `grouped` 两份 im root，`:669-674` 替换缓存。工具折叠组的 `:683-692` 将 owned 工具卡 clone 到 hidden_vms，assistant Arc 不深拷贝。
- **源码事实 / 释放边界**：`project_session_boundary`（`kit/session_boundary.rs:15-20`）及 bridge reset（`acp_bridge.rs:405-407,435`）清 atom/局部状态，但 `push_view_models_for_reset`（`render.rs:728-745`）没有清 static cache。空 segment 会在 `:557-565` 直接返回，不替换旧 cache。旧 root 通常要等后续非空分组重建覆盖，或进程退出才消失；单条 Todo memo（`:165`）也未在 reset 清空，但不累计多个值。
- **推断 / 触发规模**：A 会话积累 N 个 VM/工具输出，切到没有新非空 publication 的空会话 B，UI 已空但 static 仍可达 A 的 payload；最多保留最后一次 cache，不是每次切换叠加 N 个会话的历史。可能保留整段而非一个小缓存，但不提供 RSS 归因。
- **验证方案**：持 assistant 的 Weak sentinel，发布 A、丢掉测试局部 root、reset 到 B 并让 bridge 收尾，断言 VIEW_MODELS/committed 已空且 sentinel 仍被 cache 持有；再发布 B 的非空不同内容检查覆盖释放。补空 publication、相同 hash、连续 reset 和 stale event 案例；工具对象配独立 drop/估算字节观测，不只检查 assistant。
- **修复与验收建议**：让 memo 归 session/BridgeState 所有，或在同一 reset 权威边界显式失效，删除旧 static owner；不用新平行规则/兼容 cache。验收 reset 后旧 cache root 为零、旧 Weak 不可升级（其他正常 owner 已 drop），新会话空发布不复活旧数据，原分组/焦点/复制行为保持。预算计量不得把共享对象重复加总。

### M2 — 后台详情按任务累计，3 秒仅隐藏显示行（审计 P2：预算/语义待定）

- **源码事实 / 调用链**：BgTaskStarted → `system.rs:796-797` → identity/seed → `bg_task_live.rs:132-141` map.entry；后台 text 通过 `acp_events/streaming.rs:42-68` 进入 live stream，tool 通过 `acp_events/tool.rs:18-29` 进入详情。
- **源码事实 / 多份表示**：`bg_task_live.rs:94-129` 独占 stream 追加；`:51-71` 每次 publication 将 bubble 深拷贝到一个 Arc 投影。这是发布边界复制，不是已修掉的每 chunk clone。正文可能同时在 child CurrentTurn/归档组、live stream、live nested_units 中存在，具体取决于是否仍 routed_to_subagent，不统一声称三份。工具完成 `:306-309` 留在 accumulator，并由 `acp_types/tool_card.rs:31-35` 复制摘要到卡片；raw_input 也仍在 accumulator。
- **源码事实 / 边界**：完成/取消只从 BG_TASKS 移除（`acp_events/system.rs:832-844,876`），`bg_task_live.rs:327-377` 保存终态、output_preview/结果；`with_live_detail` 会 flush 后释放 stream，但留下 nested/tool/结果。`bg_task_click.rs:46-57` 的 3 秒规则是过滤，不 remove BG_DISPLAY；身份/详情表没有任务数、字节或 TTL 淘汰。全量 snapshot `system.rs:682-695` upsert，不 prune 已缺失详情。
- **释放条件**：不同 active session 时 `session_boundary.rs:64-76` clear 全部后台表；同 session replay 刻意保留。HashMap/Vec clear 释放条目 payload，但保留表桶/容量；关详情面板不释放详情 atoms。
- **推断 / 触发规模**：单一长会话完成 K 个不同 task_id，终态详情空间随累计正文、工具参数/输出、结果增长，不是随“当前活跃任务数”增长；大输出、长思考与多次运行放大。仅终态列表/详情设计缺预算，不确认泄漏；完成后可查详情是现有功能，不能一完成就删除。
- **验证、修复与验收建议**：合成 100/1000 个任务、分别携带 1/64/1024 KiB 文本（测试设计规模，非实测），比较 active=0、可见=0 时 atom 数/估算独占字节。补 snapshot 缺项、同会话 replay、跨会话 clear；定义终态详情数量+字节预算，保留运行中/当前选中项，淘汰内容可经 ACP 按身份重读，不能直读 backend。验收非选中终态有界、运行中详情不丢、选中被淘汰有明确状态，切会话旧 payload owner 消失。

### M3 — 查看一个 shell 详情/应用一个 task snapshot 时 clone 整个 live map（审计 P2）

- **源码事实 / 调用链**：选择 shell → PanelOverlay render（`kit/panel_overlay.rs:53-59`）→ ShellDetailPanel（`kit/panels/shell_detail.rs:29-40`）clone tasks/display/live 三份表，然后 `:57-58` 只展示所选任务；BgTaskSnapshot → `apply_bg_task_snapshot`（`kit/acp_events/system.rs:649-663`）clone 整个 BG_LIVE_DETAIL，只读取 status。
- **源码事实 / 深拷贝边界**：BgLiveDetail derive Clone（`kit/atoms.rs:683-704`）会复制 output_preview、subagent_result、tool_cards 的 JSON/String、BgStream 的 owned bubble；nested_units 的 im root/assistant Arc 共享。即使所选 shell 很小，另一个运行中 agent 的 stream、旧终态任务 raw_input 也被复制。
- **推断 / 触发规模与释放**：触发峰值随全表 owned payload 总量 B 增长，而不是所选任务长度；clone 局部变量在一次 render/snapshot 调用后释放，不是长期泄漏。M2 的长会话累计令这种临时峰值更大；不推定帧频与现场收益。
- **验证方案**：构造“小 shell + 大且无关的 agent detail + 大终态工具参数”，打开 shell/反复应用 task snapshot，按分配/复制字节和 AssistantCloneBytes 观测；后者只覆盖 bubble，不代替 JSON/整个堆的计量。
- **修复与验收建议**：在 guard 生命周期内生成所选 shell 的最小展示值；snapshot 只提取 task_id/status，不 clone 整个详情。保持无 await 跨 guard。验收选择小任务时复制量不随无关大任务字节增长，snapshot status 投影不复制正文/JSON；原 snapshot revision、终态与未观测状态语义不变。
- **去重**：SubAgentDetail 已使用持久 DetailRenderCache（`panels/subagent_detail_cache.rs:12-25,37-67`），不重报 F10。该面板 `:67` 每帧 clone 缓存 Lines 组装 Paragraph，是输出物化；只对当前选中组生效，cache 在换 occurrence/宽度/主题/语言时清 slots。其规模/帧物化可并入原 F10 验证，不能误说它也每帧 clone 整个 live map。

### M4 — 历史预览全量 RPC → DTO → String → 两份 Lines（审计 P2：大峰值待测）

- **源码事实 / 可达链**：History 中 `v`（`kit/panels/thread_browser.rs:356-357`）→ use_async_state（`:48-62`）→ `AcpTuiClient::read_session_history`（`src/acp_client/client/workspace.rs:154-178`）→ `peri/session_history`。TUI 请求没有 page/limit；先持整份 response Value，再逐 payload `.to_string()` 反序列化到 Vec。Value 与逐渐构建的 DTO 在同一次调用内共存；序列化临时串通常逐条释放，不是全部临时串永久保留。
- **源码事实 / 多份驻留**：`thread_browser/history_preview.rs:5-38` 将全部正文/工具请求及 pretty JSON 拼成一个 String，格式化阶段 Vec payloads 与输出共存；不保留跨 session 预览 map。每 render `thread_browser.rs:513-519` 为每行 owned String，再 `lines.clone()` 给 Paragraph，局部 Line 内容出现第二份。显示高度 clamp（`:525-527`）只限制高度，不限制文本分配。
- **释放与并发边界**：`v` 退出预览置 preview_id=None（`:193-196`），async 成功回写 None 后旧数据替换；新预览 loading/Err 期间可能保留前一次成功的 String。PanelOverlay 去掉分支后组件状态应释放，须用卸载测试确认。
- **依赖源码事实**：Cargo.lock:4503-4506 锁定 ratatui-kit 0.10.3（manifest 的 0.10.2 是版本要求）。本机 registry `~/.cargo/registry/src/rsproxy.cn-e3de039b2554c837/ratatui-kit-0.10.3/src/hooks/use_async_state.rs:45-59` 不在请求开始/Err 时清旧 data；`src/hooks/use_effect.rs:37-39,83-87` 的依赖变化直接替换 hook 所有的 boxed future。此路径不是 `tokio::spawn` 的 detached preview worker，不能声称快速预览留下无限后台任务。
- **推断 / 触发规模**：总历史正文/参数 H 大时，加载有 response/DTO/String 的峰值，展示有全文 String/Lines 的驻留与逐帧物化；切新 ID 可能同时持旧 String 与新加载 payload。未量化相加系数，也未检查服务端“取消客户端 future 是否终止 history RPC”，不能据客户端 drop 推定 backend IO 立刻停止。
- **验证、修复与验收建议**：先用合成 1/16/64 MiB 历史测 response 解码、格式化、稳定展示、换 ID、RPC 失败、v 退出和卸载的 owner/drop 曲线。立即可移动 Lines 而非 clone；大历史的分页/分块和字节预算须经 ACP 契约设计，保持 Unicode、完整工具参数可查、独立滚动与只读语义。验收稳定预览不按每帧复制两份全文，限制加载/展示峰值，取消/失败不保留无用旧预览 owner；不能静默截断历史。

### M5 — unbounded 输入/事件队列及 detached 请求无统一准入预算（合并 R6/R5，审计 P2）

- **源码事实 / 事件链**：ACP transport incoming → client pump → notification channel（`src/acp_client/client.rs:160`）→ notifier（`kit/acp_notifier.rs:79-85`）→ bridge channel（`kit/entry.rs:368`）→ bridge recv（`kit/acp_bridge.rs:575-640`）。notification/bridge 两层都是 unbounded，流式 scheduler 限 publication 频率，不限生产/排队速度。旧会话过滤在消费/dispatch 前执行，不 purge 已排队 payload。
- **边界排除**：transport 的 `peri-acp/src/transport/mpsc.rs:55-79` incoming 队列同样 unbounded，但归共享 ACP transport，本 issue 只协调端到端预算，不把它或 backend 状态全部计为 TUI。新会话转移缓冲已经有 64 条上限（TUI `interaction_lifecycle.rs:18,397-414`），不是 unbounded；条数预算不等于字节预算。
- **源码事实 / 请求链**：`kit/entry.rs:341-370` 建 submit/steer/rewind/ask/hitl/load/cancel/local unbounded channels。submit consumer `kit/submit_consumer.rs:64-78` 每请求另 spawn，不等待/保存 handle，没有本地统一 in-flight 上限；payload 可在 session gate/transport await 中持有。不能沿用“单消费者顺序等待每条 prompt 完成”的旧注释来推定有界并发。新输入队列能力开启时普通正文走 STEERS（`:152-156`），旧 prompt 分支仅在相应条件可达。
- **源码事实 / detached snapshot**：revision gap `kit/acp_events/system.rs:764-766` → `request_bg_task_snapshot`（`:725-753`）每次 spawn，未见 single-flight/cancel token；请求后检查 session ID，再丢陈旧结果。raw request（`src/acp_client/client/requests.rs:22-23`）直等 transport response，无此调用级 timeout；共享 transport `peri-acp/src/transport/router.rs:58-75` 在 pending future drop 时清 registration，不能误报 pending map 已无条件泄漏。
- **释放边界**：notifier/bridge 有 shutdown token，退出 drop receiver/BridgeState；submit 外循环也有 token（`:52-56`），但 spawned request 不继承该 token。request 需自己完成、报错、transport close 或 runtime 结束才收尾。local mini bridge（`entry.rs:374-377`）无 token且忽略 send 失败，LOCAL_EVENT_TX 为 OnceLock（`atoms.rs:612`），一般持到进程/runtime 退出；仅一常驻任务，不凭此声称重复创建或大 payload 泄漏。
- **推断 / 触发规模**：当生产速率 λ 大于消费速率 μ，排队 payload 近似随 `(λ−μ) × 持续时间 × 平均字节` 增长；这是压力模型，不是实测。revision 缺口持续而 RPC 慢/不返回时，可累计 snapshot 请求。低流量正常运行可能完全不触发。
- **验证、修复与验收建议**：可控慢消费者/慢 transport 下发固定字节事件及连续 revision gap，统计队列深度、估算字节、in-flight、session stale 数和 shutdown 后任务数。事件队列不能直接改 bounded 后 await 而制造 ACP reverse/request 循环阻塞；设计背压/可合并流式事件策略，terminal、工具与 interaction 保序且不可丢。snapshot 用 session/epoch single-flight，完成/失败清 owner；请求 task 加明确 owner/取消及超时策略，不能破坏挂起 turn 后新 prompt 可注入的契约。验收压力下有明确预算/拒绝信号，无静默丢用户数据，无死锁，shutdown 在约定 deadline 内收尾。

### M6 — STEERS 保留跨会话恢复数据，属于显式所有权而非 reset 缺失（建议 P2：预算设计）

- **源码事实 / 可达链**：输入开启队列能力 → `kit/input_area/submit.rs:45-54` → `steer_state::enqueue`（`kit/steer_state.rs:445-466`），构造含 original_draft/附件 content 的 UserInput，command clone 到全局 STEERS.pending，再 clone 到发送通道。`steer_state.rs:35-55,58-68` 以 session map 保存 snapshot/pending/recovered/delivered；reset_session 仅清目标 snapshot/direct_submissions，保留待确认命令和恢复原稿，非清整个 sessions map。
- **边界与释放**：`kit/session_boundary.rs:16-19` 调 session_boundary；旧 session 的 snapshot/恢复稿不会因切到新会话自动全删。确认/reject/recover 会移除相应 pending/recovered（`steer_state.rs:183,213,237,268-284`）；无全局 session 数/用户 payload 字节上限。consumer 自有 retries，并在执行 await 时也响应 shutdown（`kit/steer_consumer.rs:59-76`），不同于 M5 的 detached submit 请求。
- **纠正**：旧 INPUT_BUFFER 路径有 32 条上限（`kit/input_area/submit.rs:89-95`），不是无界 VecDeque；附件 base64 每项无此处字节预算（`kit/atoms.rs:253-255`），32 条也可大。terminal drain 会将 payload 移入 SUBMIT_TX（`kit/acp_events/render.rs:791-809`），clear/切会话释放输入项，不保证已经转移到 channel/task 的旧 payload 同时消失。
- **推断 / 规模**：多 session 有待确认/撤回/失败恢复原稿、大图片附件，可能长期持用户数据；无待恢复数据的 session 也可能留小 metadata。明确恢复语义是保留理由，不能仅凭 session map 未清确认 bug 或泄漏；需区分应保留草稿与无用确认快照。
- **验证、修复与验收建议**：跨 100 个 session 制造成功确认、撤回、断连失败和大附件，追踪 pending/recovered/snapshot 独占字节及 recover 后释放。先自动删除空/无恢复义务的 session metadata，设 pending payload 的可解释准入字节预算；恢复稿的 TTL/持久保存须另行批准，不直接 clear 用户未发送原稿、不转移到 TUI 文件系统。验收切换不串稿、不丢稿、恢复后 payload owner 消失，正常已确认空会话不累计大 snapshot。

## 4. 验证与实施边界

本轮未穷举全部 atom/所有面板 detached tasks；图片预览/解码缓存、插件/登录/主题下载 worker、第三方组件树卸载实现、backend RPC 取消传播均未完成专项审计。已核对的所有权不等于覆盖这些范围；Astra 可优先对 M1–M6 调用链及释放推断作对抗复核，而非扩展成全库扫描。

1. **先量 owner，再量进程**：分别记录 committed/CurrentTurn 字节、static cache roots、render slot/line/wrap 数、后台 detail 数与 owned 字节、preview payload 字节、队列/in-flight。估算器区分 len/capacity、JSON/Line overhead、Arc/im alias，禁止把同一共享对象重复计量。日志只写数量与身份的安全摘要，不写正文、工具参数、图片或密钥。
2. **可复现时序**：空会话基线 → 载入大历史 → 流式结束 → 大后台任务完成并等可见行消失 → 查看小 shell → 预览另一历史 → 关闭所有面板 → A/B 空会话切换 → 清空/退出。记录前后 owner/drop；同会话 reload、空 publication、失败和中断单列，不以一次 RSS 下降代替生命周期证明。
3. **测试建议（未执行）**：实施时在现有 acp_events/render、session_boundary、bg_task_live、shell_detail、thread_browser、steer/consumer 对应测试模块补行为回归。使用 fixture/可控 transport，不调用真实 provider，不要求完整构建；按需运行 `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- <新增定向过滤>`。共享 atom/static 用局部 RAII 恢复或 serial，避免并发测试污染。
4. **堆/RSS 验收另案协同**：主 agent 固定二进制来源/时间/配置/负载，再采同版本 heap、RSS/footprint 与退出前后曲线。allocator 保留、表桶容量、MCP/宿主 backend、线程栈及 mmap 与 TUI live bytes 分开。只有生命周期违约且堆证据支持，才升级为泄漏或现场主因。
5. **修复职责**：M1/M2/M3/M6 在 TUI 所有权边界设计；M4 的分页与 M5 的 transport 背压必要时经 ACP 契约协作，不能 TUI 直驱 Agent/Store，不能为了节省内存静默丢历史/工具/终止事件。原 F4/F10 余项继续使用原 issue 作为单一实施事实源。
6. **交付范围**：本轮只新增本文件，保留所有原有 dirty 修改；不改 standards/CLAUDE/code-index，因为没有实现、稳定规则或入口变化。实施完成后再按 DOC-UPDATE-001 同步受影响事实源。文档检查不等于上述性能/行为验收通过。

## 5. 待办与关闭条件

- [ ] M1 reset/static memo 释放回归闭环（首选：确定性所有权问题）。
- [ ] M2 后台终态保留语义/双预算获批并验证；与 M3 整表 clone 峰值回归一起计量。
- [ ] M4 大历史预览物化、替换、失败与卸载验证；分页/预算契约明确。
- [ ] M5 压力、revision gap single-flight、detached request 的准入与退出验收。
- [ ] M6 恢复草稿预算及已确认空会话释放验证；不破坏输入恢复。
- [ ] 与原流式 issue 核对缓存余项，完成匹配版本的独占堆/进程观测或明确记录未归因。

关闭要求：各项要么实施并通过行为/所有权验收，要么以验证证据明确正常驻留并记录预算/保留契约；本轮静态发现不能用“RSS 看着正常”整体关闭，也不能用现场大 RSS 提前判定泄漏。
