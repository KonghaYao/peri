# Workflow as MCP 实施

- 状态：实施与收口完成（2026-10-11）——全仓编译绿；行为/回归验证由用户承担。「保留项与未决」见文末。
- 日期：2026-10-11。
- 范围：`peri-workflow`、`npm-packages/@peri-workflow`、`peri-middlewares`（mcp/builtin、assembly）、`peri-acp`（host、requests）、`peri-acp-types`（builtin_mcp、meta_harness）、`mcp-packages/workflow`（新）、`peri-agent`（session factory 装配点）。
- 权威依据：[docs/design/workflow-mcp.md](../../docs/design/workflow-mcp.md)；现行设计 [docs/design/workflow.md](../../docs/design/workflow.md) 在本实施完成前仍描述旧链行为。
- 相关：`spec/issues/2026-10-11-module-dependency-boundary-audit.md`（Workflow 边界两项待裁决：journal 归属保持现状；`peri-resources` 门面**未删**，保留未决，见文末）。

## 裁决记录（2026-10-11）

1. 形态：builtin MCP 实例（同进程 `rmcp::serve_server` + duplex，per-session 池），Node 引擎保留为实例执行后端。
2. agent 回调：MCPP Channel 语义——任务作为 outbound 资源发布，宿主消费者订阅执行，经 host-only 工具回填；Node wire 的 `agent/run` 请求/响应改为 `agent/task` + `agent/result` 通告与回填。
3. 产出：权威设计 + 实施到完整 + 对抗修复，工作在独立 worktree（`.worktrees/workflow-as-mcp`，分支 `workflow-as-mcp`）。
4. wasm 目标（用户裁决，2026-10-11 追加）：wasm 产物不打包 workflow，workflow 实现不设 wasm 变体——`assembly/workflow_wasm.rs` stub 与 `assembly.rs` 的 cfg 双分支删除，workflow 装配模块随 `not(emscripten)` 依赖段一并移出；peri-acp 侧本地运行时路径（workflow_agent / 会话链创建与绑定 / `workflow_middleware_factory` 字段）同目标门控，`mcp__workflow__*` MCP 侧通路（`host/requests/workflow.rs` 四 handler 经池协议面）不受影响。

## 实施批次

对抗审查（2026-10-11）后的顺序原则：先落 MCP 面与 `WorkflowRuntime` 领域接口（保留旧装配、双轨可验证）→ 原子切换装配/策略并删除旧链 → wire 改造独立批次（以领域接口为边界，不与 MCP 面切换捆绑）。

| 批次 | 内容 | 状态（2026-10-11 收口） |
| --- | --- | --- |
| P0 | 权威设计、索引登记、本 issue、设计对抗审查与修订 | 完成 |
| P1 | builtin 注册表：`WORKFLOW_TOOLS`、实例条目、保留名、`model_visible` 字段与桥接过滤 | 完成 |
| P2 | `mcp-packages/workflow`：`WorkflowRuntime`（start_run/kill/resume/status/任务表）+ `WorkflowMcpServer`（tools/resources/subscriptions） | 完成（含实施对抗审查 F1–F7 修复） |
| P3 | 会话装配：`WorkflowInstanceInput`、实例 spawn、`WorkflowTaskConsumer` 就绪与订阅 | 完成（旧 middleware 已随 P5 删除） |
| P4 | ACP handler 经池迁移（`McpPoolPort::call_tool/read_resource/subscribe`）；resume 由内核完成 | 完成（含 resume-only 分支补齐） |
| P5 | 原子切换：MetaHarness 键迁移、fork/core_tools、装配切换、删除旧链（槽位/端口/旧 middleware 目录） | 完成（含「命中关闭集即不 spawn」用户裁决特例） |
| P6 | Node wire：`agent/task` + `agent/result`；消费者执行接线（任务闭环） | 完成 |
| P7 | 收口：文档/索引同步、遗留清理、最终编译检查 | 完成（行为验证由用户承担） |

## 验收

- 设计文档第 8 节全部条目（含行为矩阵：启动失败释放 reservation、取消竞态无僵尸 Node、long-output、ADLC 产物、并行与 limits）。
- 删除清单完成后，`scripts/check-layer-imports.sh` 与相关契约测试通过，且 `peri-resources` 不再依赖 workflow。
- 对抗性审查（设计阶段与实施后各一次）发现的问题逐项收敛或登记。

## 风险与注意事项

- `"WorkflowMiddleware"` 键同时是链槽位名与目标 policy key：两表不相交测试强制二选一，字符串沿用保持用户配置兼容；迁移需同步 `app.rs` 校验与测试字面量。
- `McpPoolPort` 无 `call_tool`/`read_resource`：ACP 四 handler 迁移前需先定池调用端口面。
- Node wire 改造影响 resume/kill/超时语义：任务与回填需先持久化后可见，禁止 detached 执行。

## 实施结果与遗留（2026-10-11 收口）

**验证口径**：轻验证——受影响 crate `check --all-targets` 全绿、`scripts/check-layer-imports.sh` 0 违规、`git diff --check` 干净、单文件 ≤1000 行；行为与回归验证由用户承担。实施对抗审查（V1–V3）发现 1 阻断 + 6 重大（F1–F7）已逐项修复。

**文档同步**：`ARC-CAPABILITY-CLOSURE-001` 特例注记；本实施记录；npm `DESIGN.md` 的 P6 协议（`agent/task`/`agent/result`、taskId 等待表、killed→`WorkflowAbortedError`）；`docs/design/workflow.md` 迁移提示与 `docs/design/README.md` 索引；`docs/code-index/` 指针级同步（peri-workflow / peri-acp / peri-acp-types / peri-middlewares / peri-resources / mcp-packages）。

**保留项（未自行裁决）**

1. journal 落盘归属：保持现状（`.claude/workflow-runs/`）。
2. Resources→workflow 边界：`peri-resources/src/workflow.rs` 门面**未删除**（并新增 `runtime` re-export），与设计 §2.1-3/§7 删除清单存在差异；待 `2026-10-11-module-dependency-boundary-audit.md` 裁决。
3. 关闭语义：workflow 命中关闭集即不 spawn（用户裁决；已实现，并在 `ARC-CAPABILITY-CLOSURE-001` 注记该物理关闭特例）。

**未决**

1. `WorkflowStatus` 的 limits 使用字段无运行期事实源（未实现）。
2. 取消仅工具级文本（无协议级错误码）。
3. `WorkflowAgentResult` 回填未 claim 的 `published` 任务判协议级 `invalid_params`（口径待确认）。
4. P4 kill/start 解读依赖工具文本（契约已在 `peri-acp/src/host/requests/workflow.rs` 文件头注明）。
5. 实施对抗审查 OQ3：`ProgressObserver` 回调期间仍持 `commit` 串行锁——接口说明已收紧（`peri-workflow/src/progress.rs`：回调不得重入提交/触发 `commit`，否则同线程自锁）；不改行为。
6. workflow v2 事件经 `Controller` 统一发射的接线留待单独裁定（现状 `publish_hook: None`，见 `peri-acp/src/host/requests/session_lifecycle.rs` 注明）；孤儿构造器 `build_publish_hook` 已删除（Git 可追）。

**P6 语义差异（相对旧链，已复核）**

1. `maxToolCalls` 超限：被超限 agent 的结算 promise 返回真实结果（旧链为 aborted 错误）；run 终态仍按 breach 投影 `failed`（`workflow/done` 取 breach 文本）。
2. 执行预算缺失（run 无预算条目）时容量门跳过并记 warn（liveness 优先）。

**死代码复核（P6 报告项）**

- `peri-workflow/src/rpc.rs` 的 `register_agent`/`deregister_agent` 已无生产调用（仅 `rpc_test.rs` 单测引用）；`kill_agent` 仍有生产引用链（`WorkflowKill` → `runtime.kill_agent` → `runner.kill_agent` → `channel.kill_agent`），按「有引用则保留」处理。P6 后不再有登记方，引擎侧 kill 为惰性路径；单 agent kill 由任务表栅栏承担（`runtime.rs` `cancel_agent`）。
