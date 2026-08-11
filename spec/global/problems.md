# 问题索引

按关键词索引已归档 issue，遇到相似问题时快速定位历史经验。

## 关键词索引

### 3.0 重构

- [issue_2026-08-05-3.0-l1-bg-tasks-to-agent 3.0 L1——后台任务从中间件迁入 Agent 层 async_tasks](domains/agent.md#issue_2026-08-05-3.0-l1-bg-tasks-to-agent) — architecture
- [issue_2026-08-05-3.0-l2-middleware-assembly-to-agent 3.0 L2——中间件装配迁入 SessionFactory](domains/agent.md#issue_2026-08-05-3.0-l2-middleware-assembly-to-agent) — architecture
- [issue_2026-08-05-3.0-l3-subagent-factory-to-agent 3.0 L3——subagent 创建统一迁入 Agent 层](domains/agent.md#issue_2026-08-05-3.0-l3-subagent-factory-to-agent) — architecture
- [issue_2026-08-05-3.0-l4-langfuse-bypass-consumer 3.0 L4——Langfuse 重构为 Controller 侧旁路消费者](domains/agent.md#issue_2026-08-05-3.0-l4-langfuse-bypass-consumer) — architecture
- [issue_2026-08-05-3.0-l5-executor-split 3.0 L5——executor 薄壳化拆分到 session/exec/](domains/agent.md#issue_2026-08-05-3.0-l5-executor-split) — architecture
- [issue_2026-08-05-3.0-m-event-chain-canonical 3.0 M——Agent 事件三通道收敛单链路、v2_tx 双轨退役](domains/agent.md#issue_2026-08-05-3.0-m-event-chain-canonical) — architecture
- [issue_2026-08-05-3.0-m-resources-layer 3.0 M——新建 peri-resources 存储通道层](domains/agent.md#issue_2026-08-05-3.0-m-resources-layer) — architecture

### /clear

- [/clear 后 Welcome 页滚动条仍然可见——ScrollbarFields 未重置](domains/tui/tui-rendering.md#issue_2026-07-13-clear-scrollbar-persists-at-welcome) — tui

### 20帧强制窗口

- [History 恢复会话时 scroll_to_bottom 过早——布局未就绪即无效 offset](domains/tui/tui-rendering.md#issue_2026-07-11-history-replay-scroll-too-early) — tui

### ACP messageId

- [issue_2026-07-08-tui-drop-acp-messageid-boundary TUI 丢弃 ACP messageId，消息边界靠推断](domains/tui/tui-events.md#issue_2026-07-08-tui-drop-acp-messageid-boundary) — tui

### AgentDone → TurnDone

- [issue_2026-07-06-enter-hello-cpu-spike TUI 输入 hello 并 Enter 后 CPU 100%](domains/tui/tui-rendering.md#issue_2026-07-06-enter-hello-cpu-spike) — tui

### AgentEvent 通道

- [SubagentStarted 事件被 notifier 丢弃，SubAgent 卡片完全不显示](domains/agent.md#issue_2026-07-07-subagent-group-header-shows-agent-instead-of-task-description) — agent

### AgentPool

- [issue_2026-07-16-model-login-switch-not-effective-until-restart AgentPool 缓存未失效导致旧配置复用](domains/tui/tui-panels.md#issue_2026-07-16-model-login-switch-not-effective-until-restart) — tui

### AlwaysUpdate

- [issue_2026-07-05-input-unicode-cursor-misalignment 输入框 CJK 光标残影——CjkGhostFix: AlwaysUpdate 续接 cell](domains/tui/tui-input.md#issue_2026-07-05-input-unicode-cursor-misalignment) — tui

### Anthropic adapter

- [parallel 工具调用时 tool_result 顺序反转——insert(0) 导致](domains/agent.md#issue_2026-07-18-anthropic-adapter-tool-result-order-reversed) — agent
- [system prompt cache 拆分缺失——main agent 边界标记失效](domains/agent.md#issue_2026-07-17-anthropic-adapter-system-cache-split-missing) — agent

### Arc 重建

- [issue_2026-07-07-inputarea-mouse-click-cursor-positioning InputArea 鼠标点击光标定位——Arc 每帧重建导致 handler 读到 None](domains/tui/tui-input.md#issue_2026-07-07-inputarea-mouse-click-cursor-positioning) — tui

### AreaTracker

- [issue_2026-07-07-inputarea-mouse-click-cursor-positioning InputArea 鼠标点击光标定位——AreaTracker hook 值拷贝模式](domains/tui/tui-input.md#issue_2026-07-07-inputarea-mouse-click-cursor-positioning) — tui

### AskUserQuestion

- [AskUserQuestion 面板多选交互缺失 + 文本超长不换行](domains/tui/tui-popups.md#issue_2026-07-14-ask-user-multiselect-tui-support) — tui
- [issue_2026-08-02-agent-asks-user-too-late-in-ambiguous-env agent 在环境失败/症状不明时静态深挖不收敛，提问过晚](domains/agent.md#issue_2026-08-02-agent-asks-user-too-late-in-ambiguous-env) — agent

### BTreeMap

- [tools 数组顺序随 HashMap 迭代，新增工具触发 rehash 后 prompt cache 全断](domains/agent.md#issue_2026-07-18-tools-hashmap-order-breaks-prompt-cache) — agent

### BackgroundTaskResult

- [后台 subagent 完成通知中 tool_calls_count 始终为 0——4 处硬编码](domains/tui/tui-rendering.md#issue_2026-07-10-bg-subagent-tool-count-always-zero) — tui

### BaseTool::timeout

- [issue_2026-07-13-agent-tool-300s-timeout-interrupts-normal-tasks BaseTool trait 提供 timeout() 方法](domains/agent.md#issue_2026-07-13-agent-tool-300s-timeout-interrupts-normal-tasks) — agent

### BgTaskRegistry

- [Workflow Tool 快速失败后任务条目永久卡在黄色——complete_workflow() 永不调用](domains/tui/tui-panels.md#issue_2026-07-13-workflow-tool-error-task-stuck-and-panel-freeze) — tui

### Bash 超时

- [issue_2026-08-10-bash-timeout-misdiagnosis-promotes-stalled-processes Bash 未设 stdin 致读 stdin 进程挂死被 promote 成永不结束后台任务](domains/agent.md#issue_2026-08-10-bash-timeout-misdiagnosis-promotes-stalled-processes) — tools

### Bracketed Paste

- [issue_2026-07-05-paste-newline-triggers-submit 输入框粘贴含换行文本时直接触发 Enter 提交](domains/tui/tui-input.md#issue_2026-07-05-paste-newline-triggers-submit) — tui

### Brewed总结

- [issue_2026-07-10-brewed-summary-missing-in-empty-state 空态不显示 Brewed 总结行](domains/tui/tui-rendering.md#issue_2026-07-10-brewed-summary-missing-in-empty-state) — tui

### Buffer越界

- [issue_2026-07-15-terminal-rapid-shrink-width-crash 终端极窄宽度时表格渲染 buffer 越界崩溃](domains/tui/tui-rendering.md#issue_2026-07-15-terminal-rapid-shrink-width-crash) — tui

### CJK 光标

- [issue_2026-07-05-input-unicode-cursor-misalignment 输入框 Unicode 光标估算错误，多个白色光标残影](domains/tui/tui-input.md#issue_2026-07-05-input-unicode-cursor-misalignment) — tui

### CJK 折行

- [textarea 缺少软换行——只按 \n 拆分逻辑行，不支持终端宽度自动折行](domains/tui/tui-input.md#issue_2026-07-09-textarea-no-soft-wrap) — tui

### CJK偏移

- [issue_2026-07-12-message-area-copy-unicode-misalignment CJK 字符累积偏移——视觉坐标转逻辑偏移](domains/tui/tui-rendering.md#issue_2026-07-12-message-area-copy-unicode-misalignment) — tui

### CLI 配置

- [issue_2026-08-06-cli-config-db-path CLI 新增 --db-path/--config-file——配置重定向与 fallback 语义](domains/agent.md#issue_2026-08-06-cli-config-db-path) — agent

### COPY_MESSAGE_UNTIL

- [issue_2026-07-06-message-area-copy-complex-content-crash 复制操作导致 TUI 卡死——status_bar render body 写 atom 自激回路](domains/tui/tui-rendering.md#issue_2026-07-06-message-area-copy-complex-content-crash) — tui

### CjkGhostFix

- [issue_2026-07-05-input-unicode-cursor-misalignment 输入框 CJK 光标残影——CjkGhostFix: AlwaysUpdate 续接 cell](domains/tui/tui-input.md#issue_2026-07-05-input-unicode-cursor-misalignment) — tui

### Cron

- [issue_2026-08-04-cron-trigger-lost-after-turn-error Cron 触发在 turn 结束后静默丢失——bridge 绑定 per-turn V2Session](domains/agent.md#issue_2026-08-04-cron-trigger-lost-after-turn-error) — tools
- [issue_2026-08-07-cron-tool-task-never-triggers Cron 工具注册到临时 scheduler——downcast 恒失败静默失效](domains/agent.md#issue_2026-08-07-cron-tool-task-never-triggers) — tools

### Ctrl+C回滚

- [issue_2026-07-11-cancel-no-rollback-no-restore Ctrl+C 取消后未回滚用户消息和文本](domains/tui/tui-events.md#issue_2026-07-11-cancel-no-rollback-no-restore) — tui

### Defer 消息

- [bg agent 完成后主 agent 永久卡死、合成消息未注入主消息区](domains/agent.md#issue_2026-07-07-bg-agent-complete-no-resume) — agent

### downcast

- [issue_2026-08-06-e2e-workflow-not-completing downcast_arc 对 trait object 调 type_id() 恒失败——完成通知无订阅者](domains/agent.md#issue_2026-08-06-e2e-workflow-not-completing) — workflow
- [issue_2026-08-07-cron-tool-task-never-triggers CronSchedulerPort downcast 恒失败——cron 工具注册到临时实例](domains/agent.md#issue_2026-08-07-cron-tool-task-never-triggers) — tools

### Drag事件过滤

- [issue_2026-07-11-message-area-mouse-selection-regression 消息区鼠标 Drag 事件穿透导致 CPU 暴涨](domains/tui/tui-rendering.md#issue_2026-07-11-message-area-mouse-selection-regression) — tui

### E2E 测试

- [issue_2026-08-06-e2e-glob-grep-match-suffix-missing E2E Judge 误判 Glob 卡缺匹配数后缀——卡片被挤出视口](domains/agent.md#issue_2026-08-06-e2e-glob-grep-match-suffix-missing) — tools
- [issue_2026-08-06-e2e-tmux-server-dies E2E 残留 tui-test-* session 干扰导致 tmux server 挂掉](domains/tui/tui-rendering.md#issue_2026-08-06-e2e-tmux-server-dies) — tui

### ESC 卡死

- [AskUserQuestion 面板 ESC 退出后 TUI 卡死——全局 ESC handler 先于面板 handler 消费事件](domains/tui/tui-popups.md#issue_2026-07-13-ask-user-esc-freeze-reject) — tui

### Enter 关闭

- [Login 面板 Enter 选择 provider 后不关闭面板——标准步骤缺失](domains/tui/tui-panels.md#issue_2026-07-13-login-panel-enter-does-not-close) — tui

### Event::Paste

- [issue_2026-07-05-paste-newline-triggers-submit 输入框粘贴含换行文本时直接触发 Enter 提交](domains/tui/tui-input.md#issue_2026-07-05-paste-newline-triggers-submit) — tui

### EventBus

- [issue_2026-07-16-eventbus-unified-emission 统一事件发射路径走 v2 EventBus](domains/agent.md#issue_2026-07-16-eventbus-unified-emission) — agent

### EventPriority::High

- [AskUserQuestion 面板 ESC 卡死——全局 handler（Normal 优先级）先消费事件](domains/tui/tui-popups.md#issue_2026-07-13-ask-user-esc-freeze-reject) — tui

### Event过滤

- [issue_2026-07-15-setup-wizard-no-paste-login-no-edit 向导用 `let Event::Key` 过滤丢弃 Paste 事件](domains/tui/tui-input.md#issue_2026-07-15-setup-wizard-no-paste-login-no-edit) — tui

### Global scope

- [issue_2026-07-05-mouse-move-cpu-spike 鼠标晃动导致 CPU 暴涨](domains/tui/tui-rendering.md#issue_2026-07-05-mouse-move-cpu-spike) — tui

### HashMap 顺序

- [tools 数组顺序随 HashMap 迭代顺序，跨进程 resume 或新增 key 触发 rehash 后 prompt cache 全断](domains/agent.md#issue_2026-07-18-tools-hashmap-order-breaks-prompt-cache) — agent

### Hook type mismatch

- [issue_2026-07-05-enter-clear-hook-mismatch-panic Enter 提交 & /clear 清屏触发 Hook type mismatch panic 导致 TUI 崩溃](domains/tui/tui-input.md#issue_2026-07-05-enter-clear-hook-mismatch-panic) — tui

### Langfuse

- [issue_2026-08-07-langfuse-v3-subagent-parent-chain-validation-memo v3 后线上 trace subagent 父链验证备忘——先立部署切点再采样](domains/agent.md#issue_2026-08-07-langfuse-v3-subagent-parent-chain-validation-memo) — langfuse
- [issue_2026-08-02-langfuse-bridge-drops-provider-request-id bridge 事件转换用 `..` 丢弃 provider request_id](domains/agent.md#issue_2026-08-02-langfuse-bridge-drops-provider-request-id) — langfuse
- [issue_2026-08-02-reason-rs-loses-request-id-without-usage usage=None 时 request_id 被 unwrap_or 替换一并丢弃](domains/agent.md#issue_2026-08-02-reason-rs-loses-request-id-without-usage) — langfuse
- [issue_2026-08-05-langfuse-subagent-attribution-stack-lifetime subagent 内容整体错挂主 agent——LIFO 栈归属改身份注册表](domains/agent.md#issue_2026-08-05-langfuse-subagent-attribution-stack-lifetime) — langfuse
- [issue_2026-08-05-langfuse-batcher-drops-during-slow-flush batcher 命令通道容量=max_events+DropNew——慢 flush 期间静默丢事件](domains/agent.md#issue_2026-08-05-langfuse-batcher-drops-during-slow-flush) — langfuse
- [issue_2026-08-03-langfuse-trace-step-order-shuffled-with-parallel-subagents 并行 subagent 的 step 顺序错乱——观测图依赖并发完成序](domains/agent.md#issue_2026-08-03-langfuse-trace-step-order-shuffled-with-parallel-subagents) — langfuse

### Login 面板

- [Login 面板 Enter 选择 provider 后不关闭面板](domains/tui/tui-panels.md#issue_2026-07-13-login-panel-enter-does-not-close) — tui
- [Login 面板缺少 Provider 类型编辑字段](domains/tui/tui-panels.md#issue_2026-07-17-login-panel-missing-provider-type-field) — tui

### Markdown

- [Markdown 行内代码无颜色渲染——Modifier::DIM 哨兵检测失效](domains/tui/tui-rendering.md#issue_2026-07-14-inline-code-no-color) — tui

### MessageFlags

- [Compact 标记在 Session 恢复后丢失——DB 加载路径只 SELECT content 不读 flags 列](domains/agent.md#issue_2026-07-17-compact-flags-lost-on-session-restore) — compact

### MessageKind::Defer

- [Goal 自驱续跑在 v2 架构下完全断裂——Info 不唤醒循环，Defer 才能唤醒](domains/agent.md#issue_2026-07-15-goal-continuation-loop-broken-in-v2) — agent

### MessageQueue

- [bg agent 完成后主 agent 永久卡死、合成消息未注入主消息区](domains/agent.md#issue_2026-07-07-bg-agent-complete-no-resume) — agent

### Modifier::DIM 哨兵

- [行内代码颜色检测基于第三方库修饰符，上游版本升级改变行为](domains/tui/tui-rendering.md#issue_2026-07-14-inline-code-no-color) — tui

### MouseMove 高频事件

- [issue_2026-07-05-mouse-move-cpu-spike 鼠标晃动导致 CPU 暴涨](domains/tui/tui-rendering.md#issue_2026-07-05-mouse-move-cpu-spike) — tui

### OpenAI 流式

- [issue_2026-07-05-tool-call-ai-text-invisible-after-commit AI 消息文本在 ViewCommit 后消失——OpenAI stream.rs ToolUse 分支遗漏](domains/agent.md#issue_2026-07-05-tool-call-ai-text-invisible-after-commit) — agent

### panic hook

- [issue_2026-08-04-tui-garbled-crash-after-agent-panic agent panic 后 TUI 乱码崩溃——span guard 跨线程 + panic hook 写 escape 序列](domains/tui/tui-rendering.md#issue_2026-08-04-tui-garbled-crash-after-agent-panic) — tui

### Paste事件

- [issue_2026-07-05-paste-newline-triggers-submit 输入框粘贴含换行文本时直接触发 Enter 提交](domains/tui/tui-input.md#issue_2026-07-05-paste-newline-triggers-submit) — tui
- [issue_2026-07-15-setup-wizard-no-paste-login-no-edit Setup 向导表单不支持粘贴](domains/tui/tui-input.md#issue_2026-07-15-setup-wizard-no-paste-login-no-edit) — tui

### Plugin 面板

- [Plugin 面板 Tab 切换导致 UI 卡死——use_state 自激循环](domains/tui/tui-panels.md#issue_2026-07-13-plugin-panel-left-right-freeze) — tui

### Prompt Cache

- [tools 数组顺序随 HashMap 迭代，提示词缓存前缀全断](domains/agent.md#issue_2026-07-18-tools-hashmap-order-breaks-prompt-cache) — agent

### ProviderType

- [Login 面板缺少 ProviderType 编辑字段——编辑模式无法修改 provider 类型](domains/tui/tui-panels.md#issue_2026-07-17-login-panel-missing-provider-type-field) — tui

### RENDER_CACHE 同步

- [issue_2026-07-05-message-flow-render-sync-freeze 消息流渲染同步问题——提交后用户输入不显示、loading 卡死、history 恢复异常](domains/tui/tui-events.md#issue_2026-07-05-message-flow-render-sync-freeze) — tui

### rewind

- [issue_2026-08-06-e2e-rewind-input-not-refilled Rewind 回填文本被 <system-reminder> 注入块污染](domains/tui/tui-popups.md#issue_2026-08-06-e2e-rewind-input-not-refilled) — tui
- [issue_2026-08-02-rewind-popup-hardening-quartet rewind 弹窗四连：测试口径/选择越界/错误事件复用/e2e 假绿](domains/tui/tui-popups.md#issue_2026-08-02-rewind-popup-hardening-quartet) — tui

### RwLock 重入死锁

- [issue_2026-08-02-plugin-panel-uninstall-enter-freeze Plugin 面板卸载按 Enter 卡死——scrutinee 中 .read() 临时 guard 重入死锁](domains/tui/tui-panels.md#issue_2026-08-02-plugin-panel-uninstall-enter-freeze) — tui
- [issue_2026-08-08-e2e-compact-command-screenshot-too-early /compact 完成提示重注入块读锁 guard 存活致写锁死锁](domains/tui/tui-events.md#issue_2026-08-08-e2e-compact-command-screenshot-too-early) — tui

### ScrollThrottle

- [长数据高速滚动时刷新卡顿——高频 write 触发多次原子通知→多次 draw](domains/tui/tui-rendering.md#issue_2026-07-05-scroll-performance-lag) — tui

### ScrollView 事件拦截

- [issue_2026-07-04-message-area-scrollview-steals-input 主输入框无法输入——ScrollView 事件处理器消费所有键盘事件](domains/tui/tui-rendering.md#issue_2026-07-04-message-area-scrollview-steals-input) — tui

### ScrollView 事件过滤

- [issue_2026-07-05-mouse-move-cpu-spike 鼠标晃动导致 CPU 暴涨](domains/tui/tui-rendering.md#issue_2026-07-05-mouse-move-cpu-spike) — tui

### ScrollView 双重滚动

- [issue_2026-07-05-message-area-crashes-and-rendering 消息区多场景崩溃/白屏/滚动异常](domains/tui/tui-rendering.md#issue_2026-07-05-message-area-crashes-and-rendering) — tui

### ScrollbarFields 重置

- [/clear 后 Welcome 分支提前 return 跳过 ScrollbarFields 更新](domains/tui/tui-rendering.md#issue_2026-07-13-clear-scrollbar-persists-at-welcome) — tui

### SubAgent

- [SubAgent 沙箱写工具：让 readonly agent 能输出交接文件](domains/agent.md#issue_2026-07-18-subagent-write-sandbox-tool) — agent
- [子 agent 工具包装器未透传 aliases()，resolve_tool() 找不到别名](domains/agent.md#issue_2026-07-16-subagent-tool-alias-not-resolved) — agent
- [issue_2026-08-09-subagent-resume-mechanism subagent 中断后无法找回现场——resume_thread_id 恢复机制](domains/agent.md#issue_2026-08-09-subagent-resume-mechanism) — subagent
- [issue_2026-08-06-e2e-bg-task-area-entry-missing L5 归位回归——工具注入早于 set_parent_session 致 bg 静默降级同步](domains/agent.md#issue_2026-08-06-e2e-bg-task-area-entry-missing) — subagent

### SubagentStarted

- [SubagentStarted 事件被 acp_notifier 静默丢弃，SubAgent 卡片完全不显示](domains/agent.md#issue_2026-07-07-subagent-group-header-shows-agent-instead-of-task-description) — agent

### SubagentStopped

- [主 agent 完成后 loading 不退——SubagentStopped 覆盖了 TurnDone 清除的 loading](domains/tui/tui-rendering.md#issue_2026-07-13-main-agent-done-loading-persists-bg-still-running) — tui

### SystemNote

- [SystemNote 的 Warning/Error 等级颜色未区分——渲染函数忽略 data.level 枚举](domains/tui/tui-rendering.md#issue_2026-07-17-system-note-level-color-not-rendered) — tui
- [issue_2026-07-16-system-note-cache-warning-position-wrong Cache 警告 SystemNote 消息流位置错位](domains/tui/tui-rendering.md#issue_2026-07-16-system-note-cache-warning-position-wrong) — tui
- [issue_2026-08-08-e2e-compact-command-screenshot-too-early /compact 完成 SystemNote 在 replay 后丢失——PENDING_COMPACT_NOTE 跨 replay 桥接](domains/tui/tui-events.md#issue_2026-08-08-e2e-compact-command-screenshot-too-early) — tui

### thinking 渲染

- [issue_2026-08-11-tui-think-end-messageid TUI 无"推理结束"信号——thinking 动画空转；sync_cache 缓存复用守卫陈旧](domains/tui/tui-rendering.md#issue_2026-08-11-tui-think-end-messageid) — tui

### TUI 交互

- [AskUserQuestion 多选交互缺失——TUI 面板未实现 multiSelect](domains/tui/tui-popups.md#issue_2026-07-14-ask-user-multiselect-tui-support) — tui

### TUI 独立循环

- [spinner 帧推进应解耦自业务事件流——TUI 侧独立高频 tick 驱动](domains/tui/tui-rendering.md#issue_2026-07-17-spinner-tick-decouple-from-acp-bridge) — tui

### Tab 切换

- [Plugin 面板 Tab 切换导致 UI 卡死——handler 重新注册→自激循环](domains/tui/tui-panels.md#issue_2026-07-13-plugin-panel-left-right-freeze) — tui

### Theme 面板

- [下载完成后 Theme 面板未刷新新增主题——回调未触发列表重载](domains/tui/tui-panels.md#issue_2026-07-15-theme-panel-not-refreshed-after-download) — tui

### ThreadBrowser

- [issue_2026-07-05-hide-empty-threads-in-history-panel ThreadBrowser 面板应隐藏 message_count 为 0 的空线程](domains/tui/tui-panels.md#issue_2026-07-05-hide-empty-threads-in-history-panel) — tui

### TuiNoteLevel

- [SystemNote 的 Warning/Error 等级颜色未区分——渲染忽略 data.level 用启发式判断](domains/tui/tui-rendering.md#issue_2026-07-17-system-note-level-color-not-rendered) — tui

### TurnInterrupted

- [issue_2026-07-11-cancel-no-rollback-no-restore TurnInterrupted 处理器缺少零产出回滚](domains/tui/tui-events.md#issue_2026-07-11-cancel-no-rollback-no-restore) — tui

### UI 卡死

- [Plugin 面板 Tab 切换导致 UI 卡死——state 更新→重渲染→handler 重注册→自激](domains/tui/tui-panels.md#issue_2026-07-13-plugin-panel-left-right-freeze) — tui

### Unicode宽度

- [issue_2026-07-12-message-area-copy-unicode-misalignment Unicode 字符宽度导致复制后段错位](domains/tui/tui-rendering.md#issue_2026-07-12-message-area-copy-unicode-misalignment) — tui

### UserBubble

- [用户提交后消息区不自动跳转底部——用户主动操作应触发强制滚底](domains/tui/tui-rendering.md#issue_2026-07-13-submit-no-scroll-to-bottom) — tui

### V2Session

- [Compact 效果在 v2 路径中跨 prompt 丢失——persist_tx 始终为 None](domains/agent.md#issue_2026-07-18-compact-effect-lost-between-prompts-v2) — compact

### Welcome 页

- [/clear 后 Welcome 页滚动条仍然可见——提前 return 跳过状态重置](domains/tui/tui-rendering.md#issue_2026-07-13-clear-scrollbar-persists-at-welcome) — tui

### WriteSandbox

- [SubAgent 沙箱写工具：allowedWriteDirs 声明目录白名单，路径穿越校验](domains/agent.md#issue_2026-07-18-subagent-write-sandbox-tool) — agent

### _meta 序列化

- [issue_2026-07-08-history-replay-missing-tool-interactions History 面板恢复的对话历史缺少工具调用](domains/tui/tui-events.md#issue_2026-07-08-history-replay-missing-tool-interactions) — tui
- [issue_2026-07-09-history-session-switch-loading-freeze History 面板切换 session 后 loading 永久卡死](domains/tui/tui-rendering.md#issue_2026-07-09-history-session-switch-loading-freeze) — tui

### acp_notifier

- [SubagentStarted 事件被 notifier 丢弃，SubAgent 卡片完全不显示](domains/agent.md#issue_2026-07-07-subagent-group-header-shows-agent-instead-of-task-description) — agent

### active=false

- [issue_2026-07-04-message-area-scrollview-steals-input 主输入框无法输入——ScrollView 事件处理器消费所有键盘事件](domains/tui/tui-rendering.md#issue_2026-07-04-message-area-scrollview-steals-input) — tui

### allowedWriteDirs

- [SubAgent 沙箱写工具：让 readonly agent 能输出交接文件](domains/agent.md#issue_2026-07-18-subagent-write-sandbox-tool) — agent

### arboard 剪贴板线程

- [issue_2026-07-05-message-area-crashes-and-rendering 消息区多场景崩溃/白屏/滚动异常](domains/tui/tui-rendering.md#issue_2026-07-05-message-area-crashes-and-rendering) — tui

### atom 写入副作用

- [issue_2026-07-03-tui-double-slash-cpu-spike TUI 输入区输入 // 导致 CPU 持续高负载](domains/tui/tui-rendering.md#issue_2026-07-03-tui-double-slash-cpu-spike) — tui

### background agent

- [issue_2026-07-09-bg-agent-loading-never-stops-after-first-turn Agent background 模式启动后 loading 不停止](domains/agent.md#issue_2026-07-09-bg-agent-loading-never-stops-after-first-turn) — agent

### bg agent

- [bg agent 完成后主 agent 永久卡死、合成消息未注入主消息区](domains/agent.md#issue_2026-07-07-bg-agent-complete-no-resume) — agent
- [主 agent 完成后 loading 不退——bg agent 仍在运行导致 SubagentStopped 覆盖 loading 状态](domains/tui/tui-rendering.md#issue_2026-07-13-main-agent-done-loading-persists-bg-still-running) — tui

### bg subagent

- [后台 subagent 完成通知中 tool_calls_count 始终为 0——4 处硬编码 0](domains/tui/tui-rendering.md#issue_2026-07-10-bg-subagent-tool-count-always-zero) — tui

### block_continue

- [Goal 自驱续跑在 v2 架构下完全断裂](domains/agent.md#issue_2026-07-15-goal-continuation-loop-broken-in-v2) — agent

### block类型变更

- [issue_2026-07-15-markdown-table-raw-text-streaming 增量缓存需覆盖 block 类型变更场景](domains/tui/tui-rendering.md#issue_2026-07-15-markdown-table-raw-text-streaming) — tui

### boundary marker

- [Anthropic adapter 未对 request.system 调用 split_system_blocks](domains/agent.md#issue_2026-07-17-anthropic-adapter-system-cache-split-missing) — agent

### bundle key不匹配

- [issue_2026-07-13-config-language-switch-no-effect i18n bundle key "zh" vs "zh-CN" 不匹配](domains/tui/tui-panels.md#issue_2026-07-13-config-language-switch-no-effect) — tui

### cached_context

- [Compact 标记在 Session 恢复后丢失——cached_context JSON 不支持 flags](domains/agent.md#issue_2026-07-17-compact-flags-lost-on-session-restore) — compact

### can_reuse

- [issue_2026-07-15-markdown-table-raw-text-streaming can_reuse 未检测 block 类型变更](domains/tui/tui-rendering.md#issue_2026-07-15-markdown-table-raw-text-streaming) — tui

### cancel_ask_user

- [AskUserQuestion 面板 ESC 退出后 TUI 卡死——ESC handler 优先级问题](domains/tui/tui-popups.md#issue_2026-07-13-ask-user-esc-freeze-reject) — tui

### close_active_panel

- [Login 面板 Enter 选择 provider 后不关闭面板——未调用 close_active_panel()](domains/tui/tui-panels.md#issue_2026-07-13-login-panel-enter-does-not-close) — tui

### committed

- [issue_2026-07-16-system-note-cache-warning-position-wrong SystemNote 直接 push committed 绕过 TurnSegment](domains/tui/tui-rendering.md#issue_2026-07-16-system-note-cache-warning-position-wrong) — tui

### compact

- [Compact 效果在 v2 路径中跨 prompt 丢失——persist_tx 始终为 None](domains/agent.md#issue_2026-07-18-compact-effect-lost-between-prompts-v2) — compact
- [Compact 标记（truncated/excluded）在 Session 恢复后丢失——DB 与 cached_context 双重遗漏](domains/agent.md#issue_2026-07-17-compact-flags-lost-on-session-restore) — compact
- [issue_2026-07-25-micro-compact-silently-fails-within-turn Micro Compact 同 turn 内静默失效——dry-run 与投影双重职责冲突](domains/agent.md#issue_2026-07-25-micro-compact-silently-fails-within-turn) — compact
- [issue_2026-07-25-compact-decay-full-fails-micro-skipped-no-fallback Compact 退化链——Micro 跳过/Full 失败/无 fallback/计数器跨域污染](domains/agent.md#issue_2026-07-25-compact-decay-full-fails-micro-skipped-no-fallback) — compact
- [issue_2026-07-25-micro-compact-treadmill-reclaim-target-zero Micro Compact 跑步机效应——reclaim_target 恒 0，每次"成功"预算不降](domains/agent.md#issue_2026-07-25-micro-compact-treadmill-reclaim-target-zero) — compact
- [issue_2026-07-29-micro-compact-no-system-note Micro Compact 无 SystemNote——Debug 格式 vs 文案字面值错配](domains/agent.md#issue_2026-07-29-micro-compact-no-system-note) — compact

### complete_workflow

- [Workflow Tool 快速失败时 early return 跳过通知任务 spawn](domains/tui/tui-panels.md#issue_2026-07-13-workflow-tool-error-task-stuck-and-panel-freeze) — tui

### content_text 丢失

- [issue_2026-07-05-tool-call-ai-text-invisible-after-commit AI 消息文本在 ViewCommit 后消失——stream.rs content_text 丢失](domains/agent.md#issue_2026-07-05-tool-call-ai-text-invisible-after-commit) — agent

### deprecation

- [ask_user → interaction 类型迁移——消除 24 个 deprecation 警告](domains/agent.md#issue_2026-07-18-ask-user-migration) — agent

### distance_to_bottom

- [消息区自动吸底应基于 proximity 判断而非二元开关](domains/tui/tui-rendering.md#issue_2026-07-07-message-area-scroll-proximity-follow) — tui

### drain_for_end

- [Goal 自驱续跑在 v2 架构下完全断裂](domains/agent.md#issue_2026-07-15-goal-continuation-loop-broken-in-v2) — agent

### event_tx 关闭

- [同步 Agent 子工具调用卡片完全不显示——or_insert_with 保留已关闭的 event_tx](domains/agent.md#issue_2026-07-13-sync-agent-tool-cards-not-showing) — agent

### flags 持久化

- [Compact 效果在 v2 主路径中跨 prompt 丢失——V2Session 的 persist_tx=None](domains/agent.md#issue_2026-07-18-compact-effect-lost-between-prompts-v2) — compact

### flush-then-push

- [issue_2026-07-16-system-note-cache-warning-position-wrong 消息推 committed 前需先 flush current_turn](domains/tui/tui-rendering.md#issue_2026-07-16-system-note-cache-warning-position-wrong) — tui

### footer 常驻

- [issue_2026-08-04-spinner-footer-missing-after-restore-history 恢复历史后 spinner footer 消失——idle 早退点](domains/tui/tui-rendering.md#issue_2026-08-04-spinner-footer-missing-after-restore-history) — tui

### fork SubAgent

- [issue_2026-07-07-fork-subagent-no-parent-conversation-history Fork SubAgent 收不到父对话历史](domains/agent.md#issue_2026-07-07-fork-subagent-no-parent-conversation-history) — agent

### forwarder

- [流式输出时文本和工具调用卡片重复——render 事件双轨扇出](domains/tui/tui-events.md#issue_2026-07-18-duplicate-streaming-text-and-tool-cards) — tui

### goal 续跑

- [Goal 自驱续跑在 v2 架构下完全断裂——MessageKind 语义错误 + ActOutput 不完整](domains/agent.md#issue_2026-07-15-goal-continuation-loop-broken-in-v2) — agent

### history 恢复

- [History 恢复会话时 scroll_to_bottom 过早——布局未就绪即无效 offset](domains/tui/tui-rendering.md#issue_2026-07-11-history-replay-scroll-too-early) — tui

### history 草稿

- [issue_2026-07-05-message-flow-render-sync-freeze 消息流渲染同步问题——提交后用户输入不显示、loading 卡死、history 恢复异常](domains/tui/tui-events.md#issue_2026-07-05-message-flow-render-sync-freeze) — tui

### hook state 稳态写入

- [issue_2026-07-06-enter-hello-cpu-spike TUI 输入 hello 并 Enter 后 CPU 100%](domains/tui/tui-rendering.md#issue_2026-07-06-enter-hello-cpu-spike) — tui

### i18n

- [issue_2026-07-13-config-language-switch-no-effect 语言切换 key 不匹配导致无效](domains/tui/tui-panels.md#issue_2026-07-13-config-language-switch-no-effect) — tui

### insert(0) vs push

- [Anthropic adapter 合并连续 Tool 消息时 insert(0) 导致 tool_result 顺序反转](domains/agent.md#issue_2026-07-18-anthropic-adapter-tool-result-order-reversed) — agent

### interaction 统一

- [ask_user → interaction 类型迁移——消除 24 个 deprecation 警告](domains/agent.md#issue_2026-07-18-ask-user-migration) — agent

### loading spinner 自激

- [issue_2026-07-06-enter-hello-cpu-spike TUI 输入 hello 并 Enter 后 CPU 100%](domains/tui/tui-rendering.md#issue_2026-07-06-enter-hello-cpu-spike) — tui

### loading 事实源

- [issue_2026-07-05-message-flow-render-sync-freeze 消息流渲染同步问题——提交后用户输入不显示、loading 卡死、history 恢复异常](domains/tui/tui-events.md#issue_2026-07-05-message-flow-render-sync-freeze) — tui

### loading 生命周期

- [issue_2026-07-09-bg-agent-loading-never-stops-after-first-turn Agent background 模式启动后 loading 不停止](domains/agent.md#issue_2026-07-09-bg-agent-loading-never-stops-after-first-turn) — agent
- [主 agent 完成后 loading 不退——SubagentStopped 无条件设 phase=PromptRunning 覆盖了 TurnDone 清除的 loading](domains/tui/tui-rendering.md#issue_2026-07-13-main-agent-done-loading-persists-bg-still-running) — tui
- [issue_2026-08-05-loading-stuck-after-transport-close transport 关闭后 is_loading 永久卡 true——pump 独占 sender 使"退出=关闭"](domains/tui/tui-events.md#issue_2026-08-05-loading-stuck-after-transport-close) — tui
- [issue_2026-08-05-cancel-consumer-loading-phase-desync cancel 后 loading 闪回——is_loading 应从 phase 单一事实源派生](domains/tui/tui-events.md#issue_2026-08-05-cancel-consumer-loading-phase-desync) — tui

### message_count 过滤

- [issue_2026-07-05-hide-empty-threads-in-history-panel ThreadBrowser 面板应隐藏 message_count 为 0 的空线程](domains/tui/tui-panels.md#issue_2026-07-05-hide-empty-threads-in-history-panel) — tui

### or_insert_with

- [同步 Agent 子工具调用卡片完全不显示——or_insert_with 保留第一 turn 的 SubAgentTool](domains/agent.md#issue_2026-07-13-sync-agent-tool-cards-not-showing) — agent

### parent_messages

- [issue_2026-07-07-fork-subagent-no-parent-conversation-history Fork SubAgent 收不到父对话历史——parent_messages 注入失败](domains/agent.md#issue_2026-07-07-fork-subagent-no-parent-conversation-history) — agent

### persist_tx=None

- [Compact 效果在 v2 路径中跨 prompt 丢失——flags 写入为 no-op](domains/agent.md#issue_2026-07-18-compact-effect-lost-between-prompts-v2) — compact

### phase=PromptRunning

- [SubagentStopped 无条件设 phase=PromptRunning——覆盖了 TurnDone 清除的 loading](domains/tui/tui-rendering.md#issue_2026-07-13-main-agent-done-loading-persists-bg-still-running) — tui

### proximity 跟随

- [消息区自动吸底应基于 proximity 判断，距离≤阈值→吸底](domains/tui/tui-rendering.md#issue_2026-07-07-message-area-scroll-proximity-follow) — tui

### ratatui-kit 迁移

- [状态栏上下文消耗显示 + 缓存命中率警告在 ratatui-kit 迁移后丢失](domains/tui/tui-rendering.md#issue_2026-07-13-statusbar-context-cache-display-regression) — tui

### ratatui-kit迁移

- [issue_2026-07-11-message-area-mouse-selection-regression 大规模重构依赖标记与迁移回归](domains/tui/tui-rendering.md#issue_2026-07-11-message-area-mouse-selection-regression) — tui

### render body 写 atom

- [issue_2026-07-06-message-area-copy-complex-content-crash 复制操作导致 TUI 卡死——status_bar render body 写 atom 自激回路](domains/tui/tui-rendering.md#issue_2026-07-06-message-area-copy-complex-content-crash) — tui

### render 自激循环

- [issue_2026-07-03-tui-double-slash-cpu-spike TUI 输入区输入 // 导致 CPU 持续高负载](domains/tui/tui-rendering.md#issue_2026-07-03-tui-double-slash-cpu-spike) — tui

### resolve_tool

- [子 agent 工具包装器未实现 aliases() 透传，子 agent 内部 resolve_tool() 找不到别名](domains/agent.md#issue_2026-07-16-subagent-tool-alias-not-resolved) — agent

### scroll_start_for_selected

- [issue_2026-07-06-panels-selection-no-scroll-follow 面板选中项超出可见行后看不到——scroll_start_for_selected 算法复用](domains/tui/tui-rendering.md#issue_2026-07-06-panels-selection-no-scroll-follow) — tui

### scroll_to_bottom

- [History 恢复会话时 scroll_to_bottom 过早——布局未就绪](domains/tui/tui-rendering.md#issue_2026-07-11-history-replay-scroll-too-early) — tui
- [用户提交后消息区不自动跳转底部——proximity guard 阻止滚底](domains/tui/tui-rendering.md#issue_2026-07-13-submit-no-scroll-to-bottom) — tui

### session replay

- [issue_2026-07-08-history-replay-missing-tool-interactions History 面板恢复的对话历史缺少工具调用](domains/tui/tui-events.md#issue_2026-07-08-history-replay-missing-tool-interactions) — tui
- [issue_2026-07-09-history-session-switch-loading-freeze History 面板切换 session 后 loading 永久卡死](domains/tui/tui-rendering.md#issue_2026-07-09-history-session-switch-loading-freeze) — tui

### session 恢复

- [Compact 标记在 Session 恢复后丢失——DB 和 cached_context 双重遗漏](domains/agent.md#issue_2026-07-17-compact-flags-lost-on-session-restore) — compact

### slash popup

- [issue_2026-07-03-tui-double-slash-cpu-spike TUI 输入区输入 // 导致 CPU 持续高负载](domains/tui/tui-rendering.md#issue_2026-07-03-tui-double-slash-cpu-spike) — tui

### slash 前缀检查

- [issue_2026-07-09-agent-prefix-triggers-command-without-slash 输入 agent 开头触发 OpenPanel 命令](domains/tui/tui-input.md#issue_2026-07-09-agent-prefix-triggers-command-without-slash) — tui

### spinner

- [spinner 帧推进挂在 acp_bridge 1s interval——100ms/帧动画实际每 1s 跳一帧](domains/tui/tui-rendering.md#issue_2026-07-17-spinner-tick-decouple-from-acp-bridge) — tui

### split_system_blocks

- [Anthropic adapter 未调用已存在的 split_system_blocks() 拆分静态/动态 system 内容](domains/agent.md#issue_2026-07-17-anthropic-adapter-system-cache-split-missing) — agent

### system prompt cache

- [Anthropic adapter 未拆分 system 块的静态/动态内容，边界标记失效](domains/agent.md#issue_2026-07-17-anthropic-adapter-system-cache-split-missing) — agent

### textarea

- [textarea 缺少软换行——长行被截断且视口跟随异常](domains/tui/tui-input.md#issue_2026-07-09-textarea-no-soft-wrap) — tui

### tick 解耦

- [spinner 帧推进应解耦自业务事件流——TUI 侧独立高频 tick 驱动](domains/tui/tui-rendering.md#issue_2026-07-17-spinner-tick-decouple-from-acp-bridge) — tui

### tmux

- [长数据高速滚动时刷新卡顿——tmux 下 PTY 开销放大高频 write 问题](domains/tui/tui-rendering.md#issue_2026-07-05-scroll-performance-lag) — tui

### tool wrapper

- [子 agent 工具包装器未完整透传 BaseTool trait 的 aliases() 方法](domains/agent.md#issue_2026-07-16-subagent-tool-alias-not-resolved) — agent

### tool_call 重放

- [issue_2026-07-08-history-replay-missing-tool-interactions History 面板恢复的对话历史缺少工具调用](domains/tui/tui-events.md#issue_2026-07-08-history-replay-missing-tool-interactions) — tui

### tool_calls_count

- [后台 subagent 完成通知中 tool_calls_count 始终为 0——硬编码默认值](domains/tui/tui-rendering.md#issue_2026-07-10-bg-subagent-tool-count-always-zero) — tui

### tool_result 顺序

- [Anthropic adapter 合并 Tool 消息时 insert(0) 导致 tool_result 顺序反转](domains/agent.md#issue_2026-07-18-anthropic-adapter-tool-result-order-reversed) — agent

### trim_start_matches

- [issue_2026-07-09-agent-prefix-triggers-command-without-slash 输入 agent 开头触发 OpenPanel 命令](domains/tui/tui-input.md#issue_2026-07-09-agent-prefix-triggers-command-without-slash) — tui

### u16 overflow

- [issue_2026-07-05-message-area-crashes-and-rendering 消息区多场景崩溃/白屏/滚动异常](domains/tui/tui-rendering.md#issue_2026-07-05-message-area-crashes-and-rendering) — tui

### update_config

- [issue_2026-07-16-model-login-switch-not-effective-until-restart client.update_config() 推送配置到 ACP 服务端](domains/tui/tui-panels.md#issue_2026-07-16-model-login-switch-not-effective-until-restart) — tui

### use_effect 自激回路

- [issue_2026-07-09-message-area-periodic-white-flash-streaming 消息区流式回复中周期性闪白——吸底 use_effect 写入环路](domains/tui/tui-rendering.md#issue_2026-07-09-message-area-periodic-white-flash-streaming) — tui

### use_state 自激

- [Plugin 面板 Tab 切换导致 UI 卡死——use_state 自激循环](domains/tui/tui-panels.md#issue_2026-07-13-plugin-panel-left-right-freeze) — tui

### use_state 调用顺序

- [issue_2026-07-05-enter-clear-hook-mismatch-panic Enter 提交 & /clear 清屏触发 Hook type mismatch panic 导致 TUI 崩溃](domains/tui/tui-input.md#issue_2026-07-05-enter-clear-hook-mismatch-panic) — tui

### vis_width对齐

- [issue_2026-07-12-message-area-scrollbar-not-reaching-bottom vis_width 与实际渲染宽度不一致](domains/tui/tui-rendering.md#issue_2026-07-12-message-area-scrollbar-not-reaching-bottom) — tui

### workflow

- [Workflow Tool 快速失败后任务条目永久卡在黄色——complete_workflow() 永不调用](domains/tui/tui-panels.md#issue_2026-07-13-workflow-tool-error-task-stuck-and-panel-freeze) — tui

### write_no_update

- [长数据高速滚动卡顿——高频 write() 触发多次原子通知→多次 draw](domains/tui/tui-rendering.md#issue_2026-07-05-scroll-performance-lag) — tui

### 参数归一化

- [issue_2026-08-02-grep-glob-path-parameter-ignored Grep/Glob 的 path 参数被 normalize_params 静默重命名丢弃](domains/agent.md#issue_2026-08-02-grep-glob-path-parameter-ignored) — tools

### 下载

- [下载完成后 Theme 面板未刷新新增主题——异步操作回调未通知面板](domains/tui/tui-panels.md#issue_2026-07-15-theme-panel-not-refreshed-after-download) — tui

### 主题颜色

- [Markdown 行内代码无颜色——span_style 通过 Modifier::DIM 哨兵检测行内代码失败](domains/tui/tui-rendering.md#issue_2026-07-14-inline-code-no-color) — tui

### 事件丢弃

- [SubagentStarted 事件被 acp_notifier 静默丢弃，SubAgent 卡片完全不显示](domains/agent.md#issue_2026-07-07-subagent-group-header-shows-agent-instead-of-task-description) — agent

### 事件优先级

- [AskUserQuestion 面板 ESC 卡死——全局 handler（Normal 优先级）先于面板 handler 消费 ESC](domains/tui/tui-popups.md#issue_2026-07-13-ask-user-esc-freeze-reject) — tui

### 事件路径统一

- [issue_2026-07-16-eventbus-unified-emission 三条独立发射路径合并为单一 EventBus 入口](domains/agent.md#issue_2026-07-16-eventbus-unified-emission) — agent

### 事件路由

- [issue_2026-07-04-message-area-scrollview-steals-input 主输入框无法输入——ScrollView 事件处理器消费所有键盘事件](domains/tui/tui-rendering.md#issue_2026-07-04-message-area-scrollview-steals-input) — tui

### 二元开关

- [消息区自动吸底应基于 proximity 而非二元开关——滚一下就永久不回](domains/tui/tui-rendering.md#issue_2026-07-07-message-area-scroll-proximity-follow) — tui

### 保存失败

- [issue_2026-07-13-config-panel-save-silently-discards-errors Config 保存失败静默丢弃无提示](domains/tui/tui-panels.md#issue_2026-07-13-config-panel-save-silently-discards-errors) — tui

### 分隔线

- [AskUserQuestion 面板宽终端下布局混乱——固定宽度分隔线不匹配](domains/tui/tui-popups.md#issue_2026-07-15-ask-user-panel-layout-wrong-wide-terminal) — tui

### 列表刷新

- [下载完成后 Theme 面板未刷新新增主题——异步回调未触发 atom/event 通知](domains/tui/tui-panels.md#issue_2026-07-15-theme-panel-not-refreshed-after-download) — tui

### 功能回归

- [状态栏上下文消耗显示 + 缓存命中率警告在 ratatui-kit 迁移后丢失](domains/tui/tui-rendering.md#issue_2026-07-13-statusbar-context-cache-display-regression) — tui

### 动画帧率

- [spinner 帧推进挂在 acp_bridge 1s interval——~100ms/帧动画实际每 1s 跳一帧](domains/tui/tui-rendering.md#issue_2026-07-17-spinner-tick-decouple-from-acp-bridge) — tui

### 单帧延迟

- [issue_2026-07-10-brewed-summary-missing-in-empty-state 状态读取在 mutation 前导致单帧延迟](domains/tui/tui-rendering.md#issue_2026-07-10-brewed-summary-missing-in-empty-state) — tui

### 后台任务

- [issue_2026-08-05-bg-cancel-abort-skips-cleanup Agent 类 bg 取消仅 abort 跳过收尾——active_agents 泄漏 + 子进程孤儿化](domains/agent.md#issue_2026-08-05-bg-cancel-abort-skips-cleanup) — tools
- [issue_2026-08-05-bg-command-expect-panic-via-rpc BgCommand 两处 expect 可经公开 RPC 传 None 触发 panic](domains/agent.md#issue_2026-08-05-bg-command-expect-panic-via-rpc) — tools
- [issue_2026-08-05-bg-task-over-limit-still-runs 超限后台任务仍实际运行——检查-注册竞态](domains/agent.md#issue_2026-08-05-bg-task-over-limit-still-runs) — tools
- [issue_2026-08-05-bg-shell-task-id-collision bg shell task_id 截断 UUID v7 前缀——同毫秒碰撞静默吞 Completed](domains/agent.md#issue_2026-08-05-bg-shell-task-id-collision) — tools
- [issue_2026-08-05-cancel-bg-task-workflow-kind-ineffective Workflow 注册固定 Kill(None)——cancel 只删条目 runner 继续跑](domains/agent.md#issue_2026-08-05-cancel-bg-task-workflow-kind-ineffective) — tools

### 双轨扇出

- [流式输出时文本和工具调用卡片重复——render 事件双轨扇出到 VIEW_MODELS](domains/tui/tui-events.md#issue_2026-07-18-duplicate-streaming-text-and-tool-cards) — tui

### 鼠标手势

- [issue_2026-08-01-model-panel-profile-row-click-no-response Model 面板 profile 行点击无响应——click-as-enter 覆盖遗漏](domains/tui/tui-panels.md#issue_2026-08-01-model-panel-profile-row-click-no-response) — tui
- [issue_2026-08-11-tui-click-expand-broken 单击展开失效三缺陷叠加——坐标空间不一致 + Drag 无阈值 + 焦点不回退](domains/tui/tui-rendering.md#issue_2026-08-11-tui-click-expand-broken) — tui

### 数据门

- [issue_2026-08-10-chat-redesign-slice1-data-gates Chat redesign Slice 1——5 项数据门只读核验为零消费方定案](domains/tui/tui-events.md#issue_2026-08-10-chat-redesign-slice1-data-gates) — tui

### 同步 SubAgent

- [同步 Agent 子工具调用卡片完全不显示——or_insert_with 保留已关闭 channel](domains/agent.md#issue_2026-07-13-sync-agent-tool-cards-not-showing) — agent

### 启发式判断

- [SystemNote 颜色用文本关键词判断而非 data.level 枚举——不可靠](domains/tui/tui-rendering.md#issue_2026-07-17-system-note-level-color-not-rendered) — tui

### 吸底滚动

- [issue_2026-07-09-message-area-periodic-white-flash-streaming 消息区流式回复中周期性闪白——吸底 use_effect 写入环路](domains/tui/tui-rendering.md#issue_2026-07-09-message-area-periodic-white-flash-streaming) — tui

### 固定宽度

- [AskUserQuestion 面板宽终端下布局混乱——WRAP_WIDTH=80 固定换行不匹配](domains/tui/tui-popups.md#issue_2026-07-15-ask-user-panel-layout-wrong-wide-terminal) — tui

### 增量缓存

- [issue_2026-07-15-markdown-table-raw-text-streaming Markdown 增量缓存 block 类型翻转导致表格不渲染](domains/tui/tui-rendering.md#issue_2026-07-15-markdown-table-raw-text-streaming) — tui

### 多选

- [AskUserQuestion 多选交互缺失——JSON Schema 已支持但 TUI 面板未实现](domains/tui/tui-popups.md#issue_2026-07-14-ask-user-multiselect-tui-support) — tui

### 子工具卡片

- [同步 Agent 子工具调用卡片完全不显示——event_tx 在第二 turn 时已关闭](domains/agent.md#issue_2026-07-13-sync-agent-tool-cards-not-showing) — agent

### 字段枚举

- [Login 面板 LoginEditField 枚举缺少 ProviderType 变体——编辑模式无法修改 provider 类型](domains/tui/tui-panels.md#issue_2026-07-17-login-panel-missing-provider-type-field) — tui

### 宽终端

- [AskUserQuestion 面板宽终端下布局混乱——硬编码宽度与面板实际宽度不匹配](domains/tui/tui-popups.md#issue_2026-07-15-ask-user-panel-layout-wrong-wide-terminal) — tui

### 工具别名

- [子 agent 工具包装器未实现 aliases() 透传，resolve_tool() 找不到别名](domains/agent.md#issue_2026-07-16-subagent-tool-alias-not-resolved) — agent

### 工具超时

- [issue_2026-07-13-agent-tool-300s-timeout-interrupts-normal-tasks 统一 300s 超时导致 Agent/SubAgent 中断](domains/agent.md#issue_2026-07-13-agent-tool-300s-timeout-interrupts-normal-tasks) — agent
- [issue_2026-08-02-background-task-15s-timeout-kills-and-misreports 后台任务受 15s 默认超时约束——只杀 wrapper 致孤儿进程 + 通知误报](domains/agent.md#issue_2026-08-02-background-task-15s-timeout-kills-and-misreports) — tools

### 差异化超时

- [issue_2026-07-13-agent-tool-300s-timeout-interrupts-normal-tasks 按工具类型差异化超时策略](domains/agent.md#issue_2026-07-13-agent-tool-300s-timeout-interrupts-normal-tasks) — agent

### 布局时序

- [History 恢复会话时 scroll_to_bottom 过早——布局未就绪即无效 offset](domains/tui/tui-rendering.md#issue_2026-07-11-history-replay-scroll-too-early) — tui

### 并行工具调用

- [Anthropic adapter 合并连续 Tool 消息时 tool_result 顺序反转](domains/agent.md#issue_2026-07-18-anthropic-adapter-tool-result-order-reversed) — agent

### 快速失败

- [Workflow Tool 快速失败后任务条目永久卡在黄色——early return 跳过通知](domains/tui/tui-panels.md#issue_2026-07-13-workflow-tool-error-task-stuck-and-panel-freeze) — tui

### 技术债清理

- [ask_user → interaction 统一方案后旧类型未迁移，24 个 deprecation 警告](domains/agent.md#issue_2026-07-18-ask-user-migration) — agent

### 持久化丢失

- [issue_2026-07-13-model-login-panel-persistence-lost Model/Login 面板切换后重启配置丢失](domains/tui/tui-panels.md#issue_2026-07-13-model-login-panel-persistence-lost) — tui

### 提交交互

- [用户提交后消息区不自动跳转底部——is_loading=true 但 UserBubble 仍在飞行中](domains/tui/tui-rendering.md#issue_2026-07-13-submit-no-scroll-to-bottom) — tui

### 图片输入

- [issue_2026-07-29-image-input-support TUI→ACP→Agent 管线图片输入通道打通（@image 语法）](domains/agent.md#issue_2026-07-29-image-input-support) — agent

### 文本折行

- [AskUserQuestion 面板宽终端下布局混乱——WRAP_WIDTH=80 固定换行不匹配](domains/tui/tui-popups.md#issue_2026-07-15-ask-user-panel-layout-wrong-wide-terminal) — tui
- [AskUserQuestion 面板文本超长不换行——Line::from() 不折行](domains/tui/tui-popups.md#issue_2026-07-14-ask-user-multiselect-tui-support) — tui

### 文本选中

- [issue_2026-07-11-message-area-mouse-selection-regression 消息区鼠标拖拽选中复制功能回归修复](domains/tui/tui-rendering.md#issue_2026-07-11-message-area-mouse-selection-regression) — tui

### 本地提交 vs ACP 事件

- [issue_2026-07-05-message-flow-render-sync-freeze 消息流渲染同步问题——提交后用户输入不显示、loading 卡死、history 恢复异常](domains/tui/tui-events.md#issue_2026-07-05-message-flow-render-sync-freeze) — tui

### 条件早退

- [issue_2026-07-05-enter-clear-hook-mismatch-panic Enter 提交 & /clear 清屏触发 Hook type mismatch panic 导致 TUI 崩溃](domains/tui/tui-input.md#issue_2026-07-05-enter-clear-hook-mismatch-panic) — tui

### 标记持久化

- [Compact 标记（truncated/excluded）在 Session 恢复后丢失——需独立于内容字段存储](domains/agent.md#issue_2026-07-17-compact-flags-lost-on-session-restore) — compact

### 沙箱写

- [SubAgent 沙箱写工具：allowedWriteDirs 白名单 + 路径穿越校验](domains/agent.md#issue_2026-07-18-subagent-write-sandbox-tool) — agent

### 流式输出

- [流式输出时文本和工具调用卡片重复——render 事件双轨扇出](domains/tui/tui-events.md#issue_2026-07-18-duplicate-streaming-text-and-tool-cards) — tui

### 路径精简

- [issue_2026-07-31-tui-tool-card-absolute-path-too-long 工具卡头行绝对路径过长——仅显示层裁剪 cwd 前缀](domains/tui/tui-rendering.md#issue_2026-07-31-tui-tool-card-absolute-path-too-long) — tui

### 消息边界推断

- [issue_2026-07-08-tui-drop-acp-messageid-boundary TUI 丢弃 ACP messageId，消息边界靠推断](domains/tui/tui-events.md#issue_2026-07-08-tui-drop-acp-messageid-boundary) — tui

### 滚动性能

- [长数据高速滚动时刷新卡顿——高频 write 触发多次 draw，tmux 下 PTY 开销放大](domains/tui/tui-rendering.md#issue_2026-07-05-scroll-performance-lag) — tui

### 滚动条

- [/clear 后 Welcome 页滚动条仍然可见——提前 return 跳过 ScrollbarFields 重置](domains/tui/tui-rendering.md#issue_2026-07-13-clear-scrollbar-persists-at-welcome) — tui
- [issue_2026-07-12-message-area-scrollbar-not-reaching-bottom 滚动条 thumb 未抵达底部 + 宽度变化失效](domains/tui/tui-rendering.md#issue_2026-07-12-message-area-scrollbar-not-reaching-bottom) — tui

### 状态刷新

- [Login 面板 Enter 后不关闭面板——标准步骤：close_active_panel() + 推送 snapshot](domains/tui/tui-panels.md#issue_2026-07-13-login-panel-enter-does-not-close) — tui

### 状态同步

- [下载完成后 Theme 面板未刷新新增主题——异步操作完成后需通知面板刷新](domains/tui/tui-panels.md#issue_2026-07-15-theme-panel-not-refreshed-after-download) — tui

### 状态栏

- [状态栏上下文消耗显示 + 缓存命中率警告在 ratatui-kit 迁移后丢失](domains/tui/tui-rendering.md#issue_2026-07-13-statusbar-context-cache-display-regression) — tui

### 硬编码

- [后台 subagent 完成通知中 tool_calls_count 始终为 0——4 处构造硬编码默认值](domains/tui/tui-rendering.md#issue_2026-07-10-bg-subagent-tool-count-always-zero) — tui

### 确定性序列化

- [HashMap 顺序不确定导致跨进程 prompt cache 断裂，需确定性序列化（BTreeMap）](domains/agent.md#issue_2026-07-18-tools-hashmap-order-breaks-prompt-cache) — agent

### 空线程

- [issue_2026-07-05-hide-empty-threads-in-history-panel ThreadBrowser 面板应隐藏 message_count 为 0 的空线程](domains/tui/tui-panels.md#issue_2026-07-05-hide-empty-threads-in-history-panel) — tui

### 类型迁移

- [ask_user → interaction 类型迁移——消除 24 个 deprecation 警告](domains/agent.md#issue_2026-07-18-ask-user-migration) — agent

### 终端模式

- [issue_2026-07-05-paste-newline-triggers-submit 输入框粘贴含换行文本时直接触发 Enter 提交](domains/tui/tui-input.md#issue_2026-07-05-paste-newline-triggers-submit) — tui

### 续跑

- [bg agent 完成后主 agent 永久卡死——合成消息未注入 MessageQueue](domains/agent.md#issue_2026-07-07-bg-agent-complete-no-resume) — agent

### 渲染风暴

- [issue_2026-08-02-multi-agent-concurrent-cpu-high 3 agent 并发 50% CPU——每 token 全量重建渲染路径](domains/tui/tui-rendering.md#issue_2026-08-02-multi-agent-concurrent-cpu-high) — tui

### 缓存命中率

- [状态栏上下文消耗显示 + 缓存命中率警告在 ratatui-kit 迁移后丢失](domains/tui/tui-rendering.md#issue_2026-07-13-statusbar-context-cache-display-regression) — tui

### 编辑模式

- [Login 面板缺少 ProviderType 编辑字段——LoginEditField 枚举不完整](domains/tui/tui-panels.md#issue_2026-07-17-login-panel-missing-provider-type-field) — tui

### 自动吸底

- [消息区自动吸底应基于 proximity 判断——距离≤阈值→吸底，距离>阈值→不抢](domains/tui/tui-rendering.md#issue_2026-07-07-message-area-scroll-proximity-follow) — tui

### 行内代码

- [Markdown 行内代码无颜色——Modifier::DIM 哨兵因上游库版本不再设置而失效](domains/tui/tui-rendering.md#issue_2026-07-14-inline-code-no-color) — tui

### 视口跟随

- [textarea 缺少软换行——光标移动需基于视觉行而非逻辑行](domains/tui/tui-input.md#issue_2026-07-09-textarea-no-soft-wrap) — tui

### 软换行

- [textarea 缺少软换行——长行被截断，视口跟随异常](domains/tui/tui-input.md#issue_2026-07-09-textarea-no-soft-wrap) — tui

### 选中项跟随

- [issue_2026-07-06-panels-selection-no-scroll-follow 面板选中项超出可见行后看不到](domains/tui/tui-rendering.md#issue_2026-07-06-panels-selection-no-scroll-follow) — tui

### 配置推送

- [issue_2026-07-16-model-login-switch-not-effective-until-restart 面板配置未推到 agent 需重启生效](domains/tui/tui-panels.md#issue_2026-07-16-model-login-switch-not-effective-until-restart) — tui

### 重复渲染

- [流式输出时文本和工具调用卡片重复——双轨扇出导致同一内容走两个路径](domains/tui/tui-events.md#issue_2026-07-18-duplicate-streaming-text-and-tool-cards) — tui

### 错误提示

- [issue_2026-07-13-config-panel-save-silently-discards-errors 用户 I/O 操作必须有结果反馈](domains/tui/tui-panels.md#issue_2026-07-13-config-panel-save-silently-discards-errors) — tui

### 零产出

- [issue_2026-07-11-cancel-no-rollback-no-restore 零 AI 产出时的回滚判定条件](domains/tui/tui-events.md#issue_2026-07-11-cancel-no-rollback-no-restore) — tui

### 颜色渲染

- [SystemNote 的 Warning/Error 等级颜色未区分——忽略 data.level 字段](domains/tui/tui-rendering.md#issue_2026-07-17-system-note-level-color-not-rendered) — tui

### 鼠标拖拽

- [issue_2026-07-11-message-area-mouse-selection-regression 消息区鼠标拖拽选中因重构回归 + CPU 暴涨](domains/tui/tui-rendering.md#issue_2026-07-11-message-area-mouse-selection-regression) — tui

### bg shell 超时/回调

- [bg shell 缺少超时机制、完成回调未注入 Agent inbox、并发竞态](domains/agent.md#issue_2026-07-28-bg-shell-no-timeout-no-callback-to-agent) — tools

### cancel 丢失前文

- [取消后下一轮 Agent loop 丢失全部前文——不完整 transcript 被写回 ThreadStore](domains/agent.md#issue_2026-07-30-cancel-loses-agent-loop-context) — agent

### compact 死循环

- [consecutive_failures 提前清零导致死机开关失效、无限 Full 重试](domains/agent.md#issue_2026-07-25-compact-consecutive-failures-reset-causes-infinite-loop) — compact

### compact_v2 拆分

- [compact_v2.rs ~900 行拆分为 micro.rs / full.rs / smart.rs](domains/agent.md#issue_2026-07-16-p1-4-compact-v2-split) — agent

### deferred tools 过滤

- [BaseTool::is_direct() 自声明层级替代集中式白名单](domains/agent.md#issue_2026-07-25-deferred-tools-not-filtered-from-llm-tools) — tools

### micro compact 字段级

- [Micro Compact 整体替换导致 Agent 必填参数缺失](domains/agent.md#issue_2026-07-29-micro-compact-loses-agent-tool-context) — compact
- [Micro Compact 字段级压缩设计——Planner/Projection 分离、Unicode 安全截断](domains/agent.md#issue_2026-07-29-micro-compact-field-level-design) — compact
- [issue_2026-08-01-micro-compact-invisible-no-trigger Micro Compact 无触发痕迹——field-level 重写后空 plan 跳过](domains/agent.md#issue_2026-08-01-micro-compact-invisible-no-trigger) — compact

### StageContext 拆分

- [StageContext 22 字段 god object 拆分为 SessionHandle + RuntimeServices + CompactContext + AsyncContext](domains/agent.md#issue_2026-07-16-p1-1-stagecontext-split) — agent

### subagent orphan spans

- [Fork subagent 的 ObservationCreate 缺失导致 152 个 orphan observation 父链断裂](domains/agent.md#issue_2026-07-25-subagent-orphan-spans-dangling-parent-observation) — subagent

### 架构升级清单

- [三维护审视识别 35 个待升级点（4 P0 + 13 P1 + 18 P2）](domains/agent.md#issue_2026-07-16-architecture-upgrade-checklist) — architecture

### caps 协商

- [issue_2026-08-05-caps-negotiated-once-broken-second-session caps 协商值 take() 一次性消费——第 2+ session 门控错乱](domains/agent.md#issue_2026-08-05-caps-negotiated-once-broken-second-session) — acp-protocol

### turn 代际

- [issue_2026-08-05-stale-turn-interrupted-overwrites-new-turn 旧 turn TurnInterrupted 污染新 turn——request_id 配对 + 代际兜底](domains/tui/tui-events.md#issue_2026-08-05-stale-turn-interrupted-overwrites-new-turn) — tui

### 悬挂 span

- [issue_2026-08-05-stage-ended-missing-on-error-path run_stage Err 路径不 emit StageEnded——成对事件全路径对称](domains/agent.md#issue_2026-08-05-stage-ended-missing-on-error-path) — agent

### transcript 落库

- [issue_2026-08-05-transcript-drop-loses-final-messages transcript Drop abort 丢积压——正常退出必须显式 flush](domains/agent.md#issue_2026-08-05-transcript-drop-loses-final-messages) — agent

### 事件身份

- [issue_2026-07-25-event-identity-diverges-across-dual-delivery-paths 同一事件双轨投递身份漂移——收敛单链路+类型契约](domains/agent.md#issue_2026-07-25-event-identity-diverges-across-dual-delivery-paths) — architecture
- [issue_2026-07-25-stale-v2-events-bypass-session-filter 旧会话 v2 事件绕过 session 过滤——空 session_id 守卫缺口](domains/agent.md#issue_2026-07-25-stale-v2-events-bypass-session-filter) — architecture

### 死路径

- [issue_2026-08-05-background-task-completed-event-dead-path BackgroundTaskCompleted 事件无映射——注释声称的 Path A 是死代码](domains/agent.md#issue_2026-08-05-background-task-completed-event-dead-path) — subagent

### 非结构化错误

- [issue_2026-07-22-p1-3-unstructured-error-cleanup #[from] anyhow 吸收一切错误——高频错误提升独立变体](domains/agent.md#issue_2026-07-22-p1-3-unstructured-error-cleanup) — agent

### prompt 分层

- [issue_2026-08-02-prompt-security-runtime-contracts Prompt 安全边界与运行时契约——五层模型+单一事实源](domains/agent.md#issue_2026-08-02-prompt-security-runtime-contracts) — architecture

### 粘性吸底

- [issue_2026-07-14-auto-follow-loses-track-during-streaming 流式跳增误判上滚——follow_bottom 粘性语义替代距离阈值](domains/tui/tui-rendering.md#issue_2026-07-14-auto-follow-loses-track-during-streaming) — tui

### 点击区域

- [issue_2026-08-02-statusbar-model-quick-switch-click-fails 状态栏模型段点击失效——点击区域必须镜像折行算法](domains/tui/tui-rendering.md#issue_2026-08-02-statusbar-model-quick-switch-click-fails) — tui

### 遮挡裁决

- [issue_2026-08-01-tui-mouse-multi-layer-conflict 鼠标多层路由冲突——集中式 MouseRouter 遮挡裁决](domains/tui/tui-rendering.md#issue_2026-08-01-tui-mouse-multi-layer-conflict) — tui

### 错误可见性

- [issue_2026-07-22-llm-api-error-silently-swallowed-in-tui LLM API 报错 TUI 静默无提示——AgentExecutionFailed 事件契约](domains/tui/tui-rendering.md#issue_2026-07-22-llm-api-error-silently-swallowed-in-tui) — tui

### Profile

- [issue_2026-08-01-model-profiles-independent-config Model Profile 独立配置——每档独立持有请求参数，整体替换合并](domains/agent.md#issue_2026-08-01-model-profiles-independent-config) — llm-provider

### peri-model

- [issue_2026-07-31-extract-peri-model-protocol-crate 抽取 peri-model 标准模型协议 crate——协议核心独立于运行时](domains/agent.md#issue_2026-07-31-extract-peri-model-protocol-crate) — architecture

### 硬编码索引

- [issue_2026-08-02-config-panel-alias-fallback-points-to-opus active_alias 空值回退索引漂移——选项回退按名称查找](domains/tui/tui-panels.md#issue_2026-08-02-config-panel-alias-fallback-points-to-opus) — tui

### 测试对齐生产

- [issue_2026-08-02-rewind-popup-hardening-quartet rewind 单测以 AI 消息为目标——测试场景必须对齐生产口径](domains/tui/tui-popups.md#issue_2026-08-02-rewind-popup-hardening-quartet) — tui

### 假绿

- [issue_2026-08-02-rewind-popup-hardening-quartet rewind e2e 前置条件缺失静默通过——Write 未调用/文件缺失仍绿](domains/tui/tui-popups.md#issue_2026-08-02-rewind-popup-hardening-quartet) — tui

### 取消误报

- [issue_2026-08-05-cancel-misreported-as-llm-failure 用户取消被误报为 LLM 失败——match 两分支完全相同](domains/agent.md#issue_2026-08-05-cancel-misreported-as-llm-failure) — agent

### RCRA

- [issue_2026-07-27-rcra-simplify-agent-loop Agent Loop 五阶段 CRRAE 简化四阶段 RCRA——预消费与退出判断冲突](domains/agent.md#issue_2026-07-27-rcra-simplify-agent-loop) — agent

## 更新记录

- 2026-07-06: 首次创建，归档 8 个 issue
- 2026-07-06: 从 spec/issues/ 归档 8 个 issue
- 2026-07-10: 归档 13 个 issue，新增 agent 领域，新增 24 个关键词
- 2026-07-17: 归档 16 个 issue，新增 35 个关键词
- 2026-07-18: 归档 37 个 issue
- 2026-07-30: 归档 15 个 issue，新增 9 个关键词，agent 领域新增 8 条经验
- 2026-08-11: 归档 41 个 issue，新增 19 个关键词，agent 领域新增 29 条经验
- 2026-08-11: 归档 58 个 issue（删除 10 份被取代文档），新增 16 个关键词段，agent 领域新增 23 条经验，TUI 各子域新增 17 条经验
