# 用户待发送队列

状态：现行设计。范围为用户输入的投递管理与 TUI 待发送区域；聊天区沿用既有消息渲染。
实现入口见 [Agent 索引](../code-index/peri-agent.md)、[ACP 索引](../code-index/peri-acp.md)
和 [TUI 索引](../code-index/peri-tui.md)。

## 状态归属

待发送消息及其原稿由 [Thread MQ](thread-mq.md) 持久保存在所属 thread 的 MQ 中；MQ 是待发事实的唯一持久权威。
发布/领取选择、当前 run ticket 和执行恢复仍只属于当前进程。不保存持久 Work 命令或回放旧发送命令；重启后
尚未移交的消息仍待发送，但旧执行不恢复。已提交的 canonical 历史照常持久化。当前发送/取回/取消的
身份校验与交互行为保留。

Agent 会话的 `UserInputMailbox` 是投递生命周期的唯一 owner，宿主持有跨 turn 的共享实例。
它保留稳定输入身份、命令回执、运行 ticket，以及一份**内存投影缓存**（正文、原稿与展示状态）；
缓存不是第二份持久待发账本。`snapshot()` 只读这份投影，不查询 MQ、不清理记录、不推进 revision；
`refresh_from_store()` 是显式对账入口：查询本 thread 的用户待发 MQ，成功且 revision 窗口未变化时
补齐缺失记录并清理已确定不在 MQ 的 Queued 记录，查询失败或窗口内 revision 变化时不修改已有投影。
`session/input/snapshot` RPC 先 refresh 再取纯快照，查询失败返回 RPC 错误而不是空队列；
错误响应只附纯快照，不产生查询、清理或 revision 副作用。
ACP 定位会话、检查能力和写权限，执行 Agent 给出的准入决定；TUI 只保存编辑器草稿、未确认请求及服务端投影。

TUI 默认声明 `peri.userInputQueue.toolBoundary`。用户在活跃执行中按 Enter（或点击发送）后，
TUI 先入队、收到受理回执后立即提交发送选择：Enter 与 ↑ 共用同一条发送路径，输入身份、
generation 与回执核对规则不变。发送选择按 loop 所处阶段生效：模型流式期间打断当前 turn；
工具批次执行中只放行 MQ，当前工具批次不取消，由整批完成后的下一次 Receive 在同一 attempt
内摄取，按 MQ 顺序写入历史并进入下一次实际模型请求。未立即发送的剩余队列项仍按
`toolBoundary` 能力在工具边界自动抽取。若当前执行先自然结束，输入沿用 idle 调度。
未协商该能力的客户端、重启恢复的草稿与 Stop 回收的输入遵循原有排队规则。
主 Agent 进入 idle 时，Mailbox 按 FIFO 选择队首一条 Queued 输入并提交发送选择，跳过已撤回或已投递项。
等待后台任务而 idle 时，复用当前 attempt 唤醒 loop，不必等待后台任务全部完成；
当前 turn 自然成功结束时，由宿主按同一规则启动下一条。后续普通输入等待下一次 idle。
空闲提交同样遵循队首顺序；一次交接占用本次 idle，即使 Receive 尚未开始，也不会追加第二条。
单条与全部立即发送使用同一个指定 ID 集合的操作，只提前处理选中内容；Enter 的自动发送选择
复用这一操作，且同一 `input_id` 只自动发送一次，失败保留待发送项而不重新入队。

立即发送按 loop 所处阶段投递：只有实际 LLM 调用/流式期间、且该输入尚未被 Receive 摄取时，
才设置 steer 并取消当前 turn；已摄取（Claimed/Delivered）的输入已经进入当次请求上下文，
取消只会让同一请求重跑一遍，因此不再取消。Act 工具批次执行中、Compact、idle 等待与收尾阶段
不取消执行，只放行 MQ。工具批次放行的输入由整批完成后的下一次 Receive 在同一 attempt 内摄取，
并进入下一次实际模型请求。若前面有超过一个 MQ 事务批次的后台消息，Receive 按 FIFO 分批摄取，
直到选中的输入进入上下文。若发送在工具后的 Receive 选取之后、下一次请求发出之前完成接纳，
Reason 放弃旧请求快照，用同一 attempt 和语义迭代重新进入 Receive。Stop 不受阶段门限制，
任何阶段都立即硬取消；摄取前的输入回到 Queued，需要重新发送。Stop 收尾中的重新发送仍是显式
继续：令牌已由 Stop 取消，不重复取消，但记录 steer 让宿主保留续跑激活。记录复位与发送选择
撤回在同一临界区完成：复位之后才提交的接纳不会被撤回；复位前已经接纳的记录回落到 Queued、
不写 `handed_off`，并撤回其选择，必须重新发送。

入队保存完整内容与原始草稿，不创建额外持久账本。显式发送提交选中的完整集合，由下一次
Receive 原子写入历史；后到草稿不能加入，自动 idle 只选择队首一条。变更请求绑定当前 lifecycle、
revision、generation 与 attempt；回执不明确时保留原命令与 input ID，不能换身份重建。

```text
Queued → Dispatching → Claimed → Delivered
   └──→ Withdrawn
Dispatching → Queued：摄取前执行失败、Stop，或确认未领取后撤回其发送选择（MQ 行保留，与发送选择撤回同一临界区）
```

Receive 在 MQ 事务中领取；Stop 撤回未领取输入的发送选择，正文仍留待发送。显式取消待发项才删除 MQ 行。展示状态只投影已确认结果。
写入 canonical transcript 才算 Delivered；仅入队或提交发送选择不能当作用户消息出现在聊天区。

## 交互与投递

- 标题为“待发送”（英文 Pending）；空队列完全隐藏，一条一行，无左侧序号；默认最多展示 5 条，可展开其余条目。
- 空闲且无其他待发输入时，本地直接提交不展示待发送行，输入框顶边立即显示“正在提交…”；收到 Delivered 后显示原聊天气泡并结束提交反馈，拒绝、未知回执或会话边界变化按实际状态恢复反馈与原稿。致命执行失败时，尚未进入发送分支（未绑定 generation）的输入按确定未发出恢复完整原稿；已绑定 generation 的输入结果未知，保留完整正文、附件与稳定 `input_id`，以“未确认”行继续可见，等待服务端终态或快照对账，不自动重发、不换身份。首次会话绑定保持此展示。服务端确认 Queued、回执失败或会话重载时恢复可见投影，未确认请求与原稿仍按既有规则保留。Submitting 仅是本地投递反馈，不代表正式交付；该展示区分不改变服务端准入与聊天确认边界。
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

流式 steer 触发的中断和收尾期间，不释放普通待发内容；用户已选中的发送集合优先执行。
Act 工具批次、Compact、idle 与收尾阶段的立即发送只放行 MQ，由下一次 Receive 摄取，
不取消当前执行。选中内容执行到 idle 后，普通待发内容恢复逐条调度。跨 turn 的自动启动仍须等待
transcript flush 与事件 forwarder 收尾，确认自然成功；同一 attempt 的 idle 唤醒经既有 inbox 进行。
用户 Stop 取消尚未开始的 ticket，保留待办，并只回收明确未被 Receive 领取的内容。
执行失败不视作自然完成；当前消息写入失败显式报错，不以旧 Work 恢复状态冻结新输入。
用户再次提交、显式继续或立即发送可以恢复停止后的处理：恢复由再次提交的那条输入带动，
它在空闲时按新任务发布并自动解除暂停；暂停之前入队的待办不因此自动发布，仍按逐条调度交接。

## ACP 与事件

`peri.userInputQueue` 双向协商后，使用四个短控制请求：

| 方法 | 意图 |
| --- | --- |
| `session/input/enqueue` | 保存完整内容和原始草稿，以稳定 input ID 入队 |
| `session/input/dispatch` | 发送指定 input ID 集合 |
| `session/input/takeback` | 原子撤回一条 Queued 输入并返回完整载荷 |
| `session/input/snapshot` | 先按 MQ 显式对账，再查询当前 generation、revision、待发项与实际运行身份 |

变更请求绑定 session ID、generation 与 command ID。回执和事件按 revision 合并，
迟到响应不能覆盖新状态；响应不明确时只在相同会话实例中以同一身份重试，不能改走旧 prompt 重复投递。
服务端在同一线性化点生成 revision 与投影内容：同 revision 只在内容完全一致时幂等接受，
内容不同或严格更旧一律拒绝，避免乱序旧快照覆盖新状态。
consumer 发现命令已不在本地待结算账本（被致命失败结算、会话重置或实例 generation 变化）时，
只能报告“已结算”：不得当作发送成功，也不得重试或改换 input ID。
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
render FIFO，稳定 input ID 用于 live/replay 去重，复用原 `TuiUserBubble`。事件泵在推送
任何后续事件前，按 mailbox 的进程内交付日志以 Receive 实际顺序补发 `UserInputDelivered`；
交付日志条目在 `UserInputDelivered` 发布时绑定补打的 `session_seq`，未绑定序号的条目绝不提前补发。
泵只允许在序号严格更小的当前事件之前补发已绑定交付，其余留到 turn 尾部：交付事件经
可靠 render FIFO 进入渲染链路（通道满时等待容量、不降级丢弃）；若发布失败或发布后的
路径丢失，气泡仍会按交付日志在 turn 尾部补发，不会丢失。交付槽位在 enqueue 准入时预留，
容量不足在消费/Claimed 之前拒绝，不会在消息写入 transcript
后才失败。sink 未确认写出前不 ack，失败保留日志并有界重试；重试耗尽显式失败，不以普通
done 收尾。TUI 不从终态快照合成气泡，终态只清除提交反馈。

投递语义是 **at-least-once**：sink 返回错误不证明客户端未收到；未 ack 的 Delivered 可能重复
投递，消费端必须按稳定 input ID 幂等去重。客户端事件重试有界；耗尽后发送
`AgentExecutionFailed`（该通知本身复用同一有界重试）并跳过正常 done。若连失败通知也不可达，
TUI 依赖通知通道关闭路径收敛（`ACP_STATE.is_loading=false`、清空排队输入、显示断连提示），
执行泵仍会发送 `pump_done` 释放 prompt；该恢复不依赖失败事件被送达，前提是 transport 关闭
能够被 notifier 观察到。

运行开始的服务端 request ID 与 done 配对，客户端据此建立权限确认和 AskUser 的运行 owner。
新建/加载会话在同一个 operation gate 内绑定队列 generation 并恢复实际运行身份，之后才
接纳新交互。带身份的 Stop 不能取消后来的执行。

已持久接纳而未移交的待发正文、原稿和插件选择，在支持持久 MQ 的后端上可于重启后按新 generation 从 MQ 对账。旧发送选择、命令回执与运行 ticket 不恢复，不自动重放旧发送。仅内存后端不保证跨进程保留；已经显式删除的项不会恢复。

交互回归见 `KonghaYao/peri-e2e` 的 `tests/smoke/steer-queue-live.test.ts`，使用临时会话配置与本地模型服务；运行方式见 [E2E 指南](../standards/testing.md)。
