# ratatui-kit 0.10.1 + ratatui-kit-markdown 迁移设计

**日期**：2026-07-09
**作者**：KonghaYao + Claude
**关联 issue**：`spec/issues/2026-07-05-ratatui-kit-markdown-migration.md`
**状态**：Draft（待 writing-plans 细化）

## 1. 背景与目标

将 peri-tui 从当前 fork（`KonghaYao/ratatui-kit@peri/deps`, v0.9.0+auto_quit patch）+ 自研 markdown 渲染管道（~2700 行）迁移到**官方 `ratatui-kit 0.10.1` + `ratatui-kit-markdown 0.2.0`**。

`ratatui-kit-markdown` 是 peri-tui 作者贡献的官方 contrib crate（PR yexiyue/ratatui-kit#12），现在以独立 crate 形式发布。本迁移用官方版替换 peri-tui 内的自研副本。

### 目标

- 净减 ~2700 行：删除 `render_bridge.rs` + `RENDER_CACHE` atom + `wrap_map` + `peri-widgets/markdown/` + `kit/markdown/`
- `VIEW_MODELS` atom 成为消息流唯一数据源
- 仅 `AssistantBubble` 使用 `Markdown` 组件；`UserBubble` 改为纯 Span
- `text_selection.rs` 保留代码但功能失效（后续独立 plan 用 ratatui-kit 原生能力补回）

### 非目标

- 不替换 `TuiDiffBlock`（保留自研纯 Span 拼接）
- 不在本次迁移中补回文本选区
- 不处理 `auto_quit_on_ctrl_c` 的上游 PR（由作者另行推动）

## 2. 架构变化

### 当前

```
ACP 事件 → VIEW_MODELS atom → render_bridge (独立 tokio task)
                                   │
                                   ├─ content_hash 增量检测
                                   ├─ 预计算 Vec<Line<'static>>
                                   ├─ 构建 wrap_map (WrappedLineInfo 视觉行映射)
                                   └─ 写入 RENDER_CACHE atom
                                   ↓
                        RENDER_CACHE atom (entries + cumulative_heights + wrap_map)
                                   ↓
                        message_area (Paragraph + wrap_map 二分视口裁剪)
```

### 目标

```
ACP 事件 → VIEW_MODELS atom → MessageArea (#[component])
                                   │ use_atom(VIEW_MODELS)
                                   ↓
                              ScrollView (ratatui-kit 原生视口裁剪)
                                   │ 遍历 committed + current_turn
                                   ↓
                              match variant → 各变体 #[component]
                                   ├─ UserBubble (纯 Span)
                                   ├─ AssistantBubble → Markdown(content) + ReasoningBlock
                                   ├─ ToolCard (纯 Text)
                                   ├─ SystemNote / SubAgentGroup / CollapsedGroup / ...
```

### 关键变化

1. **删除 `RENDER_CACHE` 中间层**——`VIEW_MODELS` 是唯一数据源
2. **删除 `render_bridge` 异步 task**——其异步能力被增强版 `Markdown` 组件的 `use_async_state` + `spawn_blocking` 替代（见 §3 Markdown 组件增强方案）
3. **视口裁剪交给 ratatui-kit `ScrollView`**——增强版 `Markdown` 组件通过 `use_previous_size().width` 在解析阶段已知终端宽度，`total_height` 精确（不再是近似值）
4. **删除 `RESIZE_TX` → `render_bridge` 通道**——终端宽度变化由 ratatui-kit 框架 `use_previous_size` 自动感知

## 3. 组件设计（变体级组件化）

每个 `ViewModel` 变体一个 `#[component]`，放在 `peri-tui/src/kit/bubbles/` 新目录。

| 组件 | Props | 子组件 | 说明 |
|------|-------|--------|------|
| `UserBubble` | `content: Arc<str>`, `reminder: Option<ReminderInfo>` | — | 纯 Span 拼接（删除 markdown 解析） |
| `AssistantBubble` | `content: Arc<str>`, `reasoning: Option<TuiReasoningBlock>` | `Markdown(content)`, `ReasoningBlock` | **唯一**用 `Markdown` 组件的地方 |
| `ToolCard` | `data: TuiToolCard` | — | 保留现有 `format_tool_name` 等纯函数 |
| `SystemNote` | `data: TuiSystemNote` | — | Info/Warning/Error 三级 |
| `SubAgentGroup` | `data: TuiSubAgentGroup` | 递归各变体 | 复用变体组件 |
| `CollapsedGroup` | `data: TuiCollapsedGroup`, `expanded: bool` | — | 折叠/展开 |
| `ReasoningBlock` | `text: Arc<str>`, `expanded: bool` | — | "Thought for N chars" |
| `AskUserBlock` / `DividerData` | 各自 DTO | — | 子类型 |

### `MessageArea` 父组件

- `use_atom(VIEW_MODELS)` 订阅
- 遍历 `committed.iter().chain(current_turn.iter())`
- `match vm.variant` 分发到对应子组件
- 外层 `ScrollView` 自动视口裁剪

### 保留的行为

- **智能跟随**：`current_turn` 非空时自动滚到底；用户主动上滚时不抢夺滚动位
- **Sticky Header + 滚动按钮**：当前用户消息摘要在顶部固定 + ▲/▼ 按钮
- **Todo 渲染**：`TODO_ITEMS` atom → ◼/✔/◻ 图标

### Markdown 组件增强方案（贡献回 `ratatui-kit-markdown`）

为确保行高精确 + 异步不阻塞 UI，对 `ratatui-kit-markdown` 的 `Markdown` 组件做两阶段异步改造（利用 ratatui-kit 0.10.1 原生 hooks 能力）：

```
第一帧（width 未知）:
  use_memo → light_parse(content)          # 仅切行、无 syntect 高亮，<1ms
  ↓ 作为 fallback 渲染

第二帧起（width 精确）:
  use_previous_size → width                # 上一帧的 Rect::width
  use_async_state(async {                  # 异步解析，不阻塞 UI
    spawn_blocking → parse_markdown(content, width, highlight=true)
  }, dep=(content, width))
  ↓ 解析完成 → 自动 re-render
  ↓ 此后 use_memo 命中缓存（同 content+width 不再解析）
```

**使用的 hooks**：
- `use_previous_size()` — 拿到组件的 Rect（width 已知，1 帧滞后）
- `use_async_state(F, (content, width))` — 异步解析，返回 data/loading/error 三态
- `use_memo(light_parse, (content, width))` — 同步 fallback（无高亮），解析中渲染

**关键约束**：
- 第一帧 `use_previous_size` 返回 `Rect::default()`（width=0），退到默认值（如 80）
- 第二帧 width 精确后，重新解析含高亮版本
- 视觉上 1-2 帧的"颜色闪现"（无高亮 → 高亮），可接受

**实施策略**：此增强以 PR 形式贡献到 `yexiyue/ratatui-kit-contrib`。Peri-tui 消费增强后的版本，不在 peri-tui 内自研新组件。

## 4. 数据流

### 不变

- ACP 事件 → `acp_notifier` → `acp_bridge` → `VIEW_MODELS` atom
- `BRIDGE_RESET_COUNTER`（/clear、thread 切换的桥梁重置）

### 删除

- `spawn_render_bridge` task 及其 supervisor（`entry.rs:164-178`）
- `render_bridge_tx` / `render_bridge_rx` 通道（`acp_notifier.rs` 全部分发分支）
- `RESIZE_TX` 通道（终端宽度变化由框架处理）
- `render_bridge.rs` 内的 `content_hash` 增量、`wrap_map` 构建、`cumulative_heights` 计算

### 新增

- `AppShell` 顶层包裹 `PaletteProvider(peri_palette)`——`Markdown` 组件 `use_component_theme::<MarkdownTheme>()` 自动派生

## 5. 主题迁移（9 种色值 → PaletteProvider）

将 `DefaultMarkdownTheme` 的 9 种硬编码色值映射到 ratatui-kit `Palette`，注入 `PaletteProvider`。

| 色值（#hex, RGB） | 当前用途 | Palette 槽位 |
|-------------------|----------|-------------|
| `#FFC107` (255,193,7) | heading | `Palette::warning` |
| `#FFFFFF` (255,255,255) | text / list_bullet | `Palette::text` |
| `#999999` (153,153,153) | muted / quote_prefix / separator | `Palette::muted` |
| `#A2A9E4` (162,169,228) | code | `Palette::info`（或扩展槽） |
| `#4EBA65` (78,186,101) | link / code_prefix | `Palette::success` |

新增 `peri-tui/src/kit/theme/markdown_palette.rs`：构造 `Palette` 实例 + 注入 `PaletteProvider`。

## 6. 错误处理

- **Markdown 解析**：`pulldown-cmark` 不会失败（流式 parser）；若 `Markdown` 组件渲染 panic，由 ratatui-kit 框架 panic → `panic_notify_rx` 接管（已有机制）
- **流式追加**：`current_turn` 增量更新时，`Markdown` 组件的 `use_memo(content)` 按 content 自动缓存——同一内容只解析一次
- **/clear**：直接清 `VIEW_MODELS` atom（不再需要 `RENDER_CACHE` 同步重置）
- **text_selection.rs 失效**：保留代码但加 `#[allow(dead_code)]`，因 `RENDER_CACHE` 删除后编译期会报未使用警告

## 7. 测试策略

### 保留/改造

- `kit::acp_events::tests` —— `drain_input_buffer` 等（不受影响）
- `kit::submit_consumer::tests` —— 移除 `RENDER_CACHE` 断言

### 新增

- `kit::bubbles::user_bubble::tests` —— 纯 Span 渲染、reminder 两行缩略
- `kit::bubbles::assistant_bubble::tests` —— Markdown 组件挂载、reasoning 集成
- `kit::bubbles::tool_card::tests` —— `format_tool_name`、折叠/展开
- `kit::message_area::tests` —— VIEW_MODELS 变化触发重渲染、ScrollView 视口、智能跟随

### 删除

- `kit::render_bridge::tests`（整个文件随 render_bridge 删除）
- `peri-widgets/src/markdown/` 下所有测试（随模块删除）

## 8. 文件变更清单

### 删除（17 文件）

- `peri-tui/src/kit/render_bridge.rs` (~353 行)
- `peri-tui/src/kit/markdown/mod.rs` (21 行)
- `peri-widgets/src/markdown/` (13 文件 ~2100 行)

### 修改（10 文件）

- `peri-tui/Cargo.toml` —— `ratatui-kit` 切官方 0.10.1，新增 `ratatui-kit-markdown = "0.2"`（features `markdown-highlight`）
- `peri-widgets/Cargo.toml` —— 移除 `markdown-highlight` feature + pulldown-cmark/syntect 依赖
- `peri-widgets/src/lib.rs` —— 移除 `pub mod markdown;`
- `peri-tui/src/kit/atoms.rs` —— 删除 `RENDER_CACHE` / `RESIZE_TX`
- `peri-tui/src/kit/entry.rs` —— 删除 `spawn_render_bridge` 及 supervisor
- `peri-tui/src/kit/acp_notifier.rs` —— 删除 `render_bridge_tx` 分支
- `peri-tui/src/kit/submit_consumer.rs` —— 删除 `RENDER_CACHE` 重置
- `peri-tui/src/kit/app_shell.rs` —— 包裹 `PaletteProvider`
- `peri-tui/src/kit/message_area.rs` —— 重写为 `#[component]` 组件树
- `peri-tui/src/kit/view_render.rs` —— 拆分纯函数到 `bubbles/` 各组件

### 新增

- `peri-tui/src/kit/bubbles/` 目录 + 各变体组件文件
- `peri-tui/src/kit/theme/markdown_palette.rs`

### 保留但失效

- `peri-tui/src/kit/text_selection.rs`（加 `#[allow(dead_code)]`）

## 9. 已知风险

### R1. 第一帧无高亮（已消解，可接受）

`use_previous_size` 第一帧返回 `Rect::default()`（width=0），退到默认宽度 + light parse（无 syntect 高亮）。第二帧 width 精确后触发 `use_async_state` 重解析含高亮版本。

**实际表现**：1-2 帧内完成 light → heavy 过渡（~30-60ms 内，取决于终端 resize 事件时序）。首次打开的消息有短暂的代码无高亮闪烁，后续 `use_memo` 缓存命中不再触发。

**对比当前**：当前 `render_bridge` 完全异步，解析完成前 UI 显示旧内容。新方案解析完成前渲染 light fallback（至少显示了内容），完成后自动升级到高亮版。**整体体验不退化**。

### R2. text_selection 功能丢失（接受，后续补回）

保留 `text_selection.rs` 代码但功能失效（鼠标 Drag 复制不可用）。

**原因**：`text_selection.rs` 依赖 `RENDER_CACHE.entries: Vec<Line>` + `wrap_map` 做终端坐标 → 视觉坐标 → 文本提取的映射。删除 `RENDER_CACHE` 后无数据可读。

**缓解**：加 `#[allow(dead_code)]` 避免编译警告。后续独立 plan 用 ratatui-kit 原生能力补回（待调研）。

### R3. `auto_quit_on_ctrl_c` 上游合并时机（外部依赖）

本设计假设官方 0.10.1 已支持禁用 Ctrl+C 默认退出。实际未合并前，项目仍需依赖 fork。

**缓解**：执行阶段由作者推动上游 PR 决定切换时机。代码改造不阻塞——可以在 fork 上完成所有 markdown 改造，等 PR 合并后再切官方依赖。

## 10. 与 2026-07-05 旧 issue 的差异

| 维度 | 2026-07-05 issue | 2026-07-09 spec |
|------|------------------|-----------------|
| ratatui-kit-markdown 来源 | ratatui-kit 内置（fork 上有） | 独立 contrib crate（官方 0.2.0） |
| text_selection | 删除 | 保留代码但失效 |
| TuiDiffBlock | 未提及 | 明确保留自研 |
| 主题迁移 | 未提及 | 明确迁移 9 种色值到 PaletteProvider |
| Markdown 组件增强 | 未提及 | 两阶段异步（贡献回 ratatui-kit-markdown） |
| 行高精度 | wrap_map（当前精确） | use_previous_size.width（增强后精确） |
| 异步渲染 | render_bridge（独立 task） | use_async_state + spawn_blocking（组件内） |
| 执行时序 | 单 plan | 等 auto_quit PR 后切官方 |
