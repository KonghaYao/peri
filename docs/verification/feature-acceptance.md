# 功能验收映射

> 盘点基线：`cee019f34337e420178be0c2671c98f8c23c4083`，2026-11-18。
> “源码存在”只证明入口存在；本表不将测试文件存在、历史 PR 通过或文档检查通过当成本次运行验收。

功能定义与评级只在 [功能目录](../reference/feature-catalog.md)；本表维护当前实现、源码定位和证据缺口。“相关测试”是已定位的候选证据入口，仍须逐断言确认覆盖范围，不能据此声称整行边界已验证。各行“未复验”指本次文档变更未重跑对应运行行为，并非已确认产品缺陷。所有 L1–L4 的场景 ID、关键断言和固定产品/测试 SHA 对账交 [peri-e2e #10](https://github.com/KonghaYao/peri-e2e/issues/10)，不在 Peri 复制测试编排。L5 没有自动新增 E2E 的义务。

已知证据：#443 已由 [#455](https://github.com/KonghaYao/peri/pull/455) 合并，async-only 已由 [#453](https://github.com/KonghaYao/peri/pull/453) 合并；这是维护者历史验收声明，本次未重跑。History V2 的细项证据与缺口只在 [既有矩阵](history-v2-acceptance.md)。试验性 TS 适配层（`peri-rivet`、`agent-builder`）已于 2026-11-22 移除，F-SDK-008 与 F-RIVET-001..008 退役；原 [#448](https://github.com/KonghaYao/peri/issues/448) 的真实 Engine/24h 证据随该层归档，Peri Swarm 主线的未验收项见[本地开发验收记录](peri-swarm-local-dev.md) 的 `unverified` 清单。内存证据归 [#447](https://github.com/KonghaYao/peri/issues/447)，工具命名与懒加载归 [#449](https://github.com/KonghaYao/peri/issues/449)。下表 F-MCP-015/016 已单独按 `b71c5e82` 对齐 #449 合入状态；其余行仍沿用各自注明的证据基线。浏览器网关 `peri-browser-gateway` 已于 2026-11-26 移除，F-EXT-016 退役；其验收记录随组件删除，历史由 Git 保留。`peri-wasm` 与 Emscripten/WASM 构建链已于 2026-11-24 移除，F-PLATFORM-001/002 退役；原验收基线由 Git 历史保留。

### PostgreSQL 会话存储验收

在 `a2c0149` 上应用 PostgreSQL 替换后，在独立 PostgreSQL 17 与 19 实例运行了远程会话契约。
部署入口经过创建、关闭、换进程冷恢复、只读打开及删除会话树；另外验证了并发初始化、
事务回滚、取消、分页、MQ 与 OAuth。常规资源测试通过 623 项；两种 PostgreSQL 版本各通过 107 项显式远程测试。运行命令如下，数据库使用专用测试库。

```sh
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib
PERI_TEST_POSTGRES_URL=postgresql://postgres@localhost/peri_test \
  ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- --ignored sessions::remote::
```

在 PostgreSQL 19 上，SDK 已分别通过 TCP 和 Neon 兼容 HTTP 验证。两个独立 Bun 进程运行完整 WASM ACP，
前一进程通过 `session/new` 创建会话，后一进程通过 `session/load` 恢复了相同 ID。
完整 WASM ACP 的会话与生命周期 smoke 均通过，覆盖两个连接独立取消、正常关闭及重启后的回复回放。
打包 SDK 的 WASM 集成测试通过 2 项、24 个断言。
空投递队列的 WASM 修正也经过原生 `peri-agent --lib` 验证，1,603 项通过、5 项忽略。

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
| F-TASK-001 | 源码存在 | [契约](../design/session-async-tasks.md) | [后台任务注册](../../peri-agent/src/session/subagent/background_registration_test.rs)、[explorer 及旧名解析](../../peri-middlewares/src/mcp/agent_registry_resource_test.rs)、[派工提示词](../../peri-middlewares/prompts/sections/11_subagent.md) | explorer 派工和交付已做场景模拟，后台任务注册待复验 |
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
| F-MCP-015 | 源码存在 | [契约](../design/tool-system.md) | [别名回归](../../peri-middlewares/src/tool_search/execute_tool_test.rs)、[搜索契约](../../peri-middlewares/src/tool_search/search_tool_test.rs)；旧名仅解析兼容，不另注册 schema | 未复验 |
| F-MCP-016 | 源码存在 | [契约](../design/mcp-v5-single-exit.md) | [Loader](../../peri-middlewares/src/mcp/loader.rs)、[启动不连接断言](../../peri-middlewares/src/mcp/dynamic_initialize_test.rs)；外置 P01 的范围见 [测试规范](../standards/testing.md)，不以 ACP 动态注册替代 | 未复验 |
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
| F-PLATFORM-001 | 已退役 | [契约](../design/serverless-execution-identity.md) | 源码已随 Emscripten/WASM 构建链移除（见 Git 历史）；原缺口：WASM 运行的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
| F-PLATFORM-002 | 已退役 | [契约](../design/serverless-execution-identity.md) | 源码已随 Emscripten/WASM 构建链移除（见 Git 历史）；原缺口：执行环境身份的场景/断言与当前 SHA 尚待测试仓对账 | 未复验 |
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
| F-CLI-007 | 源码存在 | [契约](../design/peri-acp-protocol.md) | [源码](../../peri-tui/src/startup.rs)、[认证握手](../../peri-acp/src/host/stdio/auth_required_gate.rs)、[测试](../../peri-acp/src/host/stdio/auth_required_gate_test.rs)、[进程级握手](../../peri-tui/tests/it/acp_registry_auth.rs)；缺口：真实 Registry 端到端未复验 | 未复验 |
| F-SWARM-001 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/http/mod.rs)；[测试](../../peri-swarm/tests/it/http_contract_test.rs)；[证据](peri-swarm-local-dev.md)、[官方 SDK 对齐](peri-swarm-official-sdk-2026-11-21.md)；缺口：真实第三方调用方的门禁组合未验证 | 本地真实链路已验证；本轮补充既有行 create 在运行中/已预占 turn 上返回 `409 turn_in_progress` 且不写 user item（对齐 §9）；本次未重跑真实链路 |
| F-SWARM-002 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/domain/identity.rs)；[测试](../../peri-swarm/tests/it/actor_runtime_test.rs)；[证据](peri-swarm-local-dev.md)；缺口：跨机接管未验证；创建中断的 root id 核对由预置 `creating` 行测试覆盖，未注入真实进程崩溃 | 本地真实链路已验证；本次未重跑 |
| F-SWARM-003 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/http/sse.rs)；[测试](../../peri-swarm/tests/it/streaming_test.rs)、[items 分页](../../peri-swarm/tests/it/items_pagination_test.rs)、[create-stream 首轮](../../peri-swarm/tests/it/actor_runtime_test.rs)；[证据](peri-swarm-local-dev.md)、[官方 SDK 对齐](peri-swarm-official-sdk-2026-11-21.md)、[官方默认值对齐](peri-swarm-official-defaults-2026-11-21.md)；缺口：`gap` 后中断行为由单测覆盖，多订阅者容量未验证；官方帧未用真实 SDK 进程消费；历史失败 turn 回放为 `completed`；真实 Rivet engine 下 create-stream 未运行 | 无游标实时（含 Tail 不读行）、item 分页/行预算与 lookback 终态由契约测试覆盖；本轮补充 actor 路径官方首轮帧顺序、既有 idle 行 defer 与 `pendingInitial` 不跨进程（§9.3）；items 默认 `order=desc` 与 `include_obfuscation` 接受并忽略由契约测试覆盖；本次未重跑真实链路 |
| F-SWARM-004 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/domain/input_id.rs)；[测试](../../peri-swarm/src/domain/input_id_test.rs)、[幂等契约](../../peri-swarm/tests/it/http_contract/idempotency_keys.rs)；[证据](peri-swarm-agents-compat-item-granularity-2026-11-26.md)、[官方 SDK 追问、重连与幂等](peri-swarm-local-followup-2026-11-23.md)；缺口：Node e2e 未在合并基线运行；非 UUID 键的官方 SDK 探针来自 compat 分支 | 非 UUID `Idempotency-Key` 派生 UUIDv5、header/body 一致与重放 `existing` 由单测和 HTTP 契约覆盖；compat 分支探针 11/11，合并基线未重跑 |
| F-SWARM-005 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/store/postgres/mod.rs)；[测试](../../peri-swarm/src/store/postgres/integration_test.rs)、[两库部署](../../peri-swarm/tests/it/state_store_test.rs)；[证据](peri-swarm-postgres-state-2026-11-24.md)；真实 PG 契约测试默认 ignored；Linux CI 的 `Run PostgreSQL session and Swarm state contracts` 步骤用一次性 `postgres:17-alpine` 显式执行 store 契约与 `type=none` 删除回归（本地仍需 `PERI_TEST_POSTGRES_URL` / 两个 DSN） | 两库隔离与真实 turn 已在 PostgreSQL 上运行；本次未重跑 Loom 时代记录 |
| F-SWARM-006 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/actor/runtime.rs)、[入口解析](../../peri-swarm/src/http/request.rs)；[测试](../../peri-swarm/tests/it/mcp_injection_test.rs)、[请求解析](../../peri-swarm/src/http/request_test.rs)、[HTTP 契约](../../peri-swarm/tests/it/http_contract_test.rs)；[证据](peri-swarm-agents-compat-item-granularity-2026-11-26.md)、[本地开发](peri-swarm-local-dev.md)；缺口：真实第三方 MCP server 与真实官方 SDK 工具声明未端到端运行；`stdio` 无运行时 | 官方 `transport` 形状、平铺别名等价、authorization scheme 幂等、冲突 400 与 `stdio`/未知类型 422 由单测和 HTTP 契约覆盖 |
| F-SWARM-007 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../npm-packages/peri-sandbox/src/swarm/manager.ts)；[测试](../../npm-packages/peri-sandbox/tests/swarm/manager.test.ts)；[证据](peri-swarm-local-dev.md)；缺口：Linux SRT 与跨机调度未验证 | 真实 SRT + Bash 已验证；本次未重跑 |
| F-SWARM-008 | 源码存在 | [契约](../design/peri-swarm.md) | [生命周期回归](../../npm-packages/peri-sandbox/tests/task-activity.test.ts)；[用户模拟](peri-swarm-local-followup-2026-11-23.md) | 进行中请求及后台任务保活，自动回收后原客户端按需唤醒 |
| F-SWARM-009 | 源码存在 | [契约](../design/peri-swarm.md) | [中断恢复测试](../../peri-swarm/tests/it/actor_runtime/interrupted.rs)；[用户模拟](peri-swarm-local-followup-2026-11-23.md) | 历史只读，交互恢复并核对持久 MQ，避免重复执行已提交命令 |
| F-SWARM-010 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/bootstrap.rs)、[DSN 契约](../../peri-swarm/src/config.rs)；[证据](peri-swarm-postgres-state-2026-11-24.md)、[1000 空 session 复测](peri-swarm-1000-concurrent-sessions-2026-11-27.md)；缺口：跨机部署与 TLS 未验证；1000 个空 session 创建已验证（修复后 1000/1000、连接耗尽未复现），1000 个同时执行 turn 的 Agent 吞吐/长时内存未验证 | 本地启动序已记录；状态库改为 PostgreSQL DSN 后按 README 顺序启动；本轮补充 1000 并发空 session 修复前/后对比；未重跑旧记录 |
| F-SWARM-011 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/config.rs)、[bootstrap.rs](../../peri-swarm/src/bootstrap.rs)；[测试](../../peri-swarm/src/config_test.rs)、[bootstrap_test.rs](../../peri-swarm/src/bootstrap_test.rs)；[证据](peri-swarm-multi-replica-2026-11-21.md)；缺口：域名实际指向、双进程多副本、单 engine 节点、TLS 未验证 | 配置矩阵与 endpoint 私网校验单测、编译门结果见证据；M0 未执行；人工双进程矩阵未执行 |
| F-SWARM-012 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/http/mod.rs)、[子代理读取](../../peri-swarm/src/actor/runtime/subagents.rs)；[测试](../../peri-swarm/tests/it/session_resources_test.rs)、[子代理测试](../../peri-swarm/tests/it/subagents_test.rs)；[证据](peri-swarm-session-resources-2026-11-21.md)、[官方默认值对齐](peri-swarm-official-defaults-2026-11-21.md)、[subagents](peri-swarm-subagents-2026-11-27.md)；缺口：Loom 存量不做自动迁移；提供一次性离线导入脚本（`import_loom_state.py`，未对真实 Loom 生产快照验证）、engine 上的 delete/update 未验证；subagent 的 items/turns 仍 404，真实官方 SDK 未调用 subagents | 契约测试覆盖 list/update/delete/turns、POST update 与显式 404；真实 `openai@7.60.0` SDK 已对进程内网关执行 create/retrieve/update/turns.list 探针；持久未完成 turn 的 502 准入与取消状态持久化有单测；本轮补充 `prompt_cache_options:{}` 默认 30m、官方会话默认字段与环境 `none` 归一化、`{}` update 与无 body 同为 no-op；subagents list/retrieve 由真实临时 SQLite fixture 经 HTTP 覆盖 direct/nested/closed/hidden、分页与跨会话 404 |
| F-SWARM-013 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/actor/reader.rs)；[测试](../../peri-swarm/src/actor/reader_test.rs)、[审批与合并行](../../peri-swarm/src/actor/service_test.rs)、[回放与重连](../../peri-swarm/tests/it/actor_runtime_test.rs)；[证据](peri-swarm-agents-compat-item-granularity-2026-11-26.md)；缺口：历史多行真实回放、`afterSeq` 断线重连、审批折叠未实测 | ILOG-1..7 由进程内用例覆盖；真实单 turn `swarm_items` 3 行（2 message + 1 thought，0 持久 delta）；未验证项不构成验收 |
| F-SWARM-014 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/domain/official_item.rs)、[投影](../../peri-swarm/src/service/official_items.rs)；[测试](../../peri-swarm/src/domain/official_item_test.rs)、[投影用例](../../peri-swarm/src/service/official_items_test.rs)、[分页](../../peri-swarm/tests/it/items_pagination_test.rs)；[证据](peri-swarm-agents-compat-item-granularity-2026-11-26.md)；补充证据见下方官方 items 组分页验收；缺口：真实 SDK 只消费过消息项 | 四类官方投影、恢复行排除、悬挂调用状态、游标与回扫上限由用例覆盖；真实 `/items` 为 `raw=3 official=3` |
| F-SWARM-015 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/http/request.rs)、[受理](../../peri-swarm/src/actor/service.rs)、[错误](../../peri-swarm/src/domain/error.rs)；[测试](../../peri-swarm/tests/it/http_contract/official_cancel.rs)、[actor 取消](../../peri-swarm/src/actor/service_cancel_test.rs)、[flush 失败](../../peri-swarm/src/actor/reader_test.rs)；[证据](peri-swarm-agents-compat-item-granularity-2026-11-26.md)；缺口：真实宿主 30s 取消超时与重发失败的上游行为未验证；取消 turn 落库失败只记录日志 | 事件解析、202 收据、两类下发失败、管理冲突与取消 turn 保持 `cancelled` 由用例覆盖；真实宿主未跑 |
| F-SWARM-016 | 源码存在 | [契约](../design/peri-swarm.md) | [源码](../../peri-swarm/src/domain/official.rs)、[实时](../../peri-swarm/src/actor/reader.rs)、[回放](../../peri-swarm/src/service/mod.rs)；[测试](../../peri-swarm/src/actor/reader_test.rs)、[回放用例](../../peri-swarm/src/service/in_memory_test.rs)、[历史多行](../../peri-swarm/src/actor/service_test.rs)；[证据](peri-swarm-official-text-delta-2026-11-26.md)；缺口：reasoning delta 未实现；回放无 chunk 边界；历史同 itemId 多条 complete 行未测；added 首段去重未用真实客户端验证；关闭-广播窄竞态未复现 | 实时 85 chunk / 回放 1 条 336 字符全文 delta 在本地 Compose 单 turn 实测；`72c10cfe9` 上 lib 421 / it 115 / 27 ignored 与 PG 20 通过；未验证项不构成验收 |
| F-SWARM-017 | 源码存在 | [契约](../design/peri-swarm-dashboard.md) | [源码](../../peri-swarm/dashboard/src/main.rs)、[状态机](../../peri-swarm/dashboard/src/app.rs)、[HTTP 客户端](../../peri-swarm/dashboard/src/api.rs)；[测试](../../peri-swarm/dashboard/src/app_test.rs)、[配置](../../peri-swarm/dashboard/src/config_test.rs)、[API](../../peri-swarm/dashboard/src/api_test.rs)、[鼠标命中](../../peri-swarm/dashboard/src/ui/mouse_test.rs)、[live smoke（ignored）](../../peri-swarm/dashboard/tests/live_gateway.rs)；缺口：人工 TUI 走查未覆盖发送、审批、退出登录；自动 live smoke 只覆盖只读 health + 列表；无像素快照；500 条上限与长时间轮询未压测；真实 401/409/502 信封未验证；Session log 分页器仅在本地 stub 数据源上做过 pty 点击验证，未在真实网关验证；固定宽度分页器与本地排序已在本地 120 条 stub 上完成 pty 验证，未在真实网关验证。观测与查询见 F-SWARM-018 | crate check/test/clippy/fmt 与静态门通过；组件 548 + 74、dashboard 333 passed / 0 failed / 1 ignored（观测页加入后的当前全量）；真实网关只读 live smoke 通过；真实 pty 人工走查列表/设置/详情/标签点击与一次 create/edit/delete 通过，发现并修复详情页标签点击偏移（回归测试覆盖）；按钮改为 Primary 填充块 / Secondary·Danger 纯文字，抓屏不再出现方括号按钮；Session log 的 `Load more` 已替换为客户端分页器（每页 10 条，末页 `Next ›` 按 50 条步长追加至 500）；分页控件改为固定字符宽度（页码带预留最宽窗口，翻页时 `Next ›` 不移动），列表改为按 `last_active_at` 降序、同值 `created_at` 降序稳定排序，二者由单元/渲染命中测试覆盖；固定宽度分页器与本地排序另在本地 120 条 stub 上完成 pty 走查（12 页时 `Next ›` 同列、最新在前、末页不追加） |
| F-SWARM-018 | 源码存在 | [dashboard 契约](../design/peri-swarm-dashboard.md)、[网关用量契约](../design/peri-swarm.md) | [usage 领域](../../peri-swarm/src/domain/usage.rs)、[usage 聚合](../../peri-swarm/src/service/usage.rs)、[usage 投影](../../peri-swarm/src/actor/reader.rs)、[OTel](../../peri-swarm/src/telemetry.rs)、[HTTP 遥测](../../peri-swarm/src/http/telemetry.rs)、[部署 compose](../../deploy/docker-compose.yml)、[OpenObserve 初始化](../../deploy/init/openobserve-users.sh)、[dashboard 观测客户端](../../peri-swarm/dashboard/src/observability.rs)、[查询规则](../../peri-swarm/dashboard/src/query.rs)；[测试](../../peri-swarm/src/domain/usage_test.rs)、[遥测配置](../../peri-swarm/src/telemetry_test.rs)、[访问日志](../../peri-swarm/src/http/telemetry_test.rs)、[观测客户端](../../peri-swarm/dashboard/src/observability_test.rs)、[指标页状态](../../peri-swarm/dashboard/src/app/metrics_test.rs)、[查询页状态](../../peri-swarm/dashboard/src/app/query_test.rs)；缺口：Agent metrics 未对真实网关做端到端页面验证；Query 未对真实 OpenObserve 做端到端页面验证；完整 Compose 栈未跑；观测页无人工 TTY 走查；Postgres usage SQL 未对真实数据库运行；p95 为 histogram 桶内近似；OSS dashboard 账号可写；`openobserve-init` 不轮换密码、默认保留策略未配置；两副本 `service.instance.id` 未验证；5s shutdown 上限不完全可靠 | 阶段 2 网关 460 unit + 127 integration passed / 29 ignored；阶段 3 480 unit + 128 integration（608 passed）/ 29 ignored，OTLP stub 收到三类信号且无敏感泄漏；阶段 5a/5b/6a dashboard 240/290/333 passed / 1 ignored；隔离 compose project 验证 OpenObserve healthy、初始化幂等、OTLP 写入 200 可搜、dashboard 账号写入探针 200；未验证项不构成验收 |

### PR #614 Swarm 补充验收

基线为 `bca78fb1b` 加本次补丁。以下均用本地 fixture，未重跑真实 Compose / Rivet 链路。

- F-SWARM-001/004：用 [准入回归](../../peri-swarm/src/actor/service_admission_test.rs) 暂停第一条 user item 的写入，验证竞争输入无收据且可在空闲后用原键重试。
- F-SWARM-003/014：该基线的 [分页回归](../../peri-swarm/tests/it/items_official_pagination_test.rs) 当时保持首个原始页和游标，只前探 `data`；实际 `openai@7.60.0` 对进程内 HTTP 网关迭代，升降序均能跨过纯内部页并读到完整答案。组分页的新契约见下一节。
- F-SWARM-016：[广播回归](../../peri-swarm/src/actor/reader_broadcast_test.rs) 验证发送期间关闭方不能取走 stream、引入帧不重复，以及无订阅者时完成行补发；不代替真实客户端并发验证。

### F-SWARM-003/014 官方 items 组分页验收

基线为 `b9fd1a293` 加本次补丁。测试使用进程内 HTTP 与 Memory store；SDK 探针使用真实
`openai@7.60.0` 访问本地监听端口。本轮未覆盖真实 Postgres、Rivet 或 Compose。

1. [跨页整组](../../peri-swarm/tests/it/items_official_pagination_test.rs)：升序 `afterSeq` 在
   两代 assistant 帧之间时仍返回全组、正文与整页读取一致，`nextCursor` 为组最大 `seq`；降序续页不重选该组。
2. [逐页投影回归](../../peri-swarm/tests/it/items_official_pagination_test.rs)：十组 thought
   多帧与 receipt 交错，`limit=11` 从 `nextCursor=21` 续读与直接 `after=31` 的 answer 正文和 `turn_id` 一致。
3. [请求预算](../../peri-swarm/tests/it/items_official_pagination_test.rs)：第 101 页才有投影时
   返回 `422 items_scan_limit`、无部分页或游标，第 100 页可成功；累计或单组超过 2000 行不返回残组。
4. [读取失败](../../peri-swarm/src/service/official_items_paging_test.rs)：第二存储页或 turn 历史读失败均整页
   返回 `502`；首页有投影不探页；空页游标不推进时返回扫描错误。
5. [SDK 分页探针](../../peri-swarm/scripts/sdk_items_pagination_test.mjs)：升降序分别用原生 `nextCursor`、
   SDK `data` 末项 id 或交替游标遍历，官方 id 不重不漏；SDK 可重扫内部原始行。
6. [SDK 会话 E2E 断言](../../peri-swarm/scripts/sdk_sessions_e2e.mjs)：`data.length <= min(limit, 原始组数)`、
   `data` id 唯一且来自 `items`；有续页时 `nextCursor` 等于末组最大 `seq`，否则为 `null`。
   已用提取函数体的最小 harness 实际执行 `assertOfficialItems`：正常合并页不抛错、
   `data.length <= min(limit, rawItemCount)`、`nextCursor` 为末组最大 `seq`；未跑真实网关。
7. [升序并发追加](../../peri-swarm/src/service/official_items_paging_test.rs)：请求开始 `lastItemSeq=2`，
   每次读页前追加一条独立 receipt；响应 200、只读 1 页、游标停在 2 且不追尾（上限为 100 页）。
8. [组增长去重](../../peri-swarm/src/service/official_items_paging_test.rs)：首读后向无正文 thought
   追加第 4 帧并追加 answer 5；同一响应 `seq=[2,3,4,5]` 唯一。1000 行组增长后只新增 1 帧，
   重复行不把纳入量推到 2000 以上，也不误报 `items_scan_limit`。
9. [SDK 重扫预算差异](../../peri-swarm/tests/it/items_official_pagination_test.rs)：
   `official_pagination::openai_sdk_iterates_past_internal_only_pages`（`#[ignore]`，需 Node.js 与
   `openai@7.60.0`）的脚本在 7 行 fixture 上让真实 SDK 迭代，断言 SDK 从 `data` 末项 id 续页
   会重读内部 receipt 原始行（原始 `seq` 序列出现重复读取）；不覆盖纳入预算。
   SDK 续页因此可能比原生 `nextCursor` 更早触发 `items_scan_limit` 属基于重扫行为的推断，
   本轮未实测 2000 行预算边界，也未在真实 Postgres、Compose 或全链路 E2E 上验证。

### F-TASK-001 补充验收

基线为 `a2c0149` 加本次补丁。在受控本地文件和网页工具上，Luna 将 14/14 个调查委派给 explorer，另 4/4 个已知位置查询或改代码的对照任务均选对执行者。四个交付任务覆盖本地追踪、网页查证、混合核对和网页不可用。首次 3/4 满足取证要求，同一子会话补回混合核对的源码定位后达到 4/4。该模拟未测生产流量、实际联网或后台任务生命周期。

### F-SWARM-017 补充验收

基线为 `b9fd1a293` 加 `feat/peri-swarm-dashboard` 的未提交变更。本行覆盖
`peri-swarm-dashboard` crate 的自动检查、一次真实网关只读冒烟，以及一次真实 pty 上的
人工 TUI 端到端走查。

```bash
./scripts/cargo-rmcp-patched.sh check --locked -p peri-swarm-dashboard --all-targets
./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm-dashboard
./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-swarm-dashboard --all-targets -- -D warnings
cargo fmt --check
bash scripts/check-layer-imports.sh
python3 scripts/check-feature-catalog.py
bash scripts/check-file-size.sh
git diff --check
```

结果：check/clippy/fmt/layer-imports/catalog/diff 通过；crate 测试 187 passed / 0 failed /
1 ignored（`tests/live_gateway.rs`）。`bash scripts/check-file-size.sh` 在 `PATH` 缺少 `rg`
时无法启动；注入 VS Code 自带 ripgrep 后退出 1，67 个超限文件全部为仓库既有文件，本次
新增/修改文件均低于 1000 行（最大 993 行）。

真实网关只读冒烟（2026-11-27）：对本地 Compose 网关 `http://127.0.0.1:8080`，从
`deploy/.env` 读取凭据且全程不打印 key，运行
`cargo test -p peri-swarm-dashboard --test live_gateway -- --ignored`。
`health_then_list_sessions` 通过（1 passed / 0 failed）：`GET /healthz` 返回 `ok`，
携带 Bearer 的 `GET /v1/agents/sessions?limit=1` 成功，`x-peri-swarm-contract` 为 `1`。
该冒烟未执行任何写操作，也未覆盖真实 401/409/502 信封。

人工 TUI 端到端走查（2026-11-27）：真实 pty（tmux 140×40，独立
`HOME=/tmp/peri-dash-e2e`）连接同一网关。首启 onboarding 预填 Base URL，API key 粘贴后
显示为掩码且未出现在抓屏中，提交后配置目录 `0700`/文件 `0600` 并进入 dashboard；列表
显示真实 8 条会话与状态 chips，Settings 的 `[ Test connection ]` 返回
“Connection OK in 4 ms”。鼠标切换侧栏、打开会话详情、点击 Timeline/Turns/Info 均生效，
键盘 Left/Right 仍可循环。写路径经 `[ Create ]` 创建 `e2e-check-2711`，用编辑表单把
model 改为 `stub-model` 并生效，`[ Delete ]` 确认覆盖层外点击被吞掉，删除成功后再次
查询列表确认该会话不存在（8 条，计数 0）。走查发现详情页标签点击区域相对绘制文字左移
（`Info` 偏移 5 列而不可点击），已修复为与 `Tabs` 渲染一致的 1 列左内边距和 5 列标签
间距，新增回归测试 `tab_hit_regions_cover_the_drawn_labels`，并在真实终端按可见标签
首列点击 Info/Turns/Timeline 复验通过。

明确缺口：自动 live smoke 只覆盖只读 health + 列表，写路径仅有上述一次人工
create/edit/delete 记录；未人工走查发送消息、审批与退出登录；没有像素快照；500 条上限
与长时间轮询未压测；真实 401/409/502 信封未验证；未做 release 构建。以上缺口不构成
验收。

### F-SWARM-017 Session log 排序与固定宽度分页器验收

基线为 `1bbb7dec6` 加未提交的组件改造、按钮外观、分页器与本地排序改动，未创建提交。本轮
未运行真实网关、未读写 `deploy/.env` 或用户配置；另外用本机 120 条只读 stub 完成了 pty
走查（见下）。

```bash
./scripts/cargo-rmcp-patched.sh fmt --check
./scripts/cargo-rmcp-patched.sh clippy -p peri-tui-components -p peri-swarm-dashboard --all-targets -- -D warnings
./scripts/cargo-rmcp-patched.sh test -p peri-tui-components
./scripts/cargo-rmcp-patched.sh test -p peri-swarm-dashboard
git diff --check
git status --short Cargo.lock
```

结果：fmt/clippy/diff 通过，`Cargo.lock` 无改动；`peri-tui-components` 548 个库测试
（其中 21 个 `pagination`，比上一轮 +3）与 74 个 storybook 测试通过；dashboard 217
passed / 0 failed / 1 ignored（比上一轮 +4；`tests/live_gateway.rs` 需真实网关）。

覆盖内容：

1. 组件：页码窗口（全部显示、中段/首段/末段省略号、页码不重不漏、当前页必在其中）、
   `prev` 在首页禁用、`next` 在末页且无 `has_more` 时禁用、`has_more` 启用末页的
   `next`、每个控件矩形与绘制文字等宽、页码带内部相邻控件间隔 1 列、当前页为
   accent-on-selection 加粗、窄行回退到 `prev 当前/总数 next`、极窄裁剪、零面积与
   `page_count == 0`、页码钳制；固定宽度：页码控件占 `digits` 列（含当前页）、省略号
   固定 1 列、`page_count ∈ [1..=30]` 的所有当前页窗口宽度不超过预留宽度、
   `page_count ∈ {1,6,7,8,9,20,50}` 翻页时 `Next` 列与整行宽度不变、窄行指示器宽度
   恒定。
2. 状态：每页 10 条的切片与页码、空集合算一页、过滤/搜索回到第 1 页、已加载页之间翻页
   不发请求、末页 `next` 按 50 条步长抬升 limit 并在新数据到达后落到 pending 页、
   pending 页在旧的在途响应到达后仍保留、数据不足时继续等待、集合已取完时丢弃 pending、
   失败清除 pending、集合缩小时的页码与光标钳制、`PgUp`/`PgDn` 仅在内容区生效、
   行光标为页内坐标、最多 500 条；排序：旧→新输入在 `apply_page` 后最近活跃的排第一、
   25 条乱序日期第 1 页为最新 10 条且末页为最旧 5 条、`last_active_at` 相同保持输入
   顺序、`last_active_at` 相同时按 `created_at` 降序。
3. 命中与会话页：分页器每个启用控件的命中区与绘制文字一致；点击页码与 `Next ›` 后表格只
   绘制当前页；首页的 `‹ Prev` 与末页无更多数据时的 `Next ›` 不注册命中区；页脚提示包含
   `PgUp/PgDn page`，帮助覆盖层新增一行分页说明；会话页不再出现 `Load more`。

### pty 走查（本地 120 条数据源）

在 tmux 中以 140×40 运行重建后的二进制，`HOME` 指向临时目录，数据源为
`127.0.0.1:18082` 上按 `limit` 返回 120 条会话的只读 stub（`limit < 120` 时
`has_more = true`，数据按最旧→最新返回），未使用网关凭据。观察到：

1. 本地排序生效：第 1 页为 `sess-120..111`，第 12 页为 `sess-010..001`，每页 10 条。
2. 12 页时页脚显示 `‹ Prev 1 … 10 11 12 Next ›`；用 `PgDn`/`PgUp` 走完全部 12 页，
   `Next ›` 始终位于同一列（第 61 列），当前页为 2 列宽的 accent 填充块。
3. 在第 5 页点 `Next ›` 只发一次 `limit=100` 请求（stub 日志确认），落到第 6 页；
   在第 10 页点 `Next ›` 只发一次 `limit=150`，落到第 11 页并显示 12 页。
4. 末页 `Next ›` 为暗色；点击它之后只有既有的轮询请求，没有追加请求。
5. 切换 status chip 与输入搜索都回到第 1 页；清空搜索后第 1 页仍为 `sess-120`。

明确缺口：固定宽度分页器与本地排序尚未在真实网关数据上验证；终端高度不足时（如 79×24）
需要滚动才能看到分页器（与原 `Load more` 相同，本轮未复测）；500 条上限下的分页器渲染
性能未压测；`pending_page` 依赖后续响应包含该页，若网关在 `has_more = true` 时持续返回
不足的数据，等待会一直保留到下一次刷新（不会自动清空，也不会重复发请求）。

### F-SWARM-018 观测与查询验收

基线为 `feat/peri-swarm-observability` 的提交序列 `97270b03e`（导航与空页）→
`a9e544afb`（usage + Agent metrics）→ `9e15c3fae`（OTel）→ `92d61f52d`（OpenObserve
部署）→ `093ec8a22`（观测配置与客户端）→ `8a78dcd4a`（Core/Agent/Request 页面）→
`23e103064`（Agent metrics 15s）→ `4fde4431f`（Query 页）。本节记录各阶段门禁与隔离
部署探针；本节文档变更本身未重跑这些检查。

1. **导航**：侧栏 13 项，分组 Monitor 6 / Resources 4 / Platform 3。`Page::is_implemented()`
   为 SessionLog、Settings、CoreMetrics、AgentMetrics、RequestLog、Query。Sandbox metrics
   和 Agents、Artifacts、Vaults、MCP、Automations、Nodes 仍是有意空页；Sandbox metrics
   的文案明确说明网关只报告 sandbox 类型/id，没有 CPU/内存/资源指标。旧页 Environment
   templates、Skills、Files、Projects and keys 已删除；没有 Overview、SSE 或 demo/mock 数据。
2. **usage 与 Agent metrics**：网关在 `token_stats=true` 时投影并累加 `usage_update`；
   回放过滤保留，重连不重复计数。`input_tokens = inputTokens - cacheReadTokens -
   cacheCreationTokens`（饱和减），`total_tokens` 为 input/output/cache_read/cache_creation
   之和；`context_used`/`context_size`、`model`、`request_id` 取最新观测，`calls` 累加；
   缺 `_meta` 时只写 context，不伪造 token。每次非 replay usage 立即 upsert，后续写入用
   COALESCE 保留已存值；turn 终止写累计值，实时 `turn.completed`/`turn.failed` 与
   `turns.retrieve` 一致。`sessions.retrieve`（及 create/update 回读）聚合该会话的持久
   turn；`sessions.list` 保持 `usage:null`。
   `GET /v1/agents/metrics?start_time=<us>&end_time=<us>&limit=N` 在 auth guard 内：默认
   最近 24 小时、窗口上限 31 天、`limit` 1..=500、`start_time > end_time` → 400；按会话
   返回 input/output/cache read/cache create/total/turns/calls/last_active_at，`totals`
   覆盖整窗，`truncated` 表示扫描达到上限；没有任何 usage 事实的会话不出现，不显示 0。
   已知缺口：Postgres usage SQL 未对真实数据库运行；`usage_update` 合并进 root turn 的
   边角未单测。
3. **OTel**：标准 OTel 环境变量驱动，未设置 endpoint 或 `OTEL_SDK_DISABLED=true` 时
   no-op（不建连、不报错）。traces、metrics、logs 三类信号走 OTLP/HTTP；`service.name`
   默认 `peri-swarm`，`service.instance.id` 为 `HOSTNAME:pid`，不是共享 machine-id。
   Provider 是进程局部的；`main` 持有 guard 到 `run()` 返回之后，Drop 时 5 秒有界 flush
   （current-thread runtime 下该上限不完全可靠）。HTTP middleware 不读/不缓冲 body；
   `/healthz` 完全跳过；`/events` 不计入延迟直方图。指标为
   `peri_swarm.http.server.requests` 与 `peri_swarm.http.server.duration`。访问日志只含
   method、route、status、duration_ms、session_id、request_id；不记录 header/query/body/
   `Authorization`/`Cookie`/幂等键。阶段 3 的 OTLP stub 收到三类信号，未发现敏感信息泄漏，
   SSE 未被包裹。
4. **OpenObserve 部署**：镜像 `openobserve/openobserve:v1.2.0`（由 `OPENOBSERVE_TAG`
   固定）；一次性 `curlimages/curl:8.11.1` 初始化容器；镜像无 shell/curl/wget/nc/busybox，
   healthcheck 用 `/openobserve node metrics`；命名卷 `openobserve-data`；默认发布
   `${OPENOBSERVE_PUBLISH_HOST:-127.0.0.1}:${OPENOBSERVE_PORT:-5080}:5080`；
   `restart: unless-stopped`、`no-new-privileges`。初始化脚本幂等创建
   `peri-swarm-ingest`（OTLP 写入）与 `peri-swarm-dashboard`（dashboard 查询），两者在
   OSS v1.2.0 都是 admin。网关没有硬 `depends_on` OpenObserve，OOM/重启不阻塞网关。
   隔离 compose project 验证：`compose config --quiet` 通过；`openobserve` healthy；
   初始化幂等；错误 root 密码 401 且非零退出；OTLP logs 写入 200 且可搜；dashboard 账号
   写入探针 200；`down -v` 清理成功。未跑完整栈、默认 5080 与保留策略。
5. **dashboard 页面**：Core metrics 使用 OpenObserve PromQL（v1.2.0 的 metrics 流对
   `_search` 返回 400）；由于 `rate`/`increase`/`delta` 返回空矩阵，客户端按累计样本的
   正向增量计算窗口值；窗口 15m/1h/24h（默认 15m），页面可见时 15 秒轮询；错误率只统计
   5xx，p95 用 histogram `le` 桶增量插值，落 `+Inf` 或无观测显示 `—`，并展示实际样本
   覆盖范围。Agent metrics 走网关 `/v1/agents/metrics`，不依赖 OpenObserve；`limit=500`、
   每页 10 条、默认 15m，`totals` 汇总，`truncated` 提示覆盖最新 500 会话，Cache 为
   read+creation，缺失显示 `—`。Request log 用 SQL 查 `peri_swarm_logs`；`_timestamp` 为
   微秒，`ORDER BY _timestamp DESC`，每页 10 条，`total` 只表示页级行数，界面只显示
   `Page N`。Query 为手动 SELECT-only：非空、≤8 KiB、无 NUL、首 token 为 `SELECT`
   （大小写不敏感）、只允许一个末尾分号且正文不得再有分号；窗口 15m/1h/24h（默认 1h），
   20 行/页、最多 100 行，不轮询，只显示 `Page N`；列优先响应 `columns`（兼容字符串或
   `{name}` 对象），否则取行 JSON 键并集，null 显示 `—`，嵌套值显示紧凑 JSON，长值 96
   字符截断，超宽列裁剪并提示 `Showing N of M columns.`。Core/Agent/Request 15 秒轮询，
   仅页面可见且无 overlay 时请求，不重叠，generation 丢弃旧响应，失败保留上次成功数据并
   标 stale，退避 3→6→12→24→30 秒；离开页面暂停。未配置 `[observability]` 时 Core/Request/
   Query 显示 “Observability is not configured.” + Settings 按钮，Agent metrics 仍工作，
   不伪造数据。Sandbox metrics 为空页，文案说明没有资源级指标。
6. **安全边界**：dashboard 用专用 admin 账号直连 OpenObserve（不用 root）；OSS v1.2.0 的
   create-user API 只接受 admin/service_account，viewer/editor 返回 400；ingestion token
   只写；没有只读 API key/endpoint。dashboard 账号可写，Core 的 PromQL 与 Request/Query
   的 SELECT-only 都只是防误操作，不是安全边界。配置依赖 `0600` 文件、内网/loopback、
   专用账号与密码轮换；密码遮蔽、保存后不回显，`Debug` 与错误脱敏，客户端禁用重定向并
   设 10 秒超时。
7. **门禁命令与结果**：

```bash
./scripts/cargo-rmcp-patched.sh fmt --check
./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-swarm --all-targets -- -D warnings
./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm
./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-swarm-dashboard --all-targets -- -D warnings
./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm-dashboard
```

阶段 1 dashboard 217 passed / 1 ignored；阶段 2 网关 460 unit + 127 integration passed，
29 ignored（21 需 `PERI_TEST_POSTGRES_URL`，8 外部/Rivet），fmt/clippy 通过；阶段 3
480 unit + 128 integration（608 passed）/ 29 ignored，fmt/clippy 通过；阶段 5a/5b/6a
dashboard 分别为 240/290/333 passed、各 1 ignored，fmt/clippy 通过。以上为阶段记录，
本轮文档变更未重跑。

明确缺口：真实网关 Agent metrics 端到端、真实 OpenObserve Query 端到端、完整 Compose
栈、观测页人工 TTY 走查、Postgres usage SQL 实跑、p95 精确性均未验证；OSS dashboard
账号可写；`openobserve-init` 不轮换密码、默认保留策略未配置；两副本
`service.instance.id` 未在真实 Compose 下核对；OTel 5 秒 shutdown 上限不完全可靠。
以上缺口不构成验收。
