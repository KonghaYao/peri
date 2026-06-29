# v2 激进切换 — 主编排计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` or `Workflow` tool. 本主文档是编排入口；4 个子计划各自独立可执行。

**Goal:** 以最激进、无兼容层的方式完成 v2 架构切换 — 删除 `thin_handle` legacy glue、`message_pipeline/` 全目录、双线程渲染、`RenderCache`、`AdaptiveChunkingPolicy`，迁移 `UiState.textarea` → `State::Idle.input`，替换 TUI 对 `peri_agent`/`peri_middlewares` 的 138 处类型依赖。

**Architecture:** TUI 成为纯 v2 状态机驱动 — `main_loop::run` 单一入口调用 `state_machine::handle`，渲染同步走 `State.view + current_turn`，运行时只通过 ACP 协议与 Agent 通信。**物理删除**所有 legacy 文件，不保留 alias / fallback / `#[deprecated]`。

**Tech Stack:** Rust 2021 + async-trait + ratatui + tokio + crossterm + tui_textarea（仅用于 widget 渲染，不再作为状态源）

---

## 子计划清单（按依赖顺序）

| # | 子计划 | 依赖 | 影响范围 | 测试基线 |
|---|--------|------|----------|----------|
| **1** | [`01-input-state.md`](./2026-06-29-v2-aggressive-cutover-01-input-state.md) | 无 | `state_machine/input/` 重写 + `InputState` 完整 API | 当前 3304 测试不能减少 |
| **2** | [`02-main-loop-cutover.md`](./2026-06-29-v2-aggressive-cutover-02-main-loop-cutover.md) | Plan 1 | 删除 `main_loop.rs::thin_handle` L136-523 + 全部 `event/keyboard/` 重写 | 端到端键盘/鼠标/Paste/ACP 事件路径测试 |
| **3** | [`03-rendering-rewrite.md`](./2026-06-29-v2-aggressive-cutover-03-rendering-rewrite.md) | Plan 2 | 物理删除 `app/message_pipeline/` 全目录 + `ui/render_thread.rs`；渲染入口切换到 `State.view + current_turn` | headless 渲染快照测试重建 |
| **4** | [`04-type-isolation.md`](./2026-06-29-v2-aggressive-cutover-04-type-isolation.md) | 无（可与 Plan 1 并行） | 替换 138 处 `use peri_agent::`/`use peri_middlewares::`；启用 `scripts/check-tui-imports.sh` pre-commit 钩子 | `check-tui-imports.sh` 0 处违规 |

---

## 关键约束（来自 `peri-tui/CLAUDE.md` [TRAP]）

执行期间**必须**保证以下不变量，否则会引入难定位的 bug：

1. **ACP 协议边界**：TUI 状态变更必须通过 `acp_client` 协议方法；本地清空 ≠ ACP Server 同步。
2. **Compact 三步清理**：`clear() + restore_completed(messages) + RebuildAll { prefix_len: 0 }` 缺一不可。
3. **Ephemeral VM 锚点**：`SystemNote`/`CacheWarning` 依赖 `view_messages.len()` 作为位置索引。
4. **frozen_subagent_vms 匹配**：先 `instance_id` 精确匹配，失败后按顺序 `agent_id` 匹配；`begin_round()` 清空但 `done()` 不清空。
5. **round_start_vm_idx vs prefix_len**：VM 索引 vs BaseMessage 长度，非 1:1。
6. **Prompt Cache**：会话内系统提示词不可变更；动态区域占位符可变但结构/段落数量必须固定。
7. **CJK 安全**：字符串截断用字符级 `chars().take(N)`，禁止 `&s[..N]`。

---

## 执行策略

**激进模式特征**：
- ✅ **物理删除** legacy 文件（不保留 `_legacy.rs` 后缀）
- ✅ **删除测试**：`message_pipeline_test.rs` (82KB, 2342 行) 全部物理删除
- ✅ **不保留** deprecated alias / re-export / fallback 分支
- ✅ **一次到位**：每个子计划完成后，legacy 代码立即消失
- ❌ **禁止**：shadow mode、双轨运行、 gradual rollout、`if v2_enabled { ... } else { legacy }`

**执行顺序**（推荐 workflow 编排）：

```
Phase A (并行):
  Workflow 1: Plan 1 (InputState 完整化)   ──┐
  Workflow 2: Plan 4 (类型隔离)            ──┤
                                              │
Phase B (串行，依赖 Phase A):               │
  Workflow 3: Plan 2 (B3 Cutover)         ←─┘ Plan 1
                                              │
Phase C (串行，依赖 Phase B):               │
  Workflow 4: Plan 3 (渲染重写)           ←── Plan 2
```

**每 Phase 验收标准**：
- `cargo build --workspace` 退出 0
- `cargo test --workspace` 通过率 ≥ 95%（允许少数测试因 legacy 删除而失效，但必须显式记录）
- `cargo clippy --workspace -- -D warnings` 0 警告
- `lefthook run pre-commit` 全过

---

## 风险登记

| 风险 | 影响 | 缓解 |
|------|------|------|
| InputState 缺失 TextArea 能力（多行/选择/单词删除） | Plan 1 阻塞 | TDD：先写完整 InputState 测试套件再迁移调用点 |
| `event/keyboard/` 重写后焦点/IME/选择行为回归 | 用户体验降级 | 保留 `tui_textarea::TextArea` 作为渲染 widget（只读 InputState） |
| 删除 `message_pipeline` 导致 ephemeral VM / frozen subagent 锚点丢失 | 渲染错位 | 在 v2 `ViewStore` 中重新实现锚点机制（Plan 3 Task 4） |
| 138 处 import 替换引入类型不匹配 | 编译失败 | 分批替换（按类型分类），每批独立 commit |
| ACP 协议边界违规（TUI 直连 Agent） | 运行时不一致 | `check-tui-imports.sh` 启用为 pre-commit 钩子（Plan 4 最后任务） |

---

## 完成定义（Definition of Done）

整个主计划完成的标志：

1. ✅ `grep -rn "thin_handle" peri-tui/src/` → 0 结果
2. ✅ `grep -rn "MessagePipeline" peri-tui/src/` → 0 结果
3. ✅ `grep -rn "RenderCache\|RenderEvent\|render_thread" peri-tui/src/` → 0 结果
4. ✅ `grep -rn "AdaptiveChunkingPolicy" peri-tui/src/` → 0 结果
5. ✅ `grep -rn "use peri_agent::\|use peri_middlewares::" peri-tui/src/` → 0 结果（`acp_stdio/` 例外，作为协议边界允许）
6. ✅ `grep -rn "ui\.textarea\b" peri-tui/src/event/ peri-tui/src/app/` → 0 编辑类引用（渲染类引用改为读 InputState）
7. ✅ `cargo test --workspace` 通过率 ≥ 95%
8. ✅ TUI 实际启动 + 基本交互（输入、submit、面板开关、滚动）手动测试通过

---

## 后续步骤

阅读各子计划文档，按 Phase A → B → C 顺序执行。每个子计划的开头都有「REQUIRED SUB-SKILL」声明。
