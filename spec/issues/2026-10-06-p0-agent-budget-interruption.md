# P0：Agent 推理预算耗尽被误报为内部错误

**状态**：Awaiting user verification；错误分类、默认预算一致性及显式新输入解阻已实施，少量针对性验证通过；真实运行最终验收由用户完成。
**优先级**：P0（用户指定）。
**现场时间**：2026-10-06 13:54:02（Asia/Shanghai）。
**范围**：长任务预算中断、错误可观测性和旧阻塞绑架新输入。用户随后授权“全部修复问题”，要求 subagent 快速修复、少量验证并由用户最终验收；据此统一默认预算并补齐显式新输入解阻，不自动重置旧账本或重放已有副作用。

## 确认证据

- 截图最后一条工具为 Grep，pattern 为 `Resume|恢复|Blocked|对账`，37 matches，随后显示 `Agent execution failed: An internal error occurred. Check logs for details.`。
- `.tmp/agent-tui.2026-10-06` 在 `2026-10-06T05:54:02.727275Z` 记录 session `01a10fb5-4c19-7e53-a4b2-4ff34e2f44d6` 的 `[v2] execution failed`，`kind="internal"`。日志只保留公开错误投影，未保留具体内部原因。
- 使用 `sqlite3 -readonly` 查询现场数据库：该 session 的 WorkState revision 为 490；末个未完成 Work `01a10fc6-9c2f-7a92-b6d5-7d24155e6bd7` 为 `blocked`，reason 为 `reason budget exhausted`，recovery condition 为 `explicit budget reset authorization`。
- 对应 budget `f8716005-f865-8c6e-bb93-e6d3125d0774` 的 `reasonRequests=64`，持久 limits 的 `reasonRequests=64`；`dispatches=76`、上限 256。前一 batch 的 50 次推理使用独立预算，不与本批次混算。
- 最后工具 invocation `01a10fc6-8726-7b60-b2d9-0ec99601406f` 的 toolName/arguments 与截图 Grep 完全匹配，status 为 `settled`，outcome 为 `completed`。工具本身与提交均成功。
- 最后 BeginReason 回执为 accepted / blocked（revision 488→489），其后 FinishAdmission 回执为 accepted（revision 489→490）。这是可确认的领域阻塞，不是模型请求失败或执行结果未知。
- 现场 ControlState 为 `lifecycle=1`、`status=active`、`revision=4`、`controlGeneration=0`、`attempt=null`。错误收尾后活跃 attempt 已清空；阻塞 Work 仍属于当前生命周期。

## 根因链

`WorkLimits::default().reason_requests=64` → 每个 processing batch 持久累计模型请求预算 → 第 65 次 Reason 准入前 `begin_reason` 将 Work 转为 Blocked → `work_reason::prepare` 把非 ReasonInFlight 回执包装为普通 anyhow 错误 → AgentError::Other → ExecutionFailure Internal → TUI 显示通用内部错误。

循环入口的 500 次语义迭代限制不替代持久预算。预算阻塞本身符合现行 `docs/design/rcra-message-activation.md` 的授权与有界执行约束；将已知预算耗尽表达为未知内部错误是实现缺陷。当前没有生产用户配置入口调整 WorkLimits，也没有发现生产调用 ResetBudget 的完整恢复入口；不能建议用户通过重启或普通 Resume 绕过累计预算。

## 二次故障：修复前的当前会话准入闭环缺口

`work/query.rs` 将当前生命周期任意 Blocked Work 汇总为 `snapshot.blocked=true`，从而不产生包括新输入在内的执行 candidates。`session/user_input_mailbox/staging.rs` 的自动推进和 `work_boundary.rs` 的执行准入也拒绝 blocked snapshot。

显式选择用户输入虽带 `interrupt_current=true`，但 `work/user_input.rs` 仅 abandon batch.execution 与当前 control.attempt 精确匹配的旧 Work。现场 attempt 已为 null，旧 Blocked Work 不符合该条件；因此不能靠普通重发保证解阻。这不是单纯错误文案问题。

`ResetBudget` 只清计数、不恢复 Work stage；`ResumeWork` 要求当前 execution guard，而 blocked 状态又阻止新 admission。错误退出清 attempt 后，恢复存在闭环缺口。该问题需要生命周期/授权语义及回归验证，不能通过直接改库、伪造 attempt 或放宽执行 guard 处理。

**旧二进制临时止损**：保留旧会话和现场，另建会话携带必要上下文继续；重发有副作用的任务前核对已提交工具结果。修复后的二进制允许在原会话显式发新输入，放弃旧 processing 而保留历史与副作用证据。未替用户执行 Close/Reopen、预算重置、数据库写入或进程重启。

## 本轮实施

- 默认 Reason 预算由 64 调整为与主 Agent 默认迭代上限同源的 500；Dispatch 由 256 调整为独立有界的 2000，Recovery 保持 8。子 Agent 的既有 200 或显式 max_turns 语义上限未放宽。
- 旧会话完整匹配旧默认 limits 时，只在成功的显式用户选择中升级策略；所有旧 budget counters 保留。任何可辨识的定制 limits 不升级；刻意设置成完整旧默认数值的定制无法与历史默认区分。加载、重启、自动扫描不迁移或恢复执行。
- 显式新输入在无 attempt 时放弃当前生命周期旧非终态 processing；有活跃 attempt 时仅精确匹配。普通空闲 enqueue 也走该用户授权入口；忙时输入仍排队，加载和历史扫描不获得解阻授权。
- 新任务发布固定 `expected_attempt=None` 和观察到的控制代际；并发启动或较新的 Stop 不会被隐式新任务接管。成功发布仍使用稳定 mutation ID、生命周期与 revision CAS；重放不再次 abandon 或重置预算。
- 已明确放弃、且无可执行 work 的旧批次，其未 ACK 终态不再绑架新 processing；原 command、真实 ACK、owner binding、已提交结果与 OutcomeUnknown 原样保留。缺失关联证据或真正 Unknown mutation 不绕过安全闸。

## 边界与独立线索

- 本次已确认 Grep 成功并持久提交，不能归因于该工具、MCP 或 subagent。
- 日志同时存在 MCP 初始化/订阅失败和 Langfuse OTLP 投递失败；没有证据证明这些错误导致本次中断。
- 现场 WorkState JSON 约 52.5 MB；这只是持久数据大小，不证明其导致此前记录的 CPU/内存异常。CPU/内存问题属于独立事故调查，不纳入本次修复结论。
- 原始只读快照位于本机 `/tmp/peri-p0-work-state-20261006.json`，权限已设为 0600。包含完整请求、工具和会话内容，不纳入仓库；临时文件可能被清理，对外分享须脱敏。

## 修复与验收

### P1 review 补修（2026-10-06）

- Dispatch 批次中途预算拒绝后，先结算已接受调用的成果，再返回预算/控制错误；确定未派发的调用记录 before-effect 取消证据并投影明确取消消息，保持消息配对，不伪造工具执行成果。
- Abandoned processing 保留原 invocation 的成果与 Unknown 结算通道；无 successor 的原身份结算不要求当前 attempt 仍活跃，但保留生命周期、batch/admission 身份、revision 和调用归属检查，不恢复执行。
- 已恢复执行按 admission 关联的 work/batch lineage 放弃 processing、隔离旧终态，不使用原始 claim execution 代替当前恢复 admission，不伪造 ACK。
- Enqueue 首次接纳持久固定 publication 授权；旧 Queued 请求重放不能获得新授权，已授权未完成发布沿用原身份和代际。
- 主会话 FinishAdmission 原子提交 attempt 退出与收尾；原收尾 journal 支持有界确定 NotApplied 重试，Unknown 不绕过，无 journal 不伪造退出证据。
- 验证仅针对上述路径；真实会话、远端网络及最终 E2E 仍由用户验收。Review 的独立 P2（普通 publication CAS 激活失败、Recovery 预算覆盖 resume_stage）不纳入本轮 P1 已修复声明。
- [x] P1 补修定向回归共 50 项通过：types work 26、Dispatch 预算与结算竞态 4、mailbox staging 15、ACP 原子收尾与恢复 5；未执行全 workspace 或真实模型 E2E。格式与差异空白检查通过。

- [x] 对推理预算、工具派发预算及冷加载预算阻塞提供 typed `WorkBudgetExhausted`；anyhow 边界保留预算错误，其他未知错误继续脱敏。ACP wire kind 保留 `internal`，公开消息不再退化为通用内部错误。
- [x] 用户可见消息明确已耗尽的预算、已用/上限及需要显式授权；保持失败/Blocked，不伪装成功。
- [x] 回归验证：恰好达到上限后的下一次请求不发送模型/执行工具；Block 状态与原预算保持持久化；安全错误投影保留可核对原因。
- [x] 按本轮用户授权统一有界默认预算，并在成功显式选择时升级完整旧默认策略；不自动 reset、不重放已提交工具。用户继续以新任务承接历史上下文，不复活旧 execution。
- [x] 修复预算耗尽 → 错误收尾清 attempt → 显式新输入仍被旧 Blocked work 阻止的闭环；真实临时 SQLite 回归确认新输入候选出现、旧 work 被 Abandoned、旧 counters 保留、重放不再次变更。
- [x] 针对性验证均退出码 0：Agent 预算回归 9 passed；types session 回归 15 passed；Agent work_recovery 6 passed；Agent work_ledger 6 passed。使用 `scripts/cargo-rmcp-patched.sh test --locked --offline` 的 crate `--lib` 定向过滤验证，未宣称全 workspace 测试通过。
- [x] 本轮仅跑 16 个关键用例，全部退出码 0：预算 policy 3、显式选择 reducer 4、旧终态候选隔离 3、低限额 Reason/Dispatch 边界 2、真实存储 mailbox 与忙时隔离 3、安全公开预算消息 1；未跑全 workspace、E2E 或真实用户会话。
- [ ] 用户最终验收：重新编译启动后，在原阻塞会话发新输入；确认历史正常、新任务执行、旧工具不重放，并验证长任务超过原 64 次仍可推进、取消与忙时排队正常。

事实源入口：`peri-acp-types/src/session_resources/work.rs`、`work/policy.rs`、`work/user_input.rs`、`work/query.rs`、`peri-agent/src/session/user_input_mailbox/staging.rs`、`peri-agent/src/agent/stages/work_reason.rs`、`work_dispatch.rs`、`peri-acp-types/src/error.rs`、`session/execution.rs`。
