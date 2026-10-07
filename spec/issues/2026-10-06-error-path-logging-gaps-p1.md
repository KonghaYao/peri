# 错误路径日志缺口（P1 批次，8 条）

**状态**：待实施。结论来自静态代码审计，**未做运行时复现**；行号以 2026-10-06 工作区为准。
**优先级**：P1。
**类型**：可观测性 / 错误路径 tracing 覆盖。
**创建日期**：2026-10-06。
**来源**：多 owner 静态审计给出的 8 条 P1 原材料；本文由单一 owner 逐条打开代码复核，与原材料不符处集中在 [§核对差异](#核对差异与原始材料的出入)，正文以代码为准。

## 背景

- **独立 P0**：[Agent 内部执行失败缺失可定位诊断](2026-10-06-p0-agent-internal-error-diagnostic-loss.md) 已有真实日志证据并由用户指定重点关注；fatal 漏斗虽有 ERROR，但仅记录通用公开文案，不等于诊断充分。本文件的 8 条 P1 不随之整体升级。
- 本文只收录 P1：**错误可达客户端/界面**，且该路径没有 `tracing`/`log` 记录，或只有默认不启用的级别。
- 判定基准：错误是否通过四出口之一到达用户/客户端；该出口链路上是否存在会在默认 `info` 过滤下可见的日志。
- 事实与推断分列：每条给 **出口形态 / 位置 / 触发条件 / 用户可见后果 / 日志缺口性质 / 证据强度 / 修复方向 / 验收标准**；行号与机制为核对后的代码事实，因果链的远端后果标注为推断。
- P2/P3 不进入本文件；本文不替代设计与标准，稳定结论在修复时回写对应事实源。

## 出口分类（四出口，正文反复引用）

| 代号 | 形态 | 说明 |
| --- | --- | --- |
| A | `Err` 上浮 | 由调用方或 fatal 漏斗记录，覆盖面最好 |
| B | 事件 / 终态 | 错误变成 error card、`TurnEnded`、`stop_reason` 交给客户端 |
| C | 状态化 / 落库 | 错误只写进内部字段或落库动作，常无日志；事后以"另一个错误"或沉默回到用户 |
| D | fail-open 降级 | 错误在产生处被抹平成空事件/默认值，用户看到结果缺失而非报错 |

## 场景

### P1-1 Interrupted 抑制路径跨层零日志（peri-model → peri-agent）

- **出口形态**：B + D。错误先被降级成 `Ok` 事件（D），再以"流中断/续跑"协议回到界面（B）。
- **位置**：
  - `peri-model/src/runtime/retry.rs:349-366`：已产生可见 delta 后，任何 `Err` 一律 `send_event(Ok(ModelStreamEvent::Interrupted{..}))` 后 `return`；不返回 `Err`、不调用 retry observer。
  - `peri-model/src/runtime/retry.rs:482-484`：`interrupted_from()` 把 transport 类错误压成 `ModelError::stream_interrupted(provider, request_id)`；`peri-model/src/runtime/error.rs:436-442` 只保留这两个字段，`Transport/HttpStatus` 等诊断信息在此丢失（非 transport 类错误保留原 error 作为负载）。
  - `peri-model/Cargo.toml:8-19`：依赖表无 `tracing`/`log`；`peri-model/src` 全目录无任何日志宏（静态确认）。
  - `peri-agent/src/agent/model_bridge.rs:375-413`：`Interrupted` 折叠为 `Ok(reasoning)`，错误放进 `reasoning.stream_interruption`。
  - `peri-agent/src/agent/stages/reason.rs:177-188`：`error!` 只在 `Err(e)` 分支，因此被绕过。
  - `peri-agent/src/agent/stages/mod.rs:417-439`：`enqueue_stream_interruption_continuation` 只入队提醒，无日志。
  - `peri-agent/src/agent/workflow/agent/observation.rs:113-120`：`warn!("... llm retrying")` 只在 `LlmRetrying` 事件触发，而该事件由 `retry.rs:457-462` 的 `observer.on_retry` 发出，抑制路径不经过它。
- **触发条件**：流式响应已经产生至少一个可见 delta 后，transport/decoder/protocol 错误或流提前结束（`retry.rs:349-366, 382-408`）。
- **用户可见后果**：界面出现"模型流中断，正在继续"的提示（提醒文案见 `mod.rs:427-428`），回答内容可能已缺失；日志文件中该次中断无任何记录，且 transport 细节在源头即丢失。
- **日志缺口性质**：错误在产生处被降级为成功事件（D），下游 `error!` 分支不可达；承载记录的 crate 结构上无日志能力。
- **证据强度**：静态确认（依赖表、无日志宏、折叠为 `Ok`、retry observer 未触发、提醒无日志）。推断：用户可见"内容缺失"的具体程度取决于中断时机。条件性：**预算耗尽**（`mod.rs:869-874` 返回 `StreamRecoveryExhausted`）会落到 `v2_execute.rs:780-789` 的 fatal 分类，因此 `v2_execute.rs:629-636` 的 `error!` 会记录；本缺口的范围是**未耗尽的中断续跑路径**与源头诊断丢失，不是"耗尽也无日志"。
- **修复方向**：在 `retry.rs` 的抑制分支保留可注入的诊断出口（`RetryObserver` 已有 `on_retry`，可复用一个"中断/抑制"观察点），或在 `peri-agent` 侧接收 `Interrupted` 时（`model_bridge.rs:375-413`）以 `warn!` 记录 `attempts/max_attempts` 与保留的 error；transport 细节需在 `interrupted_from()` 之前捕获，不能只靠 `provider + request_id`。
- **验收标准**：制造"已出 delta 后中断"（可复用 `peri-agent/tests/stream_interruption_test.rs` 的 HTTP fixture 手法），断言默认 `info` 过滤下日志包含中断事件与尝试次数；断言非耗尽续跑与耗尽终态各自有一条可见记录；断言日志不含凭据/body。

### P1-2 连续截断耗尽终态无日志（peri-agent）

- **出口形态**：B（`TurnEnded` 终态 + `stop_reason`），贯穿路径上没有日志。
- **位置**：
  - `peri-agent/src/agent/stages/mod.rs:877-883`：`consecutive_truncations > MAX_TRUNCATION_CONTINUATIONS` → `LoopResult::Error(AgentError::OutputTruncated{attempts})`。
  - `peri-agent/src/agent/stages/act.rs:155-164`：截断响应提交展示事件后返回 `Ok`（不是错误）。
  - `peri-agent/src/session/exec/executor_helpers/v2_execute.rs:768-777`：`OutputTruncated` → `stop_reason=MaxTokens`、`failure=None`、`turn_status=Error`、`turn_error_kind=LlmFailure`。
  - `peri-agent/src/session/exec/executor_helpers/v2_execute.rs:667-675`：发布 `TurnEnded{status, error_kind}`。
  - `peri-agent/src/session/exec/executor_helpers/v2_execute.rs:629-636`：唯一的 fatal 日志要求 `terminal.failure` 为 `Some`；本场景 `failure=None`，不触发。
  - `peri-acp/src/event/mapper.rs:236-262`：`TurnEnded` 属于映射为空 standard 事件的分支（不产生用户可见文本）；`error_kind` 只在 `event_pump.rs:61-63` 被取给 Langfuse 遥测。
  - `peri-acp/src/host/prompt.rs:122-137`：`failure=None` 时返回成功 `PromptResponse`，`stop_reason=MaxTokens`。
- **触发条件**：无完整工具调用的响应连续达到 3 次 `MaxTokens`（预算 2 次续跑，测试见下）。
- **用户可见后果**：客户端只看到"回答被截断后停止"（`stop_reason=max_tokens`），没有错误提示；服务端既无 `error!` 也无 `warn!`，事后无法从日志区分"正常结束"与"截断耗尽"。
- **日志缺口性质**：终态被表达为事件字段（B）而非 fatal failure，日志条件（`failure.is_some()`）与"错误终态"判定不一致。
- **证据强度**：静态确认（四条映射与日志条件均可逐行核对）。已有测试 `peri-agent/src/agent/stages/truncation_test.rs:478-503` 断言 3 次截断得到 `OutputTruncated{attempts:3}`，无日志断言。推断：客户端不会自行提示错误（依赖 mapper 的空事件结论）。
- **修复方向**：把 `turn_status=Error` 的终态统一纳入一条可见记录（在 `v2_execute.rs` 终态分类处对 `turn_error_kind.is_some()` 记录，或在 `mod.rs:877-883` 返回错误处记录 attempts 与模型名）；注意不要与 `failure` 的公开错误投影混淆，避免重复或泄漏。
- **验收标准**：截断耗尽场景在默认级别日志中出现一条带 `attempts` 的记录；同时验证 `failure=Some` 路径不重复记录；补一条日志断言到 truncation 测试。

### P1-3 后台子 agent panic → 状态永久失真（peri-agent）

- **出口形态**：C（内部 `uncertain` 状态 + registry 条目），panic 可见性另走 stderr。
- **位置**：
  - `peri-agent/src/session/subagent/background.rs:397-437`：`TaskManager::spawn_owned` 起执行 future，**无 `catch_unwind`**；返回的 `JoinHandle` 存进 `BgCancelHandle::Abort`（`:411`），此后不再 inspect。
  - `peri-agent/src/agent/async_tasks/scope.rs:69-84`：`spawn_admitted` 用 `scope="owned"` 的 `ExternalGuard` 包装任务；`:141-150` `Drop` 在未 `confirm_stopped()` 时把 scope 写入 `uncertain`，**无日志**。
  - `peri-agent/src/agent/async_tasks/registry.rs:757-789`：取消分支 grace 等待；`peri_time::timeout(...).await.is_err()` 把 `Ok(Err(JoinError))`（任务 panic）当作正常结束，JoinError 被丢弃。
  - `peri-agent/src/agent/async_tasks/manager.rs:188-190`：`is_execution_idle() = scope.is_idle() && external_settled()`；`:365-369` `session_close_settled()` 同样要求 `scope.is_idle()`。
  - 生产清除入口只按真实 owner 名：`peri-middlewares/src/mcp/client/subscription_tasks.rs:176-183`（`client.name` 调用点 `:317,:340,:540`）、`peri-agent/src/agent/async_tasks/manager.rs:713`（`"workspace"`），**没有** `"owned"` 的清除调用点。
  - 对照实现（panic 也保证结算）：`mcp-packages/common/src/shell_executor.rs:165,316`、`peri-workflow/src/tool/completion.rs:93`。
  - panic 可见性：`peri-tui/src/kit/panic.rs:47-55` 装了写 `tracing::error!` 的 hook；`peri-tui/src/main.rs:791-806` 的 `Commands::Acp` 分支与 `peri-acp/src/host/stdio/mod.rs:63-85` 只 `init_tracing`，**无 panic hook** → 默认 hook 写 stderr，编辑器端不可见。
- **触发条件**：后台子 agent（`/bg` 或独立后台执行）的执行 future panic，且随后走取消分支。
- **用户可见后果**：registry 条目停留在 Running；父 agent 收不到完成提醒（推断：`complete`/`settle_completed` 未执行）；`scope` 的 `"owned"` 不确定性永不消除（事实），使 `is_execution_idle()` 恒为 false；`reopen` 被 `-32010 "Reopen Blocked: previous live execution is not closed"` 阻断（`peri-acp/src/host/requests/session_control/reopen.rs:47-61`），fork 被 `-32010 "Cannot fork while source execution is active"` 阻断（`peri-acp/src/host/requests/session_lifecycle.rs:656`），lifecycle 切换被拒（`peri-acp/src/session/lifecycle_binding.rs:33-38`）。
- **日志缺口性质**：错误被 tokio 捕获成 JoinError（不是 fatal），guard 的 `Drop` 只改状态不记录，清除入口又不认 `"owned"`，于是没有任何一条链路会说出"某个后台任务的执行 panic 了"。
- **证据强度**：静态确认（无 catch_unwind、guard Drop 无日志、JoinError 被丢弃、`is_execution_idle` 定义、无 `"owned"` 清除点、两处 panic hook 差异）。推断：UI 长期"运行中"、父 agent 收不到提醒。**待证**：`Session close`（`peri-acp/src/host/requests/session_close.rs`）是否同样被该状态阻断——关门判定未直接引用 `is_execution_idle`，需运行时确认。
- **修复方向**：与既有对照实现一致，在 `spawn_owned` 包装层用 `AssertUnwindSafe(...).catch_unwind()`（panic 也走 `confirm_stopped` + registry 结算），或在取消等待处检查并记录 `Ok(Err(JoinError))`；同时为 `"owned"` scope 增加可清除证据或把不确定性原因写进日志与任务状态。
- **验收标准**：构造后台子 agent panic，断言：默认级别日志出现一条含 task_id 的记录；registry 不停留 Running；`is_execution_idle()` 恢复 true；reopen/fork 不再被该场景阻断。

### P1-4 解码 fail-open 三连（peri-model）

- **出口形态**：D（缺失字段按空值继续），最终以"内容/参数缺失"而非报错呈现。
- **位置**：
  - `peri-model/src/openai_compatible/stream.rs:63-69`：`choices` 缺失/非数组/为空 → `return Ok(events)`；畸形空帧与合法的 usage-only 帧在此不可区分。
  - `peri-model/src/openai_compatible/stream.rs:70`：`delta` 缺失或类型错 → 以 `Value::Null` 继续。
  - `peri-model/src/openai_compatible/stream.rs:112-117`：tool call `arguments` 缺失或非字符串 → `.unwrap_or_default()` → 该分片按空串累积（`:124`）。
  - `peri-model/src/anthropic/stream.rs:239-248`、`:250-259`、`:269-280`：`text` / `thinking` / `partial_json` 缺失 → 空串；前两者空串不产出事件，`partial_json` 空串仍产出 `ToolCallDelta`。
- **触发条件**：provider 返回上述任一畸形/缺字段帧（含代理改写、截断帧）。
- **用户可见后果**：文本/思考分片缺失；工具参数分片缺失。后者**条件性**：累积参数最终会在 `openai_compatible/stream.rs:170-171` 做 JSON 解析，多数丢分片会解析失败 → `provider_protocol_error` → 走 P1-1 的中断路径（仍然无日志）；只有剩余部分仍能解析时才是真正"静默截断"并可能带着错误参数执行。
- **日志缺口性质**：crate 结构无日志能力（`peri-model/Cargo.toml:8-19`，无 `tracing`/`log`），且判定被写成"无值即默认值"，错误在产生处不可见。
- **证据强度**：静态确认（四处 `unwrap_or_default/Value::Null`、空帧早退、参数 JSON 解析点）。推断：参数静默截断的具体频率与后果（需真实畸形流验证）。关联既有记录：`spec/history/2026-09.md`（2026-09-29 条目；空参数被误判为中断）——与本条同源但不同缺陷，修复时不要一刀切（缺失/畸形帧必须区别于合法 usage-only 帧）。
- **修复方向**：为"缺 `choices` 的帧"与"usage-only 帧"建立显式判别；对 `delta`/`arguments`/`partial_json` 缺失或类型错改为可上报的解码诊断（经统一诊断出口），或在 `peri-agent` 侧对 `ToolCallDelta{arguments_delta: ""}` 做一致性检查。
- **验收标准**：为四类畸形帧各补一条测试，断言：合法 usage-only 帧不报错；畸形帧产生可见诊断（日志或事件），不静默降级；工具参数缺分片不会以残缺参数执行。

### P1-5 子 agent 终态写库静默失败（peri-agent）

- **出口形态**：C。错误只体现在"库里没写成功"，调用方看不到。
- **位置**：
  - `peri-agent/src/session/subagent/background.rs:357-372`：`let _ = store.update_session_meta(...)` 吞掉写失败。
  - `peri-agent/src/session/subagent/lifecycle.rs:154-169`：`on_subagent_stop_handler` 同样 `let _ = ...`。
  - 对照（同语义写入是上抛的）：`peri-agent/src/session/subagent/factory/claim.rs:187-196`，失败返回 `"resume terminal status write failed: ..."`。
- **触发条件**：子 agent 结束（正常/错误/取消）时 `SessionResources::update_session_meta` 返回 `Err`。
- **用户可见后果**：库中 `status` 停留 `running`（推断）；后续 resume、会话列表、关闭/回收判定读到失真状态（推断，具体消费方需按 `AgentStatus` 的读取点逐一确认）。
- **日志缺口性质**：写入结果被显式丢弃（`let _`），与同语义的 `claim.rs` 处理不一致——属处理不一致，不是有意的降级设计。
- **证据强度**：静态确认（两处 `let _` 与一处 `?` 上抛的对照）。推断：库中状态停留 running 及其下游影响。
- **修复方向**：统一为可观测处理——至少 `warn!(thread_id, %error, "subagent terminal status write failed")`；若终态写入是硬要求，按 `claim.rs` 的方式上抛并让调用方决定。
- **验收标准**：注入 `update_session_meta` 失败，断言默认级别日志中出现含 thread_id 与原因的记录；断言两条路径（bg 与 sync stop handler）行为一致。

### P1-6 workflow 终态覆写静默（peri-workflow）

- **出口形态**：C（磁盘 `state.json` 与工具结果不一致），并向父 agent 返回 `status=failed`（B）。
- **位置**：
  - `peri-workflow/src/tool/completion.rs:119-136`：`execution_error=Some` 时用 `if let Ok(mut state) = read_state(...)` 覆写为 failed；**Err 分支无 else**（整块静默跳过），且 `:128` `let _ = write_state(...)` 忽略写失败。
  - `peri-workflow/src/tool/completion.rs:129-135`：`progress.apply_event(RunDone{failed})` 也在这段 `if let Ok` 内 → 读失败时连"幽灵 running"的收敛事件也一并跳过。
  - `peri-workflow/src/tool/completion.rs:183-187`：`project()` 从 `read_state` 取 `acceptance_status`（`:207` 放进结果）；`delivery_status` 来自 `result.delivery_status`（`:188`、`:209`），**不是**由 acceptance_status 推导（推导在 `peri-workflow/src/runner/terminal.rs:55-88` 的 `project_postcondition`，且所有写入方都把 acceptance_status 记为 `Unknown`：`terminal.rs:35,125,155,236`）。
  - 对照（同类写入都有日志）：`peri-workflow/src/runner/terminal.rs:50-52`（warn）、`:177-190`（warn/info）；`:192-196` 注释记录了同源的"幽灵 running"。
  - 前置写入方：`runner/terminal.rs:113-217` 的 `finalize_workflow` 在 msg_loop 自然结束时按 `final_result.status` 写 `state.json`（成功时即 `completed`）。
  - 外部消费者：`peri-middlewares/src/workflow/mod.rs:143-151` 的 `resume_workflow` 直接读该 `state.json`。
- **触发条件**：workflow 本体已成功（msg_loop 已写 completed），随后清理失败（`completion.rs:95-112` 的 `Ok(Err(error))`/panic 分支）需要把终态覆写为 failed；此时 `read_state` 或 `write_state` 失败。
- **用户可见后果**：父 agent / 客户端收到 `status=failed`，而磁盘 `state.json` 仍为 `completed`（`error=None`、`delivery_status` 可能是 Deliverable）；读该状态的 `resume_workflow` 与审计路径看到过期成功态；若 `read_state` 失败，progress store 也不会收到 failed 的 `RunDone`，形成"幽灵 running"。
- **日志缺口性质**：错误分支被 `if let Ok` 吞掉、写结果用 `let _` 丢弃，与 `runner/terminal.rs` 的同类写入处理不一致。
- **证据强度**：静态确认（控制流、`let _`、对照日志、`project()` 字段来源、读取方）。推断：`resume_workflow` 与审计看到过期成功态的具体影响面。
- **修复方向**：`read_state` 失败时记录并仍写入可写出的失败终态（或明确放弃并 warn）；`write_state` 失败至少 `warn!`；把 `RunDone{failed}` 的 progress 收敛移出 `if let Ok`。
- **验收标准**：注入 `read_state`/`write_state` 失败，断言：默认级别日志各出现一条含 run_id 与原因的记录；磁盘 state 与返回结果不出现"completed 文件 + failed 结果"的组合（或明确在日志中声明不一致）；progress store 不停留 Running。

### P1-7 后台任务完成提醒投递失败只进内存（peri-agent）

- **出口形态**：C（内存 `PublicationStatus::Failed`），重试时只有默认不可见的记录。
- **位置**：
  - `peri-agent/src/session/bg_complete.rs:100-109`：异步投递结果写入内存 map（`Ok → Accepted`、`Err → Failed(error)`），**无日志**。
  - `peri-agent/src/session/bg_complete.rs:78`：重试时 `tracing::debug!(%error, "retrying original terminal publication")`；默认 `info` 过滤下不可见。
  - 投递失败来源：`peri-agent/src/agent/async_tasks/delivery.rs:151-195`（`load_session_work` 失败、找不到 task binding、`build_task_terminal_command` 校验失败、`WorkMutationBarrier::commit` 失败）。
  - 重试时机：`peri-agent/src/session/exec/executor.rs:302`（下一条命令开始）与 `peri-agent/src/agent/async_tasks/manager.rs:314-316`（shutdown）。
  - 可见痕迹：`peri-agent/src/agent/async_tasks/settlement.rs:60-79` 的 `warn!("task completion delivery remains pending")`（`:73`，**含 `%error`**，错误经 `DeliveryFailed{reason}` 携带原因，见 `registry.rs:41-42`）；首次失败还有 `peri-agent/src/session/subagent/background.rs:386-391` 的 `tracing::error!("subagent terminal delivery is pending")`。
- **触发条件**：后台任务完成时的终态投递（`on_bg_complete` 回调；生产装配见 `peri-agent/src/session/exec/executor/agent_build.rs:172-180`）失败。
- **用户可见后果**：发起会话收不到完成提醒（推断）；首次失败的可见日志文案是 `Pending`（回调在投递被异步 spawn 后立即返回 `Err("...Pending...")`），**真正的原因**（`delivery.rs` 的四个失败点）只在内存 map 与 `debug!` 记录里，默认日志看不到。
- **日志缺口性质**：异步投递结果只回写内存状态；同步路径能看到的只是"待投递"这一通用结论，具体原因被 `debug` 级别挡住。
- **证据强度**：静态确认（`:100-109` 无日志、`:78` 为 debug、失败来源四处、重试时机、`DeliveryFailed` 携带 reason）。推断：会话永久收不到提醒。
- **修复方向**：`bg_complete.rs:100-109` 的 `Err` 分支补 `warn!`/`error!`（含 delivery_id、task_id、原因）；把 `:78` 的重试日志提升到 `info` 或在首次失败即记录原因。
- **验收标准**：让投递在 `delivery.rs` 的任一点失败，断言默认级别日志出现含 task_id 与真实原因的记录；重试路径不重复刷屏且原因仍可见。

### P1-8 工具解析错误只出错误卡片、无逐条日志（peri-agent，条件性 P1）

- **出口形态**：B。错误通过 `ToolEnded{is_error:true}` 到界面，dispatch 本身可返回 `Ok`。
- **位置**：
  - `peri-agent/src/agent/stages/tool_dispatch.rs:124-139`：畸形/重复 tool call ID → `ToolResult::error(...)` 进 `resolution_errors`。
  - `peri-agent/src/agent/stages/tool_dispatch.rs:141-157`：resolver 返回 `Err` → 同样进 `resolution_errors`。
  - `peri-agent/src/agent/stages/tool_dispatch.rs:159-163` → `:51-70`：`emit_settled_tool_render` 直接发 `ToolEnded{is_error:true}` 给客户端。
  - `peri-agent/src/agent/stages/tool_dispatch.rs:247-248`：`run_after_tools_batch` 只针对已进 policy 的结果；`resolution_errors` 在 `:248` 才并入，绕过了逐工具日志 `peri-agent/src/agent/stages/tool_dispatch/execution.rs:492-498`（`warn!("tool call failed")`）。
  - 唯一的兜底：`tool_dispatch.rs:274-288` 的连续失败计数器在**恰为**阈值（5）时才 `warn!`，且计数是会话级累计（`handle_consecutive_failures` 在 `:251` 调用，合并之后），不是本条调用的专属记录。
  - 上游校验（决定可达性）：`peri-agent/src/agent/stages/work_reason.rs:170-209` 的 `commit_response` 在 Reason 阶段先校验 ID 与调用目标（`:199-209` 失败即 `Err`），调用点 `peri-agent/src/agent/stages/reason.rs:328`。
- **触发条件**：调用 `dispatch_tools` 时携带畸形/重复 ID，或 resolver 解析失败。
- **用户可见后果**：界面上是普通的工具错误卡片，服务端没有逐条记录；只有累计到第 5 次失败时才有一条通用 `warn!`。
- **日志缺口性质**：错误被提前结算成 `ToolResult`（事件出口），逐工具日志只在执行路径上，结算路径没有对应的记录点。
- **证据强度**：静态确认（三条路径与两处日志位置）。**条件性/待证**：当前代码下该分支**静态判定为 test-only**——`ensure()` 返回 `Ok(None)` 只在 `#[cfg(test)] BestEffortFixture`（`peri-agent/src/agent/stages/work_boundary.rs:75-78`），非 test 构建恒为 `Durable`（`work_boundary.rs:160-161`），且 `WorkRuntime::BestEffortFixture` 本身是 `#[cfg(test)]`（`peri-agent/src/agent/stages/work_pipeline.rs:35-39`）；`dispatch_tools` 的唯一调用点是 `peri-agent/src/agent/stages/act.rs:91`。因此本条目按"P1（条件性）"保留：代码路径确实无逐条日志，但生产可达性为"静态否"，需架构/装配方确认是否存在其它装配路径（未做运行时复现）。
- **修复方向**：若确认需要覆盖（或为非 durable 装配保留能力），在 `tool_dispatch.rs:159-163` 的结算循环里按条 `warn!`（tool_call_id、tool name、原因）；否则关闭本条目并在注释中说明该分支为测试面。
- **验收标准**：若实施——构造畸形 ID/resolver 失败，断言默认级别日志逐条出现；若判定不为生产可达——在文档/注释中记录静态可达性结论，并把本条目降级。

## 跨场景共性

- 默认日志过滤器是 `info`（`peri-agent/src/telemetry/subscriber.rs:56-58`：`EnvFilter::new("info,peri_middlewares::mcp=warn,peri_middlewares::plugin=warn,rmcp=warn")`），因此 P1-1/P1-7 一类 `debug!` 等于没记。
- `peri-model`（`peri-model/Cargo.toml:8-19`，生产可达）与 `peri-runtime`（`peri-runtime/Cargo.toml` 无 `tracing`，全 crate 无 `tracing::` 使用）结构上不具备日志能力；这是观察，不单列为 P1。
- 原判断"`peri-controller` 也不具备日志能力"**不成立**：`peri-controller/Cargo.toml:24` 有 `tracing.workspace = true`，`peri-controller/src` 有 44 处 `tracing::`（集中在 langfuse 桥，如 `langfuse/tracer/turn.rs:121,183`）。
- 共性机制：错误一旦被"投影"成事件、状态或默认值（B/C/D 出口），就离开了 A 出口的 fatal 漏斗，而漏斗是当前唯一稳定的日志点；修复应优先给 B/C/D 出口各补一个统一记录点，而不是逐处打补丁。

## 核对差异（与原始材料的出入）

| 条目 | 原材料 | 核对结果 |
| --- | --- | --- |
| P1-1 | 抑制分支 `retry.rs:348-365` | 分支实际为 `349-366`（含首个 cancelled 分支在 `348`）；机制描述成立 |
| P1-1 | "整条链无任何记录" | 成立范围是**未耗尽的续跑路径**；预算耗尽会经 `StreamRecoveryExhausted → fatal` 记录（`v2_execute.rs:629-636`） |
| P1-3 | `session_lifecycle.rs:656` 指 reopen/close | 实际是 `peri-acp/src/host/requests/session_lifecycle.rs:656` 的 **fork** 门禁（"Cannot fork while source execution is active"）；reopen 见 `reopen.rs:47-61`，lifecycle 切换见 `lifecycle_binding.rs:33-38`。"close 是否被阻断"原文未证，本文标为待证 |
| P1-6 | `project()` 用 `read_state` 的 acceptance_status 决定 delivery_status | `delivery_status` 来自 `result.delivery_status`（`completion.rs:188,209`）；`read_state` 的 acceptance_status 只是被复制进结果字段（`:183-187,207`）。acceptance_status→delivery 的推导在 `runner/terminal.rs:55-88`，且所有写入方都写 `Unknown`。真正的"过期成功态"是整份 `state.json` 仍为 `completed` |
| P1-6 | 只提 `read_state` 静默跳过 | 补充事实：`progress.apply_event(RunDone{failed})` 也在 `if let Ok` 内（`completion.rs:129-135`），读失败时连收敛事件一起丢 |
| P1-7 | warn 在 `peri-agent/src/session/subagent/settlement.rs:60-79`，且"不含原因" | 实际在 `peri-agent/src/agent/async_tasks/settlement.rs:73`，且**含 `%error`**；首次失败另有 `background.rs:386-391` 的 `error!`（生产 `on_bg_complete` 为 `Some`，`agent_build.rs:172-180`）。准确表述：可见日志只带"Pending/DeliveryFailed"通用结论，真实原因仅在内存与 `bg_complete.rs:78` 的 `debug!` |
| P1-8 | 非 durable 分支"是否可达未证" | 静态判定为 test-only：`work_boundary.rs:75-78,160-161` + `work_pipeline.rs:35-39`；仍按条件性 P1 保留（未做运行时复现） |
| 共性 | `peri-controller` 无日志能力 | 不成立，`peri-controller/Cargo.toml:24` 有 `tracing` 且有实际使用 |
| P1-2 | 测试 `truncation_test.rs:480-501` | 函数体为 `478-503`，断言在 `484-502`；断言内容与原文一致 |
| P1-4 | "工具参数静默截断" | 条件性成立：累积参数会在 `openai_compatible/stream.rs:170-171` 解析，多数丢分片会变成 protocol error（进入 P1-1 的中断路径），只有剩余部分仍能解析时才静默 |

## 修复批次建议（同一批次内可并行核对）

1. **先补 D/B 出口的记录点**：P1-1（`retry.rs` 抑制分支 + `model_bridge.rs`）、P1-2（`OutputTruncated` 终态）、P1-4（解码诊断出口）。
2. **再补 C 出口**：P1-5（`let _` → warn/上抛）、P1-6（`completion.rs` 读/写失败）、P1-7（`bg_complete.rs` 异步结果记录）。
3. **P1-3 单独立项**：涉及 panic 结算与 `"owned"` 不确定性的清除语义，先按运行时复现确认 close 是否同样被阻断。
4. **P1-8 先裁决可达性**（静态结论倾向 test-only），再决定实现或降级。

以上各条均以"默认 `info` 过滤下可见 + 不泄漏 body/凭据"为验收前提；日志内容需遵守 `v2_execute.rs:627-628` 已有的安全投影约束。
