# 94 个 Peri 业务场景录放语料（v3）

这些场景使用真实 Peri 二进制、ACP 与 PTY。初次录制的模型上游是本地可控服务；cassette 保存实际 HTTP 请求与响应，不代表真实商业模型的回答质量。普通重放仅消费已保存的 cassette，不启动模型上游，不允许未匹配时回源。

## 场景与独立断言

| ID | 场景 | 必须核对的行为 |
| --- | --- | --- |
| S01 | 多轮流式文本与内容块顺序 | 文本顺序、后续请求携带前轮回复、完成次数 |
| S02 | 流式工具参数分片 | 完整参数到达后实际 Write，文件字节正确 |
| S03 | 429、503 后重试成功 | 同逻辑调用 attempt 递增、只展示一次完成 |
| S04 | HTTP 错误后继续输入 | 错误可见、新执行成功、身份不复用 |
| S05 | 断流、取消续推理、再输入 | 真实取消终态、旧后缀不进入新执行 |
| S06 | Responses 工具调用闭环 | 真实工具文件、function_call_output、最终回复 |
| T01 | Write 后 Read | 文件内容与模型实际收到的读取结果一致 |
| T02 | Read 后 Edit | 精确替换、旧内容消失、工具卡完成 |
| T03 | Bash 执行 | stdout 与实际文件副作用 |
| T04 | 缺失文件后的工具修复 | 首次错误、Write 补齐、再次 Read 成功 |
| T05 | MCP 资源读取 | 实际 MCP 内容进入下一次模型请求 |
| T06 | Prompt hook 修改工具输入 | hook 有独立录制调用、文件为修改后的结果 |
| A01 | 同步子 Agent | 子任务创建、工具卡、父任务实际消费结果 |
| A02 | 同名子 Agent 并发反序完成 | 父调用锚点区分子任务、结果和工具卡不串线 |
| A03 | 异步子 Agent 回送 | 后台终态、无需新用户输入的结果回送 |
| A04 | 子 Agent resume | 使用本轮真实子会话身份并保留历史 |
| A05 | 子 Agent 工具错误与恢复 | 错误归属、后续工具结果、父任务可继续 |
| A06 | 父子共享文件 | 控制真实读写因果，检查各次读取和最终文件 |
| I01 | 双会话同输入反序请求/响应 | 同 ACP 后端隔离、各自持久历史与 TUI |
| I02 | 一份录制同时重放两次 | 独立 world、绑定、游标与完成状态 |
| I03 | 一会话取消、另一会话继续 | 目标取消、旧 requestId 的迟到 Stop 不影响新执行 |
| U01 | 进程重启后续聊 | 新进程加载原会话、旧回复进入续聊请求 |
| U02 | 同一 TUI 流式期间切换会话 | 旧流随 session/close 取消、目标会话隔离、切回可续聊 |
| U03 | 忙时连续输入排队 | 用户顺序、每项一次、正确历史前缀 |
| U04 | /clear 后新会话 | 新 owner、旧上下文不泄漏 |
| U05 | 长 Markdown 与终端缩放 | 窄屏草稿保留、提交一次、历史内容完整 |
| U06 | 手动 compact 后续聊 | compact 辅助调用、摘要落盘、下一轮使用摘要 |
| U07 | AskUser 单选 | 选项交给原工具，下一次模型请求包含真实回答 |
| U08 | AskUser 多题多选 | 多选集合与题目顺序保持 |
| U09 | 审批拒绝后明确允许 | 拒绝零副作用、下一轮批准恰好执行一次 |

## v3 新增 64 个场景

fixture 仍使用格式 v2；目录名不是覆盖版本。模型响应来自可控本地 fixture，MCP、Shell、数据库、进程和终端都实际运行。

| ID | 场景 | 用例源码 |
| --- | --- | --- |
| B01 | 父挂起保持 working，异步结果自动处理且持久化一次 | [源码](../../tests/replay-v3/async-agent-lifecycle.test.ts) |
| B02 | 双异步 child 逆序完成，结果归属与 working 终态准确 | [源码](../../tests/replay-v3/async-agent-lifecycle.test.ts) |
| B03 | 取消普通父回合不杀 Independent child，结果一次自动续跑 | [源码](../../tests/replay-v3/async-agent-lifecycle.test.ts) |
| B04 | 真实 Tasks 面板取消 child，工具副作用不发生且取消可见 | [源码](../../tests/replay-v3/async-agent-lifecycle.test.ts) |
| B05 | 异步 child 401 失败真实传回，父继续且无假成功 | [源码](../../tests/replay-v3/async-agent-lifecycle.test.ts) |
| B06 | active child resume 为 Defer 补充消息，同执行且真实工具收到补充 | [源码](../../tests/replay-v3/async-agent-resume.test.ts) |
| B07 | 已完成 child 后台恢复，同线程新执行新 task 且历史可读 | [源码](../../tests/replay-v3/async-agent-resume.test.ts) |
| B08 | 无效 Agent 定义明确失败，无幽灵线程或后台任务 | [源码](../../tests/replay-v3/async-agent-resume.test.ts) |
| B09 | full-async-tools 对 Agent 缺省及显式 false 均强制后台 | [源码](../../tests/replay-v3/async-agent-resume.test.ts) |
| B10 | 真实 120 秒 bounded handoff TurnDone 后 child 回调无需用户新输入 | [源码](../../tests/replay-v3/async-agent-boundary.test.ts) |
| B11 | 停止自动续跑后晚到 child 不复活父回合，显式输入后只消费一次 | [源码](../../tests/replay-v3/async-agent-boundary.test.ts) |
| B12 | 进程重启恢复 child 历史，不恢复旧执行、工具副作用或 working 状态 | [源码](../../tests/replay-v3/async-agent-boundary.test.ts) |
| D01 | 父环境毒化与存储逃逸拒绝，真实 shell 和数据库仍完全隔离 | [源码](../../tests/replay-v3/session-store-isolation.test.ts) |
| D02 | 八轮对话跨四代真实进程，历史精确一次且上下文连续 | [源码](../../tests/replay-v3/session-store-isolation.test.ts) |
| D03 | 同一临时 store 三会话两轮交错冷恢复，界面和模型历史无串扰 | [源码](../../tests/replay-v3/session-store-isolation.test.ts) |
| D04 | 同 store 双进程独立会话并发，关闭一方后另一方仍提交 | [源码](../../tests/replay-v3/session-store-isolation.test.ts) |
| D05 | 跨临时 store 冒用 session ID 精确拒绝，原会话仍可冷恢复续聊 | [源码](../../tests/replay-v3/session-store-isolation.test.ts) |
| D06 | 真实副作用后流中 SIGKILL，working 以新执行续跑且旧工具仅执行一次 | [源码](../../tests/replay-v3/session-store-lifecycle.test.ts) |
| D07 | 同样工作文件进入独立临时 store，真实工具可读但旧会话历史不可见 | [源码](../../tests/replay-v3/session-store-lifecycle.test.ts) |
| D08 | 模型终态错误后重启，失败输入不重发，新输入继续原历史 | [源码](../../tests/replay-v3/session-store-lifecycle.test.ts) |
| D09 | 工具失败历史跨进程保存，工作文件修复后真实重读且不覆盖旧错误 | [源码](../../tests/replay-v3/session-store-lifecycle.test.ts) |
| D10 | 真实 fork 固定历史快照，父与 fork 后续独立迭代不串扰 | [源码](../../tests/replay-v3/session-store-lifecycle.test.ts) |
| D11 | 用户 Stop 跨进程保留 stopped，不自动续跑；新输入才解除停止并完成 | [源码](../../tests/replay-v3/session-store-lifecycle.test.ts) |
| D12 | 外围实例明确关闭时崩溃恢复如实Unknown，仍以新执行续跑 | [源码](../../tests/replay-v3/session-store-lifecycle.test.ts) |
| H01 | 后台 Shell 在父回合结束后完成、唤醒与真实 Read，Shell 不占用 OSC working | [源码](../../tests/replay-v3/async-shell-output.test.ts) |
| H02 | 后台 Shell 非零退出保留 stderr、exit code 与单次副作用 | [源码](../../tests/replay-v3/async-shell-output.test.ts) |
| H03 | 超过 2 MiB Unicode 后台日志完整落盘，通知只带引用并真实读取尾部 | [源码](../../tests/replay-v3/async-shell-output.test.ts) |
| H04 | 前台超时提升为后台，原进程及输出连续且不会启动第二份 | [源码](../../tests/replay-v3/async-shell-output.test.ts) |
| H05 | 真实任务面板取消进程组，同名命令再次启动不受旧终态污染 | [源码](../../tests/replay-v3/async-shell-lifecycle.test.ts) |
| H06 | 同会话并行 Shell 反序完成，任务身份、真实 Read 和通知不串线 | [源码](../../tests/replay-v3/async-shell-lifecycle.test.ts) |
| H07 | 跨会话取消被明确拒绝，原任务均可完成且真实 TUI 历史隔离 | [源码](../../tests/replay-v3/async-shell-sessions.test.ts) |
| H08 | 下一用户回合进行中时 Shell 完成，通知恰好一次合入活动执行 | [源码](../../tests/replay-v3/async-shell-sessions.test.ts) |
| H09 | 后台 Shell 正超时产生真实 timed_out，并阻止超时后的迟写 | [源码](../../tests/replay-v3/async-shell-lifecycle.test.ts) |
| H10 | 关闭会话清理后台进程，重载独立 session store 不复活旧 Shell | [源码](../../tests/replay-v3/async-shell-sessions.test.ts) |
| H11 | full-async-tools 下 Bash 省略参数走后台，显式 false 保持前台 | [源码](../../tests/replay-v3/async-shell-output.test.ts) |
| H12 | ACP 取消保留 cancelled 快照、单次取消事件与真实输出证据 | [源码](../../tests/replay-v3/async-shell-cancel.test.ts) |
| J01 | 异步结果跨 Stop 与两次 Full Compact，响应提交前保护、提交后归档 | [源码](../../tests/replay-v3/cross-lifecycle.test.ts) |
| J02 | 父请求失败后新的后台错误自动唤醒，旧请求不自旋重试 | [源码](../../tests/replay-v3/cross-lifecycle.test.ts) |
| L01 | 32 轮真实对话重启后跨 50 条窗口分页，历史顺序与草稿保留 | [源码](../../tests/replay-v3/tui-context-history.test.ts) |
| L02 | 流式期间浏览旧历史，新输出提示与回到底部及输入草稿正确 | [源码](../../tests/replay-v3/tui-context-interactions.test.ts) |
| L03 | 三代 compact 与每代独立进程重启，当前上下文和完整显示历史分离 | [源码](../../tests/replay-v3/tui-context-history.test.ts) |
| L04 | 250 行真实 Read 截断提示与折叠展开，标准屏 4 行及窄屏 2 行摘要边界 | [源码](../../tests/replay-v3/tui-context-panels.test.ts) |
| L05 | 多行 Unicode 括号粘贴、窄宽屏切换与单次提交的真实编辑器链路 | [源码](../../tests/replay-v3/tui-context-interactions.test.ts) |
| L06 | 百行代码与表格真实渲染，窄屏滚动及宽屏回流保留首尾和完整模型历史 | [源码](../../tests/replay-v3/tui-context-interactions.test.ts) |
| L07 | 状态栏模型弹窗键盘焦点、窄屏布局和关闭后草稿恢复，无误提交或误配置写入 | [源码](../../tests/replay-v3/tui-context-panels.test.ts) |
| L08 | AskUser 拒绝确认可取消且保留选项，再确认拒绝与下一题状态隔离 | [源码](../../tests/replay-v3/tui-context-panels.test.ts) |
| L09 | 真实鼠标拖选与 OSC52 剪贴板精确文本，缩放后坐标重算且无误提交 | [源码](../../tests/replay-v3/tui-context-panels.test.ts) |
| L10 | 流式取消保留未提交草稿，下一次执行不含未接收尾部且身份隔离 | [源码](../../tests/replay-v3/tui-context-interactions.test.ts) |
| L11 | 超过 512 KiB 中文历史详情按协商大小分块加载，OSC52 导出与 SQLite 原文一致 | [源码](../../tests/replay-v3/tui-context-history.test.ts) |
| L12 | 两代真实 Read 工具往返经 compact 与重启后，工具结果归档和摘要上下文正确 | [源码](../../tests/replay-v3/tui-context-history.test.ts) |
| L13 | 过程区过渡线后的真实工具标题鼠标命中，展开与折叠内容无额外执行 | [源码](../../tests/replay-v3/tui-context-panels.test.ts) |
| L14 | 生产默认阈值下 usage 压力自动 Full，同执行继续并冷重启，原文归档可追溯 | [源码](../../tests/replay-v3/tui-context-auto-compact.test.ts) |
| M01 | monitor 运行中增量告警、真实 Tasks 面板与终态合并未报告命中 | [源码](../../tests/replay-v3/monitor-lifecycle.test.ts) |
| M02 | monitor 真实 stdout 与 EOF 末行保留，终态读完后空页稳定 | [源码](../../tests/replay-v3/monitor-buffer.test.ts) |
| M03 | monitor 终态继续分页，exclusive cursor 单调且事件无遗漏无重复 | [源码](../../tests/replay-v3/monitor-buffer.test.ts) |
| M04 | monitor ring 溢出精确报告 7 个丢弃事件，并恢复全部剩余序号 | [源码](../../tests/replay-v3/monitor-buffer.test.ts) |
| M05 | monitor 超长 Unicode 行在 UTF-8 边界截断，小读预算仍推进完整事件 | [源码](../../tests/replay-v3/monitor-buffer.test.ts) |
| M06 | monitor_stop 真正退出进程树，重复停止不改变终态或追加迟到事件 | [源码](../../tests/replay-v3/monitor-lifecycle.test.ts) |
| M07 | monitor deadline 杀掉真实进程，保留已捕获事件并失败唤醒原会话 | [源码](../../tests/replay-v3/monitor-lifecycle.test.ts) |
| M08 | monitor 非法 regex 启动失败无进程副作用，同会话随后恢复成功 | [源码](../../tests/replay-v3/monitor-lifecycle.test.ts) |
| M09 | monitor 真实 30 秒合并窗口后再次唤醒，终态不重复已报告命中 | [源码](../../tests/replay-v3/monitor-lifecycle.test.ts) |
| M10 | 同一真实 monitor MCP 实例跨 owner 的 read/stop/list/get/cancel 隔离 | [源码](../../tests/replay-v3/monitor-ownership.test.ts) |
| M11 | monitor 实例关闭清理进程树，重启后的旧 task ID 不复活且新任务正常 | [源码](../../tests/replay-v3/monitor-ownership.test.ts) |
| M12 | monitor 非零退出传播失败状态和 stderr，失败终态仍可读全部事件 | [源码](../../tests/replay-v3/monitor-buffer.test.ts) |

## 证据与范围

- 每个场景有独立 seed、cassette 和场景清单；初始化通过真实 ACP 创建新会话，重放使用新的临时目录。不会修改数据库表结构或直接伪造历史行。
- 原始录制不可变。环境路径、真实 session/child 身份等动态值只能在已声明的位置按有证据的绑定转换，其余请求内容仍严格匹配。
- 屏幕断言不能命中用户 prompt 回显来冒充模型输出。涉及工具、排队、取消和持久化时必须有对应的状态、文件或请求证据。
- 每个重放 run 必须精确消费预期调用；多余、缺失、身份冲突和未结束调用都使场景失败。协议单测不计入此表的 94 个业务场景。
- 此表定义本录放套件范围。仓库完整 TUI 手工验收目录包含更多场景，本套件不宣称覆盖其中全部条目，也不替代真实模型质量或跨平台验收。

执行结果与耗时以生成的独立 record/replay 报告为准，目录清单本身不代表通过。

## 快速与完整门禁

`npm run e2e:replay` 包含全部场景；`npm run e2e:replay:quick` 只跳过明确带 `[slow]` 的真实生产计时边界（B10 父任务等待、M09 告警窗口）。慢例不能通过缩短生产常量代替。每次报告注明具体执行/跳过数，快速门禁不称为全量通过。

所有 case 的 HOME、XDG、TMPDIR 与显式 `--db-path` 均位于独立临时 world；同 case 明确声明的多会话可以共享该 store。正常结束必须 `/exit` 且 Peri/PTY 桥均 exit 0；只有 D06 等显式崩溃步骤可 SIGKILL。报告保存进程终态、数据库 quick_check、会话状态与消息摘要；通过后清理临时 HOME/work。失败保留诊断，并报告清理错误。

长上下文验收包含 32 轮、跨 50 条分页、三代 compact/四次进程启动、超过 512 KiB 中文正文分块与全字节导出；这些是已声明边界，不等于无限历史或任意模型窗口均有保证。

L14 自动 Full 使用受控 provider usage（196000 / 默认 200000 窗口）触发生产阈值，不能称为实际 196k token 输入；L11 独立验证实际 660171 字节历史正文及全字节导出。
