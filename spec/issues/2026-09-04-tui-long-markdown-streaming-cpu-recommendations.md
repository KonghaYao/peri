# TUI 超长 Markdown 流式渲染优化建议

> 文档性质：active issue 的原始调研与建议，以及已批准方案的实施/profile 状态；稳定现行设计见 `docs/design/tui-streaming-markdown-performance.md`。
>
> 当前状态：convergence round 1 已完成 review remediation 与全门禁验证，等待独立 round 2；Phase D 使用 `SlotLines` composite storage，使 viewport/selection 按 part offset 读取 shared stable `Line` chunk，不再将稳定前缀 clone 回 contiguous slot。
>
> 问题事实源：[TUI 超长 Markdown 流式输出导致 CPU 占用爆炸](2026-09-04-tui-long-markdown-streaming-cpu.md)。以下原调研叙述保留用于解释裁决背景。

## 1. 目标

在不改变 ACP 事件语义和最终 Markdown 结果的前提下，使 TUI 流式渲染成本主要取决于：

- 新增 chunk 的内容量；
- 当前视口或不稳定尾部的规模；
- 受控的 UI 刷新频率；

而不再取决于“每个 chunk × 当前完整回复”或“每帧 × 完整历史 transcript”。

目标不是追求真正逐 token 绘制。终端用户无法稳定感知高于屏幕刷新节奏的更新，而每个 provider chunk 触发一次完整 projection/layout/render 会制造无意义工作。合理目标是：

- 首 chunk 立即可见；
- 中间状态以 20–30 FPS 为默认上限；
- Markdown block boundary 可提前触发；
- terminal event 立即 flush；
- 最终渲染与完整输入解析一致。

## 2. 设计原则

### 2.1 数据接收与视觉发布解耦

ACP chunk 必须立即进入 `CurrentTurn` 的 canonical 累积状态，但不应立即产生 owned UI snapshot。UI publication 是派生行为，可以被合帧。

```text
TextChunk
→ append canonical text / rolling hash / dirty=true
→ scheduler 请求一次 publication
→ 到达 frame deadline 后构建 snapshot
→ VIEW_MODELS write
→ MessageArea render
```

### 2.2 terminal event 不受节流

`TurnDone`、失败、中断、取消、session transition 和需要立即显示的 reverse interaction 都必须强制 flush pending stream，再发布 terminal 状态。合帧不能改变终止语义，也不能让 loading 状态残留。

### 2.3 缓存单位应匹配变化单位

变化单位是“当前 Markdown 的不稳定尾部”，不是整个 Assistant bubble；缓存单位应逐步从 VM 下沉到 block/segment。稳定前缀只保存 immutable rendered output，不应因尾部追加而重新 parse、clone 或 wrap。

### 2.4 优先削减调用次数，再优化单次常数

当前多个阶段都有全文工作。先把每秒数十或数百次调用限制为 20–30 次，能同时降低 projection、parser、layout 和 terminal draw 成本。直接微调 parser 常数无法解决 chunk 频率放大。

### 2.5 性能优化不得牺牲最终正确性

流式中间画面允许使用尾部重解析，但 terminal flush 应保证最终结果与完整 Markdown parse 一致。table、image 和未闭合 fenced code block 的结构翻转必须 fail safe，不得复用错误稳定前缀。

## 3. 推荐方案

### 3.1 Phase A：加入流式 publication scheduler

### 行为

为主 Agent 流式文本和推理引入单一 session-local publication scheduler：

1. Idle 状态收到首 chunk：立即发布；
2. deadline 窗口内收到后续 chunk：只更新 canonical state，并保持一个 pending publication；
3. deadline 到达：发布最新聚合状态；
4. Markdown block boundary 到达：允许提前发布，但仍应避免同一 event-loop turn 中重复绘制；
5. terminal event：取消 pending timer，立即发布最终状态；
6. session reset/load/rewind：取消旧 session 的 pending publication，禁止陈旧 timer 发布。

建议默认帧间隔从 33–50ms 开始，通过 profiling 决定采用 30 FPS 还是 20 FPS。不要把 provider chunk 率当作 UI frame rate。

### 所有权边界

scheduler 应位于 ACP event 到 `VIEW_MODELS` publication 的 bridge 层，而不是 Markdown 组件内部：

- canonical streaming state 仍由 `BridgeState` / `CurrentTurn` 持有；
- `MessageArea` 不感知 chunk，也不负责 debounce；
- ACP transport 和 Agent runtime 不承担前端 frame policy；
- session epoch/owner 检查必须在 publication 时再次执行，防止 timer 跨 session 生效。

### 为什么不能只 throttle redraw

如果 `append_text` 仍同步调用 `sync_cache`，即使下游 redraw 被合帧，trailing `text_slice.to_string()` 仍按每个 chunk 执行。因此 Phase A 必须与 Phase B 配套，至少让 owned projection cache 延迟到真正 publication 时同步。

### 3.2 Phase B：让 CurrentTurn cache 变为 lazy projection

### 当前问题

`CurrentTurn` 同时承担 canonical 累积状态和 eagerly-owned ViewModel cache。每次追加正文都会重建 trailing `TuiAssistantBubble.text`，导致 growing-prefix 复制。

### 建议

将 mutation 路径收敛为：

```text
append_text / append_reasoning
→ 更新 String
→ 更新 rolling hash、message metadata、timing
→ 标记 cache_dirty
→ 不构建 TuiAssistantBubble
```

在以下边界调用 `sync_cache`：

- scheduler 决定发布一帧；
- tool/subagent 边界需要冻结前序文本；
- terminal event；
- 明确读取 `view_models()` 且 cache dirty。

### 最小实现与长期实现

最小实现不必立即引入 rope、piece table 或复杂生命周期。只要把每 chunk 一次全文复制降低为每视觉帧一次，就能获得显著收益，且改动较小。

长期可选方案包括：

- `Arc<str>` snapshot，仅在发布帧构建；
- canonical backing `Arc<String>` 加 immutable range；
- chunk list/piece storage，在冻结或 terminal 时压平；
- 让 streaming-only VM 保存轻量 content handle，由 render projection 消费。

在没有 profiling 证明复制仍是主要热点前，不建议直接引入 rope；复杂数据结构会扩大 selection、copy、persistence 和生命周期风险。

### 3.3 Phase C：按稳定 Markdown block 缓存 parse/render 结果

### 当前问题

现有 `MarkdownRenderCache` 复用 conversion state，但每次仍然对完整 input 做 fence scan、image scan/replace 和 `rk_parse`。随后变化 VM 会重新生成完整 `Vec<Line>`。

### 建议模型

```text
MarkdownBubbleCache
├── stable_prefix_text/end_offset
├── stable_blocks
│   ├── rendered_lines: Arc<[Line]>
│   ├── local_wrap_map
│   └── visual_rows
└── unstable_tail
    ├── source range
    ├── parsed blocks
    ├── rendered lines
    └── local wrap map
```

每次尾部追加时：

1. 从最后稳定 block boundary 开始重建 parser 输入；
2. 只 parse 不稳定尾部；
3. 新形成稳定边界时，将对应 rendered block 冻结；
4. stable block 的 `Line` 和 wrap map 不再 clone/rebuild；
5. terminal flush 时执行完整 parse 对照或直接完整重建一次，保证最终一致。

### 稳定边界

可以沿用现有 `rollback_trailing_unstable` 的正确性原则，但需把“稳定”提升为显式 block cache 契约：

- 空行终止的普通 paragraph；
- 已闭合 fenced code block；
- 已结束 list block；
- 不再可能翻转为 table 的 paragraph；
- 不含待闭合 image syntax 的 block。

以下内容保守地留在 unstable tail：

- 未闭合 fence；
- 潜在 table header；
- 正在追加的 table；
- 未闭合 inline/link/image syntax；
- 最后一个未由明确 block boundary 终止的 paragraph。

### 代码高亮

未闭合 code block 不应每 chunk 对全部历史代码重新做 syntax highlighting。建议只高亮当前不稳定 code block，并限制单帧最大高亮输入；最终完成时再生成完整高亮结果。若存在超大单 code block，可能需要独立的 highlighter cache 或降级策略。

### 3.4 Phase D：增量维护 Line 和 wrap 信息

### 当前问题

变化 bubble 每次调用 `vm_to_lines_cached` 生成完整 `Vec<Line>`，并对全部逻辑行执行 `build_wrap_map`。

### 建议

将 wrap cache 与 Markdown block cache绑定：

- stable block 缓存 `Arc<[Line]>`、局部 wrap map 和 visual row count；
- 尾部追加只重新计算 unstable blocks；
- bubble 总高度为各 block `visual_rows` 之和；
- terminal width/theme/language 变化才使相关 block cache 整体失效；
- viewport 按 block 读取，不构造完整 bubble 的临时 `Vec<Line>`。

宽度变化仍需要重新 wrap，但不一定需要重新 Markdown parse。应区分：

```text
source/text version
parse version
style/theme version
layout/width version
```

避免终端 resize 导致无关 parser 工作。

### 3.5 Phase E：移除每帧全历史 concat_wrap_maps

### 当前问题

MessageArea 每帧把所有 VM slot 的局部 wrap map复制为一个扁平 map，即使绝大多数 slot 未变化。

### 建议

维护每个 slot：

```text
logical_line_count
visual_row_count
local_wrap_map
```

再维护 slot visual prefix sums：

```text
slot_visual_start[i] = Σ visual_row_count[0..i]
```

视觉坐标查找改为两级二分：

1. 在 slot prefix sums 中定位 slot；
2. 在该 slot 的 local wrap map 中定位逻辑行。

只有 slot 数量或某个 slot 高度变化时更新后续 prefix sums。首版也可每帧 O(VM) 重算 prefix sums，但不再 O(全部逻辑行) 分配和复制。

selection 与 copy 路径继续使用 `(slot_index, local_logical_idx)`，避免恢复全局扁平 line storage。

## 4. 不推荐方案

### 4.1 只切换 Markdown library

只要调用方式仍是“每 chunk 把完整 growing String 传给 parser 和 layout”，更换 parser 只能改变常数，不能消除累计二次工作。

### 4.2 默认改为 `streaming_mode = none`

它能止血，但失去产品需要的流式反馈，不应作为最终默认方案。

### 4.3 仅使用 `block` 模式

block 模式降低 publication 次数，但 canonical mutation 当前仍 eager `sync_cache`。而且 block boundary 检测使用字符偏移回扫，对超长 CJK 内容也需要优化为 byte offset 或增量 scanner state。

### 4.4 立即引入 rope

rope 可能减少部分 append/snapshot 成本，但不能自动解决 parser、Line、wrap 和 redraw 的全文工作。应先做 publication 合帧和 lazy projection，依据 profile 再决定是否需要复杂文本存储。

### 4.5 只优化 `concat_wrap_maps`

它主要改善长历史，无法解决单个 growing bubble 的全文复制、parse 和 wrap，优先级低于 Phase A–D。

## 5. 兼容性与正确性风险

### 5.1 最终 chunk 丢失

terminal event 必须同步 flush pending canonical state。需要覆盖：

- 正常 TurnDone；
- interrupted；
- failed；
- cancel；
- session load/reset/rewind；
- transport shutdown。

### 5.2 陈旧 timer 跨 session 发布

pending publication 必须绑定 session identity/epoch。session transition 后旧 timer 即使被调度也必须因 owner 校验失败而 no-op。

### 5.3 首 token 延迟

首 chunk 应立即发布，然后进入 frame window；不能所有 chunk 都等第一个 33–50ms deadline。

### 5.4 Markdown 中间状态与最终状态不一致

尾部增量 parser 可以保守失效，但不能错误复用。terminal 完整 parse 可作为 correctness barrier。table、image、fence 和 list lazy continuation 必须有针对性测试。

### 5.5 selection/scroll anchor 跳动

局部 wrap map 更新会改变尾部高度。auto-follow、手动滚动位置、selection anchor 和 copy button 坐标必须继续使用稳定的 slot-local 坐标，并测试 streaming 更新期间的行为。

### 5.6 动画刷新被误合并

running tool、subagent 和 reasoning animation 可能使用独立 heartbeat。stream publication scheduler 不应禁用必要动画，但可以共享统一 redraw request，避免同一时间窗口重复绘制。

## 6. 验证方案

## 6.1 正确性测试

现有 Markdown cached/full equivalence 测试继续作为基础。新增：

- 任意 chunk 切分下，terminal 输出等于一次性完整 parse；
- timer deadline 前多 chunk 只产生一次中间 publication；
- 首 chunk立即 publication；
- terminal event 强制 flush；
- session epoch 切换后旧 timer 不发布；
- block boundary 可提前发布且不重复发布；
- width/theme/language 失效只影响对应 cache 层；
- table/image/unclosed fence/list/CJK 与长无空格行；
- selection、copy、scroll anchor 和 auto-follow。

建议使用可控 fake clock，不让测试依赖真实 sleep。

## 6.2 性能 benchmark

建立不依赖真实 provider 和终端的 deterministic benchmark，至少分开测：

1. `CurrentTurn` chunk append 与 projection；
2. Markdown append parse/render；
3. bubble wrap update；
4. transcript slot aggregation；
5. end-to-end synthetic stream。

矩阵：

| 维度 | 建议样本 |
| --- | --- |
| 最终长度 | 64 KiB / 256 KiB / 1 MiB |
| chunk 大小 | 16 B / 128 B / 1 KiB / 一次性 |
| Markdown | prose / long line / fence / table / image-like |
| 历史规模 | 10 / 100 / 1000 VM |
| 宽度 | 40 / 80 / 160 columns |

关键指标：

- 总 wall time；
- publication/render count；
- full parse count；
- stable/unstable parsed bytes；
- copied bytes；
- wrap recalculated logical lines；
- allocation count/bytes。

不要只测 debug build；最终裁决以 release build 和真实 TUI profile 为准。

## 6.3 建议性能不变量

先通过 baseline 校准绝对阈值，再锁定以下趋势不变量：

- 固定 `L` 时，chunk 数增长不应导致 full parse 次数同步增长；
- 非 terminal publication 频率不超过 scheduler 上限；
- copied bytes 应接近“发布帧数 × 当前 snapshot”或更低，而不是“chunk 数 × 当前 prefix”；
- append 一个尾部 block 不重算稳定 prefix 的 wrap；
- 长历史每帧不得按全部逻辑行重新分配 aggregate map；
- terminal 最终结果必须与 full parse byte-for-byte 或结构等价。

## 7. 推荐实施顺序

1. **Instrumentation baseline**：补齐 chunk/publication、copied bytes、parse bytes、wrap lines 计数，使用 release profile 验证量级。
2. **Scheduler + lazy projection**：共同实施，切断 per-chunk snapshot/render。
3. **当前 bubble block cache**：消除稳定 prefix 的重复 parse/render/wrap。
4. **slot prefix sums**：消除全历史 wrap-map 扁平复制。
5. **再评估文本存储**：仅当 snapshot copy 仍是主要热点时考虑 `Arc` range、chunk storage 或 rope。

每一步都应保留前一步 benchmark，避免优化移动热点后仍沿用旧判断。

## 8. 方案裁决建议

建议批准以下目标语义：

- canonical stream ingest 不受 UI 帧率限制；
- UI 中间 publication 默认上限为 20–30 FPS；
- 首 chunk、重要 block boundary 和 terminal event 可立即 publication；
- owned ViewModel projection 只在 publication 边界构建；
- Markdown 最终状态以完整 parse 一致性为准；
- 渲染缓存以稳定 block/slot 为单位，避免全文扁平中间结构。

建议暂不裁决：

- 具体采用 20 FPS 还是 30 FPS；
- 是否引入 rope；
- 是否长期保留 terminal full parse；
- 超大代码块是否需要高亮降级阈值。

这些选择需要先由 baseline profile 和原型数据决定。

## 9. 实施与 profile 状态（2026-09-04）

已批准并完成默认 50 ms / 20 FPS 方案：canonical ingest 与 visual publication 解耦，主 Agent `Streaming` 使用 bridge-local single-pending fixed deadline，首 chunk、明确 block boundary 和 terminal 可提前；`Block` 与 `None` 保持兼容。`CurrentTurn` mutation 改为 lazy projection，tool/SubAgent/message interleave 与 SubAgent cadence 保持。

Phase E-1 已完成：`SlotIndex` 使用 per-slot logical/visual prefix 和 slot-local lookup，production 不再构建 transcript-wide aggregate wrap map；selection、semantic copy、hover/focus、entry click、scroll/anchor/auto-follow、footer 与 Unicode 路径迁移完成。

Release profile 对 Phase C+D 作保守触发，随后已实施 conservative stable rendered chunks、Line 与 local wrap-map 复用；terminal/frozen Markdown 继续以完整 parse 输出作为结构 oracle。table、image-like、list continuation、未闭合到闭合 fence、CJK、长无空格行及多种 chunk split/width 已覆盖。

条件项独立裁决：

| 条件 | 结果 | 产品动作 |
| --- | --- | --- |
| C+D | triggered | 已实施增量 Markdown materialization 与 stable wrap reuse |
| rope/storage | not triggered | 保留 `String`；lazy projection 已消除 chunk × growing-prefix copy amplifier |
| terminal full-parse removal | not triggered | 保留 correctness barrier |
| intermediate highlight fallback | not triggered | 保留完整 highlighting fidelity |

Deterministic release matrix 的 scheduler/projection 与 history 行已改为驱动真实 production 状态机和 `SlotIndex` counter seam；wall time 仅用于诊断，不作为 CI truth。本轮全量结果：`peri-tui` lib 1420 passed / 2 ignored，release harness 1 passed；all-targets clippy、workspace doc tests、format 与 diff check 均通过。完整 exit status 与收敛结果见 `.peri/adlc/tasks/2026-09-04-tui-long-markdown-streaming-cpu/artifacts/test-results/convergence-round1.md`。
