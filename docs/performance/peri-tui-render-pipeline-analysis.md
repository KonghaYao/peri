# peri-tui 渲染管线性能分析报告

> 分析日期: 2026-07-13
> 仓库: `/Users/konghayao/code/ai/perihelion`

---

## 1. VIEW_MODELS 读写模式

### 写路径

**`push_view_models`** — `peri-tui/src/kit/acp_events.rs:831-869`

```
每次流式 chunk（TextChunk/ReasoningChunk/ToolStarted/ToolEnded 等）都会调用:
  1. state.committed.clone()           — im::Vector O(1) clone
  2. items.push_back(vm.clone())       — 每个 current_turn VM: O(log n) per push
  3. 反向遍历 items 折叠 reasoning     — O(n) 全量扫描
  4. *VIEW_MODELS.state().write() =    — 唤醒所有 5 个订阅者
```

**`VIEW_MODELS.state().write()` 调用点** — 15 处写入:

| 文件:行号 | 场景 | 频率 |
|-----------|------|------|
| `acp_events.rs:868` | 每次流式 chunk | **极高** (10-60Hz) |
| `acp_events.rs:878` | BRIDGE_RESET_COUNTER 复位 | 低频 |
| `acp_events.rs:1003/1157/1211` | AgentDone/Interrupted/Error | 每轮 1 次 |
| `acp_events.rs:1257/1302` | TurnDone / CompactComplete | 每轮 1 次 |
| `acp_events.rs:1392/1425/1467/1606/1634` | 各种 reset 路径 | 低频 |
| `submit_consumer.rs:486` | 提交新 prompt | 每轮 1 次 |
| `input_area.rs:1168` | 输入框重置 | 低频 |

> 🔴 **严重风险**: 流式期间 `push_view_models` 每次 chunk 都执行 O(n) 反向遍历折叠 reasoning block。n = committed + current_turn items。2000+ committed items 时，每个 text delta（≈每 50ms）就做一次 O(2000) 扫描。一天长对话可累积到 5000+ items，扫描成本线性增长。
>
> `peri-tui/src/kit/acp_events.rs:849-861`

### 读路径

**5 个 `use_atom(&VIEW_MODELS)` 订阅者**:

| 组件 | 文件:行号 | 读取内容 |
|------|----------|---------|
| `MessageArea` | `mod.rs:48` | `items.iter()` 全量遍历 → `vm_to_lines` |
| `AgentPanel` | `panels/agent.rs:43` | `items` 渲染 + SubAgent 嵌套 |
| `StatusPanel` | `panels/status.rs:38` | `view_count` 统计 |
| `TasksPanel` | `panels/tasks.rs:44` | 遍历查找 subagent groups |
| `WorkflowPanel` | `panels/workflow.rs:30` | 遍历查找 workflow 信息 |

另外:

| 文件:行号 | 方式 |
|-----------|------|
| `submit_consumer.rs:254` | `VIEW_MODELS.state().read().clone()` — 裸读 |

> 🟡 **中等风险**: 5 个订阅者全部在 atom 写入时唤醒。流式期间 10-60Hz 写入 × 5 订阅者 = 50-300 次 render body 调用/秒。不过非可见面板在 ratatui-kit 中如果被路由系统跳过渲染，成本较低。需确认隐藏面板是否真正跳过 render body。
>
> `peri-tui/src/kit/atoms.rs:51-64` (ViewModelsSnapshot 定义)

---

## 2. vm_to_lines 复杂度分析

### 调用链

**缓存未命中时** — `peri-tui/src/kit/message_area/mod.rs:94-111`:

```rust
for item in snapshot.items.iter() {       // O(N_items)
    lines.extend(vm_to_lines(item, props.width));
}
```

**`vm_to_lines` 内部** — `peri-tui/src/kit/message_area/render.rs:99-219`:

每个 VM 变体的成本不同：

| 变体 | 文件:行号 | 主要操作 | 复杂度 |
|------|----------|---------|--------|
| `TuiAssistantBubble` | `render.rs:101-140` | `parse_markdown()` + syntect syntax highlighting | O(text_len) + O(code_blocks × syntect) |
| `TuiUserBubble` | `render.rs:141-211` | `parse_markdown()` + span style patching | O(text_len) |
| `TuiToolCard` | `render.rs:212` | `render_tool_card_lines()` — 纯 span 构建 | O(1) |
| `TuiSystemNote` | `render.rs:213` | `render_system_note_lines()` | O(text_len) |
| `TuiSubAgentGroup` | `render.rs:214` | `render_subagent_group_lines()` — 递归渲染 children | O(children × child_cost) |
| `TuiCollapsedGroup` | `render.rs:215` | 折叠渲染 — 仅标题 | O(1) |
| `TuiDivider` | `render.rs:216` | 纯 span 构建 | O(1) |
| `TuiAskUserBlock` | `render.rs:217` | QA items 渲染 | O(items) |

> 🟡 **中等风险**: 单个 VM 的 `vm_to_lines` 是 O(vm_text_len)，不涉及跨 VM 的二次遍历。不是 O(N²)。
>
> 但 `parse_markdown()` (`peri-tui/src/kit/markdown/mod.rs:35`) 内部调用 `rk_parse` (pulldown-cmark) + syntect syntax highlighting for code blocks。大段 markdown 文本（如 LLM 回复的完整代码块）可能有数万字符，syntect 高亮是 O(text_len)。长对话累积了大量 AssistantBubble 时，cache miss 重建会很慢。
>
> **缓存在 generation 不变时不触发**，所以正常使用场景下此代码路径不执行。

### 实际场景评估

- **缓存命中** (generation 不变): 跳过整个 vm_to_lines → O(1)
- **缓存未命中** (generation 变化): O(Σ text_len_of_all_vms)

> 🟢 **良好实践**: `lines_cache` 使用 `(generation, width)` 作为 key，Drag 期间缓存命中率 100%，跳过 markdown 解析。`core_lines_arc` 使用 `Arc<Vec>` 而非 `Vec`，Drag 60-120Hz 下 Arc::clone 是 O(1)。
>
> `peri-tui/src/kit/message_area/mod.rs:64, 94-111`

---

## 3. wrap_map_cache 缓存策略

### 缓存结构

**`wrap_map_cache`** — `peri-tui/src/kit/message_area/mod.rs:136`:

```rust
hooks.use_state(|| (0u64, 0u16, Arc::<Vec<WrappedLineInfo>>::default()));
//                  ^generation ^width  ^Arc 包装的折行映射
```

**`WrappedLineInfo`** — `peri-tui/src/kit/message_area/selection.rs:15-19`:

```rust
struct WrappedLineInfo {
    logical_idx: usize,
    visual_start: usize,
    visual_end: usize,    // 区间 [visual_start, visual_end)
}
```

### 失效触发

**`mod.rs:158-178`**:
- 触发条件: `generation != vm_generation || vis_width != cached_width`
- 仅在 cache miss 时调用 `build_wrap_map()`

### 构建成本 — `build_wrap_map`

**`peri-tui/src/kit/message_area/selection.rs:23-38`**:

```rust
for (idx, line) in lines.iter().enumerate() {          // O(N_lines)
    let rows = Paragraph::new(Text::from(line.clone()))
        .wrap(Wrap { trim: false })
        .line_count(width);                             // O(line_len × unicode_width)
    wrap_map.push(WrappedLineInfo { ... });
}
```

每行调用 `paragraph.line_count(width)` — 内部做 unicode-width 计算 + word wrap 模拟。对于 N=10000 行，width=120 的典型场景，这是 ~1.2M 次 unicode-width 计算。

### 缓存命中率

| 场景 | 命中? | 说明 |
|------|-------|------|
| 流式追加 (generation 持续递增) | ❌ Miss | 每个 chunk generation 都变 |
| Drag 滚动 (generation 不变) | ✅ Hit | 典型场景下命中率 100% |
| Resize (width 变化) | ❌ Miss | 应避免频繁 resize |
| 只读浏览历史 | ✅ Hit | generation 不变 |

> 🔴 **严重风险**: **流式期间 generation 每个 chunk 都递增** (acp_events.rs:863)，导致 wrap_map_cache 每 chunk 都 miss，频繁重构建。10000 行 × per-line line_count(width) 开销显著。
>
> 但实际上 wrap_map 的重建是增量性的——流式期间 committed 不变，只有 current_turn 的几个 items 在变。`build_wrap_map` 对全量 core_lines_arc 调用，这是 O(全量) 而非 O(增量)。优化方向：**增量更新 wrap_map**，只重新计算 current_turn 对应的几行。
>
> `peri-tui/src/kit/message_area/selection.rs:23-38`

### 读取成本

`mod.rs` 中多次获取 wrap_map_cache.read():

| 行号 | 用途 | 频率 |
|------|------|------|
| `367` | 获取 `core_total_visual_rows` | 每帧 |
| `379` | `visual_to_logical` 选区转换 | 每帧（有选区时） |
| `401` | `viewport_logical_range` 视口计算 | 每帧 |
| `424` | clone Arc 用于循环 | 每帧（视口内有行时） |

> 🟡 **中等风险**: 每帧 3-4 次 `read()` 获取 RwLock read guard。虽然每次只持有锁极短时间，但如果同一帧内 `build_wrap_map` 正在进行 write，会有轻微的锁争用。

---

## 4. ScrollThrottle 16ms 节流

### 机制

**`peri-tui/src/kit/message_area/scroll.rs:24, 153-179`**:

```rust
const SCROLL_FRAME_MS: u64 = 16;  // ≈60fps

fn apply_scroll(delta: i32, scroll_throttle: &State<ScrollThrottle>,
                scroll_state: &State<ScrollViewState>) {
    let mut st = scroll_throttle.write_no_update();
    st.pending_delta += delta;
    let now = Instant::now();
    if now.duration_since(st.last_flush) >= Duration::from_millis(SCROLL_FRAME_MS) {
        let pending = st.pending_delta;
        st.pending_delta = 0;
        st.last_flush = now;
        drop(st);
        if pending != 0 {
            let mut state = scroll_state.write_no_update();
            if pending > 0 {
                for _ in 0..(pending as u16) { state.scroll_down(); }  // 逐次调用
            } else {
                for _ in 0..((-pending) as u16) { state.scroll_up(); }
            }
        }
    }
}
```

> 🟢 **良好实践**: 16ms 节流正确限制到 ≈60fps。`write_no_update` 避免自激回路。
>
> **但是**: `for _ in 0..(pending as u16) { state.scroll_down(); }` 使用逐次循环而非批量设置 offset。如果 pending 累积到 100 行，需要循环 100 次 `scroll_down()`。虽然单次 scroll_down 极廉价，但不应依赖此行为——应该直接 `set_offset`。

### 极限场景帧率分析 (1M+ 行消息)

节流本身不是瓶颈。**真正的瓶颈在渲染帧内**:

**每帧做的事情** (`mod.rs:329-477`):

1. **`viewport_logical_range`** — `selection.rs:63-84`
   - `wrap_map.iter().position(|e| e.visual_end > scroll_y)` — **线性扫描** O(n)
   - `wrap_map.iter().take_while(...).last()` — **从头线性扫描** O(end_idx)
   
2. **`visual_to_logical`** — `selection.rs:42-56`
   - `binary_search_by` — O(log n) ✅

3. **构建 viewport_lines** — `mod.rs:415-445`
   - clone ~60 行 — O(60) ✅

4. **`Paragraph::scroll` + `Paragraph::wrap`** — `mod.rs:471-474`
   - ratatui 渲染视口内 ~60 行

> 🔴 **严重风险**: **`viewport_logical_range` 的两次线性扫描是最大性能瓶颈**。
>
> `selection.rs:72` — `position()` 是 O(n) 线性扫描:
>   - 1M 行 → 最坏情况 1M 次比较
>   - 滚动到底部 → 扫描 ~1M 个 entries
>   - 60fps 下 → 每秒 60M 次比较（仅此操作）
>
> `selection.rs:77-82` — `take_while().last()` 从头扫描到 end_idx:
>   - 视口在底部时 → ~1M 个 entries
>   - 视口在顶部时 → ~60 个 entries
>
> **修复方案**: wrap_map 已按 `visual_start` 升序排列，`position()` 可替换为 `binary_search_by` 或 `partition_point`。`take_while().last()` 可从 `start_idx` 开始扫描。
>
> `peri-tui/src/kit/message_area/selection.rs:63-84`

### DragThrottle

**`scroll.rs:44-53`**:
```rust
struct DragThrottle { last_flush: Instant }
const SCROLL_FRAME_MS: u64 = 16;
```
> 🟢 **良好实践**: Drag 节流同样 16ms，与 scroll 共用常量。Drag 期间文本选区更新、highlight 重绘都受限于此节流。

---

## 5. message_area 视口裁剪逻辑

### 核心流程

**`peri-tui/src/kit/message_area/mod.rs:309-477`**:

```
1. clamp scroll_y → [0, max_scroll]                          (行 329-346)
2. 更新 scrollbar_fields                                       (行 348-361)
3. 计算 core_total_visual_rows (从 wrap_map 最后一条)          (行 365-369)
4. 计算选区 sel_bounds (binary_search visual_to_logical)       (行 376-396)
5. viewport_logical_range → [vp_core_start, vp_core_end]       (行 398-406)
6. 视口是否包含 footer?                                        (行 408-410)
7. clone + highlight 视口内 ~60 行                             (行 412-441)
8. extend footer_lines                                         (行 443-445)
9. Paragraph::scroll((vp_first_offset, 0))                    (行 471-474)
```

### 优点

> 🟢 **良好实践**: 
> - **只渲染视口内行**: clone 约 60 行而非全量 N 行，per-frame 成本 O(60) 而非 O(N)
> - **Arc<Vec> 缓存**: `core_lines_arc` 使用 Arc，视口行 clone 是 O(60) — 不触及全量数据
> - **scrollbar_fields用 write_no_update**: `mod.rs:355` — 不触发额外渲染
> - **Paragraph 内部 wrap**: ratatui 的 Wrap 仅对输入的 viewport_lines 做 word wrap，不对全量行
>
> `peri-tui/src/kit/message_area/mod.rs:415-474`

### 不足

> 🔴 **严重风险**: `viewport_logical_range` 线性扫描 (见 §4)。
>
> 🟡 **中等风险**: `core_total_visual_rows` 每次从 `wrap_map_cache.2.last().visual_end` 读取——依赖于 `build_wrap_map` 已正确更新。如果 footer 在 core 之后附加但 footer 的行高度被错误计入 core_total_visual_rows，会导致滚动范围计算偏差。
>
> `peri-tui/src/kit/message_area/mod.rs:365-369`

---

## 6. use_atom 订阅机制

### 订阅拓扑

```
VIEW_MODELS atom 写入 (push_view_models)
    ├── MessageArea (mod.rs:48)      — 全量 vm_to_lines 转换
    ├── AgentPanel   (agent.rs:43)   — SubAgent 渲染
    ├── StatusPanel  (status.rs:38)  — view_count 统计
    ├── TasksPanel   (tasks.rs:44)   — BG task 查找
    └── WorkflowPanel (workflow.rs:30) — workflow 查找
```

### 唤醒机制分析

ratatui-kit 中 `atom.write()` 触发 notifier.wake() → 所有订阅者 re-render。流式期间：

| 写入频率 | 订阅者数 | 总 render 调用/秒 |
|---------|---------|------------------|
| 10 Hz | 5 | 50 |
| 30 Hz (快速流式) | 5 | 150 |
| 60 Hz (理论极限) | 5 | 300 |

### 各订阅者的每帧成本

| 组件 | 是否始终可见 | 每帧成本 |
|------|------------|---------|
| `MessageArea` | 是 | lines_cache hit: O(1)；miss: O(N·M) |
| `AgentPanel` | 否 (按 Tab 切换) | 中等 (遍历 items 构建 SubAgent 树) |
| `StatusPanel` | 否 | 极低 (仅读取 view_count) |
| `TasksPanel` | 否 | 中等 (遍历查找 subagent groups) |
| `WorkflowPanel` | 否 | 中等 (遍历查找 workflow) |

> 🟡 **中等风险**: 5 个订阅者全部在每次流式 chunk 时唤醒。虽然非可见面板成本较低，但 ratatui-kit 仍会调用其 render body。若面板系统确实跳过隐藏面板的 body 执行，风险可降为 🟢。
>
> **优化方向**: 对仅需 view_count 的 StatusPanel，可引入独立的 `VIEW_COUNT` atom（从 push_view_models 同步更新），避免 StatusPanel 每次做无意义的 `hooks.use_atom` 触发。
>
> `peri-tui/src/kit/acp_events.rs:868` (写入点),
> `peri-tui/src/kit/panels/status.rs:38` (订阅点)

### use_effect 的依赖数组

**`mod.rs:261-281`**:
```rust
hooks.use_effect(
    { move || { scroll::run_auto_follow(&ctx) } },
    (items_len, vm_generation, is_loading, total_visual_rows),
);
```

> 🟢 **良好实践**: 依赖数组 `(items_len, vm_generation, is_loading, total_visual_rows)` 精确捕捉 auto-follow 需要响应的状态变化。流式期间 `vm_generation` 每个 chunk 都变，所以 effect 频繁触发——但这是 auto-follow 语义必需的正确行为。

---

## 7. hooks.use_* 顺序规则

### 当前 Hook 数量

**`MessageArea` 组件** — `mod.rs:47-281` 约 22 个 hooks:

| Hook 类型 | 数量 | 行号 |
|-----------|------|------|
| `use_atom` | 5 | 48-51, 255-256 |
| `use_state` | 12 | 64, 71, 124-136, 252-260 |
| `use_hook` | 1 | 139, 143 |
| `use_event_handler` | 1 | 233 |
| `use_effect` | 1 | 261 |

### 顺序规则开销

ratatui-kit 使用 hook slot 数组，每次 render 按调用顺序遍历。22 个 hooks 的数组查找是 O(22) → 可忽略。

> 🟢 **良好实践**: Hook 数量合理，未引入性能问题。
>
> **需注意**: `use_event_handler` 闭包 (`mod.rs:233-248`) 使用 `move` 捕获了 12 个 State 引用。每次 render 时 closure 重建，但 `move` 语义下 State 是 Arc clone，O(12) 个 Arc::clone → 可忽略。

### hidden trap

> 🟡 **中等风险**: `write_no_update` 语义依赖——如果在 render body 中误用 `.write()` 而非 `.write_no_update()`，会触发 notifier.wake() → 无限自激回路。代码中已有多处 `[TRAP]` 注释标明，但新开发者容易踩坑。
>
> `peri-tui/src/kit/message_area/mod.rs:88-89` (自激回路陷阱说明)

---

## 8. TuiRenderUnit PartialEq 分析

### 实现方式

**`peri-tui/src/kit/tui_render_unit.rs:22-31`** — `tui_impl_partial_eq!` 宏:

```rust
macro_rules! tui_impl_partial_eq {
    ($ty:ty: $($field:ident),+ $(,)?) => {
        impl PartialEq for $ty {
            fn eq(&self, other: &Self) -> bool {
                $(self.$field == other.$field)&&+
            }
        }
    };
}
```

### 各变体 PartialEq 覆盖字段

| 变体 | 宏调用位置 | 比较字段 | 排除字段 |
|------|-----------|---------|---------|
| `TuiUserBubble` | `:131` | `text, reminder` | `content_hash` ✅ |
| `TuiAssistantBubble` | `:147` | `text, reasoning` | `content_hash` ✅ |
| `TuiToolCard` | `:174` | `tool_id, tool_name, input_summary, output_summary, is_error, is_running, running_duration_ms, diff` | `content_hash, tool_calls_count` ✅ |
| `TuiSystemNote` | `:185` | `text, level` | `content_hash` ✅ |
| `TuiSubAgentGroup` | `:212` | `agent_id, agent_name, view_models, collapsed, is_running` | `content_hash` ✅ |
| `TuiCollapsedGroup` | `:226` | `title, count, view_models` | `content_hash` ✅ |
| `TuiDivider` | `:237` | `label` | `content_hash` ✅ |
| `TuiAskUserBlock` | `:250` | `items, is_error` | `content_hash` ✅ |

### 顶层 enum

`TuiRenderUnit` 本身 `#[derive(PartialEq)]` — `:38`，委托给各变体。

### 递归比较成本

> 🟡 **中等风险**: `TuiSubAgentGroup.view_models: im::Vector<TuiRenderUnit>` 参与 PartialEq 比较。SubAgent 的 children 也是 `im::Vector`。深度嵌套的 SubAgent 组（如 coder → subagent → subagent）会导致递归比较全部子元素。如果 PartialEq 被频繁调用（如 change detection），这是 O(total_children) 的成本。
>
> **但是**: 当前代码中**没有热路径**使用 TuiRenderUnit 的 PartialEq。`lines_cache` 使用 `generation` (u64) 判断，`wrap_map_cache` 使用 `(generation, width)` 判断。`push_acp_state` 比较的是 `AcpStateSnapshot`（不含 TuiRenderUnit）。**PartialEq 实际仅在 push_acp_state 的 `if *acp != snapshot` 路径调用，snapshot 是 AcpStateSnapshot，不是 TuiRenderUnit。**
>
> 降级为 🟢 — PartialEq 在热路径中未被使用。
>
> `peri-tui/src/kit/tui_render_unit.rs:38, 212`
>
> 🟢 **良好实践**: `content_hash` 字段被正确排除出 PartialEq——它是缓存衍生值，不应参与等价判断。

---

## 汇总

### 🔴 严重风险 (3 项)

| # | 问题 | 位置 | 影响 |
|---|------|------|------|
| 1 | **`viewport_logical_range` 线性扫描 wrap_map** | `selection.rs:63-84` | 1M+ 行时每帧 O(n) 扫描，60fps 不可保证。`position()` 和 `take_while().last()` 都应改为二分查找 / 从 start_idx 开始 |
| 2 | **`push_view_models` 反向遍历折叠 reasoning** | `acp_events.rs:849-861` | 每个流式 chunk 都 O(n) 扫描全量 items。5000+ items 时每个 text delta 做 5000 次比较 |
| 3 | **流式期间 wrap_map_cache 每 chunk miss** | `mod.rs:158-178` + `selection.rs:23-38` | generation 每个 chunk 递增 → `build_wrap_map` 频繁重构建（O(N × line_count(width))）。应改为增量更新 |

### 🟡 中等风险 (5 项)

| # | 问题 | 位置 | 影响 |
|---|------|------|------|
| 4 | **流式 chunk 5 订阅者全部唤醒** | `acp_events.rs:868` + 各面板 | 10-60Hz × 5 = 50-300 render/秒 |
| 5 | **apply_scroll 逐次循环 scroll_down** | `scroll.rs:169-175` | 大量累积时多次调用，应直接 set_offset |
| 6 | **流式期间 per-chunk 全量 lines_cache 重建** | `mod.rs:102-105` | generation 变化 → `vm_to_lines` × N_items → markdown+syntect 全量重算 |
| 7 | **wrap_map_cache 每帧 3-4 次 read lock** | `mod.rs:367,379,401,424` | 轻微锁争用，非主要瓶颈 |
| 8 | **TuiSubAgentGroup PartialEq 递归比较** | `tui_render_unit.rs:212` | 潜在 O(children) 成本，但热路径未使用 |

### 🟢 良好实践 (7 项)

| # | 实践 | 位置 |
|---|------|------|
| 9 | `lines_cache` 用 Arc<Vec> 避免 Drag 期间深拷贝 | `mod.rs:64, 91-111` |
| 10 | `wrap_map_cache` 用 Arc<Vec> + write_no_update | `mod.rs:136, 166-175` |
| 11 | `total_rows_cache` 多 key 缓存 line_count | `mod.rs:71, 198-229` |
| 12 | `ScrollThrottle` 16ms 节流 ≈60fps | `scroll.rs:24, 153-179` |
| 13 | DragThrottle 同步采用 16ms 节流 | `scroll.rs:44-53` |
| 14 | 视口裁剪仅 clone ~60 行 | `mod.rs:415-445` |
| 15 | `content_hash` 正确排除出 PartialEq | `tui_render_unit.rs:22-31` |

---

## 推荐修复优先级

### P0 — 修复 viewport_logical_range 线性扫描

**文件**: `peri-tui/src/kit/message_area/selection.rs:63-84`

```rust
// 当前 (线性 O(n)):
let start_idx = wrap_map.iter().position(|e| e.visual_end > scroll_y)?;

// 应改为 (O(log n)):
let start_idx = wrap_map.partition_point(|e| e.visual_end <= scroll_y);
// 或 binary_search_by
```

`take_while().last()` 应从 `start_idx` 开始而非从头:

```rust
let end_logical = wrap_map[start_idx..]
    .iter()
    .take_while(|e| e.visual_start < vp_visual_end)
    .last()
    ...
```

预计收益: 1M 行场景下每帧从 1M 次比较降至 ~20 次比较 (log₂(1M) + viewport_range)。

### P1 — 优化 push_view_models 反向遍历

**文件**: `peri-tui/src/kit/acp_events.rs:849-861`

只遍历 current_turn 的 assistant bubbles（而非全量 items），或延迟折叠到 generation 写入时统一处理。

### P2 — wrap_map 增量更新

**文件**: `peri-tui/src/kit/message_area/selection.rs:23-38`

流式期间只重新计算 current_turn 对应的最后几条 lines，而非全量 N 条。
