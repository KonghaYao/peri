# 30 个 Peri 业务场景录放语料

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

## world 快照与同步

cassette `formatVersion` 为 2 时，请求正文不再各存一份完整 system prompt。共享快照位于 `world/`：

- `world/system/skills.txt` 对应 `peri-acp/prompts/sections/13_skills.md`，稳定 leaf 为 `sys.skills`；
- `world/system/compact.summary.txt` 对应 compact summary system prompt；
- `world/tools/Agent.*.json` 保存 Agent tool schema（description 与 `send_only`）；
- `world/system/block0.main.txt`、`world/system/block1.*.prefix.txt` / `*.tail.txt`、`world/tools/*.json` 由录制内容按字节拆分；
- 其余无法映射的 system 段（如 T06 hook prompt）走内容寻址 leaf，`world:sync` 会列出这些未映射 leaf。

请求 stub 只保留 messages/参数等 per-request 字节与 leaf/recipe 引用；`rawSha256`、`canonicalSha256` 与 response 哈希仍逐条校验，`requestRules` 的 typed-template pattern 在加载时按录制边界重新展开。world leaf 变化后，stub 中的录制哈希会以 `report.worldDrift` 报告，加载器改用当前 world 重建 canonical；篡改仍由 cassette seal 与 world seal 拦截。

高频 prompt 变更流程：

1. `npm --prefix e2e run e2e:replay:world-check` 查看是否有 stale leaf。
2. `npm --prefix e2e run e2e:replay:world-sync` 写入映射 leaf 与 `world/seal.json`。
3. 审核 `git diff --stat`：`13_skills.md` 变化应只出现 2 个文件（skills leaf + seal）；Agent description/`send_only` 为 2 个 Agent leaf + seal；compact summary 为 1 个 leaf + seal。
4. `npm --prefix e2e run e2e:replay:verify` 重新校验 30 cassettes / world seal。

限制：单条消息（如 U06 compact 用户指令）变化仍落在对应 stub + cassette seal（2 文件）；`summary_user_prompt.md` 不在 world 映射内，变化需要重录或显式编辑 stub。`world:sync` 只同步上述源文件，未映射的 prompt 内容变化仍需重录。

## 证据与范围

- 每个场景有独立 seed、cassette 和场景清单；初始化通过真实 ACP 创建新会话，重放使用新的临时目录。不会修改数据库表结构或直接伪造历史行。
- 原始录制不可变。环境路径、真实 session/child 身份等动态值只能在已声明的位置按有证据的绑定转换，其余请求内容仍严格匹配。
- 屏幕断言不能命中用户 prompt 回显来冒充模型输出。涉及工具、排队、取消和持久化时必须有对应的状态、文件或请求证据。
- 每个重放 run 必须精确消费预期调用；多余、缺失、身份冲突和未结束调用都使场景失败。协议单测不计入此表的 30 个业务场景。
- 此表定义本录放套件范围。仓库完整 TUI 手工验收目录包含更多场景，本套件不宣称覆盖其中全部条目，也不替代真实模型质量或跨平台验收。

执行结果与耗时以生成的独立 record/replay 报告为准，目录清单本身不代表通过。
