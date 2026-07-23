# TUI / 前端领域

> **此文档内容已迁移至 [tui/](tui/tui-index.md) 子目录。各子域详细文档如下。**

## 领域综述

Peri TUI 的前端渲染与交互系统，基于 ratatui-kit 框架。负责终端界面渲染、用户输入处理、消息展示、面板管理。

## 子文档索引

| 文件 | 内容概要 |
|------|---------|
| [tui-index.md](tui/tui-index.md) | 总索引、架构概述、技术方案、面板导航、快捷键规范、设计落地注意事项 |
| [tui-rendering.md](tui/tui-rendering.md) | 渲染系统：AppShell、MessageArea、StatusBar、BgTaskArea、视口裁剪、滚动节流 |
| [tui-events.md](tui/tui-events.md) | ACP 事件系统：事件分派管线、acp_bridge/acp_events、VIEW_MODELS 原子状态 |
| [tui-input.md](tui/tui-input.md) | 输入系统：InputArea、@mention、slash 命令、粘贴、软换行、视口跟随 |
| [tui-panels.md](tui/tui-panels.md) | 面板系统：PanelOverlay 容器、16 个 Panel 设计、导航互斥、快捷键约定 |
| [tui-popups.md](tui/tui-popups.md) | 弹窗系统：PopupOverlay 容器、HITL 审批、AskUser 问答、OAuth 授权、SetupWizard |

## Issue 经验附录

详细的 Issue 经验已分散至各子文档的「相关 Issue 经验」章节中。各 issue 的分类如下：

| Issue | 归属子文档 |
|-------|-----------|
| 渲染相关 (scroll/render/viewport/SystemNote/Markdown/copy/spinner) | [tui-rendering.md](tui/tui-rendering.md) |
| 事件相关 (acp_notifier/acp_bridge/AgentDone/TurnInterrupted/forwarder) | [tui-events.md](tui/tui-events.md) |
| 输入相关 (InputArea/paste/textarea/slash/cursor) | [tui-input.md](tui/tui-input.md) |
| 面板相关 (panel/ThreadBrowser/Model/Login/Config/Theme/Plugin/Workflow) | [tui-panels.md](tui/tui-panels.md) |
| 弹窗相关 (Popup/HITL/AskUser/OAuth/Rewind/Confirm) | [tui-popups.md](tui/tui-popups.md) |
