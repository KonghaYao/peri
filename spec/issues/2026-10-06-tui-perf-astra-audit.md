# TUI CPU / 内存 / Runtime 扫描：Astra 最终独立审计

**状态**：Open；三路静态审计完成，修复、行为与性能验收未完成，不标 Closed。
**日期**：2026-10-06（Asia/Shanghai）。**范围**：只读生产代码、锁定依赖源码和已有 sample/ps；仅新增本文件，不构建、不运行 Rust 测试、不改扫描报告、不提交。

## 1. 审计输入、基线与裁决口径

已完整审阅 [CPU 扫描](2026-10-06-tui-perf-cpu-render-scan.md) CPU-1/CPU-2、[内存扫描](2026-10-06-tui-perf-memory-scan.md) M1–M6、[Runtime 扫描](2026-10-06-tui-perf-runtime-scan.md) R1–R7，以及 [主 agent 汇总](2026-10-06-tui-perf-hotspot-audit.md)。三份到齐后才形成此裁决。先读 standards index、tui、rust、architecture-contracts、testing、documentation、peri-tui/CLAUDE 与 code-index/peri-tui；跨 ACP 追踪补读其模块指引，Git 检查前读 git 标准。

这是**非冻结工作树**：主 agent 初读 HEAD `bd5e296b`，内存扫描记录 `f36d9fb0`，本审计后段实读 HEAD `b7c6770d2c0a85a4e7bd5ca3fd24e28e630a6ee8`；并行工作已经把 Markdown/message_area 优化提交，另有 `steer_state.rs` 未提交修改。所有这些提交和源码修改均非本审计执行。按符号重读当前文件，以下行号是审计时定位，不把任何一个 HEAD 或扫描报告的旧行号当成冻结快照。

裁决分为：**确认**（当前可达机制）、**条件成立**（需指定负载/生命周期）、**测量假设**（现场占比、收益或增长量未证实）、**驳回**（被当前代码反证的表述）。一条发现可确认机制而驳回泄漏/事故归因。P1/P2 是后续工作优先级；旧 issue 的用户指定 P0 不传递给新增发现。

依赖定位：`Cargo.lock:4503` 锁定 ratatui-kit **0.10.3**；下文 `KIT/` 指本机 `/Users/konghayao/.cargo/registry/src/rsproxy.cn-e3de039b2554c837/ratatui-kit-0.10.3/`，不是相邻开发 checkout。未执行依赖构建或网络查询。

## 2. 现场证据复核

只读 `/tmp/peri-tui-perf-20261006-2nsZNM/` 原始材料，未新采样、重启或干预进程。

| PID | ps RSS 前 → 后（KiB） | ps %CPU 前 → 后 | sample footprint / peak | 主线程等待包含计数 |
| --- | --- | --- | --- | --- |
| 9500 | 1,712,656 → 1,856,752 | 103.8 → 91.3 | 1.1G / 1.7G | 1,241 / 1,288 |
| 55171 | 560,672 → 560,688 | 13.5 → 2.3 | 318.3M / 475.7M | 1,772 / 1,790 |
| 49449 | 2,757,952 → 2,757,952 | 204.9 → 18.2 | 1.9G / 4.4G | 1,320 / 1,371 |

- 窗口为 20:30:09–20:30:12。`sample-9500.txt:969` 的 `commit_response` 经 `WorkMutationBarrier::commit`、`apply_work_mutation`、`write_work` 到 `:980` 的 `encode<WorkState>`，后者包含计数 1,255。`sample-49449.txt:948` / `:2297` 的 WorkState decode 包含计数分别为 393 / 391。确认 backend 编解码路径被采到；不能加总为 CPU 占比、调用次数或推定由隐藏 History 触发。
- 三个主线程上述等待均为 `__psynch_cvwait` 分支（各 sample 的 `:54`）。存在 Tree/render/wrap 和 service tick 样本，只能证明短窗可达，不能证明是无变化帧、当前源码热点或 CPU 主因。UI 与同进程 backend 必须分开计量。
- **版本纠正**：不是三个不同二进制。9500 与 55171 的 Binary Images UUID 相同：`96EB24B3-4A95-36DF-AD64-6A9188A5C3CD`（各文件 `:1665` / `:2032`）；49449 为 `299FD154-2C6E-329F-8C3A-7CA3D621448A`（`:3689`）。这是两个已加载映像身份，均不能映射为当前 HEAD/dirty；启动时间、路径和磁盘 mtime 不能替代构建身份。
- ps 的 TIME 字段确有累计 CPU 时间，窗口端点差分别为 1.96s、0.40s、2.17s；只记录原始差分，秒级时间戳和采样扰动不足以给出精确利用率。表中 `%CPU` 仍是 ps 自己的口径，不是该差分。RSS、physical footprint、peak 不直接相减；三秒 RSS 变化不证明泄漏或缓存归属。
- **材料完整性纠正**：`SHA256SUMS` 含自身的空文件 hash `e3b0…b855`，这是自包含清单生成问题，不能宣称整个清单校验通过。observations、三个 sample 及三个 status 文件可独立核验；自条目错误不等于 sample 内容被篡改。status 文本记录 Sampling completed，不包含可独立重建的 shell exit code。
- 无 heap/allocator 归属、负载规模、会话关联与同构建受控复测。事故资源归因继续由 [进程 P0](2026-10-06-p0-dev-peri-high-cpu-memory.md) 承接；本审计不确认 TUI 泄漏，也不提供优化百分比。

## 3. CPU 两项逐条裁决

### CPU-1：确认全量 reasoning 折行；P2 保留

**证据链**：`peri-tui/src/kit/message_area/mod.rs:298` 的 slot 门控 → `:343` `vm_to_lines_cached_with_layout` → `message_area/render.rs:191` → `render/reasoning.rs:36` `render_reasoning_block`。该函数在 `:53` 先调用 `reasoning_visual_lines`（`:29`），经 `peri-tui/src/truncate.rs:106` `wrap_by_width` / `:113` grapheme 遍历物化所有行，之后才在 `reasoning.rs:78` 选择 0/4/100 行，completed 摘要在 `:99` 使用总行数。

**判定理由**：running Collapsed 确实付出无用全扫；Preview/Expanded 保留有界输出但计算/暂态内存随输入 R 增长。`tui_render_unit/unit.rs:56` 的 reasoning period=10 表示 100ms 桶中的秒级失效，**不是每 100ms 重算，也不是时钟自动发起 render**。completed reasoning 随同 bubble 正文变化重新计算也可达。暂态 Vec 随调用释放，不是泄漏；成本约为每次 Θ(R+视觉行数)，累计取 ΣR_i，现场命中仍是测量假设。

**修复/验收**：先短路 running Collapsed；按 reasoning 内容/宽度维护计数与有界首尾行，避免不变 reasoning 被正文或计时反复物化。completed 的准确视觉行摘要不可删除。覆盖 CJK/emoji/组合字符、跨 chunk grapheme、空白过滤、resize、改写及 4/100 行首尾语义；用访问/物化计数证明未变内容不全扫，不能借 Markdown 的 TailParsedBytes 证明 reasoning 成本。权威实施项留 CPU-1；秒级 chrome/content 分离与旧 F5 协调，不重复计算收益。

### CPU-2：确认稳定布局仍有 S 级搬运；P2 保留

**证据链**：`message_area/render.rs:222` 构造稳定 chunk identity，`:256` `retain_and_wrap` 命中，`:257` 仍按稳定逻辑行数填 `Line::default()`；`message_area/mod.rs:351` → `vm_cache.rs:138` `build_slot_wrap_map`，`:158` clone 每条稳定 wrap entry 并改偏移。`selection.rs:35` 表明 wrap map 是每逻辑行一项。每帧另在 `mod.rs:376`、`vm_cache.rs:128`、`selection.rs:74` / `:175` 重组 Arc parts 和 prefix。

**判定理由**：稳定逻辑行 S、chunk K、尾部 T 时，slot rebuild 仍有 Θ(S+K) 元数据成本；所有 slots 的每帧索引为 O(N+ΣK)。这是默认 Line 初始化和 wrap 元数据复制，**不是稳定正文 String 深拷贝、全历史重解析或永久泄漏**。`WrapRecalculatedLines` 只计重新折行，不覆盖这些搬运。

**修复/验收**：以共享稳定 chunk + 尾部 + 基址表示 slot 行与 wrap；固定短尾、递增 S 时，稳定 placeholder 初始化/稳定 entry 复制应为 0，单独记录 chunk 索引成本。总成本不能因此宣称 O(T)：旧 F3 前缀一致性扫描仍在。跨 chunk 选择/语义复制、滚动高度、图片表格、空 slot、resize 与主题失效须保持行为。S 级操作由 CPU-2 主责；每帧 O(N+ΣK) 归旧 F5，内存表示与旧 F4 共用验收。

## 4. 内存六项逐条裁决

### M1：确认 reset 后过期 static root；保留 P1

`peri-tui/src/kit/acp_events/render.rs:123` 对**完整快照**调用 `group_successful_tools(..., 0)`；`ToolGroupCache` 在 `:247` 持 input/grouped 两个 im root，static 位于 `:287`，`:669` 替换。`GroupCut` 只是索引元数据，512 个 cut 的上限不限制 root 正文大小。`session_boundary.rs:20` → `render.rs:728` `push_view_models_for_reset` 只清展示 atom/覆盖/焦点；`acp_bridge.rs:405` 清局部 committed/current/folded，但都不清 static。空 publication 在 `render.rs:557` 提前返回，不替换旧 root；唯一 reset helper `:291` 仅编入测试。

因此 A 大会话 → 空 B → 无非空新发布时，旧 A payload 仍可达；随后不同非空快照重建覆盖才释放该 memo 的引用。最多保留一个 memo 版本，不是每次切换累积整会话，也不是两个 root 就两份正文。assistant Arc 共享，`build_collapsed_group`（`:679` / `:692`）的 owned tool clone 要单独计量。P1 理由是明确跨 session 释放边界缺口和可能整段保留，不是已测 GiB 泄漏。

**验收**：memo 收归 bridge/session owner 或在唯一 reset 边界失效。以 Weak/drop 证明所有正常 owner 已释放后，reset + 空 publication 不再持旧正文；连续 reset、同 hash、新旧事件竞争、非空覆盖、分组/焦点行为全覆盖。TODO_SUMMARY_CACHE（`:165`）为单值附带清理，不再开一个“累积泄漏”。实施权威：M1。

### M2：确认累计终态详情；预算问题条件成立，P1 下调 P2

`bg_task_live.rs:132` `with_live_detail` 建/取 entry，`:139` flush 后 `:140` 清 stream；终态函数 `:327` / `:341` / `:360` 保留 nested_units、tool_cards、结果/预览。`acp_events/system.rs:832` / `:876` 移除活跃行但详情仍在；`bg_task_click.rs:46` `visible_bg_display_entries` 只是过滤三秒显示，非删除。snapshot 在 `system.rs:682` upsert，不 prune live map 缺项；不同 session 才在 `session_boundary.rs:64` 清后台表，同 session replay 保留。

空间随同一会话累计不同 task 的内容增长，与当前可见/活跃数无必然关系；stream 在终态已释放，不能报告其永远和 terminal bubble 共存。发布时 `bg_task_live.rs:68` 的 owned bubble clone 是 F1/F2 已知物化，非逐 chunk 回归。完成后可查详情本来就是功能，因此没有保留预算契约与实测前不认定泄漏或 P1 事故。

**验收**：计数 active=0、visible=0 下终态详情独占字节与任务数；明确数量/字节双预算、选中/运行中 pin 和淘汰后的可读语义，再实施。当前未证明存在可恢复全部详情的 ACP 接口，不能把“可经 ACP 重读”当现状；必要接口另评估，禁止静默丢内容。跨 session 释放 payload、同 session replay 与历史详情完整性均须验证。实施权威：M2。

### M3：确认整 live map 深拷贝；P1 下调 P2，建议靠前修

`panels/shell_detail.rs:24` `ShellDetailPanel` 在 `:31` / `:35` / `:39` clone tasks/display/live，然后 `:58` 才按选择构造内容。`acp_events/system.rs:642` `apply_bg_task_snapshot` 在 `:652` clone 整个 live map，只在 `:661` 读取 status。`atoms.rs:683` 的 `BgLiveDetail: Clone` 包含 owned JSON/String/tool_cards，`bg_task_live.rs:12` 的 `BgStream: Clone` 复制 bubble；nested assistant Arc 与 im root 则共享，不能全部记为深拷贝。

小 shell + 无关大 detail 时，每次该面板构建/每次有效 snapshot 有 O(全表 owned 字节) 暂态复制，调用后释放。M2 放大其规模；隐藏 shell 没有这条 render 路径，不宣称固定帧率或现场峰值。降级原因是频率、载荷和资源影响未量化；局部修复明确，所以仍排在早期。

**验收**：借用 guard 内只提取选中任务展示值；snapshot 仅提取 task/status。无 await 跨 guard。增加无关大 detail 时，所选小任务复制字节不得随之增长，snapshot 不复制正文/JSON；终态与 Unobserved 语义不变。AssistantCloneBytes 不覆盖全部 JSON 分配。实施权威：M3，不与 F10 混同。

### M4：确认大历史预览全量物化；大峰值为测量假设，P1 下调 P2

`panels/thread_browser.rs:48` `use_async_state` → `:57` → `acp_client/client/workspace.rs:154` `read_session_history`：无页/字节上限，`:175` 每项 Value 序列化再解码；`thread_browser/history_preview.rs:5` 格式化完整 String。`thread_browser.rs:516` 逐行 owned String，`:519` 再 clone Lines 给 Paragraph，`:525` 全文 line_count；高度 clamp 不限制分配。

确认加载时 Value/DTO/输出阶段性重叠和稳定预览每帧全文复制，未确认固定倍数。KIT `src/hooks/use_async_state.rs:45` 开始/失败不清旧成功 data；`src/hooks/use_effect.rs:37` / `:86` 是 hook-owned boxed future，依赖变化直接替换，**不是 detached preview task**。`thread_browser.rs:196` 退出预览，下一次成功 None 回写后旧值释放；面板卸载/请求 drop 不代表 backend 请求即时取消。

**验收**：先去除不必要 Lines clone并缓存稳定派生行，再评估 ACP 分页/分块契约。用合成历史测加载、展示、换 ID、失败、退出与卸载的 owner/drop 和 allocated bytes；无静默截断、只读身份不切执行 session、Unicode/完整工具参数可查、独立滚动保持。归 M4；与 R2 的“轻量列表分页”是不同路径。

### M5：队列/派生请求风险条件成立；与 R6/R5 合并，不双计

notification `acp_client/client.rs:160`、bridge `kit/entry.rs:368` 均 unbounded；`acp_notifier.rs:79` / `:140` 与 `acp_bridge.rs:575` 构成消费链。50ms scheduler 只限普通发布，不限队列字节。增长必须有持续生产超过消费或消费者停顿；没有测到实际 backlog，故“现场大 RSS 来自队列”为**测量假设**。

额外确认 `acp_events/system.rs:756` `accept_bg_task_delta` 遇 revision gap 在 `:765` 调 `request_bg_task_snapshot`（`:725` / `:733` spawn），未推进 revision 也无 single-flight；慢响应期间后续 gap 可继续创建请求。完成仅 `:743` 校验 session ID，同 ID 新 epoch 的保护需纳入验收。此项属于同一 UI 请求 owner/准入问题，不再登记第二个 fan-out 风险。

反证：`transport/router.rs:67` `PendingRequest::drop` 清 pending registration；`input_area/submit.rs:45` 普通正文在 capability 开启时走 STEERS，legacy loading buffer `:93` 有 32 条限制，不能说每次按键都无限 detached prompt。条数限制不等于字节限制。mini bridge 生命周期见 R5。

**主责/验收**：R6 唯一承接事件/字节背压、submit 准入与 gap single-flight，M5 为内存证据；R5 承接相同任务 owner 的关闭/join。P2 风险项，压力验证靠前，不保留“已证实 P1 内存事故”。测 queue depth/bytes、in-flight 与 drain；burst 后收敛，terminal/tool/interaction 保序不可丢。不能直接 bounded+await 阻塞 ACP 双向请求形成死锁，也不能对挂起 prompt 加统一超时冒充 Agent 取消。

### M6：确认跨会话显式保留；保留 P2 预算研究，驳回 reset 泄漏表述

已按最新 dirty 重定位：`steer_state.rs:35` `SessionSteers` 新增 interrupted 集合；`:55` `SteerState.sessions`，`:61` `reset_session` 只清目标 snapshot/direct_submissions，保留恢复义务。`:501` `enqueue` 构造正文/附件输入，`:519` / `:520` command clone 到状态和通道。`:217` `settle` 校验 pending/epoch/generation，`:229` 清 interrupted、`:231` 清 pending；`:277` `reject` 防旧回执，`:303` `claim_delivery` 消耗已投递项，`:316` `recover` 移走恢复原稿。

**并行改动已纳入**：这些防重复/陈旧回执保护和 `:156` `resume_pending` 的 generation 变更停止重试已经存在，不登记成未修正确性缺陷。它们未增加 sessions 总数/字节预算，也未全局删除无恢复义务的 session。长期持未确认原稿有明确语义；新增 interrupted 状态尤其不能靠 clear 强制“释放”而重复投递。无用确认快照与必要恢复数据须分开计量。

**验收**：成功投递、明确拒绝、撤回、Unknown、generation 改变、切换/重载及大附件分别追踪独占 payload；只回收无义务对象，恢复仍按原 command/输入身份，不丢稿、不串稿、不把 Unknown 当失败重投。恢复稿 TTL/持久保存是待批准策略，不是本审计授权实现。归 M6；warned 是 consumer 独立集合，归 R7。

## 5. Runtime 七项逐条裁决

### R1：确认无变化轮询与旧会话响应竞态；P2

`entry.rs:429` → `workflow_snapshot.rs:90` `spawn_workflow_poll`，`:95` 每 2s 串行 RPC，`:104` 取 sid，`:107` await，`:128` 无判等替换且不再校验 sid/epoch。A 请求中切 B 可写回 A 快照；静态竞态成立，未作时序复现。没有 session 时仅写空值，不发此 RPC。KIT `reactive_handle.rs:299` / `:310` 可变解引用后通知；无 subscriber 不等于必画一帧。

**验收**：延迟 A→切 B→返回 A 不覆盖 B；同 session/epoch 相同内容无 mutation。隐藏/不支持的查询按需求门控，打开/恢复/终态有明确 freshness；失败保留可见错误/日志，不用 empty 掩盖。先修身份正确性与判等，不能把单串行 poll 写成每两秒新增一个并发任务。

### R2：确认隐藏 History 重取已加载页；P1 下调 P2

`service_snapshot.rs:94` 2s interval，`:223` 到期或 scope/cwd/page 变化触发，`:245` 下次仍 2s；`refresh_threads`（`:401`）在 `:428` 重置 cursor，`:430` 从第一页遍历已加载 P 页，末页提前结束。没有 History 可见性门控。`:247` / `:258` / `:273` clone 慢缓存，`:281` 的 `session_services::query`（`session_services.rs:38`）请求 plugin/mcp，`:299` metadata；`:456` 判等只省最终通知。文件/memory 扫描分别在 `:250` / `:263`，30s 门控仍在。

**主责划分**：重复请求生产、隐藏可见性、分页缓存归 TUI R2；数据库/远端字节与查询复杂度归 resources。`acp_client/client/workspace.rs:181` → `session/list`；ACP `requests/session_lifecycle.rs:483` / `:524` 调 list_sessions，`peri-resources/src/sessions/resources.rs:534` 直达 data，SQLite `workspace.rs:478` / `:484` 查询轻量 threads/binding/workspace 字段。metadata 经 ACP `session_restore.rs:345` / `:369` → resources `:503` 的 load_meta。**这两条不是 WorkState 全量读链**。当前 backend `continuation.rs:152` 已改 load_work_availability，旧报告“仍全量读取”的叙述也不能直接沿用。

确认每次重复工作随已加载页/条目增长，未证实用户加载深度、请求耗时及现场占比，故降 P2。修复不得简单“已加载永不刷新”：标题/归档/删除/外部更新仍需失效或增量刷新协议。

**验收**：加载 1/10/更多实际页后隐藏，分别计请求数/条目/bytes，隐藏不持续回放全页；新增页只取缺失页，真实更新正确失效。慢 RPC 与 cwd/session 切换不发表陈旧结果；保留当前会话状态的新鲜度。backend WorkState 热点另归进程 P0，不能将 R2 优化收益与之双计。

### R3：确认空闲 timer 与心跳；P2

`entry.rs:235` / `:239` 的 5s heartbeat 常驻；`:257` / `:267` spinner 每约 100ms 检查 loading，非 loading 不写 atom仍会唤醒 task；loading 且帧变才在 `:273` 写。`app_shell.rs:31` 常驻订阅，KIT `render/tree.rs:80` / `:84` 响应更新。bridge 秒级 tick（`acp_bridge.rs:532` / `:564`）仅 running Bash 请求发布，不是每秒无条件全快照刷新。

计时检查本身轻量，实际 draw 可合并，不能把 10/s + 0.2/s 算作实测 FPS 或主因。**验收**：idle/长历史/loading 无输出/HITL/失焦分别计 wake、mutation、update/draw；无可见动画时停高频检查，watchdog 的切窗口恢复能力保留。内容重建放大归旧 F5。

### R4：确认 raw 事件强制 render；P2，依赖层主责

KIT `src/render/tree.rs:78` `render_loop` 在 `:97` dispatch 后 `:100` 无条件回到 `:80` render；`:66` render 包含 update+draw，`:55` 重建输入注册表。ignored/no-op/节流 pending 也可触发；退出 Ctrl+C 特例在 `:94` 直接 break，故不应绝对写“所有 raw 事件必 render”。hover 本地判等不能消除框架这次 render。

增长随取出的输入事件率×树成本，鼠标压力条件成立，无输入自旋驳回。**验收**：无变化 Moved/键事件不逐条全树绘制；真实拖选、滚轮最终位置、capture、resize、纯副作用退出保持。修复在 kit render-demand 契约，不能只改应用 debounce 或任意丢输入。旧 F5 承接被触发的布局成本，不另算同一帧两次收益。

### R5：确认 UI owner/drain 缺口；P1 下调 P2，驳回正常退出长期泄漏

`entry.rs:374` mini bridge 只有 recv/send，无 token/join，OnceLock sender（`atoms.rs:612`）使它可等待；该任务是 `tokio::spawn`，不是凭“local event”名称推断的独立 LocalSet。`submit_consumer.rs:73` 每个已接受请求 detach child；`workflow_snapshot.rs:107`、`hitl_response.rs:67` 在外层 select 的分支中 await，单独 cancel 外层 token不能中断该 await。归属与退出可核对性缺口确认。

**退出反证**：`entry.rs:553` cancel → `:556` teardown；`launch.rs:263` → `acp_client/deployment.rs:20` 先 explicit transport close 再 host.shutdown。`cli_tui.rs:46` 创建本次 CLI runtime，`:48` block_on 返回后 `:69` **显式 drop(rt)**。KIT render tree 自己的 future/组件随 fullscreen 返回释放；ACP host 在 `peri-acp/src/host/lifecycle.rs:54` 由 tokio task 持有。不能推导 UI task 在普通 CLI 进程退出后长期驻留，也未证明同 runtime 重复装配 TUI 是现有生产路径。若某任务另在独立线程/LocalSet，必须追踪该 owner，不能由此处推及所有外部任务。

因此 mini bridge 是单个 parked task，不是 CPU 自旋、每次切会话重复创建或已证实大内存泄漏。慢请求的活跃期增长归 R6；服务复用/退出不完整时的残留是条件风险。teardown 没有统一 UI join，runtime 回收也不等于协议/资源 drain 已完成；Incomplete 仍须报告。

**验收**：connected silence 下 token-only、explicit close、正常 CLI runtime drop、持久 runtime 嵌入分别验证完成信号与 pending/drop；所有派生请求有 owner，退出无晚到全局 UI 写入。停止 observer 和取消 Agent 分开，禁止用本地 abort 宣称远端已取消。与 M5 共用 owner，不再单列重复 fan-out 修复。

### R6：确认无容量边界；积压/扇出条件成立，P1 下调 P2

主责覆盖 M5 全部队列/字节预算、submit 准入、BG gap single-flight。证据为 `acp_client/client.rs:160` → `acp_notifier.rs:79` / `:140` → `entry.rs:368` → `acp_bridge.rs:575`，以及 `submit_consumer.rs:73`。publication scheduler 不提供入队背压；但没有 λ/μ、live tasks、backlog bytes 的观测，不能把结构无界升级成现场 P1/P0。

**验收**：受控事件大小/速率、慢 bridge/transport 和连续 revision gap；统计两级队列独占字节、最大 in-flight、终态延迟与停产后收敛。session/epoch 过期请求不能回写，single-flight 失败/取消释放准入；reverse/control 不被普通 backlog 永久阻塞。不以无界变有界后死锁或静默丢帧换低 RSS。压力验证排在早期，风险严重度须据实际失败再升级。

### R7：确认同 revision 冗余 mutation 与失败 ID 保留；P2

最新源码仍为 `steer_consumer.rs:15` / `:57` 5s reconciliation；`:124` 只在 enabled+active session 生成 Refresh；`:244` 取 snapshot，`:246` 调更新，`:259` 再可变访问 bind。dirty `steer_state.rs:80` `accept_snapshot` 在 `:95` 只拒绝更旧 revision，`:104` 同 revision 仍替换；当前 `establish_session_snapshot` 已移动到 `:482`，`:489` / `:491` 可变写并接受。新 interrupted/receipt 保护未修此冗余，不能记作已完成。

`steer_consumer.rs:56` warned 集合只在 `:92` / `:93` 的**非 Refresh 失败且新 command ID**增长，无 epoch 清理，任务结束释放；不是成功输入/每次轮询都增长。通知与 draw 并非一一对应。**验收**：相同 snapshot 且无待结算命令时不 mutation；不得仅按 revision 提前 return 而跳过必要的 resume_pending/回执恢复。新 revision、Unknown、重连/新 generation 保持原身份对账；warned 去重有生命周期/预算。M6 的 session payload 是另一 owner，两者字节不能混算。

## 6. 去重与权威实施路由

2026-10-06 用户另行指定并经 Astra 复核的
[P0 TUI 架构优化](2026-10-06-p0-tui-architecture-optimization.md)独占承接原 F4 历史渲染缓存预算、
F5 增量索引/动画分离及 F10 共享缓存/详情聚合余项。本审计其他 CPU/M/R 项维持既有定级与归属，
不因架构总 issue 为 P0 而批量升级；下表按该路由更新。

| 工作 | 唯一实施/验收权威 | 本审计如何使用 |
| --- | --- | --- |
| backend WorkState/进程资源事故 | [进程 P0](2026-10-06-p0-dev-peri-high-cpu-memory.md) | 样本热点旁证；不归 TUI 绘制，不沿用已失效的全量 clone/窄查询描述 |
| replay fold、主子合帧、发布与 dirty 分离 | [9/27 发布链 issue](2026-09-27-p0-tui-streaming-view-rebuild-cpu.md) | 修复机制已有，现场验收未闭环；不重报逐 chunk 必发布 |
| 原有流式修复、未迁出余项与原现场验收 | [流式冗余 issue](2026-10-06-tui-streaming-render-redundancy.md) 的“裁决与实施” | 不按历史发现清单重开已修项；F10 测试 oracle 清理仍归原 issue |
| F4 重型历史缓存预算、F5 增量索引/动画分离、F10 共享缓存/详情聚合余项 | [架构 P0](2026-10-06-p0-tui-architecture-optimization.md) A–C | 唯一实施入口，不在原 issue 另开清单 |
| reasoning 全量折行、稳定 slot S 级搬运 | CPU-1 / CPU-2 | 分别新增计量；与架构 A/B 的共同依赖不重复认领收益 |
| static memo reset、后台保留、整表 clone、预览、恢复预算 | 内存 M1/M2/M3/M4/M6 | 各自唯一 owner；历史渲染缓存预算转由架构 P0 承接 |
| 队列/请求背压与 gap single-flight | Runtime R6 | M5 只补所有权证据，不重复条目/收益；退出归 R5，共用 task owner |
| poll/唤醒/退出/对账 | Runtime R1–R5/R7 | UI 发起频率归 UI；被调用服务自身的全量存储工作归 backend |

已核实的排除项：`markdown/mod.rs:239` terminal 释放 chunk_source buffer；`:339` 的旧 parser 字段仅 cfg(test)；`markdown/code_block.rs:45` 双预算、`:119` 高亮返回 Arc；`panels/subagent_detail_cache.rs:37` 按 occurrence/主题/宽度/语言清 slots，`:63` 只重建失效项，`:67` 仍物化当前组选中行。这些不能分别再报 terminal 原文永久残留、无界高亮、详情每帧从零建 parser。主消息区 slot 仍按全历史保留（`message_area/mod.rs:285`），不是视口内存上限，原 F4 的预算余项现归架构 P0 工作包 C。

Markdown renderer 遇空 bubble/其他 VM 变体可能绕过 terminal（`message_area/render.rs:178`），slot 也按索引复用；原 F4 的生命周期反例现由架构 P0 的缓存生命周期验收覆盖，尚未证明真实事件链留下多大旧缓存，不升级为新增泄漏。原 F6/F7/F8 本次没有新的反证，沿用旧 issue 的实施状态，不把旧测试数字当本审计复跑。

## 7. 建议顺序与统一验收

1. **先固定可观测基线**：最终源码/dirty diff 摘要、lock、profile、binary hash/UUID、负载与启动参数；分别数 UI frame/publish、RPC、backend 编解码、queue bytes、task owners。已有 sample 只能选取观测入口，不能作为当前优化的前基线。
2. **先做确定、局部的修复**：M1 reset root；M3 最小投影替代全表 clone；CPU-1 running Collapsed 短路；R1 session/epoch 保护与判等。M3 虽为 P2，仍因边界小、收益机制明确而靠前；P1 不代表这些修复已经授权执行。
3. **抑制无变化工作**：R2 可见性/增量刷新与慢缓存复用，R7 语义判等；CPU-1 缓存与 CPU-2 布局共享。R3/R4 修改计时/框架前先量实际触发率，与旧 F5 去重测端到端成本。
4. **尽早压力验证，再落实统一 owner/预算**：R6/M5 与 R5 同一实现计划；事件 burst、reverse request、挂起 prompt 注入、连续 revision gap、connected silence + 关闭。不能靠终态丢失、返回空快照或无限等待“通过”。
5. **保留策略最后按功能契约落地**：M2 终态详情、M4 大预览、M6 恢复数据维持本审计归属；原 F4 历史渲染缓存预算由架构 P0 工作包 C 实施。先确认哪些内容必须可回查、何时可淘汰，再设计 ACP 查询/预算；不直接截断或清理用户草稿。

行为验收用确定性 fixture、虚拟时间/受控 transport 和 owner/drop；不连接真实模型或读真实会话正文。UI 显示人工核验，纯逻辑/事件/选择/复制/生命周期用仓库现有测试位置，不新增截图框架。Rust 测试实施时使用 patched cargo 入口及精确过滤词，核对非零用例数和退出状态；跨协议变化遵守 architecture/testing 并补真实 wire/ordering，不能用单个 unit test 替代退出或重连生命周期。

性能验收至少分离：历史条数 N、稳定行 S/chunk K、当前正文/推理字节 R、终态详情总字节 B、已加载页 P、历史预览 H、事件生产率与消费率。分别记录扫描/复制/解析/通知/请求计数及分配峰值；Arc/im 共享对象只算一次，len/capacity 与 allocator 保留分列。同 profile/同负载比较 CPU 时间差分、延迟与 heap/RSS，debug 与 release 不混比；不预设改善百分比。

## 8. 本轮检查与未验证范围

- 已完成：三份扫描全部条目和汇总/旧 issue 去重；逐项独立源码及锁定 kit 依赖读取；原 sample/ps 口径和映像身份复核；最新 steer dirty 的语义与行号重定位。
- 文档检查：本文件 8 个本地链接与完整仓库路径引用检查通过；7 个原始证据文件的 SHA256 全部匹配，SHA256SUMS 自条目失败单列，不改材料。写文档前后核对的 30 个关键源码文件 hash 一致；这不把更早的并行工作树变成冻结基线。文首 Markdown 硬换行空格已移除；本文件逐行 whitespace 检查 exit 0，仓库 `git diff --check` exit 0，二者均非构建/性能验收。
- 未运行：Cargo build/check/test、benchmark、终端交互、heap、allocator 计量、新 profiling；未验证真实退出任务数、面板卸载的堆释放曲线或新旧构建的资源收益。静态控制流成立不代表已有回归测试通过。
- 非冻结工作树仍可能被并行任务修改；行号与结论适用于读取到的当前内容，后续实现前按符号重核。不以同时出现的新提交归属本任务，也不宣称所有未改模块无问题。
- 按 DOC-UPDATE-001，本次只新增 active 审计 issue，没有实现/规则/入口变更，不更新 standards/CLAUDE/code-index。由主 agent 依据本裁决更新三份扫描及汇总；原 issue 的待验收工作继续 Open。
