# 用户待发送队列

状态：现行设计。范围为用户输入的投递管理与 TUI 待发送区域；聊天区沿用既有消息渲染。
实现入口见 [Agent 索引](../code-index/peri-agent.md)、[ACP 索引](../code-index/peri-acp.md)
和 [TUI 索引](../code-index/peri-tui.md)。

## 状态归属

Agent 会话的 `UserInputMailbox` 是投递生命周期的唯一 owner，宿主持有跨 turn 的共享实例。
它保留待发送内容、稳定输入身份、命令回执及运行 ticket；ACP 定位会话、检查能力和写权限，
执行 Agent 给出的准入决定；TUI 只保存编辑器草稿、未确认请求及服务端投影。

待发区与现有消费 MQ 分开。loading 期间普通输入只进入待发区，不打断当前模型或工具执行。
主 Agent 进入 idle 时，Mailbox 按 FIFO 取第一条 Queued 输入交给 MQ，跳过已撤回或已投递项。
等待后台任务而 idle 时，复用当前 attempt 唤醒 loop，不必等待后台任务全部完成；
当前 turn 自然成功结束时，由宿主按同一规则启动下一条。后续普通输入等待下一次 idle。
空闲提交同样遵循队首顺序；一次交接占用本次 idle，即使 Receive 尚未开始，也不会追加第二条。
单条与全部立即发送使用同一个指定 ID 集合的操作，只提前处理选中内容。

```text
Queued → Dispatching → Claimed → Delivered
   └──→ Withdrawn
Dispatching → Queued：仅在已从 MQ 撤出且确认未领取时
```

Receive 与 Stop 在同一 MQ 锁下裁决领取/撤出，随后回报 Mailbox。写入 canonical transcript
才算 Delivered；仅入队或交接给 MQ 不能当作用户消息出现在聊天区。

## 交互与投递

- 标题为“待发送”（英文 Pending）；空队列完全隐藏，一条一行，无左侧序号；默认最多展示 5 条，可展开其余条目。
- 空闲且无其他待发输入时，本地直接提交不展示待发送行，收到 Delivered 后直接显示原聊天气泡；首次会话绑定保持此展示。服务端确认 Queued、回执失败或会话重载时恢复可见投影，未确认请求与原稿仍按既有规则保留。该展示区分不改变服务端准入与聊天确认边界。
- 队列透明背景，仅使用输入框既有主题上边线；统一字符 `↑` 单发、`⇈` 全发、`↶` 取回，
  ASCII 环境使用相应降级符号，不使用 emoji。
- 单发 B 允许越过 A；随后再发 A 时按 B、A 接收，其余等待项保持相对顺序。
- 全发携带点击时已确认的 ID 快照，包含折叠项；后来加入的内容不进入该批。
- 取回只允许 Queued，成功回执返回完整正文和附件。输入框非空时也可撤回，只移除队列项，
  保留当前草稿；仅点击时与实际恢复时输入框都为空且无附件，才恢复原稿。等待期间产生新稿
  时不覆盖，也不在之后清空输入框时补恢复。
- 容量满时拒绝新输入并保留草稿，不挤出旧消息。同一 command ID 重试复用原输入身份与选择集合。

首次 Receive 将本批用户消息 ID 交给输入准备 hook；图片与文件引用按这些 ID 逐条处理，
保留已有附件和消息身份，不重新读取历史消息的引用。

为接收立即发送内容而中断和收尾期间，不释放普通待发内容；用户已选中的发送集合优先执行。
选中内容执行到 idle 后，普通待发内容恢复逐条调度。跨 turn 的自动启动仍须等待
transcript flush 与事件 forwarder 收尾，确认自然成功；同一 attempt 的 idle 唤醒经既有 inbox 进行。
用户 Stop 取消尚未开始的 ticket，保留待办，并只回收明确未被 Receive 领取的内容。
执行失败不视作自然完成；持久化状态不确定时冻结当前 generation，要求重新加载。
用户再次提交、显式继续或立即发送可以恢复停止后的处理。

## ACP 与事件

`peri.userInputQueue` 双向协商后，使用四个短控制请求：

| 方法 | 意图 |
| --- | --- |
| `session/input/enqueue` | 保存完整内容和原始草稿，以稳定 input ID 入队 |
| `session/input/dispatch` | 发送指定 input ID 集合 |
| `session/input/takeback` | 原子撤回一条 Queued 输入并返回完整载荷 |
| `session/input/snapshot` | 查询当前 generation、revision、待发项与实际运行身份 |

变更请求绑定 session ID、generation 与 command ID。回执和事件按 revision 合并，
迟到响应不能覆盖新状态；响应不明确时只在相同会话实例中以同一身份重试，不能改走旧 prompt 重复投递。
首次输入的准备阶段（工作区发现、服务端建会话、初次快照）与已发出请求的受理回执各自计时：
准备慢不按回执超时收尾，准备失败发生在发送之前，按确定未受理恢复完整原稿；准备期限保持有界，
不把输入留在无法撤回的状态。准备期间状态栏给出明确状态（`SESSION_PREPARING`，文案
「正在准备会话」）：这段窗口里输入既不在待发送队列、也还没有发出请求，只能由状态栏说明正在
做什么；建立结束（成功、失败、超时或 future 被丢弃）即清除，不把提示留在可用会话上。
实例变化后保留未知输入投影，结合历史核对，不能将旧请求自动提交到新实例或假定此前未发送。
未协商能力的客户端继续使用旧提交路径；已注册 slash 命令继续进入原命令路由。
完整 DTO 以 `peri-acp-types::session` 为事实源。

队列快照、运行开始及投递结果均由 Agent 发 canonical v2 事件，经 Controller、ACP 映射与
能力门控到达 TUI，不进入公开 activity 摘要。Delivered 与随后 assistant 输出处于本轮同一
render FIFO，稳定 input ID 用于 live/replay 去重，复用原 `TuiUserBubble`。

运行开始的服务端 request ID 与 done 配对，客户端据此建立权限确认和 AskUser 的运行 owner。
新建/加载会话在同一个 operation gate 内绑定队列 generation 并恢复实际运行身份，之后才
接纳新交互。带身份的 Stop 不能取消后来的执行。

队列首版只保存在当前进程内；切换会话查询原实例，进程重启或会话销毁不承诺保留待发送内容。

交互回归见[正式 TUI 测试](../../e2e/tests/smoke/steer-queue-live.test.ts)，使用临时会话配置与本地模型服务；运行方式见 [E2E 指南](../../e2e/CLAUDE.md)。
