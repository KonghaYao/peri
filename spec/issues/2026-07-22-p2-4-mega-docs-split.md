# P2-4：巨型文档拆分

**状态**：Open
**优先级**：低
**类型**：文档改进
**创建日期**：2026-07-22
**来源**：架构成熟度评估 — 工程规范维度

## Problem Statement

两个文档过大，影响查找效率和维护：

| 文件 | 大小 | 内容 |
|------|------|------|
| `spec/global/domains/TUI-PAGE.md` | 126KB | TUI 渲染、事件、面板、快捷键等全部 TUI 领域知识 |
| `spec/global/domains/tui.md` | 58KB | TUI 陷阱速查 + issue 归档 |

两者有内容重叠（都在记录 TUI 陷阱），但组织方式不同。126KB 的单一文档加载慢、难以定位具体问题。

## 建议方案

1. 将 `TUI-PAGE.md` 按子域拆分为 4-6 个文档（渲染、事件、面板、快捷键、输入、弹窗）
2. 将 `tui.md` 的 issue 归档部分与 `TUI-PAGE.md` 的陷阱速查合并，消除重复
3. 在 `domains/` 下建 `tui/` 子目录组织拆分后的文档

## 涉及文件

- `spec/global/domains/TUI-PAGE.md`（126KB）
- `spec/global/domains/tui.md`（58KB）

## 风险

- **低**：纯文档重组。注意保留 wikilink 和交叉引用不失效
