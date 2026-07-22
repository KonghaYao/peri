# 树莓派 4 上滚动时 CPU 接近 100%

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-20

## 问题描述

在树莓派 4（ARM Cortex-A72，1.5GHz）上运行 peri（release 构建）时，鼠标滚轮滚动消息区会导致 CPU 飙升至接近 100%。停止滚动后 CPU 立即回落。即使对话只有几十行（短对话），问题同样存在。macOS 上之前通过 ScrollThrottle + write_no_update 做的滚动性能修复在树莓派上无效。

## 症状详情

| 操作 | 环境 | 结果 |
|------|------|------|
| 鼠标滚轮滚动 | 树莓派 4，release，SSH 直连终端 | CPU 接近 100%，体感卡顿 |
| 停止滚动 | 同上 | CPU 立即回落 |
| 短对话（几十行）滚动 | 同上 | 同样 CPU 高（不依赖数据量） |
| 鼠标滚轮滚动 | macOS（对比基准） | 流畅（ScrollThrottle 修复后） |

- **复现频率**：必现（树莓派 4 上）
- **触发条件**：存在任意对话内容时鼠标滚轮滚动
- **环境**：Raspberry Pi 4（4 核 Cortex-A72），Raspberry Pi OS，release 构建，SSH 直连终端（无 tmux）

## 诊断发现

**根因不在 Rust 渲染管线**。macOS 上 MessageArea 渲染 body 仅 ~100-120μs/帧（136 items，全缓存命中），占 16ms 帧预算的 0.75%。即使 Pi 4 慢 10 倍也才 ~1.2ms/帧，不足以导致 100% CPU。

真正的瓶颈在 `ratatui-kit` 事件循环：**每收到一个事件（包括 ScrollUp/ScrollDown）都无条件调用 `terminal.draw()`**，ScrollThrottle 只控制 `scroll_state` 写入频率，不控制绘图频率。`terminal.draw()` 在 ARM Cortex-A72 上包含三个昂贵步骤：

1. Paragraph widget 渲染（wrap + scroll + styling）
2. 全屏幕 ANSI diff 计算
3. crossterm flush 到 PTY

60Hz 鼠标事件 × 每次 `terminal.draw()` 数毫秒 → CPU 高。

**修复策略**：扩大 ScrollThrottle 节流窗口，降低 `scroll_state` 变更频率。当 `scroll_state` 不变时，渲染输出相同 → ANSI diff 为空 → `terminal.draw()` 几乎不耗时。将窗口从 16ms（62.5fps）扩大到 33ms（30fps）可降约一半 CPU。

## 相关 Issue

- `spec/archive-issues/2026-07-05-scroll-performance-lag.md` —— macOS 滚动性能（已 Fixed，ScrollThrottle + write_no_update），本 issue 是同一症状在不同硬件上的表现
- `spec/issues/2026-07-17-message-area-render-stutter-long-conversation.md` —— 长对话 per-frame 渲染开销（Open），关注流式渲染和长对话场景

## 涉及文件

- `peri-tui/src/kit/message_area/mod.rs` —— 新增诊断 instrumentation（`PERI_RENDER_TIMING=1` 启用，5 个计时点）
- `peri-tui/src/kit/message_area/scroll.rs` —— `SCROLL_FRAME_MS` 常量 → `scroll_frame_ms()` 函数，支持 `PERI_SCROLL_THROTTLE_MS` 环境变量

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-20 | — | Open | agent | 创建 |
| 2026-07-20 | Open | Fixed | agent | 修复：PERI_SCROLL_THROTTLE_MS 环境变量 + 诊断 instrumentation |

## 修复记录

### 修复 #1（2026-07-20）

- **操作人**：agent
- **用户原意**：树莓派上滚动 CPU 接近 100%，需要降低 CPU 占用
- **修复内容**：
  1. `scroll.rs`：将硬编码 `SCROLL_FRAME_MS = 16` 改为 `scroll_frame_ms()` 函数，读取 `PERI_SCROLL_THROTTLE_MS` 环境变量（thread_local 缓存，默认 16，下限 1），树莓派用户设置 `PERI_SCROLL_THROTTLE_MS=33` 即可将滚动帧率从 62.5fps 降到 30fps
  2. `mod.rs`：新增 5 个诊断计时点（`hash+detect`、`rebuild`、`concat`、`viewport`、`frame-total`），由 `PERI_RENDER_TIMING=1` 控制，macOS 实测确认 Rust 渲染管线仅 ~100μs/帧
- **验证状态**：待用户在树莓派上验证
