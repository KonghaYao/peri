# TUI CPU 渲染链专项：有界显示后的全量 reasoning 折行与稳定布局搬运

**状态**：Open；源码扫描与 Astra 独立审计完成，修复及验收待实施。未修改生产代码，未运行构建、测试或新增 profiling。
**日期**：2026-10-06。
**优先级**：CPU-1 / CPU-2 经 Astra 确认机制，均保留 P2；操作计量与现场归因未完成，不认定为 P0 或现场 CPU 事故根因。
**事实源**：本次读取的当前工作树，包括未提交的 `markdown/mod.rs`、`markdown/boundary.rs`、`message_area/mod.rs`、`message_area/vm_cache.rs`。行号为扫描快照，后续并行修改须按符号重定位。

## 范围与证据口径

- 最终裁决见 [Astra 审计 §3/§6](2026-10-06-tui-perf-astra-audit.md)：CPU-1 确认全量 reasoning 折行；CPU-2 确认稳定逻辑行元数据搬运，不是正文深拷贝。每帧索引及 chrome/content 分离仍归旧 F5，禁止重复计收益。下文保留扫描时证据，是否仍待修及定级以审计为准。
- 已读 standards index / tui / rust / architecture-contracts / testing / documentation、`peri-tui/CLAUDE.md`、`docs/code-index/peri-tui.md`；只写本 issue。
- 核查主消息区状态订阅、slot 检测、Assistant Markdown 分片、reasoning 渲染、wrap map 与 SlotIndex，以及 bridge 发布入口；未完整扫描其他面板、工具 presentation、ratatui 依赖内部或内存生命周期。这些范围不宣称无问题。
- **源码确认**指调用与数据操作可直接读出；**推断**指复杂度、潜在 CPU/分配收益；**实测**仅能对应实际被观测的二进制。本报告两个候选均没有当前源码的运行计量。
- 主 agent 提供 `/tmp/peri-tui-perf-20261006-2nsZNM/sample-{9500,55171,49449}.txt`，采样窗口 20:30:09–20:30:12。专项符号检索在 sample-9500 / sample-49449 中看到 `MessageArea::implementation`，不据此归因到以下具体操作。二进制启动早于 dirty 修改，不能认为样本对应当前工作树；包含计数不是互斥 CPU 百分比。进程归属、观测数值和完整现场口径归主 agent 汇总 issue，本文件不复制整体分析。

## 去重结论

权威实施进度参考 [流式与渲染冗余](2026-10-06-tui-streaming-render-redundancy.md) 的「裁决与实施」「验收限制」，以及 [2026-09-27 CPU 记录](../history/2026-09.md)（2026-09-27 条目）的实施记录；不把旧发现位置当成当前实现。

| 旧项 | 当前源码复核 | 本次裁决 |
| --- | --- | --- |
| F3 保守尾部重解析 / 前缀比较 | `markdown/mod.rs:149` 仍比较已冻结源前缀；`:174` 从 stable_source_end 扫描；`:195` 之后解析可变尾部。`boundary.rs:6` 是现行边界实现 | 已闭合安全 fence 分片不能报为未修；长敏感尾部与前缀比较仍归旧 F3，不新开 |
| F4 多份缓存 | `render.rs:257` 已使用空 Line 占位而非稳定正文深拷贝；`vm_cache.rs:128` overlay Arc 共享。`markdown/mod.rs:239` terminal full parse 后清分片 | 不再报「稳定正文每次深拷贝」或「终态仍留 chunk_source」；CPU-2 只补充占位与 wrap 元数据搬运成本，不另立内存缺陷 |
| F5 每帧扫描 / 动画 | `message_area/mod.rs:265` 之后复用三个扫描缓冲；`tui_render_unit/unit.rs:56` reasoning period=10，工具/子组 period=1 | 旧分配和 reasoning 100ms 强刷不再报；仍有 O(N) hash/index 工作及 running 内容/动画耦合，归原 issue |
| 9/27 fold/replay 与调度 | `acp_bridge.rs:350` 的 fire_at 到 `:377` 才 push_view_models；旧 issue 已有 replay fold 缓存与发布合帧实施记录 | 不报每个主/子 chunk 必然发布，不报历史 fold 每次重 hash |
| F9 高亮 / F10 详情缓存 | 原 issue 实施记录已明确修复与余项 | 未证伪已有修复，不把原始发现清单重复算新增 |

## 公共调用链

ACP 通知 → `acp_notifier` → `acp_events::dispatch_for_bridge`（`peri-tui/src/kit/acp_events/mod.rs:293`）→ streaming handler 更新 BridgeState → PublicationScheduler 的 intent/deadline 或同步 barrier → `push_view_models`（`peri-tui/src/kit/acp_events/render.rs:26`）→ `VIEW_MODELS` 写入（同文件 `:153`）→ `MessageArea` 订阅（`peri-tui/src/kit/message_area/mod.rs:93`）→ hash/动画/宽度/主题/语言检测（同文件 `:269`、`:298`）→ 需要 rebuild 的 slot 调用 `vm_to_lines_cached_with_layout`（同文件 `:343`）→ slot lines/wrap map → SlotIndex → 视口输出。

组件也可因交互、主题或已有动画驱动构建；**不把每次组件构建等同于发布，也不假设 wall clock 自己触发重绘**。每次构建都执行第三阶段索引装配（`message_area/mod.rs:376`），slot rebuild 则有显式门控。

## CPU-1：reasoning 输出限为 0/4/100 行，但提前物化全部视觉行

**证据级别**：源码确认（全量折行及先后顺序）＋推断（渐进成本和收益）；**置信度**：高。**建议**：P2，未证实现场命中。

### 精确位置与完整局部调用链

公共调用链 → `peri-tui/src/kit/message_area/render.rs:190`（有 reasoning 即调用）→ `render/reasoning.rs:36` 的 render_reasoning_block → `:53` reasoning_visual_lines → `:29` 对全部 text.lines 做 flat_map → `peri-tui/src/truncate.rs:106` wrap_by_width → `:113` 按 grapheme 遍历、创建所有 String → collect 全量 Vec → 才按状态/fold 限制输出：`render/reasoning.rs:78` running tail_max=0/4/100，`:83` 取尾；completed 在 `:95` 生成行数摘要，`:112` 之后只在 Expanded 取前 100 行。

### 触发与成本

- Running 的 Collapsed 也在判断 fold 前完整折行，最终只画状态行，且该分支不消费 line_count。无正文变化但跨秒重建仍走此路径：`tui_render_unit/unit.rs:60`–`:66` period=10；MessageArea 在 `mod.rs:306` 检测 tick 桶变化。
- Preview 最终最多 4 个视觉行；Expanded 最多 100 个视觉行。限行没有限制折行与中间分配；宽度变更、fold 变更、reasoning hash 更新、同 bubble 正文变化也会重进 renderer。
- Completed 的折叠摘要确实需要准确视觉行总数，不能简单删除计数；但同 bubble 正文还在流式增长时，没有单独的 reasoning 内容/宽度缓存，未变的 completed reasoning 也再次计数与分配。
- 设 R 为 reasoning 字节量、L 为折行后的视觉行数，每次重建全量 grapheme 扫描至少随输入线性增长，创建 Θ(R+L) 中间字符串/元数据；Unicode width 的具体常数未计量。连续增长 M 次成本按 ΣR_i 推断，不声明 CPU 百分比。中间 Vec 是本次调用的暂态，不是常驻泄漏。

### 验证方法

在既有 `render_reasoning_test.rs` / `render_reasoning_block` 邻域增加测试操作计数（不要只复用 TailParsedBytes，它只覆盖 Markdown）。固定宽度、主题及语言，构造 4 KiB/64 KiB/1 MiB reasoning，分别覆盖 running Collapsed/Preview/Expanded 和 completed Collapsed/Expanded；测试正文未变的秒级 rebuild，以及 completed reasoning 未变但同 bubble 正文追加。记录 grapheme 访问、物化字节与行数；另用 release fixture 比较时间/分配，不拿 debug 与 release 混比。

### 修复边界与验收

- 在 reasoning 渲染/slot 派生布局边界处理，不变更 ACP、BridgeState 权威或用户 fold 语义；先短路 running Collapsed。
- Preview/Expanded 可保存宽度绑定的增量折行摘要和有界首/尾行；completed 总行数单独缓存。末行追加、grapheme 跨 chunk、改写/缩短、resize 必须使派生状态正确失效；不能以字符数代替显示宽度。
- 验收：running Collapsed 不访问 reasoning 正文；未变 reasoning 的 tick 或同 bubble 正文更新不再次全折行/物化。Cold/resize 的正确全扫允许保留；completed 摘要、CJK/emoji/组合字符、空行过滤、首尾取行和滚动高度与完整参考等价。

## CPU-2：已冻结 Markdown 行免解析，但 slot rebuild 仍线性重建占位与稳定 wrap 元数据

**证据级别**：源码确认（占位填充、遍历、克隆偏移）＋推断（渐进成本和收益）；**置信度**：高。**建议**：P2。与旧 F3/F4/F5 相关但新增的是稳定逻辑行级的 CPU 工作；实施时协调同一修复，不重复记收益。

### 精确位置与完整局部调用链

公共调用链 → `render.rs:212` parse_markdown_chunks_cached 复用 stable Arc → `render.rs:222` 为各稳定 chunk 构造 identity 列表，`:256` retain_and_wrap 命中后 → `:257` 为每一稳定逻辑行 resize 一个 `Line::default()` 占位 → 返回完整逻辑长度的 lines → `message_area/mod.rs:350` 新建 Arc → `:351` build_slot_wrap_map → `vm_cache.rs:146` 累加稳定行数 → `:157` 遍历所有稳定 wrap_map，`:158` 逐项 cloned 并重写 logical_idx / visual_start / visual_end → 与新 tail 拼成新 Vec → `message_area/mod.rs:359` 替换整个 slot wrap_map。

每次组件构建还从 `message_area/mod.rs:384` 调 stable_overlay（`vm_cache.rs:128`）收集 Arc → `selection.rs:74` SlotLines::composite → `:86` 累加稳定长度、`:88` 构造 parts → `:104` 重建 part prefix → `selection.rs:175` SlotIndex::new_with_overlays → 两个 slot prefix（`:180`），再在 `message_area/mod.rs:399` 建 slot_visual_starts。这个 **O(N+K)** 元数据重装配是旧 F5 余项的具体落点，不独立计为第三项。

### 触发与成本

- 长 assistant 正文已有多个安全稳定块，继续在尾部追加短 token；hash 改变令同一 slot rebuild。即便 stable parser / materializer / wrap cache 全命中，仍填满稳定行的占位并复制稳定 wrap entries。
- 设 S 为稳定逻辑行数、K 为稳定 chunks、T 为尾部成本：该布局链额外 Θ(S+K)，而不是仅 Θ(T)。每次重建新占位 Vec 与新 wrap Vec 大小 Θ(S)，旧缓存替换后释放；不能称为永久增长的泄漏，也不是正文字符串深拷贝。每帧额外 O(N+K) 索引工作与历史条数/分片数相关，不是对全部正文重新解析。
- `vm_cache.rs:217` 的既有测试验证 WrapRecalculatedLines=2，只计重新折行，不能覆盖 `:158` 稳定 metadata clone 与 `render.rs:258` 默认 Line 填充。这是为何已有“只重算 tail”操作计数不等于整个布局链只做 tail 工作。

### 验证方法

固定尾部与终端宽度，构造 128/1024/8192 个已冻结逻辑行，再做 32 次短尾追加；分离记录 parser 输入、materialized lines、wrap 重算、stable wrap entries copied、placeholder lines initialized、索引 parts 创建。当前测试 counter 不覆盖后两项，应新增测试级计量，而不是拿零 TailParsedBytes 声称 CPU 为零。

### 修复边界与验收

- 将 slot-local wrap 数据与逻辑行访问表示为稳定共享分片＋可变尾部＋prefix，复用既有 SlotLines 思路；不新增另一个 Markdown 规则权威、不改变语义选择/复制的索引口径，不凭行数截断历史。
- 稳定 chunk 长度直接纳入索引，而不是为稳定正文铺一份空 Line；稳定 wrap_map 通过分片基址做定位，不每次复制全部 entries。纯元数据未变的帧可缓存 index；此部分与旧 F5 统一处理。
- 验收：稳定块完全命中且只追加短尾时，稳定 placeholder 初始化与稳定 wrap entries 复制计数为 0；新增工作的计数不随 S 线性增长。resize/主题/语言/改写允许必要重建，但语义复制、跨 chunk 选择、滚动、空 slot、图片/表格、pending interaction 命中必须保持完整参考等价；保留既有 selection / semantic copy / render layout 测试。

## 交付与审计门禁

- 本轮仅静态复核及提供样本的符号检索；没有运行 Cargo、测试、benchmark、采样命令或修改源码。旧 issue 中的测试结果属于旧任务记录，不算本轮通过。
- 只新增本文件；生产 dirty changes 保留。按 DOC-UPDATE-001，本轮没有架构/规则/模块入口变更，不改 standards、CLAUDE 或 code-index。
- Astra 已审：CPU-1/CPU-2 的确定机制作为新增计量项保留 P2；chrome/content 与逐帧索引归旧 F5，内存表示与旧 F4 协同验收。缓存命中不代表全链 O(tail)，stable 元数据和旧 F3 前缀扫描仍需计量。完整裁决见前文审计路由。
