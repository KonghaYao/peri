# tui-message-area

消息区——渲染、滚动、选中、复制、闪烁、流式显示

| # | 日期 | 标题 | 状态 |
|---|------|------|------|
| 1 | 2026-07-04 | [主输入框无法输入——MessageArea ScrollView 事件处理器消费所有键盘事件](2026-07-04-message-area-scrollview-steals-input.md) | Fixed |
| 2 | 2026-07-05 | [消息区多场景崩溃/白屏/滚动异常](2026-07-05-message-area-crashes-and-rendering.md) | Fixed |
| 3 | 2026-07-05 | [消息区域无文本选中和复制能力](2026-07-05-message-area-no-copy-capability.md) | Fixed |
| 4 | 2026-07-05 | [消息区有换行内容时向上滚动，视口底部约 20% 为空白](2026-07-05-message-area-viewport-empty-bottom.md) | Fixed |
| 5 | 2026-07-05 | [消息流渲染同步问题——提交后用户输入不显示、loading 卡死、history 恢复异常](2026-07-05-message-flow-render-sync-freeze.md) | Fixed |
| 6 | 2026-07-05 | [消息流渲染中，AI 消息文本在多分支渲染路径下不可见](2026-07-05-tool-call-ai-text-invisible-after-commit.md) | Fixed |
| 7 | 2026-07-06 | [消息区滚动到底部时内容下方仍有空白](2026-07-06-message-area-bottom-blank-at-scroll-end.md) | Fixed |
| 8 | 2026-07-06 | [Message Area 复制操作导致 TUI 崩溃/卡死](2026-07-06-message-area-copy-complex-content-crash.md) | Fixed |
| 9 | 2026-07-07 | [消息区在宽度变化和流式输出完成时短暂空白闪烁](2026-07-07-message-area-blank-flicker-on-resize-and-stream-end.md) | Fixed |
| 10 | 2026-07-07 | [消息区自动吸底应基于滚动位置就近判断，而非二元开关](2026-07-07-message-area-scroll-proximity-follow.md) | Fixed |
| 11 | 2026-07-07 | [消息区滚动条缺少拖拽、箭头点击和 Human 消息刻度标记](2026-07-07-message-area-scrollbar-interaction.md) | Fixed |
| 12 | 2026-07-07 | [消息流偶发渲染不全，下一事件后才恢复](2026-07-07-message-stream-partial-render-missing-until-next-event.md) | Fixed |
| 13 | 2026-07-07 | [/clear 清空消息区后约 1 秒，旧对话消息全部恢复](2026-07-07-slash-clear-messages-reappear-after-1s.md) | Fixed |
| 14 | 2026-07-09 | [消息区在 agent 流式回复中周期性闪白（每 2-5 秒）](2026-07-09-message-area-periodic-white-flash-streaming.md) | Fixed |
| 15 | 2026-07-09 | [消息区 system-reminder 内容冗余，需改为缩略两行渲染](2026-07-09-system-reminder-condensed-rendering.md) | Fixed |
| 16 | 2026-07-10 | [MessageArea 空态时不显示「✻ Brewed for Xm Xs」总结行](2026-07-10-brewed-summary-missing-in-empty-state.md) | Fixed |
| 17 | 2026-07-11 | [工具调用吞掉前面 AI 消息文本的显示](2026-07-11-final-ai-reply-disappear-after-turn-done.md) | Fixed |
| 18 | 2026-07-11 | [消息区鼠标拖拽选中复制功能因重构回归 + 鼠标拖拽 CPU 暴涨](2026-07-11-message-area-mouse-selection-regression.md) | Fixed |
| 19 | 2026-07-12 | [消息区拖拽复制时 Unicode 字符后段错位（越往后偏移越大）](2026-07-12-message-area-copy-unicode-misalignment.md) | Fixed |
| 20 | 2026-07-12 | [消息区滚动不到最末尾（内容+滚动条均未到底）+ 宽度变化后滚动失效](2026-07-12-message-area-scrollbar-not-reaching-bottom.md) | Fixed |
| 21 | 2026-07-13 | [/clear 后回到 Welcome 页面，滚动条仍然可见](2026-07-13-clear-scrollbar-persists-at-welcome.md) | Fixed |
| 22 | 2026-07-13 | [用户发送 prompt 后消息区不自动跳转到最底部](2026-07-13-submit-no-scroll-to-bottom.md) | Fixed |
| 23 | 2026-07-15 | [Markdown 表格流式输出时显示为原始 pipe 格式](2026-07-15-markdown-table-raw-text-streaming.md) | Fixed |
| 24 | 2026-07-18 | [流式输出时文本和工具调用卡片重复显示](2026-07-18-duplicate-streaming-text-and-tool-cards.md) | Fixed |
| 25 | 2026-07-18 | [流式输出时文本和工具调用卡片重复显示](2026-07-18-duplicate-streaming-tool-cards.md) | Fixed |
| 26 | 2026-07-21 | [长 UserBubble 纯文本消息导致滚动条 thumb 不准](2026-07-21-user-bubble-long-text-scrollbar-inaccurate.md) | Fixed |

*共 26 条*
