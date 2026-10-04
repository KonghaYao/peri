# [P0] bg 回执信息投递缺陷批次（subagent 断裂为其中一例）

**状态**：实施中，批次 1 核心、2.1、2.3、3.1、§7.3 有界等待（含 parked-idle 定时器修复）、§7.4 身份条目已落地主工作区（未提交）；4.2（workflow agent MCP 调用）由并行 owner 落地。批次 5 余项（e2e 实跑）与 ACP 响应层 `pending:n` 未完成。已完成：终态提醒按**投递归属**（直接发起会话）经 `TaskTerminalDelivery` 原子提交到其 canonical transcript（幂等、可重投，MQ 只唤醒；子会话不再断裂），冷恢复降级 root 投递并在 metadata 标 `root-fallback`；回执按可达性诚实承诺；lost 有界重试后 `abandon_external` 产出终态并标注远端副作用未知；`scope.uncertain` 改为按执行 scope 记录、仅由该 scope 的对账证据清除。§7.3 上限 120s，到点把 `pending: N`+任务身份+scope owner 原子写入归属会话 transcript（ACP 响应层的 pending 标记未接线）。批次 5 仅剩 e2e 实跑。架构裁决仍为方案 A（三维分离）+ D1–D4；权威设计为 `docs/design/session-async-tasks.md` §7，H1 证据见[设计提案](2026-10-04-async-task-ownership-design.md) §10。
**优先级**：P0（最高）。**权重提升**：3.1（scope.uncertain 无清除，会话锁死）与 2.1/2.3（结算静默丢弃）由 P1 提升为 P0；4.2 待实验，若成立即 P0
**类型**：任务回执语义 / 投递路由 / 任务闭环（结算-取消-销毁）
**创建日期**：2026-10-04
**来源**：2026-10-04 subagent 工具链测试（HEAD `3a999a1e`，macOS arm64）；三路只读代码审计（投递链路一致性 / 场景枚举 / 文案与获取途径）
**最后核查**：2026-10-04（运行时实验 6 组 + 代码审计静态定位；审计行号未逐项运行时复核处已标注；野生实例 1 例——超时提升路径自然复现，见 §一）

## 判定

bg（后台任务）回执信息投递错误是一类缺陷，不是单点：

1. 回执向发起者承诺投递（"Completion is delivered by MCP Tasks subscription."），实际按 owner session（subagent 场景 = root）注册订阅与投递，发起者不可达；
2. subagent 会话**结构上没有接收通道**（无 inbox、无 session_tasks 绑定）；
3. 模型面对后台任务只有被动 push 一条路，无任何查询/取结果/取消途径，回执中的 id 对发起者不可用；
4. 结算、丢失、取消、销毁路径存在多处静默丢弃与永不收敛，且文案/文档与实际能力冲突。

### 最高优先（权重提升，建议先修）

1. **1.1 / 1.2**：回执承诺投递与实际投递断裂；subagent 无接收通道（结构性——"投不了"，而非"投错了"）。
2. **3.1**：MCP 调用取消/超时 → `scope.uncertain` 永久置位 → owner 会话无法 close、fork 永久被拒（确定性、可静态闭环定位，不依赖产品裁决）。
3. **2.1 / 2.3**：结算静默丢弃（lost 无终态且无限重试；`settle_external` 返回 false 当作成功）→ 结果永久丢失且无感知。
4. **4.2**：workflow 内 agent 的 MCP 调用疑似直接失败（接收者不存在；待实验，成立即 P0）。

## 一、已实证（运行时实验，6 组对照）

| # | 执行者 | 调用 | 结果 |
| - | - | - | - |
| 1 | 父 agent | 前台 `sleep 5` | 同步返回 |
| 2 | 同步 subagent | 前台 `sleep 5` | 同步返回 |
| 3 | 后台 subagent | 前台 `sleep 5` | 同步返回 |
| 4 | 后台 subagent | 显式后台 `sleep 5` | `mcp-` 回执；通知只到 root，发起者永久缺失 |
| 5 | 后台 subagent | 前台 `sleep 20`（超时提升） | `mcp-` 回执；通知只到 root，发起者永久缺失 |
| 6 | 父 agent | 显式后台 `sleep 8` | `mcp-` 回执；完成通知回到自身会话（闭环已实证） |

区分变量是"命令被后台化 + 订阅归属 owner"：**同一条 MCP Tasks 路径，主会话（owner=自身）闭环，subagent（owner=root）断裂**。当事 subagent 两次 resume 复核确认通知从未到达。

### 野生实例（自然发生，非受控实验；2026-10-04 17:20–17:50）

只读审计 subagent（thread `01a10637-56b7-7c50-b7b1-4539589b7df7`，title=code-reviewer）在真实审计工作中自然触发，是实验 4/5 路径在无人为构造条件下的复现：

- 任务提示明确"禁止 run_in_background"（只允许快速只读命令），其三条仓库级慢 grep（未排除 `target/`，如 `grep -rln … .`）相继超过 15s 前台超时，被**静默提升**为后台任务（tool 消息 `01a1063b-25bb / 01a1063b-6b08 / 01a1063b-ec52`，回执 `mcp-2a62eefc… / mcp-04272ca1… / mcp-04cc79ed…`）——提升不感知也不受发起者意图约束（补强 1.7）。
- 发起者从未收到完成通知：其 17:28:03 结束，三个任务 17:49:06–17:50:22 才完成（晚 21 分钟）；resume 与查询途径均无。
- 完成通知进入 root（本会话）transcript：system_reminder `fb864ea5… / a4d3fb7e… / 246efda1…`（category=task, source=shell）；root 无归属信息可处置，命令上下文存于 child transcript。
- 级联效应（补强 1.7/5.1）：提升回执无"命令超时被提升"说明，也无输出/pid/日志路径；该 subagent 正在审计回执文案，遂把自己的回执误读为"grep 命中的文件内容"（其 reasoning 原话："the output is exactly the receipt text with mcp-2a62eefc… a real task id hash, and it appears as a standalone line"），继而触发更多慢命令（共 3 次提升），最终将症状归因为 "harness projection weirdness" 而未上报。
- 同时验证 §5 约束方向：child 结束后其 root scope 任务未被取消，继续运行至完成（`docs/design/session-async-tasks.md` §5）。
- 该实例即设计提案 §10 H1 场景的自然复现：发起者已结束，投递若落其 canonical transcript（幂等、非活跃可行）再经祖先聚合投影即可正确归属；现状则落入 root 而无从关联。

## 二、批次清单

> 证据列中带 ✅ 的为运行时实证；其余为静态代码审计定位（`file:line` 为当前工作区磁盘内容，未逐项运行时复核）。

### 批次 1：回执语义与投递目标（P0，核心）

| # | 问题 | 证据 |
| - | - | - |
| 1.1 | 回执承诺投递，实际按 owner 投递；subagent 发起者结构上不可达 | ✅ 运行时实证；`peri-middlewares/src/mcp/tool_bridge.rs:291-295,435-445`；`client/subscription_tasks.rs:569-597,680-693` |
| 1.2 | subagent 无接收通道：`session_inboxes`/`session_tasks` 仅由 ACP SessionManager 注册，child 不经过 ACP host | `peri-middlewares/src/mcp/client.rs:869-885`；`peri-acp/src/session/bridges.rs:77`；`peri-acp/src/host/requests.rs:222-238` |
| 1.3 | 任务 id 与完成通知不含发起者身份：id 输入为 (owner session, server/source)；通知 metadata 仅 server/task_id | `peri-agent/src/agent/async_tasks/manager.rs:33-44`；`subscription_tasks.rs:742,791-799,943-954` |
| 1.4 | 回执信息缺失：无 pid、无日志路径、无输出，与 Bash 工具描述承诺（task_id + pid + log paths）直接冲突 | `tool_bridge.rs:443-445` vs `mcp-packages/workspace/src/descriptions/bash.md:19,39`；`peri-acp-types/src/tasks.rs:133-151` |
| 1.5 | 回执 id 对发起者不可查询/不可取消：模型面无任务工具；ACP `cancel-bg-task` 需 owner sessionId 且 session 已注册 | `peri-acp/src/host/requests/session_lifecycle.rs:565-591,593-603`；`peri-acp-types/src/meta_harness.rs:228-250` |
| 1.6 | 订阅 admit 失败（OwnerClosed/DuplicateKey）仅 warn，回执照发承诺投递 | `tool_bridge.rs:435-445`；`peri-middlewares/src/mcp/task_scope.rs:283-311`；`subscription_tasks.rs:673-677` |
| 1.7 | 超时提升回执语义误导：说"started"，实际已运行 ≥15s；前台已产生的输出被丢弃（回执不得携带输出被测试锁定） | `mcp-packages/workspace/src/workspace.rs:266-281`；`peri-tui/tests/print_background_exit.rs:152-171` |
| 1.8 | id 前缀三套并存（`shell-` / `mcp-` / `bg-`），e2e 提示语要求"回复 shell- 开头" | `terminal.rs:413`；`workspace.rs:240`；`tool_bridge.rs:444`；`manager.rs:43`；`e2e/tests/subagent/bg-task-area.test.ts:87` |

### 批次 2：结算与丢失路径（P0 提升项 + P1，投递的健壮性基础）

| # | 问题 | 证据 |
| - | - | - |
| 2.1 | **[P0]** `mark_external_lost` 只改投影，不设终态、无放弃语义，2s 无限重试 → 任务永为 active | `peri-agent/src/agent/async_tasks/registry.rs:364-397`；`subscription_tasks.rs:653-657` |
| 2.2 | 订阅循环以 Weak 失效为唯一退出，break 后无恢复；恢复协议（snapshot/reconcile）仅覆盖 workspace，第三方 MCP 无 | `subscription_tasks.rs:587-597,230-277,407-484` |
| 2.3 | **[P0]** `settle_external` 返回 `Ok(false)`（无通知/无条目）被当成功，订阅随即 break → 静默丢弃 | `subscription_tasks.rs:680-693`；`manager.rs:376-398` |
| 2.4 | `DuplicateTask` 注册返回 Ok 但未建条目，回执照发；结算永远失败 | `registry.rs:475-477`；`manager.rs:329-337` |
| 2.5 | 非 workspace 任务幂等键是常量 `"{raw}:terminal"`，无法区分重放与真实新终态 | `subscription_tasks.rs:625-627` |
| 2.6 | `restore_external_terminal` 忽略 settle 的 false 返回；"先投递后写投影"非原子 | `manager.rs:340-374` |
| 2.7 | reconciliation 头阻塞：单条不可投递记录使 cursor 不推进，整台 server 变更反复重放 | `subscription_tasks.rs:315-330,341-355` |
| 2.8 | 关闭协议不对称：workspace 有 taskClose/epoch 规范关闭，第三方依赖 peer 在线 | `subscription_tasks.rs:407-484` vs `390-399` |

### 批次 3：取消、销毁与执行收敛（P0/P1，含独立确定性缺陷）

| # | 问题 | 证据 |
| - | - | - |
| 3.1 | **[P0]** `scope.uncertain` 单调置位、无清除路径 → owner 会话永久非 idle：shutdown 恒 Incomplete、fork 被拒（"Cannot fork while source execution is active"） | `peri-agent/src/agent/async_tasks/scope.rs:82-115`；`manager.rs:139-141,233-250`；`session_lifecycle.rs:659-670` |
| 3.2 | guard 绑定 owner(root) scope：child 内被取消/超时的调用污染 root 执行证据；guard 不携带 task id | `tool_bridge.rs:296-320,352-361`；`peri-agent/src/agent/stages/tool_dispatch/execution.rs:337-344` |
| 3.3 | owner manager 缺失/scope closing 时**所有** MCP 调用（含纯同步）在发送前失败，错误信息误导 | `tool_bridge.rs:296-320`；`client.rs:351-361` |
| 3.4 | 响应丢失/调用中断时远端可能已建任务：本地无登记、无订阅、无取消句柄 → 孤儿 | `client/output_store_test.rs:79-109`（锁定行为）；`tool_request.rs:39-94` |
| 3.5 | Task 注册成功即 `confirm_stopped()`：本地"已停止"与远端"仍在运行"双轨分裂 | `tool_bridge.rs:409-411`；`scope.rs:97-115`；`registry.rs:746-755` |
| 3.6 | `cancel_all` 只取 `local_task_ids()`（显式排除 External）→ 会话销毁不取消外部任务；依赖 close_workspace_task_scope 成功 | `manager.rs:491-502,240-249`；`registry.rs:757-776` |
| 3.7 | 取消 child（subagent）不取消其任务：任务登记在 root，取消需 root sessionId | `peri-acp/src/session/mod.rs:341-346,359-374`；`session_lifecycle.rs:565-591` |
| 3.8 | cancel 路径无超时：`cancel_async` 闭包内 `peer.cancel_task(...).await` 无 timeout 包裹 | `subscription_tasks.rs:917-939` vs `tool_bridge.rs:567-569` |
| 3.9 | close 循环对已 lost（peer 离线）任务仍无条件 cancel → Err 冒泡，会话无法关闭 | `subscription_tasks.rs:390-392` |
| 3.10 | 取消成功后本地条目保持 Running，终态依赖订阅投递；投递失败即永 active | `registry.rs:352-362,623-653` |

### 批次 4：场景扩展（待实验确认）

| # | 场景 | 问题 | 证据 |
| - | - | - | - |
| 4.1 | 嵌套 subagent | owner 仍解析到最顶层 root；中间层同样收不到；通知无层级信息 | `peri-agent/src/session/subagent/factory.rs:31-53`；`factory/spawn.rs:228-229`；`factory/resume.rs:119-131` |
| 4.2 | workflow 内 agent（in-process） | **✅ 成立（即 P0），且范围大于静态定位：所有 MCP 工具（含前台同步）在发送前失败**。`session_context` 为空 → 归属 id 回退为内部 `AgentId`（池中无注册）→ `begin_external_task_execution` 失败。运行时（假模型驱动真实 workflow）：workflow agent 的 Bash/Read/WebSearch 全部返回 `MCP 服务器 "<server>" 工具 "<tool>" 调用失败: session task manager unavailable`（workspace 与 web 两类来源）；对照组主 agent/同步 subagent 正常拿到 `mcp-` 回执 | ✅ exp-4.2/4.2b（逐字输出见 §六末证据块）；`peri-agent/src/agent/workflow/agent.rs:448-460`；`v2_bridge.rs:284-299`；`client.rs:351-358`；`host/workflow_agent.rs:219`（`subagent_ctx_builder: None`）；修复实施：workflow 执行体（`peri-agent/src/agent/workflow/agent.rs:466-475`）把宿主装配注入的 root session ID 写入现有 `mcp_task_owner_session_id` 上下文，保留 agent 自身身份、不新增 shim；隔离 worktree（`.tmp/verify-4.2`，detached `a40e2607`）定向测试 `assembly::tests::workflow::mcp_owner` 5 passed / 0 failed、`build --locked --workspace` exit 0、`lefthook run pre-commit --all-files` exit 0，全链路 A/B（假模型驱动，同一脚本）pre-fix 逐字复现 `session task manager unavailable`、post-fix workflow agent 的同步 Bash 返回真实输出且 Read 成功（WebSearch 在无网 harness 中为真实执行期失败、非发送前预检失败）。 |
| 4.3 | print 模式 | **主会话维度证伪"通知无消费时机"**：`session/prompt` 响应被持有至会话真正空闲——后台任务完成后提醒触发新 turn，该 turn 结束才返回（运行时 bg `sleep 20`：prompt response +20.56s 返回；提醒 turn 的 last message 为 category=task/source=shell 的 system_reminder，符合 design §3）。close 等待外部任务结算且**无界**（bg `sleep 300`：30s 时进程仍存活，被实验 harness SIGKILL）。**subagent 维度同型断裂仍成立**（child 回执 `mcp-…`，提醒只进 root） | ✅ exp-4.3/4.3d/4.6；`peri-tui/src/cli_print.rs:191-238,382-385`；`session_close.rs:200-215`；design §3 |
| 4.4 | ACP 远端客户端 | **静态确证：首次投递必失败、后续补投**——session 建立后 `recover_workspace_tasks` 立即执行（500ms 超时、错误被 `let _ =` 吞掉），而 inbox 到首 turn 才惰性注册（`stage_builder.rs:137`）→ `deliver_task_reminder` 报 "session inbox unavailable"；因 `on_terminal` 先于投影写入，watch 循环每 2s 重放快照会补投，首 turn 注册 inbox 后成功。**运行时只复现前置条件**：远程 owner 的任务在 host EOF 后存活（marker=true），但裸 CLI 的 `session/load` 被 supervisor 信任门拒绝（"Session restore incomplete: trusted Workspace owner unavailable"），无法驱动恢复场景 | ⏳ exp-4.4；`peri-acp/src/host/requests.rs:268-275`；`bridges.rs:69-80`；`subscription_tasks.rs:230-276,546-567,750-768`；`manager.rs:340-374` |
| 4.5 | 会话销毁两路径 | **部分成立 + 一点证伪**（运行时四组）：(a) 显式 close：真实取消外部任务并成功（close 210ms 返回、后台 shell 进程立即消失、marker 未生成）；(b) EOF（内建同进程实例）：进程退出时任务同样被清理（marker 未生成）——"EOF 不取消外部任务"在"不发 cancel"意义上为真，但内建部署随进程消失（design §4 故障范围）；(c) **远程 owner**：EOF 后任务存活、无 cancel（marker=true）；(d) SIGKILL：任务成孤儿、自然跑完、无回收。"close 失败重试注册已清空"未复现（需故障注入） | ✅ exp-acp-lifecycle2 E1/E2/E3 + exp-4.4 run1；`host/shutdown.rs:40-90`；`session_close.rs:202-223`；`peri-acp/src/session/mod.rs:288-308`；design §4/§5 |
| 4.6 | subagent 结束/resume | **✅ 成立**：child 结束不检查/不等待其发起的未结算外部任务——运行时 child 在 ~0.6s 返回 `child_thread_id: 01a1067c-…` + DONE，其 bg `sleep 8` 继续运行至完成，完成提醒只触发 root 新 turn；resume 不重放/不补投（静态重建路径不枚举未结算任务；既有批次 1 实证：当事 subagent 两次 resume 均未收到） | ✅ exp-4.6；`factory/resume.rs:119-131,228-231`；`manager.rs:233-250,486-497` |

### 批次 5：文案与文档一致性（P2/P3，文案即接口）

| # | 问题 | 证据 |
| - | - | - |
| 5.1 | Bash 工具描述承诺后台回执含 task_id/pid/日志路径（run_in_background 与超时提升两处），实际回执为单一 opaque id | `mcp-packages/workspace/src/descriptions/bash.md:19,39` vs `tool_bridge.rs:443-445` |
| 5.2 | 工具描述承诺模型可 "track that task or explicitly stop it"，模型面无任何任务查询/取消工具 | `bash.md:22`；`meta_harness.rs:228-250` |
| 5.3 | 系统提示语称 AgentResult "只返回已完成结果"，工具实现与自带描述均声明"永不返回、不要调用" | `peri-acp/prompts/sections/11_subagent.md:59` vs `peri-middlewares/src/subagent/agent_result.rs:57-64`、`descriptions/agent_result.md:1` |
| 5.4 | multitask SKILL 把"完成通知必达"当作唯一收敛机制，无兜底 | `mcp-packages/workspace/src/resources/builtin/skills/multitask/SKILL.md:42` |
| 5.5 | 契约文档与 code-index 以 `shell-` 本地回执（含 pid/日志/面板）为模型面事实源，与生产 `mcp-` 回执不符 | `peri-acp-types/src/tasks.rs:133-151`；`docs/code-index/peri-middlewares.md:57-58` |
| 5.6 | UI 指引（Tasks 面板、通知路径）写进面向模型的工具描述 | `bash.md:44-45` |
| 5.7 | e2e 提示语要求"回复 shell- 开头 task_id"（未强断言，但误导） | `e2e/tests/subagent/bg-task-area.test.ts:87` |

## 三、模型面获取途径现状（审计结论）

| 主体 | 完成推送（push） | 查询/取结果 | 取消 | 读取输出 |
| - | - | - | - | - |
| 主 agent（owner=自身） | ✅ | ❌ | ❌（仅 ACP 客户端面） | ✅ 通知带 stdout/stderr 路径 + Read |
| subagent（owner=root） | ❌ 断裂 | ❌ | ❌ | ❌ 回执无路径，root 的 reminder 不进 child transcript |
| TUI（对照） | ✅ `bg-task-*` 事件 | ✅ `session/bg-tasks` 拉取 | ✅ `session/cancel-bg-task` | ✅ 面板/预览 |

结论：模型面对后台任务**只有被动接收一条路**；`task snapshot` / `external_task_ids` / `has_unsettled_external` 均无模型可见面。

## 四、修复批次建议（架构裁决已出）

> 实施进度（P0 owner，未完成整批，未提交）。已落地并测试锁定：
> 1. **批次 1 核心（D1）**：`ToolContext.task_terminal_delivery` 由执行会话的 transcript 持久化句柄构造（可信 binding，不接受模型参数）；`ExternalTaskRegistration.initiator_session_id` 记录发起者；投递先做 `append_reminder_if_absent` 原子提交（稳定 delivery ID、幂等、崩溃可重投），再做 MQ/收件箱唤醒；冷恢复无发起者时降级 root 并标 `delivery: root-fallback`；回执按路由可达性区分「持久送达」与「活跃期送达」，不再声称订阅必达。
> 2. **1.6**：monitor 准入失败向调用方返回诚实错误（任务已存在、通知不保证、禁止盲目重跑）；`DuplicateKey` 复用已有 monitor。
> 3. **2.1**：`LOST_ABANDON_ATTEMPTS`（300 次 2s 轮询 ≈10 分钟）后 `abandon_external` 产出 failed 终态，提醒明确「远端副作用未知，需直接核对 owner」。
> 4. **2.3**：`settle_external` 对缺失路由、正在结算、投影提交失败返回错误；只有终态投影已存在时返回 `Ok(false)`；`deliver_managed_task_status` 对 false 也检查终态快照，不再静默退出观察。
> 5. **3.1**：`ExecutionScope` 以 (guard id, 执行 scope) 记录不确定，`resolve_external_execution_evidence(scope)` 仅清除该 scope 的记录；Workspace scope 快照完整应用（含 close 的 epoch/barrier 对账）即证据，其他 owner 的证据不得顺带清除。
>
> 6. **§7.3 有界等待（含验收修复）**：`HANDOFF_MAX_WAIT = 120s`，`BoundedWait` 自本会话首次出现未结算任务起算（tokio 时钟域）；idle 挂起 `select!` 增加 `sleep_until(deadline)` 定时器分支——**界自身唤醒 loop**，不再依赖偶发唤醒（验收发现的 parked-idle 缺陷：原先界只在恰好被唤醒时重新求值，`sleep 100000` 实测无界）。到点回退出判断，`write_pending_handoff` 把 `pending: N`、任务身份与 scope owner 原子写入归属会话 canonical transcript（稳定投递 ID、幂等；刻意不唤醒队列，供下一轮/resume 消费），覆盖 print/close 的无限等待。 回归 `stages::bounded_wait_exit_tests`（paused 时间；禁用定时器分支即失败）；行为复跑（重建二进制 + replay 假模型）：`sleep 100000` + 200s 上限 → **123.5s exit 0**（修复前 200s SIGKILL），`sleep 300` + 360s 上限 → **120.8s exit 0**（修复前 301.23s）。
> 7. **§7.4**：`TaskRecord.initiator_session_id`（serde 可选，快照/增量透传，root 面板据此归属子会话发起的任务）；`BackgroundTask` 记录 initiator/owner_session/owner_identity 供交接与展示。
>
> 验证（冷构建，target 曾被用户清空）：`check --workspace --all-targets` 通过、`lefthook run pre-commit --all-files` 全绿（fmt/check/clippy/typos/layer-imports）；Agent `agent::async_tasks` 64 passed；`peri-agent` 全量 877 passed / 1 failed；`peri-mcp-workspace` 422 passed；`peri-acp` 715 passed / 10 failed。失败集合与干净基线 a40e2607（715/10）**逐条一致**——10 个 ACP 失败（`-32010` owner/admission 家族与 provider 配置持久化）+ 1 个 agent provenance 失败均为既有缺陷，非本轮引入；首次全量中多出的 `test_update_config_切换provider后cfg_provider更新` 是磁盘满窗口的临时写失败（健康磁盘下单独复跑 3/3 通过，且此后全量与基线同集合）。**仍未完成**：ACP `PromptResponse` 层的 `pending:n` 标记（`pending:n` 现落在交接记录与 tracing）、批次 5 的 e2e 实跑、§7.2 中「不可达不得承诺」在无 store 会话下的完整覆盖。H2 未做运行时验证——静态确认 scope 与 reminder route 分别位于 `ExecutionScope` 与 `SessionTerminalDelivery`/`write_pending_handoff`。

- **批次 1 为核心**：先定义 bg 任务对"发起者"的投递语义（投递到发起者，或明确拒绝并诚实回执），回执/通知/身份三位一体改；随后测试锁定。
- **3.1 与批次 1 并列为最高优先（P0）**：独立确定性缺陷，不依赖产品裁决，可立即修复（补 `scope.uncertain` 的清除/恢复路径）。
- **批次 2 静默丢弃项（2.1/2.3，P0）与批次 1 同批修复**：丢失必须可观测、可重试、有终态。
- **批次 4 先补实验再定**（4.2 workflow agent 失败形态优先验证）。
- **批次 5 随批次 1 一并改**（文案是接口的一部分，改文案需同步测试与文档）。

## 五、验收（建议）

- 复现矩阵：subagent 内显式后台 / 超时提升；断言发起者获得结果，或回执与实际投递一致且可归属。
- 收敛：一次 MCP 调用取消/超时后，owner 会话仍可正常 shutdown、fork 不被误拒。
- 结算：投递失败可观测、可重试、有明确终态；第三方 MCP 任务有恢复或放弃语义。
- 文案：测试锁定的回执与实际能力一致（同步更新 bash.md / 提示语 / e2e / code-index）。

## 六、未确认 / 需实验清单（批次 4 运行时实验后更新；revision `252bce89`，隔离 worktree）

1. workflow agent 的 MCP 工具调用实际失败行为（4.2）——**已实验：成立，且范围大于静态定位**：workspace 与 web 两类来源的全部 MCP 工具（含前台同步）在发送前即失败（`session task manager unavailable`）。**即 P0**。
2. print 中通知消费时机与 close 失败路径——**已实验**：主会话通知会被消费（`session/prompt` 响应持有至提醒 turn 完成），close 等待任务结算且无界（>30s 未退出，被实验 harness SIGKILL）；close 失败路径未复现（无故障注入）。
3. ACP 首 turn `recover_workspace_tasks` 投递失败后是否补投——**静态确证：首次失败、后续补投**（`recover` 错误被 `let _ =` 吞掉；`on_terminal` 先于投影写入，watch 循环 2s 重放快照不会去重跳过，首 turn 注册 inbox 后补投成功）。运行时仅复现前置条件：远程 Workspace owner 的任务在 host EOF 后存活；裸 CLI 无法越过 supervisor 信任门（`Session restore incomplete: trusted Workspace owner unavailable`）驱动恢复。
4. tasks 能力协商失败时的降级路径可达性（生产客户端恒声明 tasks）——**未实验**：需篡改客户端能力；降级分支回退为工具自身回执（`Background shell task started.\ntask_id: shell-…`），未验证。
5. `DuplicateTask` 的现实触发概率（workspace 侧 task id 生成/清理策略）——**静态结论：≈0（被幂等吸收）**：公共 id = sha256(session_id, owner_identity, raw_task_id)，raw id 为完整 UUIDv7（`shell-{uuid v7}`，同毫秒不碰撞、无复用）；恢复重放（快照+变更重叠）由 `Manager::register_external` 的 DuplicateTask→refresh 分支吸收为 Ok；`task_spawner` 的 DuplicateKey 仅是监控去重（warn）。
6. 3.4 孤儿任务在远端是否有兜底回收——**已实验（部分）**：host 被 SIGKILL 后内建实例的后台 shell 成孤儿并自然跑完（无回收，`psAfterKill` 可见 `sleep 20`）；远程 owner 在 EOF 后任务存活且未收到 cancel。兜底仅存在于"后续会话关闭/恢复对账可达"的路径（design §4），内建部署不提供进程级任务高可用。
7. 多客户端 attach 同一 ACP session 的通知归属——**未实验（不可行）**：stdio CLI 单连接，多连接需自定义 transport/服务部署。静态：inbox/task_manager 按 session_id 单点注册（`bridges.rs:69-80`、`requests.rs:222-238`），通知归属 = 持有执行所有权并注册 inbox 的连接。
8. host EOF 与显式 close 的行为差异是否有意（对照设计文档）——**已实验 + 对照**：显式 close 走 `close_workspace_task_scope`（210ms 内取消外部任务并成功）；EOF 不发 cancel（design §4"单个 Agent loop 或 session runtime 意外退出时，不向 MCP 发送取消"为有意语义），内建同进程实例随进程消失、远程 owner 继续运行。即：取消语义差异有意，后果差异还取决于部署形态。

### 本轮实验环境与证据（可核对）

- 环境：worktree `/Users/konghayao/code/ai/peri-v4p3/.tmp/batch4-exp`（detached `252bce89`，自建 target）；`peri 0.2.0`；全部实验用**假模型 replay**（隔离 HOME + 本地 SSE，无真实凭据/网络）；脚本与请求/进程时间线在 `.tmp/batch4-exp/scratch/`（`exp-4.2.mjs`、`exp-4.2b.mjs`、`exp-4.3*.mjs`、`exp-4.6.mjs`、`exp-acp-lifecycle2.mjs`、`exp-4.4.mjs`，含 `*-requests.json` 请求体与 `acp-lifecycle2.log`）。
- 构建（逐字）：`CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_DEV_INCREMENTAL=false ./scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --bin peri`（+ 4.4 用 `-p peri-mcp-workspace --bin peri-mcp-workspace`）。
- 4.2 关键输出（逐字，模型可见 tool_result）：`MCP 服务器 "workspace" 工具 "Bash" 调用失败: session task manager unavailable`；同批 `Read`、`WebSearch`（web 实例）同错误；对照组主 agent/subagent 回执 `Background task started: mcp-<64hex>. Completion is delivered by MCP Tasks subscription.`。
- 4.3 关键时间线：ACP `bg sleep 20` → `session/prompt` 响应 `{"stopReason":"end_turn"}` 在 +20.56s 返回；print `bg sleep 300` → 30s 时进程仍存活（SIGKILL, ms=30005）。
- 4.5/3.4 关键输出：E1 `session/close (210ms) -> {}`、`ps after close: ""`、marker ABSENT；E2 EOF 后 `peri exit {"code":0}`、`ps after EOF: ""`、marker ABSENT；E3 `ps after SIGKILL` 含 `bash -c sleep 20 …` 与 `sleep 20`、marker PRESENT；4.4 run1 远程 owner marker=true。
- 隔离说明：主工作区未改动任何被跟踪代码文件；本轮实验结果仅回写本 issue 的批次 4 表格与 §六。

## 附：同批次测试的其他观察（轻微，供参考）

1. **agent catalog 快照与运行时不一致**：系统注入的 Available agent types 显示 "No agents currently configured"，但运行时 loader 正常加载 `.claude/agents/` 下的 `advisor` / `code-reviewer` 且均可执行。
2. **Glob 绝对路径 pattern 静默返回空**：`Glob(pattern="/abs/path/**/*.rs")` 返回空且无提示；`Glob(path="<dir>", pattern="**/*.rs")` 正常。
