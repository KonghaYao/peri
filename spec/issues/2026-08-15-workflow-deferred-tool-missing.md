# Workflow deferred tool 对模型不可见：SearchExtraTools 搜不到，e2e workflow 全挂

**状态**：已修复（2026-08-15，e2e 验证中）
**优先级**：高（核心功能 workflow 完全不可用）
**类型**：缺陷
**创建日期**：2026-08-15
**来源**：用户报告 + e2e 复现（2026-08-15 上午排查）

## 症状

1. **e2e 全挂**：`e2e/tests/workflow/` 三个用例（workflow-run / workflow-panel-columns / workflow-reporting）两轮重试全部失败：
   - 等不到 `"completed. ("` 完成通知（300s 超时 ×2）
   - 180s 内没有新的 `state.json`（workflow-reporting）
2. **无 run 产出**：`.claude/workflow-runs/` 最新 run 停在 **2026-08-14 17:15**，e2e 期间（08-15 12:24-12:35）零新增 → WorkflowTool 从未真正执行到 runner（非执行中报错，是根本没被调用）。
3. **手动复现（tmux + dev.sh 真实 TUI）**，prompt `/ultracode 请派发一个简单的 workflow，用并行 agent 分别执行 echo hello workflow test`，模型自述：
   > 「当前环境的工具注册表中没有 Workflow deferred tool（SearchExtraTools 多次搜索均无结果），因此无法用 /ultracode skill 所述的 parallel([...]) 脚本方式派发。改用原生 Agent 工具并行派发 3 个 sub-agent」
   - 模型实际成功执行过 `SearchExtraTools select:Workflow`（返回 ✓）——**索引里存在 Workflow 条目**，但模型认为不可用/无法执行，最终降级为 Agent 工具模拟。
   - 结论：WorkflowTool 未进入模型可执行的 deferred tool 视图（或索引条目存在但执行路径断）。

## 时间窗口

- `.claude/workflow-runs/` 最后一次成功写入：2026-08-14 17:15。
- 落在 `123a547f Feature/20260814 (#88)` merge 之后的提交范围内；当日后续提交：`1b8358be`（langfuse workflow input 上报）、compact 记录结构补全、`61c23726`（meta_harness 全关组合告警 + e2e HOME 隔离）、`4bc82c8f`（审批/提问职责拆分，AskUserQuestion 纳入关闭面）、`79f99f6a`（MCP skill 发现对齐 SEP-2640）。
- **首要嫌疑**：`4bc82c8f` 拆分后 `MIDDLEWARE_TOOL_NAMES` 剔除面 / `build_session_tool_view` / `ToolSearchMiddleware` 索引构建的变化；以及 #88 merge 对工具收集链的改动。

## 已排除项（静态核查）

- `WorkflowMiddlewareAdaptor::collect_tools` 存在（`peri-middlewares/src/workflow/mod.rs:386`），装配条件 `workflow_executor.is_some()` 满足（`peri-acp/src/host/prompt.rs:168` 恒 Some）。
- `assembly.rs:228-253` Workflow 中间件条件注册逻辑完整；MetaHarness disabled 列表不含 WorkflowMiddleware（用户默认配置下）。
- `stage_builder.rs`、`chain_test.rs` 中 `16_workflow` 段落已删（ultracode skill 覆盖）——**注意**：模型在复现中明确引用了 `/ultracode skill 所述的 parallel([...]) 脚本方式`，若 ultracode skill 内容与实际注册的 deferred tool 名不一致（如 `Workflow` vs `WorkflowTool` vs `run_workflow`），模型会搜不到。

## 待查方向（bg agent 排查中）

1. **全量 deferred tool 清单**：枚举所有 middleware `collect_tools` 产出的 deferred tool（workflow / subagent / skills / cron / artifact / permission / hitl-askuser / MCP 等），逐一核对是否进入每 turn 工具视图。
2. **`MIDDLEWARE_TOOL_NAMES` 剔除面**（`peri-acp-types/src/meta_harness.rs`）：确认 4bc82c8f 后剔除名单是否正确，是否误剔 Workflow 或其它 deferred tool。
3. **`ToolSearchMiddleware::before_agent` 索引数据源**（`peri-middlewares/src/tool_search/`）：索引是否覆盖所有 deferred tool；`select:Workflow` 命中的条目名与实际 `ExecuteExtraTool` 需要的 tool_name 是否一致。
4. **`build_session_tool_view` / `shared_tools` 生产路径**：deferred tool 是否被 `is_direct` / 过滤规则误分类。
5. 对比 `123a547f` 之前（8/14 17:15 之前最后一次可用）的 tool view 构建差异。

## 复现步骤

```bash
cd e2e && npm run e2e -- --only workflow --parallel 3
# 手动：tmux 起 TUI（./dev.sh），发
#   /ultracode 请派发一个简单的 workflow，用并行 agent 分别执行 echo hello workflow test
# 观察模型工具调用：应出现 WorkflowTool 调用，实际退化为 Agent 并行
```

## 影响面

- workflow 功能（`/workflows` 面板、workflow 运行、workflow-runs 落盘）整体不可用；
- 若为剔除面误伤，**其它 deferred tool（cron / subagent / skills 等）可能同样不可见**——需全量核对，不排除"不止 workflow 一个"。

## 根因（已定位）

**断的是"发现/可见性面"，不是执行面**：

- 链路：宿主 `shared_tools`（assemble.rs:341 / stdio/init.rs:143 建空表，**全仓库无生产写点**）→ `ToolSearchMiddleware::new(index, Arc::clone(shared_tools))`（assembly.rs:511-514）→ `build_session_tool_view`（stage_builder.rs:219-243）产出的**每 turn 本地视图**（含 Workflow 等全部链工具）只进 `ctx.runtime.tools`（stage_builder.rs:820）→ `before_agent`（middleware.rs:64-107）却从自己持有的宿主空表取 `deferred_arcs` → `should_rebuild = !deferred_arcs.is_empty() && …` **恒 false** → `ToolSearchIndex` 永不构建 → SearchExtraTools 恒空、cached_prompt 恒 None。
- 执行面（`ExecuteExtraToolResolver` 按每 turn 视图解析，execute_tool.rs:43-66）实际是通的——模型搜不到工具，自然走不到执行。
- 起点：#88 merge（123a547f，8/14 17:11）引入本地视图架构但 `ToolSearchMiddleware` 读源未迁移；4bc82c8f（8/15）删除最后一个 shared_tools 写点后把断链固化为"设计"（其 spec 决策第 3 条声称"ToolSearchMiddleware 持每 turn 本地视图"，实现未跟进）。**影响全部 deferred tool**（Workflow / AgentResult / cron_* / mcp__* / LSP / goal），非 workflow 独有。

## 修复（2026-08-15）

落实 4bc82c8f 设计意图"ToolSearchMiddleware 持每 turn 本地视图"，经 `MiddlewareState` 桥接（v2 middleware 钩子的 state 是 `AgentContext`，薄封装 StageContext）：

1. `peri-agent/src/middleware/state.rs` — `MiddlewareState` trait 新增 `fn local_tools(&self) -> Option<&SharedToolMap>`，默认 `None`。
2. `peri-agent/src/agent/agent_context.rs` — override 返回 `Some(&self.ctx.runtime.tools)`（每 turn 本地视图）。
3. `peri-middlewares/src/tool_search/middleware.rs` — `before_agent` 优先读 `state.local_tools()`，无本地视图时回退 `self.shared_tools`（v1/测试路径兼容）。
4. 回归测试：`tool_search::middleware::tests::test_before_agent_builds_index_from_local_tools_when_shared_empty`（宿主表为空 + 本地视图注入 → 索引构建 + prompt 贡献）。

验证：单元测试全绿（peri-middlewares tool_search 79 项 / assembly 28 项、peri-agent agent_context 16 项）；手动复现（tmux + dev.sh）模型成功 `SearchExtraTools workflow` → `ExecuteExtraTool Workflow` 派发，`completed. (5400ms, 3 agents, 12 tool calls)`，state.json 落盘完整；e2e workflow 三用例复跑验证中。
