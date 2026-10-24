# P1：LangfuseTracer 无条件累积 final_answer 导致长任务内存线性增长

**状态**：已修复，待 PR 合并。
**优先级**：P1。
**类型**：性能 / 可观测性
**创建日期**：2026-10-24。
**来源**：代码审计。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 e598d178）。

## 确认证据

- `peri-controller/src/langfuse/tracer/mod.rs:66` 定义 `pub(crate) final_answer: String`，注释为“累积的最终回答”。
- `peri-controller/src/langfuse/tracer/mod.rs:107` 在 `LangfuseTracer::new` 中初始化 `final_answer: String::new()`。
- `peri-controller/src/langfuse/tracer/tool_events.rs:11-14` 的 `on_text_chunk` 对每个流式文本增量执行 `self.final_answer.push_str(chunk)`，无采样判断、无上限、无清理点。
- `peri-controller/src/langfuse/bridge.rs:157-159` 在统一事件分发中把 `UnifiedLangfuseEvent::TextChunk` 无条件转发到 `on_text_chunk`。
- `peri-controller/src/langfuse/bridge/unified_event.rs:52` 定义 `TextChunk { chunk: String }` 变体。
- 变体构造点仅两处：`peri-controller/src/langfuse/bridge/v1_conversion.rs:62-64`（`ExecutorEvent::TextChunk`）与 `peri-controller/src/langfuse/bridge/v2_conversion.rs:14`（`RenderEvent::TextChunk`）。
- 生产代码对本字段零读取：`final_answer` 在 `peri-controller/src/langfuse/**` 内只出现在字段定义、初始化、`on_text_chunk` 写入和 `tracer_test.rs:404-412` 的断言中；无 serde 输出、无 flush、无清理。
- `peri-middlewares/src/hooks/executor.rs` 中的 `final_answer` 是另一类型的同名字段，与本字段无关。

## 根因链

1. ACP forwarder 把渲染层的每个 `RenderEvent::TextChunk`（及 v1 路径的 `ExecutorEvent::TextChunk`）转换为 `UnifiedLangfuseEvent::TextChunk`。
2. `LangfuseBridge::process_event` 无条件调用 `LangfuseTracer::on_text_chunk`。
3. `on_text_chunk` 把增量追加到 `LangfuseTracer::final_answer`，该字段既不在采样控制下，也没有长度上限或生命周期清理。
4. 主 Agent 与全部子 Agent 共用同一个 per-turn `LangfuseTracer`，一次 turn 内所有流式文本（含子 Agent 输出）都会进入同一 `String`。
5. 该字段没有任何读取者：Generation 事件已通过 `on_llm_end` 携带完整 LLM 输出，turn 结束时不会读取或上报 `final_answer`。因此该累积只会线性占用内存并在 turn 结束后随 tracer 释放，属于纯浪费。

## 影响面

- 长回答、长 turn、多子 Agent 并行的会话中，内存随流式文本总量线性增长，且增长量与是否采样无关（采样率为 0 时同样累积）。
- 每个增量都会触发一次 `String` 追加与可能的扩容/拷贝，额外增加流式热路径开销。
- 无功能收益：字段无读取者、无 serde 输出、无 flush 点。
- 不改变 ACP 协议输出：`ExecutorEvent::TextChunk` 仍由 `peri-acp/src/event/forwarder.rs` 原样映射到 ACP 客户端。

## 修复方案

1. 删除 `LangfuseTracer::final_answer` 字段及其初始化。
2. 删除 `LangfuseTracer::on_text_chunk`。
3. 删除 `LangfuseBridge` 中的 `UnifiedLangfuseEvent::TextChunk` 分发分支。
4. 删除 `UnifiedLangfuseEvent::TextChunk` 变体，并删除 v1/v2 两个转换分支（v1 落入既有的无映射列表，v2 落入 `_ => None`）。
5. 更新测试：删除对 `final_answer` 的断言，新增回归测试，验证文本增量不再产生任何 Langfuse 事件/状态副作用，同时 `on_llm_end` 产生的 Generation body output 仍为完整文本。
6. `peri-acp` 的协议输出路径不改；仅在编译要求时调整 forwarder。

## 验收标准

1. `peri-controller/src/langfuse/**` 内不再存在 `final_answer`、`on_text_chunk`、`UnifiedLangfuseEvent::TextChunk` 及其构造点。
2. 回归测试证明：投递多个文本增量不会产生 Langfuse 事件，也不会改变 tracer 状态。
3. `on_llm_end` 生成的 Generation body `output` 等于完整文本（`{"text": "<完整文本>"}`）。
4. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-controller --lib -- langfuse` 通过。
5. `./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp` 通过。
6. `git diff --check` 无输出，`git status --short` 仅包含允许修改的文件。

## 验证记录

基线 e598d178；以下命令均在 worktree `/Users/konghayao/code/ai/peri/.worktrees/langfuse-answer-cache` 内执行。

1. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-controller --lib -- langfuse` → `test result: ok. 165 passed; 0 failed; 0 ignored; 0 measured; 17 filtered out; finished in 5.47s`。其中新增的 `test_text_chunks_produce_no_langfuse_events` 与 `test_generation_output_remains_complete_after_text_chunks` 均通过。
2. `./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp` → `Finished dev profile [unoptimized + debuginfo] target(s) in 34.93s`，退出码 0。
3. `git diff --check` → 无输出，退出码 0。
4. `git status --short` → 仅 7 个允许修改的 `peri-controller/src/langfuse/**` 文件与新建 issue 文件，未修改 `peri-acp` 与 docs。
