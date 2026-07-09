# 消息区在 agent 流式回复中周期性闪白（每 2-5 秒）

**状态**：Open
**优先级**：高
**创建日期**：2026-07-09

## 问题描述

agent 流式回复过程中，消息区每隔约 2-5 秒会出现一次整体闪白——画面短暂全白/全黑后立即恢复正常内容。闪烁不影响 agent 运行，但持续不断的闪白严重影响阅读体验和使用感受。该问题一直存在，非近期引入。

## 症状详情

| 维度 | 观察到的现象 |
|------|--------------|
| 表现 | 消息区整体闪白（或闪黑），瞬间恢复——类似整屏全量重绘 |
| 触发时机 | 仅在 agent 回复/流式输出 token 过程中出现 |
| 周期性 | 约每 2-5 秒闪一次，持续不停，直到流式输出结束 |
| event / resize 触发闪烁 | 与此无关（已有独立 issue #2026-07-07 跟踪 resize + stream end 一次性闪烁） |
| 空闲态 | 不闪烁 |

### 补充（2026-07-09）

**thinking 阶段最为明显**——模型在扩展思考/推理阶段（reasoning chunk 密集输出）时，闪烁频率和感知强度明显高于普通文本 streaming 阶段。可能原因：推理阶段 ViewModel 更新更密集（每条 reasoning chunk 追加都递增 generation），1 秒轮询更频繁地捕获到 generation 变更，触发全量重建的概率更高。

| 组件 | 表现 |
|------|------|
| 消息区 | 闪白 |
| 状态栏 | 不受影响 |
| 输入区 | 不受影响 |

## 复现条件

- **复现频率**：必现（任何 agent 流式回复过程中）
- **触发步骤**：
  1. 在 TUI 中输入任意需 agent 回复的消息
  2. 等待 agent 流式输出 token
  3. 每隔 2-5 秒观察消息区整体闪白一次，持续至流式结束
- **环境**：macOS 26.5.1，ratatui-kit 架构，任意模型

## 涉及文件

| 文件 | 角色 |
|------|------|
| `peri-tui/src/kit/render_bridge.rs` | RENDER_CACHE 预计算任务——每 1 秒轮询 `VIEW_MODELS` atom 检测 generation 变化，流式期间 generation 持续递增，每次轮询触发全量 `rebuild_entries` + RENDER_CACHE 写入，可能导致缓存中间态被消息区读到并渲染为空白 |
| `peri-tui/src/kit/message_area.rs` | 消息区渲染——从 RENDER_CACHE 取数据渲染，当缓存为空或重建过程中读到不完整数据时可能渲染空白帧 |
| `peri-tui/src/kit/atoms.rs` | VIEW_MODELS / RENDER_CACHE atom 定义 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-09 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
