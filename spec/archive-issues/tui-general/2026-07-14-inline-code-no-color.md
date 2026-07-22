> 归档于 2026-07-18，原路径 spec/issues/2026-07-14-inline-code-no-color.md
# Markdown 行内代码无颜色渲染

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-14

## 问题描述

消息区中 Markdown 的行内代码（`` `code` ``）不显示任何主题颜色/样式。代码内容本身正常显示，但没有应用 `theme.inline_code_style` 配色，与普通文本完全一致，导致视觉上无法区分行内代码和普通文字。

## 症状详情

| 场景 | 表现 |
|------|------|
| 任意包含 `` `code` `` 的 Markdown 消息 | 行内代码无颜色，与普通文本一致 |
| fenced code block (```) | 不受影响，正常渲染 |

### 示例

发送或渲染以下 Markdown：

```
Use `std::process::Command` to spawn a child process.
```

预期：`` `std::process::Command` `` 应以 `inline_code_style` 颜色（如 WARNING 色 `#ECA76E`）显示，与周围普通文字区分。

实际：`` `std::process::Command` `` 以默认文字色显示，与 `Use` 和 `to spawn a child process` 完全一致。

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 在聊天区输入包含行内代码的 Markdown 消息（例如 `使用 `println!` 打印`）
  2. 发送并查看消息区的渲染结果
  3. 观察行内代码是否有颜色
- **环境**：任意模型/OS/配置

## 涉及文件

- `peri-tui/src/kit/markdown/span_style.rs` —— 行内代码的样式检测逻辑，当前通过 `Modifier::DIM` 哨兵判断，但上游 parser（`ratatui-kit-markdown 0.3.0`）不设置此修饰符
- `peri-tui/src/kit/markdown/mod.rs:217` —— `test_inline_code` 测试用例已更新为断言"不应用 fg 颜色"（第 228 行），说明当前无颜色行为被接受为预期

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-14 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
