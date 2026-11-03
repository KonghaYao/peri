# P1：Micro Compact 低收益提交、Bash 信息丢失与收据位置错乱

**状态**：已确认，待修复。
**优先级**：P1。
**类型**：错误（上下文压缩核心行为）+ 可观测性缺失。
**创建日期**：2026-10-27。
**来源**：用户提交的联合修复设计 `/Users/konghayao/Downloads/peri-micro-compact-design-2026-10-27.md`。
**确认方式**：两个只读 explorer 独立核查 + 主 agent 复核（基线 `83d6de3a56e43ea611cd16301f84a2111d74dbea`，等于 `origin/main`）。

## 确认证据

1. 75%～95% 区间低收益仍提交：`peri-agent/src/agent/compact_v2/mod.rs:512-539` 在 `estimated_tokens_saved < target_reclaim_tokens` 时直接应用 Micro，注释为「部分收益也好」；唯一过滤是 `G <= 0`（`planner/planning.rs:775-788`）。全仓无 `div_ceil(10)` 或等价窗口比例门槛。
2. 既有测试把该行为固化为断言：`trigger_test.rs:203-252`（0.80、`saved < R`、断言 `Micro`）、`stages/compact_test.rs:499-541`（160k/200k、8×2k 字符、断言已压缩）。
3. 高压先 Micro 再 Full：`mod.rs:465-511` 在 `G < R && pct >= 95%` 时先 `apply_micro_plan`，再调用 Full；与设计「先决定最终策略，再执行一次提交」相反。
4. Smart 旁路先写后判：`mod.rs:577-590` 先 `apply_micro_plan`，`mod.rs:596/634` 才判断收益；`smart.rs:7-14` 的便捷入口完全无收益判断。
5. Bash 可被整组省略：`peri-acp-types/src/builtin_mcp.rs:232-242` 默认 `ContextRetention::SideEffectReceipt`；整组省略白名单包含它（`planner/planning.rs:505-511`）；省略后调用参数与结果全部移出请求（`planning.rs:641-651`、`projection_archive.rs:210-217`）。
6. 收据事实不足：`planner/planning.rs:888-924` 只输出工具名、call ID、`is_error`、执行状态、`exit_code`、`path`；Bash 命令与输出片段均不保留，`output_ref` 也不进入收据。
7. 收据/归档入口落在历史末尾：`projection_archive.rs:218-220, 280-299` 遍历完全部可见消息后 `projected.extend(receipts)`、`projected.extend(root_messages)`；该数组直接作为 provider 请求消息（`stages/reason.rs:114-123, 232`；`agent/model_bridge.rs:237-310`）。因此旧步骤收据会越过新用户消息与最新工具结果。
8. 位置行为被黄金测试钉死：`projection_borrowed_test.rs:693-770`（`GOLDEN_V3_ARCHIVE` 收据位于全部历史之后），断言点 `:412-415`。

## 根因链

**收益门槛（观察）**：75%～95% 区间只检查「存在合法候选且 G>0」→ 每次新增内容产生的少量可省略差异都会被提交 → 每次仅回收一两千 token，压力样本随新 usage/工具输出重新生成 → 同一高压区反复小额 Micro。`TokenTracker` 只在 Full 后 reset，Micro 不重置，故连续提交机制成立。

**Full 叠加（观察）**：达到 Full 条件但 G<R 时，当前实现先提交 Micro 再 Full → 摘要输入先承受一轮有损变换，且产生「MicroAppliedThenFullFailed」语义分支。

**Bash 退化（观察）**：整组省略同时移除命令参数与结果 → 历史只剩「执行成功/退出码」→ 后续任务无法从上下文中恢复命令与输出，只能依赖 ReadContextArchive 定向回查。

**收据顺序（观察）**：派生收据与归档入口统一追加到可见历史之后 → 旧步骤的派生信息出现在最新用户要求与工具结果之后 → 改变时间顺序，并让内部收据成为最新历史内容。

**Smart 旁路（观察）**：Smart 入口先 apply 再判断收益 → 与统一决策表冲突；虽默认关闭，仍违反「业务规则单一权威」。

**推断**：真实会话中的触发频率与行为质量损失未做运行时采样；代码与既有测试证明机制成立。

## 影响面

- 用户影响：高压区上下文反复被低收益压缩；历史工具证据（命令/输出）不可见；派生收据打乱时间顺序，可能被模型当作最新状态或任务答案。
- 受影响模块：`peri-agent/src/agent/compact_v2/`（决策、规划、投影渲染）、`peri-agent/src/agent/stages/compact.rs`（事件与生命周期）、`peri-acp-types/src/builtin_mcp.rs`（Bash 保留策略）、`peri-acp-types/src/compact.rs`（结果语义）。
- 规避：手动 `/compact` 触发 Full；或调高 `micro_compact_threshold`。均为临时规避，不修复信息丢失与顺序问题。
- 定级依据：核心上下文管理路径错误、可稳定复现（确定性测试可红绿验证），存在手动规避；不涉及数据丢失、安全或无法启动，故为 P1。

## 修复方案

范围（以设计文档第 3-8 节为准）：

1. **统一收益准入**：在 `compact_v2` 建立唯一决策点；`M = u64::from(C).div_ceil(10)`（C 为完整 `context_window`，整数向上取整）；决策表：
   - 未达 Micro 阈值 → Skip；
   - 未达 Full 条件且 `G < M` 或候选为空 → Deferred（零写入、零归档正文验证、零成功事件、不增加失败计数）；
   - 未达 Full 条件且 `G >= M` → Micro；
   - 达到 Full 条件且 `G >= max(M, R)` 且候选非空 → Micro；
   - 达到 Full 条件其余情况 → 直接 Full（本轮不先提交 Micro）；
   - `C == 0` 或缺预算 → 不执行比例型自动 Micro；手动 force、驻留 Full 独立；
   - shadow 只报告决策，不提交。
2. **决策前置于提交**：删除 Micro 先写后判与 Smart 旁路；Smart 归一为同一决策路径。
3. **Bash 保留策略**：`builtin_mcp.rs` 中 Bash 从 `SideEffectReceipt` 改为 `PreviewOnly`；保留现有 head/tail 预览、`[N 字符已省略]` 标注与 archive_ref 回查后缀。Read/Grep/Glob 本轮不改。
4. **收据与归档入口位置**：在 `projection_archive.rs` 渲染期按 canonical 位置插入：
   - 完整归档步骤：放在该步骤调用与全部结果处理完毕之后；
   - A/B 部分归档：不得插入 B 调用与其结果之间；
   - 混合 inline 结果：不移动同条消息内真实内容；
   - 签名推理依赖组保持完整闭包；
   - 收据预算超限退化为归档入口时位置不变；
   - `ArchiveRoot` 锚到 `last_anchor`（可见时）或持有该 root 的条目/摘要；
   - 不新增持久化格式，不升级 `PROJECTION_POLICY_VERSION`；旧 v2/v3 投影渲染时应用同一位置规则。
5. **事件与观测**：新增 Deferred 结果语义；`CompactEnded` 携带跳过原因（收益不足时含 G、M）；结构化日志记录 `C/P/M/R/G`、决策与原因、规划/原文验证/提交耗时、变更条数与写入次数；延后不增加失败计数、不触发错误传播。
6. **文档**：同步更新 `docs/design/micro-compact.md`（决策表、Bash 策略、收据位置、Deferred 语义），保持现行行为与文档一致。

明确不做：

- 不改数据库表结构、不新增环境变量、不新增摘要模型调用、不新增任务记忆子系统。
- 不把 R 的 2% 下限改成 10%，不把 45% planner 目标变成强制删除目标。
- 不自动重建已存在整组省略的旧投影；不扩大省略范围凑门槛。
- 不在本 PR 内完成设计 7.3 的真实模型行为对照验收（需固定模型/提示词/可控工具结果的独立实验）；以确定性测试覆盖控制流与协议，行为验收单列为遗留风险。
- 不修改 Read/Grep/Glob 策略，除非既有测试证明必要。

## 验收标准

1. 设计 7.2 确定性场景全部有对应测试：
   - C=200k、P=160k、G=19,999 → Deferred；压缩写入、归档正文验证、成功事件均为 0；
   - 同上 G=20,000 → Micro 提交一次；
   - C=200,001 → M=20,001（不得向下取整）；
   - C=200k、P=190k、G=19k → 直接 Full，本轮 Micro 提交为 0；
   - C=200k、P=210k、R=23k、G=20k → Full（不能只因达到 10% 选 Micro）；
   - C=0 或缺预算 → 不执行自动 Micro；手动/驻留路径不变；
   - Smart 开关打开 → 相同输入得到相同结论，无先写后判；
   - Full 失败/提交前取消 → 无本轮 Micro 隐藏提交；
   - Micro 成功后下一次 usage 偏高 → 不重置真实 usage；
   - 每轮确有足量合法收益 → 允许连续 Micro。
2. 位置场景测试：
   - 旧工具步骤后出现新用户任务 → 收据在新任务之前；
   - A/B 部分归档、混合结果、签名依赖 → 最终 provider 请求配对合法、真实内容顺序不变、无虚构执行；
   - 收据超预算合并、Full 后 root、冷恢复、provider 切换 → 位置正确、引用有效；
   - v2/v3 旧持久化投影渲染使用同一位置规则。
3. Bash 改为 `PreviewOnly` 后：Bash 不再进入整组省略；长输出保留带省略标注的 head/tail 预览与 `archive_ref`；参数不被改写。
4. 目标检查通过：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib`、相关 `peri-acp-types` 测试、`bash scripts/check-changed.sh`、`cargo fmt --check` 与仓库 clippy；`git diff --check` 干净。
5. 更新后的 `docs/design/micro-compact.md` 与实现一致。

## 验证记录

2026-10-27，分支 `fix/micro-compact-gain-gate`（基线 `83d6de3a5`）：

| 检查 | 结果 |
| --- | --- |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- compact` | 302 passed / 0 failed / 3 ignored |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib` | 1384 passed / 0 failed / 5 ignored |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- gain_gate_tests` | 12 passed |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- projection_position_tests` | 7 passed |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent`（含全部集成测试） | 所有 target 通过（wire 5 passed、failure adversarial 21 passed 等） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib` | 677 passed / 0 failed |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-controller` / `-p peri-acp` | 通过（事件字段新增未破坏下游映射） |
| `bash scripts/check-changed.sh` | 反向依赖闭包 check 通过 |
| `cargo fmt --all -- --check` | 通过 |
| `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-acp-types -p peri-agent --all-targets -- -D warnings` | 通过 |
| `git diff --check` | 干净 |
| `bash scripts/check-file-size.sh` | 本次改动文件均 < 1000 行（mod.rs 738、projection_archive.rs 713、compact_test.rs 943） |

新增/迁移的确定性覆盖：`compact_v2/gain_gate_test.rs`（M 取整、G=19,999/20,000 边界、Full 条件 max(M,R)、C=0、空候选、Deferred 零写入、连续 Micro、Smart 归一、Bash PreviewOnly）、`compact_v2/projection_position_test.rs`（旧步骤后新用户任务、A/B 部分归档、inline 混合、预算合并 root、Full 后 root、v2/v3、冷恢复与 provider 切换）、`stages/compact_test.rs`（Deferred 事件配对、失败计数不变、Micro 不 reset usage、取消无隐藏提交）、`tests/micro_compact_wire_test.rs`（最终 provider 请求中的收据位置与配对）。

未完成：设计 7.3 的真实模型行为对照（固定模型/提示词/可控工具结果的独立实验）不在本 PR 范围；Bash 预览中部的关键事实、模型是否复述内部收据、回查失败处理仍需该实验验收。模拟 wire 与确定性控制流测试不构成行为质量结论。
