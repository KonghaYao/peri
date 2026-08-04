# TUI 使用中崩溃：乱码溢出到界面（agent 日志记录时 panic 触发）

**状态**：Fixed
**优先级**：高
**创建日期**：2026-08-04

## 问题描述

TUI 正常使用中（agent 长对话深挖、界面上正显示 `<system-reminder>` 用户消息时），终端界面突然出现"一堆乱码溢出到 TUI 上"，视觉上 TUI 崩溃。进程实际存活（日志持续写入、渲染循环继续），但终端 alt screen 已退出、raw mode 被关闭，界面完全错乱，无法继续使用。崩溃发生无任何 TUI 提示，只留下日志文件中的 ERROR 记录。

## 症状详情

- 用户观察：正在显示 system reminder 时，乱码溢出到 TUI 上，界面崩溃
- 日志证据（`agent-tui.2026-08-04`，2026-08-04T08:36:28.430Z，本地 16:36:28）：
  - `ERROR peri: thread panicked at 'the subscriber should have data for the current span (Id(2252349569499138))!'`
  - panic 位置：`tracing-subscriber-0.3.23/src/layer/context.rs:264:9`（`Context::lookup_current`）
  - 触发点：agent `Reason 阶段：准备调用 LLM step=3` 记录 DEBUG 日志的瞬间
  - backtrace 关键帧：`fmt_layer::on_event` → `format_event` → `on_record` → `event_span` → `lookup_current` panic；panic hook 链中出现 `crossterm::style::Print::write_ansi` 与 `ratatui::init::set_panic_hook`
- 当天共 2 次同类 panic（06:19、08:36），均发生在"工具调用后、Reason 阶段"附近
- panic 后进程存活：agent 08:36:41 正常 `run_react_loop: exit`，TUI 渲染持续到 08:54 之后

## 复现条件

- **复现频率**：偶发（依赖 tokio multi-thread runtime 的 task 线程迁移时序）
- **触发步骤**：
  1. TUI 中启动 agent 长对话，agent 连续多轮工具调用（Bash 等耗时工具）
  2. 工具执行期间 tokio task 跨线程迁移
  3. 迁移后 agent 主循环（Reason 阶段）记录日志，tracing fmt layer 格式化 event 时 panic
  4. panic hook（ratatui 包装）向 stdout 输出终端恢复 escape 序列 → 界面乱码崩溃
- **环境**：macOS 26.5.1，tokio multi-thread runtime，ratatui 0.30.2，tracing-subscriber 0.3.23

## 涉及文件

- `peri-agent/src/agent/stages/tool_dispatch.rs` —— `info_span!` + `span.enter()` guard 在 async 块内跨 await 持有（疑似根因位置）
- `peri-tui/src/main.rs` —— `install_panic_hook` / `init_panic_notify`（panic hook 安装）
- `peri-tui/src/kit/entry.rs` —— `element!(AppShell).fullscreen()` → `ratatui::init()` 包装 panic hook
- `peri-tui/src/launch.rs` —— `panic_notify_rx` 已退役，panic 无 TUI 提示
- `peri-agent/src/agent/stages/speculation_guard.rs` —— 注入 `<system-reminder>`（与崩溃时间重合但非根因，已排除字符切割嫌疑）

## 诊断线索（2026-08-04 调查结论）

> 已完成的调查结论，供修复阶段直接使用。核心结论：**不是 system reminder 字符切割问题**。

1. **根因**：`tool_dispatch.rs:477-482` 的 `span.enter()` guard 跨 await 持有。tokio multi-thread 下 task 迁移线程后，guard drop 会错误重置当前线程的 thread-local current span → 后续 event 的 current span id 在 subscriber 中缺失 → `lookup_current` panic。
2. **乱码机制**：`ratatui::init()`（init.rs:557-568）包装 panic hook：先 `restore()`（`disable_raw_mode` + `LeaveAlternateScreen` escape 序列写 stdout）再调用原 hook。agent task panic 时从**非渲染线程**向 stdout 写 escape，与渲染循环并发写 → 终端 alt screen 退出、raw mode 关闭 → 视觉崩溃。
3. **无提示**：`launch.rs:51` 注释确认 `panic_notify_rx` 已退役，panic 只进日志。
4. reminder 渲染路径字符安全：`extract_reminder_inner` 字节切片安全、`extract_summary`/`truncate_by_width` 按 chars/grapheme/width 截断、ratatui 0.30.2 `set_stringn` 过滤控制字符。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-04 | — | Open | agent | 创建 |
| 2026-08-04 | Open | Fixed | agent | 修复：span 跨线程 + panic hook 链（见修复 #1） |

## 修复记录

### 修复 #1（2026-08-04）

- **操作人**：agent
- **用户原意**：TUI 使用中（显示 system reminder 时）乱码溢出、界面崩溃，需要修复
- **修复内容**：
  - **P1（根因）** `peri-agent/src/agent/stages/tool_dispatch.rs`：`info_span!` + `span.enter()` guard 改为 async 块外创建 span + `.instrument(span)` 包裹整个 future——enter guard 跨 await 持有在 tokio multi-thread 下随 task 线程迁移错误重置 thread-local current span，导致 tracing-subscriber `lookup_current` panic；instrument 每次 poll 重新 enter，跨线程安全
  - **P0（乱码来源）** 新增 `peri-tui/src/kit/panic.rs`（从 main.rs 移入 lib 侧）：AppShell mount 时重装 panic hook，覆盖 `ratatui::init()` 的包装 hook（其 `restore()` 会从任意线程向 stdout 写 escape 序列导致终端乱码）；重装后 panic 只记录日志 + PANIC_NOTIFY 通知
  - **P0（可见性）** `peri-tui/src/kit/entry.rs`：恢复 PANIC_NOTIFY 消费端（原 rx 传入 build_app_and_acp 后被丢弃），收到 panic 通知后在状态栏显示 ⚠️ 提示（首行摘要，30s 过期）
- **涉及 commit**：未提交（工作区改动）
- **验证状态**：待验证
