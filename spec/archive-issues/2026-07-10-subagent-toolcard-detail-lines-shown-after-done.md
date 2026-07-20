# SubAgent 内部工具调用完成后 ⎿ 详情行不应显示


> 归档于 2026-07-20，原路径 spec/issues/2026-07-10-subagent-toolcard-detail-lines-shown-after-done.md
**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-10

## 问题描述

SubAgent 展开区内嵌的工具调用（Grep、Read、Bash 等），在流式运行期间（loading）只显示头行 `● ToolName (参数摘要)`，不显示 `⎿` 详情行。但 SubAgent 完成（done）后，所有 `⎿` 详情行（如 tool output summary、Running 状态行）又全部显示出来。用户期望 done 后这些详情行**持续隐藏**——SubAgent 本身内容很长，工具调用的输出细节不应展开显示。

## 症状详情

| 场景 | 当前行为 | 期望行为 |
|------|----------|----------|
| SubAgent 正在运行 | 只显示工具调用头行 `● ToolName (输入摘要)`，不显示 `⎿` 详情 | 只显示头行 ✅ |
| SubAgent 完成 | 头行 + 全部 `⎿` 详情行（output summary 等） | 只显示头行 ❌ |

典型影响：SubAgent 内部有 10 个工具调用时，每个展开后的详情行显著增加信息密度，与 SubAgent 紧凑展示的设计意图不符。

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 Peri TUI
  2. 提交一个派发 SubAgent 的 prompt（如 "用 explorer 搜索 XXX"）
  3. 展开 SubAgent 卡片观察内部工具调用
  4. 等待 SubAgent 完成——done 后之前隐藏的 `⎿` 详情行重新显示

## 涉及文件

- `peri-tui/src/kit/view_render.rs:620-632` —— `render_subagent_group()` 中的 `⎿` 过滤逻辑仅在 `is_running` 时生效，done 后不过滤

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-10 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
