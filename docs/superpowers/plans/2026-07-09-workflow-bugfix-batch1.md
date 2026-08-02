# Workflow Bugfix Batch 1 — Kill 路径修复 + compact_config 接入

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 workflow 系统中 4 个 HIGH 级 bug：kill 路径状态覆写 + 双重 JSON-RPC 响应 + compact_config 未接入

**Architecture:** 两阶段修复。阶段一（Task 1-4）修复 kill 路径：去掉 `child_handle.abort()` 避免 runner 协程中断，kill 分支 Abort msg_loop 防止 done_tx/state.json 覆写，agent task 检测 Dead 结果跳过 send_response 消除双重响应。阶段二（Task 5-7）在 `build_v2_subagent_context` 增加 compact 参数并从 `WorkflowAgentContext` 对接。

**Tech Stack:** Rust 2021 async (tokio), peri-workflow, peri-acp, peri-middlewares

---

## Scope Check

本次修复涉及单一子系统（workflow），不跨独立子系统。两条线（kill + compact）可独立测试，故拆为两个阶段。

---

## File Structure

| 文件 | 变更 | 职责 |
|------|------|------|
| `peri-workflow/src/registry.rs:149-160` | 修改 | 移除 `child_handle.abort()` |
| `peri-workflow/src/runner.rs:264-318` | 修改 | msg_loop agent task 跳过 Dead response |
| `peri-workflow/src/runner.rs:425-467` | 修改 | kill 分支 abort msg_loop |
| `peri-middlewares/src/subagent/v2_bridge.rs:48-142` | 修改 | 增加 compact_config/context_budget/compact_llm 参数 |
| `peri-acp/src/agent/workflow_agent.rs:368-379` | 修改 | 传入 compact_config 到 v2_bridge |
| `peri-workflow/src/runner_test.rs` | 无变更 | 现有 `#[ignore]` 测试保持 |

---

### Task 1: registry.kill() — 移除 child_handle.abort()

**Files:**
- Modify: `peri-workflow/src/registry.rs:149-160`

**背景:** `registry.kill()` 同时使用 `kill_tx.send(())`（触发 runner 的优雅 kill 分支）和 `child_handle.abort()`（无条件取消 tokio task）。后者会中断 runner 协程中的全部清理代码（`child.kill().await`、state.json 写入、`active_channels.remove()`、`progress_store.cleanup_completed()`），造成 Node 进程泄露、DashMap 泄露、无状态快照。

**修复:** 仅保留 `kill_tx.send(())`，由 runner 的 kill 分支全权处理清理。

- [ ] **Step 1: 修改 registry.rs kill() 方法**

将 `registry.rs:158` 的 `run.child_handle.abort();` 替换为注释说明 kill_tx 已接管。

替换前 (`peri-workflow/src/registry.rs:149-160`):
```rust
    pub fn kill(&self, run_id: &str) -> Result<(), RegistryError> {
        let run = self
            .runs
            .lock()
            .remove(run_id)
            .ok_or_else(|| RegistryError::NotFound(run_id.into()))?;
        if let Some(kill_tx) = run.kill_tx {
            let _ = kill_tx.send(());
        }
        run.child_handle.abort();
        Ok(())
    }
```

替换后:
```rust
    pub fn kill(&self, run_id: &str) -> Result<(), RegistryError> {
        let run = self
            .runs
            .lock()
            .remove(run_id)
            .ok_or_else(|| RegistryError::NotFound(run_id.into()))?;
        if let Some(kill_tx) = run.kill_tx {
            let _ = kill_tx.send(());
        }
        // kill_tx 触发 runner 的 kill 分支（runner.rs:425），其中完成所有清理：
        // workflow/kill RPC → child.kill().await → state.json → done_tx.send()
        // → active_channels.remove() → progress_store.cleanup_completed()
        // 不在此处 abort child_handle，避免中断 runner 清理路径。
        Ok(())
    }
```

注意：`runs.lock().remove(run_id)` 已经在调用前从 HashMap 移除条目（`run.child_handle` 被 move 出来）。即使后续 runner 的 msg_loop 继续运行，`done_tx` 是 `watch::Sender`，msg_loop 发送的值会被 kill 分支后续发送的值覆盖——解决方法见 Task 2。

- [ ] **Step 2: 编译验证**

```bash
cd /Users/konghayao/code/ai/perihelion && cargo build -p peri-workflow 2>&1
```

Expected: 编译通过。若 `child_handle` 无其他引用，确认 field 可保留（仅注释掉 `.abort()` 调用）。

---

### Task 2: runner kill 分支 — abort msg_loop 防止 state.json/done_tx 覆写

**Files:**
- Modify: `peri-workflow/src/runner.rs:425-467`

**背景:** kill 分支写 state.json(status="killed") 和 done_tx 后，msg_loop（独立 spawn 的 task）因 stdout 关闭退出 while 循环，以默认 `status: "failed"` 再次写 state.json 并发送 done_tx。watch channel 机制下，后到的 "failed" 覆盖先到的 "killed"。

**修复:** kill 分支在执行完清理后 `msg_loop.abort()`，阻止 msg_loop 的覆写路径。

- [ ] **Step 1: 修改 kill 分支，末尾加 msg_loop.abort()**

替换 `peri-workflow/src/runner.rs:425-467`:

替换前:
```rust
        tokio::select! {
            _ = kill_rx => {
                // 超时保护：Node crash 时不会阻塞 (M-ARCH6)
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    channel.send_request("workflow/kill", serde_json::json!({"runId": run_id})),
                )
                .await;
                let _ = child.kill().await;

                // 写入异常退出 state.json（此前仅在 msg_loop 写）
                let _stderr_tail = {
                    let lines = stderr_for_kill.lock();
                    if lines.is_empty() {
                        None
                    } else {
                        Some(
                            lines
                                .iter()
                                .rev()
                                .take(20)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("\n"),
                        )
                    }
                };
                let state = crate::journal::RunState {
                    run_id: run_id.clone(),
                    workflow_name: kill_wf_name,
                    status: "killed".to_string(),
                    return_value: None,
                    script: kill_script,
                    started_at: kill_started_at,
                    finished_at: Some(chrono::Utc::now().to_rfc3339()),
                    error: Some("workflow killed by user".to_string()),
                };
                let _ = journal_clone2.write_state(&run_id, &state);
            }
            _ = &mut msg_loop => {
                // Message loop completed naturally
            }
        }
```

替换后:
```rust
        tokio::select! {
            _ = kill_rx => {
                // 超时保护：Node crash 时不会阻塞 (M-ARCH6)
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    channel.send_request("workflow/kill", serde_json::json!({"runId": run_id})),
                )
                .await;
                let _ = child.kill().await;

                // Abort msg_loop 防止 state.json 和 done_tx 被覆写为 "failed"
                // （msg_loop 检测到 stdout 关闭后会以默认 status="failed" 写 state.json + done_tx，
                //  而 watch channel 后到值会覆盖先到值 → kill 事实丢失）
                msg_loop.abort();

                // 写入 killed state.json
                let _stderr_tail = {
                    let lines = stderr_for_kill.lock();
                    if lines.is_empty() {
                        None
                    } else {
                        Some(
                            lines
                                .iter()
                                .rev()
                                .take(20)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("\n"),
                        )
                    }
                };
                let state = crate::journal::RunState {
                    run_id: run_id.clone(),
                    workflow_name: kill_wf_name,
                    status: "killed".to_string(),
                    return_value: None,
                    script: kill_script,
                    started_at: kill_started_at,
                    finished_at: Some(chrono::Utc::now().to_rfc3339()),
                    error: Some("workflow killed by user".to_string()),
                };
                let _ = journal_clone2.write_state(&run_id, &state);

                // 发送 done_tx（kill 分支作为唯一出口，确保通知任务收到 "killed" 状态）
                let killed_result = WorkflowResult {
                    run_id: run_id.clone(),
                    status: "killed".to_string(),
                    return_value: None,
                    error: Some("workflow killed by user".to_string()),
                    stderr_tail: _stderr_tail,
                };
                let _ = done_tx.send(Some(killed_result));
            }
            _ = &mut msg_loop => {
                // Message loop completed naturally
            }
        }
```

- [ ] **Step 2: 编译验证**

```bash
cd /Users/konghayao/code/ai/perihelion && cargo build -p peri-workflow 2>&1
```

Expected: 编译通过。`msg_loop` 是 `JoinHandle<()>`，`.abort()` 返回 `()`（ignore）。

---

### Task 3: runner agent task — 跳过 Dead 结果的 send_response

**Files:**
- Modify: `peri-workflow/src/runner.rs:264-318`

**背景:** `kill_agent()`（rpc.rs:274-288）通过 `send_error(id, -32000)` 向 Node 发送了错误响应后，agent task 在 `tokio::select!` 中走 cancel 分支返回 `AgentRunResult::Dead`，然后无条件 `send_response(id, result_val)` 发送第二次响应。违反 JSON-RPC 2.0 每条请求对应唯一响应的规范。

**修复:** agent task 在结果为 `Dead` 时跳过 `send_response`（因为 `kill_agent` 已发送 error response）。

- [ ] **Step 1: 修改 agent task 响应发送逻辑**

替换 `peri-workflow/src/runner.rs:307-317`:

替换前:
```rust
                                if let Some(id) = id {
                                    let result_val =
                                        serde_json::to_value(&result).unwrap_or_else(|_| {
                                            serde_json::json!({
                                                "kind": "dead",
                                                "reason": "runagent-threw",
                                                "detail": "serialize failed"
                                            })
                                        });
                                    let _ = ch.send_response(id, result_val).await;
                                }
```

替换后:
```rust
                                // 仅非 Dead 结果发送响应（Dead 时 kill_agent 已发送 error response，
                                // 避免双重 JSON-RPC 响应违反协议规范）
                                if let Some(id) = id {
                                    // skip response for Dead: kill_agent already sent error via send_error(-32000)
                                    let was_killed = matches!(result, AgentRunResult::Dead { .. });
                                    if !was_killed {
                                        let result_val =
                                            serde_json::to_value(&result).unwrap_or_else(|_| {
                                                serde_json::json!({
                                                    "kind": "dead",
                                                    "reason": "runagent-threw",
                                                    "detail": "serialize failed"
                                                })
                                            });
                                        let _ = ch.send_response(id, result_val).await;
                                    }
                                }
```

- [ ] **Step 2: 编译验证**

```bash
cd /Users/konghayao/code/ai/perihelion && cargo build -p peri-workflow 2>&1
```

Expected: 编译通过。`AgentRunResult` 已 derive `Debug`/`Clone`，`matches!` 可用。

---

### Task 4: 验证 kill 路径修复

- [ ] **Step 1: 全量编译**

```bash
cd /Users/konghayao/code/ai/perihelion && cargo build --workspace 2>&1
```

Expected: 全量编译通过（peri-workflow 变更不影响其他 crate 的 API）。

- [ ] **Step 2: 运行现有测试**

```bash
cd /Users/konghayao/code/ai/perihelion && cargo test -p peri-workflow 2>&1
```

Expected: 所有现有测试通过。`runner_test.rs` 中的 E2E 测试已 `#[ignore]`（需要 peri-workflow binary），不参与本次验证。

- [ ] **Step 3: 运行全量测试**

```bash
cd /Users/konghayao/code/ai/perihelion && cargo test --workspace 2>&1
```

Expected: 所有测试通过。

---

### Task 5: v2_bridge.rs — 增加 compact 参数

**Files:**
- Modify: `peri-middlewares/src/subagent/v2_bridge.rs:48-142`

**背景:** `build_v2_subagent_context()` 没有 `compact_config`、`context_budget`、`compact_llm` 参数，导致 SubAgent 和 Workflow agent 都无法启用 auto-compact。主 agent 路径（`builder_v2.rs:240-248`）正确传入了这三者。

**修复:** 给 `build_v2_subagent_context` 增加三个可选参数，并写入 `StageContext` builder。

- [ ] **Step 1: 修改函数签名和文档注释**

替换 `peri-middlewares/src/subagent/v2_bridge.rs:48-61`:

替换前:
```rust
/// - `compact_config` / `context_budget`：可选配置
#[allow(clippy::too_many_arguments)]
pub fn build_v2_subagent_context(
    llm: Box<dyn ReactLLM + Send + Sync>,
    chain: MiddlewareChain,
    tools: Vec<Arc<dyn BaseTool>>,
    cwd: &str,
    cancel_token: CancellationToken,
    parent_messages: Vec<BaseMessage>,
    system_prompt: Option<String>,
    shared_tools: Option<SharedToolMap>,
    error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    tool_registry_snapshot: Option<ToolRegistrySnapshot>,
) -> V2SubagentContext {
```

替换后:
```rust
/// - `compact_config`：auto-compact 阈值配置（None = 不启用）
/// - `context_budget`：上下文预算（None = 不追踪 token 使用率）
/// - `compact_llm`：Full Compact 专用 LLM（None 时 Full Compact 跳过）
/// - `error_suggest_registry`：错误感知建议（可选）
/// - `tool_registry_snapshot`：工具注册表快照（None 用 default）
#[allow(clippy::too_many_arguments)]
pub fn build_v2_subagent_context(
    llm: Box<dyn ReactLLM + Send + Sync>,
    chain: MiddlewareChain,
    tools: Vec<Arc<dyn BaseTool>>,
    cwd: &str,
    cancel_token: CancellationToken,
    parent_messages: Vec<BaseMessage>,
    system_prompt: Option<String>,
    shared_tools: Option<SharedToolMap>,
    compact_config: Option<CompactConfig>,
    context_budget: Option<ContextBudget>,
    compact_llm: Option<Arc<dyn Model>>,
    error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    tool_registry_snapshot: Option<ToolRegistrySnapshot>,
) -> V2SubagentContext {
```

注意：新参数插入在 `shared_tools` 和 `error_suggest_registry` 之间。

- [ ] **Step 2: 在 builder 链中注入 compact 参数**

在 `v2_bridge.rs:128` 行（`if let Some(reg) = error_suggest_registry` 之前）插入 compact 注入代码:

```rust
    if let Some(reg) = error_suggest_registry {
        builder = builder.with_error_suggest_registry(reg);
    }
    // ── compact 参数注入（与主 agent builder_v2.rs:240-248 一致）──
    if let Some(budget) = context_budget {
        builder = builder.with_context_budget(budget);
    }
    if let Some(cc) = compact_config {
        builder = builder.with_compact_config(cc);
    }
    if let Some(llm) = compact_llm {
        builder = builder.with_compact_llm(llm);
    }
    // system_prompt 已作为 BaseMessage::System 注入 transcript...
```

- [ ] **Step 3: 更新所有调用方编译**

```bash
cd /Users/konghayao/code/ai/perihelion && cargo build --workspace 2>&1
```

Expected: 编译会报错，提示其他调用 `build_v2_subagent_context` 的地方缺少新参数。逐个修复调用方。

- [ ] **Step 4: 修复 4 个 SubAgent 调用方**

所有 SubAgent 调用方传 `None`（SubAgent 不需要 auto-compact），在 `shared_tools` 和 `error_suggest_registry` 之间插入 `None, None, None`：

**4a. `peri-middlewares/src/subagent/tool/execute_fork.rs:112-123`**

```rust
        // ── 现有 (line 112-123) ──
        let v2_ctx = build_v2_subagent_context(
            llm,
            chain,
            tools,
            cwd,
            cancel_token,
            parent_msgs,
            system_prompt,
            None,  // shared_tools
            None,  // ← 插入点：compact_config
            None,  // context_budget
            None,  // compact_llm
            None,  // error_suggest_registry
            None,  // tool_registry_snapshot
        );
```

**4b. `peri-middlewares/src/subagent/tool/execute_bg.rs:116-127`**

```rust
        let v2_ctx = build_v2_subagent_context(
            llm,
            chain,
            tools,
            &cwd,
            cancel_token,
            Vec::new(),
            build_result.system_prompt,
            None,  // shared_tools
            None,  // ← compact_config
            None,  // context_budget
            None,  // compact_llm
            None,  // error_suggest_registry
            None,  // tool_registry_snapshot
        );
```

**4c. `peri-middlewares/src/subagent/tool/define.rs:478-489`**

```rust
        let v2_ctx = crate::subagent::v2_bridge::build_v2_subagent_context(
            llm,
            chain,
            tools,
            &cwd,
            cancel_token,
            Vec::new(),
            build_result.system_prompt,
            None,  // shared_tools
            None,  // ← compact_config
            None,  // context_budget
            None,  // compact_llm
            None,  // error_suggest_registry
            None,  // tool_registry_snapshot
        );
```

**4d. `peri-middlewares/src/subagent/spawner.rs:183-194`**

```rust
    let v2_ctx = build_v2_subagent_context(
        config.llm,
        chain,
        tools,
        &cwd,
        cancel_token.clone(),
        config.parent_messages,
        None,  // fork 路径无 system_prompt
        None,  // shared_tools
        None,  // ← compact_config
        None,  // context_budget
        None,  // compact_llm
        None,  // error_suggest_registry
        None,  // tool_registry_snapshot
    );
```

---

### Task 6: workflow_agent.rs — 对接 compact_config

**Files:**
- Modify: `peri-acp/src/agent/workflow_agent.rs:368-379`

**背景:** `WorkflowAgentContext` 已有 `compact_config: Option<CompactConfig>` 字段（第 55 行），但在调用 `build_v2_subagent_context` 时从未传入。

**修复:** 从 `WorkflowAgentContext` 读取 compact_config，构建 context_budget 和 compact_llm，传入 v2_bridge。

- [ ] **Step 1: 修改 workflow_agent execute() 中的 v2_bridge 调用**

替换 `peri-acp/src/agent/workflow_agent.rs:368-379`:

替换前:
```rust
        // 构造 v2 StageContext（workflow agent 无 parent_messages）
        let v2_ctx = peri_middlewares::subagent::v2_bridge::build_v2_subagent_context(
            llm,
            chain,
            tools_arc,
            &self.ctx.cwd,
            cancel_token.clone(),
            Vec::new(),
            Some(system_prompt),
            None,
            Some(error_suggest_registry),
            Some(snapshot),
        );
```

替换后:
```rust
        // ── compact 配置 ──
        // 从 WorkflowAgentContext 读取 compact_config，与主 agent builder_v2.rs 模式一致。
        let compact_config = self.ctx.compact_config;
        let context_budget = compact_config.as_ref().map(|cc| {
            peri_agent::agent::token::ContextBudget::new(
                cc.micro_compact_threshold,
                cc.full_compact_threshold,
            )
        });
        // compact_llm：workflow agent 没有 auxiliary_model，但可以复用主 provider 的 Model
        // 创建新的 Model 实例用于 compact 摘要（与主 agent 的 compact_llm 独立）
        let compact_llm: Option<Arc<dyn peri_model::Model>> =
            if compact_config.is_some() {
                Some(Arc::from(effective_provider.clone().into_model()))
            } else {
                None
            };

        // 构造 v2 StageContext（workflow agent 无 parent_messages）
        let v2_ctx = peri_middlewares::subagent::v2_bridge::build_v2_subagent_context(
            llm,
            chain,
            tools_arc,
            &self.ctx.cwd,
            cancel_token.clone(),
            Vec::new(),
            Some(system_prompt),
            None,
            compact_config,
            context_budget,
            compact_llm,
            Some(error_suggest_registry),
            Some(snapshot),
        );
```

- [ ] **Step 2: 编译验证**

```bash
cd /Users/konghayao/code/ai/perihelion && cargo build -p peri-acp 2>&1
```

Expected: 编译通过。`ContextBudget` 类型来自 `peri_agent::agent::token::ContextBudget`，检查 import 是否已有。

- [ ] **Step 3: 检查 import 完整性**

确认 `peri-acp/src/agent/workflow_agent.rs` 顶部已有以下 import（若缺失则补充）:

```rust
use peri_agent::agent::token::ContextBudget;
use peri_agent::agent::compact::config::CompactConfig;
use peri_model::Model;
```

---

### Task 7: 验证 compact 接入

- [ ] **Step 1: 全量编译**

```bash
cd /Users/konghayao/code/ai/perihelion && cargo build --workspace 2>&1
```

Expected: 全量编译通过。

- [ ] **Step 2: 运行全量测试**

```bash
cd /Users/konghayao/code/ai/perihelion && cargo test --workspace 2>&1
```

Expected: 所有测试通过。

- [ ] **Step 3: 运行 clippy 检查**

```bash
cd /Users/konghayao/code/ai/perihelion && cargo clippy --workspace 2>&1
```

Expected: 无新增 warning。

---

## 验证清单（完成所有 Task 后）

| 场景 | 验证方法 | 预期 |
|------|---------|------|
| Kill workflow → state.json | `cat .claude/workflow-runs/{runId}/state.json` | `"status": "killed"` |
| Kill workflow → LLM 通知 | 检查注入消息文本 | 含 "killed" 而非 "failed" |
| Kill agent → Node 侧 | log Node 侧 agent/run response | 仅一次 response（非双重） |
| Workflow agent 长时间运行 | 观察 compact 日志 | `CompactOutput { compacted: true }` 出现 |
| SubAgent 不受影响 | 已有测试 | `cargo test -p peri-middlewares --lib -- subagent` 通过 |

---

## Self-Review

1. **Spec 覆盖：**  
   - H1: Task 1（移除 abort）  
   - H2: Task 2（msg_loop.abort + done_tx）  
   - H3: Task 3（跳过 Dead response）  
   - H4: Task 5+6（v2_bridge 参数 + workflow_agent 对接）  
   全部 HIGH 级 bug 有对应 Task。

2. **Placeholder 检查：** 无 TBD/TODO/implement later/error handling 占位符。所有代码段完整。

3. **类型一致性：**  
   - `ContextBudget::new(micro, full)` 签名与 `builder_v2.rs:240` 一致  
   - `compact_llm: Option<Arc<dyn Model>>` 与 `StageContext.compact_llm` 类型一致
   - `build_v2_subagent_context` 新参数顺序：`shared_tools, compact_config, context_budget, compact_llm, error_suggest_registry, tool_registry_snapshot`

---

**Plan complete.** Task 1-4 是 kill 路径修复（互相关联），Task 5-7 是 compact 接入（独立）。两个阶段可以独立执行。
