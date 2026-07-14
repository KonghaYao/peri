# Perihelion Context Compact 性能分析报告

> 分析范围：`peri-agent/src/agent/stages/compact.rs` + `compact_v2.rs` + `token.rs` + `compact/config.rs`
> 分析方法：代码静态分析，非 benchmark 实测

---

## 1. Compact 触发逻辑

### 1.1 触发时机

Compact 在 **ReAct 循环每轮开头** 无条件执行（可跳过），发生在 Reason（LLM 调用）之前。

`peri-agent/src/agent/stages/mod.rs:531`
```rust
// ── Compact ──
let _compact_out = match compact::run_compact(CompactInput { ... }).await
```

**🟡 中等风险：每轮必定经过 compact 检查，即使预算远低于阈值**

每次 ReAct step 都要执行 budget 计算 + 条件判断，随着 step 增长（长任务可到数百轮），累积的检查开销不可忽视。好在这部分只是读 token_tracker 和做除法，CPU 开销极小。

### 1.2 预算计算公式

`peri-agent/src/agent/token.rs:82-84`
```rust
pub fn context_usage_percent(&self, context_window: u32) -> Option<f64> {
    self.estimated_context_tokens()
        .map(|used| (used as f64 / context_window as f64) * 100.0)
}
```

`estimated_context_tokens` 的计算：
`peri-agent/src/agent/token.rs:71-79`
```rust
self.last_usage.as_ref().map(|u|
    u.input_tokens as u64 + self.estimated_tool_tokens_since_last_llm
)
```

**🟡 中等风险：工具结果 token 估算是 chars/4 近似，中文场景偏差可达 2-3x**

`peri-agent/src/agent/token.rs:63-68`
```rust
let estimated = (tool_output.chars().count() / 4) as u64;
```

对于 CJK 文本，实际 token 数约为 `chars/2`（中文每个字约 1.5-2 tokens），`chars/4` 会**低估**中文工具输出的 token 占用。如果 agent 操作包含大量中文文件，实际 context 使用率可能比预算显示高 50-100%。

**🟢 良好实践：`estimated_tool_tokens_since_last_llm` 在 LLM `accumulate` 时清零，避免双计**

`peri-agent/src/agent/token.rs:56` — 每次 LLM 调用后重置为 0。

### 1.3 三级阈值

`peri-agent/src/agent/stages/compact.rs:1-6`

| 预算占用 | 策略 | 门槛文件 |
|---------|------|---------|
| < 0.70 | 跳过（无操作） | `config.rs:13-14`: `DEFAULT_WARNING_THRESHOLD` |
| 0.70–0.85 | **Micro** Compact | `config.rs:14`: `DEFAULT_MICRO_COMPACT_THRESHOLD` |
| ≥ 0.85 | **Full** Compact | `config.rs:11`: `DEFAULT_AUTO_COMPACT_THRESHOLD` |

**🟢 良好实践：三级分治，Micro 免 LLM 调用，Full 才走 LLM。**

### 1.4 连续失败降级

`peri-agent/src/agent/compact_v2.rs:98-107`
```rust
if *consecutive_failures >= config.max_consecutive_failures {
    // 连续失败超限则跳过，默认 max_consecutive_failures = 3
}
```

**🟢 良好实践：防止 Full Compact 失败引发死循环反复重试。**

---

## 2. 压缩算法复杂度

### 2.1 Micro Compact

`peri-agent/src/agent/compact_v2.rs:202-292`

**流程：**
1. `compute_round_starts()` — **O(n)** 单次遍历，按 AI+Tool 对分组
2. `round_index` 构建 — **O(n)** 为每条消息建立 round 映射
3. 遍历所有自有消息（除 ancestor）— **O(n)**，检查：
   - 是否在最近 `micro_compact_stale_steps`（默认 5）轮内 → 跳过
   - 是否已被 truncated → 跳过
   - Tool 消息：查工具名是否在白名单 + 非 error
   - 非 Tool 消息：检查是否含 Image/Document
4. `set_truncated()` — **O(1)** HashMap 写 + 异步持久化（unbounded channel，不阻塞）

**总体时间复杂度：O(n)**，其中 n 为 transcript 消息数。无 LLM 调用。

**🔴 严重风险：`find_tool_name_in_entries` 对每条 Tool 消息做 O(n) 反向扫描**

`peri-agent/src/agent/compact_v2.rs:295-309`
```rust
fn find_tool_name_in_entries(entries, tool_call_id) -> Option<String> {
    for entry in entries.iter().rev() {
        if let BaseMessage::Ai { tool_calls, .. } = &entry.message {
            for tc in tool_calls {
                if tc.id == tool_call_id { return Some(tc.name.clone()); }
            }
        }
    }
    None
}
```

每条 Tool 消息都重新从尾部倒序扫描整个 entries 列表。当 transcript 有数千条消息且每轮都有多个 Tool 调用时，Micro Compact 的实际复杂度接近 **O(n²)**。

举例：一个 500 轮的任务，每轮 1 个 AI + 1 个 Tool，共 1000 条消息。Micro 需处理约 500 条 Tool 消息，每条平均扫描 250 项 → 约 125,000 次迭代，虽非天文数字但完全可避免。

**建议**：在 `compute_round_starts` 时并行构建 `HashMap<tool_call_id, tool_name>`，将函数降为 O(1) 查询。

### 2.2 Full Compact

`peri-agent/src/agent/compact_v2.rs:355-472`

**流程：**
1. `visible_messages()` — **O(n)** filter
2. `preprocess_messages_for_summary(2000 char/message)` — **O(n)** 逐条格式化
3. `llm.invoke()` — **O(1) 次 LLM 调用**，summary_max_tokens 默认 16000
4. `postprocess_summary()` — 字符串处理，移除 `<analysis>`/提取 `<summary>`
5. 所有旧消息标 `excluded` — **O(n)** HashMap 写入
6. 追加 Human 摘要消息 — **O(1)** append
7. `re_inject_v2()` — 文件读取（spawn_blocking）+ 并行 I/O

**总体时间复杂度：O(n) 本地 + 1 次 LLM 调用 + k 次文件 I/O。**

**🟡 中等风险：re_inject 文件读取通过 `futures::future::join_all` 并行，但每个文件默认预算 5000 tokens，最多 5 个文件（25000 tokens 总预算）+ 25000 tokens skills 预算**

`peri-agent/src/agent/compact_v2.rs:828-833`
```rust
let mut file_futures = Vec::new();
for path in &resolved_paths {
    file_futures.push(read_file_with_budget(path, config.re_inject_max_tokens_per_file));
}
let file_contents = futures::future::join_all(file_futures).await;
```

如果 re-inject 的文件较大（如 5000 tokens × 4 char/token = 20KB），5 个文件共 100KB 的磁盘 I/O。`tokio::task::spawn_blocking` 避免了阻塞 async runtime，设计合理。

---

## 3. CompactMessage 选择策略

### 3.1 Micro Compact 白名单

`peri-agent/src/agent/compact/config.rs:5`
```rust
const DEFAULT_COMPACTABLE_TOOLS: &[&str] = &["Bash", "Read", "Glob", "Grep", "Write", "Edit"];
```

**规则：**
- **只操作自有消息**（ancestor_len 之后），`compact_v2.rs:210`
- **保留最近 5 轮**（`micro_compact_stale_steps`），`compact_v2.rs:214`
- **白名单工具的 Tool 消息** 标 `truncated`，`compact_v2.rs:259-262`
- **含 Image/Document 的消息** 标 `truncated`（无论角色），`compact_v2.rs:265-273`
- **Tool error 不 truncate**，`compact_v2.rs:256-258`
- **已 truncated 的不重复标记**，`compact_v2.rs:244-247`

**🟢 良好实践：**
- 白名单设计合理——这 6 类工具输出通常是 disposable 的
- 保留最近 5 轮防止 agent 仍需要引用最近的工具结果
- 跳过 tool error 防止丢失关键调试信息

**🟡 中等风险：白名单不可用户配置（硬编码默认值）**

`config.rs:58` 虽有 `micro_compactable_tools` 字段，但 `config_test.rs` 未包含自定义工具的白名单测试。如果用户依赖第三方 MCP 工具产生大量输出且工具名不在白名单，Micro Compact 对其无效果。

### 3.2 Full Compact 全量替换

`peri-agent/src/agent/compact_v2.rs:432-447`
```rust
// 5. 所有旧消息标 excluded
for id in &old_ids {
    transcript.set_excluded(*id, true);
}
// 6. 追加 Human 摘要消息
transcript.append(BaseMessage::human(hint_text));
```

**🟢 良好实践：标记而非删除** — 旧消息仍然保留在 `entries` 中供 re_inject 提取，`visible_messages()` 自动过滤 `excluded` 标记。

**🟡 中等风险：全部 excluded 后仅靠摘要恢复状态**

Full Compact 把整个对话历史替换为一段 LLM 摘要 + re-injected 文件 + skills。如果 LLM 摘要丢失了关键决策信息（如一次失败的尝试为何被放弃），agent 可能重复犯错。9 段模板（`compact_v2.rs:32-45`）覆盖面广但质量依赖 LLM 本身。

### 3.3 Re-inject 智能提取

`peri-agent/src/agent/compact_v2.rs:678-706` — 从全部 entries（含 excluded）倒序遍历，提取最近通过 `Read` 工具读取的文件路径，去重，最多 5 个。

**🟢 良好实践：倒序遍历保证取到"最近"的文件；从全部 entries 而非 visible_messages 提取，确保被 excluded 的消息中的 Read 调用也不丢失。**

**🟡 中等风险：仅提取 `Read` 工具的文件路径，不包含 `Write`/`Edit` 操作的路径**

`compact_v2.rs:683-684`
```rust
for tc in msg.tool_calls() {
    if tc.name == "Read" { ... }
}
```

`Write` 和 `Edit` 工具操作的路径没有被 re-inject。虽然这些文件的内容已经在 workspace 里，但丢失这些路径意味着 compact 后 agent 不知道"我最近编辑了哪些文件"——除非 LLM 摘要在 `Files` 段里提到了。

---

## 4. 频繁触发对 LLM 轮次延迟的影响

### 4.1 Full Compact 阻塞 ReAct 循环

`peri-agent/src/agent/stages/compact.rs:100-118`
```rust
let result = tokio::select! {
    biased;
    _ = ctx.turn.cancel_token.cancelled() => { break ... }
    r = crate::agent::compact_v2::run_compact(...) => r,
};
```

**🔴 严重风险：Full Compact 的 LLM 调用阻塞整个 ReAct 循环**

Full Compact 通过 `tokio::select!` 与 cancel_token 竞争，但本质上是同步阻塞——在 Full Compact LLM 调用完成之前，Receive、Reason、Act 都无法执行。这意味着：
- 如果 Full Compact 的 LLM 耗时 5 秒，agent 在这 5 秒内无法响应任何用户输入
- 如果 Full Compact 的 LLM 耗时 30 秒（长摘要），用户会感觉 agent "卡住了"

**对比分析：Full Compact LLM 时间 vs 节省的 token 时间**

假设场景：200K context window，85% 触发 = 170K tokens 占用。
- Full Compact LLM 调用约需 3-10 秒（生成 16K token 摘要）
- Compact 后 context 降至约 3-5K tokens（摘要 + re-inject）
- 节省约 165K tokens
- 后续每轮 LLM 调用可能节省 1-3 秒（减少 prompt processing 时间）
- **收回成本需要约 3-10 轮后续调用**

**结论：对于长任务（30+ 轮），Full Compact 的延迟成本可以收回。对于短任务（触发后仅剩 2-3 轮），是净损失。**

### 4.2 Micro Compact 延迟

**🟢 良好实践：Micro Compact 零 LLM 调用，延迟仅来源于 transcript 扫描和标记写入。**

在典型场景（500-2000 条消息）下，Micro 的 CPU 时间约 1-5ms，几乎不增加轮次延迟。但如果 `find_tool_name_in_entries` 的 O(n²) 问题在极端场景下会被放大——例如 10,000 条消息、5,000 条 Tool 输出，Micro 可能耗时 50-100ms。仍可接受但值得优化。

### 4.3 触发频率

**🟡 中等风险：连续 step 可能每轮都触发预算检查**

ReAct 循环在有 `tool_calls` 时回到 Compact 阶段（`stages/mod.rs:576-581`），不做 End 检查。这意味着如果 agent 在接近 85% 阈值时频繁调工具，每轮都会做 budget 检查。如果恰好 Full Compact 被连续失败降级（3 次后跳过），则 agent 在 85% 以上区域连续跑多轮也不 compact——可能导致 context overflow。

---

## 5. 压缩后消息量减小比率

### 5.1 Micro Compact 减小量

Micro Compact 不减少消息数量，只是将部分 Tool 消息的内容标为 `truncated`。实际上 `visible_messages()` 仍返回所有消息——truncated 标记仅影响 LLM 请求构造时的消息截断逻辑。

**🟡 中等风险：Micro Compact 的 token 减少效果不易量化**

没有代码显式计算或日志记录 Micro Compact 前后的 token 节省量。`tracing::info!` 仅记录 `affected` 消息数，不含 token 估算。这使性能评估困难。

### 5.2 Full Compact 减小量

`peri-agent/src/agent/compact_v2.rs:458-463` 日志记录 `before_len` 和 `after_visible`：

```rust
debug!(before_len, after_visible, "Full Compact: excluded 旧消息 + 追加摘要 + re-inject");
```

消息数从 `before_len`（可能数百条）降至 `after_visible`（约 3-15 条：1 摘要 + N 个文件 re-inject + M 个 skills re-inject）。

**🟢 良好实践：Full Compact 后 `token_tracker.reset()` 防止误触发**

`peri-agent/src/agent/stages/compact.rs:157-161`
```rust
if r.strategy == CompactStrategy::Full {
    cx.token_tracker_mut().reset();
}
```

如果不 reset，token_tracker 仍保留 compact 前的累积值，下轮 budget 计算会立刻再次 >= 0.85。

---

## 6. Compact 异步执行

### 6.1 同步阻塞模式

`peri-agent/src/agent/stages/mod.rs:531-535`
```rust
let _compact_out = match compact::run_compact(CompactInput { ... }).await
```

**🔴 严重风险：Compact 不是异步后台任务，而是 ReAct 循环的同步步骤**

Compact 在 Reason（LLM 调用）之前执行。Full Compact 本身又调用一次 LLM。这意味着：
```
User input → [Full Compact LLM: 5-10s] → [Reason LLM: 3-5s] → Response
```
用户看到的总延迟 = Full Compact LLM + Reason LLM，可能翻倍。

**🟢 良好实践：cancel_token 支持中断**

`compact.rs:100-118` — 用户 Ctrl+C 可以在 Full Compact LLM 调用期间中断，不会永久阻塞。

### 6.2 与 MessageTranscript 的交互

`peri-agent/src/agent/stages/compact.rs:80-84`
```rust
let mut transcript_owned = {
    let mut guard = ctx.transcript.write();
    std::mem::take(&mut *guard)
};
```

**🟢 良好实践：取出 transcript 所有权后操作，避免跨 `.await` 持锁**

使用 `std::mem::take` + 操作后写回，确保 Full Compact LLM 调用期间不持有 RwLock。这允许其他路径（如 event bus、中间件）在 compact 期间并发读取 transcript（虽然在 compact 期间 transcript 是 empty 的，这是设计权衡）。

### 6.3 持久化异步

`peri-agent/src/session/transcript.rs:360-364` — `set_truncated`/`set_excluded` 通过 `unbounded_channel` 异步发送 `PersistOp::UpdateFlags`。

**🟢 良好实践：持久化不阻塞 compact 主流程。**

---

## 7. 压缩结果缓存

### 7.1 无缓存机制

**🔴 严重风险：不存在任何 compact 结果缓存**

以下场景都没有缓存：
- **同一 session 内重复 Full Compact**：每次重新调用 LLM 生成摘要，即使对话内容变化不大
- **跨 session 共享**：重新开始类似任务时，无法复用之前的 compact 摘要
- **Micro Compact 结果**：每次重新扫描 transcript 并标记，不做增量

### 7.2 Prompt Cache 间接受益

虽然有 Anthropic prompt cache（frozen system prompt），但 compact 后的消息结构（摘要 + re-inject）是 `BaseMessage::human()`，属于动态部分，**不在 cache 范围内**。因此 compact 节省的 token 主要体现在减少 prompt processing 时间，而非 cache hit。

### 7.3 TokenTracker Reset 的副作用

`compact.rs:157-161` 中 Full Compact 后 reset token_tracker，意味着 `last_usage` 被清空，下一次 `context_usage_percent` 返回 `None` 直到新一轮 LLM 调用完成。这期间 **无法计算预算**，compact 检查会直接跳过。

**🟡 中等风险：reset 后的"盲区"内无预算监控**

在 Full Compact 后第一轮 LLM 调用完成之前，如果收到大量工具输出（如在 compact 后立即执行大文件读取），无法触发二次 compact。

---

## 总结

### 风险汇总

| 风险 | 等级 | 位置 | 影响 |
|------|------|------|------|
| `find_tool_name_in_entries` O(n²) | 🔴 | `compact_v2.rs:295-309` | 长对话 Micro Compact 延迟放大 |
| Full Compact 同步阻塞 ReAct 循环 | 🔴 | `stages/compact.rs:100-118` | 用户延迟翻倍（Full + Reason 两次 LLM） |
| 无 compact 结果缓存 | 🔴 | 全局 | 重复 Full Compact 浪费 LLM 调用 |
| CJK token 估算偏小（chars/4） | 🟡 | `token.rs:64` | 中文场景 budget 低估 50-100% |
| Micro 白名单不可动态扩展 | 🟡 | `config.rs:5-58` | 第三方工具输出无法被 Micro compact |
| Re-inject 仅提取 Read 路径 | 🟡 | `compact_v2.rs:683-684` | 丢失 Write/Edit 操作的文件路径 |
| reset 后盲区无预算监控 | 🟡 | `stages/compact.rs:157-161` | 大工具输出可能撑爆 context |
| 短任务 Full Compact 净损失 | 🟡 | 全局 | 触发后仅剩 2-3 轮的 compact 不划算 |

### 良好实践

| 实践 | 等级 | 位置 |
|------|------|------|
| 三级分治（跳过/Micro/Full） | 🟢 | `stages/compact.rs:3-6` |
| 连续失败降级（max 3 次后跳过） | 🟢 | `compact_v2.rs:98-107` |
| Micro 保留最近 5 轮 | 🟢 | `compact_v2.rs:214` |
| Tool error 不 truncate | 🟢 | `compact_v2.rs:256-258` |
| Cancel token 支持中断 | 🟢 | `stages/compact.rs:100-118` |
| transcript 取出-操作-写回 避免跨 await 持锁 | 🟢 | `stages/compact.rs:80-84` |
| Full Compact 后 reset token_tracker | 🟢 | `stages/compact.rs:157-161` |
| 持久化异步不阻塞 | 🟢 | `transcript.rs:360-364` |
| `estimated_tool_tokens` 在 LLM accumulate 时清零 | 🟢 | `token.rs:56` |
| 重跑保护：清除上轮残留 excluded 标记 | 🟢 | `compact_v2.rs:139-154` |

### 改进建议优先级

1. **P0 — 修复 `find_tool_name_in_entries` O(n²)**：在 `compute_round_starts` 时并行构建 `HashMap<tool_call_id, tool_name>`，将查询降为 O(1)
2. **P0 — Full Compact 考虑后台化**：探索在后台 spawn compact task + 将结果注入 MessageQueue 的模式（类似 SubAgent bg），不阻塞主循环
3. **P1 — 增加 compact 结果缓存**：记录上次 Full Compact 的 transcript hash/摘要，重复触发时跳过 LLM 调用直接复用
4. **P1 — CJK token 估算修正**：对含 CJK 字符的工具输出使用更准确的 tokenizer（如 tiktoken-rs）
5. **P2 — Re-inject 扩展至 Write/Edit 路径**：在 `extract_recent_files` 中同时提取 Write/Edit 工具的文件参数
