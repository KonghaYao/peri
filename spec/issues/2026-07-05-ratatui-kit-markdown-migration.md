# 用 ratatui-kit Markdown 组件替换自研 markdown 渲染管道

**状态**：Open
**优先级**：中
**创建日期**：2026-07-05

## 问题描述

当前 TUI 使用自研 markdown 渲染管道（`peri-widgets/markdown/` 13 个文件 + `render_bridge.rs` 353 行），基于 pulldown-cmark + syntect 手动实现事件流状态机 + LRU 缓存 + 视口裁剪。项目已切换到 ratatui-kit GitHub 源，该框架内置 `Markdown` 组件（同样基于 pulldown-cmark + syntect），应删除自研代码，改用框架原生组件。

仅 Assistant 回复的正式文本需要 markdown 渲染，用户文本和 ReasoningBlock 保持纯文本。

## 症状详情

| 维度 | 当前状态 | 目标状态 |
|------|----------|----------|
| 渲染入口 | `render_bridge` + `RENDER_CACHE` 异步预计算 `Vec<Line>` | `MessageArea` 直接读 `VIEW_MODELS`，组件树渲染 |
| UserBubble | 调用 `parse_markdown()`（多余的） | 纯文本 Span |
| AssistantBubble | 调用 `parse_markdown()` → `Vec<Line>` + 前/后空行 | ratatui-kit `Markdown(content: text)` 组件 |
| ReasoningBlock | 纯文本（正确） | 不变 |
| ToolCard 等其余变体 | 纯 Span 拼接 | 不变（纯 Text） |
| 文本选区 | 基于 `Arc<[Line]>` + `wrap_map` 二分查找 | 暂时降级删除，后续用 kit 原生能力补回 |
| markdown 代码量 | peri-widgets/markdown/（13 文件 ~2100 行）+ render_bridge.rs（353 行）+ kit/markdown/mod.rs（20 行）+ text_selection.rs（264 行）≈ 2700 行 | 0 行（全部删除） |

## 期望改进方向

- 删除全部自研 markdown 代码（~2700 行，24 个文件）
- `render_bridge` + `RENDER_CACHE` 完全移除，MessageArea 变为纯组件树
- 仅 AssistantBubble.text 使用 ratatui-kit `Markdown` 组件
- 文本选区降级删除（后续独立补回）

## 涉及文件

详见已归档的 [`2026-07-09-ratatui-kit-markdown-migration.md`](../archive-docs/superpowers/plans/2026-07-09-ratatui-kit-markdown-migration.md) 实施计划（原 07-05 计划文件不存在）。

| 角色 | 文件数 | 典型文件 |
|------|--------|----------|
| 删除 | 16 | `peri-widgets/src/markdown/` (13), `render_bridge.rs`, `kit/markdown/mod.rs`, `text_selection.rs` |
| 修改 | 11 | `message_area.rs` (重写), `view_render.rs`, `atoms.rs`, `entry.rs`, `acp_notifier.rs`, `input_area.rs`, `submit_consumer.rs`, `mod.rs`, `peri-widgets/{Cargo.toml,lib.rs}`, `peri-tui/Cargo.toml` |
| 文档 | 2 | `peri-tui/CLAUDE.md`, `CLAUDE.md` |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-05 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
