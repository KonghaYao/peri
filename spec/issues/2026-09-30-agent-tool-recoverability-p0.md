# P0：Agent 系统工具可恢复性闭环

状态：**待用户审批（Pending User Approval）**；尚未批准实施，审批前不启动修复或修改现行契约。

优先级：**整个任务为 P0**（用户指定）。下文 P1/P2 是 SCAN 时的分项风险判断，不降低整体任务优先级。

日期：2026-09-30。

来源：三路 subagent 并行只读 SCAN，以及主 Agent 对工具执行与会话持久化链路的核对。

## 一、问题与目标

可恢复性是 Agent 系统的根本原则：工具失败、取消、超时或计算实例消失后，已经产生的工作不能仅随进程内状态消失，用户或 Agent 应能找到原执行、核对结果，并得到具体恢复入口与提醒。

当前存在局部机制，但尚未形成系统级保证：Write 有同实例草稿恢复，subagent 有合作式中断恢复，Bash 有日志与部分后台晋升机制，workflow 有成功 journal 前缀复用；这些机制仍有跨进程寿命、身份丢失、恢复准入和副作用未知等缺口。

本任务拟建立的用户可观察底线：

- **保留成果**：完整待写内容、已产生输出、已完成步骤或恢复所需输入通过持久能力保存，不仅放在实例内存中。
- **保留身份**：原调用、child session、task 或 run 能在重建计算实例后定位；失败和取消不能仅删除查询入口。
- **提供入口**：可查询状态、读取保留成果，并明确继续原任务、恢复写入或核对外部副作用的操作。
- **具体提醒**：错误说明保存了什么、恢复标识是什么、下一步如何操作，以及尚未确认的状态。
- **安全恢复**：区分确定失败与结果未知；恢复不能盲目重放可能已经生效的副作用。

上述内容是待审批目标，不表示当前实现已经满足，也不表示存储接口或恢复协议设计已获批准。

## 二、范围与边界

范围覆盖 Agent 工具执行公共链路、Write/Edit、subagent、workflow、Bash/后台任务，以及远端 MCP 调用的失败和恢复语义。

Cron、LSP、WebFetch/Artifact 等其余工具纳入后续恢复矩阵核查；本轮只做入口级或有限路径检查，不据此宣称通过。

边界与相关任务：

- 持久状态归 Peri 持久能力，工具执行环境可独立于计算实例驻留；不能默认由 Agent 宿主本地路径保存所有恢复载荷。
- 工具环境经 MCP 消费，用户可见恢复语义沿现有 ACP 出口交付；不建立旁路恢复入口。
- 与 [Session ID 恢复与机器环境分区](../../docs/design/session-id-environment.md) 及 [核心改动任务](2026-09-30-session-id-environment-core-change.md) 协同，实施前按迁移后的代码重新核对。
- 不借本任务重新引入已批准删除的 session owner、文件锁或 dirty 认领门槛；receiver 缺失也不能单独证明旧执行已经结束。
- 无法保存恢复载荷时必须明确报告恢复不可用及保留范围，不虚构恢复 ID、不暗示完整成果已保存。
- 本任务不要求对任意外部副作用实现 exactly-once；不具备查询或幂等能力时，必须保留结果未知状态和人工核对信息。

## 三、SCAN 发现

以下为读取时快照。除 F4 的现行批次提交行为外，本轮未执行真实强杀、断电或远端故障注入；触发场景与后果由代码及已有测试推导，审批后须补确定性复现。

### F1 / P1：Write 失败草稿不能跨进程恢复

- **触发**：文件提交发生 I/O 错误，拿到 `from_draft` 提醒后重启进程或重建 workspace 工具实例，再使用原草稿 ID 恢复。
- **现状**：提交失败会尝试删除临时文件，完整调用内容与 append 标记保存到工具实例的内存草稿；新实例找不到旧 ID，需要重新提供内容。
- **证据入口**：`mcp-packages/workspace/src/filesystem/transaction.rs` 的 `guard_and_commit`；`mcp-packages/workspace/src/filesystem/draft.rs` 的 `DraftStore`；`mcp-packages/workspace/src/filesystem/write.rs` 的失败保存与 `restore_write`。
- **待实现行为**：保存完整恢复载荷、目标信息与写入语义；重建实例后可发现并按稳定 ID 恢复。目标已经改变或原工具环境不可用时明确拒绝或要求核对，不能静默写入当前机器的同名路径。

注意：当前并不是“Write 报错后保留临时文件并提示其路径”。临时文件服务于原子替换，恢复入口实际依赖内存草稿，二者不能混为一谈。

### F2 / P1：subagent 异常退出后，残留 active 阻断原会话恢复

- **触发**：已有持久工作后强杀宿主并重启；或取消后台 subagent 超过宽限期，触发强制 abort；随后以原 `resume_thread_id` 恢复。
- **现状**：进程内注册表与 receiver 消失，但持久状态可能仍为 active。恢复入口仅凭该状态拒绝；底层提示包含另建 Agent 的建议，未提供原会话恢复闭环。强制 abort 分支可能丢失异步状态收尾。
- **证据入口**：`peri-middlewares/src/subagent/tool/execute_resume.rs`；`peri-agent/src/session/subagent/factory/claim.rs` 的 `validate_thread`；`peri-agent/src/agent/async_tasks/registry.rs` 的取消路径；`peri-agent/src/session/subagent/lifecycle.rs`。
- **待实现行为**：按迁移后的执行生命周期能力对账；旧执行结束可被确认时，允许原 child ID 继续。不能把 receiver 缺失直接当作死亡，也不能让陈旧 active 永久阻断恢复；错误必须保留 child 身份与可操作入口。

### F3 / P1：workflow 运行中崩溃缺失完整恢复输入，且不可发现

- **触发**：带 args、并发数和执行限制启动 workflow，已有部分 journal 后，在进入终态收尾前强杀宿主。
- **现状**：初始化只保存脚本，完整 `RunState` 在终态写入；没有 `state.json` 的目录不进入持久历史枚举，正常恢复入口又必须先读取 state。
- **证据入口**：`peri-workflow/src/journal.rs` 的 `init_run`、`list_runs`、`read_state`；`peri-workflow/src/runner/terminal.rs` 的终态保存；`peri-middlewares/src/workflow/mod.rs` 的 `resume_workflow`。
- **待实现行为**：开始执行前保存完整启动清单；冷启动可发现未终结 run，保留原参数与成功前缀，并提供具体恢复入口，而非要求用户重新拼接脚本与参数。

### F4 / P1：工具已完成，但执行证据仍等待整批提交

- **触发**：同批快工具已完成并产生副作用，慢工具仍等待，此时进程崩溃。
- **现状**：dispatch 先收集整批结果，再 stage/commit transcript；快工具先发出的 Render 事件不是持久恢复记录。当前会话提交路径可能没有这批调用与结果的证据。
- **证据入口**：`peri-agent/src/agent/stages/tool_dispatch.rs` 的 `dispatch_tools`；`peri-agent/src/agent/stages/tool_dispatch/execution.rs` 的并发收集；`peri-controller/src/controller.rs` 的事件发布；现有 `test_dispatch_emits_fast_completion_before_atomic_batch_commit`。
- **待实现行为**：在不破坏规范 transcript 的批次语义及 middleware 顺序的前提下，保留执行前身份与恢复必要输入、逐项完成证据；中途崩溃后可区分已完成、未开始和结果未知，不能直接重放整批。

### F5 / P1：远端 MCP 超时未表达结果未知，也未保留核对身份

- **触发**：远端副作用已经发生，但响应超时；远端忽略取消或在取消到达前已提交。
- **现状**：客户端发出有界取消后返回普通 Timeout；错误不携带请求 ID，也不说明取消未获确认、远端可能已执行及不要盲目重试。本轮未发现该层自动重放，风险是恢复提示不足引发后续重复调用。
- **证据入口**：`peri-middlewares/src/mcp/tool_request.rs` 的 `call_tool`；`peri-middlewares/src/mcp/tool_bridge.rs` 的 `ToolCallError::Timeout`；`peri-middlewares/src/mcp/tool_bridge_test.rs` 的超时及服务端继续执行场景。
- **待实现行为**：保留可核对调用身份，明确结果未知；有远端查询能力时先查状态，没有时提供人工核对信息。取消通知不能被呈现为远端已停止或副作用已回滚。

### F6 / P1：后台 Bash 的期限未覆盖完整执行

- **触发**：后台命令的 shell leader 提前退出，但后代继续持有 stdout/stderr，例如 `sleep 600 & printf launched`，并指定显式 timeout。
- **现状**：timeout 只包裹 leader 的 `wait()`；其返回后，输出 reader 的等待不再受该期限约束，任务不能按预期进入可核对终态。
- **证据入口**：`mcp-packages/common/src/shell_executor.rs` 的后台 wait 与输出排空；`mcp-packages/workspace/src/terminal_background_test.rs`；`mcp-packages/workspace/src/terminal_lifecycle_test.rs`。
- **待实现行为**：期限覆盖完整执行与管道排空；到期保留原 task ID 和日志，区分请求清理与确认退出，提供查询入口。已有前台场景不能替代后台路径回归。

### F7 / P2：Edit 提交失败不保存完整修改结果

- **触发**：目标可读取且替换匹配，但提交因权限、空间或其他 I/O 问题失败。
- **现状**：原文件受到原子替换保护，已生成的完整修改结果却随调用结束丢弃；提醒只要求检查环境并 Read 后重试，没有草稿入口。
- **证据入口**：`mcp-packages/workspace/src/filesystem/edit.rs` 的两条 `guard_and_commit` 失败分支；共用 `transaction.rs`。
- **待实现行为**：保留完整修改结果和原文件版本证据，提供恢复 ID；恢复前检查目标是否变化，避免覆盖后续修改。

### F8 / P2：后台 shell 取消后，原任务 ID 不再提供结果查询

- **触发**：启动持续输出的后台 shell，取消后再以原 task ID 查询或等待最终清理结果。
- **现状**：取消先删除 registry 条目，再请求终止；最终收尾无法 claim completion，直接返回。日志文件虽保留，取消事件不提供最终输出引用、退出信息或清理状态。
- **证据入口**：`peri-agent/src/agent/async_tasks/registry.rs` 的取消路径；`peri-agent/src/agent/async_tasks/shell.rs` 的 `finalize_bg_shell`。
- **待实现行为**：取消请求与清理完成分开表达；保留可查询终态和日志引用。取消原任务不应迫使用户重新执行命令才能取回结果。

## 四、已有能力与验证局限

已有能力，不应在修复时退化：

- Write/Edit 使用同目标锁与临时文件 rename，避免直接截断原文件；Write 同实例恢复再次失败会保留草稿 ID，成功后才消费。
- 同步 subagent 合作式中断返回 child ID 与具体恢复调用；已有测试保护完整工具轮次重放，不重复已完成副作用。
- 前台 Bash 超时可晋升同一进程为后台任务，提供 task ID、日志及避免重复运行的提醒；后台输出从执行开始落盘。
- workflow 有成功 journal 连续前缀复用，完整终态 state 存在时可恢复原输入。
- MCP 超时或请求 drop 会发出有界取消通知，但这不等于远端完成确认。

本轮实际验证：

| 项目 | 执行结果 | 能证明什么 |
| --- | --- | --- |
| `cargo test -p peri-agent --lib -- test_dispatch_emits_fast_completion_before_atomic_batch_commit` | 1 passed / 0 failed | 证实现行行为：快工具先展示完成，整批结束前 transcript 不提交；不是强杀恢复验证 |
| `cargo test -p peri-mcp-workspace --lib -- filesystem::write::draft_tests::write_draft_receipt_can_restore_without_resending_content --exact` | 退出 101，实际执行 0 个用例 | 依赖编译错误阻断，不能宣称草稿恢复测试通过 |
| 其余发现 | 静态代码及已有测试审查，未实跑 | 提供待复现故障链，不构成跨进程验收 |

审查期间 `peri-acp-types/src/workspace.rs` 与 `peri-resources/src/sessions/*` 等有外部并发迁移改动。本 issue 不产生或归因这些改动；实施前必须基于合并后快照复核，特别是 subagent 状态与执行准入链路。

额外测试盲区：Write 现有名为 rename 失败的场景使用目录作为目标，可能在读取目标时提前失败，不能据此证明 rename 失败后的恢复行为；Write/Edit 原子替换不能等同于断电持久性。

## 五、待审批实施方向

以下是候选顺序，不是已批准施工计划：

1. **失败写入完整恢复**：先贯通 Write 的持久载荷、稳定 ID、恢复提醒与重启恢复，再覆盖 Edit 的版本校验和成果保存。
2. **原 subagent 恢复**：贯通异常退出、超时 abort、状态对账、原 child ID 继续及提示；与 Session ID/env 迁移后的契约一致。
3. **原 workflow 恢复**：贯通启动清单、未终结运行发现、原输入恢复与成功前缀复用。
4. **公共执行证据与未知结果**：贯通批次中途崩溃恢复证据、MCP 结果未知提醒及核对入口，维持规范 transcript 和 middleware 契约。
5. **后台执行终态闭环**：贯通完整执行期限、取消后查询、日志取回与清理状态。
6. **补齐工具恢复矩阵**：其余工具逐项说明有无副作用、保存成果、恢复身份、失败/取消/超时/重启行为和验收证据；没有必要恢复载荷的工具也须给出理由。

已有 Write 与 subagent 等真实用例可作为共同恢复语义的依据，但不预先指定一个统一框架、表结构或接口；抽象范围、持久位置与协议投影需在审批后设计并验证。

## 六、验收目标（待审批）

- [ ] Write 提交故障后完整内容与 append 语义可恢复；进程退出、工具实例重建后同一恢复 ID 仍可发现和使用，且不会重复追加。
- [ ] Edit 提交故障后可取回完整修改结果；原文件变化会被检测，不静默覆盖后续修改。
- [ ] subagent 强杀重启和取消超时后，旧执行结束可被确认时能以原 child ID 继续；存活执行不会因本机 receiver 缺失被盲目重复启动。
- [ ] workflow 在启动、部分成功与终态提交前分别中断，冷启动均能发现原 run、读取完整启动输入，并只复用可确认的成功前缀。
- [ ] 同批快工具完成、慢工具等待时中断，恢复后能核对已完成成果；未确认结果不会被当作未执行而自动重放。
- [ ] 远端 MCP 已执行但响应超时时，呈现结果未知、稳定核对身份与具体行动提示；取消未确认不能呈现为确定停止。
- [ ] 后台 Bash 的期限覆盖后代持有管道场景；取消后原 task ID 仍能查询清理状态、终态及保留日志。
- [ ] 恢复载荷保存失败、原工具环境不可用或结果无法确认时，错误明确说明限制，不伪造恢复承诺或切换到当前环境执行。
- [ ] 恢复载荷具备访问边界和明确保留/清理策略；提示、日志与协议投影不泄露敏感输入，不随意删除仍需恢复的成果。
- [ ] 对适用本机/远端后端和执行环境完成契约验证；与既有 Session ID/env 迁移、父子关系、取消和资源关闭语义保持一致。
- [ ] 每项高风险故障有确定性回归，跨进程恢复有真实退出/重启验证；报告实际通过数、未验证平台与限制，不把测试启动或 0 tests 当作通过。
- [ ] 交付时同步受影响 standards、模块指引与 code-index；只有用户审批且验收完成后才能关闭本任务。

## 七、用户审批门禁

- [ ] 用户批准系统级恢复底线、范围与验收目标。
- [ ] 用户批准与 Session ID/env 迁移的边界；不以新增恢复锁替代已批准删除的旧门槛。
- [ ] 明确恢复载荷存储归属、寿命与清理策略，以及副作用未知时允许的恢复动作。
- [ ] 批准实施顺序及必要的后续切片；批准前本 issue 保持待用户审批，不进入自动施工队列。

本次授权仅创建 P0 issue 并等待用户审批，不包含修复实现、修改协议/架构标准或发布可自动领取的实施子任务。
