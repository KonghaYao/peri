# tui-general

通用 TUI——Loading 状态、崩溃、CPU 高负载、Resize、性能

| # | 日期 | 标题 | 状态 |
|---|------|------|------|
| 1 | 2026-07-05 | [Enter 提交 & /clear 清屏触发 Hook type mismatch panic 导致 TUI 崩溃](2026-07-05-enter-clear-hook-mismatch-panic.md) | fixed+ verify |
| 2 | 2026-07-05 | [鼠标晃动导致 CPU 暴涨](2026-07-05-mouse-move-cpu-spike.md) | fixed |
| 3 | 2026-07-05 | [长数据高速滚动时刷新卡顿/掉帧](2026-07-05-scroll-performance-lag.md) | Fixed |
| 4 | 2026-07-05 | [Thread 切换后 TUI 完全卡死——加载长历史 thread 时渲染后冻结](2026-07-05-thread-switch-freeze-long-history.md) | Fixed |
| 5 | 2026-07-06 | [TUI 输入 hello 并 Enter 后 CPU 100%](2026-07-06-enter-hello-cpu-spike.md) | Verified |
| 6 | 2026-07-07 | [Loading 中按 Ctrl+C 会直接退出应用](2026-07-07-ctrl-c-exits-during-loading.md) | Fixed |
| 7 | 2026-07-08 | [Loading 指示器完全不显示——TUI ACP 重构后遗症](2026-07-08-loading-indicator-never-displays.md) | Fixed |
| 8 | 2026-07-08 | [MQ 注入的 user message 不通过 ACP 反馈到 TUI，导致用户气泡缺失 + AI 消息重叠](2026-07-08-mq-injected-user-message-not-in-tui.md) | Fixed |
| 9 | 2026-07-08 | [TUI 丢弃 ACP agent_message_chunk 的 messageId，消息边界靠推断而非协议字段](2026-07-08-tui-drop-acp-messageid-boundary.md) | Fixed |
| 10 | 2026-07-08 | [消灭 ViewModel 共享类型体系，自定义事件精简化](2026-07-08-viewmodel-elimination.md) | Fixed |
| 11 | 2026-07-08 | [ViewModelsSnapshot 大重构：单层持久化向量 + 消灭所有旁路](2026-07-08-viewmodels-flatten-refactor.md) | Fixed |
| 12 | 2026-07-09 | [输入 "agent " 开头触发 OpenPanel 命令，无需 / 前缀](2026-07-09-agent-prefix-triggers-command-without-slash.md) | Fixed |
| 13 | 2026-07-11 | [Ctrl+C 取消后未回滚用户消息、未恢复文本到输入框](2026-07-11-cancel-no-rollback-no-restore.md) | fixed |
| 14 | 2026-07-11 | [History 恢复会话时 scroll_to_bottom 过早，布局未就绪导致滚动位置停在中间](2026-07-11-history-replay-scroll-too-early.md) | Fixed |
| 15 | 2026-07-11 | [Markdown 表格使用 ratatui-kit TableTheme 替代硬编码样式](2026-07-11-markdown-table-ratatui-kit-theme.md) | Fixed |
| 16 | 2026-07-13 | [主 agent 完成回复后 loading 不退，因后台 agent 仍在运行](2026-07-13-main-agent-done-loading-persists-bg-still-running.md) | Fixed |
| 17 | 2026-07-14 | [Markdown 行内代码无颜色渲染](2026-07-14-inline-code-no-color.md) | Fixed |
| 18 | 2026-07-15 | [快速缩小终端宽度到极小值时程序直接退出崩溃](2026-07-15-terminal-rapid-shrink-width-crash.md) | Fixed |
| 19 | 2026-07-17 | [TUI Loading 状态机制 split-brain——三个写入源造成状态分裂](2026-07-17-loading-state-split-brain.md) | Fixed |
| 20 | 2026-07-17 | [Spinner 帧推进绑定 acp_bridge 1s tick，应改为 TUI 独立 tick](2026-07-17-spinner-tick-decouple-from-acp-bridge.md) | Fixed |
| 21 | 2026-07-17 | [SystemNote 的 Warning/Error 等级字体颜色未区分，全部显示为灰色](2026-07-17-system-note-level-color-not-rendered.md) | Fixed |
| 22 | 2026-07-19 | [streaming_mode 配置切换无效——"block"和"none"模式未实现，渲染始终为流式](2026-07-19-streaming-mode-config-not-effective.md) | Fixed |
| 23 | 2026-07-20 | [树莓派 4 上滚动时 CPU 接近 100%](2026-07-20-raspberry-pi-scroll-cpu-high.md) | Fixed |
| 24 | 2026-07-21 | [Auto Compact 完成后 loading 短暂变冷却态再恢复](2026-07-21-compact-completed-loading-flicker.md) | Fixed |

*共 24 条*
