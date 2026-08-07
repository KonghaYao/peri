# Agent/Shell 工具运行计时和 tool calls 计数显示改进 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 Shell 计时丢秒数，为 Agent 工具卡片增加 tool calls 计数和运行时长行，删除 SubAgent 组的 ❯ 箭头头行。

**Architecture:** 所有改动集中在 `peri-tui/src/kit/view_render.rs`。`render_tool_card` 增加 Agent 工具的运行行渲染（仿 Bash Running 行），`render_subagent_group` 删除头行构建逻辑。`format_running_duration` 格式修正。`SubAgentRenderInfo` 扩展 `tool_calls_count` 字段。

**Tech Stack:** Rust, ratatui text primitives (Line/Span/Style), 纯函数渲染（零副作用）

---

### Task 1: 修正 `format_running_duration` 格式

**Files:**
- Modify: `peri-tui/src/kit/view_render.rs:330-337`

- [ ] **Step 1: 修改 `format_running_duration` 函数**

```rust
fn format_running_duration(ms: u64) -> String {
    let secs = ms / 1000;
    let mins = secs / 60;
    let secs = secs % 60;
    if mins > 0 {
        format!("{}min {}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}
```

旧的 `Xmin` 格式丢失秒数（85 秒显示 `1min`），新格式 `Xmin Xs` 保留秒精度。

- [ ] **Step 2: 运行现有测试验证格式变更**

```bash
cargo test -p peri-tui --lib -- test_tool_card_bash_running_shows_elapsed_line
```

这个测试断言了 `"Running (1min)"`（`running_duration_ms: Some(61_000)`），现在会失败——需要更新断言。

- [ ] **Step 3: 更新 Bash 运行测试断言**

**`test_tool_card_bash_running_shows_elapsed_line`（第 1000 行）**：

```rust
// 旧断言
assert!(
    text.contains("⎿ Running (1min)"),
    "运行中 Bash 应显示耗时行：{}",
    text
);

// 新断言
assert!(
    text.contains("⎿ Running (1min 1s)"),
    "运行中 Bash 应显示耗时行（含秒数）：{}",
    text
);
```

- [ ] **Step 4: 运行测试确认通过**

```bash
cargo test -p peri-tui --lib -- test_tool_card_bash_running_shows_elapsed_line
```
Expected: PASS

- [ ] **Step 5: 新增边界测试 `test_format_running_duration`**

在 `view_render.rs` 的 `mod tests` 中新增：

```rust
#[test]
fn test_format_running_duration_seconds_only() {
    // < 60s 只显示秒
    assert_eq!(format_running_duration(0), "0s");
    assert_eq!(format_running_duration(45_000), "45s");
    assert_eq!(format_running_duration(59_000), "59s");
}

#[test]
fn test_format_running_duration_minutes_and_seconds() {
    // >= 60s 显示 "Xmin Ys"
    assert_eq!(format_running_duration(60_000), "1min 0s");
    assert_eq!(format_running_duration(61_000), "1min 1s");
    assert_eq!(format_running_duration(85_000), "1min 25s");
    assert_eq!(format_running_duration(3_600_000), "60min 0s");
}
```

- [ ] **Step 6: 运行新测试**

```bash
cargo test -p peri-tui --lib -- test_format_running_duration
```
Expected: PASS

- [ ] **Step 7: 运行全套 view_render 测试**

```bash
cargo test -p peri-tui --lib -- view_render
```
Expected: ALL PASS

- [ ] **Step 8: Commit**

```bash
git add peri-tui/src/kit/view_render.rs
git commit -m "fix(tui): format_running_duration now shows seconds after minutes (1min 23s instead of 1min)"
```

---

### Task 2: `render_tool_card` 为 Agent 工具增加运行行

**Files:**
- Modify: `peri-tui/src/kit/view_render.rs:253-262` (在 Bash running 行之后新增 Agent running 行)
- Modify: `peri-tui/src/kit/view_render.rs:32-45` (`SubAgentRenderInfo` 扩展)

- [ ] **Step 1: 扩展 `SubAgentRenderInfo`**

在 `SubAgentRenderInfo` 结构体中新增 `tool_calls_count` 字段：

```rust
#[derive(Clone, Debug, Default)]
pub struct SubAgentRenderInfo {
    pub is_running: bool,
    pub is_error: bool,
    pub total_steps: usize,
    pub final_result: Option<String>,
    pub recent_messages: Vec<TuiRenderUnit>,
    /// SubAgent 内部工具调用总数（用于 Agent ToolCard running 行显示）。
    pub tool_calls_count: usize,
}
```

`SubAgentRenderInfo` 派生 `Default`，`tool_calls_count` 默认为 `0`。

- [ ] **Step 2: 在 `render_tool_card` 中增加 Agent running 行**

在现有 Bash running 行代码块之后（约第 262 行），紧接 `}` 和 `}` 之后插入：

```rust
    // Bash running 行（现有，保留不变）
    if data.tool_name == "Bash" && data.is_running && !data.is_error {
        let duration = data.running_duration_ms.unwrap_or(0);
        lines.push(Line::from(vec![
            Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim)),
            Span::styled(
                format!("Running ({})", format_running_duration(duration)),
                Style::default().fg(semantic.text.muted),
            ),
        ]));
    }

    // 新增：Agent running 行
    if data.tool_name == "Agent" && data.is_running && !data.is_error {
        let duration = data.running_duration_ms.unwrap_or(0);
        let tool_count = lookup_subagent_status(&data.tool_id)
            .map(|s| s.tool_calls_count)
            .unwrap_or(0);
        if tool_count > 0 {
            lines.push(Line::from(vec![
                Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim)),
                Span::styled(
                    format!(
                        "{} tool calls, running {}",
                        tool_count,
                        format_running_duration(duration)
                    ),
                    Style::default().fg(semantic.text.muted),
                ),
            ]));
        }
    }
```

注意：通过 `data.tool_id` 查找 `lookup_subagent_status`。SubAgent 的 `agent_id` 与 Agent 工具调用的 `tool_id` 需要对应——需确认 ACP 层 `SubAgentAccumulator` 的 `agent_id` 是否与 Agent ToolCard 的 `tool_id` 使用了相同值。

- [ ] **Step 3: 新增测试 `test_tool_card_agent_running_shows_tool_calls_and_duration`**

```rust
#[test]
fn test_tool_card_agent_running_shows_tool_calls_and_duration() {
    RENDER_CALL_COUNT.with(|c| c.store(0, Ordering::Relaxed));

    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "agent-tc-1".into(),
        tool_name: "Agent".into(),
        input_summary: "search rust patterns".into(),
        output_summary: String::new(),
        is_error: false,
        is_running: true,
        running_duration_ms: Some(85_000),
        diff: None,
        content_hash: 0,
    });
    let probe = std::rc::Rc::new(StaticProbe {
        info: Some(SubAgentRenderInfo {
            is_running: true,
            is_error: false,
            total_steps: 3,
            final_result: None,
            recent_messages: Vec::new(),
            tool_calls_count: 5,
        }),
    });
    let lines = with_status_probe(probe, || render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        text.contains("● Agent (search rust patterns)"),
        "Agent 工具卡应使用 ● 原点前缀：{}",
        text
    );
    assert!(
        text.contains("⎿ 5 tool calls, running 1min 25s"),
        "Agent 运行行应显示 tool calls 计数和时长：{}",
        text
    );
}
```

- [ ] **Step 4: 新增测试 `test_tool_card_agent_not_running_shows_no_running_line`**

```rust
#[test]
fn test_tool_card_agent_not_running_shows_no_running_line() {
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "agent-tc-done".into(),
        tool_name: "Agent".into(),
        input_summary: "search done".into(),
        output_summary: "found matches".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        content_hash: 0,
    });
    let lines = render_v2_vm(&vm, 80);
    let text = collect_text(&lines);
    assert!(
        !text.contains("tool calls"),
        "Agent 完成态不应显示 running 行：{}",
        text
    );
}
```

- [ ] **Step 5: 运行新增测试**

```bash
cargo test -p peri-tui --lib -- test_tool_card_agent
```
Expected: 2 PASS

- [ ] **Step 6: 运行全套 view_render 测试**

```bash
cargo test -p peri-tui --lib -- view_render
```
Expected: ALL PASS (包括 Task 1 更新后的测试)

- [ ] **Step 7: Commit**

```bash
git add peri-tui/src/kit/view_render.rs
git commit -m "feat(tui): Agent ToolCard shows tool calls count and running duration"
```

---

### Task 3: 删除 `render_subagent_group` 的 ❯ 箭头头行

**Files:**
- Modify: `peri-tui/src/kit/view_render.rs:540-607` (删除头行构建逻辑)

- [ ] **Step 1: 删除 `render_subagent_group` 头行**

删除从 `let agent_color = ...`（第 548 行）到 `let mut lines = vec![Line::from(header_spans)];`（第 607 行）之间的全部代码。

修改后的函数开头：

```rust
fn render_subagent_group(data: &TuiSubAgentGroup, width: usize) -> Vec<Line<'static>> {
    let semantic = theme::semantic();

    // 查询运行时状态（v2 DTO 缺失字段由 status probe 注入）
    let status = lookup_subagent_status(&data.agent_id);

    // 删除: ❯ Agent(agent_id) name · ⏳ 头行构建逻辑
    // SubAgent 组不再有独立头行——其 tool calls 计数和时长由
    // render_tool_card 中 Agent 工具卡片的 "⎿ n tool calls, running Xmin Xs" 行展示。

    let mut lines: Vec<Line<'static>> = Vec::new();

    // 子内容来源优先级：
    // 1. v2 DTO `view_models`（ACP 层填充）
    // 2. status probe 的 `recent_messages`（app 层填充）
    let children: Vec<TuiRenderUnit> = if !data.view_models.is_empty() {
        data.view_models.iter().cloned().collect()
    } else if let Some(ref s) = status {
        s.recent_messages.clone()
    } else {
        Vec::new()
    };

    // 后面折叠摘要、children 遍历、final_result 预览逻辑保持不变...
```

保留折叠摘要（`collapse_count`）、children 遍历（`for inner_vm in &children`）、final_result 预览的全部现有逻辑。

- [ ] **Step 2: 更新 `test_subagent_group_always_shows_content`（第 1297 行）**

旧的断言 `text.contains("Agent(sa-1) file-searcher")` 不再成立——头行已被删除。更新为验证子内容存在：

```rust
fn test_subagent_group_always_shows_content() {
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "sa-1".into(),
        agent_name: "file-searcher".into(),
        view_models: im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble {
            text: "find foo".into(),
            content_hash: 0,
            reminder: None,
        })]),
        collapsed: true,
        is_running: false,
        content_hash: 0,
    });
    let lines = render_v2_vm(&vm, 80);
    let text = collect_text(&lines);
    // 头行已删除——不再有 "Agent(sa-1)" 文本
    assert!(
        !text.contains("Agent(sa-1)"),
        "SubAgent 组不再渲染 ❯ 头行：{}",
        text
    );
    assert!(
        text.contains("find foo"),
        "子内容应始终可见：{}",
        text
    );
}
```

- [ ] **Step 3: 更新 `test_subagent_group_with_running_probe_shows_status_icon`（第 1409 行）**

旧断言验证 `· ⏳` 和 `Agent(fork)`——头行已删除，这些不再存在。更新为验证头行不存在：

```rust
fn test_subagent_group_with_running_probe_shows_status_icon() {
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "fork".into(),
        agent_name: "Agent".into(),
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: false,
        content_hash: 0,
    });
    let probe = std::rc::Rc::new(StaticProbe {
        info: Some(SubAgentRenderInfo {
            is_running: true,
            is_error: false,
            total_steps: 3,
            final_result: None,
            recent_messages: Vec::new(),
            tool_calls_count: 0,
        }),
    });
    let lines = with_status_probe(probe, || render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        !text.contains("❯"),
        "SubAgent 组不再渲染 ❯ 箭头头行：{}",
        text
    );
    assert!(
        !text.contains("· ⏳"),
        "运行状态指示器已随头行删除：{}",
        text
    );
}
```

- [ ] **Step 4: 更新 `test_subagent_group_with_done_probe_shows_final_result`（第 1434 行）**

移除对 `Agent(sa-result)` 头行文本的依赖。验证聚焦于 final_result 渲染：

```rust
fn test_subagent_group_with_done_probe_shows_final_result() {
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "sa-result".into(),
        agent_name: "result".into(),
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: false,
        content_hash: 0,
    });
    let probe = std::rc::Rc::new(StaticProbe {
        info: Some(SubAgentRenderInfo {
            is_running: false,
            is_error: false,
            total_steps: 1,
            final_result: Some("task completed successfully".into()),
            recent_messages: Vec::new(),
            tool_calls_count: 0,
        }),
    });
    let lines = with_status_probe(probe, || render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        !text.contains("❯"),
        "SubAgent 组不再渲染 ❯ 箭头头行：{}",
        text
    );
    assert!(
        text.contains("task completed successfully"),
        "最终结果应显示：{}",
        text
    );
}
```

- [ ] **Step 5: 更新 `test_subagent_group_with_error_probe_shows_failed`（第 1463 行）**

同理，移除头行断言：

```rust
fn test_subagent_group_with_error_probe_shows_failed() {
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "sa-err".into(),
        agent_name: "error".into(),
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: false,
        content_hash: 0,
    });
    let probe = std::rc::Rc::new(StaticProbe {
        info: Some(SubAgentRenderInfo {
            is_running: false,
            is_error: true,
            total_steps: 0,
            final_result: Some("something went wrong".into()),
            recent_messages: Vec::new(),
            tool_calls_count: 0,
        }),
    });
    let lines = with_status_probe(probe, || render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        !text.contains("❯"),
        "SubAgent 组不再渲染 ❯ 箭头头行：{}",
        text
    );
    assert!(
        !text.contains("· ❌"),
        "错误指示器已随头行删除：{}",
        text
    );
    assert!(
        text.contains("something went wrong"),
        "错误结果应显示：{}",
        text
    );
}
```

- [ ] **Step 6: 更新 `test_subagent_group_without_probe_shows_success_icon_for_committed_placeholder`（第 1488 行）**

```rust
fn test_subagent_group_without_probe_shows_success_icon_for_committed_placeholder() {
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "sa-committed".into(),
        agent_name: "committed".into(),
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: false,
        content_hash: 0,
    });
    let lines = render_v2_vm(&vm, 80);
    let text = collect_text(&lines);
    assert!(
        !text.contains("❯"),
        "SubAgent 组不再渲染 ❯ 箭头头行：{}",
        text
    );
}
```

- [ ] **Step 7: 更新 `test_subagent_group_streaming_dto_shows_running`（第 1504 行）**

```rust
fn test_subagent_group_streaming_dto_shows_running() {
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "sa-stream".into(),
        agent_name: "streaming".into(),
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: true,
        content_hash: 0,
    });
    let lines = render_v2_vm(&vm, 80);
    let text = collect_text(&lines);
    assert!(
        !text.contains("❯"),
        "SubAgent 组不再渲染 ❯ 箭头头行：{}",
        text
    );
}
```

- [ ] **Step 8: 更新 collapsed summary 测试（第 1695 行）**

`test_subagent_group_collapsed_summary_replaces_hard_truncation` 涉及 SubAgentGroup 头行中的 "N collapsed tools"。检查是否需要更新——如果该测试仅验证折叠摘要行（`▶ N collapsed tools`），则可能不需修改。如果包含了对头行的断言，则更新。

- [ ] **Step 9: 更新 `StaticProbe` 辅助结构（测试模块内）**

在 `StaticProbe` 构造函数中，`SubAgentRenderInfo` 的所有创建处补上 `tool_calls_count: 0`（`Default` 自动为 `0`）。如有显式构造 `SubAgentRenderInfo` 而未列全字段的地方，补全。

- [ ] **Step 10: 运行全套 view_render 测试**

```bash
cargo test -p peri-tui --lib -- view_render
```
Expected: ALL PASS

- [ ] **Step 11: Commit**

```bash
git add peri-tui/src/kit/view_render.rs
git commit -m "refactor(tui): remove ❯ Agent header from render_subagent_group"
```

---

### Task 4: 编译检查与集成验证

- [ ] **Step 1: 全编译检查**

```bash
cargo build -p peri-tui
```
Expected: 编译通过，无 warning

- [ ] **Step 2: 全量 peri-tui 测试**

```bash
cargo test -p peri-tui --lib
```
Expected: 全部通过（或确认无回归的已知 failure）

- [ ] **Step 3: Commit（如有遗漏改动）**

---

### 自检（Self-Review）

1. **Spec 覆盖**：三个问题均有对应 Task——问题 1 → Task 1，问题 2 → Task 2，问题 3 → Task 3。
2. **占位符检查**：无 TBD/TODO/空壳步骤，每个步骤都有具体代码或命令。
3. **类型一致性**：`SubAgentRenderInfo.tool_calls_count` 在 Task 2 新增，Task 3 的测试更新中统一使用 `tool_calls_count: 0`。`format_running_duration` 新签名在 Task 1 定义，Task 2 的 running 行中使用。
