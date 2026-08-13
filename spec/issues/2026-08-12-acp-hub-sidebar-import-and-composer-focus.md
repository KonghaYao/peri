# acp-hub 左栏混入外部会话且输入区出现蓝色边框

**状态**：Fixed
**优先级**：中
**创建日期**：2026-08-12

## 问题描述

进入 acp-hub Web 面板时，左侧项目会话列表自动包含 ACP 中已有的历史会话，无法区分由 acp-hub 创建的会话与外部会话。期望左栏默认只记录 acp-hub 创建或用户明确导入的会话，同时在面板中提供按项目导入 ACP 历史会话的入口。聊天输入区当前聚焦时还出现明显蓝色边框，期望重构为更克制的一体化输入面。

## 现状

- server 启动时会把 legacy Registry 中的 ACP sessions 自动写入 SQLite project session catalog。
- Web 左栏直接渲染全部 `project_sessions`，没有来源字段或显式导入流程。
- Composer 容器使用 accent focus border，textarea 又受到全局 `:focus-visible` 描边影响。

## 期望改进方向

- project session 增加来源语义：hub 创建、用户导入、legacy 隐藏。
- 新增认证且可幂等的 `session/import` action；导入候选来自当前 project cwd 对应的 ACP `session/list` Registry 投影。
- 左栏仅投影 hub/imported；旧自动导入记录迁移为隐藏，用户明确导入后再出现。
- 重构 Composer 为无蓝色边框的一体化浮层，键盘焦点改用中性阴影/底部状态，不移除可访问焦点反馈。

## 涉及文件

- `acp-hub/server/src/persist/metadata.rs` —— SQLite schema、来源与显式导入。
- `acp-hub/server/src/control/project_service.rs` —— Catalog 投影边界。
- `acp-hub/server/src/channel/command_coordinator.rs` —— `session/import` action 编排。
- `acp-hub/proto/src/action.rs` —— 导入协议。
- `acp-hub/web/src/panel/components/ProjectSidebar.tsx` —— 导入入口与候选面板。
- `acp-hub/web/src/panel/components/Composer.tsx` —— 输入区结构与焦点样式。
- `acp-hub/web/src/styles.css` —— 导入列表与 Composer 视觉 token。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-12 | — | Open | agent | 创建并开始修复 |
| 2026-08-12 | Open | Fixed | agent | 增加 session 来源迁移、显式导入流程并重构 Composer 焦点样式 |

## 修复记录

### 修复 #1（2026-08-12）

- **操作人**：agent
- **用户原意**：左栏只保留 acp-hub 生成或用户明确导入的会话，并去掉输入框聚焦时的蓝色边框。
- **修复内容**：SQLite schema 升级到 v2，project session 增加 `hub/imported/legacy_hidden` 来源；新增幂等 `session/import` action 和按 project cwd 筛选的导入 Dialog；Registry 不再投影隐藏历史；Composer 改为中性一体化浮层和无蓝框焦点反馈。
- **涉及 commit**：无
- **验证状态**：已验证（proto、server 全量、coordinator 导入链、frontend、typecheck、build、clippy）
