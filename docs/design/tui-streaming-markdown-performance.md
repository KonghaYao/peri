# TUI 流式 Markdown 性能设计

> 状态：现行设计

## Scope

本文定义 `peri-tui` 从 ACP 流式事件到消息区 Markdown 渲染的性能与正确性边界。ACP transport、Agent runtime 和 Markdown library 本身不在此设计范围。

## 数据接收与视觉发布

ACP chunk 必须立即、完整、有序地写入 `BridgeState::current_turn` 的 canonical state。视觉 publication 是派生行为，由 bridge-local scheduler 控制，不能反向限制接收。

默认 `Streaming` 模式采用单 pending、固定 50 ms cadence：每类主 Agent text/reasoning 的首个 chunk 立即发布，后续 chunk 合并到已有 deadline，新的 chunk 不延后该 deadline。明确 Markdown block boundary 可提前形成 publication barrier。`Block` 仅在边界发布，`None` 不发布中间主 Agent 内容。

Scheduler 与 `BridgeState` 由同一 bridge task 持有。reset、session transition、terminal、receiver close 和 shutdown 都必须使 pending deadline 失效；receiver close 可发布已接收但尚未投影的最终 canonical state，shutdown 不再写 UI。publication 前必须完成 session/reset 所有权检查，旧 deadline 不得覆盖新 session 或 terminal snapshot。

Tool 与 SubAgent boundary 保持消息顺序和既有可见性。SubAgent 在 `Streaming`/`Block` 下仍可立即发布，在 `None` 下跳过中间 publication；其 canonical mutation 同样使用 lazy projection。

## Lazy ViewModel projection

`CurrentTurn` mutation 只更新 canonical state、rolling hash 和 dirty 标记。Owned ViewModel projection 只在真实 publication、tool/SubAgent/message freeze、terminal correctness barrier 或明确读取时同步。单次 publication 只取得一次 current-turn projection并复用。

Terminal handler 保持各事件既有 archive、reset、note、hash、segment order 和 loading 退出语义。合帧不得伪造 terminal，也不得让旧 pending publication 在 terminal 后再次出现。

## 增量 Markdown 与最终 oracle

流式 Assistant bubble 使用既有 `MarkdownRenderCache` 的保守稳定前缀：只冻结明确闭合且不含高风险结构的 block，保存 immutable rendered chunks；table、image/reference-like、list-like 与未配对 fence 保留在 mutable tail。追加时只 parse/materialize mutable tail 和新稳定区域。

`VmCacheSlot` 复用稳定 chunk 的 `Line` 与 slot-local wrap map，并以稳定 chunk identity 和确定 offset 跳过不变 chunk 的 materialize/wrap。slot 内保留轻量 placeholder 以维持既有 logical index，`SlotLines` 以 chrome/tail slice 与 stable chunk `Arc` 组合，viewport/selection 按 part offset 取行，不再把稳定 `Line` 克隆回 contiguous slot。宽度或主题改变时允许重做 conversion/layout，但复用 retained parsed blocks；source replacement 完整失效。

冻结或 terminal bubble 必须通过完整输入 parse/render 形成 correctness barrier。最终结果应与相同完整输入的一次性 parse 在结构上等价；不得以中间帧优化牺牲 table、image、fence、list、CJK、长无空格行或 syntax highlighting fidelity。

## Transcript slot index

消息区每帧构建仅随 VM slot 数量增长的 logical/visual prefix index。Global visual 或 logical 坐标先定位 slot，再查询 slot-local lines/wrap map；production 不构建 transcript-wide aggregate wrap map。

Selection、semantic copy、hover/focus、entry click、scroll、anchor、auto-follow、viewport、footer 与 image 坐标均消费同一 slot index。计数使用 `usize` 和饱和边界；文本映射保持 Unicode 字符边界与终端显示宽度语义。

## 条件策略

当前设计保留 `String` canonical storage、terminal full parse 与完整 intermediate syntax highlighting。只有 release profile 证明对应路径成为主要热点，并具备独立正确性证据时，才可引入 rope/storage、移除 terminal full parse 或增加中间高亮 fallback。

## 验证

- `cargo test -p peri-tui --lib`
- `cargo check -p peri-tui`
- `cargo clippy -p peri-tui --all-targets -- -D warnings`
- `cargo test --workspace --doc`
- release synthetic matrix：长度、chunk size、Markdown shape、history slots 与 terminal oracle
- `cargo fmt --all --check`
- `git diff --check`

行为入口以 `docs/code-index/peri-tui.md` 和源码为准；实施/profile 状态保留在对应 active issue。
