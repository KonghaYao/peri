# P1：Workflow 终态投影、限额可见性与 Git 后置条件

**状态**：已确认，待修复。
**优先级**：P1（issue #522 原标 P2；按设计决策与用户要求走 P1 流程）。
**类型**：可观测性 / 状态投影语义。
**创建日期**：2026-11-21。
**来源**：用户 issue #522（23 个本地 run 证据）+ 两路对抗审查设计（`peri-522-p1-workflow-projection.md`）。
**确认方式**：explorer 只读确认 + 主 agent 复核，基线 `origin/main` @ `39eddb1dbad52941d3cfbda9b64fd6efc599a2f2`。
**交付**：三部分合入同一 PR（用户 2026-11-21 指示“统一一个 pr”）。

## 确认证据

- `peri-workflow/src/journal/git_baseline.rs:80-88`：`capture_for_intent` 在 `intent.is_none()` 时直接返回 `Ok(None)`，不采样。
- `peri-workflow/src/runner/terminal.rs:67-74`：`(None, None)` 落入 `_ => PostProcessingStatus::Blocked`。
- `peri-workflow/src/runner/terminal.rs:78-88`：`write_intent.is_none()` 直接令 `delivery = DeliveryStatus::Blocked`。
- `peri-workflow/src/runner/terminal.rs:135-138`：`finalize_workflow` 只在 `git_baseline` 为 `Some` 时校验；无基线即无观测。
- `peri-acp-types/src/workflow.rs:363-384`：`PostProcessingStatus` / `DeliveryStatus` 为封闭 serde 枚举；新增枚举值会让旧二进制读新 `state.json` 失败。
- `peri-workflow/src/runner/message_loop.rs:315-328`：`error = limit_state.breach().or(done.error)`，首条限额字符串覆盖其他原因；`RunLimitState` 只保存一条字符串。
- `peri-workflow/src/runner/run_limit_state.rs:38-67`：限额只记录首条 breach；`observed_tool_calls` 无对外读取。
- `peri-agent/src/agent/workflow/agent/workflow_agent_executor.rs:396-400`：无 agentType 时迭代上限回落 200；`peri-middlewares/src/assembly/workflow/workflow_agent_middleware_factory.rs:347-350` 将 `max_turns` 映射为 `max_iterations`（builtin coder 为 400）。有效值与用量未写入 attempt/state。
- `npm-packages/@peri-workflow/src/server.ts:126-137`：attempt 的 `disposition` 一律 `produced`，不区分 ok/dead/skipped；执行结果只在同一 journal entry 的 `result`。
- `peri-workflow/src/journal/git_baseline.rs:109-137`：基线身份只含 repo_root/cwd/HEAD/porcelain 原始字节；无 git-dir/common-dir/worktree 身份；`git_output` 丢弃 stderr。
- 用户证据（issue #522，未在本次修复环境复现）：15 个 completed 中 14 个 delivery=blocked；`01a1f862` completed 仍 blocked；`01a1f863` 400 次迭代超限但顶层 `error=null`；`01a1f36e` token 75,323 超 60k，顶层只报 maxToolCalls；`01a1b661` 主分支外部提交被当作本 run 后置条件失败。

## 根因链

1. **交付投影**：未声明 `writeIntent` → 不采样 → 终态 `Blocked`；`delivery` 分支再次强制 `Blocked`。执行完成且无失败证据的 run 被读成交付阻塞。
2. **限额可见性**：`RunLimitState` 只保留首条字符串；迭代上限与 token 用量不进 `state.limits` / attempt；`disposition=produced` 不表示执行成功，dead 原因被淹没。
3. **Git 归因**：基线只按 workflow cwd 捕获，不记录 worktree 身份与观测范围；错误文案不区分本 run 写入、外部提交与未知；stderr 在生成点被丢弃。
4. 后果：用户在状态里看到错误的“交付阻塞”、不可见的硬上限与无法解释的 Git 差异。

推断（未单独复现）：多数只读 run 不声明 intent，因此该分支是 14/15 blocked 的主要来源。

## 影响面

- 用户影响：completed run 被读成交付阻塞；超限与失败阶段不可见；Git 外部变化被误归因。
- 受影响模块：`peri-workflow` 终态投影/限额/Git baseline；`peri-acp-types` wire；`npm-packages/@peri-workflow` attempt 生成；`peri-agent` workflow agent 结果；`mcp-packages/workflow`、`peri-tui` 只读消费。
- 是否可规避：显式声明 `ReadOnly` 可得到 `not_required`；限额与 Git 归因无可规避路径。

## 修复方案（同一 PR，三部分）

### 1. 交付投影（已完成于 714a951fe，本 PR 继续保留）

1. 无写意图时仍在 Git 仓库内采样基线；`GitObservation::{Captured, NotApplicable, Incomplete}` 区分完整性。
2. 终态分档：无 intent + 完整观测 + 无差异 → `delivery=unknown` + `not_requested`；显式只读 → `not_required`；未归因变化 → `blocked` + `unattributed_change`；无基线/采样失败 → `blocked` + `observation_incomplete`；非 Git + 无 intent → `not_applicable`。
3. 不新增 `PostProcessingStatus` / `DeliveryStatus` / `WorkflowOutcome` 枚举值；`RunState` 可选原因字段兼容旧读者。

### 2. 限额与 attempt

1. 保留 `state.limits` 与顶层首要 `error` 兼容；新增结构化 `limit_breaches[]`（scope、limit、observed、attempt 身份、检测时点），同时列出已触发的全部约束，不只首条。
2. 记录有效迭代上限与来源：无 agentType 的 200 与定义 `max_turns`（含 builtin coder 400）都按实际解析值记录；不得写死，不统一默认值。
3. token：按稳定 attempt 身份累计 `AgentRunResult.token_count`；与 run `budgetTotal` 比较，超限时加入 breaches。重放/缓存命中不重复计费；真实重试独立计量。未配置或未采集用量不得显示为 0。
4. attempt 增加可选执行结果（`outcome: ok|dead|skipped|limited`、reason、有效迭代上限/用量、token 预算/用量）；`disposition` 保持原语义，`disposition=produced` 不覆盖 dead。旧 journal 缺字段为 `None`。
5. 不引入抢占、不新增 token 中断、不改 #495 的取消/预算策略。

### 3. Git 身份与归因

1. `GitBaseline` 记录 `--git-dir`、`--git-common-dir`、repo_root/cwd/HEAD 与观测能力（可见提交/索引/工作区；ignored 不观测）；`RunState` 可选记录观测身份与完整性，不持久化 baseline 内容。
2. `git_output` 失败保留 stderr 摘要（有界），避免统一 “git pre/postcondition command failed” 丢失诊断。
3. 错误文案结构化区分：`run-owned violation`（可归因）、`unattributed change (external or unknown)`（无法归因）、`observation incomplete`（无法观测）。无法独立确认外部提交时保留未知，不冒充本 run 写入。
4. 不引入清理/回滚、不改沙箱授权、不承诺全程写入审计。

### 明确不做

- 新数据库 schema；新枚举值进入旧字段。
- TUI 面板新增展示（只保证状态与原因可读；UI 展示可后续）。
- 完整 e2e 重录；只做定向回归。
- 不修改 #495 的全局取消/预算策略。

## 回归测试

- 投影表逐行：无 intent 干净、非 Git、无基线、采样失败、未归因、显式只读、写 intent 通过、failed/killed。
- 限额：`maxToolCalls` + token 预算同时触发时首要 error 兼容且两项原因均可见；200/400 角色配置与覆盖值；重试不重计。
- attempt：5 个 dead attempt 的原因可查；脚本吸收子阶段失败仍可 completed；dead 不被改成 produced。
- Git：外部提交不归因本 run；显式绑定另一 worktree/未观测域标为不完整；本 run 越界仍失败；旧 state 缺字段默认。
- 兼容：旧 `state.json`、旧 journal、旧 ACP consumer 反序列化成功。

## 验收标准

1. 无 intent、观测完整且无差异的 completed run：`delivery != blocked`，通知不再出现 “delivery is blocked”。
2. 所有触发的硬上限（工具调用、token、迭代、elapsed、agents）都可见；首条 error 仍兼容；200/400 按实际解析值记录。
3. dead/失败 attempt 的原因可见，`produced` 不再掩盖执行结果；已有产物但未结算的阶段可识别。
4. Git 变化按“本 run / 外部或未知 / 观测不完整”分档，外部提交不被当作本 run 违例；观测范围与身份可读。
5. 旧 `state.json` / 旧 journal / 旧 consumers 不失败；failed/cancelled/超限不显示全绿。
6. 目标测试通过：`peri-workflow`、`peri-acp-types`、`peri-agent` 相关 lib 测试与 clippy/fmt。

## 验证记录

### 第 1 部分（714a951fe）

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-workflow --lib` → 234 passed，0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib` → 849 passed，0 failed。
- `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-workflow -p peri-acp-types --all-targets -- -D warnings` → 通过（仅依赖 `hyper-util` 既有 warning）。
- `cargo fmt --all -- --check`、`git diff --check` → 通过。
- 未运行时复现本地 run journal（未取得）；issue #522 的 23 个 run 证据来自用户，本次以代码路径与定向测试确认。

### 第 2/3 部分

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-workflow --lib` → 249 passed，0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib` → 854 passed，0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- workflow` → 61 passed，0 failed。
- `npm-packages/@peri-workflow`：`bun test` → 110 pass / 0 fail；`bun run typecheck` 通过。
- `clippy -p peri-workflow -p peri-acp-types -p peri-agent --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`git diff --check` → 通过。
- 新增定向回归：maxToolCalls + token 同时触发（首要 error 兼容、两项可见）、200/300/400/覆盖值解析、重投不重计、dead/limited 可见、旧 state/journal 缺省；Git 身份与可见范围、stderr 保留、档位文案、旧 state 缺省、git-dir 重绑定 → incomplete。
- 未复现 issue #522 的 23 个本地 run；结论来自代码路径 + 定向测试。
