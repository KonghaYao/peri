# 用户待发送队列

状态：现行设计。范围为用户输入的投递管理与 TUI 待发送区域；聊天区沿用既有消息渲染。
实现入口见 [Agent 索引](../code-index/peri-agent.md)、[ACP 索引](../code-index/peri-acp.md)
和 [TUI 索引](../code-index/peri-tui.md)。

## 状态归属

Agent 会话的 `UserInputMailbox` 是投递生命周期的唯一 owner，宿主持有跨 turn 的共享实例。
它保留待发送内容、稳定输入身份、命令回执及运行 ticket；ACP 定位会话、检查能力和写权限，
执行 Agent 给出的准入决定；TUI 只保存编辑器草稿、未确认请求及服务端投影。

待发区与现有消费 MQ 分开。普通待发内容不参与 MQ 非空、唤醒或后台续跑判断。
运行中普通提交等待当前工作自然完成；空闲提交直接开始，已挂起的输入经同一 owner 唤醒。
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

为接收立即发送内容而中断、收尾和续跑，属于同一工作的延续，不释放普通待发内容。
Agent 完成 transcript flush 与事件 forwarder 收尾，确认自然成功后，才释放其余普通等待项。
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
