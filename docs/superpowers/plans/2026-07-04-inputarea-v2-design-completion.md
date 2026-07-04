# InputArea v2 设计规范补全实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 逐项对齐 TUI-PAGE 第 3 节设计规范中的 8 个 GAP，重点补全输入历史持久化、草稿保存/恢复、预测输入 UI、macOS Option 兼容层、@mention 模糊匹配、Slash 合并 skills、历史容量。Windows 兼容层单独另立计划。

**Architecture:** 按文件聚合度拆分 5 个任务——`input_history.rs` 三合一（容量+持久化+草稿）、`input_area.rs` 预测渲染、`event_handlers.rs` macOS 兼容、`mention_popup.rs` 模糊匹配、`slash_completion.rs` skills 合并。每个任务独立可交付测试。

**Tech Stack:** Rust, ratatui-kit, skim, serde_json, tempfile

---

### Task 1: 输入历史：容量修正 + 持久化 + 草稿保存/恢复

**Files:**
- Modify: `peri-tui/src/kit/input_history.rs`（全部重写，从 atom 辅助函数升级为完整历史模块）
- Modify: `peri-tui/src/kit/atoms.rs`（确认 INPUT_HISTORY / INPUT_HISTORY_INDEX atom 定义）
- Modify: `peri-tui/src/kit/input_area.rs:404-419`（启用草稿恢复）

**架构**：历史栈仍用 `VecDeque<String>` atom 作为运行内存，新增 `load()` 从 `~/.peri/input-history.json` 启动时加载，新增 `save()` 在每次 push 后异步持久化。`history_up()` 内自动保存当前文本为 `draft`（存于 atom），`history_down()` 回到 None 时返回 draft 文本。容量从 100 → 1000。

- [ ] **Step 1: 修正历史容量 MAX_HISTORY 100 → 1000**

`peri-tui/src/kit/input_history.rs:18` 当前：

```rust
const MAX_HISTORY: usize = 100;
```

替换为：

```rust
const MAX_HISTORY: usize = 1000;
```

更新对应测试 `test_max_history_capacity` 中 `0..150` → `0..1050`：

```rust
#[test]
#[serial]
fn test_max_history_capacity() {
    setup();
    for i in 0..1050 {
        push_history(&format!("cmd-{}", i));
    }
    assert_eq!(history_len(), MAX_HISTORY);
    let stored: VecDeque<String> = INPUT_HISTORY.state().read().clone();
    assert_eq!(stored.front().map(String::as_str), Some("cmd-50"));
    assert_eq!(stored.back().map(String::as_str), Some("cmd-1049"));
}
```

- [ ] **Step 2: 新增 DRAFT atom**

`peri-tui/src/kit/atoms.rs` —— 在 `INPUT_HISTORY_INDEX` 之后新增：

```rust
/// 进入历史模式时保存的用户当前输入文本草稿。
pub static DRAFT: AtomStatic<Option<String>> = AtomStatic::new(|| None);
```

- [ ] **Step 3: 新增 `history_path()` 辅助函数 + `load()` / `save()`**

`peri-tui/src/kit/input_history.rs` 文件末尾（`#[cfg(test)]` 之前）新增持久化函数：

```rust
use std::path::PathBuf;

/// ~/.peri/input-history.json
fn history_path() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".peri").join("input-history.json")
}

/// 启动时从磁盘加载历史到 atom。文件不存在或解析失败时静默跳过。
pub fn load_history() {
    let path = history_path();
    let Ok(data) = std::fs::read_to_string(&path) else {
        return;
    };
    let entries: Vec<String> = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => return,
    };
    let mut history = entries
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect::<VecDeque<String>>();
    while history.len() > MAX_HISTORY {
        history.pop_front();
    }
    *INPUT_HISTORY.state().write() = history;
}

/// 原子写入历史到磁盘（先写 .tmp 再 rename）。
fn save_history() {
    let path = history_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("tmp");
    let history = INPUT_HISTORY.state().read();
    let entries: Vec<&String> = history.iter().collect();
    if let Ok(json) = serde_json::to_string(&entries) {
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}
```

- [ ] **Step 4: 修改 `push_history()` —— 每次 push 后调用 `save_history()`**

`push_history()` 函数末尾（`*index_atom.write() = None;` 之后）新增：

```rust
    // 异步持久化到磁盘
    save_history();
```

- [ ] **Step 5: 新增草稿保存/恢复逻辑**

修改 `history_up()` 函数——在进入历史模式时（`current` 为 `None`）保存草稿：

```rust
pub fn history_up(current_text: Option<&str>) -> Option<String> {
    let history_atom = INPUT_HISTORY.state();
    let index_atom = INPUT_HISTORY_INDEX.state();

    let history = history_atom.read().clone();
    if history.is_empty() {
        return None;
    }

    let new_idx = match *index_atom.read() {
        None => {
            // 进入历史模式：保存当前文本为草稿
            let draft_text = current_text
                .map(|s| s.to_string())
                .filter(|s| !s.trim().is_empty());
            *crate::kit::atoms::DRAFT.state().write() = draft_text;
            history.len() - 1
        }
        Some(0) => 0,
        Some(i) => i - 1,
    };
    *index_atom.write() = Some(new_idx);
    history.get(new_idx).cloned()
}
```

修改 `history_down()` 函数——回到编辑态时返回草稿：

```rust
pub fn history_down() -> Option<String> {
    let history_atom = INPUT_HISTORY.state();
    let index_atom = INPUT_HISTORY_INDEX.state();

    let history = history_atom.read().clone();
    let current = *index_atom.read();
    let new_idx = match current {
        None => return None,
        Some(i) => i + 1,
    };

    if new_idx >= history.len() {
        *index_atom.write() = None;
        // 回到编辑状态：返回草稿
        crate::kit::atoms::DRAFT.state().read().clone()
    } else {
        *index_atom.write() = Some(new_idx);
        history.get(new_idx).cloned()
    }
}
```

修改 `reset_history_cursor()`——提交后清除草稿：

```rust
pub fn reset_history_cursor() {
    *INPUT_HISTORY_INDEX.state().write() = None;
    *crate::kit::atoms::DRAFT.state().write() = None;
}
```

- [ ] **Step 6: 更新调用点——添加 `current_text` 参数**

`peri-tui/src/kit/input_area.rs:404-419` 当前：

```rust
KeyCode::Up if !is_ctrl && !mention_active && !slash_active => {
    tracing::info!(?key, "input area consumed up");
    let moved = state.write().move_up(false);
    if !moved && let Some(historical) = history_up() {
        state.write().replace_all(historical);
    }
    EventResult::Consumed

KeyCode::Down if !is_ctrl && !mention_active && !slash_active => {
    tracing::info!(?key, "input area consumed down");
    let moved = state.write().move_down(false);
    if !moved && let Some(historical) = history_down() {
        state.write().replace_all(historical);
    }
    EventResult::Consumed
```

修改 `history_up()` 调用传入当前文本：

```rust
KeyCode::Up if !is_ctrl && !mention_active && !slash_active => {
    tracing::info!(?key, "input area consumed up");
    let moved = state.write().move_up(false);
    if !moved {
        let current = state.read().all_text();
        if let Some(historical) = history_up(Some(&current)) {
            state.write().replace_all(historical);
        }
    }
    EventResult::Consumed

KeyCode::Down if !is_ctrl && !mention_active && !slash_active => {
    tracing::info!(?key, "input area consumed down");
    let moved = state.write().move_down(false);
    if !moved {
        if let Some(historical) = history_down() {
            state.write().replace_all(historical);
        }
    }
    EventResult::Consumed
```

- [ ] **Step 7: 更新测试**

`input_history.rs` 中所有调用 `history_up()` 的测试加 `None` 参数：

```rust
assert_eq!(history_up(None).as_deref(), Some("third"));
assert_eq!(history_up(None).as_deref(), Some("second"));
```

新增草稿测试：

```rust
#[test]
#[serial]
fn test_draft_saved_on_history_entry_and_restored() {
    setup();
    push_history("old");
    // 进入历史模式，传入草稿
    assert_eq!(history_up(Some("current draft")).as_deref(), Some("old"));
    // draft atom 应包含草稿
    assert_eq!(DRAFT.state().read().as_deref(), Some("current draft"));
    // 向下浏览回到编辑状态
    assert_eq!(history_down(), Some("current draft".to_string()));
    // draft 应已清空
    assert!(DRAFT.state().read().is_none());
}

#[test]
#[serial]
fn test_draft_cleared_on_reset() {
    setup();
    push_history("x");
    history_up(Some("draft"));
    assert!(DRAFT.state().read().is_some());
    reset_history_cursor();
    assert!(DRAFT.state().read().is_none());
}
```

在 `setup()` 中新增 DRAFT 初始化：

```rust
fn setup() {
    crate::kit::atoms::init_atoms();
    *INPUT_HISTORY.state().write() = VecDeque::new();
    *INPUT_HISTORY_INDEX.state().write() = None;
    *crate::kit::atoms::DRAFT.state().write() = None;
}
```

- [ ] **Step 8: 编译与测试**

```bash
cargo check -p peri-tui && cargo test -p peri-tui --lib -- input_history
```

预期：所有 input_history 测试 PASS（含新增草稿测试）。

- [ ] **Step 9: Commit**

```bash
git add peri-tui/src/kit/input_history.rs peri-tui/src/kit/atoms.rs peri-tui/src/kit/input_area.rs
git commit -m "feat(tui): input history persistence, draft save/restore, capacity 100→1000

- MAX_HISTORY 100→1000
- load_history() / save_history(): 启动加载 + 原子写入 ~/.peri/input-history.json
- history_up() 进入历史模式时自动保存草稿到 DRAFT atom
- history_down() 回到编辑态时返回草稿
- reset_history_cursor() 清除草稿"
```

---

### Task 2: 预测输入 UI 渲染

**Files:**
- Modify: `peri-tui/src/kit/input_area.rs` —— 渲染预测文本为灰色占位符，Tab 接受逻辑
- Verify: `peri-tui/src/kit/atoms.rs` —— `PREDICTION` atom 已定义

**架构**：`PredictionState { text: String }` atom 由 ACP `peri/prediction_ready` 事件写入。InputArea 渲染时若 buffer 为空且 prediction.text 非空，在 textarea 下方显示灰色弱色预测文本。Tab 时若 prediction 文本非空，注入到 buffer 并清除 prediction。

- [ ] **Step 1: 确认 PredictionState atom 定义**

`peri-tui/src/kit/atoms.rs:154` 当前：

```rust
pub struct PredictionState {
    pub text: String,
}
```

`peri-tui/src/kit/atoms.rs:236` 当前：

```rust
pub static PREDICTION: AtomStatic<PredictionState> = AtomStatic::new(|| PredictionState::default());
```

无需修改。

- [ ] **Step 2: 在 InputArea render body 中添加预测文本显示**

`peri-tui/src/kit/input_area.rs` —— 在 `composer` 下方（popup 渲染之后）新增预测文本行。找到 `let composer = ...` 和 popup 渲染之后的位置，添加：

```rust
// 预测文本（灰色占位符，只在 buffer 为空时显示）
let prediction = hooks.use_atom(&atoms::PREDICTION);
let pred = prediction.read();
let pred_visible = !pred.text.is_empty() && {
    let state = hooks.use_atom(&atoms::INPUT_BUFFER);
    state.read().is_empty()
};

let pred_line = if pred_visible {
    Line::from(Span::styled(
        format!("  {}", pred.text),
        Style::default().fg(statusbar_muted),
    ))
} else {
    Line::from("")
};
```

在返回的 View 中，composer 和 popup 下方插入预测行。

- [ ] **Step 3: 修改 Tab 处理——优先接受预测**

`peri-tui/src/kit/input_area.rs` 的 key handler 中找到 Tab 处理逻辑。当前 Tab 仅处理 mention/slash 补全。在 mention_active 和 slash_active 检查之前，新增：

```rust
// Tab 优先接受预测文本
let pred = hooks.use_atom(&atoms::PREDICTION);
if !pred.read().text.is_empty() {
    let text = pred.read().text.clone();
    state.write().replace_all(&text);
    *pred.write() = PredictionState::default();
    EventResult::Consumed
}
```

- [ ] **Step 4: 确保任何打印字符输入时清除预测**

在 `Paste` 和普通字符处理之后，预测已在 Tab 时清除。额外确保 `Enter` 提交消息时清除预测——找到 `Enter` 提交逻辑处添加：

```rust
*atoms::PREDICTION.state().write() = PredictionState::default();
```

- [ ] **Step 5: 编译与测试**

```bash
cargo check -p peri-tui
```

---

### Task 3: macOS Option 键兼容层

**Files:**
- Modify: `peri-tui/src/kit/event_handlers.rs` —— 新增 `KeyBinding` 结构体 + 双重匹配
- Modify: `peri-tui/src/kit/input_area.rs` —— Alt/Option modifier 在具体快捷键中的处理

**架构**：`KeyBinding` 结构体同时存储 macOS Option 合成 Unicode 字符和标准 Ctrl+字母路径，`matches()` 方法检查当前按键命中任一路径。macOS 终端按下 Option+字母时发送 Unicode 合成字符（无 modifier 标志），标准终端使用 Ctrl+字母（带 modifier 标志）。

- [ ] **Step 1: 新增 KeyBinding 结构体**

`peri-tui/src/kit/event_handlers.rs` 文件头部新增：

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// 跨平台快捷键绑定——同时匹配 macOS Option 合成字符和标准 Ctrl+字母路径。
struct KeyBinding {
    /// macOS Option 键产生的合成字符（如 Alt+M = µ）
    macos_char: Option<char>,
    /// 标准 Ctrl+字母 路径
    ctrl_letter: Option<char>,
    /// 其他修饰符组合
    ctrl_shift_letter: Option<(char, char)>,
}

impl KeyBinding {
    fn new() -> Self {
        Self {
            macos_char: None,
            ctrl_letter: None,
            ctrl_shift_letter: None,
        }
    }

    fn macos(c: char) -> Self {
        Self { macos_char: Some(c), ..Self::new() }
    }

    fn ctrl(c: char) -> Self {
        Self { ctrl_letter: Some(c), ..Self::new() }
    }

    fn ctrl_shift(c: char, s: char) -> Self {
        Self { ctrl_letter: Some(c), ctrl_shift_letter: Some((c, s)), ..Self::new() }
    }

    fn matches(&self, key: &KeyEvent) -> bool {
        // macOS 路径：无修饰符 + 合成字符匹配
        if let Some(ch) = self.macos_char {
            if key.modifiers == KeyModifiers::NONE
                || key.modifiers == KeyModifiers::ALT
            {
                if let KeyCode::Char(c) = key.code {
                    if c == ch {
                        return true;
                    }
                }
            }
        }
        // 标准 Ctrl+字母 路径
        if let Some(ch) = self.ctrl_letter {
            if key.modifiers == KeyModifiers::CONTROL {
                if let KeyCode::Char(c) = key.code {
                    if c.to_ascii_lowercase() == ch.to_ascii_lowercase() {
                        return true;
                    }
                }
            }
        }
        // Ctrl+Shift+字母 路径
        if let Some((c, s)) = self.ctrl_shift_letter {
            if key.modifiers == KeyModifiers::CONTROL | KeyModifiers::SHIFT {
                if let KeyCode::Char(ch) = key.code {
                    if ch.to_ascii_lowercase() == s.to_ascii_lowercase() {
                        return true;
                    }
                }
            }
        }
        false
    }
}
```

- [ ] **Step 2: 定义快捷键绑定常量**

在同一文件中，替换原有的 Ctrl+T / Ctrl+Shift+T 匹配为 KeyBinding：

```rust
const SHORTCUT_CYCLE_MODEL: KeyBinding = KeyBinding {
    macos_char: Some('µ'),       // Alt+M on macOS terminal
    ctrl_letter: Some('t'),       // Ctrl+T everywhere
    ctrl_shift_letter: None,
};

const SHORTCUT_CYCLE_PROVIDER: KeyBinding = KeyBinding {
    macos_char: Some('Â'),       // Alt+Shift+M on macOS terminal
    ctrl_letter: None,
    ctrl_shift_letter: Some(('t', 't')), // Ctrl+Shift+T
};
```

- [ ] **Step 3: 替换事件处理中的匹配逻辑**

找到 Ctrl+T / Ctrl+Shift+T 的事件匹配（通常在 Ctrl+B 附近），替换为：

```rust
if SHORTCUT_CYCLE_MODEL.matches(key_event) {
    // cycle model alias
    cycle_model_alias();
    return EventResult::Consumed;
}
if SHORTCUT_CYCLE_PROVIDER.matches(key_event) {
    // cycle provider
    cycle_provider();
    return EventResult::Consumed;
}
```

- [ ] **Step 4: 更新 input_area.rs 中的 Alt 修饰符处理**

`peri-tui/src/kit/input_area.rs:386-393` 已有 `is_alt` 处理词跳转（Alt+Left/Right）。确认 Alt+Enter 换行已在 key event 中正确处理（`is_alt && Enter` → 插入换行）。

- [ ] **Step 5: 编译与测试**

```bash
cargo check -p peri-tui
```

---

### Task 4: @mention 模糊匹配

**Files:**
- Modify: `peri-tui/src/kit/mention_popup.rs` —— 替换前缀匹配为 SkimMatcherV2 模糊匹配
- Verify: `peri-tui/Cargo.toml` —— 确认 skim 依赖

- [ ] **Step 1: 确认 skim 依赖**

`peri-tui/Cargo.toml` 中确认是否存在 skim，若不存在则添加：

```toml
skim = "0.15"
```

若已有则跳过。

- [ ] **Step 2: 修改 mention_popup.rs 匹配逻辑**

当前 mention_popup 使用前缀匹配过滤文件列表。找到过滤逻辑（通常是 `MENTION_PREFIX` atom 读取后 filter 文件列表）：

```rust
let mention_prefix = MENTION_PREFIX.state().read();
let files = FILE_LIST.state().read();

let skimmer = skim::SkimMatcherV2::default();
let query = mention_prefix.trim_start_matches('@').to_lowercase();

let mut scored: Vec<(i64, &str)> = files
    .iter()
    .filter_map(|f| {
        let score = skimmer.fuzzy_match(f, &query)?;
        Some((score, f.as_str()))
    })
    .collect();

// 按分数降序排列
scored.sort_by_key(|(score, _)| -*score);

let matches: Vec<String> = scored
    .into_iter()
    .take(20)
    .map(|(_, s)| s.to_string())
    .collect();
```

用 `matches` 替代原来的前缀过滤结果。

- [ ] **Step 3: 编译与测试**

```bash
cargo check -p peri-tui
```

---

### Task 5: Slash completion 合并 skills 条目

**Files:**
- Modify: `peri-tui/src/kit/slash_completion.rs` —— 纳入 skills 数据源
- Modify: `peri-tui/src/kit/input_area.rs` —— 构建 SlashCompletionItem 时合并 skills

**背景**：已有 issue `2026-07-03-slash-popup-missing-skills.md`。ACP 通过 `AvailableCommandsUpdate` 通知下发完整命令列表（含 skills），TUI 写入 `AVAILABLE_SLASH_COMMANDS` atom。当前 `input_area.rs` 构建 slash items 时只用了 panel commands + ACP 内置命令，未包含 `AVAILABLE_SLASH_COMMANDS`。

- [ ] **Step 1: 读取 AVAILABLE_SLASH_COMMANDS atom 的来源**

确认 `peri-tui/src/kit/acp_notifier.rs:92-122` 中 `update_available_slash_commands` 已将 ACP 下发的 commands（含 skills）写入 `AVAILABLE_SLASH_COMMANDS` atom。

- [ ] **Step 2: 修改 input_area.rs 中 SlashCompletionItem 构建逻辑**

在 slash completion 的 `use_hook` 中，找到构建 `items: Vec<SlashCompletionItem>` 的位置。当前逻辑大致是：panel commands + 硬编码 ACP 命令（bg/clear/compact/rewind）。改为三源合并：

```rust
let panel_items: Vec<SlashCompletionItem> = PANELS.iter().map(|m| {
    SlashCompletionItem {
        label: format!("/{}", m.slash_command),
        description: m.description.to_string(),
        kind: SlashActionKind::Panel,
    }
}).collect();

let acp_commands = AVAILABLE_SLASH_COMMANDS.state().read();
let acp_items: Vec<SlashCompletionItem> = acp_commands.iter().map(|(name, desc)| {
    SlashCompletionItem {
        label: format!("/{}", name),
        description: desc.clone(),
        kind: SlashActionKind::Command,
    }
}).collect();

let slash_prefix = SLASH_PREFIX.state().read().clone().to_lowercase();

// 三源合并 + 排序：前缀精确匹配 > panel > command > 字母序
let mut all_items: Vec<SlashCompletionItem> = Vec::new();
all_items.extend(panel_items);
all_items.extend(acp_items);

// 排序：前缀精确匹配优先
let exact_prefix = slash_prefix.clone();
all_items.sort_by(|a, b| {
    let a_exact = a.label.to_lowercase().starts_with(&exact_prefix);
    let b_exact = b.label.to_lowercase().starts_with(&exact_prefix);
    b_exact.cmp(&a_exact)
        .then_with(|| a.label.cmp(&b.label))
});
```

- [ ] **Step 3: 编译与测试**

```bash
cargo check -p peri-tui && cargo test -p peri-tui --lib -- slash
```

---

### Task 6: 全量验证

- [ ] **Step 1: 运行全量测试**

```bash
cargo test -p peri-tui --lib
```

预期：所有测试 PASS。

- [ ] **Step 2: Clippy 检查**

```bash
cargo clippy -p peri-tui --lib
```

预期：无新增 warning。

- [ ] **Step 3: 最终 commit**

```bash
git status
# 确认仅预期文件被修改
git add -A
git commit -m "feat(tui): InputArea v2 design completion — history, prediction, macOS, fuzzy, skills

- input_history: MAX_HISTORY 100→1000, persistence to ~/.peri/input-history.json, draft save/restore
- input_area: prediction text grey placeholder, Tab acceptance, draft integration
- event_handlers: KeyBinding struct with macOS Option Unicode + Ctrl dual path
- mention_popup: SkimMatcherV2 fuzzy matching replaces prefix filter
- slash_completion: merge panel commands + ACP skills from AVAILABLE_SLASH_COMMANDS atom"
```

---

### Self-Review

**1. Spec coverage:**
- [x] GAP-1 历史持久化 → Task 1
- [x] GAP-2 草稿保存/恢复 → Task 1
- [x] GAP-3 @mention 模糊匹配 → Task 4
- [x] GAP-4 Slash skills 合并 → Task 5
- [x] GAP-5 macOS Option 兼容 → Task 3
- [x] GAP-6 Windows 兼容 → 不在本计划范围，另立计划
- [x] GAP-7 历史容量 100→1000 → Task 1
- [x] GAP-8 预测 UI → Task 2

**2. Placeholder scan:** 无 "TBD" / "TODO" 模式。

**3. Type consistency:** `KeyBinding` 在 Task 3 定义并在同一 task 中使用；`DRAFT` atom 在 Task 1 定义并在 input_area 中使用；`PredictionState` 已在 atoms.rs 定义，Task 2 仅消费。
