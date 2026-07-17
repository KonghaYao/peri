# 子 agent 工具别名解析失败

**状态**：Open
**优先级**：中
**创建日期**：2026-07-16

## 问题描述

子 agent 调用工具时使用工具别名（如 "Shell" 代替 "Bash"），工具查找失败。主 agent 的别名解析（`resolve_tool` → `BaseTool::aliases()`）正常工作，但子 agent 中所有工具的别名都无法被识别。对所有类型的子 agent 均必现。

## 症状详情

在子 agent 执行任务时：

| 现象 | 详情 |
|------|------|
| **别名调用失败** | 子 agent 输出工具别名（如 `Shell`），执行时报 `Tool 'Bash' not found` |
| **主 agent 正常** | 同一会话中，主 agent 用 `Shell` 调用 Bash 工具正常工作 |
| **影响范围** | 所有子 agent 类型（fork / 有 agent 定义 / built-in）均受影响 |
| **影响工具** | 所有有声明的别名的工具（Bash → "Shell"、Read → "reading"、Agent → "task" 等） |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动任意子 agent（如 coder、explorer）
  2. 让子 agent 执行一个需要 Bash 的任务（如运行 shell 命令）
  3. 子 agent 用别名 `Shell` 调用 Bash 工具时，工具未找到
- **环境**：macOS，所有模型

## 涉及文件

- `peri-middlewares/src/tools/mod.rs` —— `ArcToolWrapper` 和 `BoxToolWrapper`（子 agent 工具包装器）
- `peri-middlewares/src/subagent/fork.rs` —— `filter_tools()`（子 agent 工具过滤，返回包装后的工具列表）
- `peri-agent/src/tools/mod.rs` —— `BaseTool::aliases()` trait 方法
- `peri-agent/src/agent/stages/tool_dispatch.rs` —— `resolve_tool()`（别名解析逻辑）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-16 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
