# Micro Compact 字段级压缩设计

**状态**：Approved
**日期**：2026-07-29
**关联 Issue**：[自动 micro compact 后 Agent 工具上下文丢失并持续失忆](./2026-07-29-micro-compact-loses-agent-tool-context.md)

## 1. 背景

当前 Micro Compact 会把一个历史工具调用的完整 input object 替换为：

```json
{"_compact_note":"tool input compacted"}
```

该行为虽然保持了 JSON object 根类型，却删除了 `prompt`、`subagent_type`、`description` 等短字段和必填字段。对于 `Agent` 等工具，模型可能根据这段不完整历史生成新的缺参调用，最终触发 `missing required parameter prompt`。即使没有触发工具错误，整体替换也会使后续 LLM 视图丢失已经委派的任务语义。

Micro Compact 的职责应收窄为：**只回收明显偏长、可安全截断的 payload，短内容和结构信息原样保留。** 对上下文进行语义总结或移除整段历史属于 Full Compact 的职责。

## 2. 目标

1. Micro Compact 只截断超过阈值的长字符串，不再整体替换 tool input。
2. 保留工具调用的 JSON 结构、字段名、短字段、必填参数和类型信息。
3. 短 ToolResult 不做无收益投影；长成功结果按相同长度策略截断。
4. 错误 ToolResult 保持完整，避免丢失诊断信息。
5. Planner 明确决定压缩对象，Projection 仅执行持久化计划；同一 directive 的结果不随运行时配置漂移。
6. 节省量估算基于实际被移除的字符，而不是按整条消息固定比例猜测。
7. 保持 OpenAI `tool_calls.arguments` 与 Anthropic `ContentBlock::ToolUse.input` 一致。

## 3. 非目标

本次设计不处理以下内容：

- 不递归压缩嵌套 object 或 array 中的字符串。
- 不压缩顶层 array、object、number、boolean 或 null 字段。
- 不总结长字段的语义，只做确定性的 Unicode 安全 head/tail 截断。
- 不调整 Micro Compact 和 Full Compact 的触发阈值。
- 不取消现有工具保护名单。
- 不改变 Image/Document Base64 的现有占位替换策略。
- 不修改 TUI、ACP 映射或工具执行接口。

## 4. 核心语义

### 4.1 默认长度配置

在 `CompactConfig` 增加三个字段：

```rust
micro_field_threshold_chars: usize = 500
micro_field_keep_head_chars: usize = 350
micro_field_keep_tail_chars: usize = 100
```

长度统一按 Unicode scalar value 数量计算，即 Rust 的 `text.chars().count()`，不得使用 UTF-8 字节长度。

压缩条件为严格大于：

```text
field_chars > micro_field_threshold_chars
```

因此长度恰好为 500 的字符串保持不变。

有效配置必须满足：

```text
threshold > 0
keep_head + keep_tail < threshold
```

若配置无效，Planner 不生成字符串截断 action，并通过 `tracing::warn!` 记录结构化诊断；Micro 由现有收益判断决定 no-op 或升级 Full Compact。不得通过切片尝试“尽量执行”，也不得 panic。

### 4.2 截断格式

超过阈值的字符串保留前 350 个字符和后 100 个字符，中间插入：

```text
\n... [N 字符已省略] ...\n
```

其中：

```text
N = original_chars - keep_head - keep_tail
```

截断结果必须通过字符迭代构造，保证 CJK、emoji 和组合文本不会在 UTF-8 字节中间断开。

### 4.3 Tool input

Planner 只检查 tool input 根 object 的顶层字段：

| 顶层值类型 | 长度/条件 | Micro 行为 |
| --- | --- | --- |
| String | `chars <= 500` | 原样保留 |
| String | `chars > 500` | 记录字段名，按 head/tail 截断 |
| Number / Boolean / Null | 任意 | 原样保留 |
| Array / Object | 任意 | 原样保留，不递归 |
| 非 object 根 | 任意 | 原样保留 |

只有至少一个顶层长字符串字段时，Planner 才生成 `CompactToolInput` action。字段名必须稳定排序后写入 directive，避免序列化顺序依赖 map 迭代实现。

Projection 克隆原始 input object，只替换 directive 中列出的字符串字段。未列出的字段逐值保持不变，不添加 `_compact_note`，不删除字段，也不改变字段类型。

示例：

```json
{
  "prompt": "<1200 chars>",
  "subagent_type": "explorer",
  "description": "定位 compact",
  "fork": false,
  "metadata": {"nested_long_text": "<2000 chars>"}
}
```

投影后：

```json
{
  "prompt": "<前350>\n... [750 字符已省略] ...\n<后100>",
  "subagent_type": "explorer",
  "description": "定位 compact",
  "fork": false,
  "metadata": {"nested_long_text": "<2000 chars>"}
}
```

`metadata.nested_long_text` 不在本次递归范围内，因此保持完整。

### 4.4 ToolResult

ToolResult 使用相同的默认长度策略：

| 结果 | 长度 | Micro 行为 |
| --- | --- | --- |
| `is_error = true` | 任意 | 完整保留 |
| `is_error = false` | `chars <= 500` | 完整保留，不生成 action |
| `is_error = false` | `chars > 500` | 保留头 350 + 尾 100 |

只有实际超过阈值的成功 ToolResult 才生成 `CompactToolResult` action。原有 recovery handle 保留行为继续生效，但不能使短结果进入压缩计划。

### 4.5 工具保护名单

现有保护名单维持不变：

- `AskUserQuestion`
- `goal`
- `TodoWrite`

`tool_retention_map` 中标记为 `Preserve` 或 `StateBearing` 的工具继续整体跳过 Micro Compact。保护判断优先于字段长度判断。

`Agent` 不加入保护名单；其顶层长 `prompt` 可以被截断，但 `subagent_type`、`description`、`fork`、`cwd` 等短字段必须保留。

### 4.6 媒体 block

Image/Document Base64 payload 继续使用现有 `ReplaceMedia` 行为，以文本占位保留标题、MIME 或引用信息，并移除大体积二进制编码。媒体处理不受 500 字符阈值约束，因为它不是 tool input/result 字符串字段策略的一部分。

## 5. 数据模型与版本

### 5.1 Projection action

`CompactToolInput` 需要携带确定性投影所需的全部信息：

```rust
CompactToolInput {
    fields: Vec<String>,
    keep_head: usize,
    keep_tail: usize,
}
```

`fields` 仅包含 Planner 已确认超过阈值的顶层字符串字段。阈值不必写入 action，因为是否超过阈值已经由 Planner 决定；head/tail 必须写入 action，保证持久化 directive 在配置变化后仍产生相同投影。

现有 `preserve_shape` 不再需要。新语义始终保留 object 根和全部未压缩字段。

`CompactToolResult` 继续携带 `keep_head`、`keep_tail` 和 `preserve_recovery_handle`。

### 5.2 Policy version

projection policy version 从 `1` 提升到 `2`。所有以下位置必须引用同一版本常量，禁止继续散落字面量：

- Planner 生成的 `MicroCompactPlan`
- `MessageProjectionDirective`
- Compact 阶段持久化
- Reason 阶段恢复 directive

旧版 directive 的 `fields=[]` 表示“整体替换”，不得在新版解释为任何字段压缩，也不得继续生成 `_compact_note`。

对旧 directive 的迁移采用安全策略：

1. Reason 遇到 policy version 1 时，当前请求使用原始可见消息，不应用旧投影。
2. 新版 Planner 可以重新考虑“已 truncated 但 directive 版本过旧”的消息；只有当前 policy version 2 的 truncated 消息才按 `skip_existing_truncated` 跳过。
3. Planner 根据原始 transcript 重新生成字段级 action，并由 Compact 阶段以 version 2 directive 覆盖旧 directive。
4. 如果原消息没有任何长字段或长结果，则清除该消息过时的 projection/truncated 状态，避免它永久处于无实际压缩效果的标记状态。

迁移过程中任何失败都必须回退到原始消息，不得回退到 `_compact_note` 投影。

## 6. Planner 行为

Planner 的顺序为：

1. 按现有 `micro_compact_stale_steps` 跳过最近 TurnGroup。
2. 检查 tool retention 和保护名单；受保护工具直接跳过。
3. 检查 tool input：
   - 仅根 object；
   - 收集顶层 `String` 且长度超过阈值的字段；
   - 稳定排序字段名；
   - 非空时生成一个 per-tool-call `CompactToolInput` action。
4. 检查 ToolResult：
   - 错误结果跳过；
   - 成功结果只有超过阈值时生成 `CompactToolResult` action。
5. 只有消息内存在真实 action 时，才持久化 projection 并标记 `truncated`。
6. 没有 action 的候选计入 `no_op_candidates`，但不计入 `affected_count`、`changed_messages` 或 `changed_fields`。

同一 AI 消息有多个 tool call 时，每个 tool call 独立规划；一个调用包含长字段不能影响同消息中的其他调用。

## 7. Projection 行为

Projection 必须满足：

- 是纯函数，不修改 transcript 和配置。
- 从原始 `ToolCallRequest.arguments` 克隆 object。
- 仅处理 action 中列出的字段。
- action 中字段不存在、已不再是字符串或长度不足以执行 head/tail 时，保留原值并继续，不报错、不删除字段。
- 同步更新 `BaseMessage::Ai.tool_calls` 和 `ContentBlock::ToolUse.input`。
- 投影后继续运行现有 tool call/result 配对及 provider protocol 验证。
- 不生成 `_compact_note`。

## 8. 节省量估算与报告

当前“有 action 就假设整条消息缩到三分之一”的估算必须替换为 action 级确定性估算。

### 8.1 Tool input 字段

对每个计划字段：

```text
before_chars += original_field_chars
after_chars += keep_head + keep_tail + omission_marker_chars
```

只有 `after_chars < before_chars` 时才计入节省量。省略标记的实际字符数必须包含动态数字位数。

### 8.2 ToolResult

按实际原始结果和 `apply_head_tail` 结果的字符差计算。

### 8.3 Token 近似

保持当前 `chars / 4` 的粗略 token 换算，但输入必须是真实字符差，而不是整条消息比例。CJK token 估算精度问题不在本次范围内。

### 8.4 统计语义

- `changed_fields`：实际被截断的 tool input 顶层字段数量。
- `changed_messages`：至少有一个字段、结果或媒体 block 实际变化的消息数量，按 message id 去重。
- `affected_count`：沿用事件契约，但不得把 no-op 候选计为已影响。
- `estimated_tokens_saved`：所有 action 的实际估算节省总和。
- `no_op_candidates`：通过 stale/retention 筛选但没有任何超过阈值内容的候选数量。

## 9. 配置与兼容性

新增配置字段使用 serde default，旧配置文件无需迁移即可获得 500/350/100 默认值。配置命名遵循现有 `CompactConfig` snake_case Rust 字段与项目序列化约定。

本次不新增环境变量覆盖。Micro 参数先通过配置文件和默认值控制，避免扩大配置入口数量。

Full Compact、Smart Compact 兼容字段和触发阈值不在本次修改范围内。若 Smart 路径仍复用 Micro Planner，则自动继承字段级安全语义，不保留旧整体替换行为。

## 10. 测试设计

测试遵循 `docs/design/testing-standards.md`，纯逻辑覆盖放在 compact_v2 邻近的 `*_test.rs`。

### 10.1 Planner 测试

1. 顶层字符串长度 500：不生成 action。
2. 顶层字符串长度 501：生成 action，`fields` 只含该字段。
3. 同一 input 中长短字符串混合：只记录长字段。
4. 嵌套 object 和 array 包含超长字符串：不生成相应字段 action。
5. 非 object 根 input：不生成 action。
6. 多个长字段：字段名稳定排序。
7. 保护名单工具含长字段：不生成 action。
8. Agent 长 `prompt`：生成只针对 `prompt` 的 action。
9. 短成功 ToolResult：不生成 action。
10. 长成功 ToolResult：生成 head 350/tail 100 action。
11. 任意长度错误 ToolResult：不生成 result action。
12. 无效长度配置：不生成字符串截断 action且不 panic。

### 10.2 Projection 测试

1. 长 `prompt` 被 head/tail 截断，短必填字段逐值相等。
2. 不再出现 `_compact_note`。
3. action 未列出的长字段保持不变。
4. action 指向不存在或非字符串字段时安全 no-op。
5. CJK 和 emoji 在字符边界截断。
6. OpenAI tool_calls 与 Anthropic ToolUse block 的 input 完全一致。
7. ToolResult 500/501 边界行为正确。
8. 错误 ToolResult 完整保留。

### 10.3 持久化与迁移测试

1. version 2 directive serde roundtrip 保留 fields/head/tail。
2. version 1 directive 不被新版 Projection 应用。
3. version 1 truncated 消息可被新版 Planner 重新规划并覆盖。
4. version 1 消息无长字段时清除过时 projection/truncated 状态。
5. Reason 恢复 version 2 directive 后多轮投影结果确定且一致。

### 10.4 回归测试

增加带历史背景的回归测试：

```text
历史 Agent tool call 包含短 prompt 和其他必填字段
→ Micro Compact
→ LLM view 中 Agent input 与原 input 完全相等
→ 不存在 _compact_note
→ 不可能因投影本身触发 missing required parameter prompt
```

另增加长 Agent prompt 场景，确认只有 prompt 内容缩短，工具 schema 所需字段仍存在。

### 10.5 验证命令

```bash
cargo test -p peri-agent --lib compact_v2
cargo check -p peri-agent
cargo test -p peri-agent --lib
cargo test -p peri-agent --doc
git diff --check
```

如果改动触及 compact 事件统计映射，再运行：

```bash
cargo test -p peri-acp --lib mapper
```

## 11. 涉及模块

预计实现范围：

- `peri-agent/src/agent/compact_v2/config.rs`：新增默认配置和有效性检查。
- `peri-agent/src/agent/compact_v2/planner.rs`：字段筛选、短结果 no-op、真实收益估算和旧 directive 重规划。
- `peri-agent/src/agent/compact_v2/projection.rs`：字段级投影、policy version 常量和旧语义移除。
- `peri-agent/src/agent/compact_v2/micro.rs`：仅对真实 action 持久化标记，并处理旧 directive 覆盖/清理。
- `peri-agent/src/agent/stages/reason.rs`：统一使用 policy version 常量并安全处理旧 directive。
- compact_v2 邻近测试文件：边界、迁移和回归覆盖。

不需要修改：

- `peri-tui`：只读显示服务端工具事件。
- `peri-acp`：只透传 `ToolStart.input`。
- `peri-middlewares` 的 Agent 工具：`prompt` 校验保持正确。

## 12. 验收标准

实现完成必须同时满足：

1. Micro 不再把任何 tool input 整体替换为 `_compact_note`。
2. 所有长度不超过 500 的顶层字符串字段逐值保持不变。
3. 超过 500 的顶层字符串只截断自身，保留前 350 和后 100 个字符。
4. Tool input 的所有 key、根类型和未选中 value 类型保持不变。
5. 嵌套 object、array 和非字符串字段完全不变。
6. 短成功 ToolResult 不投影；长成功 ToolResult 按 350/100 截断；错误结果始终完整。
7. 保护名单和媒体替换行为维持现状。
8. 旧 version 1 directive 不再产生整体参数替换，并能安全迁移或清除。
9. 节省量和 changed/no-op 统计只反映实际变化。
10. Agent 的短 `prompt` 不因 Micro 丢失，回归测试覆盖原始报错链。
11. 所有目标测试、`cargo check -p peri-agent` 和 `git diff --check` 通过。
