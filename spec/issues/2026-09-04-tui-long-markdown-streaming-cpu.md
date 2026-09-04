# TUI 超长 Markdown 流式输出导致 CPU 占用爆炸

**状态**：In verification（convergence round 1 已修复 review findings，待独立 round 2）
**优先级**：高
**类型**：性能 / TUI / Markdown / Streaming
**创建日期**：2026-09-04
**证据状态**：deterministic release matrix、增量 Markdown terminal oracle 与全量正确性测试已完成；wall time 仅作诊断

## 问题描述

Peri TUI 在 Assistant 持续流式输出超长 Markdown 时，CPU 占用会随当前回复长度和 chunk 数量显著增长。问题不是单一 Markdown parser 调用，而是流式事件频率与多个“针对增长中全文”的工作叠加：

```text
ACP TextChunk
→ CurrentTurn::append_text
→ CurrentTurn::sync_cache
→ push_view_models
→ VIEW_MODELS 更新
→ MessageArea render
→ parse_markdown_cached
→ build_wrap_map
→ concat_wrap_maps
→ viewport render
```

现有按 VM `content_hash` 分片缓存已经避免每个 chunk 重解析全部历史 Markdown，但单个正在增长的 Assistant bubble 仍会被重复复制、完整预处理/解析并重新折行。若最终文本长度为 `L`、平均 chunk 大小为 `C`，这些路径的累计工作量可接近 `Θ(L² / C)`；chunk 越碎，放大越明显。

## 用户影响

- 超长回复流式输出时，单个 TUI 进程可能长期占用大量 CPU。
- 文本越长，token/chunk 到画面的延迟越明显。
- 未闭合 fenced code block、表格、图片 Markdown 和长历史会话会进一步放大开销。
- 多 agent 或高 chunk 频率场景可能叠加为渲染风暴。
- 降低流式可见性可以缓解，但当前没有既保持低延迟又限制刷新频率的默认策略。

## 修复前已确认的代码事实

以下各节记录 Workflow 1 调研时的 pre-fix baseline，用于解释方案来源；它们不是当前实现描述。当前状态以 `docs/design/tui-streaming-markdown-performance.md`、`docs/code-index/peri-tui.md` 与源码为准。

### 1. 每个主 Agent text chunk 都同步构建 CurrentTurn 缓存

`CurrentTurn::append_text` 先增量追加到累积 `String`，随后立即调用 `sync_cache`：

- `peri-tui/src/kit/acp_types/current_turn.rs`：`CurrentTurn::append_text`
- `peri-tui/src/kit/acp_types/current_turn.rs`：`CurrentTurn::sync_cache`

`push_str` 和 rolling hash 本身是增量操作；问题在于 `sync_cache` 重建未冻结 trailing bubble 时，通过 `text_slice.to_string()` 创建当前 trailing 文本的 owned 快照。普通连续回复没有 tool/message boundary 时，这个 slice 通常覆盖当前回复的全部未冻结正文，因此每个 chunk 都会重新分配并复制增长中的全文。

这会产生累计 `Θ(Σ prefix_len_i)` 的复制量，并引入 allocator 和内存带宽压力。

### 2. 默认 Streaming 模式按每个 chunk 发布 ViewModel

- `peri-tui/src/kit/acp_events/mod.rs`：`StreamingMode`、`current_streaming_mode`
- `peri-tui/src/kit/acp_events/streaming.rs`：`handle_text_chunk`

默认值为 `StreamingMode::Streaming`。主 Agent 每个 text chunk 都调用 `push_view_models`，当前路径没有明确的 20–30 FPS 合帧上限。provider 输出越碎，projection、组件更新和渲染调用越频繁。

该行为是其余全文工作的重要频率放大器。

### 3. Markdown conversion 有增量缓存，但 parser 输入仍按全文处理

- `peri-tui/src/kit/markdown/mod.rs`：`parse_markdown_cached`
- `peri-tui/src/kit/markdown/mod.rs`：`ensure_closed_code_fences`

每次变化 bubble rebuild 时，`parse_markdown_cached` 仍对完整输入执行：

1. fenced code block 扫描；
2. 图片语法扫描和 placeholder 处理；
3. `ratatui_kit_markdown::parse_markdown`；
4. stable prefix 比较；
5. parsed block 到渲染 segment 的转换。

`MarkdownRenderCache` 复用的是稳定 block 的 conversion state，不能避免前面的全文预处理和 parser 调用。

特殊退化路径：

- 未闭合 fenced code block 会为补齐 closing fence 创建新的完整 `String`；
- 已处理 block 含图片时会回滚增量状态；
- table 或潜在 table header 会使稳定前缀复用失效。

### 4. 变化 bubble 每次重建全部 Line 和 wrap map

- `peri-tui/src/kit/message_area/mod.rs`：VM slot rebuild 阶段
- `peri-tui/src/kit/message_area/render.rs`：`vm_to_lines_cached`
- `peri-tui/src/kit/message_area/selection.rs`：`build_wrap_map`

按 VM 分片缓存可复用未变化的历史 slot，但正在增长的 bubble 每次仍会重新生成完整 `Vec<Line>`，随后遍历全部逻辑行，通过 `Paragraph::line_count` 重算视觉折行。

因此单个超长 bubble 仍存在“每 chunk × 当前 bubble 全部行”的累计成本。

### 5. 每帧仍扫描全部 VM 并拼接历史 wrap map

- `peri-tui/src/kit/message_area/mod.rs`：hash/rebuild detection 与 concat 阶段
- `peri-tui/src/kit/message_area/selection.rs`：`concat_wrap_maps`

每帧会扫描全部 VM 获取 hash/animation 状态，并将所有 slot 的 wrap map 复制为一个全局扁平 map。该路径与整个 transcript 的 VM 数量和逻辑行数成正比。

这通常不是单个超长 bubble 的第一根因，但会在长历史会话中放大每次流式刷新成本。

## 已排除或已缓解的旧根因

当前代码不支持以下笼统判断：

- `CurrentTurn.text` 每次追加本身会复制全文：不成立，累积字符串使用 `push_str`。
- 每个 chunk 对当前正文重新计算完整 hash：不成立，正文使用 rolling hash。
- 每个 chunk 深拷贝所有 committed ViewModel：不成立，`im::Vector` 提供结构共享。
- 每个 chunk 重解析和重折行所有历史消息：已由按 VM `content_hash` 分片缓存显著缓解。

准确的问题边界是：**单个正在增长的超长 bubble 仍然按 chunk 执行多项全文工作，长历史另有 per-frame 线性放大。**

## 复现建议

### 基准输入

使用脱敏、确定性生成的数据，分别测试：

1. 普通多段 prose；
2. 单个超长无空格行；
3. 长时间未闭合的 fenced code block；
4. 超长 Markdown table；
5. 高频 image-like syntax；
6. 相同当前回复叠加短历史与长历史。

### 控制变量

分别改变：

- 最终文本长度 `L`；
- chunk 数量和平均 chunk 大小 `C`；
- 历史 VM 数量 `M`；
- 终端宽度；
- `streaming_mode`：`streaming` / `block` / `none`；
- debug 与 release build。

必须比较“完整文本一次提交”和“相同文本分 chunk 提交”。如果总 CPU 随 chunk 数显著上升，即支持 per-chunk 全文处理和缺少合帧的判断。

### 观测手段

短时间启用：

```bash
PERI_RENDER_TIMING=1 cargo run -p peri-tui
```

观察 `hash+detect`、`rebuild`、`concat`、`viewport`、`frame-total`。诊断会逐帧记录 INFO，不应长期启用，否则日志本身会干扰数据。

同时使用 macOS Instruments 或 `sample` 获取 release build CPU 栈，并记录：

- 每秒 chunk 数；
- 每秒 ViewModel publication/render 次数；
- `sync_cache` 累计 copied bytes；
- Markdown parse 调用次数和耗时；
- `build_wrap_map` 调用次数和耗时；
- allocation count/bytes；
- 最终长度、chunk 分布、历史 VM 数和终端尺寸。

## 临时缓解

在 `PeriConfig.config.extra` 中设置：

```json
{
  "streaming_mode": "block"
}
```

可减少 ViewModel publication 和 render 次数，但不能消除 `append_text → sync_cache` 中的 trailing 全文复制。

若需要优先保证资源稳定，可临时设置：

```json
{
  "streaming_mode": "none"
}
```

这会牺牲中间流式可见性，并且同样不是完整修复。

## 建议修复方向

详细建议见 [TUI 超长 Markdown 流式渲染优化建议](2026-09-04-tui-long-markdown-streaming-cpu-recommendations.md)。

推荐顺序：

1. 将 chunk 累积与 UI projection 解耦，限制中间 publication/render 到 20–30 FPS；首 chunk与 terminal event 立即 flush。
2. `append_text` 只更新累积状态和 dirty 标志，`sync_cache` 延迟到真实 publication 边界。
3. 将当前 bubble 的 Markdown/Line/wrap 缓存细化到稳定 block 或 segment，只重算不稳定尾部。
4. 用 slot 视觉高度前缀和分层查找替代每帧 `concat_wrap_maps`。
5. 添加长度、chunk 数和历史规模三个维度的性能基准与回归预算。

## 验收标准

功能正确性：

- 首个可见 chunk 不产生明显额外延迟；
- 流式中间状态按目标帧率稳定更新；
- `TurnDone`、失败、中断和取消必须立即发布最终状态；
- Markdown 最终渲染与一次性完整解析一致；
- table、image、fenced code block、CJK 和宽度变化保持正确；
- scrolling、selection、copy button 和 auto-follow 行为不退化。

性能门槛应在实施前通过基线校准，至少覆盖：

- 固定最终文本长度时，增加 chunk 数不再近似线性增加完整 parse/wrap 次数；
- 流式 publication/render 频率不超过设定上限，terminal flush 除外；
- `sync_cache` copied bytes 不再接近 `Θ(Σ prefix_len_i)`；
- 当前 bubble 尾部追加不再触发全部稳定 block 的 layout；
- 长历史下 concat 阶段不再随全部逻辑行逐帧分配；
- release profile 中没有单一 TUI 渲染路径持续占满一个 CPU core。

## 验证记录

2026-09-04 执行现有正确性测试：

```text
cargo test -p peri-tui --lib markdown
95 passed

cargo test -p peri-tui --lib boundary_
12 passed
```

这些测试证明现有 Markdown cache 与 block-boundary 行为通过正确性检查，不构成性能达标证据。当前缺少覆盖超长流式文本的性能 benchmark。

## 相关记录

- `spec/issues/2026-07-17-message-area-render-stutter-long-conversation.md`：长对话消息区渲染卡顿。
- `spec/archive-issues/subagent/2026-08-02-multi-agent-concurrent-cpu-high.md`：多 agent 场景中的历史渲染风暴问题。
- `spec/issues/2026-07-05-ratatui-kit-markdown-migration.md`：Markdown 渲染管线迁移背景。

## 状态变更记录

| 日期 | 从 | 到 | 说明 |
| --- | --- | --- | --- |
| 2026-09-04 | — | Open | 静态确认 per-chunk growing-text 全文工作，等待 release runtime profiling 与方案裁决 |
| 2026-09-04 | Open | Implemented | WP-07 集成完成，后续独立 review 要求 convergence |
| 2026-09-04 | Implemented | In verification | convergence round 1 修复 scheduler lifecycle、failure terminal、reference/超长坐标、真实 release harness 与门禁问题；等待独立 round 2 |

## 修复记录

2026-09-04 Ultra-ADLC workflow-2 完成以下范围：

- bridge-local single-pending 50 ms scheduler；canonical ingest 不节流，首个主 Agent text/reasoning、Markdown boundary 和 terminal 可立即 publication；reset、session transition、receiver close 与 shutdown 有明确失效策略。
- `CurrentTurn` 采用 lazy projection；tool/SubAgent/message boundary 与 terminal 保持 segment order、archive/reset/note/hash/loading 语义，`Streaming`/`Block`/`None` 兼容。
- `MarkdownRenderCache` 与 `VmCacheSlot` 复用 conservative stable rendered chunks、Line 和 slot-local wrap map；frozen/terminal 保留 full parse correctness oracle。
- `SlotIndex` 以 O(VM) prefix + slot-local lookup 替代 production transcript-wide wrap-map flatten，覆盖 selection/copy/hover/focus/scroll/anchor/auto-follow/footer/Unicode。

Profile 裁决：

- Phase C+D：触发并已实施。
- rope/storage：未触发；1 MiB/16 B burst projection copied bytes 从 34,360,262,656 降至 1,048,592，publication/projection 为 2。
- terminal full-parse removal：未触发；保留最终正确性 barrier。
- intermediate highlight fallback：未触发；保留完整 fidelity。

验证状态（convergence round 1）：`cargo test -p peri-tui --lib` 为 1422 passed / 2 ignored；release harness 连续三次通过且 49 条结构化数据行 digest 均为 `d4b4df16a0eb847de9607bfc29fd9056e0b3832d8284d4b16e8201a3da5b2205`；`cargo check -p peri-tui`、`cargo clippy -p peri-tui --all-targets -- -D warnings`、workspace doc tests、format 与 diff check 通过。release harness 的 scheduler/projection、streaming parser/materializer/wrap 与 history 数值来自实际 production 路径；绝对 wall time 不作为完成门禁。

稳定设计见 `docs/design/tui-streaming-markdown-performance.md`；完整 convergence 命令、exit status 与偏差见 `.peri/adlc/tasks/2026-09-04-tui-long-markdown-streaming-cpu/artifacts/test-results/convergence-round1.md`。
