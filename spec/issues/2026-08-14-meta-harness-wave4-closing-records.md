# MetaHarness 波 4 收尾记录——未落地建议 / 债务 / 遗留确认

**状态**：Open（记录项，非实施任务）
**优先级**：低（记录与后续跟进）
**类型**：决策记录
**创建日期**：2026-08-14
**来源**：wave3-4 workflow 执行收尾（Plan agent 死亡容错 / flaky 取证 /
include_str 所有权核查 / 遗留项对账）

## 背景

wave3-4 workflow（C1-C5）已全部落地并通过验证（clippy 零警告 + 全量测试
全绿）。收尾阶段产生三类未落地记录与一类遗留确认：workflow Plan 失败策略、
flaky 测试取证、include_str 跨 crate 所有权债务、既有遗留项状态。本文件
仅为记录（log-and-watch / 后续批次跟进），不构成波 4 范围内的工作。

## F：workflow Plan failure policy（advisor 建议，未落地）

**事实**：wave3-4 workflow 的 Plan agent（opus）运行中死亡
（`journal.jsonl` seq 0 `{"kind":"dead","reason":"runagent-threw",
"detail":"LLM error: model protocol error: provider failure"}`，输出 0
tokens，实际为空 plan）。引擎容错继续，C1 在空 plan 下自行设计推进
（D1-D6 决策记录），磁盘产物完整、验证全绿——工作完成但缺 Plan 把关。

**advisor 建议（2026-08-14）**：workflow Plan 阶段失败应**默认 fail-closed**
——重试（有限次数）或启用备用 planner；仅在**低风险**场景才显式批准降级
（无 plan 继续执行），且降级必须留痕（journal/notify 可见）。

**现状**：未落地。workflow 引擎当前行为 = fail-open（Plan 死亡 → 后续
agent 自行设计继续）。风险：复杂批次（需 Plan 拆分与把关的任务）在无 plan
下执行可能产生范围漂移或遗漏约束。

**后续动作（未排期）**：workflow 引擎（`.claude/workflows/` 运行时 +
run-agent 实现）增加 Plan 失败策略：默认 fail-closed（有限重试 + 备用
planner 或 abort），低风险显式批准降级路径 + 留痕。落地前，编制 workflow
时如预期 Plan 可能失败，可先独立验证 Plan 输出再放行后续批次。

## G：flaky 取证记录（log-and-watch）

**事实**：wave3-4 验证期间，`cargo test -p peri-middlewares --lib` 首次运行
`1327 passed; 1 failed`（失败测试名未捕获，输出被并行 grep 干扰丢失）；
随后连续 3 次 `1328 passed; 0 failed; 3 ignored`。结论为 flaky，与并行
时序冲突相关，未复现。

**处置（按 advisor 建议）**：最小分诊 + log-and-watch——记录本取证，后续
复现时按以下步骤取证：

1. 复现失败时**立即单独重跑失败测试**（`cargo test -p peri-middlewares
   --lib <test_name>`）确认是否稳定复现；
2. 记录完整失败输出（断言消息 + backtrace）到本文件或新 issue，不要只记
   "1 failed"；
3. 若与并行时序相关，尝试 `--test-threads=1` 对照；
4. 复现 ≥2 次再开专项 issue；单次偶发保持 log-and-watch。

**候选嫌疑（供复现时参考）**：tempdir 清理 / cwd 切换（store_test.rs 的
CwdGuard 模式是本仓库首次在测试中使用 `set_current_dir`，已用
`#[serial]` 防护）/ 文件系统事件竞态。

## include_str 跨 crate 所有权债务（短期接受，不复制文件）

**事实**：段落文件物理位置在 `peri-acp/prompts/sections/`，但持有者
middleware 在 `peri-middlewares`（`default_system_prompt/` 模块经
`concat!(env!("CARGO_MANIFEST_DIR"), ...)` 或 workspace 相对路径
`../peri-acp/prompts/sections/...` 引用——设计 §3.5.1 步骤 3"文件可留在
sections/ 由 middleware include_str!"授权）。

**裁定**：短期接受该跨 crate 引用债务（不复制文件，避免双轨内容漂移），
理由：段落文件与持有者分离是迁移期过渡形态；若未来段落文件归各 middleware
自有（按 3.1.1 归属全景"文件随持有者走"的最终形态），需将文件物理迁移
至 `peri-middlewares` 或段落声明层——**迁移时注意**：`include_str!` 是
编译期内联，不产生运行时路径依赖，跨 crate 引用仅在构建期生效，无运行时
成本；风险集中在目录重组时的路径断链（构建期即暴露，fail-fast）。

**后续动作（未排期）**：段落文件物理归属统一（per holder 目录），与
SECTION_IDS / 持有者声明一致性测试联动。

## 遗留项状态确认（对账 2026-08-14）

| 遗留项 | 状态 | 说明 |
| --- | --- | --- |
| C4 遗留 1：`subagent_system_prompt` 字段完整移除 | ✅ 已收口（C5） | 字段 + `from_frozen_parts` 参数 + fork 复制路径已删，子面向复用主冻结 prompt |
| C4 遗留 2 / P2-5：`project_enabled_sections` 未接生产渲染面 | 保持（有意） | 收集机制天然承担 gate 判定；投影保留为契约 3 显式视图与一致性测试载体（dual implementation test-guarded），禁止双轨由测试锁定 |
| C4 遗留 3：10_hitl 不再描述 mode 通知通道 | 保持（演进 1 语义代价，已落定） | 段落文本明确 "There is no runtime notification when it changes"；设计文档 §3.5 演进 1 语义代价已同步 |
| P2-6：workflow 10_hitl 渲染 | ✅ 已修复 | advisor 裁决 B：workflow 链无 HumanInTheLoopMiddleware，渲染面调用点过滤 `10_hitl`（`build_workflow_system_prompt_fallback` / `build_workflow_agent_prompt_builder`），`executor_flow_test::test_workflow_prompt_excludes_hitl_section` 锁定 |

## 涉及文件

- `spec/issues/2026-08-14-meta-harness-wave4-c4-evolution1-and-wave3-docs.md` — 遗留项 1/2/3 源记录
- `docs/design/meta-harness-design.md` — 3.5 演进 1 语义代价 / 3.5.2 步骤 2 偏差记录
- `.claude/workflows/meta-harness-wave3-4.mjs` — Plan 死亡现场（F 项证据）
- `peri-acp/src/provider/store_test.rs` — CwdGuard（G 项嫌疑点）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-14 | — | Open | agent | 波 4 收尾记录建档（F/G/include_str/遗留确认） |
