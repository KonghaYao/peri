# Spinner 帧推进绑定 acp_bridge 1s tick，应改为 TUI 独立 tick

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-17

## 问题描述

Loading spinner 的帧推进当前挂在 `acp_bridge` 的 1 秒固定 `tokio::time::interval` 上。spinner 的动画帧率设计为 ~100ms/帧（基于壁钟 `start_time.elapsed()` 计算），但因为重渲染触发器是 1s 一次，spinner 视觉上每 1 秒才跳一帧，动画不连续、不流畅。

TUI 渲染与 ACP 事件流应该是解耦的——spinner 帧推进不该依赖 ACP 桥的 tick，而应由 TUI 侧独立管理。

## 症状详情

| 维度 | 当前状态 | 期望状态 |
|------|---------|---------|
| spinner 帧推进周期 | 1 秒（acp_bridge `tokio::time::interval(1s)`） | TUI 侧独立高频 tick（如 50ms） |
| spinner 动画流畅度 | 1 秒跳一帧，肉眼可见卡顿 | 帧计算仍基于壁钟（~100ms/帧），但渲染触发足够频繁 |
| 重渲染触发者 | `acp_bridge` tick 分支 → `advance_spinner()` → cache invalidate → `push_view_models` | TUI 侧独立 tick → 触发组件重渲染，spinner 通过壁钟自动算帧 |
| 非 loading 态 | tick 只调 `advance_spinner()`，不调 `push_view_models`（除非有 running Bash） | 非 loading 态不需要 tick，不浪费资源 |

## 现状

当前 TUI 有两个独立的定时循环：

| 定时器 | 周期 | 位置 | 职责 |
|--------|------|------|------|
| acp_bridge tick | 1s | `peri-tui/src/kit/acp_bridge.rs:67` | spinner 帧推进 + running Bash 计时刷新 |
| service_snapshot tick | 2s | `peri-tui/src/kit/service_snapshot.rs` | CPU/MEM 采样 + 线程列表 + 插件列表 |
| RENDER_HEARTBEAT | 5s | `peri-tui/src/kit/entry.rs:156` | 确保 render loop 周期性唤醒 |

spinner 的帧计算已经基于壁钟（`peri-tui/src/components/spinner/mod.rs:114`：`elapsed_ms / 50 → raw_tick → frame`），不需要外部计数器。问题在于 TUI 缺少一个独立的高频 tick 来驱动 spinner 所在组件的重渲染。

## 期望改进方向

1. **TUI 侧新增独立高频 tick**（如 50ms），专门用于驱动 spinner / 动画类 UI 的重渲染
2. **spinner 组件不再依赖 `advance_spinner()` 计数器**——渲染时完全基于壁钟计算帧位置
3. **acp_bridge 的 1s tick 分支可简化或移除**——running Bash 的计时刷新如需要独立处理可另行设计
4. **非 loading 态不触发 tick**——idle 时节省资源

## 涉及文件

- `peri-tui/src/kit/acp_bridge.rs:67-86` —— 1s tick_interval，spinner 帧推进入口
- `peri-tui/src/kit/acp_types.rs:266-271` —— `CurrentTurn::advance_spinner()` 和 `spinner_frame` 字段
- `peri-tui/src/components/spinner/mod.rs:104-116` —— 帧计算逻辑（已基于壁钟，无需改）
- `peri-tui/src/kit/message_area/footer.rs:74-140` —— spinner 渲染与 state 管理
- `peri-tui/src/kit/entry.rs` —— 入口，需新增 TUI tick task

## 关联

- `spec/issues/2026-07-16-architecture-upgrade-checklist.md` P2-10：「VIEW_MODELS spinner tick（1s）绕过 push_view_models 完整路径」

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-17 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
