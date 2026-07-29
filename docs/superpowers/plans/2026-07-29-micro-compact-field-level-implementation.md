# Micro Compact 字段级压缩 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 Micro Compact 从整段 tool input 替换改为顶层长字符串字段的确定性 head/tail 截断，避免丢失 Agent 等工具的必填参数和工作记忆。

**Architecture:** Planner 从旧工具交换中识别超过阈值的顶层字符串字段，并把字段名与截断尺寸写入 version 2 projection directive。Projection 只执行 directive 所列字段的变换，始终保留原 object 的其他结构和值；Micro/Smar​​t 仅持久化实际有变更的 directive。旧 version 1 directive 在 Reason 阶段安全回退为原始消息，并在下一次 compact 时被新计划覆盖或清除。

**Tech Stack:** Rust 2021、serde/serde_json、tracing、peri-agent 的 `MessageTranscript`、compact_v2 单元测试。

---

## 实施前约束

- 实现前先阅读：
  - `spec/issues/2026-07-29-micro-compact-field-level-design.md`
  - `spec/issues/2026-07-29-micro-compact-loses-agent-tool-context.md`
  - `peri-agent/CLAUDE.md`
  - `docs/standards/rust.md`
  - `docs/design/testing-standards.md`
- 本计划只改 `peri-agent`。不要修改 `peri-tui`、`peri-acp` 或 `peri-middlewares`；它们分别只展示、透传和校验服务端工具参数。
- 不执行 `git commit`，除非用户在执行时明确要求。所有“提交”检查以 `git diff --check` 和目标测试替代。
- 保留现有 `AskUserQuestion`、`goal`、`TodoWrite` 保护名单，以及 `ContextRetention::Preserve` / `StateBearing` 语义。
- 任何新诊断使用 `tracing`；不得增加 `println!`、`eprintln!` 或 `dbg!`。

## 文件结构与职责

| 文件 | 修改目的 |
| --- | --- |
| `peri-agent/src/agent/compact_v2/config.rs` | 500/350/100 默认配置与可验证的长度策略。 |
| `peri-agent/src/agent/compact_v2/config_test.rs` | 默认值、serde 兼容和非法策略测试。 |
| `peri-agent/src/agent/compact_v2/projection.rs` | policy version 常量、字段级 `CompactToolInput`、Unicode 截断和 action 级收益估算。 |
| `peri-agent/src/agent/compact_v2/projection_test.rs` | tool input 投影、provider 同步、旧 directive 和 CJK/emoji 回归。 |
| `peri-agent/src/agent/compact_v2/planner.rs` | 顶层字段筛选、短 ToolResult no-op、稳定字段排序、基于真实差值的估算。 |
| `peri-agent/src/agent/compact_v2/planner_test.rs` | planner 的 500/501 边界、保护名单、嵌套值和估算测试。 |
| `peri-agent/src/agent/compact_v2/micro.rs` | 仅持久化真实 action，清理旧 policy directive，返回按 message 去重的影响数。 |
| `peri-agent/src/agent/compact_v2/micro_test.rs` | directive 持久化、无长字段 no-op、v1 覆盖/清理测试。 |
| `peri-agent/src/agent/compact_v2/smart.rs` | 复用 `micro_compact`，避免 Smart 写入无 directive 的 `truncated` flags。 |
| `peri-agent/src/agent/compact_v2/mod.rs` | 重导出 policy version；保持 Micro/Smart 复用同一字段级语义。 |
| `peri-agent/src/agent/stages/reason.rs` | 使用 policy version 常量；遇到 v1 directive 后安全回退并以新版 planner 生成本轮视图。 |
| `peri-agent/src/session/transcript.rs` | 复用现有 `clear_flags` 清理无可替代 action 的旧 flags，不新增平行持久化 API。 |
| `peri-agent/src/thread/sqlite_store_test.rs` | 将硬编码的有效 directive 测试迁到 policy version 常量，验证持久化仍能 roundtrip。 |

---

### Task 1: 定义字段级投影契约和长度配置

**Files:**
- Modify: `peri-agent/src/agent/compact_v2/config.rs`
- Modify: `peri-agent/src/agent/compact_v2/config_test.rs`
- Modify: `peri-agent/src/agent/compact_v2/projection.rs`
- Modify: `peri-agent/src/agent/compact_v2/projection_test.rs`

- [ ] **Step 1: 写入 config 默认值的失败测试**

在 `config_test.rs` 的 `test_default_values` 中追加：

```rust
assert_eq!(config.micro_field_threshold_chars, 500);
assert_eq!(config.micro_field_keep_head_chars, 350);
assert_eq!(config.micro_field_keep_tail_chars, 100);
assert!(config.has_valid_micro_field_limits());
```

在 `test_serde_roundtrip` 的 `CompactConfig` 中设定非默认值并断言往返：

```rust
micro_field_threshold_chars: 900,
micro_field_keep_head_chars: 600,
micro_field_keep_tail_chars: 200,
```

在同文件新增非法配置测试：

```rust
#[test]
fn test_micro_field_limits_reject_zero_threshold_and_non_shrinking_bounds() {
    let zero = CompactConfig {
        micro_field_threshold_chars: 0,
        ..CompactConfig::default()
    };
    assert!(!zero.has_valid_micro_field_limits());

    let non_shrinking = CompactConfig {
        micro_field_threshold_chars: 500,
        micro_field_keep_head_chars: 350,
        micro_field_keep_tail_chars: 150,
        ..CompactConfig::default()
    };
    assert!(!non_shrinking.has_valid_micro_field_limits());
}
```

- [ ] **Step 2: 运行 config 测试确认编译失败**

Run:

```bash
cargo test -p peri-agent --lib compact_v2::config::tests -- --nocapture
```

Expected: 编译失败，提示 `CompactConfig` 没有 `micro_field_*` 字段或 `has_valid_micro_field_limits` 方法。

- [ ] **Step 3: 实现默认值和配置验证**

在 `config.rs` 添加默认函数：

```rust
fn default_micro_field_threshold_chars() -> usize { 500 }
fn default_micro_field_keep_head_chars() -> usize { 350 }
fn default_micro_field_keep_tail_chars() -> usize { 100 }
```

在 `CompactConfig` 的 `tool_result_keep_chars` 后添加：

```rust
/// Micro 对顶层 tool-input 字符串生效的严格长度阈值。
#[serde(default = "default_micro_field_threshold_chars")]
pub micro_field_threshold_chars: usize,
/// 超过阈值的字段保留的开头 Unicode 字符数。
#[serde(default = "default_micro_field_keep_head_chars")]
pub micro_field_keep_head_chars: usize,
/// 超过阈值的字段保留的结尾 Unicode 字符数。
#[serde(default = "default_micro_field_keep_tail_chars")]
pub micro_field_keep_tail_chars: usize,
```

在 `Default` 实现中初始化三个字段；在 `impl CompactConfig` 添加：

```rust
pub fn has_valid_micro_field_limits(&self) -> bool {
    self.micro_field_threshold_chars > 0
        && self.micro_field_keep_head_chars
            .saturating_add(self.micro_field_keep_tail_chars)
            < self.micro_field_threshold_chars
}
```

- [ ] **Step 4: 先为 action shape 写失败测试**

将 `projection_test.rs` 中现有 `CompactToolInput` 构造改为新版参数，并新增 serde roundtrip 场景：

```rust
ProjectionAction::CompactToolInput {
    fields: vec!["prompt".to_string()],
    keep_head: 350,
    keep_tail: 100,
}
```

断言反序列化后的 action 与原 action `assert_eq!`。此时不修改生产 enum。

- [ ] **Step 5: 运行 projection serde 测试确认失败**

Run:

```bash
cargo test -p peri-agent --lib test_projection_directive_serde_roundtrip -- --nocapture
```

Expected: 编译失败，提示 `CompactToolInput` 缺少 `keep_head` / `keep_tail`，或不存在 `preserve_shape`。

- [ ] **Step 6: 实现新的 projection action 和版本常量**

在 `projection.rs`、`ProjectionAction` 前定义唯一版本事实源：

```rust
pub const PROJECTION_POLICY_VERSION: u32 = 2;
```

将 enum variant 替换为：

```rust
CompactToolInput {
    /// Planner 已确认超过阈值的顶层字符串字段，按字典序持久化。
    fields: Vec<String>,
    /// 该 directive 固化的 head 长度，配置改变后不影响历史投影。
    keep_head: usize,
    /// 该 directive 固化的 tail 长度，配置改变后不影响历史投影。
    keep_tail: usize,
},
```

不要保留 `preserve_shape`，也不要保留兼容 `_compact_note` 分支。更新 `mod.rs` 的 public re-export，使其他 crate/测试可通过 `compact_v2::PROJECTION_POLICY_VERSION` 读取该常量。

- [ ] **Step 7: 运行 Task 1 的测试确认通过**

Run:

```bash
cargo test -p peri-agent --lib compact_v2::config::tests -- --nocapture
cargo test -p peri-agent --lib test_projection_directive_serde_roundtrip -- --nocapture
```

Expected: 两组测试通过；serde JSON 包含 `fields`、`keep_head`、`keep_tail`，不含 `preserve_shape`。

- [ ] **Step 8: 检查格式与差异**

Run:

```bash
cargo fmt --check
cargo check -p peri-agent
```

Expected: 两条命令成功。

---

### Task 2: 按顶层字段生成准确的 Micro 计划

**Files:**
- Modify: `peri-agent/src/agent/compact_v2/planner.rs`
- Modify: `peri-agent/src/agent/compact_v2/planner_test.rs`
- Modify: `peri-agent/src/agent/compact_v2/projection.rs`

- [ ] **Step 1: 写 Planner 的字段筛选失败测试**

在 `planner_test.rs` 增加一个 helper，构造 4 个 turn，令第一个 turn 已过 stale window；其第一个 tool input 为：

```rust
serde_json::json!({
    "prompt": "P".repeat(501),
    "description": "short description",
    "enabled": true,
    "metadata": {"nested": "N".repeat(900)},
    "items": ["A".repeat(900)]
})
```

新增测试 `test_plan_micro_selects_only_long_top_level_strings`，从该旧 `Agent` 调用的 action 取出 `CompactToolInput` 并断言：

```rust
assert_eq!(fields, &vec!["prompt".to_string()]);
assert_eq!((*keep_head, *keep_tail), (350, 100));
```

同时断言 input action 的数量为 1，确保 nested object、array、boolean 与短 `description` 不产生 action。

新增两个边界测试：

```rust
#[test]
fn test_plan_micro_does_not_compact_string_at_threshold() { /* "x".repeat(500) */ }

#[test]
fn test_plan_micro_compacts_string_above_threshold() { /* "x".repeat(501) */ }
```

前者断言没有 `CompactToolInput`，后者断言 `fields == ["prompt"]`。

- [ ] **Step 2: 运行 planner 新测试确认失败**

Run:

```bash
cargo test -p peri-agent --lib test_plan_micro_selects_only_long_top_level_strings -- --nocapture
cargo test -p peri-agent --lib test_plan_micro_does_not_compact_string_at_threshold -- --nocapture
cargo test -p peri-agent --lib test_plan_micro_compacts_string_above_threshold -- --nocapture
```

Expected: 第一个和 501 测试失败，因为当前 Planner 会对整个 tool call 生成 `fields: vec![]`；500 测试也会失败，因为当前实现不检查字段长度。

- [ ] **Step 3: 让 `ToolExchange` 携带 input 副本并实现字段选择 helper**

在 `planner.rs` 的 `ToolExchange` 增加：

```rust
pub tool_input: serde_json::Value,
```

在 `TurnGroup::tool_exchanges()` 用 `tc.arguments.clone()` 初始化它。添加只读 helper：

```rust
fn compactable_top_level_string_fields(
    input: &serde_json::Value,
    config: &CompactConfig,
) -> Vec<String> {
    if !config.has_valid_micro_field_limits() {
        tracing::warn!(
            threshold = config.micro_field_threshold_chars,
            keep_head = config.micro_field_keep_head_chars,
            keep_tail = config.micro_field_keep_tail_chars,
            "Micro Compact 字段长度配置无效，跳过 tool input 压缩"
        );
        return Vec::new();
    }

    let Some(object) = input.as_object() else {
        return Vec::new();
    };

    let mut fields: Vec<String> = object
        .iter()
        .filter_map(|(key, value)| {
            value
                .as_str()
                .filter(|text| text.chars().count() > config.micro_field_threshold_chars)
                .map(|_| key.clone())
        })
        .collect();
    fields.sort();
    fields
}
```

在原 `actions.push(CompactToolInput ...)` 位置先计算 `fields`；只有非空时才 push：

```rust
let fields = compactable_top_level_string_fields(&exchange.tool_input, config);
if !fields.is_empty() {
    actions.push(ProjectionActionEntry {
        message_id: exchange.ai_message_id,
        target: ProjectionTarget::ToolCall {
            tool_call_id: exchange.tool_call_id.clone(),
        },
        action: ProjectionAction::CompactToolInput {
            fields,
            keep_head: config.micro_field_keep_head_chars,
            keep_tail: config.micro_field_keep_tail_chars,
        },
    });
}
```

保留 `should_preserve_tool` 检查在该逻辑之前。`Agent` 不应出现在黑名单；不要为其添加特殊分支。

- [ ] **Step 4: 写 ToolResult 500/501 和错误保护的失败测试**

新增三项测试，每项构造足够旧的 `Bash` exchange：

```rust
#[test]
fn test_plan_micro_skips_short_success_tool_result() { /* "R".repeat(500) */ }

#[test]
fn test_plan_micro_compacts_long_success_tool_result() { /* "R".repeat(501) */ }

#[test]
fn test_plan_micro_keeps_error_tool_result_at_any_length() { /* tool_error + "E".repeat(2_000) */ }
```

分别断言没有/存在/没有 `ProjectionTarget::Message` 的 `CompactToolResult`。长结果 action 必须断言：

```rust
keep_head == 350
keep_tail == 100
preserve_recovery_handle
```

- [ ] **Step 5: 运行 ToolResult 测试确认失败**

Run:

```bash
cargo test -p peri-agent --lib test_plan_micro_skips_short_success_tool_result -- --nocapture
cargo test -p peri-agent --lib test_plan_micro_compacts_long_success_tool_result -- --nocapture
cargo test -p peri-agent --lib test_plan_micro_keeps_error_tool_result_at_any_length -- --nocapture
```

Expected: 500 测试失败，因为现有 Planner 对任意成功结果生成 action；501 测试失败，因为现有保留尺寸是 2000/200；错误测试通过或继续保持通过。

- [ ] **Step 6: 用同一阈值策略规划成功 ToolResult**

在 `planner.rs` 仅当 `!has_error` 时读取每个 result 的文本：

```rust
let result_chars = result_entry.message.message_content().text_content().chars().count();
if result_chars > config.micro_field_threshold_chars {
    actions.push(ProjectionActionEntry {
        message_id: result_entry.message.id(),
        target: ProjectionTarget::Message,
        action: ProjectionAction::CompactToolResult {
            keep_head: config.micro_field_keep_head_chars,
            keep_tail: config.micro_field_keep_tail_chars,
            preserve_recovery_handle: true,
        },
    });
}
```

不要对 error result 生成 action。不要改变媒体 `ReplaceMedia` 逻辑。

- [ ] **Step 7: 为无效配置和保护名单添加测试并实现 no-op 行为**

新增：

```rust
#[test]
fn test_plan_micro_invalid_field_limits_produce_no_string_actions() { /* threshold=500, head=350, tail=150 */ }

#[test]
fn test_plan_micro_preserves_blacklisted_tool_with_long_prompt_and_result() { /* AskUserQuestion, 2_000 chars */ }
```

第一个测试断言长 top-level string 与成功 ToolResult 都不生成 action；第二个断言 actions 为空。

实现必须依赖 `has_valid_micro_field_limits()`，而不是在 projection 时临时修正尺寸。由于 `plan_micro` 是纯函数，warn 只允许描述无效配置，不得写 transcript。

- [ ] **Step 8: 将 Planner policy version 改为常量并运行全部 Planner 测试**

将 `policy_version: 1` 替换为：

```rust
policy_version: super::projection::PROJECTION_POLICY_VERSION,
```

Run:

```bash
cargo test -p peri-agent --lib compact_v2::planner::tests -- --nocapture
```

Expected: Planner 测试通过；短 input/result 不再产生 action；长 input/result 的 action 具有 350/100 尺寸。

---

### Task 3: 执行字段级投影，并以实际字符差估算收益

**Files:**
- Modify: `peri-agent/src/agent/compact_v2/projection.rs`
- Modify: `peri-agent/src/agent/compact_v2/projection_test.rs`
- Modify: `peri-agent/src/agent/compact_v2/planner.rs`
- Modify: `peri-agent/src/agent/compact_v2/planner_test.rs`

- [ ] **Step 1: 以当前 Agent 故障为基准写失败回归测试**

替换旧的 `test_tool_input_projection_preserves_object_root`，新测试命名为：

```rust
#[test]
fn test_tool_input_projection_truncates_only_selected_agent_prompt() { /* ... */ }
```

使用 input：

```rust
let prompt = format!("{}{}", "前".repeat(350), "中".repeat(751));
let original = serde_json::json!({
    "prompt": prompt,
    "subagent_type": "explorer",
    "description": "定位 compact 数据流",
    "fork": false,
    "run_in_background": false
});
```

使用 `CompactToolInput { fields: vec!["prompt".into()], keep_head: 350, keep_tail: 100 }` 渲染后断言：

```rust
assert!(tc.arguments["prompt"].as_str().unwrap().contains("字符已省略"));
assert_eq!(tc.arguments["subagent_type"], "explorer");
assert_eq!(tc.arguments["description"], "定位 compact 数据流");
assert_eq!(tc.arguments["fork"], false);
assert_eq!(tc.arguments["run_in_background"], false);
assert!(tc.arguments.get("_compact_note").is_none());
```

增加短 Agent prompt 回归场景，使用 `fields: vec![]` 的 plan 或完全没有 action，并断言 projected arguments 与 original 完全相等。这条测试的注释必须包含：

```rust
/// [回归测试] v1 Micro Compact 曾把 Agent 的完整 arguments 替换为
/// `_compact_note`，使模型可能回显缺失 prompt 的新调用并导致 SubAgentTool 拒绝执行。
```

- [ ] **Step 2: 运行回归测试确认失败**

Run:

```bash
cargo test -p peri-agent --lib test_tool_input_projection_truncates_only_selected_agent_prompt -- --nocapture
```

Expected: 失败，因为当前 `project_tool_input` 丢弃所有字段并生成 `_compact_note`。

- [ ] **Step 3: 实现可复用的 Unicode 安全截断函数**

在 `projection.rs` 保留并泛化现有 `apply_head_tail`，将签名改为可表示无变更：

```rust
fn truncate_head_tail(text: &str, keep_head: usize, keep_tail: usize) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= keep_head.saturating_add(keep_tail) {
        return None;
    }

    let omitted = chars.len() - keep_head - keep_tail;
    let head: String = chars[..keep_head].iter().collect();
    let tail: String = chars[chars.len() - keep_tail..].iter().collect();
    Some(format!("{head}\n... [{omitted} 字符已省略] ...\n{tail}"))
}
```

保留或迁移现有调用点，使 `CompactText` 与 `CompactToolResult` 使用同一个 Unicode 安全 helper。只有 action 对应内容确实更长时才替换；否则返回原内容。

- [ ] **Step 4: 实现 `project_tool_input` 的字段级 clone-and-replace**

用下面逻辑完整替换 `_compact_note` 分支：

```rust
fn project_tool_input(tc: &ToolCallRequest, action: &ProjectionActionEntry) -> ToolCallRequest {
    let ProjectionAction::CompactToolInput {
        fields,
        keep_head,
        keep_tail,
    } = &action.action else {
        return tc.clone();
    };

    let Some(mut object) = tc.arguments.as_object().cloned() else {
        return tc.clone();
    };

    for field in fields {
        let Some(text) = object.get(field).and_then(|value| value.as_str()) else {
            continue;
        };
        if let Some(truncated) = truncate_head_tail(text, *keep_head, *keep_tail) {
            object.insert(field.clone(), serde_json::Value::String(truncated));
        }
    }

    ToolCallRequest {
        id: tc.id.clone(),
        name: tc.name.clone(),
        arguments: serde_json::Value::Object(object),
    }
}
```

此函数不得递归 object/array，不得更改 number/boolean/null，不得添加 `_compact_note`。现有 `project_ai_content` 已从同一个 projected `ToolCallRequest` 同步 `ContentBlock::ToolUse`；不要添加第二套变换逻辑。

- [ ] **Step 5: 写并运行 projection 的边界失败测试**

新增以下测试后运行，确认在实现前会失败或暴露旧行为：

```rust
#[test]
fn test_tool_input_projection_leaves_unselected_long_and_nested_values_unchanged() { /* selected=prompt only */ }

#[test]
fn test_tool_input_projection_ignores_missing_and_non_string_selected_fields() { /* fields=["missing", "enabled"] */ }

#[test]
fn test_tool_input_projection_preserves_cjk_and_emoji_boundaries() { /* "你".repeat(400) + "🦀".repeat(200) */ }

#[test]
fn test_tool_use_content_block_matches_projected_tool_call_arguments() { /* compare both JSON values */ }
```

Run:

```bash
cargo test -p peri-agent --lib test_tool_input_projection_leaves_unselected_long_and_nested_values_unchanged -- --nocapture
cargo test -p peri-agent --lib test_tool_input_projection_ignores_missing_and_non_string_selected_fields -- --nocapture
cargo test -p peri-agent --lib test_tool_input_projection_preserves_cjk_and_emoji_boundaries -- --nocapture
cargo test -p peri-agent --lib test_tool_use_content_block_matches_projected_tool_call_arguments -- --nocapture
```

Expected: 这些测试在旧整体替换逻辑下失败；Task 3 Step 4 后均通过。

- [ ] **Step 6: 将 action 级字符差变成唯一收益估算来源**

在 `projection.rs` 新增 crate-visible helper：

```rust
pub(crate) fn estimate_projection_chars(
    transcript: &MessageTranscript,
    actions: &[ProjectionActionEntry],
) -> (u64, u64)
```

该 helper 必须按 action target 读取原始内容并累计：

- `CompactToolInput`：只读取该 `tool_call_id` 的根 object 中 `fields` 所列字符串；使用 `truncate_head_tail` 的实际结果长度。找不到 message/call/field、类型不符或不产生缩短时均计零。
- `CompactToolResult`：只读取目标 Tool 消息的 `text_content()`；使用同一 helper 的实际结果长度；错误结果不会有该 action，但 helper 对意外 action 仍安全计算。
- `ReplaceMedia` / `CompactText`：保持现有估算语义；若现有代码没有可靠字符差，不要假装为 tool input/result 节省量。
- 每个字段只按 action 中的字段名计一次；同一 message 多个 action 可独立累计。

将 `plan_micro` 与 `plan_from_persisted_directives` 的旧 `chars / 3` 估算替换为该 helper，再除以 4 得到 token。删除旧的整消息 `estimate_tokens` 和 `estimate_tokens_for_actions`，避免双重事实源。

- [ ] **Step 7: 为实际收益写失败测试并实现**

在 `planner_test.rs` 新增：

```rust
#[test]
fn test_token_estimation_counts_only_selected_field_character_difference() {
    // 一个 501-char prompt、一个 2_000-char unselected nested value、一个短 ToolResult。
    // 断言 estimated_tokens_saved > 0 且远小于 2_000 / 4，证明 nested value 未被虚报回收。
}

#[test]
fn test_token_estimation_for_short_values_is_zero() {
    // 所有顶层字符串和成功结果均为 500 chars，断言 actions 与 saved 都为 0。
}
```

Run:

```bash
cargo test -p peri-agent --lib test_token_estimation_counts_only_selected_field_character_difference -- --nocapture
cargo test -p peri-agent --lib test_token_estimation_for_short_values_is_zero -- --nocapture
```

Expected: 新实现通过，且 `estimated_tokens_saved` 只来自实际替换的 `prompt` 字符差。

- [ ] **Step 8: 运行投影和 Planner 全量测试**

Run:

```bash
cargo test -p peri-agent --lib compact_v2::projection -- --nocapture
cargo test -p peri-agent --lib compact_v2::planner -- --nocapture
```

Expected: 全部通过；没有断言 `_compact_note` 的遗留测试。

---

### Task 4: 迁移 policy version 1 directive 并修复持久化路径

**Files:**
- Modify: `peri-agent/src/agent/compact_v2/micro.rs`
- Modify: `peri-agent/src/agent/compact_v2/micro_test.rs`
- Modify: `peri-agent/src/agent/compact_v2/smart.rs`
- Modify: `peri-agent/src/agent/compact_v2/projection.rs`
- Modify: `peri-agent/src/agent/compact_v2/projection_test.rs`
- Modify: `peri-agent/src/agent/stages/reason.rs`
- Modify: `peri-agent/src/thread/sqlite_store_test.rs`

- [ ] **Step 1: 写 version 1 安全回退的失败测试**

在 `projection_test.rs` 添加一个 transcript，其中历史 AI `Agent` call 的 input 为完整 `{"prompt":"short", "subagent_type":"explorer"}`，并对该 AI message 调用：

```rust
transcript.set_flags_projection(
    ai_id,
    MessageProjectionDirective {
        policy_version: 1,
        entries: vec![ProjectionActionEntry {
            message_id: ai_id,
            target: ProjectionTarget::ToolCall { tool_call_id: "agent_1".into() },
            action: ProjectionAction::CompactToolInput {
                fields: vec![],
                keep_head: 350,
                keep_tail: 100,
            },
        }],
    },
);
```

调用：

```rust
let result = plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION);
```

断言 result 是包含 `DIRECTIVE_VERSION_MISMATCH` 的错误，且直接使用原 `visible_messages()` 时 input 没有 `_compact_note`。

- [ ] **Step 2: 运行 version mismatch 测试确认现有行为不满足 v2 常量**

Run:

```bash
cargo test -p peri-agent --lib test_plan_from_persisted_directives_rejects_v1_agent_input_directive -- --nocapture
```

Expected: 编译或断言失败，直到 `PROJECTION_POLICY_VERSION` 被所有调用方采用。

- [ ] **Step 3: 统一使用 policy version 常量**

完成以下替换：

- `projection.rs` 的 `plan_from_persisted_directives` 调用测试使用 `PROJECTION_POLICY_VERSION`。
- `micro.rs` 已持久化 `plan.policy_version`，保持此行为。
- `planner.rs` 使用 `PROJECTION_POLICY_VERSION`。
- `reason.rs` 将字面量 `1` 改为：

```rust
crate::agent::compact_v2::PROJECTION_POLICY_VERSION
```

- `sqlite_store_test.rs` 中代表“当前有效 directive”的 `policy_version: 1` 改为该常量；专门测试旧版本的 case 保留显式 `1` 并写明是 legacy fixture。

`plan_from_persisted_directives` 保持 fail-closed：只要看到非当前 policy directive 就返回 `DIRECTIVE_VERSION_MISMATCH`，绝不解释或渲染旧 action。

- [ ] **Step 4: 为 Reason 的旧 directive 重规划写失败测试**

在与 `reason.rs` 相邻的既有 stage 测试文件中增加一条可构造的单元/集成测试；如果该模块没有可用 mock LLM seam，则在 `projection_test.rs` 以等价纯逻辑覆盖以下决策：

```text
v1 directive → plan_from_persisted_directives 返回 version mismatch
→ 使用 plan_micro(transcript, config, false)
→ 若存在 501-char 顶层 prompt，得到 v2 CompactToolInput action
→ render_llm_view 不出现 _compact_note，保留 subagent_type
```

测试必须明确断言 fallback planner 仍运行，不能只断言使用原始消息。

- [ ] **Step 5: 修改 Reason 对 version mismatch 的回退分支**

在 `reason.rs` 中，保留对 version mismatch 的 `tracing::warn!`，但不要 `visible` 直接返回。把该分支并入无 persisted directive 的 fallback planner 路径：

```rust
let plan = crate::agent::compact_v2::planner::plan_micro(&guard, config, false);
if plan.has_changes() {
    match crate::agent::compact_v2::projection::render_llm_view(&guard, &plan, &caps) {
        Ok(view) => view,
        Err(render_err) => {
            tracing::warn!(error = %render_err, "字段级 Micro 投影失败，使用原始可见消息");
            visible
        }
    }
} else {
    visible
}
```

该变更保证旧 v1 directive 在当前 turn 不会产生 `_compact_note`，但如果原始历史仍有长字段，当前请求仍可以安全使用 v2 字段级 view。

- [ ] **Step 6: 写旧 directive 覆盖与清理的失败测试**

在 `micro_test.rs` 增加两个场景：

```rust
#[test]
fn test_micro_compact_replaces_v1_directive_when_v2_actions_exist() { /* old v1 AI + 501-char prompt */ }

#[test]
fn test_micro_compact_clears_v1_directive_when_no_v2_actions_exist() { /* old v1 AI + short input/result */ }
```

第一个执行 `micro_compact` 后断言：

```rust
let flags = transcript.flags(ai_id);
assert!(flags.truncated);
assert_eq!(flags.projection.unwrap().policy_version, PROJECTION_POLICY_VERSION);
```

并断言新 directive 的 `CompactToolInput.fields == ["prompt"]`。

第二个执行后断言：

```rust
assert_eq!(transcript.flags(ai_id), MessageFlags::default());
```

- [ ] **Step 7: 实现 v1 cleanup/replace，且仅跳过当前版本已截断消息**

在 `micro.rs` 增加私有 helper：

```rust
fn clear_legacy_projection_flags(transcript: &mut MessageTranscript) {
    let legacy_ids: Vec<MessageId> = transcript
        .entries()
        .iter()
        .map(|entry| entry.message.id())
        .filter(|id| {
            transcript
                .flags(*id)
                .projection
                .as_ref()
                .is_some_and(|directive| directive.policy_version != PROJECTION_POLICY_VERSION)
        })
        .collect();

    for id in legacy_ids {
        transcript.clear_flags(id);
    }
}
```

在 `micro_compact` 的 `plan_micro` 调用前执行它。这样旧 v1 flags 被清除后，Planner 能以 `skip_existing_truncated=true` 重新生成 v2 action；若没有长字段/结果，则 flags 保持清除状态。

同时在 `planner.rs` 调整已截断跳过条件，使其只跳过已被**当前版本 directive**标记的消息：

```rust
let flags = transcript.flags(exchange.ai_message_id);
let already_has_current_projection = flags
    .projection
    .as_ref()
    .is_some_and(|directive| directive.policy_version == PROJECTION_POLICY_VERSION);
if skip_existing_truncated && flags.truncated && already_has_current_projection {
    continue;
}
```

不要把 `projection=None && truncated=true` 视为可自动清除：这仍是 `CORRUPTED_PROJECTION`，必须保持 Reason 的 fail-closed 保护，避免吞掉未知持久化损坏。

- [ ] **Step 8: 修复 Smart 路径的无 directive flags**

将 `smart_compact` 的 action 遍历和 `set_truncated` 替换为：

```rust
let saved = super::planner::plan_micro(transcript, config, true)
    .estimated_tokens_saved;
let affected = super::micro::micro_compact(transcript, config);
(affected, saved)
```

保留现有 deprecated warning。这样 Smart 不会留下 `truncated=true, projection=None` 的损坏状态，且与 Micro 使用同一 policy version、字段级语义和持久化指令。

更新 Smart 测试：短 input/result 的旧 exchange 预期 `affected == 0`；使用 `"x".repeat(501)` 的 long input/result 才预期有影响。删除任何依赖“每个旧 exchange 固定两个 action”的断言。

- [ ] **Step 9: 运行迁移与持久化测试**

Run:

```bash
cargo test -p peri-agent --lib test_plan_from_persisted_directives_rejects_v1_agent_input_directive -- --nocapture
cargo test -p peri-agent --lib test_micro_compact_replaces_v1_directive_when_v2_actions_exist -- --nocapture
cargo test -p peri-agent --lib test_micro_compact_clears_v1_directive_when_no_v2_actions_exist -- --nocapture
cargo test -p peri-agent --lib compact_v2::smart::tests -- --nocapture
cargo test -p peri-agent --lib test_update_message_flags_persists_projection -- --nocapture
```

Expected: 所有测试通过；Smart 不会产生缺 directive 的 truncated flag；旧 directive 不能影响 LLM view。

---

### Task 5: 让 Micro 持久化与统计只反映真实变化

**Files:**
- Modify: `peri-agent/src/agent/compact_v2/micro.rs`
- Modify: `peri-agent/src/agent/compact_v2/micro_test.rs`
- Modify: `peri-agent/src/agent/compact_v2/mod.rs`
- Modify: `peri-agent/src/agent/compact_v2/planner.rs`
- Modify: `peri-agent/src/agent/compact_v2/planner_test.rs`

- [ ] **Step 1: 写 no-op 持久化失败测试**

在 `micro_test.rs` 构造 6 个过期 turn，每个 `Bash` input/result 都不超过 500 字符。新增：

```rust
#[test]
fn test_micro_compact_does_not_persist_flags_when_all_candidates_are_short() {
    let affected = micro_compact(&mut transcript, &CompactConfig::default());
    assert_eq!(affected, 0);
    assert!(transcript.entries().iter().all(|entry| {
        transcript.flags(entry.message.id()) == MessageFlags::default()
    }));
}
```

- [ ] **Step 2: 运行 no-op 测试确认当前实现失败**

Run:

```bash
cargo test -p peri-agent --lib test_micro_compact_does_not_persist_flags_when_all_candidates_are_short -- --nocapture
```

Expected: 当前 Planner 对短 input/result 仍会生成 action，因此测试在 Task 2 前失败；若 Task 2 已完成，应直接通过，作为防回归检查。

- [ ] **Step 3: 用 per-message directive 分组计算影响数**

保持 `micro_compact` 用 `HashMap<MessageId, Vec<ProjectionActionEntry>>` 分组，再以 `directives_by_msg.len()` 作为唯一 `affected` 值。不得使用 `plan.actions.len()`，因为同一 AI message 的多个长字段或 tool call 只能算一个 changed message。

在 `micro_test.rs` 新增：

```rust
#[test]
fn test_micro_compact_counts_one_message_with_two_long_fields_once() {
    // 一个旧 AI tool call 有 prompt 和 content 两个 501-char 顶层字段。
    // 断言 directive entries 中 fields 已含两个字段，但 affected == 1（若无 long ToolResult）。
}
```

- [ ] **Step 4: 为 `changed_fields` / `no_op_candidates` 的可观测统计建立纯 helper**

在 `planner.rs` 添加一个只读报告函数，不改变现有 `MicroCompactPlan` 的公开字段：

```rust
pub fn report_plan(plan: &MicroCompactPlan, candidate_count: usize) -> ApplyReport
```

实现：

```rust
let changed_messages = plan
    .actions
    .iter()
    .map(|action| action.message_id)
    .collect::<std::collections::HashSet<_>>()
    .len();
let changed_fields = plan.actions.iter().map(|action| match &action.action {
    ProjectionAction::CompactToolInput { fields, .. } => fields.len(),
    ProjectionAction::CompactToolResult { .. } | ProjectionAction::ReplaceMedia { .. }
    | ProjectionAction::CompactText { .. } => 1,
    ProjectionAction::Keep | ProjectionAction::Exclude => 0,
}).sum();
```

`candidate_count` 是通过 stale window、非保护工具筛选后的 tool exchange 数；`no_op_candidates` 使用 `candidate_count.saturating_sub(changed_messages)`。不要改 ACP/TUI event schema，本任务仅保证 planner/micro 内部统计可被 Compact stage 后续接入。

在 `planner_test.rs` 添加一个 plan：同一 message 有两个字段 + 一条长 ToolResult，断言 `changed_messages == 2`（AI + Tool），`changed_fields == 3`，且候选数大于等于 changed messages 时 no-op 为非负。

- [ ] **Step 5: 将现有 Micro 日志改为真实指标并确认不泄露内容**

在 `micro.rs` 的 debug 字段中记录：

```rust
let report = super::planner::report_plan(&plan, candidate_count);
debug!(
    affected = report.changed_messages,
    changed_fields = report.changed_fields,
    no_op_candidates = report.no_op_candidates,
    estimated_tokens_saved = report.estimated_tokens_saved,
    "Micro Compact: 持久化字段级 projection directive"
);
```

日志不得包含 tool input、ToolResult、prompt 或任何截断前后文本。

- [ ] **Step 6: 调整 `run_compact` / Smart 测试的旧“固定 action 数”断言**

检查 `compact_v2/mod.rs`、`micro_test.rs`、`smart.rs` 中预期 `affected > 0` 或固定 `4 stale rounds × 2 actions` 的测试。把 fixture 中普通短参数改为 501 字符的明确长字符串，或将预期改为 0；不得为了保留旧测试数量而降低阈值。

Run:

```bash
cargo test -p peri-agent --lib compact_v2::micro::tests -- --nocapture
cargo test -p peri-agent --lib compact_v2::smart::tests -- --nocapture
cargo test -p peri-agent --lib compact_v2::planner::tests -- --nocapture
```

Expected: 影响数是按 message 去重后的真实数量，短候选全部 no-op。

---

### Task 6: 全链路回归验证、文档状态和最终检查

**Files:**
- Modify: `spec/issues/2026-07-29-micro-compact-loses-agent-tool-context.md`
- Modify: `spec/issues/2026-07-29-micro-compact-field-level-design.md`（仅在实现与设计不一致时更新；否则不改）
- Verify: `peri-agent/src/agent/compact_v2/{config,planner,projection,micro,smart}.rs`
- Verify: `peri-agent/src/agent/stages/reason.rs`

- [ ] **Step 1: 添加最终端到端纯逻辑回归测试**

在 `projection_test.rs` 添加：

```rust
/// [回归测试] Micro Compact 不能删除 Agent tool input 的必填字段。
///
/// 历史背景：v1 将整个 arguments object 替换为 `_compact_note`；模型可能将该
/// 占位对象作为新的 Agent 调用回显，随后 SubAgentTool 因缺少 prompt 返回
/// `Error: missing required parameter prompt`。
#[test]
fn test_micro_projection_keeps_agent_required_fields_and_never_emits_compact_note() {
    // 1. 构造至少 4 个 turn，令第一个 Agent exchange 过期。
    // 2. Agent input 包含 501-char prompt、subagent_type、description、cwd、fork。
    // 3. 用 plan_micro(..., false) 与 render_llm_view 生成 view。
    // 4. 找到 call id 为 "agent_0" 的 ToolCallRequest。
    // 5. 断言所有短字段等于原始值，prompt 包含省略标记，且 JSON 中不含 _compact_note。
}
```

测试不调用真实 LLM、ACP 或 SubAgentTool；它锁定导致执行失败的真实边界——进入下一次 LLM 请求的历史 view。

- [ ] **Step 2: 运行最终回归测试**

Run:

```bash
cargo test -p peri-agent --lib test_micro_projection_keeps_agent_required_fields_and_never_emits_compact_note -- --nocapture
```

Expected: PASS；断言消息应明确显示哪个必填字段不一致。

- [ ] **Step 3: 运行目标测试组**

Run:

```bash
cargo test -p peri-agent --lib compact_v2 -- --nocapture
cargo test -p peri-agent --lib test_update_message_flags_persists_projection -- --nocapture
cargo check -p peri-agent
cargo test -p peri-agent --lib
cargo test -p peri-agent --doc
git diff --check
```

Expected: 全部通过。若任何测试失败，先按失败所属模块修正，不要跳过或删除测试。

- [ ] **Step 4: 审阅跨层不变量**

人工确认：

1. `Reason` 传给 `generate_reasoning` 的 `messages_snapshot` 在 v2 action 下从未含 `_compact_note`。
2. `ToolStarted.input` 仍只从当前 `Reasoning.tool_calls` 产生；没有新增执行历史 tool call 的路径。
3. `ContentBlock::ToolUse.input` 与 `tool_calls[].arguments` 使用同一个 projected `ToolCallRequest`。
4. `AskUserQuestion`、`goal`、`TodoWrite` 与 retention metadata 工具没有生成 Micro action。
5. error ToolResult、Image/Document Base64 处理符合 design：前者完整保留，后者仍替换 payload。
6. 除 active issue 和 design 外，不更新 root `CLAUDE.md`、standards 或 TUI 文档，因为稳定路由、跨层契约和用户接口均未改变。

- [ ] **Step 5: 更新 issue 修复记录，但不将状态标为 Verified**

实现与全部验证通过后，在 `spec/issues/2026-07-29-micro-compact-loses-agent-tool-context.md`：

1. 将 `**状态**：Open` 更新为 `**状态**：Fixed`。
2. 在状态变更表追加：

```markdown
| 2026-07-29 | Open | Fixed | agent | Micro 改为顶层长字符串字段级截断，移除整体 `_compact_note` 替换，并加入 v1 directive 安全迁移与回归测试。 |
```

3. 将修复记录占位替换为：

```markdown
### 修复 #1（2026-07-29）

- **操作人**：agent
- **用户原意**：Micro 只 compact 长字段，短字段不处理，避免显示层和 Agent 持续失忆。
- **修复内容**：Micro Planner 仅选择超过 500 字符的 tool input 顶层字符串及成功 ToolResult；Projection 保留 JSON 结构和短字段，使用 350/100 Unicode 安全截断；旧 policy version 1 directive 安全回退并在后续 compact 时迁移或清理。
- **涉及 commit**：无（未按请求提交）
- **验证状态**：待验证
```

不要标为 `Verified`，因为该状态需要用户在真实 `peri-tui` 长会话中确认。

- [ ] **Step 6: 报告验证与请求真实会话验证**

报告必须包含：

- 改动的文件列表；
- 关键回归测试和完整 `peri-agent` 测试的实际结果；
- `git diff --check` 结果；
- 未创建 commit；
- 需要用户在 `peri-tui` 进行的验证步骤：长会话触发 Micro → 调用 Agent → 确认工具卡不显示 `_compact_note`，Agent 不再报 `missing required parameter prompt`，并确认任务语义能延续。

---

## 覆盖映射

| Approved design 需求 | 实施任务 |
| --- | --- |
| 500/350/100 配置与 Unicode 长度 | Task 1、Task 3 |
| 仅顶层字符串，短字段/结构不变 | Task 2、Task 3 |
| 长成功 ToolResult，错误完整保留 | Task 2、Task 3 |
| 保留工具名单与媒体处理 | Task 2、Task 6 |
| version 2 directive、v1 安全迁移 | Task 4 |
| 实际差值收益估算和 no-op | Task 3、Task 5 |
| Agent `prompt` 失忆回归 | Task 3、Task 6 |
| provider 同步和 Reason 消费路径 | Task 3、Task 4、Task 6 |
| issue 状态与人工验收 | Task 6 |
