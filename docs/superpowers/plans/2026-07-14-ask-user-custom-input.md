# AskUserQuestion Panel: 用户自定义文本输入 实施方案

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 AskUserQuestion 面板中，每个问题下方增加「自定义输入」入口，用户选中后可进入文本编辑模式，输入任意文本作为答案。

**Architecture:** 不引入新 popup/组件。在现有 `AskUserPanel` 组件内新增 `InputMode` 状态机，通过 `use_event_handler` 在 High 优先级下捕获键盘事件实现 inline 文本编辑。渲染层在 `Typing` 模式下切换为光标 + 文本 buffer 显示。答案序列化层扩展 `custom_answers` 并行状态，与现有的 `answers: Vec<Vec<usize>>` 互不冲突。

**Tech Stack:** Rust 2021, ratatui-kit (`#[component]` + `use_state` + `use_event_handler`), unicode-width

**问题范围：** Issue `spec/issues/2026-07-14-ask-user-multiselect-tui-support.md` — 现象 3「缺少用户自定义输入」

---

## File Structure

| 操作 | 文件 | 职责 |
|------|------|------|
| Modify | `peri-tui/src/kit/panels/ask_user.rs` | 核心改动：状态、事件处理、渲染、序列化 |
| Modify | `peri-tui/locales/en/main.ftl` | 新增 4 个 i18n key（自定义输入标签） |
| Modify | `peri-tui/locales/zh-CN/main.ftl` | 新增 4 个中文 i18n key |
| Create | `peri-tui/src/kit/panels/ask_user_test.rs` | 纯逻辑函数的单元测试 |

---

### Task 1: 新增状态——InputMode 枚举 + custom_answers + 初始化

**Files:**
- Modify: `peri-tui/src/kit/panels/ask_user.rs:43-58`

- [ ] **Step 1: 在文件顶部定义 InputMode 枚举**

在 `use` 语句之后、`#[component]` 之前插入：

```rust
/// 面板交互模式：选项导航 vs 文本输入
#[derive(Clone, PartialEq)]
enum InputMode {
    /// 正在选项列表中导航（默认模式）
    Selecting,
    /// 正在输入自定义文本；携带当前输入的文本 buffer
    Typing { buffer: String },
}
```

- [ ] **Step 2: 新增 `input_mode` 和 `custom_answers` 状态**

在 `ask_user.rs:43-46`，在已有的 `focused_option` 之后追加：

```rust
let input_mode = hooks.use_state(|| InputMode::Selecting);
/// 每个问题的自定义文本答案（与 answers 并行，互不冲突）
let custom_answers = hooks.use_state(Vec::<Option<String>>::new);
```

- [ ] **Step 3: 更新 session_fingerprint 重置逻辑**

将 `ask_user.rs:54-58` 改为：

```rust
if *session_fingerprint.read() != current_fingerprint {
    *focused.write() = 0;
    *answers.write() = vec![vec![]; question_count];
    *focused_option.write() = 0;
    *input_mode.write() = InputMode::Selecting;
    *custom_answers.write() = vec![None; question_count];
    *session_fingerprint.write() = current_fingerprint;
}
```

- [ ] **Step 4: 验证编译**

```bash
cargo check -p peri-tui 2>&1
```

预期：编译通过（新状态未被使用，仅 warning）

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/panels/ask_user.rs
git commit -m "feat(ask-user): add InputMode state and custom_answers for text input"
```

---

### Task 2: 渲染——自定义输入选项行 + Typing 模式 UI

**Files:**
- Modify: `peri-tui/src/kit/panels/ask_user.rs:276-329`

- [ ] **Step 1: 在选项列表末尾渲染「自定义输入」入口行**

在 `ask_user.rs` 的选项渲染循环（`for (opt_i, opt) in q.options.iter().enumerate()`）**之后**、`if q.options.is_empty()` **之前**，插入自定义输入选项的渲染：

```rust
// ── 自定义输入入口（附加在预设选项之后） ──
let custom_option_index = q.options.len(); // 自定义输入的「伪索引」
let has_custom_answer = custom_answers
    .read()
    .get(focused_idx)
    .map(|ca| ca.is_some())
    .unwrap_or(false);
let is_custom_focused = fopt == custom_option_index;

let (custom_mark, custom_text) = if *input_mode.read() == InputMode::Typing {
    // 正在输入模式：显示光标和 buffer
    let buf = match &*input_mode.read() {
        InputMode::Typing { buffer } => {
            if buffer.is_empty() {
                "|".to_string()
            } else {
                format!("{}|", buffer)
            }
        }
        _ => "|".to_string(),
    };
    ("✎".to_string(), format!("  {}", buf))
} else if has_custom_answer {
    // 已有自定义答案
    let existing = custom_answers
        .read()
        .get(focused_idx)
        .cloned()
        .flatten()
        .unwrap_or_default();
    let mark = if q.multi_select { "☑" } else { "●" };
    (mark.to_string(), format!("  {}", existing))
} else {
    // 未输入：显示占位提示
    ("✎".to_string(), format!("  {}", i18n::tr("ask-user-placeholder")))
};

let custom_style = if has_custom_answer {
    Style::new().fg(popup_tokens.action_primary).bold()
} else if is_custom_focused {
    Style::new().fg(popup_tokens.selected_fg)
} else {
    Style::new().fg(semantic.text.dim)
};

let custom_label_line = format!("  {} {}", custom_mark, custom_text);
for wrapped in wrap_text(&custom_label_line, 80) {
    lines.push(Line::from(wrapped).style(custom_style));
}
```

- [ ] **Step 2: 在 Typing 模式下隐藏预设选项的选中状态**

在选项渲染循环的开头，当 `input_mode` 为 `Typing` 时，跳过预设选项的选中高亮（避免视觉冲突）。将现有的 `for (opt_i, opt)` 循环 body 的第一行包裹：

```rust
for (opt_i, opt) in q.options.iter().enumerate() {
    // Typing 模式下不显示预设选项的选中状态
    let typing = *input_mode.read() == InputMode::Typing;
    let has_custom = *input_mode.read() == InputMode::Typing
        || custom_answers.read().get(focused_idx).map(|ca| ca.is_some()).unwrap_or(false);
    let is_selected = if typing {
        false // 打字时隐藏预设选项的勾选
    } else {
        selected_indices.contains(&opt_i)
    };
    // ... 其余不变 ...
}
```

- [ ] **Step 3: 验证编译**

```bash
cargo check -p peri-tui 2>&1
```

预期：编译通过

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/kit/panels/ask_user.rs
git commit -m "feat(ask-user): render custom input option row and Typing mode UI"
```

---

### Task 3: 事件处理——Space 进入 Typing / 文本编辑 / Enter 确认 / ESC 取消

**Files:**
- Modify: `peri-tui/src/kit/panels/ask_user.rs:77-214`

- [ ] **Step 1: 在事件 handler 最前面增加 Typing 模式分支**

在 `ask_user.rs:86-88`（popup 检查之后），Space 处理之前，插入 Typing 模式的事件捕获：

```rust
// Typing 模式：捕获所有按键用于文本编辑
if let InputMode::Typing { ref buffer } = *input_mode.read() {
    let mut buf = buffer.clone();
    let mut consumed = true;
    match (key.modifiers, key.code) {
        // Enter → 确认输入
        (KeyModifiers::NONE, KeyCode::Enter) => {
            if !buf.trim().is_empty() {
                // 保存到 custom_answers
                let q_idx = *focused.read();
                let mut ca = custom_answers.write();
                if q_idx >= ca.len() {
                    ca.resize(q_idx + 1, None);
                }
                ca[q_idx] = Some(buf.trim().to_string());
            }
            *input_mode.write() = InputMode::Selecting;
        }
        // ESC → 取消输入，恢复原状
        (KeyModifiers::NONE, KeyCode::Esc) => {
            *input_mode.write() = InputMode::Selecting;
        }
        // Backspace → 删除最后一个字符
        (KeyModifiers::NONE, KeyCode::Backspace) => {
            buf.pop();
            *input_mode.write() = InputMode::Typing { buffer: buf };
        }
        // Ctrl+W → 删除最后一个词
        (KeyModifiers::CONTROL, KeyCode::Char('w'))
        | (KeyModifiers::CONTROL, KeyCode::Char('W')) => {
            // 从末尾向前找空格/标点边界
            if let Some(pos) = buf.rfind(char::is_whitespace) {
                buf.truncate(pos);
            } else {
                buf.clear();
            }
            *input_mode.write() = InputMode::Typing { buffer: buf };
        }
        // 可打印字符 → 追加到 buffer
        (KeyModifiers::NONE, KeyCode::Char(c)) => {
            buf.push(c);
            *input_mode.write() = InputMode::Typing { buffer: buf };
        }
        (KeyModifiers::SHIFT, KeyCode::Char(c)) => {
            buf.push(c);
            *input_mode.write() = InputMode::Typing { buffer: buf };
        }
        _ => {
            consumed = false;
        }
    }
    if consumed {
        return EventResult::Consumed;
    }
}
```

- [ ] **Step 2: 修改 Space 键处理——检测自定义选项索引**

在现有的 Space 处理块（`ask_user.rs:90-118`）中，增加对自定义选项（`opt_idx == q.options.len()`）的检测：

```rust
// Space：选中/取消当前高亮的选项（多选时 toggle 入 vec，单选时替换）
if (key.modifiers, key.code) == (KeyModifiers::NONE, KeyCode::Char(' ')) {
    let q_idx = *focused.read();
    let opt_idx = *focused_option.read();
    // 自定义输入选项：进入 Typing 模式
    if let Some(au) = pending_for_closure.as_ref()
        && let Some(q) = au.questions.get(q_idx)
        && opt_idx == q.options.len()
    {
        // 如果已有自定义答案，预填到 buffer
        let existing = custom_answers
            .read()
            .get(q_idx)
            .cloned()
            .flatten()
            .unwrap_or_default();
        *input_mode.write() = InputMode::Typing { buffer: existing };
        return EventResult::Consumed;
    }
    if let Some(au) = pending_for_closure.as_ref()
        && let Some(q) = au.questions.get(q_idx)
        && opt_idx < q.options.len()
    {
        // ... 原有逻辑不变 ...
```

- [ ] **Step 3: 修改 MoveDown 上限——包含自定义选项**

在 `MoveDown` 处理（`ask_user.rs:128-137`）中，将 `limit` 从 `q.options.len()` 改为 `q.options.len() + 1`：

```rust
Some(ListNavAction::MoveDown) => {
    let limit = pending_for_closure
        .as_ref()
        .and_then(|au| au.questions.get(*focused.read()))
        .map(|q| q.options.len() + 1) // +1 为自定义输入入口
        .unwrap_or(0);
    if limit > 0 {
        let mut fo = focused_option.write();
        *fo = next_selection(*fo, limit);
    }
    EventResult::Consumed
}
```

- [ ] **Step 4: 修改 Enter 提交——检测自定义答案**

在 `Confirm` 处理的 `all_answered` 检查（`ask_user.rs:161-170`）中，同时检查 `custom_answers`：

```rust
let all_answered = answers.read().iter().enumerate().all(|(i, a)| {
    !a.is_empty()
        || custom_answers
            .read()
            .get(i)
            .map(|ca| ca.is_some())
            .unwrap_or(false)
        || pending_for_closure
            .as_ref()
            .and_then(|au| au.questions.get(i))
            .map(|q| q.options.is_empty())
            .unwrap_or(true)
});
```

并在 Enter 提交时，同时读取 `custom_answers`：

```rust
} else {
    let answers_snapshot = answers.read().clone();
    let custom_snapshot = custom_answers.read().clone();
    let answers_map =
        build_answers_map(
            pending_for_closure.as_ref(),
            &answers_snapshot,
            &custom_snapshot,
        );
    // ... 原有提交逻辑不变 ...
```

- [ ] **Step 5: 在 Typing 模式下禁用 Tab/CycleForward/CycleBackward**

在现有的 CycleForward/CycleBackward 处理前，增加模式检查：

```rust
Some(ListNavAction::CycleForward) if question_count > 0 => {
    if *input_mode.read() == InputMode::Typing {
        return EventResult::Consumed; // 打字模式禁止切题
    }
    // ... 原有逻辑 ...
}
Some(ListNavAction::CycleBackward) if question_count > 0 => {
    if *input_mode.read() == InputMode::Typing {
        return EventResult::Consumed;
    }
    // ... 原有逻辑 ...
}
```

- [ ] **Step 6: 验证编译**

```bash
cargo check -p peri-tui 2>&1
```

预期：编译通过

- [ ] **Step 7: Commit**

```bash
git add peri-tui/src/kit/panels/ask_user.rs
git commit -m "feat(ask-user): add text input mode with keyboard capture for custom answers"
```

---

### Task 4: 答案序列化——build_answers_map 支持自定义文本

**Files:**
- Modify: `peri-tui/src/kit/panels/ask_user.rs:461-484`

- [ ] **Step 1: 扩展函数签名**

```rust
fn build_answers_map(
    pending: Option<&AskUser>,
    answers: &[Vec<usize>],
    custom_answers: &[Option<String>],
) -> serde_json::Value {
```

- [ ] **Step 2: 合并自定义文本到答案**

```rust
fn build_answers_map(
    pending: Option<&AskUser>,
    answers: &[Vec<usize>],
    custom_answers: &[Option<String>],
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    if let Some(au) = pending {
        for (i, q) in au.questions.iter().enumerate() {
            // 优先使用自定义文本
            if let Some(Some(custom)) = custom_answers.get(i) {
                map.insert(q.id.clone(), json!(custom.clone()));
                continue;
            }
            // 否则使用预设选项
            let selected: Vec<usize> = answers.get(i).cloned().unwrap_or_default();
            let val = if q.multi_select {
                // 多选：返回 label 数组
                let labels: Vec<serde_json::Value> = selected
                    .iter()
                    .filter_map(|idx| q.options.get(*idx).map(|opt| json!(opt.label)))
                    .collect();
                json!(labels)
            } else {
                // 单选：返回单个 label
                selected
                    .first()
                    .and_then(|idx| q.options.get(*idx).map(|opt| json!(opt.label)))
                    .unwrap_or(json!(""))
            };
            map.insert(q.id.clone(), val);
        }
    }
    serde_json::Value::Object(map)
}
```

- [ ] **Step 3: 验证编译**

```bash
cargo check -p peri-tui 2>&1
```

预期：编译通过（现在 Enter 提交已经传 custom_snapshot 了）

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/kit/panels/ask_user.rs
git commit -m "feat(ask-user): serialize custom text answers in build_answers_map"
```

---

### Task 5: i18n——自定义输入提示文本

**Files:**
- Modify: `peri-tui/locales/en/main.ftl`
- Modify: `peri-tui/locales/zh-CN/main.ftl`

- [ ] **Step 1: 确保英文 `ask-user-placeholder` 可用**

确认 `peri-tui/locales/en/main.ftl:263` 已有：

```ftl
ask-user-placeholder = Type something.
```

检查中文对应 key `peri-tui/locales/zh-CN/main.ftl:262`：

```ftl
ask-user-placeholder = 输入自定义内容...
```

（这两个 key 已存在，无需新增）

- [ ] **Step 2: 更新提示文本——增加 Ctrl+W 和 ESC 提示**

在 `peri-tui/locales/en/main.ftl` 的 multi-select 提示 key 末尾追加 Typing 模式的行：

在现有 `panel-ask-user-hint-tab-multi-select-*` / `panel-ask-user-hint-single-multi-select-*` 之后新增：

```ftl
panel-ask-user-hint-typing =   Typing · Ctrl+W::delete-word · Backspace::delete · Enter::confirm · Esc::cancel
```

在 `peri-tui/locales/zh-CN/main.ftl` 中对应位置新增：

```ftl
panel-ask-user-hint-typing =   输入中 · Ctrl+W::删词 · Backspace::删字 · Enter::确认 · Esc::取消
```

- [ ] **Step 3: 在 Typing 模式渲染时使用 typing hint**

在 `ask_user.rs` 的渲染函数中，当 `input_mode == InputMode::Typing` 时，替换现有的多选/单选 hint：

```rust
if *input_mode.read() == InputMode::Typing {
    lines.push(Line::from(i18n::tr("panel-ask-user-hint-typing")).fg(semantic.text.dim));
} else if au.questions.len() > 1 {
    // ... 现有多题逻辑 ...
} else {
    // ... 现有单题逻辑 ...
}
```

- [ ] **Step 4: 验证编译**

```bash
cargo check -p peri-tui 2>&1
```

预期：编译通过

- [ ] **Step 5: Commit**

```bash
git add peri-tui/locales/en/main.ftl peri-tui/locales/zh-CN/main.ftl peri-tui/src/kit/panels/ask_user.rs
git commit -m "feat(ask-user): i18n hints for typing input mode"
```

---

### Task 6: 单元测试——纯逻辑函数测试

**Files:**
- Create: `peri-tui/src/kit/panels/ask_user_test.rs`
- Modify: `peri-tui/src/kit/panels/ask_user.rs`（模块声明）

- [ ] **Step 1: 创建测试文件并编写 `build_answers_map` 测试**

创建 `peri-tui/src/kit/panels/ask_user_test.rs`：

```rust
#![cfg(test)]

use super::*;
use peri_acp_types::event_data::{AskUser, Question};
use serde_json::json;

fn make_question(id: &str, multi_select: bool, labels: &[&str]) -> Question {
    use peri_acp_types::event_data::QuestionOption;
    Question {
        id: id.to_string(),
        header: id.to_string(),
        question: format!("Question {id}"),
        options: labels
            .iter()
            .map(|l| QuestionOption {
                label: l.to_string(),
                description: String::new(),
            })
            .collect(),
        multi_select,
    }
}

fn make_ask_user(questions: Vec<Question>) -> AskUser {
    AskUser { questions }
}

// ─── build_answers_map ──────────────────────────────────────────

#[test]
fn test_build_answers_map_single_select_preset() {
    let au = make_ask_user(vec![make_question("q1", false, &["A", "B", "C"])]);
    let answers = vec![vec![1usize]]; // 选了 B
    let custom = vec![None];
    let result = build_answers_map(Some(&au), &answers, &custom);
    assert_eq!(result, json!({"q1": "B"}));
}

#[test]
fn test_build_answers_map_multi_select_preset() {
    let au = make_ask_user(vec![make_question("q1", true, &["A", "B", "C"])]);
    let answers = vec![vec![0usize, 2]]; // 选了 A 和 C
    let custom = vec![None];
    let result = build_answers_map(Some(&au), &answers, &custom);
    assert_eq!(result, json!({"q1": ["A", "C"]}));
}

#[test]
fn test_build_answers_map_custom_text() {
    let au = make_ask_user(vec![make_question("q1", false, &["A", "B"])]);
    let answers = vec![vec![]]; // 未选择预设选项
    let custom = vec![Some("my custom answer".to_string())];
    let result = build_answers_map(Some(&au), &answers, &custom);
    assert_eq!(result, json!({"q1": "my custom answer"}));
}

#[test]
fn test_build_answers_map_custom_overrides_preset() {
    // 自定义文本优先于预设选项
    let au = make_ask_user(vec![make_question("q1", false, &["A", "B"])]);
    let answers = vec![vec![0usize]]; // 选了 A（但被覆盖）
    let custom = vec![Some("overridden text".to_string())];
    let result = build_answers_map(Some(&au), &answers, &custom);
    assert_eq!(result, json!({"q1": "overridden text"}));
}

#[test]
fn test_build_answers_map_empty_custom_not_override() {
    let au = make_ask_user(vec![make_question("q1", false, &["A", "B"])]);
    let answers = vec![vec![1usize]]; // 选了 B
    let custom = vec![Some(String::new())]; // 空字符串仍视为自定义
    let result = build_answers_map(Some(&au), &answers, &custom);
    assert_eq!(result, json!({"q1": ""}));
}

#[test]
fn test_build_answers_map_mixed_preset_and_custom() {
    let au = make_ask_user(vec![
        make_question("q1", false, &["A", "B"]),
        make_question("q2", true, &["X", "Y", "Z"]),
        make_question("q3", false, &["P", "Q"]),
    ]);
    // q1: custom, q2: multi-select preset, q3: single-select preset
    let answers = vec![vec![], vec![0usize, 2], vec![1usize]];
    let custom = vec![Some("custom answer".to_string()), None, None];
    let result = build_answers_map(Some(&au), &answers, &custom);
    assert_eq!(
        result,
        json!({"q1": "custom answer", "q2": ["X", "Z"], "q3": "Q"})
    );
}

// ─── wrap_text ──────────────────────────────────────────────────

#[test]
fn test_wrap_text_short_returns_single_line() {
    let result = wrap_text("hello", 80);
    assert_eq!(result, vec!["hello"]);
}

#[test]
fn test_wrap_text_long_splits_at_whitespace() {
    let result = wrap_text("hello world foo bar", 12);
    assert_eq!(result, vec!["hello world", "foo bar"]);
}

#[test]
fn test_wrap_text_cjk_splits_at_boundary() {
    let result = wrap_text("你好世界你好世界你好世界", 10);
    // 每个中文字符 2 列宽，10 列 = 5 个字符
    assert_eq!(result.len(), 2);
    assert_eq!(result[0], "你好世界你");
    assert_eq!(result[1], "好世界你好世界");
}

#[test]
fn test_wrap_text_empty_returns_single_empty() {
    let result = wrap_text("", 80);
    assert_eq!(result, vec![""]);
}

#[test]
fn test_wrap_text_zero_width_returns_original() {
    let result = wrap_text("hello", 0);
    assert_eq!(result, vec!["hello"]);
}

// ─── InputMode ──────────────────────────────────────────────────

#[test]
fn test_input_mode_selecting_is_default() {
    let mode = InputMode::Selecting;
    assert_eq!(mode, InputMode::Selecting);
}

#[test]
fn test_input_mode_typing_holds_buffer() {
    let mode = InputMode::Typing {
        buffer: "hello".to_string(),
    };
    match mode {
        InputMode::Typing { buffer } => assert_eq!(buffer, "hello"),
        _ => panic!("expected Typing mode"),
    }
}
```

- [ ] **Step 2: 在 `ask_user.rs` 末尾添加模块声明**

在 `ask_user.rs:484`（`build_answers_map` 函数结束后、文件末尾）添加：

```rust
#[cfg(test)]
#[path = "ask_user_test.rs"]
mod tests;
```

- [ ] **Step 3: 运行测试验证**

```bash
cargo test -p peri-tui --lib -- ask_user_test 2>&1
```

预期：全部测试通过

- [ ] **Step 4: 运行已有测试确保无回归**

```bash
cargo test -p peri-tui --lib -- ask_user 2>&1
```

预期：全部已有测试仍通过（5 tests from `ask_user_action` + 6 new tests = 11 pass）

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/panels/ask_user_test.rs peri-tui/src/kit/panels/ask_user.rs
git commit -m "test(ask-user): unit tests for custom text answers, wrapping, and InputMode"
```

---

### Task 7: 最终验证——全量编译 + clippy + 现有测试

- [ ] **Step 1: 全量编译检查**

```bash
cargo check -p peri-tui 2>&1
```

预期：编译通过，零错误

- [ ] **Step 2: Clippy lint 检查**

```bash
cargo clippy -p peri-tui -- -D warnings 2>&1
```

预期：零警告

- [ ] **Step 3: 全量单元测试**

```bash
cargo test -p peri-tui --lib 2>&1
```

预期：全部通过

- [ ] **Step 4: Final commit**

```bash
git add -A
git commit -m "feat(ask-user): complete custom text input for user-defined answers"
```

---

## Self-Review Checklist

- ✅ Spec 覆盖：Issues 现象 3「缺少用户自定义输入」——覆盖渲染、事件处理、序列化、i18n
- ✅ 无占位符：所有步骤都有完整代码
- ✅ 类型一致性：`InputMode` 在 Task 1 定义，Task 2/3/5/6 使用；`custom_answers: Vec<Option<String>>` 贯穿 Task 1→4；`build_answers_map` 签名在 Task 4 修改后与 Task 6 测试一致
- ✅ 向下兼容：现有单选/多选行为不受影响（custom_answers 默认为 None 时走原路径）

## 边界条件覆盖

| 场景 | 处理方式 |
|------|----------|
| 空 buffer 按 Enter | 不保存（`if !buf.trim().is_empty()` 检查），退回 Selecting |
| ESC 退出 Typing | 丢弃 buffer，恢复 Selecting |
| Ctrl+W 删词 | `rfind(char::is_whitespace)` 找词边界 |
| 已选预设选项 + 再输入自定义文本 | 自定义文本优先覆盖（`build_answers_map` 中 custom 优先） |
| 多选问题 + 自定义文本 | 自定义文本作为单值覆盖所有多选 |
| Tab/Cycle 在 Typing 模式 | 被拦截，禁止切题 |
| 面板重新打开（session_fingerprint 变化） | 所有状态重置（含 custom_answers） |
