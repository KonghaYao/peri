# [P1] Print 模式在后台 Bash 大输出完成后无法结束

**状态**：Fixed（生产修复与自动化验收完成，待原现场反馈）
**优先级**：P1（用户定级；影响范围有限）
**创建日期**：2026-09-18

## 问题描述

`peri -p --output-format stream-json` 执行的前台 Bash 超时后被提升为后台任务。
该任务完成时，如果结果构造出的 reminder body 超过 64 KiB，后台回调会 panic，
任务注册表却仍保留 Running。主 Agent 可以继续执行其他工作，直到最终无工具回答后
进入等待后台任务的分支，无法返回 prompt 结果，也不输出 `result`。

附件中的修复建议按调查材料处理；生产修复在用户明确授权后实施。

本次修复范围限定为后台大输出通知与完成生命周期。已确认的触发组合是
前台 Bash 超时提升到后台、通知 body 超限、主 Agent 随后准备结束；不据此认定
所有 print 会话都会受影响。`--max-turns` 按用户要求不纳入本 issue 的实施或验收。

## 现场与结论边界

来源：用户提供的桌面文件 `bug-peri-3.16.5-print-mode-hang-2026-09-18.md`。
现场版本为 3.16.5，session 为 `01a0b225-79d8-7632-b7b6-d895b04e04e3`。

已在当前工作树的真实 CLI 复现同类停滞；同时核对本地标签 `agent-v3.16.5`
（commit `f6d99bc9`）包含相同因果链。当前 shell 相比标签有前台 child 回收改动，
但输出阈值、reminder 校验、callback/complete 顺序及 Receive 等待判断均仍相同。
首次复现环境是 macOS；修复验收另使用 Linux ARM64 Docker。本地 HTTP fixture 替代 provider，未运行原事故容器或真实模型。

原现场最吻合的证据：

- 01:39 的 `route_bg_result` 记录 `success=false, output_len=100130`，已超过 reminder 上限。
- 随后缺少同版本代码正常路径应输出的 `registry.complete() called`。
- 主 Agent 仍执行至 01:51，最后保留 `text` 和 `assistant.usage`，没有 `result`。
- 无存活 shell 子进程与任务注册表残留 Running 并不矛盾：进程已经回收，逻辑终态未提交。

这些证据高度支持下述机制；原附件未提供触发 Bash 的完整参数与 stderr，
因此尚未独立确认原现场使用的正是 timeout promotion 分支。原 stderr 若保留，
可核对 `peri-agent reminder contract must be valid: BodyTooLarge`。

## 已验证的故障链

1. `peri-agent/src/agent/async_tasks/shell.rs::finalize_bg_shell`
   把输出截到 100,000 字节，再追加落盘提示；其结果依然可能超过 64 KiB。
2. `peri-agent/src/session/async_router.rs::route_bg_result`
   将 `result.to_notification()` 整体作为 reminder body。
3. `peri-acp-types/src/system_reminder.rs::SystemReminder::validate`
   对超过 65,536 字节的 body 返回 `BodyTooLarge`。
4. `peri-agent/src/session/producer_reminders.rs::trusted_reminder`
   对此结果调用 `expect`，导致后台 Tokio task panic。
5. `finalize_bg_shell` 在 `registry.complete` 之前调用该 callback；panic 跳过终态提交。
   `peri-middlewares/src/middleware/terminal.rs` 的前台超时 promotion 路径没有
   显式后台 `spawn_shell` 路径的 panic 收尾保护。
6. `peri-agent/src/session/exec/stage_builder.rs` 用 `TaskManager.active_count() > 0`
   决定是否等待；`peri-agent/src/agent/stages/mod.rs::run_react_loop` 在最后纯回答后
   进入 `inbox.await_wake()`，此时没有可正常完成该任务的 producer。

这是后台任务终态丢失导致的无限异步等待，现有证据不支持把它定位为 ThreadStore 互斥锁死锁。

## 复现与验证结果

真实 CLI 使用临时 HOME、数据库与工作目录，stdin 为 `/dev/null`，仅替换 provider。
HTTP fixture 的第一轮发起 Bash，后续轮返回普通文字 `Task finished.`。
两组唯一业务差异是 Bash 输出大小：

```json
{
  "command": "printf '%*s' 100000 '' | tr ' ' x; sleep 0.2; exit 1",
  "timeout": 10
}
```

`timeout` 单位为毫秒；`sleep` 确保触发前台超时提升。小输出对照改为 `1000`。
不要替换为 `run_in_background: true`：显式后台路径具有不同的 panic 收尾处理。

| 检查 | 实际结果 |
| --- | --- |
| `cargo test -p peri-tui --test print_exit -- --nocapture` | exit 0；6 passed、0 failed，含标准模式、provider 错误、多步 Read 与最终 usage |
| 普通结束正文为“请告诉我正确的姓名和邮箱。” | exit 0，存在 `result` |
| timeout promotion + 1,000 字节输出 | exit 0，约 6.49 秒，`registry.complete` 和 `result` 均存在 |
| timeout promotion + 100,000 字节输出 | 15 秒测试超时后被终止，stderr 出现 `BodyTooLarge`，无 `registry.complete`、无 `result` |

大输出复现最后两条 stdout：

```json
{"type":"text","content":"Task finished."}
{"type":"assistant","message":{"id":"msg_peri_2","usage":{"input_tokens":100,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":7}}}
```

stderr：

```text
panicked at peri-agent/src/session/producer_reminders.rs:38:10:
peri-agent reminder contract must be valid: BodyTooLarge
```

本机复现脚本和原始证据暂存于 `/tmp/peri-print-hang-audit-20260918/`：
`probe.py`、`probe-bg.py`，以及各场景子目录下的 `stdout.jsonl`、`stderr.txt`、
`requests.json`、`result.json` 和 `.peri/logs/`。目录是临时证据，可能被系统清理；
上文已保留触发条件、关键日志和结果。

fixture 请求数包含会话标题生成，不能直接当作主模型轮数。
普通提问场景是 1 次主调用 + 1 次标题调用。
现有 print_exit 测试没有覆盖后台大输出与 callback panic，因此它们通过不反驳本缺陷。

## 对原报告的修正

- Full compact 和末轮提问都不是该机制的必要条件；本地复现没有触发 compact，末句也不是提问。
- `ep_poll` 表示等待事件，本身不能排除网络等待；`futex` 也可能是空闲 worker 的正常等待。
- execution-lock 是会话执行期间正常持有的所有权凭证，获取使用非阻塞 `try_lock`；
  仅凭该 fd 存在无法推断死锁。
- `cli_print.rs` 先输出 `result`，之后才调用 `session/close` 和 deployment shutdown。
  本案无 `result` 且无 canonical transcript adoption 日志，定位应先覆盖 prompt 内部等待。
- 附件中的 `route_bg_result: calling push_defer` 日志位于构造 reminder 之前，
  它不能证明 push_defer 已成功完成。

## 已确认的修复方案

### 1. 完整输出落盘，reminder 只通知状态和读取入口（用户确认方向）

后台 Bash 的输出统一落盘，system reminder 只包含任务 ID、完成/失败/超时状态、
已知退出码、输出文件绝对路径，以及明确的读取提示。默认不附输出正文，
body、summary、metadata 都不重新塞入大段结果。模型根据当前任务需要，通过
Read 工具分段读取文件；不要求每次收到通知都把文件全文读回上下文。

示例（路径仅作格式示意）：

```text
后台任务 shell-<id> 已结束，执行失败，退出码 1。
输出文件：/absolute/path/to/output.log
完整输出已保存到文件系统。需要检查结果时，请使用 Read 工具读取该文件；大文件按需分段读取。
```

实施位置与约束：

- 输出采集侧负责生成文件及结构化引用；`AsyncRouter::route_bg_result` 只将结果事实
  和文件引用投影为短通知。不要从提示字符串里反解析路径，也不要把文件 I/O 放进
  `peri-acp-types` 的 DTO 格式化方法。
- 优先复用显式后台执行现有的 stdout/stderr 日志；可以通知两个文件路径，
  无需为了单一路径额外复制合并大文件。timeout promotion 路径也应从进程输出采集
  开始持续写文件，避免超时后才从有限内存缓冲补写。
- 当前 `drain_pipe`/`tee_pipe` 的内存缓冲上限为 2 MiB，超过后不再保留后续输出。
  因此“完整落盘”必须从原始输出流写入，不能把这个截断缓冲落盘后称为完整结果。
- 文件创建、写入和收尾结果都要可检查。只有确认落盘成功才发送“完整输出已保存”；
  失败时发短诊断，明确文件缺失或不完整，仍完成任务状态结算。不能回退为将大段正文
  注入 reminder，也不能用不存在的文件路径表示成功。
- 文件至少保留到本轮接收方完成按需读取，沿用现有输出产物清理策略，不在发送通知后
  立即删除。通知大小与输出大小脱钩，动态字段仍受现有 reminder 契约约束。

该路径处理运行时数据，不再以 `trusted_reminder(...).expect(...)` 假定外部数据必然合法。
通知构造错误需显式处理；保留全局 64 KiB 限制。其他后台任务类型的输出策略不在本次
Bash 修复中一并改造。

### 2. 将终态保障收口到共享的 shell 完成路径

由共享 `finalize_bg_shell` 路径负责完成结算，让显式后台与 timeout promotion
遵守同一条生命周期契约。可预期的通知失败使用明确错误返回；意外 callback panic
需在任务边界被观察并结算，不能使任务永久停留在 Running。

输出文件完成写入后，携带结果引用的短通知必须在允许主循环退出前进入 inbox；
通知失败须保留诊断，避免静默丢结果。
进程执行结果与通知投递结果应分别记录：通知失败不能篡改真实进程退出码。
任务已被取消时保持既有去重规则，不重复发送完成事件，也不绕过 OS 进程回收证据。

只补 `registry.complete()` 仍不充分：`SessionInbox::await_wake()` 只认队列中的
Prompt/Defer，不会因为 `active_count` 降为零而自动返回。实施时必须让已经进入等待的
Receive 也能观察任务终态变化并重新检查退出条件；可从 TaskManager 的终态提交派生
可等待信号，与 inbox 一起等待，注册等待后重查状态，避免丢失唤醒。
这个信号只通知状态变化，任务事实仍由 registry 持有。

### 3. 回归验收

- 文件与通知：64 KiB 附近、100 KB、超过 2 MiB、中文多字节及 stdout/stderr；
  验证文件末尾未丢失、内容与实际输出一致，reminder 保持短小且没有输出正文。
  经 Read 工具按通知路径读取，确认该路径在执行上下文可访问。
- 落盘失败：覆盖文件创建、写入失败；通知明确报告不可读或不完整，不伪造成功路径，
  不回灌大段正文，也不妨碍终态结算。
- 生命周期：通知正常、返回错误、意外 panic，以及完成与取消竞争；检查终态只结算一次，
  没有 Running 残留，也没有重复完成事件。
- 等待顺序：主 Agent 已在等待时后台结束；主 Agent 正准备退出时后台结束；结果已被
  快速消费但 registry 尚在结算时。均需保证不会漏结果、漏终态或永远休眠。
- 真实 CLI：小输出对照与 timeout promotion 大输出场景，使用本地 provider，
  断言有 `result`、进程自行退出且无 `BodyTooLarge` panic。至少在原事故平台 Linux
  运行该场景；本轮已有 macOS 复现可作补充。

保持本次修复聚焦以上链路，不改 compact、ThreadStore execution-lock 或 CLI 轮数配置。
不通过放宽 reminder 全局上限、闲置超时或直接 `process::exit` 掩盖生命周期缺陷。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
| --- | --- | --- | --- | --- |
| 2026-09-18 | — | Open | agent | 核对 3.16.5 源码，真实 CLI 复现后台输出超限 panic 后无法结束；仅调查 |
| 2026-09-18 | Open | Open | 用户 / agent | 按用户要求定级 P1，限定修复范围；移除 max-turns 实施与验收，补充修复建议 |
| 2026-09-18 | Open | Open | 用户 / agent | 确认完整输出落盘，system reminder 仅保留少量状态与文件引用，明确通过 Read 按需读取；更新方案与验收 |
| 2026-09-18 | Open | In Progress | 用户 / agent | 授权 Luna 并行修改生产代码，由主 agent 集成和验收 |
| 2026-09-18 | In Progress | Fixed | agent | 完整输出文件、短通知、原子完成认领与 idle 唤醒已修复；macOS/Linux 真实 CLI 与工程门禁通过 |

## 修复记录

### 修复 #1（2026-09-18）

- 初始实现已提交：`81585448`（`fix(agent): persist background shell output and settle print completion`）。
- 按用户要求由 Luna 并行修改输出采集与终态唤醒，主 agent 集成、复审和验收。
- `ShellOutputCapture` 从原始 stdout/stderr 写入独立文件，保留超过 2 MiB 的输出；原始字节不经过预览截断。文件使用排他创建及 Unix 0600 权限；创建在 blocking context，流式写入、flush、清理使用异步文件 API。
- `BackgroundTaskResult::shell_output` 保存路径、完整性、错误与已知退出码，旧 JSON 缺少该字段仍可读取。仅两路完成且无读写错误时宣称完整；正常前台未引用文件会清理。
- shell 完成通知仅包含状态、退出码、文件引用和 Read 提示；`background_result_reminder` 由 inbox 路由和 executor 的 queue fallback 复用，校验失败生成有界诊断。全局 reminder 上限不变。
- `finalize_bg_shell` 在 registry 原子认领 `Running → Completing` 后才通知；取消先赢则不发布幽灵结果。认领期间仍计入活跃任务，回调在 registry 锁外执行，意外 panic 被记录但不跳过结算，也不篡改进程结果。
- registry 的 watch 信号唤醒 idle 重查；进程回收证据与可见任务终态分别结算，避免取消后清理证据遗漏。wait 错误路径显式终止并回收进程。
- 同步更新 Agent、types、middleware、TUI 的代码索引。`--max-turns`、compact 与 execution-lock 不在改动范围内。

### 自动化验收

最终门禁已通过；完整日志位于 `/tmp/peri-print-fix-20260918/`（临时目录可能被系统清理）。

| 验收 | 结果 |
| --- | --- |
| `cargo test -p peri-acp-types -p peri-agent --lib` | types 421 passed；Agent 807 passed |
| `cargo test -p peri-middlewares --lib terminal` | 42 passed |
| `cargo test -p peri-acp --lib background_projection_omits` | 1 passed；新增文件引用不泄漏至外部活动投影 |
| macOS `cargo test -p peri-tui --test print_background_exit --test print_exit` | 8 passed，进程均自行退出 |
| Linux ARM64 Docker 同一 CLI 命令 | 8 passed，进程均自行退出 |
| Linux `cargo test -p peri-agent --lib async_tasks` | 64 passed，含关闭排空与取消竞态 |
| `cargo build --workspace` | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo test --workspace --doc` | exit 0；8 passed，3 个既有 ignored |
| 改动文件 rustfmt、`git diff --check` | 通过 |

macOS 构建仍有既有 `__eh_frame section too large` 链接器警告，不影响本次构建与退出断言。

- 新 CLI 用例输出超过 2 MiB 的中文及独立 stderr，以退出码 1 结束；逐字节核对文件，并在删除原始输入文件后通过真实 Read 读取输出尾行，最终验证 `result` 和进程自行退出。
- 回归覆盖文件创建/写入失败、未 flush/丢弃 writer、回调 panic、取消与完成认领、idle 终态唤醒和队列消费顺序。

原现场完整命令与模型环境未提供，原事故 session 尚未重新运行；本次平台验收使用受控 provider。

验收中发现并修复了取消先赢时清理证据未结算的问题：显式后台进程先独立确认实际停止，再尝试完成认领。最后一轮静态复审无确定阻塞项。未在 Windows 上执行这些 Unix CLI 用例。

### 修复 #2：提交后审查（2026-09-18）

- 按用户要求对 `81585448` 派出独立 Luna code-reviewer，主 agent 同时验证边界场景。
- 修复前台取消后遗留输出文件：采集状态在最后一个 capture/writer owner 释放后回收未发布文件；成功交给后台任务的路径显式保留，继续供 Read 使用。显式 cleanup 在被取消时也保留待清理路径。新增最后一个 writer 释放、runtime shutdown、已关闭 runtime 拒绝清理任务三个回归场景；清理闭包持有 Drop guard，即使未启动被丢弃也执行删除。测试移至相邻 `shell_output_test.rs`。
- 修复剩余后台进程误报退出码：父 Shell 退出后只观察进程组是否停止，无法取得后代退出码，故通知使用未知退出码。真实 Shell 回归令后代退出 7，修复前错误报告父 Shell 的 0，修复后报告未知。
- 两项故障均先用回归测试证实失败，再验证修复通过。隔离子进程的 TMPDIR 验证前台取消：修复前残留两个文件，修复后无残留。
- 独立 reviewer 发现启动失败与 registry 容量耗尽同时发生时，会返回成功句柄却没有失败通知。新增真实 spawn 失败回归证实，现同步返回启动/注册错误，保留正常注册后的完成通知路径。已关闭 runtime 拒绝清理任务的风险也用失败用例证实后修复。
- 复验 `cargo test -p peri-middlewares --lib terminal`：43 passed；macOS/Linux ARM64 各运行两个真实 print-mode CLI 用例，均能在后台结束后读取完整输出并自行退出。
- `cargo test -p peri-agent --lib async_tasks`：68 passed；`cargo clippy --workspace --all-targets -- -D warnings` 与 workspace doc tests 均 exit 0；格式及 diff 检查通过。
- Linux ARM64 最终异步任务回归同样 68 passed；独立 reviewer 对修复后的失败通知、文件所有权、取消/关闭与未知退出码分支复核，未发现确定的残留阻塞项。
- 本轮证据：`/tmp/peri-print-review-20260918/`（临时目录可能被系统清理）。
