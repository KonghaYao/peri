# P6 完成后手动 Smoke Test 清单

> **2026-06-26**：v1 已完全物理删除（P5.5 + P6.1-P6.6），本清单替代 `verification.md` 中失效的双轨对比。
>
> **当前架构**：所有执行路径 → `run_react_loop`（v2 stages 单路径）。详见仓库根 `CLAUDE.md`「v2 架构状态」段落。

## 标准验证套件（每次实质改动后必跑）

- [ ] 编译：`cargo build --workspace` 绿
- [ ] 全量测试：`cargo test --workspace --lib` 全过（baseline 2721 / 14 ignored）
- [ ] clippy 严格：`cargo clippy --workspace --all-targets -- -D warnings` 零 warning
- [ ] 格式化：`cargo fmt --all -- --check` 通过
- [ ] v1 残留 grep 零结果：
   - [ ] `grep -rn 'ReActAgent' --include='*.rs' --exclude-dir=.worktrees --exclude-dir=target .`
   - [ ] `grep -rn 'trait Middleware<S' --include='*.rs' --exclude-dir=.worktrees --exclude-dir=target .`
   - [ ] `grep -rn 'trait State\b' --include='*.rs' --exclude-dir=.worktrees --exclude-dir=target .`

---

## 手动 Smoke Test 矩阵

启动：`cargo run -p peri-tui -- -a`（HITL 审批模式，推荐，能验证审批弹窗）

每个场景跑 3 轮，记录任何 panic / UI 卡顿 / 事件丢失。

### 1. 对话 + 工具调用

- [ ] 1.1 读取文件：输入「读取 README.md 并总结」
   - [ ] Read 工具调用
   - [ ] 文本回答 + 流式渲染
- [ ] 1.2 写文件：输入「在 /tmp 写一个 hello.txt 内容是 hi」
   - [ ] Write 工具
   - [ ] HITL 审批弹窗（-a 模式）
   - [ ] 完成提示
- [ ] 1.3 搜索：输入「在项目里 grep 'v2' 看有哪些文件」
   - [ ] Grep 工具调用
   - [ ] 结果分组渲染
- [ ] 1.4 Glob：输入「用 glob 找所有 Cargo.toml」
   - [ ] Glob 工具调用
   - [ ] 文件列表渲染
- [ ] 1.5 Shell：输入「跑 ls -la 看下当前目录」
   - [ ] Bash 工具
   - [ ] HITL 审批
   - [ ] 输出格式化

**关注点**：
- `before_tools_batch → 并发 invoke → after_tool × N → after_tools_batch` 事件序列无丢失
- 工具错误时 ErrorSuggestRegistry 注入建议（如路径候选）
- HITL 批量审批（多个工具调用逐个审批）

### 2. Compact（v2 关注点）

- [ ] 2.1 持续对话到上下文 > 70%
   - [ ] Micro compact 触发
   - [ ] 标 `truncated`
   - [ ] UI 提示
- [ ] 2.2 持续对话到上下文 > 85%
   - [ ] Full compact 触发
   - [ ] 摘要 LLM 调用
   - [ ] re-inject 消息
- [ ] 2.3 再次 > 85%
   - [ ] 触发新的 Full compact
   - [ ] **不重复触发** micro（truncated 标记幂等）
- [ ] 2.4 `DISABLE_COMPACT=1 cargo run -p peri-tui -- -a` 跑 2.1
   - [ ] Compact **不**触发
- [ ] 2.5 `DISABLE_AUTO_COMPACT=1` 同上
   - [ ] 同样不触发
- [ ] 2.6 `/compact` 命令（强制 Full Compact）
   - [ ] 立即触发 Full Compact
   - [ ] 摘要注入
- [ ] 2.7 Compact 后立即跑工具调用
   - [ ] 不出现「每轮都触发 compact」（验证 `token_tracker.reset()`）

**关注点（P6 后新增）**：
- TUI 把 compact 摘要消息折叠为 `📋 Context compacted`（依赖 `CONTINUATION_HINT` 三方共享）
- 摘要消息必须是 `BaseMessage::human(...)`（不是 System）—— System 会被 invoke hoist 污染 frozen prompt
- Full compact 后 `token_tracker` 显式 reset（`stages/compact.rs`）
- `MessagesCompacted` 事件携带 `visible_messages` 快照（用于 TUI `pipeline.restore_completed()`）

### 3. Cancel（取消语义）

- [ ] 3.1 长 LLM 调用中按 Ctrl+C
   - [ ] 立即中断
   - [ ] `PromptStopReason::Cancelled`
- [ ] 3.2 工具调用中按 Ctrl+C
   - [ ] 中断
   - [ ] agent 已写入的消息保留（避免 amnesia）
- [ ] 3.3 Full Compact 中（LLM 摘要）按 Ctrl+C
   - [ ] `compact_v2::run_compact` 中断
   - [ ] transcript 还原（不丢消息）
- [ ] 3.4 SubAgent 执行中按 Ctrl+C
   - [ ] 父 cancel 传播到子
   - [ ] 子 agent 立即中断

**关注点**：
- Cancel 后 `result.messages.len()` > 0 时保留历史（有进展）
- `compact/pipeline.rs::run_v2_compact_with_cancel` 在 cancel 时把 transcript 还原到 RwLock（避免遗失）
- SubAgent 的 `CancelPolicy::Cascade` 通过 `Session::new_with_cancel` 链式传播

### 4. SubAgent / Workflow

- [ ] 4.1 同步 Fork：输入「派出 explore agent 查找 X 文件」
   - [ ] SubAgent fork 走 v2 stages
   - [ ] 事件正确路由（不泄漏到父）
- [ ] 4.2 Background：输入「后台执行 Y 任务」
   - [ ] Background agent 启动
   - [ ] bg_results 通过 v2 MessageQueue Defer kind 回传
- [ ] 4.3 Define：输入「用 verification agent 验证 Z」
   - [ ] Define SubAgent
   - [ ] 独立 EventBus
- [ ] 4.4 Workflow：Workflow 工具调用（如 `/ultracode`）
   - [ ] Workflow 进度面板渲染
   - [ ] agent 事件序列正确
- [ ] 4.5 并发：同 prompt 多次派发 SubAgent
   - [ ] `source_agent_id` 精确路由
   - [ ] 事件不串

**关注点**：
- SubAgent 4 文件（`define / execute_bg / execute_fork / spawner`）已全部迁移 v2 stages
- Workflow agent 通过 `WorkflowAgentExecutor` + v2 stages 驱动
- 父子 transcript 关系（ancestor 边界）在 micro compact 时正确处理

### 5. DTO 渲染（TUI）

- [ ] 5.1 TodoWrite 工具调用
   - [ ] Todo 面板渲染（`TodoItemDto`）
- [ ] 5.2 触发 compact
   - [ ] Compact 提示渲染
   - [ ] 文件列表（`CompactFileInfoDto`）
- [ ] 5.3 Workflow 执行中
   - [ ] 进度面板（`WorkflowProgressDto`）
- [ ] 5.4 状态栏上下文使用率
   - [ ] 实时更新（`TokenUsageDto`）
- [ ] 5.5 OAuth 登录流程
   - [ ] OAuth 弹窗（后台事件 DTO）
- [ ] 5.6 MCP 工具调用
   - [ ] MCP 工具名
   - [ ] 进度

**关注点**：
- TUI 仅消费 `AcpEvent` DTO，零 `use peri_agent::agent::events::AgentEvent`
- DTO-ify 类型在 `peri-acp/src/event/dto.rs`
- SubAgentGroup 使用 `instance_id`（非 `message_id`）标识

### 6. Prompt Cache 不变量（第一优先）

- [ ] 6.1 触发 goal steering
   - [ ] 注入 `<system-reminder>` Human 消息
   - [ ] **不污染** frozen_system_prompt
- [ ] 6.2 触发 stop_hook_feedback
   - [ ] Human + `<system-reminder>` wrap
   - [ ] 不污染 frozen prompt
- [ ] 6.3 触发 compact
   - [ ] re-inject 用 Human
   - [ ] `<system-reminder>` + CONTINUATION_HINT
- [ ] 6.4 工具失败提示
   - [ ] Human 注入
   - [ ] 不用 System

**关注点**：
- 所有中途纠正消息必须 `BaseMessage::human(...)`（CLAUDE.md [TRAP]）
- System 消息会被 `invoke.rs` hoist 到顶层，破坏 Prompt Cache
- 验证路径：`goal_middleware.rs` / `hooks/middleware.rs` / `compact_v2.rs::re_inject_v2` / `tool_dispatch.rs`

### 7. Workflow 故障排查（CLAUDE.md 优先检查）

出现 "0 agents, 0 tool calls" 或启动即失败时按序检查：

- [ ] 7.1 peri-workflow binary 存在且可用
   - [ ] `which peri-workflow` 能找到
   - [ ] `head -1 $(which peri-workflow)` 是 `#!/usr/bin/env node`
   - [ ] 不存在则安装：`cd npm-packages/@peri-workflow && npm install && npm run build && npm install -g --prefix ~/.npm-global .`
   - [ ] 确保 `~/.npm-global/bin` 在 PATH 中（`export PATH="$HOME/.npm-global/bin:$PATH"` 加入 `~/.zshrc`）
- [ ] 7.2 Rust 编译通过
   - [ ] `cargo build -p peri-workflow -p peri-acp` 无错误
   - [ ] 修改 `peri-workflow/src/tool.rs` 后尤其注意 `watch::channel` 的 `changed()` 需要 `&mut self`
- [ ] 7.3 重启 Peri TUI 使新 binary 生效

---

## 关键不变量（代码审计时关注）

| 不变量 | 实现位置 | 失败现象 |
|--------|----------|---------|
| Micro compact 幂等 | `compact_v2::micro_compact` 跳过已 `truncated` | 每轮重复触发 compact |
| Full compact 后 reset token_tracker | `stages/compact.rs:132` | Full compact 后每轮都触发 |
| CONTINUATION_HINT 三方共享 | `compact/mod.rs::CONTINUATION_HINT` | TUI 不折叠 compact 摘要 |
| Human wrap 中途纠正消息 | 6.1-6.4 路径 | Prompt Cache 失效 + 模型行为漂移 |
| SubAgent cancel 链式传播 | `Session::new_with_cancel` | Ctrl+C 无法中断 SubAgent |
| EventBus 三层事件映射 | `event/mapper_v2.rs` | TUI 状态不一致 |
| DTO 与运行时类型解耦 | `peri-acp/src/event/dto.rs` | TUI 编译依赖中间件运行时 |

---

## 提交前检查清单（每次实质改动）

- [ ] `cargo build --workspace` 绿
- [ ] `cargo test --workspace --lib` 全过（baseline 2721 / 14 ignored）
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 零 warning
- [ ] `cargo fmt --all -- --check` 通过
- [ ] v1 残留 grep 零结果（见上文命令）
- [ ] 关键 smoke test 场景（1-2 项）手动验证
- [ ] 若改动 ACP 事件：TUI `map_executor_event` 映射同步更新
- [ ] 若改动 compact：CONTINUATION_HINT 三方同步
- [ ] 若改动工具：core_tools.rs / hitl / event mapper / tool_display 同步
- [ ] 若改动 middleware：chain 顺序文档（`peri-middlewares/CLAUDE.md`）同步

---

## 历史 smoke test 文档

- `verification.md`（2026-06-25 Stage 1 期间）—— 已归档，双轨对比部分失效
- `roadmap.md` —— P5.1-P5.5 任务列表已全部完成
- `p5-v1-removal-checklist.md` —— P5 物理删除检查清单，已完成

本文件（`2026-06-26-p6-complete-smoke-test.md`）是 P6 完成后的**当前权威 smoke test 清单**。
