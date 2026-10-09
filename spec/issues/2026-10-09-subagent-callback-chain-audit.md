# Subagent 回调与 loop 激活全链路核查

- 状态：当前源码缺口已修复；现场旧进程需退出后重新启动，真实场景验收未完成。
- 范围：当前进程投递、父队列、Receive、宿主 continuation；不改表结构、不恢复旧执行。

## 现场证据

读取 `~/.peri/logs/agent-tui.2026-10-09`，而不把仓库 `.tmp` 日志误作所有会话的现场日志。
日志时间为 UTC，以下时间已换算为 2026-10-09 Asia/Shanghai：

- 10:12:03，`bg-01a11e64-e079-70b2-8aef-accd14a34a12` 完成投递返回
  `terminal publication Pending: owner must retain and retry until queue acceptance`。
- 10:19:38，同一任务才出现 `registry.complete()`；其前紧邻父会话
  `01a11a7e-6599-76a3-b6d9-525c305e51b1` 的用户 `session/input/enqueue`。
  这支持“结果等待后续用户输入才结算”的表现，不证明此任务就是用户截图中的任务。
- 10:22:55，另一个 coder 任务再次走同一旧 Pending 路径。

该 Pending 字符串在当前源码和磁盘上的 `target/debug/peri` 均不存在。
核查 `ps` 与 `lsof`：PID 87648（10 月 8 日 15:50 启动，工作区
`remote-control-server`）与 PID 13359（10 月 8 日 17:29 启动，工作区
`workflow-studio`）仍映射旧 executable inode 192381884 / 197453540；
核查时磁盘文件 inode 为 207311545。新旧进程共享日志，现有日志没有每条记录的 PID，
因此不能把每条 Pending 精确归属某一进程。更换磁盘二进制不会更新已运行的进程。
未自行终止这些进程，以免中断尚在执行的用户任务。

## 当前实现逐段核查

1. 背景 child 的 `background.rs` 在输出/转发收尾后调用 TaskManager 的
   `settle_completed`；投递失败保留原结果，不伪装 completed。
2. `bg_complete.rs` 冻结投递路由、使用稳定 delivery ID 去重，`delivery.rs`
   同步接纳到目标 MQ。Defer 默认是 Required / EnsureProcessing；Info 严重度
   不是被动 MessageKind，也不撤销模型可见性。
3. `AsyncRouter` 和直接 terminal delivery 均能唤醒队列，先前怀疑直接路径
   缺少显式 policy 的假设被构造函数实现和现有测试排除，不为此加入冗余修复。
4. Receive 按 policy 计入 wake_up_count、落历史并发提醒；loop 的退出复查、
   bounded idle、迭代预算边界及 child 的累计预算均有现有测试。
5. 宿主 activation listener 与 dispatch 收尾复查 MQ；scheduler 经过 prompt lock、
   epoch 和队列复查；失败 watermark、Stop、关闭各自限制保持原契约。

## 确定性复现并修复的源码缺口

普通后台 spawn 使用 `completion_delivery(parent, configured)`，但后台 resume
直接传递 configured callback。存在直接父 Session、未设置自定义 callback 时，
任务会正常 completed，但没有父队列通知，父 loop 无法消费结果。

新增 `background_resume_without_custom_callback_wakes_parent` 进入真实
SessionFactory/background owner，等待任务排空后验证父 MQ，再实际运行父 loop，
断言一次模型请求包含一次 child 结果、child 队列为空、parent 队列被消费。
修复前该测试在父 MQ `has_ensure_processing()` 断言确定性失败。
修复使 resume 与 spawn 共用已有 `completion_delivery`，不新增兼容层或规则副本。
生产宿主通常已注入 callback，故不能把这个缺口直接认定为现场 Pending 的根因。

## 验证与剩余验收

- 新回归修复后通过；`session::subagent` 88 项、`agent::stages` 156 项、
  宿主 `activation_tests` 12 项均在修复后通过。
- `session::bg_complete::tests` 7 项通过，覆盖同步接纳、去重、失败保留与重试。
- `peri-middlewares --lib subagent::` 209 项通过，覆盖真实工具入口、resume、
  嵌套直接父路由、session 隔离与模型失败反馈。
- `cargo fmt --all -- --check`、`git diff --check`、全库文件大小检查通过；
  文件大小扫描 2267 个文件，无超限。
- `build --locked -p peri-tui` 完成，最新 `target/debug/peri` 已构建；
  不自动替换运行中进程，不以构建成功代替真实场景验收。
- 真实验收：等待或显式处理当前任务后退出旧进程，重新启动当前构建；
  在主 run 结束后等待后台 child 完成，确认无需新输入即可发起模型处理。
- 如当前构建仍复现，需要提醒正文/task ID 与父 session ID 精确关联日志；
  单独“Task · subagent · Info”标题不足以区分完成通知、父发消息或 replay。
