# 长对话时消息区域渲染卡顿（流式输出和滚动）

**状态**：Open
**优先级**：中
**创建日期**：2026-07-17
**最后核查**：2026-08-11

## 最新情况（2026-08-11）

渲染侧缓解已落地：`vm_to_lines_cached` 按 VM content_hash 分片缓存（增量渲染）与 ScrollThrottle 滚动节流（scroll_fps）均已存在；长对话帧率是否达标无法静态确认——**待运行时复测**。

**状态**：Open（保持）

## 问题描述

长对话（数百行以上消息）时，消息区域的渲染流畅度明显下降。具体表现为两个场景：AI 流式输出文本时画面刷新有延迟感，以及滚轮滚动时帧率不足。短对话时正常，随消息量增长卡顿加剧。

## 症状详情

| 操作 | 期望行为 | 实际行为 |
|------|----------|----------|
| 长对话中 AI 流式输出 | 逐 token 流畅刷新 | 刷新有延迟感，token 到达与画面更新不同步 |
| 长对话中鼠标滚轮滚动 | 流畅滚动，帧率稳定 | 掉帧，画面不流畅 |
| 长对话中上下键滚动 | 流畅 | 同样受影响 |
| 短对话（几十行）上述操作 | 流畅 | 正常 |

- **加剧因素**：对话越长越卡（消息量线性增长时卡顿加剧）
- **复现频率**：必现（长对话条件下）
- **环境**：macOS，kit 架构

## 出现场景

- 流式输出时，每个 token 到达都触发消息区域完整重渲染，per-frame 开销随消息量增长
- 滚动时，`ScrollThrottle`（16ms）已缓解事件风暴，但渲染帧本身的 per-frame 开销未解决

## 相关 Issue

- `spec/issues/2026-07-05-scroll-performance-lag.md` —— 同类问题，聚焦滚动事件风暴（已通过 ScrollThrottle 部分缓解），本 issue 覆盖流式渲染的 per-frame 开销

## 涉及文件

- `peri-tui/src/kit/message_area/mod.rs` —— MessageArea 渲染管线（每帧执行）
- `peri-tui/src/kit/acp_events.rs` —— push_view_models（每 token 执行）
- `peri-tui/src/kit/acp_types.rs` —— build_view_models（每 token 执行）
- `peri-tui/src/kit/markdown/mod.rs` —— parse_markdown_cached（每 token 执行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-17 | — | Open | agent | 创建 |

## 修复记录

（待后续 fix-issue 追加）
