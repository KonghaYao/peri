# P0：长会话中 Full 与 Micro Compact 反复交错且缺少可信进展度量

**状态**：Investigating
**优先级**：P0
**类型**：可用性 / Token 成本 / Compact 活性 / 可观测性
**创建日期**：2026-09-10

> 2026-09-10 再次审计：用户反馈修复后 Full / Micro 仍反复交错。已增加 4 个可显式运行的失败回归和 1 个真实 SQLite Full 成功的循环反例，见文末「修复后逻辑复审」。本事故仍未修复完成。

## 事故摘要

用户现场截图显示，同一长会话内自动 Full Compact 与 Micro Compact 多次交错：

- Full 显示约 367–390 条消息、`~0 tokens saved`，并重复显示 10 files；
- Micro 显示约 27–37 条消息、约 10.8k–18.8k tokens saved；
- Full 后会话仍继续运行，并在后续再次出现 Micro 或 Full。

本事故按 P0 管理：重复 Full 会调用 compact LLM、重新读取并注入文件，可能造成显著延迟与 token 成本，并存在长会话无法稳定退出高压区的可用性风险。当前尚缺少结构化运行日志来量化循环频率、额外成本、context overflow 或请求失败；P0 定级不等同于已确认全部根因。

## 用户可见影响

已观察到：

1. Compact 通知密集出现，Full 与 Micro 交错。
2. Full 始终显示 `~0 tokens saved`，用户无法判断 Full 是否有效。
3. Full 的 affected messages 在多轮间保持数百量级并波动或增长。
4. 每次 Full 均显示相同的文件数量。

待量化影响：

- 单会话额外 compact LLM 调用次数、输入/输出 token 与墙钟时间；
- Full 后第一轮正常模型请求的实际 input tokens；
- 是否发生 context limit、请求失败、任务中断或无法继续；
- 文件 re-inject 对 Full 后上下文基线的实际贡献。

## 已确认的代码事实

### 1. Full 的 `estimated_tokens_saved` 当前恒为 0

`full_compact_inner` 的 fallback 分支和正常摘要分支都构造：

```rust
estimated_tokens_saved: 0
```

位置：`peri-agent/src/agent/compact_v2/full.rs:81`、`peri-agent/src/agent/compact_v2/full.rs:171`。

因此截图中的 `~0 tokens saved` 不是 Full 前后 token 差值证据，而是当前后端没有为 Full 计算该指标。TUI 直接展示事件字段，见 `peri-tui/src/kit/acp_events/compact.rs:44`。

### 2. Full 的 affected count 使用 transcript 总长度

`full_compact_inner` 在执行前读取 `transcript.len()`，并将其作为 `affected_count`，见 `peri-agent/src/agent/compact_v2/full.rs:53`、`peri-agent/src/agent/compact_v2/full.rs:173`。

`MessageTranscript::len()` 返回全部 entries 数量，见 `peri-agent/src/session/transcript.rs:527`；它不是“本轮新排除的消息数”。因此该指标可包含此前已 excluded 的历史和其他未在本轮发生状态变化的 entry。截图中不断变化的数百条 messages 不能直接解释为本轮实际压缩量。

### 3. Full re-inject 从全部历史 entries 收集文件来源

`collect_reinject_v2` 明确遍历 transcript 全部 entries，包括已 excluded 的历史，见 `peri-agent/src/agent/compact_v2/full.rs:542`、`peri-agent/src/agent/compact_v2/full.rs:552`。文件候选来自历史 `Read` tool call，并按最近路径去重和数量上限选择，见 `peri-agent/src/agent/compact_v2/full.rs:407`、`peri-agent/src/agent/compact_v2/full.rs:562`。

这意味着：只要历史 `Read` 调用仍在 transcript 中，后续 Full 可以再次读取并注入同一批路径。上一轮生成的 re-inject Human 消息本身不是新的 `Read` tool call，不能据此断言 re-inject 消息会自我倍增；已确认风险是同一文件集合在每轮 Full 后重复成为活跃上下文基线。

截图中的 `10 files` 不能单独证明重复内容、默认上限或累积数量。代码默认 `re_inject_max_files` 为 5，见 `peri-acp-types/src/compact.rs:294`；现场可能使用了不同配置，必须采集 effective config。

### 4. 只有成功 Full 会 reset `TokenTracker`

自动 Compact 根据 `TokenTracker::estimated_context_tokens()` 构造 `ContextPressure`，见 `peri-agent/src/agent/stages/compact.rs:62`。该估算来自最近一次 provider usage，加上尚未被下一轮 LLM 感知的工具结果估算，见 `peri-agent/src/agent/token.rs:68`。

成功 Full 后 stage 会 reset tracker，见 `peri-agent/src/agent/stages/compact.rs:334`。Micro 成功后不会 reset 或本地 rebase。

这不等于已证明 Micro 后必然使用陈旧高水位重复触发：下一次正常 LLM response 会通过 `TokenTracker::accumulate` 更新 `last_usage`，见 `peri-agent/src/agent/token.rs:31` 和 `peri-agent/src/agent/stages/reason.rs:294`。必须用完整时序测试区分：

- Micro 后、下一次 LLM 调用前的本地重复判断；
- 下一次真实 provider usage 仍然超过阈值；
- tracker accounting 错误；
- Micro 估算收益与实际 provider-facing prompt delta 不一致。

### 5. 当前没有并发或乱序提交证据

Compact stage 会暂时取走 transcript 所有权，跨 `await` 执行后再放回，见 `peri-agent/src/agent/stages/compact.rs:108`。用户截图只展示事件顺序，不包含 compact id、base revision、开始/结束时间或提交 revision，不能证明 Full/Micro 并发、重入或乱序提交。

本事故首先调查串行 churn；并发陈旧提交作为必须排除的风险，不作为当前已确认根因。

## 对抗审查后的修正

以下早期建议被撤回或降级：

1. **撤回“按 Micro `estimated_tokens_saved` 直接扣减 `TokenTracker`”**。该值是 planner 估算，不一定等于实际序列化请求的 token delta；直接扣减可能重复记账、覆盖更新的 provider usage或低估上下文，延迟必要 compact。
2. **撤回“re-inject 消息自身无限累积”**。下一次 Full 会排除上一轮活跃消息；需要验证的是重复读取和注入是否让 Full 后基线长期高于安全阈值。
3. **不以 messages 数下降判断进展**。Full 可能减少 token 但保留类似消息数，也可能减少消息数却因摘要或文件注入增加 token。
4. **不先做全局 path dedup**。同一路径内容可能已更新；缺少 content version/provenance 时永久去重会丢失新内容。
5. **不先加入永久 cooldown 或禁用 Full**。无安全逃生路径的抑制机制可能把会话推向 context overflow。
6. **C、D 指标问题与 compact churn 根因分开验收**。修正指标不会自动修复重复触发。

## 工作假设

- **H0：普通 root Agent 历史 ownership 错接（已确认并修复）。** `build_and_execute_agent_v2` 的 Phase 5 声明已加载 history 是当前 Agent 的 own region，却调用 `MessageTranscript::with_ancestor_payloads` 将全部历史置于只读 ancestor boundary。Full 摘要请求仍读取这些可见历史，但提交时 `skip(ancestor_len)` 无法排除它们；下一次 provider 请求继续携带原历史并返回新的高位 usage，从而再次触发 Full。普通 root 主路径现改用 `with_own_payloads`。该修复不等于已解决含 parent snapshot 的混合 history：SubAgent spawn/resume 与直接加载 hidden child thread 仍缺少显式 payload provenance，必须区分只读父快照和当前 thread 的可压缩 own history，不能把整个平铺 payload 列表统一归类。
- **H1：Full 后基线过大（ownership 主因已修复，剩余载荷仍待量化）。** 历史 `Read` 路径曾使同一文件在连续 Full 中重新注入；现已限制为 Full 前可见 `Read` 来源。摘要、Skills 与新文件注入后的真实 provider-facing 基线仍待测量。
- **H2：Micro 收益估算偏离真实收益。** planner 的 projection 估算没有覆盖完整 provider request，实际 input tokens 未按 UI 所示幅度下降。
- **H3：阈值附近缺少 progress/hysteresis 约束。** 每轮根据单次 usage 独立决策，可能在高压区形成合法但低收益的 Full/Micro 交错。
- **H4：旧压力样本可被重复消费（已修复）。** 正常非零 provider usage 在 RCRA 时序中只供下一次 Compact 使用；但 Micro 后若后续 provider 返回 `input_tokens=0`，旧 `last_usage` 曾会继续存活并再次触发。现以有效 usage generation 与 tool-growth generation 标识压力样本，同一样本只尝试一次。
- **H5：事件指标掩盖真实状态（本轮已缓解）。** Full 的 saving wire 仍为 legacy `u64=0`，但 TUI 已将 Full/未知 strategy 展示为“未测量”；Full 与 mixed 路径的 affected count 已按本轮实际 excluded transition 去重。
- **H6：存在并发或陈旧提交。** 当前无证据，仅作为需要通过 revision/generation 测试排除的假设。

## 调查与修复范围

### WP-001：建立可复现事件链

构造长 transcript，执行至少两轮完整的：

```text
Reason usage → Compact → Reason usage → Compact
```

同时记录每轮：

- compact strategy、trigger reason 与 outcome；
- compact 前后 transcript revision；
- provider-facing 序列化请求的估算 token；
- provider 返回的 input usage；
- summary、re-inject files 与 skills 的 token 占用；
- planner estimated saving 与实际 request delta。

不得仅通过重复调用 Compact stage 并复用同一个静态 `TokenUsage` 来证明生产时序。

### WP-002：修正 Full 指标语义

- Full 的 token saving 必须明确为 `estimated`、`actual` 或 `unknown`；unknown 不得伪装成真实 0。
- `affected_count` 必须定义为本轮实际发生 flag transition 的去重 message 数；若仍需 scope size，使用独立字段。
- 指标仅用于观测，未经单独设计不得直接成为控制面输入。

### WP-003：验证并约束 re-inject 生命周期

先用测试回答：

- 连续 Full 且没有新 `Read` 或文件版本变化时，第二轮是否重新读取并注入相同内容；
- 重复注入占 Full 后 provider-facing prompt 的 token 比例；
- 同一路径内容更新后，下一轮是否应重新注入；
- configured cap 与 UI `files` 字段是否一致。

若 H1 成立，修复应基于明确的 provenance/content version/generation，而不是全局永久 path dedup。

### WP-004：修正触发状态机

在得到 WP-001 的真实时序后选择最小方案：

- 若下一次 provider usage 是准确高位：处理 Full 后基线或增加基于真实 progress 的 hysteresis；
- 若同一 usage 被重复消费：为 compact 决策记录 usage/request generation，禁止同一高水位样本重复触发；
- 若 Micro 估算严重失真：校准 estimator，不能直接改写 provider usage；
- 若存在并发：增加 base revision/generation compare-and-commit，拒绝陈旧结果。

### WP-005：P0 临时保护

只有在确认无进展紧密循环后才启用临时 guard。Guard 必须：

- 以 compact generation 和真实或稳定的结构性 progress 为依据；
- 对 context limit 保留强制 Full 或 fail-safe 路径；
- 不永久禁用后续有效 compact；
- 发出可诊断 outcome，而不是静默 Skip。

## 本轮修复状态（2026-09-10）

已完成并经独立 verifier 复验：

1. `TokenTracker` 使用有效 provider usage generation 与 tool-growth generation 组成压力样本；同一高压样本不会因后续零 input usage 被重复用于自动 Compact，新非零 usage 或新增工具输出仍可重新评估。
2. Full 文件 re-inject 仅从 Full 前可见 `Read` tool call 收集；连续 Full 无新 `Read` 时不再重复注入旧文件，同路径出现新 `Read` 后仍读取并注入更新内容。
3. Full `affected_count` 仅统计本轮 own-region 非 System 消息的 `excluded: false → true` transition；Micro/Smart→Full 成功时不再重复累加已被 Full transition 覆盖的消息。
4. Full 的 legacy `estimated_tokens_saved=0` wire 尚未迁移；TUI 对 Full、unknown 和 empty strategy 显示“token 节省量未测量”，Micro/Smart 保持数值展示。
5. 普通 root Agent Phase 5 改用 `MessageTranscript::with_own_payloads` 装载跨 turn 历史，使 Full 能对其提交 `excluded` transition；ACP `session/fork` 为复制后的 payload 分配新 `MessageId`、迁移 compact flags/projection 引用，并让新 thread 独立拥有可压缩历史；显式 ancestor API 继续保留给来源明确的只读继承区。SubAgent/hidden child 的混合 parent snapshot / own history provenance 仍须另行校准。

RCRA 时序证据：

- `test_run_react_loop_new_high_usage_generations_continue_full_micro_churn` 证明每个新的非零高位 provider usage generation 都会合法重新 arm Compact。该场景的两次 Full 均失败，outcome 为 `MicroAppliedThenFullFailed`；它验证“新证据可重试”，不代表已经复现截图中的成功 Full chronology。
- `test_run_react_loop_successful_full_replaces_history_reinjects_read_file_and_resets_usage` 覆盖一次成功 Full：旧可见历史被摘要替换，下一次 Reason 收到摘要与文件 re-inject，tracker 完成 reset，并接受随后返回的新低位 provider usage。
- `test_main_agent_history_is_seeded_as_compactable_own_region` 锁定 `run_session_loop → build_and_execute_agent_v2` 的 Phase 5 ownership；`full_excludes_loaded_root_history` 证明跨 turn 载入的 root history 会被 Full 排除，provider-facing 可见历史不再残留。
- `forked_payloads_have_independent_ids_flags_and_compaction_lifecycle` 证明 ACP fork 的 payload、projection 引用和 compact flags 迁移到新 ID，Full lifecycle 只更新 fork thread；stdio `test_fork_creates_session_scoped_lsp_pool` 同时锁定 handler 采用持久化复制后的 ID。
- 独立 verifier 对已确认的 stale-sample、历史 `Read` re-inject、affected count 和 TUI unknown-saving 修复给出 `PASS`。

**决策：不新增 hysteresis/no-progress production guard。** 当前证据表明，同一旧压力样本应被去重，而不同的新 provider usage 或工具增长是必须保留的重新评估证据。宽泛 guard 可能压制 context limit 前必要的 Compact；只有生产观测证明“新的 provider-confirmed 高位样本仍形成无进展紧密循环”时，才重新评估 WP-005。

本事故保持 **Investigating / P0**：已确认缺陷已有修复和自动化证据，但仍需生产/runtime 观测量化 Full 后实际 input tokens、额外 Compact 次数、token/延迟成本、请求失败，以及会话是否稳定退出高压区。

## Verification checklist

- [x] RCRA characterization 覆盖连续新高位 usage generation 的串行重复评估，并明确其 Full 失败边界。
- [x] RCRA successful-Full chronology 覆盖历史替换、文件 re-inject、tracker reset 与后续低位 usage。
- [x] 普通 root Agent 跨 turn 历史保持在 compactable own region；Full 会排除已加载旧历史。
- [x] ACP `session/fork` 以新 `MessageId` 复制 payload 和 compact flags；fork Full lifecycle 可提交且不会修改 source flags。
- [ ] 为 SubAgent spawn/resume 与直接加载含祖先链的 child thread 保留显式 provenance：父快照是 ancestor，当前 thread 历史是 own，且 compact lifecycle 只写当前 thread。
- [x] 连续 Full 测试覆盖：无新文件版本时不再 re-inject；同路径新 `Read` 后可注入更新内容。
- [ ] 记录并比较 Micro planner estimated saving 与实际 provider-facing request token delta。
- [x] 验证 Full 后 tracker reset 保留 generation 单调性，新非零 provider usage 产生新的权威压力样本。
- [x] 验证同一 usage/tool-growth pressure sample 不会被重复用于自动 Compact。
- [x] Full、unknown 与 empty strategy 的 unknown saving 不再展示为具有实际含义的 `0 tokens saved`。
- [x] Full 与 mixed 成功路径的 `affected_count` 不包含未在本轮发生状态变化的 excluded 历史，也不重复计算同一消息。
- [ ] effective `re_inject_max_files` 与事件/UI files 数量一致，解释现场的 10 files。
- [ ] 采集 runtime 事件顺序；若发现并发或乱序，再补旧 revision 不得覆盖新 transcript 的测试。
- [x] 根据现有 RCRA 证据决定不加入 no-progress guard，并保留新压力证据触发必要 Compact 的行为。
- [x] `cargo build -p peri-agent -p peri-acp -p peri-tui` 通过。
- [x] `cargo test -p peri-agent --lib --quiet`：729 passed / 0 failed；`cargo test -p peri-acp --lib --quiet`：606 passed / 0 failed。
- [x] `cargo test -p peri-tui --lib --quiet`：1460 passed / 2 ignored；此前一次并行执行出现一个无关 flaky，单测与串行完整重跑通过。
- [x] `cargo clippy -p peri-agent -p peri-acp --all-targets -- -D warnings` 与 `cargo clippy -p peri-tui --all-targets -- -D warnings` 通过。
- [x] 独立 verification 复验为 `PASS`，`git diff --check` 通过。

## P0 退出条件

以下条件全部满足后方可解除 P0：

1. 能用自动测试解释并复现现场 Full/Micro 交错的主因。
2. 同一长会话不会基于同一压力样本或无进展状态反复执行 Full。
3. Full 后的实际 provider-facing token 基线可观测，并低于目标阈值；若无法低于阈值，必须给出明确 outcome 和安全降级。
4. 更新后的文件仍可 re-inject，且无新版本时不会产生未经解释的重复成本。
5. Full saving、affected messages 与 files 指标具有已定义且经测试锁定的语义。
6. 补充至少一项生产影响量化：额外 compact 次数/token/延迟、请求失败率、context overflow 或任务中断。

## 明确 non-goals

- 不把 planner 估算值直接写成 provider 的真实 usage。
- 不用全局 path 去重阻止更新文件进入上下文。
- 不因截图交错直接重构整个 RCRA 循环。
- 不在缺乏证据时宣称存在并发竞态。
- 不把 telemetry 修复包装成 compact 活性根因修复。

## 修复后逻辑复审（2026-09-10）

### 范围与结论

复审基线为 `74e29dc0`，重点核查 `0d98cdc6`（压力样本）和 `36ddc645`（root history ownership）。用户明确补充现象仍为「Full / Micro 反复交错」。

此前 root ownership、旧样本去重、可见 Read re-inject 修复有效，但没有覆盖下列缺陷。此次只增加审计测试和本节记录，未修改生产实现。未获得用户现场会话 ID、provider 请求或 runtime 日志；因此区分「代码可确定复现」与「现场主要成本来源已量化」。

### A1：已被 Full 排除的消息再次成为 Micro 候选，并参与 Full 升级决策

**证据：已执行默认配置失败回归，以及真实 RCRA 成功 Full 循环。优先级 P1，属于本 P0 事故待修项。**

- `peri-agent/src/agent/compact_v2/planner.rs::plan_micro` 将全部 entries 交给 `TurnGroup::collect`，没有排除 `flags.excluded` 的消息。
- `projection.rs::estimate_projection_chars` 对全部 entries 估算收益，同样没有 visible 过滤。
- `full.rs::full_compact_inner` 新排除消息时将 flags 替换为 `excluded=true` 和其他默认值，清掉此前的 Micro directive。
- 因而这些消息又能通过 Micro 的 truncated/directive 检查；`set_flags_projection` 保留 excluded，renderer 始终不会发送它们。此次 Micro 实际模型消息增量收益为零。
- 虚假 saving 仍用于 `run_compact` 的 `estimated_tokens_saved >= reclaim_target` 决策，足够大时会阻止高压下需要的 Full。

默认配置反例：4 个 Human turn，每轮含一对 Bash 调用/40,000 字符结果；SQLite Full 成功排除 12 条消息后，只剩 summary 可见。默认 stale=3 下仍选出两个已排除工具结果，报告 **19,763 tokens** 收益；完整模型可见 JSON 前后相同。新压力设为 96k / 100k、输出预留 4k、安全缓冲 5k 时，虚假收益超过 5k 回收目标，实际结果为 `MicroApplied`，没有执行 Full。

完整循环反例使用脚本 usage `96k → 80k → 96k → 80k → 1k`、stale=0，以及交替长短工具结果，实际得到：

```text
FullApplied → MicroApplied → FullApplied → MicroApplied
```

两次 Full 均成功提交 SQLite，两次 Micro 均报告正 saving，最终两条持久化 projection 都指向 excluded 消息，模型消息无投影变化。这补上此前「成功 Full 测试后只给低 usage」和「交错测试中的 Full 全失败」的组合缺口。脚本 usage 证明状态机路径，不是现场实际 token 基线测量；默认 stale 可达性由前一个回归单独证明。

这不表示同一批消息会立即无限重复：Micro 重新标记后会被 truncated 检查跳过；后续 Full 新排除的工具结果、以及新增 Human 边界使其他旧 group 变 stale，都可再次引入无收益候选。

测试：

- `test_audit_full_excluded_history_must_not_be_micro_candidate`
- `test_audit_excluded_savings_must_not_suppress_full`
- `test_audit_run_react_loop_successful_full_micro_churn_has_zero_micro_delta`

### A2：Reason 已经投影过的收益，随后又被 Micro 记为本轮回收量

**证据：已执行真实 `Reason → Compact → Reason` 失败回归。优先级 P2。**

`stages/reason.rs::run_reason` 在没有持久化 directive 时直接 `plan_micro(..., false)` 并渲染，不以本轮 Compact 是否触发为条件。后续 Compact 仍从 canonical 原文估算同样的投影收益。

默认配置、四个 Human turn 的反例中，两次成功 Reason 的完整消息快照完全相同，但中间 Micro 报告 **1,882 tokens** 节省。此路径不依赖 excluded，说明即使修复 A1，也不能把当前 estimated saving 当成实际增量回收量。

测试：`test_audit_micro_savings_must_change_previous_reason_view`。

### A3：新增工具输出没有接入生产压力追踪

**证据：已执行真实 tool dispatch 失败回归。优先级 P1。**

`TokenTracker::add_estimated_tool_tokens` 在生产代码中没有调用者；已有调用只在 token/stage 测试里。`tool_dispatch::dispatch_tools` 已把工具结果写入 transcript，却未增加 tool-growth generation 或 estimated tool tokens。

测试从 provider usage 74,000 开始，执行工具并确认 8,000 字符结果已进入 transcript，压力仍为 **74,000**，按该 tracker 已定义的 chars/4 估算应为 **76,000**。因此「新工具增长可重新评估」目前只在直接调用 tracker 的单测中成立，实际大结果可能在下一次请求前绕过应有的 Compact 检查。

测试：`test_audit_dispatch_must_account_for_tool_output_pressure`。

### A4：Full 成功后本轮失败，host 丢弃已提交摘要快照

**证据：静态完整调用链确认，尚未新增 host runtime 回归。优先级 P1；后续写失败回滚分支有持久内容不可见风险。**

成立条件：Full 已成功提交，之后本轮因 Interrupted/cancel、MaxIterations、LLM failure 或 forwarder error 以 `ok=false` 结束，且 host session 未被移除。

1. `full_compact_inner` 已在 SQLite 提交 old excluded + summary/re-inject。
2. `executor_helpers/v2_execute.rs` 的 Phase 8 flush 成功时，仍正确取出包含摘要的 canonical `persisted_payloads`；`collect.rs` 原样返回。
3. `peri-acp/src/host/prompt.rs::run_prompt` 只有 `result.ok` 时采纳这些 payload；失败分支只 truncate `state.history`，保留旧 `state.history_payloads`。
4. 下一轮从旧 payload snapshot 重建，Phase 5.5 却恢复新 excluded flags：旧历史被隐藏，新摘要又不在 snapshot 内。

正常 Completed 后 cancel token 被置位不属于此条件，因为 terminal classifier 优先认定 Completed 为成功。自动 Compact 没有强制 TUI reload；已有非空 history 的热 `session/load` 也不替换 host snapshot。真正移除内存 session 后冷加载可恢复磁盘上的摘要。

更严重的条件分支：若 Full 已提交后 writer flush 失败，Phase 8 rollback 删除本 turn 新增 ID（包括摘要），只比较旧 payload IDs 判定恢复成功，却不恢复旧消息的 excluded flags。此时冷加载也无法恢复被删摘要。这需要故障注入回归，不能把单纯取消误写成此分支。

### A5：Full 不能降低 canonical Reminder 本身的累积基线

**证据：静态路径与已有 `full_compact_preserves_canonical_reminder_without_flags` 测试。现场影响未量化。**

`MessageTranscript::visible_messages` 不包含 canonical Reminder；Full 的摘要输入和排除集合都基于普通消息。Reason 的 `visible_model_messages` 则包含 canonical Reminder（其中包括 model audience）。现有契约测试明确要求 Full 保留 Reminder，Goal steering 等生产者又可持续追加。

若该基线本身已高于触发阈值，反复成功 Full 只替换普通消息/摘要，无法使基线回落；每次新高位 usage 都可重新 arm。不能为降低 token 直接删除仍有效指令，需要为可过期/可替换控制消息定义生命周期，或在不可回收的高压区返回明确 outcome。用户现场是否满足规模条件仍待观测。

### A6：子会话与混合历史边界仍不完整

**证据：静态子链审计；不归因于普通 root 交错。**

- SubAgent resume 只加载 child own payload，又全部 `with_ancestor_payloads`，不恢复 flags。若显式配置自动 Compact，旧 own 历史无法被 Full 排除。当前生产 Agent 工具 spawn/resume 的 compact config、budget、LLM 为 None，不能把此风险说成当前普通子任务自动交错来源。
- fork 子任务把父消息以原 MessageId append；SQLite 全局 ID 冲突 `INSERT OR IGNORE`，child 本地磁盘行没有父快照；resume 只读 child 行，继承上下文丢失。
- spawn 把 parent snapshot 截止 ID 写在 child metadata；`load_context_payloads` 却读 ancestor 自己的 snapshot，普通 fresh parent 没有该字段，祖先被跳过。
- 如果 legacy metadata 使混合父/子 payload 真正加载到 hidden child 的 root 执行，统一 own 分类会使 Micro 按全局 MessageId 更新 parent flags，而 Full 的 thread-scoped 事务拒绝 parent ID 并回滚。普通 ACP `session/fork` 的独立 ID 复制路径本轮未发现这个问题。

### 本次验证与后续修复入口

本次审计新加 5 个测试：1 个默认运行的成功 Full 循环 characterization，4 个带明确原因的 ignored regression。4 个 ignored 测试已显式执行并失败；ignored 只为不把此次审计变成默认套件永久失败，绝不代表缺陷已验证通过。修复时须移除 ignore，并调整 characterization，使其锁定正确行为。

```bash
# 应当绿色：完整成功 Full 循环已复现当前错误行为
cargo test -p peri-agent --lib test_audit_run_react_loop -- --nocapture
# 当前应当红色：4 个明确的正确性断言
cargo test -p peri-agent --lib test_audit_ -- --ignored --nocapture
# 默认 suite 的绿色不能替代上面的 4 项失败
cargo test -p peri-agent --lib -- --quiet
```

- 初始 baseline：compact 过滤 190 passed；run_react_loop 过滤 10 passed，均 exit 0。
- 新增 RCRA characterization：1 passed，exit 0，2 次 Full 真正成功。
- 显式 ignored regression：0 passed / 4 failed，exit 101；失败分别为 19,763 虚假 saving、压制 Full、74k 未计工具增长、1,882 重复 saving。
- 最终默认 peri-agent suite：731 passed / 4 ignored，exit 0；期间工作区其他任务新增了 1 项测试。
- `cargo clippy -p peri-agent --all-targets -- -D warnings`：exit 0；审计测试 rustfmt 检查、`git diff --check` 通过。
- 独立只读审查确认测试 seam 与断言有效。工作区其他任务的 reminder/replay/storage 变更不属于本次审计产出。

建议修复顺序与验收：

- [ ] A1：planner、estimator 与 renderer 使用一致可见集合；默认 stale 下 excluded 永不入选，虚假 saving 不能压制 Full。
- [ ] A2：明确一次已发送请求的 projection 基线，禁止重复计算增量收益；不要把 planner saving 写入 provider usage。
- [ ] A3：在最终工具结果提交处接入压力增长，覆盖零 usage、失败结果、PTC 最终结果与避免重复计数。
- [ ] A4：host 采纳已提交 canonical progress 与终态成功与否解耦；增加 Full→cancel/error→next turn、Full→writer failure 的真实恢复测试。
- [ ] A5：量化不可压缩 reminder/frozen/system/tool schema 基线，建立有语义的生命周期或无法降压 outcome。
- [ ] A6：恢复 payload provenance、flags 与 snapshot producer/consumer 契约，防止 child 写 parent flags。
- [ ] 用现场同一会话实际 provider input 与事件链补证，量化成功 Full 后基线和额外调用成本，再决定是否需要 no-progress guard。

此前「仅旧压力样本去重即可、不加 no-progress guard」的结论应保留为当时证据下的决策，不能作为本次剩余问题已经解决的依据。此次尚未修改生产策略或关闭 P0。
