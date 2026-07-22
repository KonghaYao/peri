# plan agent 偶发缺少 WriteSandbox 工具——沙箱目录不存在时构造失败导致静默跳过

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-20

## 问题描述

派发 `plan` agent（built-in，声明了 `allowedWriteDirs: [".peri/plans/"]`）时，agent 调用 `WriteSandbox` 工具写入计划文件，却收到 `Tool 'WriteSandbox' not found` 错误。plan agent 被迫将计划以回复文本形式输出（而非写入交接文件），破坏了 start-devflow 等依赖 subagent 交接文件的工作流。

这是 `2026-07-18-subagent-write-sandbox-tool`（Done）的遗漏场景。

## 症状详情

| 维度 | 期望行为 | 实际行为 |
|------|----------|----------|
| plan agent 可用工具 | 应有 WriteSandbox（按 allowedWriteDirs 声明注入） | `Tool 'WriteSandbox' not found` |
| plan agent 产出 | 计划写入 `.peri/plans/<topic>.md` 交接文件 | 计划只能以回复文本返回（多跳传递中易丢失） |
| 出现条件 | — | `.peri/plans/` 目录不存在时写入失败 |

**用户观察到的 agent 输出**：

```
● Agent (description: Plan unified Langfuse bridge)
  ⎿ ... I'll output the plan directly below.
  ⎿ ---
  ⎿ ## Plan Summary
  ⎿ … 
  ▶ 29 collapsed tools
  ● WriteSandbox (path: "02-plan.md")
    ⎿ Tool 'WriteSandbox' not found
```

**对应交互**：派发 plan agent 后，agent 尝试 `WriteSandbox(path: "02-plan.md")` → 失败 → 退回纯文本输出。

## 根因

**唯一根因**：`.peri/plans/` 目录不存在 → `WriteSandboxTool::new()` 的 `canonicalize()` 失败 → `build_agent_from_def` 静默跳过注入（`warn!` 日志无用户可见提示）。

```
WriteSandboxTool::new(cwd, [".peri/plans/"])
  → Path::new(&cwd).join(".peri/plans/").canonicalize()
  → Err(NotFound) ← 目录不存在
build_agent_from_def:
  → Err(e) => warn!("WriteSandbox 构造失败，跳过注入")
  → plan agent 启动，工具集中无 WriteSandbox
```

三个设计遗漏：
5. **WriteSandboxTool::new 构造契约**要求目录已存在，无自动创建
6. **build_agent_from_def 错误处理**选择静默跳过（`warn!`），不创建目录、不传播错误、无用户可见降级提示
7. **零初始化逻辑**——整个代码库没有任何代码创建 `.peri/plans/` 目录，对比 `.peri/settings.json` 有 `provider/store.rs` 按需读写

**测试覆盖缺口**：`write_sandbox_test.rs` 的 `make_tool()` 辅助函数总是先 `create_dir_all` 再构造工具，从未测试目录不存在的路径

## 关联历史

`2026-07-18-subagent-write-sandbox-tool`（Done）实现了完整链路，测试全绿但漏了未覆盖的边缘。

## 涉及文件

- `peri-middlewares/src/tools/filesystem/write_sandbox.rs:44-48` —— `WriteSandboxTool::new()` 构造时 canonicalize 沙箱目录，目录不存在则失败
- `peri-middlewares/src/subagent/tool/build_agent.rs:92-127` —— WriteSandbox 注入逻辑，构造失败时静默跳过（`warn!` 日志，不 panic）
- `peri-middlewares/src/subagent/built-in/plan.md:9-10` —— plan agent 声明 `allowedWriteDirs: [".peri/plans/"]`

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-20 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
