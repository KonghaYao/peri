> 归档于 2026-07-17，原路径 spec/issues/2026-07-15-terminal-rapid-shrink-width-crash.md
# 快速缩小终端宽度到极小值时程序直接退出崩溃

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-15
**类型**：Bug

## 问题描述

在空闲状态（无流式输出、无面板打开）下，快速将终端窗口的宽度缩小到一个非常小的值时，Peri TUI 进程直接退出，没有任何错误信息输出。即使多次尝试，每次都会必现崩溃。

（注意：进程并非真正"无信息退出"，而是自定义 panic hook 仅通过 `tracing::error!` 写日志、不输出到 stderr，在 raw mode 终端下用户看不到 panic 消息。）

## 症状详情

| 维度 | 描述 |
|------|------|
| 崩溃形式 | 进程直接退出，无 panic backtrace、无错误消息 |
| 触发条件 | 快速缩小终端宽度到很小值（约 < 10 列） |
| 发生场景 | 空闲/静止时（无流式输出，无面板打开） |
| 消息区状态 | 有历史对话消息存在 |
| 复现频率 | 必现（每次快速缩小宽度都会触发） |

## 复现条件

- **复现频率**：必现
- **环境**：macOS
- **触发步骤**：
  1. 启动 Peri TUI，进行几轮对话（确保消息区有历史内容）
  2. 等待所有流式输出完成，处于空闲状态
  3. 快速拖动终端窗口边缘，将宽度迅速缩小到一个极小值（< 10 列）
  4. 进程直接退出

## 根因

`peri-tui/src/kit/markdown/table.rs` 的 `table_data_to_lines` 函数存在 **buffer 宽度与列宽不匹配**的 bug：

1. `compute_table_col_widths` 在 `available == 0` 时返回 `[1, 1, ...]`（每列最小 1 宽）
2. `table_data_to_lines` 计算 `tw = min(cw + 2, max_width).max(4)` — buffer 被 `max_width` 钳制
3. 但列宽 `wa` 未被同步缩减，render 函数按原始列宽写入 buffer → 越界 panic

**日志验证**（`agent-tui.log:521324`）：
```
index outside of buffer: area Rect { width: 8, height: 81 } but index is (8, 0)
```
2 列表格占 9 列（边框 7 + 列宽 2），buffer 仅 8 列 → X=8 越界。

## 涉及文件

- `peri-tui/src/kit/markdown/table.rs:486-508` —— **核心修复**：`table_data_to_lines` 增加列宽 redistribution 逻辑
- `peri-tui/src/kit/message_area/render.rs:639-643` —— 新增 test module
- `peri-tui/src/kit/message_area/render_test.rs` —— 3 个回归测试

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-15 | — | Open | agent | 创建 |
| 2026-07-15 | Open | Fixed | agent | 修复 table_data_to_lines Buffer 越界 + 二次归一化 |

## 修复记录

### 修复 #1（2026-07-15）

- **操作人**：agent
- **用户原意**：快速缩小终端宽度到极小值时程序崩溃
- **修复内容**：
  1. `table.rs:table_data_to_lines`：当 `cw + 2 > tw`（列宽 + 边框超出 buffer）时，按比例缩减列宽；增加 `content_space < n` 检查（若连 n 列最小宽度都放不下则返回空）；增加**二次归一化**（redistribution 后 sum > content_space 时等比例压缩）
  2. 新增 3 个回归测试（`render_test.rs`）：宽度=1 时所有 VM 变体不 panic、极窄宽度 `build_wrap_map` 不 panic、空 AssistantBubble 返回 0 行
- **涉及 commit**：待提交
- **验证状态**：565 单元测试全过，clippy/fmt 通过
