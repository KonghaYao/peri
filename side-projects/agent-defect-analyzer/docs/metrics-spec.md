# Agent 缺陷分析 — 标准指标

定义分析指标体系，按用户场景组织。标注含义：

- ✅ 可直接从 DB 计算
- ⚠️ 可计算但有限制或边界情况
- ❌ 不可行，需额外埋点或数据源，已提供替代或标注

> **v2 更新（2026-06-23）**：移除 LineEdit 相关指标（工具已删除）；新增场景七（搜索卫生）、场景八（LLM 调用效率）、场景九（人机协作质量）；场景一/二/三各扩展新指标。

---

## 场景一：工具可靠性

> 工具调用的正确性和稳定性。

1. ✅ **工具失败率** —— 按工具类型统计 is_error 占比（严重度 critical）
2. ✅ **错误类型分布** —— 参数错误（param_parse/timeout/out_of_range）/ 匹配错误（not_found/not_unique）/ 系统错误（interrupted/tool_not_found/subagent_error），三分类
3. ✅ **连续失败序列** —— 同一工具连续失败的最大长度
4. ✅ **Grep 重复搜索率** —— 同 session 内重复相同 pattern 的次数
5. ✅ **参数错误细分**（新增）—— 按工具 × 错误消息关键词交叉，定位高频参数错误源。从 tool_use input JSON keys + tool error message 关联提取，输出 Top-N「工具→错误模式」对。用于指导工具 prompt/schema 优化

> ~~**Edit 执行成功率**~~ —— 已随 Edit 移除废弃。Write 失败率在指标 1 中体现。

## 场景二：会话效率

> 单次会话是否高效完成任务，减少无效轮次。

1. ✅ **人均消息数** —— `threads.message_count` 可直接计算分布
2. ✅ **工具调用/轮次** —— 每轮 assistant 消息的平均 tool_use block 数
3. ⚠️ **死循环检测** —— 同工具+同参数连续重复 N≥5 次。语义循环（同效果、不同参数）无法检测
4. ✅ **会话时长** —— `created_at` 到 `updated_at` 的时长分布
5. ✅ **冗余 Read** —— 同文件被 Read 多次且中间、后续均无编辑，排除 offset 递增的连续分页阅读
6. ✅ **搜索→Read 联动率** —— 搜索工具（Grep/Glob/WebSearch）调用后 N 步内对结果文件发起 Read 的占比。按工具和步数交叉细分：紧邻联动（1步）/ 延迟联动（N步）/ 零联动（搜索无效）
7. ⚠️ **用户中断信号**（新增）—— 从 tool 消息的 `interrupted`/`cancel` 错误推断。按会话聚合中断次数、中断时机（第几轮触发）、中断后会话是否恢复。限制：无法区分用户主动中断 vs 系统超时/取消
8. ✅ **极短会话占比**（新增）—— 消息数 ≤3 且无工具调用的会话占比，标记为「放弃信号」。结合会话时长 <1m 交叉验证

## 场景三：资源消耗

> 上下文窗口利用和 token 开销。

1. ✅ **编辑工具入参大小** —— Write 的 input JSON 字节分布（P50/P95/max）
2. ✅ **编辑工具出参大小** —— tool_result 字节分布
3. ✅ **超大入参检测** —— Write 超过阈值（50KB）的具体消息
4. ✅ **超大出参检测** —— tool_result 超过阈值（20KB）的具体消息
5. ✅ **手动 Compact 触发频率** —— 用户主动执行 `/compact` 命令的次数
6. ⚠️ **自动 Compact 推断**（新增）—— 通过检测消息序列中的 compact 摘要标记推断 auto/micro compact 触发次数。`threads.snapshot_at_message_id` 非空的会话可能对应 compact 快照点。限制：精确压缩比、触发时上下文占用率需 tracing 埋点

## 场景四：功能采纳

> 新功能的实际使用情况和效果。

1. ⚠️ **Skill 调用频率** —— 两维度：System 消息中的 session 级加载标记；Agent 工具调用的 subagent_type 参数（通过子代理分发）。两者都不能完美映射「LLM 每次使用 skill 知识」的语义
2. ⚠️ **Skill 链深度** —— 通过 `parent_thread_id` 递归遍历子代理层级。一级嵌套直接可查，深层需递归
3. ✅ **工具使用多样性** —— 每 session 使用的不同 tool_use name 种数

> ~~**LineEdit 使用率/成功率**~~ —— 已随 LineEdit 移除废弃。

## 场景五：编辑质量

> 文件编辑操作的正确性和效率。

1. ✅ **重读率（纯验证）** —— 编辑后读回同一文件且无后续编辑（严重度 high）
2. ✅ **重读率（编辑链）** —— 编辑后读回同一文件，含后续编辑（结构性重读，不可消除）
3. ✅ **Write 文件大小** —— Write 写入内容的字节分布

> ~~**连续编辑能力（LineEdit 链长）**~~ —— 已随 LineEdit 移除废弃。

## 场景六：SubAgent 协作

> SubAgent 的调用效率和产出质量。按 SubAgent 类型分层分析。

1. ✅ **内置 Agent 分类分析** —— 按 `subagent_type` 分组，统计各类数量、均消息数、工具使用模式（搜索/编辑/执行占比）、Top 工具，自动判定特化方向（只读型/研究型/编辑型）。编辑型与非编辑型分类统计
2. ✅ **SubAgent 消息量** —— `threads.message_count` 分布（P50/P95/max）
3. ✅ **SubAgent 工具错误率** —— tool 消息 is_error 占比
4. ✅ **SubAgent 产出比** —— 编辑类工具调用 / 总 tool_use block 数。按类型分层：编辑型与探索型分别统计

### 研究方向：general-purpose 场景特化

从父线程 Agent 调用提取原始 prompt，按实际工具使用分类 general-purpose SubAgent 的调用场景：

- **纯搜索**（无编辑工具使用）→ 可用 explore 替代，省 ~40% 工具描述 tokens
- **搜索+编辑** → 建议创建 `coder` 特化 agent，工具集精简（去 WebSearch/Agent/folder_operations），92.3% 为实现类任务

每场景展开：任务类型分布、高频工具覆盖率、消息量 P50/P75/P95、典型 prompt 示例。同时支持 `--export N` 导出指定类型的前 N 个会话文本供人工评估。

## 场景七：搜索卫生

> Glob/Grep 巨型调用的检测与分析——上下文窗口的「沉默杀手」。已实现于 `search_hygiene.ts`。

1. ✅ **巨型调用率** —— 按工具细分结果大小 >20KB（巨型）/ >50KB（严重爆炸）的占比。Glob 和 Grep 分别统计 P50/P95/max
2. ✅ **爆炸 pattern Top-N** —— 聚合 >50KB 的 (tool, pattern) 组合，按次数和累积字节排序
3. ✅ **危险路径命中率** —— path 参数含已知风险目录（`.claude/`、`node_modules/`、`plugins/cache/`、`worktrees/`、`target/`、`dist/`、`build/`）的调用占比，及其中巨型/爆炸占比
4. ✅ **危险 glob pattern 命中率** —— 宽泛递归 pattern（`**/*`、`**/*.<ext>`、`*`）的调用占比及爆炸率
5. ✅ **Grep head_limit 配置缺陷** —— 按状态分桶（=0/缺失/≤50/51-250/>250），交叉 >20KB 占比。缺失 head_limit 是巨型结果的主要成因
6. ✅ **单会话堆积** —— 巨型调用最密集的会话 Top-N（次数 × 累积大小）
7. ✅ **高危项目** —— 按 `thread.cwd` 聚合巨型调用量，定位哪些项目最「费上下文」

## 场景八：LLM 调用效率

> LLM 调用本身的效率和成本。当前 DB 仅存储对话消息，不包含 LLM 调用元数据（token/延迟/重试），多数指标需 Langfuse 联动。

1. ❌ **LLM 调用次数/会话** —— 每会话的 LLM 请求次数。需 Langfuse session 关联
2. ❌ **Token 消耗分布** —— input/output/total token 的 P50/P95/max，按会话和模型分组。需 Langfuse
3. ⚠️ **工具/LLM 轮次比** —— tool_use 总数 / assistant 消息数。可从 DB 计算，作为「每轮 LLM 调用的工具利用率」近似替代。比值低说明 LLM 多轮空转
4. ❌ **LLM 重试率** —— RetryableLLM 指数退避触发次数及错误类型分布。需 tracing 埋点
5. ❌ **LLM 响应延迟** —— 首_token 延迟 / 完整响应延迟分布。需 Langfuse

> **数据源缺口**：当前 `messages` 表只存最终对话内容，不含 LLM 调用的请求/响应元数据。建议在 `peri-agent` 的 `ReactLLM` 层增加 tracing span，通过 Langfuse 采集后联动分析。

## 场景九：人机协作质量

> 用户与 Agent 的交互信号——中断、审批、放弃。反映用户满意度（体验层）。

1. ⚠️ **用户中断率** —— 从 tool 消息的 `interrupted`/`cancel` 错误推断。按会话聚合中断密度（中断次数/消息数），中断时机分布（第几轮触发），中断后是否恢复（中断后是否继续有消息）。限制：无法区分用户主动中断 vs 系统超时
2. ❌ **HITL 审批率** —— 被 HITL 中间件拦截的工具调用 / 总工具调用。审批事件未持久化到 DB，需在 HITL 中间件增加埋点
3. ❌ **审批拒绝率** —— 用户拒绝的工具调用 / 被审批的工具调用。同上需埋点
4. ❌ **审批等待时长** —— 从工具请求到用户批准/拒绝的耗时分布。需记录审批事件时间戳
5. ❌ **YOLO_MODE 使用率** —— 从 `config` 字段解析 YOLO 开启的会话占比。当前 `threads.config` 字段为空（373 会话全部 NULL），需在 `session/new` 时写入 config 快照

> **数据源缺口**：HITL 审批流程发生在中间件层，当前不持久化。建议在 `HITLMiddleware` 增加 `AgentEvent::ApprovalRequested`/`ApprovalResult` 事件并写入 DB。

---

## 跨场景限制

| 维度 | 现状 | 解决方向 |
|------|------|----------|
| **Provider/模型分层** | ❌ `threads.config` 全部为空，无法按 Provider（Anthropic/OpenAI）或模型名分组交叉分析 | 在 `session/new` 时写入 config 快照（Provider/model/base_url），或通过 Langfuse session 关联 |
| **时间序列趋势** | ⚠️ 各指标可按时间窗口（`--since N`）切片，但无内置趋势对比（需手动跑两个窗口对比） | 增加趋势分析脚本，自动按周/月切片并对比 |
| **语义级检测** | ❌ 当前全部为结构/统计层面分析，无法判断回答是否偏题、编辑是否改对位置 | 引入 LLM-as-judge 对采样会话做语义评估 |

---

## 使用方式

```bash
cd side-projects/agent-defect-analyzer

# 全量运行所有场景
bun run src/metrics/tool_reliability.ts --since 168
bun run src/metrics/session_efficiency.ts --since 168
bun run src/metrics/resource_consumption.ts --since 168
bun run src/metrics/feature_adoption.ts --since 168
bun run src/metrics/edit_quality.ts --since 168
bun run src/metrics/subagent_collab.ts --since 168
bun run src/metrics/search_hygiene.ts --since 168

# 或通过 npm scripts
bun run tool-reliability -- --since 24
bun run session-efficiency
bun run resource-consumption
bun run feature-adoption
bun run edit-quality
bun run subagent-collab
bun run search-hygiene

# 导出 general-purpose 会话文本供人工评估
bun run src/metrics/subagent_collab.ts --since 168 --export 3
```

> 场景八（LLM 调用效率）和场景九（人机协作质量）的新指标尚未实现脚本，待数据源就绪后开发。
