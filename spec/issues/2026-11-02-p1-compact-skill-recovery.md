# P1：Full Compact 归档 skill 正文后缺少恢复清单与首动作门禁

**状态**：已确认，待修复。
**优先级**：P1。
**类型**：错误（长会话上下文管理核心行为）。
**创建日期**：2026-11-02。
**来源**：GitHub issue #308「full compact 导致 skill 中承载的行为模式被遗忘」及其评论中的 B 方案。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `8349580e4`，等于 `origin/main`）。

## 确认证据

1. skill 正文以普通 tool result 返回：`peri-middlewares/src/skills/tools.rs:107-135`。Full 归档 own 区所有非 System、未保护条目：`peri-agent/src/agent/compact_v2/planner.rs:73-81`，调用处 `peri-agent/src/agent/compact_v2/full.rs:152-180`。因此 `SkillTool` 的 ToolUse/ToolResult 在 compact 后消失。
2. 摘要 user prompt 只有 5 个优先级（active request / current state / decisions / files / failures），没有活跃 skill 或操作模式必保节：`peri-agent/src/agent/compact_v2/descriptions/summary_user_prompt.md:5-11`。
3. 当前续接标记鼓励直接继续，且方位词与实际顺序相反：`peri-acp-types/src/compact.rs:10-11`；摘要消息实际顺序为 hint 在前、摘要在后：`peri-agent/src/agent/compact_v2/full.rs:647-654`。
4. 摘要截断恢复提示只点名 active request、constraints、work status、next step；新增恢复节若不同步，恢复轮不会补写：`peri-agent/src/agent/compact_v2/full.rs:47-56`。
5. 摘要模型只能从 transcript 的 `ToolUse{SkillTool}` 与正文来源标注归纳激活状态；仓内没有结构化 active skill 记录，`extract_skill_names` 只解析旧格式文本（`peri-acp-types/src/compact.rs:132-145`）。
6. issue #308 的观测数据：172 次 compact 前有 skill 激活，仅 22% 在 compact 后重新调用 SkillTool；案例 `01a1287c` 摘要逐字保留规则仍未执行，`01a1523b` 把偏离写成当前状态。

## 根因链

**观察**：SkillTool 返回的 skill 正文是普通 tool result → Full 将它与其他 own 历史一起归档 → 正文离开活跃上下文，但摘要 prompt 不要求登记需要重载的 skill → compact 后 hint 只说继续，没有“第一个动作前恢复”的门禁 → 模型把摘要里的规则当作历史交接而非当前模式，直接执行工具 → 重复 compact 时上一份摘要被合并，恢复信息继续丢失。

**观察**：摘要 prompt 的“当前状态与下一步”会忠实记录最近实际行为；当最近行为已偏离 skill，摘要会把偏离固化成新的当前计划，而不是恢复契约。

**推断**：恢复节本身不能保证执行；需要 hint 把重载写成第一个动作的硬约束，并明确“摘要规则是提示、重载后的正文才是权威”。

## 影响面

- 用户影响：长程任务在 compact 后忘记 skill 承载的行为模式（委派、验证、workflow、禁止直改），表现为直接工具占比上升、workflow 不启动、commit 前不验证等。
- 受影响模块：`peri-agent/src/agent/compact_v2/`（摘要 prompt、摘要消息、恢复提示）、`peri-acp-types/src/compact.rs`（续接标记）、`peri-tui` 与 `compact_pipeline`（标记识别）。
- 是否可规避：用户手动重新输入 `/skill` 或模型自行重载；非系统性，22% 基线说明多数情况不会发生。
- 定级依据：核心上下文管理路径错误、有稳定可复现的代码机制与多案例证据、存在临时规避；不涉及数据丢失、安全或无法启动，故为 P1。

## 修复方案

采用 issue #308 评论的 B 方案（恢复清单 + 接续提示），限定在提示词与标记识别范围：

1. **续接标记重框（P1-1）**：改写 `CONTINUATION_HINT` 为单行文本，包含“摘要在下”“若存在 Recovery resources，先完成其要求的第一个动作再调用其他工具”“用 SkillTool 重载每个列出的非 preloaded skill”“摘要规则是提示、重载正文权威”“更新的用户显式指令优先”。标记文本不得包含 XML 特殊字符（`<`、`>`、`"`、`'`、`&`）：摘要消息进入模型视图会经 legacy codec 转义，否则首行识别与结构断言会失配。
2. **历史标记兼容**：保留旧标记为 `LEGACY_CONTINUATION_HINT`，新增单一识别函数；`legacy_compact_reminders`、TUI detect、`compact_pipeline` 统一走该函数，已持久化旧摘要不得失去 `compact_summary` 识别。
3. **恢复清单（P1-2）**：`summary_user_prompt.md` 将 `Recovery resources` 放在优先级 2（活跃请求之后、当前状态之前），要求：
   - 每个 skill 首行写 `SkillTool("<exact canonical name>")`，标注来源与 `body archived by this compaction` / `preloaded; do not reload`；
   - 只列实际激活过的资源，不得臆造名称；
   - 一条核心约束作为提示，重载正文权威；
   - 其他恢复资源：必须重读的文件、正文已归档的 MCP/deferred 工具、仍在运行的 background task/subagent；
   - 无法重载时报告不可恢复，不得臆造规则。
4. **契约 vs 实际（P1-3）**：恢复节要求写 `Contract: X. Recent actual: Y (deviation).`；恢复目标是契约，只有用户显式指令可接受或覆盖，不得把偏离写成新的 current plan。
5. **重复 compact（P1-4）**：在“Earlier summaries”段增加恢复项例外；每次 compact 重新列出仍活跃 skill，保留上一份摘要中未完成的恢复项；用户明确结束或任务已结论才省略；冲突 skill 记录用户最新选择。
6. **提示同步**：`summary_system_prompt.md`、`SUMMARY_RECOVERY_PROMPT`、`SUMMARY_FINAL_PROMPT` 点名 recovery resources；`SUMMARY_REGENERATION_PROMPT` 的 “every priority section” 保持不变。
7. **文档**：更新 `docs/code-index/peri-agent.md` 的 Full 摘要行为说明与 `docs/design/system-reminder.md` 的续接标记兼容说明。

明确不做：

- 不改 `legacy_compact_reminders` 的 kind 或 Legacy provenance，不改 rewind 逻辑。
- 不新增运行时 `after_compact` 自动注入、确定性 recovery manifest、数据库字段或 ACP caps/事件。
- 不做 issue #308 P1-5 的真实模型行为 fixture（需要外部 provider）；本次用确定性 prompt/消息/标记测试覆盖，行为指标留作 P2。
- 不改 `summary_max_requests`、摘要预算与 provider 输出上限语义。

## 验收标准

1. 新摘要消息首行为新 `CONTINUATION_HINT`，且为单行；旧持久化摘要仍被识别为 `compact_summary`（模型视图、TUI、ACP replay、手动 `/compact` 重排）。
2. 摘要 prompt 的 `Recovery resources` 位于优先级 2，包含 `SkillTool("<exact canonical name>")`、preloaded/archived 状态、`Contract: X. Recent actual: Y (deviation).` 与重复 compact 传递规则。
3. 两个恢复提示均点名 recovery resources。
4. `peri-acp-types`、`peri-agent`、`peri-tui` 目标测试通过；`bash scripts/check-changed.sh` 与 `git diff --check` 通过。

## 验证记录

- `cargo test -p peri-acp-types --lib` → 693 passed，0 failed。
- `cargo test -p peri-agent --lib` → 1421 passed，0 failed，5 ignored。
- `cargo test -p peri-acp --lib` → 1112 passed，0 failed，1 ignored。
- `cargo test -p peri-tui --lib` → 2627 passed，0 failed，9 ignored。首次与其它 cargo 任务并发时 2 个用例受负载影响失败，单跑与干净重跑均通过。
- `cargo test -p peri-agent --test compact_pressure_adversarial_test --test compact_failure_adversarial_test --test compact_session_adversarial_test --test compact_session_manual_adversarial_test --test compact_authority_projection_test --test compact_followup_behavior_test` → 全部通过。
- `bash scripts/check-changed.sh` → exit 0。
- `cargo fmt --all -- --check` → exit 0。
- `git diff --check` → exit 0。

夹具调整（均由续接标记变长引起，非产品行为变化）：

- `projection_borrowed_test.rs` 的旧摘要金标改用 `LEGACY_CONTINUATION_HINT`。
- `resident_budget_test.rs` 的新增工具结果从 1000 字节降为 700 字节，保留「总量超限、自有区未超限」边界。
- `compaction_loop_test.rs` 第二次 Full 的 `estimated_tokens_before` 期望从 97082 更新为 97081。
