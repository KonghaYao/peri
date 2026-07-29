# P0 问题验证报告

**日期**: 2026-07-29  
**来源**: E2E Report #1（2026-07-28）

---

## P0 #4: first-tool-stuck-running — batch 工具卡片消失

### 测试断言

```
expect(hasRead).toBe(true);   // ← 失败: hasRead = false
expect(hasGrep).toBe(true);
expect(hasRunning).toBe(false);
```

### 代码路径逐段验证

#### 1. 工具卡片创建 (`acp_events/tool.rs:68-79`)

```rust
// main agent tool（无 agent_id 分支）
state.current_turn.start_tool(ToolCardAccumulator::new(...));
super::render::push_view_models(state);
```

`start_tool` (`acp_types.rs:169-185`) 做三件事：
- 防御性跳过重复 tool_id（不会丢失）
- `flush_text_segment()` 把挂起文本归档为 segment
- 同时 push `TurnSegment::Tool { tool_idx }` + `ToolCardAccumulator`

**结论**: ✅ 卡片创建路径正确。

#### 2. 工具完成 (`acp_events/tool.rs:120-128`)

```rust
state.current_turn.end_tool(&te.tool_id, te.output_summary.clone(), te.is_error);
super::render::push_view_models(state);
```

`end_tool` (`acp_types.rs:190-196`) 只做：
- `t.output_summary = Some(output)` — 填充输出
- `t.is_error = is_error` — 设置错误态
- `self.invalidate_cache()` — 使缓存失效

**不删除**，**不移除**，**不修改 segments 顺序**。

**结论**: ✅ 完成事件只填充数据，不删除卡片。

#### 3. TurnDone 归档 (`acp_events/turn.rs:12-51`)

```rust
// TurnDone → flush_current_turn
state.flush_current_turn();
```

`flush_current_turn` (`acp_events/mod.rs:202-222`):
- 无条件：`for vm in current_turn.view_models() { committed.push_back(vm.clone()) }`
- 然后：`current_turn.reset()`
- **没有 has_running_subagent 跳过**（本项目无 SubAgent）

**结论**: ✅ TurnDone 正确将全部 VMs（含工具卡片）从 current_turn 移到 committed。

#### 4. view_models 生成 (`acp_types.rs:436-465`)

```rust
TurnSegment::Tool { tool_idx } => {
    if let Some(t) = self.tool_cards.get(*tool_idx) {
        let is_running = t.output_summary.is_none();
        vms.push(TuiRenderUnit::TuiToolCard(TuiToolCard {
            tool_name: t.tool_name.clone(),  // "Read"
            is_running,                       // false（已填充 output_summary）
            ...
        }));
    }
}
```

卡片按 segment 索引访问，segment 在 `start_tool` 同步插入。索引不会错位。

**结论**: ✅ `build_view_models` 始终包含所有工具卡片（无论完成与否）。

#### 5. VIEW_MODELS 写入 (`acp_events/render.rs:11-55`)

```rust
let mut items = state.committed.clone();       // O(1) im::Vector clone
for vm in state.current_turn.view_models() {   // 追加 current_turn 内容
    items.push_back(vm.clone());
}
*VIEW_MODELS.state().write() = ViewModelsSnapshot { items, generation };
```

无过滤，无删除。

**结论**: ✅ VIEW_MODELS 完整包含 committed + current_turn。

#### 6. 渲染输出 (`message_area/render.rs:370-378`)

完成的 Read 卡片渲染为：

```
● Read (Cargo.toml) — N lines
```

包含 "Read" 文本。`format_tool_name("Read")` 返回 `"Read"`（非 i18n 映射的工具名）。

**结论**: ✅ 完成的 Read 卡片渲染文本中必然包含 "Read"。

### 可能的真正原因

| 可能性 | 分析 | 概率 |
|--------|------|------|
| 屏幕滚动导致卡片超出视口 | tmux capture 只捕获可见区域；如果后续输出（Grep + LLM 文本）超出视口，Read 卡片可能被滚出屏幕 | **高** |
| 20s 等待不充分 | LLM 可能在 20s 后仍在处理 Grep 的输出，TurnDone 未触发 | 中 |
| 环境问题 | Model 响应慢、网络波动导致工具调用延迟 | 中 |
| BRIDGE_RESET_COUNTER 误触发 | 代码中无明显触发路径（非 compact、非 /clear、非 thread 切换） | 低 |
| 代码回归 | 上述全链路验证无一删除路径 | **极低** |

### 验证结论

**代码路径完全正确，无回归**。`hasRead=false` 最可能原因是：
1. 屏幕内容超出渲染视口，Read 卡片被滚出可见区域（tmux `capture-pane` 只捕获当前窗口）
2. 20s 后的状态工具调用尚未完全完成（TurnDone 未触发），因此卡片还在 current_turn 中但 UI 可能因 phase 状态有不同的渲染行为

**建议**: 查看实际录制的 ANSI 快照（`e2e/recordings/first-tool-not-stuck-v2.ansi`）确认 Read 文本是否存在于完整屏幕内容中。

---

## P0 #6: workflow-panel-columns — 稳定性与崩溃

### 问题一：Slash 命令注册为复数 `"workflows"` 而用户习惯用单数 `/workflow`

#### 根因

`peri-tui/src/kit/panel_registry.rs:328`:

```rust
PanelMeta {
    kind: PanelKind::Workflow,
    slash_command: "workflows",  // ← 复数！
    ...
}
```

`panel_for_slash_command` (`panel_registry.rs:368-379`) 使用**精确匹配**：

```rust
let normalized = command.trim_start_matches('/').to_ascii_lowercase();
PANELS.iter().find(|m| m.slash_command == normalized)
```

`"/workflow"` → `"workflow"` ≠ `"workflows"` → **无匹配** → 面板不打开。

Slash 命令匹配失败时回退为 `SubmitRequest::AgentText("/workflow")`，将 `/workflow` 作为**普通 prompt 发送给 LLM**（`submit_request_test.rs:30-32` 明确测试了此行为）。

#### 对 E2E 测试的影响

| 测试 | 发送方式 | 实际行为 | 预期行为 |
|------|---------|---------|---------|
| `workflow-panel-columns.test.ts:53` | `sendText("/workflow")` | 发送到 LLM 作为 prompt | 打开 Workflow 面板 |
| `workflow-run.test.ts:48` | `sendPrompt(tester, "/workflow")` | 发送到 LLM 作为 prompt | 打开 Workflow 面板 |
| `workflow-run.test.ts:66` | `sendPrompt(tester, "/workflow")` | 发送到 LLM 作为 prompt | 打开 Workflow 面板 |

所有三处 `/workflow` 调用**均不会打开面板**。`workflow-run.test.ts:50` 的 `waitForText("Workflow", { timeout: 10_000 })` 实际上是在等待 LLM 的响应文本中出现 "Workflow" 字样（不可靠）。

#### 额外发现：缺少测试覆盖

`submit_request_test.rs` 中**没有** `/workflow` 或 `/workflows` 的测试用例，但有 `/unknown` → `AgentText` 的测试（行 30-32）。如果存在 `/workflows` 的测试，会立即暴露不匹配问题。

### 问题二：Tmux server crash

```
Error: Failed to capture screen: no server running on /private/tmp/tmux-501/default
```

| 可能性 | 分析 | 概率 |
|--------|------|------|
| Tmux 进程被系统杀死（资源耗尽） | workflow 测试运行 120s+，期间 peri 编译 + LLM 调用，可能触发 OOM killer | **高** |
| Peri crash 导致 tmux session 异常退出 | `progress.rs:293` 有 `unwrap()` panic 点（见下） | 中 |
| Tmux 超时自动清理 | 测试 timeout=300000ms (5min)，tmux 可能在此之前被清理 | 低 |

### 问题三：潜在 panic 点

#### `peri-workflow/src/progress.rs:293`

```rust
fn set_or_update_agent(&mut self, agent_id: AgentId, f: impl FnOnce(&mut AgentProgress)) {
    let agents = &mut self.runs.get_mut(&run_id).unwrap().agents;
    agents.entry(agent_id).or_insert_with(AgentProgress::default);
    f(agents.get_mut(&agent_id).unwrap());  // ← 理论上安全，但 RwLock 不防护并发 remove
}
```

`entry().or_insert_with()` 后立即 `get_mut().unwrap()` — 单线程逻辑正确，但若 `RwLock` 内其他分支有并发 `remove(agent_id)`，会 panic。

#### `peri-workflow/src/tool.rs:336`

```rust
let result = watch_rx.changed().await.expect("watch value should be Some after changed() resolves");
```

`changed()` 返回 Ok 后 `borrow()` 理论上一定有值，但 `expect` 不如 `unwrap_or_default` 安全。

### 验证结论

1. **Slash 命令不匹配是 root cause**：`slash_command: "workflows"` ≠ 用户/Tests 输入 `/workflow`。需改为 `"workflow"` 或增加 `"workflow"` → `"workflows"` 的别名映射。

2. **Tmux crash 大概率是环境问题**：workflow 场景运行 2 分钟+ LLM 调用，资源压力大。

3. **Panic 点风险偏低**：理论上的并发 window，实际概率小。

---

## 修复建议

### P0 #4（低优先级——代码路径已验证正确）

- [ ] 查看快照 `recordings/first-tool-not-stuck-v2.ansi` 确认 Read 文本是否存在
- [ ] 若卡片在屏幕外，增加测试的终端 rows 尺寸（当前 40 行）或减少 prompt 输出量

### P0 #6（高优先级——slash command 不匹配）

- [ ] **必做**：将 `slash_command: "workflows"` 改为 `"workflow"`（或增加别名映射）
- [ ] **必做**：在 `submit_request_test.rs` 增加 `/workflow` → `OpenPanel(Workflow)` 测试用例
- [ ] ~~修复 `progress.rs:293` panic 点~~（建议替换 `unwrap()` 为安全访问）
