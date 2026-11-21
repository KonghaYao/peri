# P1：Workflow 终态把“未请求交付”投影成交付阻塞

**状态**：已确认，待修复。
**优先级**：P1（issue #522 原标 P2；按设计决策与用户要求走 P1 流程）。
**类型**：可观测性 / 状态投影语义。
**创建日期**：2026-11-21。
**来源**：用户 issue #522（23 个本地 run 证据）+ 两路对抗审查设计（`peri-522-p1-workflow-projection.md`）。
**确认方式**：explorer 只读确认 + 主 agent 复核，基线 `origin/main` @ `39eddb1dbad52941d3cfbda9b64fd6efc599a2f2`。

## 确认证据

- `peri-workflow/src/journal/git_baseline.rs:80-88`：`capture_for_intent` 在 `intent.is_none()` 时直接返回 `Ok(None)`，不采样。
- `peri-workflow/src/runner/terminal.rs:67-74`：`(None, None)` 落入 `_ => PostProcessingStatus::Blocked`。
- `peri-workflow/src/runner/terminal.rs:78-88`：`write_intent.is_none()` 直接令 `delivery = DeliveryStatus::Blocked`。
- `peri-workflow/src/runner/terminal.rs:135-138`：`finalize_workflow` 只在 `git_baseline` 为 `Some` 时校验；无基线即无观测。
- `peri-acp-types/src/workflow.rs:363-384`：`PostProcessingStatus` / `DeliveryStatus` 为封闭 serde 枚举；新增枚举值会让旧二进制读新 `state.json` 失败。
- `peri-acp-types/src/workflow.rs:405-422`：`delivery == Blocked` 时 `WorkflowOutcome::project` 产出 `Blocked`。
- 用户证据（issue #522，未在本次修复环境复现）：`01a1f862` 3.77 秒 completed、返回合规 JSON、`write_intent=null`，投影仍为 `delivery=blocked`；15 个 completed 中 14 个 delivery=blocked，其中 12 个无引擎可见失败阶段。

## 根因链

1. 触发：workflow 启动时未声明 `writeIntent`（含只读任务与省略意图的默认调用）。
2. 机制：`capture_for_intent(None)` 不采样 → `finalize_workflow` 不校验 → `project_postcondition(None, None)` 落 `Blocked`；`delivery` 分支再次因 `write_intent.is_none()` 强制 `Blocked`。
3. 后果：执行已完成且无任何失败证据的 run 被投影成“交付阻塞”，通知文案与 `agent_facing_success` 均按失败面呈现（`peri-acp-types/src/workflow.rs:717-735,767-787`）。

推断（未单独复现）：多数只读 run 不声明 intent，因此该分支是 14/15 blocked 的主要来源。

## 影响面

- 用户影响：completed run 被读成交付阻塞；新界面无法区分“未请求交付 / 已验证无变化 / 观测不完整 / 无法归因的变化”。
- 受影响模块：`peri-workflow` 终态投影与 Git baseline；`peri-acp-types` 结果 wire；`mcp-packages/workflow`、`peri-tui` 的只读消费。
- 是否可规避：显式声明 `ReadOnly` 可得到 `not_required`，但要求调用方预先知道该分支。

## 修复方案（本 PR 只做第 1 部分）

范围：

1. 无写意图时仍在 Git 仓库内采样基线；采样结果带“观测完整性”信息。
2. 终态投影按证据分档：
   - 无 intent + 观测完整 + 无差异 → 交付不阻塞（旧枚举保持 `unknown`，新增结构化原因 `not_requested`）。
   - 显式只读 + 无差异 → 保持 `not_required`。
   - 观察到差异且无法归因 → `blocked`，原因 `unattributed_change`。
   - 无基线 / 采样失败 / 范围不可验证 → `blocked`，原因 `observation_incomplete`；不得显示通过。
   - 非 Git 目录且无 intent → `not_requested` + 观测原因 `not_applicable`（没有可绑定的仓库范围）。
3. 兼容：不新增 `PostProcessingStatus` / `DeliveryStatus` / `WorkflowOutcome` 枚举值；新增可选原因字段（`serde(default)`，未知字段对旧读者透明）。旧 `state.json` 缺字段按 `unknown`，不能变绿。
4. 不改 `status`；已 failed/cancelled/超限的运行不因交付“不需要”而显示成功。

明确不做（拆到后续 PR）：

- 限额可见性、token/迭代上限、attempt 执行结果（第 2 部分）。
- Git 身份冻结、外部提交归因、错误文案区分（第 3 部分）。
- TUI 面板新增交付维度展示；本 PR 先修正状态投影与通知文案。
- 不持久化 baseline 内容，不引入清理/回滚，不改沙箱授权。

回归测试：

- `peri-workflow/src/runner_test.rs`：投影表逐行（无 intent 干净、无基线、采样失败、无法归因、显式只读、写 intent 通过）。
- `peri-workflow/src/journal_test.rs`：反转 `no_write_intent_skips_git_baseline_even_inside_a_repository`；补非 Git、采样失败、旧 state 兼容。
- `peri-workflow/src/completion_test.rs`：completed + 未请求交付不被误判 blocked；failed/cancelled 不因该分支变绿。
- `peri-acp-types` 序列化测试：旧 payload 反序列化默认值；新增原因字段旧读者可忽略。

## 验收标准

1. 无 intent、Git 采样完整且无差异的 completed run：`delivery != blocked`，通知文案不再出现 “delivery is blocked”。
2. 显式只读与写意图路径的现有投影不退化。
3. 无基线、采样失败、无法归因仍为 `blocked`，且原因可区分。
4. 旧 `state.json` / 旧 consumers 读取不失败；failed/cancelled 运行不显示全绿。
5. 目标测试通过：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-workflow --lib`、`-p peri-acp-types --lib`。

## 验证记录

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-workflow --lib` → 234 passed，0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib` → 849 passed，0 failed。
- `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-workflow -p peri-acp-types --all-targets -- -D warnings` → 通过（仅依赖 `hyper-util` 既有 warning）。
- `cargo fmt --all -- --check`、`git diff --check` → 通过。
- 未运行时复现本地 run journal（未取得）；issue #522 的 23 个 run 证据来自用户，本次以代码路径与定向测试确认。
