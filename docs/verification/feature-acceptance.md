# 功能验收映射

> 盘点基线：`cee019f34337e420178be0c2671c98f8c23c4083`，2026-11-18。
> “源码存在”只证明入口存在；本表不将测试文件存在、历史 PR 通过或文档检查通过当成本次运行验收。

功能定义与评级只在 [功能目录](../reference/feature-catalog.md)；本表维护当前实现、源码定位和证据缺口。“相关测试”是已定位的候选证据入口，仍须逐断言确认覆盖范围，不能据此声称整行边界已验证。各行“未复验”指本次文档变更未重跑对应运行行为，并非已确认产品缺陷。所有 L1–L4 的场景 ID、关键断言和固定产品/测试 SHA 对账交 [peri-e2e #10](https://github.com/KonghaYao/peri-e2e/issues/10)，不在 Peri 复制测试编排。L5 没有自动新增 E2E 的义务。

已知证据：#443 已由 [#455](https://github.com/KonghaYao/peri/pull/455) 合并，async-only 已由 [#453](https://github.com/KonghaYao/peri/pull/453) 合并；这是维护者历史验收声明，本次未重跑。History V2 的细项证据与缺口只在 [既有矩阵](history-v2-acceptance.md)。试验性 TS 适配层（`peri-rivet`、`agent-builder`）已于 2026-11-22 移除，F-SDK-008 与 F-RIVET-001..008 退役；原 [#448](https://github.com/KonghaYao/peri/issues/448) 的真实 Engine/24h 证据随该层归档，Peri Swarm 主线的未验收项见[本地开发验收记录](peri-swarm-local-dev.md) 的 `unverified` 清单。内存证据归 [#447](https://github.com/KonghaYao/peri/issues/447)，工具命名与懒加载归 [#449](https://github.com/KonghaYao/peri/issues/449)。#456 尚未合入本盘点基线。

| ID | 实现状态 | 契约 | 证据或缺口 | 验证状态 |
| --- | --- | --- | --- | --- |
| F-CORE-001 | 源码存在 | [契约](../design/architecture.md) | [源码](../../peri-agent/src/agent/stages/react_loop.rs)；[相关测试](../../peri-agent/src/agent/stages/loop_lifecycle_test.rs)；缺口：RCRA 循环：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-CORE-002 | 源码存在 | [契约](../design/architecture.md) | [源码](../../peri-agent/src/agent/stages/reason.rs)；[相关测试](../../peri-agent/src/agent/stages/reason_test.rs)；缺口：模型推理：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-CORE-003 | 源码存在 | [契约](../design/architecture.md) | [源码](../../peri-agent/src/agent/stages/tool_dispatch.rs)；[相关测试](../../peri-agent/src/agent/stages/tool_dispatch_test.rs)；缺口：工具分派：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-CORE-004 | 源码存在 | [契约](../design/architecture.md) | [源码](../../peri-agent/src/agent/stages/react_loop.rs)；[相关测试](../../peri-agent/src/agent/stages/stop_intent_test.rs)；缺口：停止执行：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-CORE-005 | 源码存在 | [契约](../design/meta-harness.md) | [源码](../../peri-acp/src/host/assemble.rs)；[相关测试](../../peri-acp/src/host/executor_flow_frozen_test.rs)；缺口：会话冻结：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-CORE-006 | 源码存在 | [契约](../design/architecture.md) | [源码](../../peri-agent/src/agent/stages/react_loop.rs)；[相关测试](../../peri-agent/src/agent/stages/default_iteration_budget_test.rs)；缺口：执行预算：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-CORE-007 | 源码存在 | [契约](../design/architecture.md) | [源码](../../peri-agent/src/agent/stages/act.rs)；[相关测试](../../peri-agent/src/agent/stages/act_test.rs)；缺口：执行失败：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-CORE-008 | 源码存在 | [契约](../design/architecture.md) | [源码](../../peri-agent/src/agent/stages/receive.rs)；[相关测试](../../peri-agent/src/agent/stages/receive_delivery_test.rs)；缺口：事件顺序：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-CORE-009 | 源码存在 | [契约](../design/architecture.md) | [源码](../../peri-agent/src/agent/stages/middleware_runner.rs)；[相关测试](../../peri-agent/src/agent/stages/middleware_cancel_test.rs)；缺口：资源收尾：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-CTRL-001 | 源码存在 | [契约](../design/middleware-system.md) | [源码](../../peri-middlewares/src/assembly.rs)；[相关测试](../../peri-middlewares/src/assembly_test.rs)；缺口：能力装配：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-CTRL-002 | 源码存在 | [契约](../design/middleware-system.md) | [源码](../../peri-middlewares/src/assembly/production_chain_assembler.rs)；[相关测试](../../peri-middlewares/src/assembly_test_meta.rs)；缺口：能力关闭：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-CTRL-003 | 源码存在 | [契约](../design/middleware-system.md) | [源码](../../peri-middlewares/src/assembly/workflow.rs)；[相关测试](../../peri-middlewares/src/assembly_test_workflow.rs)；缺口：派生控制：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-CTRL-004 | 源码存在 | [契约](../design/middleware-system.md) | [源码](../../peri-middlewares/src/assembly/hooks.rs)；[相关测试](../../peri-middlewares/src/assembly_once_scope_test.rs)；缺口：中间件生命周期：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-CTRL-005 | 源码存在 | [契约](../design/middleware-system.md) | [源码](../../peri-middlewares/src/assembly/preparation.rs)；[相关测试](../../peri-middlewares/src/assembly_test_meta.rs)；缺口：声明诊断：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-CTRL-006 | 源码存在 | [契约](../design/middleware-system.md) | [源码](../../peri-middlewares/src/host_ports.rs)；[相关测试](../../peri-middlewares/src/host_ports_test.rs)；缺口：宿主注入：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-PROMPT-001 | 源码存在 | [契约](../design/system-prompt.md) | [源码](../../peri-acp/src/prompt/prompt_template.rs)；[相关测试](../../peri-acp/src/prompt/prompt_sections_test.rs)；缺口：静态段落：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-PROMPT-002 | 源码存在 | [契约](../design/system-prompt.md) | [源码](../../peri-acp/src/prompt/prompt_template.rs)；[相关测试](../../peri-acp/src/prompt/prompt_test.rs)；缺口：请求期贡献：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-PROMPT-003 | 源码存在 | [契约](../design/meta-harness.md) | [源码](../../peri-acp/src/prompt/section_validation.rs)；[相关测试](../../peri-acp/src/prompt/prompt_overrides_test.rs)；缺口：段落覆盖：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-PROMPT-004 | 源码存在 | [契约](../design/system-prompt.md) | [源码](../../peri-acp/src/prompt/prompt_template.rs)；[相关测试](../../peri-acp/src/prompt/prompt_template_test.rs)；缺口：缓存前缀：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-PROMPT-005 | 源码存在 | [契约](../design/system-prompt.md) | [源码](../../peri-acp/src/prompt/prompt_template.rs)；[相关测试](../../peri-acp/src/prompt/prompt_template_test.rs)；缺口：Provider 投影：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-PROMPT-006 | 源码存在 | [契约](../design/system-prompt.md) | [源码](../../peri-acp/src/prompt/prompt_template.rs)；[相关测试](../../peri-acp/src/prompt/prompt_sections_test.rs)；缺口：Persona 与语言：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-PROMPT-007 | 源码存在 | [契约](../design/system-prompt.md) | [源码](../../peri-acp/src/prompt/prompt_template.rs)；[相关测试](../../peri-acp/src/prompt/prompt_test.rs)；缺口：环境上下文：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-PROMPT-008 | 源码存在 | [契约](../design/system-prompt.md) | [源码](../../peri-tui/src/acp_client/client/session/transition.rs)；[相关测试](../../peri-tui/src/acp_client/client/session/instructions_test.rs)；缺口：TUI 会话指令只在协议载荷层验证 Interactive/Headless 与 `session/load` 的 `_meta`，未在真实 TTY 观察注入后的模型行为 | 未复验 |
| F-COMPACT-001 | 源码存在 | [契约](../design/micro-compact.md) | [源码](../../peri-agent/src/agent/compact_v2/micro.rs)；[相关测试](../../peri-agent/src/agent/compact_v2/micro_test.rs)；缺口：Micro compact：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-COMPACT-002 | 源码存在 | [契约](../design/micro-compact.md) | [Full](../../peri-agent/src/agent/compact_v2/full.rs) · [恢复提示测试](../../peri-agent/src/agent/compact_v2/hard_memory_test.rs) · [完整压缩测试](../../peri-agent/src/agent/compact_v2/full_test.rs) · [#509](https://github.com/KonghaYao/peri/issues/509) 待验首轮真实句柄、回执竞态及外置回放 | 未复验 |
| F-COMPACT-003 | 源码存在 | [契约](../design/micro-compact.md) | [源码](../../peri-agent/src/agent/compact_v2/projection_archive.rs)；[相关测试](../../peri-agent/src/agent/compact_v2/projection_archive_test.rs)；缺口：原文回查：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-COMPACT-004 | 源码存在 | [契约](../design/micro-compact.md) | [源码](../../peri-agent/src/agent/compact_v2/compact_engine.rs)；[相关测试](../../peri-agent/src/agent/compact_v2/full_protected_result_test.rs)；缺口：压缩失败：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-COMPACT-005 | 源码存在 | [契约](../design/micro-compact.md) | [源码](../../peri-agent/src/messages/tool_pairing.rs)；[相关测试](../../peri-agent/src/messages/tool_pairing_test.rs)；缺口：工具配对修补：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-COMPACT-006 | 源码存在 | [契约](../design/micro-compact.md) | [源码](../../peri-agent/src/agent/compact_v2/projection.rs)；[相关测试](../../peri-agent/src/agent/compact_v2/projection_estimate_test.rs)；缺口：Token 预算：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-COMPACT-007 | 源码存在 | [契约](../design/micro-compact.md) | [源码](../../peri-agent/src/agent/compact_v2/projection.rs)；[相关测试](../../peri-agent/src/agent/compact_v2/archive_lifecycle_test.rs)；缺口：fork 历史：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-COMPACT-008 | 源码存在 | [契约](../design/micro-compact.md) | [源码](../../peri-agent/src/agent/compact_v2/projection.rs)；[相关测试](../../peri-agent/src/agent/compact_v2/projection_restore_test.rs)；缺口：rewind 历史：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-SESSION-001 | 源码存在 | [契约](../design/session-id-environment.md) | [源码](../../peri-acp/src/host/requests.rs)；[相关测试](../../peri-acp/src/host/requests_workspace_cases_test.rs)；缺口：创建会话：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-SESSION-002 | 源码存在 | [契约](../design/session-id-environment.md) | [源码](../../peri-acp/src/host/requests.rs)；[相关测试](../../peri-acp/src/host/requests_history_lifecycle_test.rs)；缺口：加载会话：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-SESSION-003 | 源码存在 | [契约](../design/session-id-environment.md) | [源码](../../peri-acp/src/host/requests.rs)；[相关测试](../../peri-acp/src/host/requests_close_failure_test.rs)；缺口：关闭会话：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-SESSION-004 | 源码存在 | [契约](../design/session-id-environment.md) | [源码](../../peri-acp/src/host/requests.rs)；[相关测试](../../peri-acp/src/host/requests_lifecycle_cases_test.rs)；缺口：切换会话：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-SESSION-005 | 源码存在 | [契约](../design/session-id-environment.md) | [源码](../../peri-acp/src/host/requests.rs)；[相关测试](../../peri-acp/src/host/requests_workspace_storage_v2_test.rs)；缺口：会话发现：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-SESSION-006 | 源码存在 | [契约](../design/session-id-environment.md) | [源码](../../peri-acp/src/host/requests.rs)；[相关测试](../../peri-acp/src/host/requests_workspace_cases_test.rs)；缺口：工作区重定位：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-SESSION-007 | 源码存在 | [契约](../design/session-id-environment.md) | [源码](../../peri-acp/src/host/requests.rs)；[相关测试](../../peri-acp/src/host/requests_workspace_cases_test_part2_test.rs)；缺口：跨工作区续作：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-SESSION-008 | 源码存在 | [契约](../design/session-id-environment.md) | [源码](../../peri-acp/src/host/requests.rs)；[相关测试](../../peri-acp/src/host/requests_workspace_storage_v2_test.rs)；缺口：会话归档：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-SESSION-009 | 源码存在 | [契约](../design/session-id-environment.md) | [源码](../../peri-acp/src/host/requests.rs)；[相关测试](../../peri-acp/src/host/requests_lifecycle_cases_test.rs)；缺口：会话删除：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-MQ-001 | 源码存在 | [契约](../design/thread-mq.md) | [源码](../../peri-resources/src/sessions)；缺口：持久收件箱的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MQ-002 | 源码存在 | [契约](../design/thread-mq.md) | [源码](../../peri-resources/src/sessions)；缺口：原子 Receive的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MQ-003 | 源码存在 | [契约](../design/thread-mq.md) | [源码](../../peri-resources/src/sessions)；缺口：子任务结果的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MQ-004 | 源码存在 | [契约](../design/thread-mq.md) | [源码](../../peri-resources/src/sessions)；缺口：用户队列的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MQ-005 | 源码存在 | [契约](../design/thread-mq.md) | [源码](../../peri-resources/src/sessions)；缺口：晚到消息的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MQ-006 | 源码存在 | [契约](../design/thread-mq.md) | [源码](../../peri-resources/src/sessions)；缺口：终态幂等的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MQ-007 | 源码存在 | [契约](../design/thread-mq.md) | [源码](../../peri-resources/src/sessions)；缺口：恢复边界的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-HISTORY-001 | 源码存在 | [契约](../design/peri-acp-protocol.md) | [源码](../../peri-acp/src/host/requests.rs)；[相关测试](../../peri-acp/src/host/requests_unnegotiated_replay_test.rs)；缺口：标准历史回放：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-HISTORY-002 | 源码存在 | [契约](../design/peri-acp-protocol.md) | [源码](../../peri-acp/src/host/history_page.rs)；[相关测试](../../peri-acp/src/host/history_page_test.rs)；缺口：History V1：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-HISTORY-003 | 源码存在 | [契约](../design/peri-acp-protocol.md) | [源码](../../peri-acp/src/host/history_v2_load.rs)；[相关测试](../../peri-acp/src/host/history_v2_load_test.rs)；缺口：History V2 协商：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-HISTORY-004 | 源码存在 | [契约](../design/peri-acp-protocol.md) | [源码](../../peri-acp/src/host/history_v2_load.rs)；[相关测试](../../peri-acp/src/host/history_v2_end_to_end_test.rs)；缺口：历史 initial：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-HISTORY-005 | 源码存在 | [契约](../design/peri-acp-protocol.md) | [源码](../../peri-acp/src/host/history_v2_adapter.rs)；[相关测试](../../peri-acp/src/host/history_v2_adapter_test.rs)；缺口：历史分页：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-HISTORY-006 | 源码存在 | [契约](../design/peri-acp-protocol.md) | [源码](../../peri-acp/src/host/history_v2_adapter.rs)；[相关测试](../../peri-acp/src/host/history_v2_adapter_visibility_test.rs)；缺口：历史 seek：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-HISTORY-007 | 源码存在 | [契约](../design/peri-acp-protocol.md) | [源码](../../peri-acp/src/host/history_v2_adapter.rs)；[相关测试](../../peri-acp/src/host/history_v2_adapter_body_test.rs)；缺口：历史正文：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-HISTORY-008 | 源码存在 | [契约](../design/peri-acp-protocol.md) | [源码](../../peri-acp/src/host/history_v2_adapter.rs)；[相关测试](../../peri-acp/src/host/history_v2_adapter_test.rs)；缺口：历史失效：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-HISTORY-009 | 源码存在 | [契约](../design/peri-acp-protocol.md) | [源码](../../peri-acp/src/host/history_v2_adapter.rs)；[相关测试](../../peri-acp/src/host/history_v2_adapter_inherited_test.rs)；缺口：继承历史索引：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-HISTORY-010 | 源码存在 | [契约](../design/peri-acp-protocol.md) | [源码](../../peri-acp/src/host/requests.rs)；[相关测试](../../peri-acp/src/host/requests_history_lifecycle_test.rs)；缺口：只读历史：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-TASK-001 | 源码存在 | [契约](../design/session-async-tasks.md) | [源码](../../peri-agent/src/session/subagent/background.rs)；[相关测试](../../peri-agent/src/session/subagent/background_registration_test.rs)；缺口：子 Agent 委派：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-TASK-002 | 源码存在 | [契约](../design/session-async-tasks.md) | [源码](../../peri-agent/src/session/subagent/background.rs)；[相关测试](../../peri-agent/src/session/subagent/child_runner_nested_test.rs)；缺口：并发子任务：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-TASK-003 | 源码存在 | [契约](../design/session-async-tasks.md) | [源码](../../peri-agent/src/session/subagent/background.rs)；[相关测试](../../peri-agent/src/session/subagent/parked_inbox_test.rs)；缺口：子任务 follow-up：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-TASK-004 | 源码存在 | [契约](../design/session-async-tasks.md) | [源码](../../peri-agent/src/session/subagent/background.rs)；[相关测试](../../peri-agent/src/session/subagent/resume_background_cases_test.rs)；缺口：子任务 resume：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-TASK-005 | 源码存在 | [契约](../design/session-async-tasks.md) | [源码](../../peri-agent/src/session/subagent/child_runner.rs)；[相关测试](../../peri-agent/src/session/subagent/close_test.rs)；缺口：子任务取消：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-TASK-006 | 源码存在 | [契约](../design/session-async-tasks.md) | [源码](../../peri-agent/src/session/subagent/background.rs)；[相关测试](../../peri-agent/src/session/subagent/background_registration_test.rs)；缺口：子任务失败：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-TASK-007 | 源码存在 | [契约](../design/session-async-tasks.md) | [源码](../../peri-agent/src/session/subagent/terminal_settlement.rs)；[相关测试](../../peri-agent/src/session/subagent/terminal_settlement_test.rs)；缺口：子任务完成：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-TASK-008 | 源码存在 | [契约](../design/session-async-tasks.md) | [源码](../../peri-agent/src/session/subagent/child_runner.rs)；[相关测试](../../peri-agent/src/session/subagent/child_runner_process_test.rs)；缺口：子任务 Shell：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-TASK-009 | 源码存在 | [契约](../design/session-async-tasks.md) | [源码](../../peri-agent/src/session/subagent/factory.rs)；[相关测试](../../peri-agent/src/session/subagent/mcp_policy_test.rs)；缺口：子任务配置：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-TASK-010 | 源码存在 | [契约](../design/session-async-tasks.md) | [源码](../../peri-agent/src/session/subagent/types.rs)；[相关测试](../../peri-agent/src/session/subagent/child_wire_capture_test.rs)；缺口：旧参数兼容：已有相关测试入口；精确断言覆盖和外置场景需对账 | 未复验 |
| F-INTERACT-001 | 源码存在 | [契约](../design/interaction-brokers.md) | [源码](../../peri-middlewares/src/permission)；缺口：工具权限的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-INTERACT-002 | 源码存在 | [契约](../design/interaction-brokers.md) | [源码](../../peri-middlewares/src/permission)；缺口：交互归属的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-INTERACT-003 | 源码存在 | [契约](../design/interaction-brokers.md) | [源码](../../peri-middlewares/src/permission)；缺口：权限模式的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-INTERACT-004 | 源码存在 | [契约](../design/interaction-brokers.md) | [源码](../../peri-middlewares/src/permission)；缺口：用户提问的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-INTERACT-005 | 源码存在 | [契约](../design/interaction-brokers.md) | [源码](../../peri-middlewares/src/permission)；缺口：交互取消的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MCP-001 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../peri-middlewares/src/mcp)；缺口：MCP 连接的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MCP-002 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../peri-middlewares/src/mcp)；缺口：工具发现的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MCP-003 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../peri-middlewares/src/mcp)；缺口：工具搜索的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MCP-004 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../peri-middlewares/src/mcp)；缺口：间接执行的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MCP-005 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../peri-middlewares/src/mcp)；缺口：MCP 资源的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MCP-006 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../peri-middlewares/src/mcp)；缺口：MCP 订阅的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MCP-007 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../peri-middlewares/src/mcp)；缺口：MCP 任务的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MCP-008 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../peri-middlewares/src/mcp)；缺口：MCP 重连的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MCP-009 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../peri-middlewares/src/mcp)；缺口：MCP OAuth的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MCP-010 | 源码存在 | [契约](../design/mcp-v5-single-exit.md) | [源码](../../peri-middlewares/src/mcp)；缺口：MCP 单出口的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MCP-011 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../peri-middlewares/src/mcp)；缺口：MCP 启动诊断的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MCP-012 | 源码存在 | [契约](../design/acp-mcp-control.md) | [源码](../../peri-middlewares/src/mcp)；缺口：动态注册的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MCP-013 | 源码存在 | [契约](../design/mcp-cache.md) | [源码](../../peri-middlewares/src/mcp)；缺口：MCP 缓存的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MCP-014 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../peri-middlewares/src/mcp)；缺口：MCP 独立服务的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MCP-015 | 目标；未合入 | [契约](../design/tool-system.md) | [源码](../../peri-middlewares/src/mcp)；缺口：#449 目标实现与端到端证据；不得以现有动态注册替代 | 未复验 |
| F-MCP-016 | 目标；未合入 | [契约](../design/tool-system.md) | [源码](../../peri-middlewares/src/mcp)；缺口：#449 目标实现与端到端证据；不得以现有动态注册替代 | 未复验 |
| F-WORKSPACE-001 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/workspace/src)；缺口：文件读取的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKSPACE-002 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/workspace/src)；缺口：文件写入的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKSPACE-003 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/workspace/src)；缺口：文件编辑的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKSPACE-004 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/workspace/src)；缺口：文件搜索的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKSPACE-005 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/workspace/src)；缺口：目录操作的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKSPACE-006 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/workspace/src)；缺口：Bash 执行的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKSPACE-007 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/workspace/src)；缺口：后台 Shell的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKSPACE-008 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/workspace/src)；缺口：Shell 取消的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKSPACE-009 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/workspace/src)；缺口：文件回退的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKSPACE-010 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/workspace/src)；缺口：项目指令的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKSPACE-011 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/workspace/src)；缺口：技能发现的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKSPACE-012 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/workspace/src)；缺口：技能加载的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKSPACE-013 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/workspace/src)；缺口：Agent 目录的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKSPACE-014 | 源码存在 | [契约](../design/git-watch-middleware.md) | [源码](../../mcp-packages/workspace/src)；缺口：Git 观察的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKSPACE-015 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/workspace/src)；缺口：路径诊断的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKSPACE-016 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/workspace/src)；缺口：大输出存档的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-EXT-001 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/web/src/web_search.rs)；缺口：Web 搜索的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-EXT-002 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/web/src/web_fetch.rs)；缺口：Web 抓取的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-EXT-003 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../mcp-packages/artifact/src/tool.rs)；缺口：Artifact的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-EXT-004 | 源码存在 | [契约](../design/session-async-tasks.md) | [源码](../../mcp-packages/cron/src)；缺口：Cron的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-EXT-005 | 源码存在 | [契约](../design/monitor-mcp.md) | [源码](../../mcp-packages/monitor/src)；缺口：Monitor 启动的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-EXT-006 | 源码存在 | [契约](../design/monitor-mcp.md) | [源码](../../mcp-packages/monitor/src/buffer/event_buffer.rs)；缺口：Monitor 读取的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-EXT-007 | 源码存在 | [契约](../design/monitor-mcp.md) | [源码](../../mcp-packages/monitor/src)；缺口：Monitor 提醒的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-EXT-008 | 源码存在 | [契约](../design/monitor-mcp.md) | [源码](../../mcp-packages/monitor/src/tools.rs)；缺口：Monitor 停止的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-EXT-009 | 源码存在 | [契约](../design/configuration-authority.md) | [源码](../../mcp-packages/config/src)；缺口：配置资源的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-EXT-010 | 源码存在 | [契约](../design/acp-mcp-control.md) | [源码](../../mcp-packages/credentials/src)；缺口：凭据资源的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKFLOW-001 | 源码存在 | [契约](../design/workflow.md) | [源码](../../peri-workflow/src)；缺口：Workflow 执行的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKFLOW-002 | 源码存在 | [契约](../design/workflow.md) | [源码](../../peri-workflow/src)；缺口：Workflow 委派的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKFLOW-003 | 源码存在 | [契约](../design/workflow.md) | [源码](../../peri-workflow/src)；缺口：Workflow 状态的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKFLOW-004 | 源码存在 | [契约](../design/workflow.md) | [源码](../../peri-workflow/src)；缺口：Workflow 取消的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKFLOW-005 | 源码存在 | [契约](../design/workflow.md) | [源码](../../peri-workflow/src)；缺口：Workflow 恢复的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKFLOW-006 | 源码存在 | [契约](../design/workflow.md) | [源码](../../peri-workflow/src)；缺口：Workflow 进度的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKFLOW-007 | 源码存在 | [契约](../design/workflow.md) | [源码](../../peri-workflow/src)；缺口：Workflow MCP的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKFLOW-008 | 源码存在 | [契约](../design/workflow.md) | [源码](../../peri-workflow/src)；缺口：Ultra-ADLC的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-WORKFLOW-009 | 源码存在 | [契约](../design/workflow.md) | [源码](../../peri-workflow/src/runtime/tasks.rs)；[真实引擎 E2E](../../peri-acp/src/host/workflow_tool_consumer_test.rs)；[Node 测试](../../npm-packages/@peri-workflow/test/tool.test.ts)；[验收记录](workflow-codemode-tool-2026-11-22.md)；缺口：审批拒绝/超时与真实外部 deferred 工具未验证；peri-e2e #10 固定 SHA 对账未做 | 本地真实引擎 E2E 5/5 通过；审批拒绝与外部 deferred 工具未验证 |
| F-CONFIG-001 | 源码存在 | [契约](../design/configuration-authority.md) | [源码](../../peri-config/src/assembly.rs)；缺口：配置解析的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-CONFIG-002 | 源码存在 | [契约](../design/configuration-authority.md) | [源码](../../peri-config/src/source.rs)；缺口：配置作用域的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-CONFIG-003 | 源码存在 | [契约](../design/configuration-authority.md) | [源码](../../peri-config/src/settings.rs)；缺口：配置保存的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-CONFIG-004 | 源码存在 | [契约](../design/configuration-authority.md) | [源码](../../peri-config/src/provider.rs)；缺口：Provider 配置的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-CONFIG-005 | 源码存在 | [契约](../design/configuration-authority.md) | [源码](../../peri-config/src/assembly.rs)；缺口：配置重载的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-CONFIG-006 | 源码存在 | [契约](../design/configuration-authority.md) | [源码](../../peri-config/src/betas.rs)；缺口：Beta 开关的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-CONFIG-007 | 源码存在 | [契约](../design/configuration-authority.md) | [源码](../../peri-middlewares/src/plugin)；缺口：插件安装的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-CONFIG-008 | 源码存在 | [契约](../design/configuration-authority.md) | [源码](../../peri-middlewares/src/plugin)；缺口：插件市场的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-CONFIG-009 | 源码存在 | [契约](../design/configuration-authority.md) | [源码](../../peri-middlewares/src/plugin)；缺口：命令扩展的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MODEL-001 | 源码存在 | [契约](../design/model-adapters.md) | [源码](../../peri-model/src/anthropic)；缺口：Anthropic 请求的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MODEL-002 | 源码存在 | [契约](../design/model-adapters.md) | [源码](../../peri-model/src/openai_compatible)；缺口：OpenAI Compatible的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MODEL-003 | 源码存在 | [契约](../design/model-adapters.md) | [源码](../../peri-model/src/openai_responses)；缺口：OpenAI Responses的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MODEL-004 | 源码存在 | [契约](../design/model-adapters.md) | [源码](../../peri-model/src/runtime)；缺口：模型流解析的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MODEL-005 | 源码存在 | [契约](../design/model-adapters.md) | [源码](../../peri-model/src/transport)；缺口：请求取消的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MODEL-006 | 源码存在 | [契约](../design/model-adapters.md) | [源码](../../peri-model/src/runtime)；缺口：模型重试的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MODEL-007 | 源码存在 | [契约](../design/model-adapters.md) | [源码](../../peri-model/src/anthropic/request.rs)；缺口：推理预算的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-MODEL-008 | 源码存在 | [契约](../design/model-adapters.md) | [源码](../../peri-model/src/protocol)；缺口：Token 用量的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-001 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：输入提交的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-002 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：流式消息的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-003 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：工具活动的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-004 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-agent/src/session/user_input_mailbox)；[相关测试](../../peri-agent/src/session/user_input_mailbox_test.rs)；阶段门定向验证见 [验收记录](steer-stage-gate-2026-11-22.md)；缺口：真实 TUI 交互与外置场景仍待测试仓对账 | 本地定向复验；未跑全量/e2e |
| F-TUI-005 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：历史浏览的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-006 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：子 Agent 面板的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-007 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：任务面板的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-008 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：审批面板的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-009 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：问答面板的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-010 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：配置面板的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-011 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：模型面板的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-012 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：插件面板的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-013 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：MCP 面板的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-014 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：Workflow 面板的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-015 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：Cron 面板的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-016 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：会话浏览器的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-017 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：主题切换的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-018 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：Markdown 渲染的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-019 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：鼠标交互的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-020 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；缺口：输入编辑的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-TUI-021 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | 本地工作区（`2cd1a5b5c` + 未提交改动）：[消息区回归](../../peri-tui/src/kit/message_area/genui_action_test.rs)覆盖 Down 分发、折行样式和取消；[storybook 回归](../../peri-tui-components/src/bin/storybook/app_test.rs)覆盖实际鼠标事件下 Buffer 样式变化、恢复与滚动命中。GenUI 整体场景仍待测试仓对账 | 按下态本地复验；未做完整 E2E |
| F-TUI-022 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；按受影响展示测试检查；不强制独立 E2E | 未复验 |
| F-TUI-023 | 源码存在 | [契约](../design/tui-acp-data-flow.md) | [源码](../../peri-tui/src/kit)；按受影响展示测试检查；不强制独立 E2E | 未复验 |
| F-SDK-001 | 源码存在 | [契约](../design/peri-sdk-chat.md) | [源码](../../npm-packages/@peri-sdk/src/chat)；缺口：ChatManager的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-SDK-002 | 源码存在 | [契约](../design/peri-sdk-chat.md) | [源码](../../npm-packages/@peri-sdk/src/chat)；缺口：ChatService的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-SDK-003 | 源码存在 | [契约](../design/peri-sdk-chat.md) | [源码](../../npm-packages/@peri-sdk/src/view)；缺口：ChatView的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-SDK-004 | 源码存在 | [契约](../design/peri-sdk-chat.md) | [源码](../../npm-packages/@peri-sdk/src/sync)；缺口：ChatReplica的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-SDK-005 | 源码存在 | [契约](../design/peri-sdk-chat.md) | [源码](../../npm-packages/@peri-sdk/src/execution)；缺口：会话执行归属的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-SDK-006 | 源码存在 | [契约](../design/peri-sdk-chat.md) | [源码](../../npm-packages/@peri-sdk/src/transport)；缺口：Runtime 适配的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-SDK-007 | 源码存在 | [契约](../design/peri-sdk-chat.md) | [源码](../../npm-packages/@peri-sdk/src/storage)；缺口：SDK 数据库的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-PLATFORM-001 | 源码存在 | [契约](../design/serverless-execution-identity.md) | [源码](../../peri-wasm/src)；缺口：WASM 运行的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-PLATFORM-002 | 源码存在 | [契约](../design/serverless-execution-identity.md) | [源码](../../peri-wasm/src)；缺口：执行环境身份的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-PLATFORM-003 | 源码存在 | [契约](../design/serverless-execution-identity.md) | [源码](../../npm-packages/peri-sandbox/src)；缺口：Sandbox 启动的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-PLATFORM-004 | 源码存在 | [契约](../design/time-runtime.md) | [源码](../../peri-time/src)；缺口：时间适配的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-PLATFORM-005 | 源码存在 | [契约](../design/serverless-execution-identity.md) | [源码](../../peri-js-runtime/src)；缺口：JS Runtime的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-PLATFORM-006 | 源码存在 | [契约](../design/architecture.md) | [源码](../../langfuse-client/src)；缺口：遥测的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-PLATFORM-007 | 部分实现 | [契约](../../spec/issues/2026-11-18-v5-memory-447.md) | [源码](../../peri-agent/src/session/memory_probe.rs)；缺口：#447 固定长时负载与资源释放曲线 | 未复验 |
| F-PLATFORM-008 | 源码存在 | [契约](../standards/tui.md) | [源码](../../peri-tui-components/src/lib.rs)；缺口：组件消费的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-PLATFORM-009 | 源码存在 | [契约](../../npm-packages/memory-plugin/DESIGN.md) | [源码](../../npm-packages/memory-plugin/src)；缺口：Memory 插件的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-DURABLE-001 | 源码存在 | [契约](../design/message-transcript.md) | [源码](../../peri-agent/src/session/transcript.rs)；[相关测试](../../peri-agent/src/session/transcript_persistence_test.rs)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-DURABLE-002 | 源码存在 | [契约](../design/session-durability.md) | [源码](../../peri-acp/src/host/recovery)；[相关测试](../../peri-acp/src/host/recovery_behavior_test.rs)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-DURABLE-003 | 源码存在 | [契约](../design/session-durability.md) | [源码](../../peri-acp/src/host/recovery)；[相关测试](../../peri-acp/src/host/recovery_child_test.rs)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-DURABLE-004 | 源码存在 | [契约](../design/session-durability.md) | [源码](../../peri-acp/src/host/recovery)；[相关测试](../../peri-acp/src/host/recovery_durability_test.rs)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-APPS-001 | 源码存在 | [契约](../design/mcp-multiplexing.md) | [源码](../../peri-acp/src/host/apps_facade.rs)；[相关测试](../../peri-acp/src/host/apps_facade_attach_test.rs)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-APPS-002 | 源码存在 | [契约](../design/mcp-multiplexing.md) | [源码](../../peri-acp/src/host/mcp_apps.rs)；[相关测试](../../peri-acp/src/host/mcp_apps_test.rs)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-APPS-003 | 源码存在 | [契约](../design/mcp-multiplexing.md) | [源码](../../peri-acp/src/host/mcp_apps.rs)；[相关测试](../../peri-acp/src/host/mcp_apps_idle_test.rs)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-EXT-011 | 源码存在 | [契约](../design/middleware-system.md) | [源码](../../peri-middlewares/src/hooks)；[相关测试](../../peri-middlewares/src/assembly_once_scope_test.rs)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-EXT-012 | 源码存在 | [契约](../design/system-prompt.md) | [源码](../../peri-middlewares/src/at_mention)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-EXT-013 | 源码存在 | [契约](../design/system-reminder.md) | [源码](../../peri-middlewares/src/goal)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-EXT-014 | 源码存在 | [契约](../design/tool-system.md) | [源码](../../peri-middlewares/src/middleware/todo.rs)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-EXT-015 | 源码存在 | [契约](../design/model-adapters.md) | [源码](../../peri-middlewares/src/middleware/image)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-CLI-001 | 源码存在 | [契约](../design/architecture.md) | [源码](../../peri-tui/src/cli_print.rs)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-CLI-002 | 源码存在 | [契约](../design/configuration-authority.md) | [源码](../../peri-tui/src/cli_auth.rs)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-CLI-003 | 源码存在 | [契约](../design/architecture.md) | [源码](../../peri-tui/src/update.rs)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-CLI-004 | 源码存在 | [契约](../design/meta-control.md) | [源码](../../peri-tui/src/cli_meta.rs)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-CLI-005 | 源码存在 | [契约](../design/workflow.md) | [源码](../../peri-tui/src/cli_workflow.rs)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-CLI-006 | 源码存在 | [契约](../design/configuration-authority.md) | [源码](../../peri-tui/src/cli_args.rs)；缺口：精确断言覆盖和外置场景需对账 | 未复验 |
| F-CLI-007 | 源码存在 | [契约](../design/peri-acp-protocol.md) | [源码](../../peri-tui/src/startup.rs)、[认证握手](../../peri-acp/src/host/stdio/auth_required_gate.rs)、[测试](../../peri-acp/src/host/stdio/auth_required_gate_test.rs)、[进程级握手](../../peri-tui/tests/acp_registry_auth.rs)；缺口：真实 Registry 端到端未复验 | 未复验 |
| F-SWARM-001 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/http/mod.rs)；[测试](../../peri-swarm/tests/http_contract_test.rs)；[证据](peri-swarm-local-dev.md)、[官方 SDK 对齐](peri-swarm-official-sdk-2026-11-21.md)；缺口：真实第三方调用方的门禁组合未验证 | 本地真实链路已验证；本轮补充既有行 create 在运行中/已预占 turn 上返回 `409 turn_in_progress` 且不写 user item（对齐 §9）；本次未重跑真实链路 |
| F-SWARM-002 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/domain/identity.rs)；[测试](../../peri-swarm/tests/actor_runtime_test.rs)；[证据](peri-swarm-local-dev.md)；缺口：跨机接管未验证；创建中断的 root id 核对由预置 `creating` 行测试覆盖，未注入真实进程崩溃 | 本地真实链路已验证；本次未重跑 |
| F-SWARM-003 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/http/sse.rs)；[测试](../../peri-swarm/tests/streaming_test.rs)、[items 分页](../../peri-swarm/tests/items_pagination_test.rs)、[create-stream 首轮](../../peri-swarm/tests/actor_runtime_test.rs)；[证据](peri-swarm-local-dev.md)、[官方 SDK 对齐](peri-swarm-official-sdk-2026-11-21.md)、[官方默认值对齐](peri-swarm-official-defaults-2026-11-21.md)；缺口：`gap` 后中断行为由单测覆盖，多订阅者容量未验证；官方帧未用真实 SDK 进程消费；历史失败 turn 回放为 `completed`；真实 Rivet engine 下 create-stream 未运行 | 无游标实时（含 Tail 不读行）、item 分页/行预算与 lookback 终态由契约测试覆盖；本轮补充 actor 路径官方首轮帧顺序、既有 idle 行 defer 与 `pendingInitial` 不跨进程（§9.3）；items 默认 `order=desc` 与 `include_obfuscation` 接受并忽略由契约测试覆盖；本次未重跑真实链路 |
| F-SWARM-004 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/http/request.rs)；[测试](../../peri-swarm/tests/http_contract_test.rs)；[证据](peri-swarm-local-dev.md)、[官方 events 信封](peri-swarm-official-events-2026-11-21.md)、[官方 SDK 对齐](peri-swarm-official-sdk-2026-11-21.md)；缺口：整批原子受理不提供；官方信封仅单条 message、纯文本；202-before-terminal 由 `TurnPolicy::Deferred` 测试覆盖，未用真实 SDK 进程调用 | 并发同键与冲突已真实运行；官方信封与 202 边界由契约测试覆盖；本次未重跑 |
| F-SWARM-005 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/store/turso.rs)；[测试](../../peri-swarm/tests/loom_store_test.rs)；[证据](peri-swarm-local-dev.md)；缺口：真实 Loom 存储测试默认 ignored，需显式启用 | 两库隔离已真实运行；本次未重跑 |
| F-SWARM-006 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/actor/runtime.rs)；[测试](../../peri-swarm/tests/mcp_injection_test.rs)；[证据](peri-swarm-local-dev.md)；缺口：真实第三方 MCP server 未端到端运行 | 注入形状已测；真实第三方未验证 |
| F-SWARM-007 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../npm-packages/peri-sandbox/src/swarm/manager.ts)；[测试](../../npm-packages/peri-sandbox/tests/swarm/manager.test.ts)；[证据](peri-swarm-local-dev.md)；缺口：Linux SRT 与跨机调度未验证 | 真实 SRT + Bash 已验证；本次未重跑 |
| F-SWARM-008 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../npm-packages/peri-sandbox/src/swarm/manager.ts)；[测试](../../npm-packages/peri-sandbox/tests/swarm/server.test.ts)；[证据](peri-swarm-local-dev.md)；缺口：后台任务引用未接入回收判定 | 在途阻断与回收已真实运行；任务引用未验证 |
| F-SWARM-009 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/actor/runtime.rs)；[测试](../../peri-swarm/tests/actor_runtime_test.rs)；[证据](peri-swarm-local-dev.md)；缺口：未完成 turn 不调用 `session/resume` | idle reattach 已验证；running 崩溃未覆盖 |
| F-SWARM-010 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/bootstrap.rs)；[脚本](../../peri-swarm/scripts/provision_state_db.py)；[证据](peri-swarm-local-dev.md)；缺口：跨机部署、TLS 与 1000 Agent 规模未验证 | 本地启动序已记录；本次未重跑 |
| F-SWARM-011 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/config.rs)、[bootstrap.rs](../../peri-swarm/src/bootstrap.rs)；[测试](../../peri-swarm/src/config_test.rs)、[bootstrap_test.rs](../../peri-swarm/src/bootstrap_test.rs)；[证据](peri-swarm-multi-replica-2026-11-21.md)；缺口：域名实际指向、双进程多副本、单 engine 节点、TLS 未验证 | 配置矩阵与 endpoint 私网校验单测、编译门结果见证据；M0 未执行；人工双进程矩阵未执行 |
| F-SWARM-012 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/http/mod.rs)、[测试](../../peri-swarm/tests/session_resources_test.rs)；[证据](peri-swarm-session-resources-2026-11-21.md)、[官方默认值对齐](peri-swarm-official-defaults-2026-11-21.md)；缺口：真实 Loom 迁移、engine 上的 delete/update 未验证 | 契约测试覆盖 list/update/delete/turns、POST update 与显式 404；真实 `openai@7.60.0` SDK 已对进程内网关执行 create/retrieve/update/turns.list 探针；持久未完成 turn 的 502 准入与取消状态持久化有单测；本轮补充 `prompt_cache_options:{}` 默认 30m、官方会话默认字段与环境 `none` 归一化、`{}` update 与无 body 同为 no-op |
