# MetaHarness 波 4 C5——实施质量审查 P2 修复实施决策记录

**状态**：Closed（C5 已实施）
**优先级**：中（实施质量审查 P2 项修复）
**类型**：决策记录
**创建日期**：2026-08-14
**来源**：`spec/issues/2026-08-14-meta-harness-advisor-review.md` 最终报告 P2-1 ~ P2-6
（P0/P1 无）；C4 遗留问题 1（`FrozenSessionData::subagent_system_prompt` 移除）

## 背景

advisor 红队复审（C1-C4 完成后）给出 P2 共 6 项：2 项测试覆盖缺口（P2-1 链
收集与静态声明双轨无直接对拍、P2-2 persona/language 无用户配置时覆盖注入无
测试锁定）、1 项文档同步（P2-3 wave3 spec 头部状态未同步）、3 项已记录遗留/
行为观察（P2-4 subagent_system_prompt 遗留字段建议尽快移除、P2-5
project_enabled_sections 未接入生产渲染面、P2-6 workflow agent 提示词含
10_hitl 审批决策描述）。本批逐项处理。

## 实施决策

### D1：P2-1——链收集与静态声明直接对拍测试（落地）

**位置**：`peri-acp/src/host/executor_flow_test.rs` 新增
`chain_collection_parity_with_build_collected_sections`。

**放置理由**：`build_collected_sections` 是 peri-acp `pub(crate)`，peri-middlewares
不依赖 peri-acp（crate 拓扑限制），对拍测试只能放 peri-acp；executor_flow_test
已有 `NoopBroker` 与装配基础设施，关注点（完整装配路径）一致。

**测试形态**：构造最小 `AssemblyContext`（复刻 assembly_test `base_context`
的持有者相关字段，条件注册全部关闭；`ParityFakeLlm` / `ParityFakeModel` /
`ParityFakeEventHandler` 三个 unimplemented fake），经 `ProductionChainAssembler`
真实装配后 `chain.collect_prompt_sections()` 与
`build_collected_sections(&MetaHarnessState{disabled_middlewares}, overrides, language)`
**集合对拍**（(id, zone, order, 内容) 四元组排序后相等）。

**集合而非序列对拍的理由**（契约 2 语义）：收集不承诺顺序——链收集按
middleware 链序（blueprint 中 Skills 在第二组、Hitl/SubAgent 在第五组），
静态声明按持有者声明序；两者内容相同但顺序不同是**设计事实**（渲染面统一
按 (zone, order) 排序装配），顺序正确性由既有位置属性测试独立锁定。本测试
锁定的是 review 指出的不变式：「5 个持有者的装配条件全部只按 disabled 集合
过滤」——未来若装配条件因非 disabled 原因排除持有者（链收集少段而静态声明
仍收集），测试立即失败，防止冻结提示词与链状态静默失同步。

**case 覆盖**：默认装配 / 关闭 gated 持有者（Hitl+SubAgent+Skills）/ 关闭
基础持有者（DefaultSystemPrompt+Lang）/ 关闭全部持有者 / persona+语言注入。

**未采纳备选**「删除链收集接口」：`collect_prompt_sections` 虽无生产调用点，
但它是契约 2 的装配期收集通道声明（chain.rs doc 注释），且 assembly_test 的
6 处链收集测试（含 `chain_collected_gated_sections_match_projection` 对拍）以
其为载体——删除接口会连带删除链侧收集行为的全部守护，双轨面不降反失守。

### D2：P2-2——persona/language 无用户配置时覆盖注入测试（落地）

**位置**：`peri-acp/src/prompt/prompt_test.rs` 新增
`meta_harness_override_persona_without_overrides` /
`meta_harness_override_language_without_config`。

**锁定路径**：「空内容段 + 覆盖注入」组合——persona/language 段恒声明
（无 overrides / 无语言配置时内容为空串），默认不渲染（空内容过滤），但
MetaHarness 覆盖（`.peri/meta/persona.md` / `.peri/meta/language.md`）先于
空内容过滤合并，覆盖全文必须渲染。每个测试三断言：① 无配置时收集段内容
为空串（恒声明前提）；② 覆盖注入后渲染含覆盖全文；③ 无覆盖时默认输出
不含覆盖标记（空内容默认不渲染）。

### D3：P2-3——wave3 spec 头部状态同步（落地）

`spec/issues/2026-08-14-meta-harness-wave3-examples-and-safety.md` 头部
`**状态**：Open` → `Closed`（变更记录表 C4 已记 Open → Closed，头部未同步，
纯文档修正）。

### D4：P2-4——`FrozenSessionData::subagent_system_prompt` 完整移除（落地）

C4 遗留问题 1，review 建议「下批次一次性移除」。移除面：

| 项 | 位置 | 动作 |
| --- | --- | --- |
| 字段 + accessor | `peri-agent/src/session/exec/executor.rs` `FrozenSessionData`（:151 字段、:186 accessor） | 删除；子面向调用点统一改 `system_prompt()` |
| 构造参数 | `from_frozen_parts` 第三参（executor.rs）+ 调用点 `peri-acp/src/session/mod.rs`（`build_frozen_data`，原传 None） | 删除 |
| fork/bg-fork 复制 | executor.rs fork 拦截路径（`frozen_system_prompt` 注入）与 `build_and_execute_agent` 解构（六元组 → 五元组） | 改为直接复用 `system_prompt()` |
| 请求载体 | `executor_helpers.rs` `StageBuildRequest` / `V2ExecuteRequest` 字段 + 构造点 | 删除 |
| stage 装配 | `stage_builder.rs` `build_agent` / `build_stage_context` 参数 + `system_prompt_for_sub` 回退 + SubagentHost `frozen_system_prompt` | 参数删除；`system_prompt_for_sub = system_prompt.clone()`；host 字段恒 None（现状等价，spawn 主路径从 parent session 读 frozen prompt，不经此字段） |
| ACP 装配面 | `peri-acp/src/host/stage_builder.rs` / `host/prompt.rs` / `host/stdio/session/prompt_exec.rs`（StageBuildFn 闭包与 workflow agent 上下文） | 参数/字段删除；`f.system_prompt().to_string()` |
| workflow agent | `peri-acp/src/host/workflow_agent.rs`（accessor 调用） | 改 `system_prompt()` |
| 测试 | `executor_flow_test.rs`（StageBuildFn 透传 + `test_frozen_subagent_prompt_identical_to_main` 回退断言）、`mod_test.rs`（SubAgent 无 workflow prompt 断言） | 透传删除；断言改为直接锁定主 prompt（子面向唯一复用来源） |

**行为不变确认**：恒 None + accessor 回退 `system_prompt()` 语义固化（C2 起
16_workflow 删除后两版字节相同），全部改动为机械删除/等值替换；回退语义由
既有测试锁定（ARC-FROZEN-001 冻结链）。移除后全仓 `subagent_system_prompt`
零残留（仅剩说明性注释）。

### D5：P2-5——`project_enabled_sections` 保持现状（保留理由）

**保留**。review 建议「保持现状或切换渲染面 gate 判定为投影结果」。维持
现状的理由：

1. **不存在 gate 判定双实现**：C3 后全部 gated 段 gate = 持有者装配（收集即
   装配），渲染面 `PromptTemplate::new` 对收集段 gate 恒 `Always`——收集机制
   天然承担判定，渲染面**没有**第二套判定逻辑可漂移（`PromptFeatures::detect`
   仅剩 15_channel 恒 false 硬编码，与投影表无关）。
2. **投影是契约 3 的显式视图**：`project_enabled_sections` 服务
   `chain_collected_gated_sections_match_projection` 对拍测试（assembly_test
   :1228），把「链上持有者名集合 → 段落 ID 集合」的映射表消费显式化；若删
   除，映射表 `SECTION_HOLDER_MIDDLEWARE` 失去唯一测试消费者。
3. 「切换渲染面 gate 判定为投影结果」会**引入**新的接线面（渲染面需接收
   投影结果），反而增加双轨风险，与契约 3「收集即装配」的简化方向相反。

### D6：P2-6——workflow agent 提示词含 10_hitl 审批决策描述（保留理由）

**保留**。review 定位为「行为观察，C3 D5 已记录」并建议「跟踪评估」。维持
现状的理由：

1. **既定决策**：workflow 与主链共用段落来源（`build_collected_sections` 不
   区分链上下文），C3 D5 已记录在案并有测试锁定。
2. **修改成本与收益不对称**：在 10_hitl 段落内加「workflow 不经历审批」注
   明会污染主会话模型指令（主链有 HITL，描述是准确的）；渲染面按链上下文
   过滤 10_hitl 属行为变化（workflow 模型失去审批机制描述），超出审查修复
   范围，需设计裁定。
3. 该文本对 workflow 运行时的实际影响是「模型可能询问审批」级别的误导，
   无安全/正确性风险（审批 UI 不存在时模型无法触发审批，仅浪费一轮），
   跟踪评估即可。

## 验证

| 命令 | 结果 |
| --- | --- |
| `cargo build --workspace` | ✅ |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ 零警告 |
| `cargo test -p peri-acp-types -p peri-acp -p peri-middlewares -p peri-agent --lib` | ✅ 139 + 388 + 1328（3 ignored 既有）+ 667 全绿（peri-acp +3：P2-1 对拍 1 + P2-2 覆盖注入 2） |
| `cargo test --workspace --doc` | ✅ 15 crate 全绿 |

残留检查：`subagent_system_prompt` 代码零残留（仅 `factory.rs` / `workflow_agent.rs`
等 3 处"已随 C5 移除"说明性注释）；`frozen_system_prompt` 保留（SubagentHost /
fork 路径字段，消费正常）。

## 偏差记录

1. 对拍测试首版以**序列**对拍失败（链收集按链序、静态声明按声明序，顺序
   不同）——修正为**集合**对拍并注释契约 2 语义（收集不承诺顺序，顺序由渲
   染面位置属性承担）。这不是实现缺陷，是对拍方式选择问题。
2. 新增测试触发 3 个 clippy 错误（type_complexity ×2 / field_reassign_with_default），
   按 clippy 建议修正（类型标注省略 / struct 更新语法）。
3. 工作区存在并发未提交改动（acp-hub 等），非本批范围，未触碰。

## 遗留问题

1. `project_enabled_sections` 未接入生产渲染面（C3/C4 遗留，D5 说明保留
   理由；一致性由对拍测试保证）。
2. workflow agent 提示词含 10_hitl 审批决策描述（C3 D5 / D6 保留理由；长期
   处理方案待设计裁定：段落注明或渲染面按链上下文过滤）。

## 涉及文件

- `peri-acp/src/host/executor_flow_test.rs` — P2-1 对拍测试（+fake +helper）
- `peri-acp/src/prompt/prompt_test.rs` — P2-2 覆盖注入测试 ×2
- `spec/issues/2026-08-14-meta-harness-wave3-examples-and-safety.md` — 头部状态 Open → Closed
- `peri-agent/src/session/exec/executor.rs` / `executor_helpers.rs` / `stage_builder.rs` / `session/factory.rs` — P2-4 移除面
- `peri-acp/src/session/mod.rs` / `mod_test.rs` / `host/stage_builder.rs` / `host/prompt.rs` / `host/stdio/session/prompt_exec.rs` / `host/workflow_agent.rs` — P2-4 移除面（ACP 侧）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-14 | — | Closed | agent | C5 实施完成：P2-1 对拍测试 / P2-2 覆盖注入测试 / P2-3 spec 头部同步 / P2-4 subagent_system_prompt 完整移除；P2-5/P2-6 保留并记录理由；四命令验证全绿 |
