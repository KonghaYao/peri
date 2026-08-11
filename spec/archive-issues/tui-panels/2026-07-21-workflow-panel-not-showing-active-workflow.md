> 归档于 2026-08-11，原路径 spec/issues/2026-07-21-workflow-panel-not-showing-active-workflow.md

# Workflow 面板在 workflow 运行时不显示 active workflow

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-21

## 问题描述

打开 `/workflows` Workflow 面板时，面板无法显示正在运行中的 workflow。尽管 workflow 的各个 agent 实际正在正常工作（执行任务、写入文件），但 TUI 面板中看不到任何 running 状态的 workflow、phase 或 agent，面板显示为空或不包含当前运行的 workflow。

## 症状详情

- 通过 Workflow 工具启动 ultracode workflow 后，workflow agent 正常工作并写入文件
- 执行 `/workflows` 打开面板，预期应显示 running 状态的 workflow run、各 phase 状态、agent 信息
- 实际表现：面板为空，或不显示正在运行的 workflow run

## 复现条件

- **复现频率**：待确认
- **触发步骤**：
  1. 通过 Workflow 工具启动一个 ultracode workflow
  2. 执行 `/workflows` 打开工作流面板
  3. 观察面板展示内容
  4. 预期：面板应显示 running 状态的 workflow、phase、agent
  5. 实际：面板为空或不显示正在运行的 workflow
- **可能原因**：WorkflowSnapshot 的轮询/更新路径与 workflow runner 的实际状态不同步

## 涉及文件

- `peri-tui/src/kit/workflow_snapshot.rs` —— WorkflowSnapshot 数据结构与轮询逻辑
- `peri-tui/src/kit/panels/workflow.rs` —— Workflow 面板 UI 渲染
- `peri-tui/src/kit/atoms.rs` —— WORKFLOW_SNAPSHOT_ATOM 全局状态管理
- `peri-tui/src/kit/acp_events.rs` / `acp_notifier.rs` —— ACP 事件流接入路径
- `peri-workflow/src/runner.rs` —— workflow runner 实际运行状态

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-21 | — | Open | agent | 创建 |
| 2026-08-11 | Open | Fixed | agent | 归档：workflow.rs 面板已渲染 running 状态（run.status/agent.status 分支，ca95303f 起多轮改进） |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
