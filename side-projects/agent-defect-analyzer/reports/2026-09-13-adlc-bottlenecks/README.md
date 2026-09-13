# ADLC 长流程卡点研究

日期：2026-09-13。状态：历史审计与改进候选；未修改生产 ADLC、Workflow 或产品实现，未验证优化收益。

**最值得先改的是结果协议、恢复粒度和边界检查的成本。** 多个长流程既有真实产品缺口，也有输出被拒收、模型不可用、超时和收尾检查失败。笼统增加重试或要求“继续做到完成”，不能区分这些原因。

## 研究覆盖

从创建时间在 `[2026-08-30T00:00:00Z, 2026-09-14T00:00:00Z)` 的 221 个可见根会话中做目的抽样，保留 7 个根会话的 3,046 条存留消息记录，涉及 6 组研究材料。sandbox 的两个根会话合并为一个目标；Meta 只剩后段，System-reminder 主要是后续整理提交，不能作为完整 ADLC 成功或短流程高效的样本。

三名 Luna 分工取数、现行契约核查和运行日志统计，随后分别审阅 sandbox、Micro/MQ、TUI/Meta/System；协调者回查关键原文、修正引用和观察边界。另由 Luna 审查研究脚本。这里不是六个完整任务的双盲评分，也不计算总体低效比例。

65 个分页覆盖这 3,046 条存留记录，文件 hash、唯一 ID 和顺序校验通过。132 条源记录带截断标记；此外存在压缩摘要、工具自身裁剪和上下文投影，不能把分页完整等同于历史完整。详见 [抽样与覆盖](sampling.json)、[验证记录](validation.json)、[方法](METHOD.md)。

本地五份 ADLC manifest 登记了 54 次物理运行，现存 50 次；Meta 缺少 4 次，另有 5 个现存运行缺少 journal。MQ 的运行材料不在本仓库这批日志中，它的空运行证据来自历史通知。两个逻辑 Workflow 可以对应多次物理执行，运行次数不等于浪费次数。

现存 journal 有 153 条新执行尝试：131 条 `ok`、22 条 `dead`。直接按子尝试的 reason/detail 分解，后者是 18 次 provider HTTP 错误、2 次 200-iteration 到限、1 次 schema 类型错误、1 次无效 JSON。`ok` 只说明有返回，缺失 journal 的运行不能补成零失败；分母与逐 run 状态见 [运行摘要](run-summary.json)。

## 案例与卡点

| 案例 | 存留根记录 / 本地运行 | 最有依据的卡点 | 必须保留的区别 |
| --- | ---: | --- | --- |
| sandbox → local MCP | 762 + 526 / 13 | 仲裁整数结果被拒收；评估者跨轮 HTTP 400/403；边界扫描巨大且 round 9 准备因路径拼接错误失败 | 用户中途明确去掉 Docker；真实安全和正确性缺陷也促成了重跑 |
| Micro Compact P0 | 772 / 4 | 4 次运行各撞上约 600 秒限额；部分实现已写入却未形成完整交接；后置范围检查又阻塞 | 主线随后继续修复，独立复核找出 2 个 P1，并用干净基线定位既有测试失败 |
| TUI 长 Markdown CPU | 260 / 8 | 脚本预检纠错、工具预算和 30/60 分钟限额、path allowlist 收尾失败 | 后续确实修复 scheduler、stale publication、长行与解析边界，不能把整轮时间算协调损耗 |
| Meta session | 266 / 登记 18，现存 14 | 用户纠正无关 `peri-cool` 范围；验收契约、缺失门禁与修订元数据继续收口 | 范围纠正没有消除真实测试缺口；可见 trailing-newline 测试先失败后通过 |
| MQ steering | 422 / 本地缺失 | 要求 agent 交接的 W1 在 108ms 内结束，0 agents / 0 tools；随后转为主线实现 | 有实际测试和提交记录；不能据此补造完整 ADLC 独立裁决与验收 |
| System-reminder 后段对照 | 38 / 11 | 原始执行过程在存留根对话中不可见 | 只支持观察“整理后经明确授权提交”，不支持速度比较 |

完整线程 ID、消息 ID、run/seq 见 [证据账本](evidence-ledger.json)。上述“通过”和产品缺陷若仅来自 reviewer/handoff，均作为报告内容理解；本轮没有重新验收这些产品。

## 优先改动

### 1. 修正结构化结果验证，并用相同验证器预检

Sandbox 的仲裁 run `01a090a7-63d9-7d91-923c-3b264fdf62db`，journal seq 0，直接记录 `packet_revision` 的 `integer/number` 类型不匹配。该次 run 起止跨度约 6 分钟，最后返回 invalid。后续只重跑仲裁，没有重做 discovery，这是合理的恢复边界。

当前 `peri-agent/src/agent/workflow/agent/result.rs` 的 `validate_json_schema` / `json_type_name` 仍将所有 JSON Number 标为 `number`，按字符串比较拒绝 `integer`；而 ultra-adlc 的仲裁 schema 仍声明 integer。这是当前静态可确认的契约不一致，尚未在本轮执行生产回归测试。

建议先补整数、小数、嵌套字段和必填字段的真实验证器回归，再让能力探针与正式结果走同一验证路径。不要靠把合法整数声明改成宽松类型，掩盖验证器语义缺口。

### 2. 将边界快照做成有界、可复用的确定性工具

本次 coordinator 自写的 `boundary.py` 从仓库根逐文件 SHA-256，只排除 `.git` 和 Workflow 日志目录。保存的 round 8 快照为 **820,879 个条目、158,699,816 字节**；round 6 比较列出 121,453 个变化路径，其中 69,914 个越界候选，69,845 个位于 `target/`。它们是扫描和变化规模，不能直接归因于本任务，更不是已测量的耗时。

round 9 准备阶段又读取了错误路径 `boundary-boundary-w2-round8.json`，工具明确返回 FileNotFoundError；根会话随后以用户“检查任务进度”结束。约 7 分钟的等待只出现在 assistant 自述中，本报告不把它当实测。

现行 skill 要求边界快照与比较，但没有要求这份全仓扫描算法。建议由确定性工具维护 baseline ID、合法路径、扫描范围和差异；扫描前先验证基线存在和格式，避免先做昂贵工作再发现输入错误。用 Git 事实、显式写入范围和生成目录策略缩小比较成本，同时保留对非 Git 写入与真实越权的检测。无法证明归属的外部变化保持 unknown，不能自动忽略或扩大 allowlist。

验证应同时测读取文件数、字节数、实际运行时间，以及能否仍抓住 allowlist 外的真实写入。原始快照与脚本的 hash 见 [边界证据](boundary-provenance.json)。

### 3. 按阻塞原因恢复，先确认评估能力

Sandbox 多轮保留了模型 HTTP 400/403 和无法解析的 assessor 结果，物理运行里仍有实现、构建、门禁和复核。已有日志也记录了真实缺陷，**尚不能量化其中多少重做可避免**。

恢复计划应明确区分产品缺口、评估能力不可用、结果协议失败和收尾检查失败。只有 accepted revision、产品/依赖 hash 与有效证据均未改变，且唯一缺口是 assessor，才只恢复只读评估。若产品有缺口，继续相应修复及受影响验证。

在昂贵执行前检查所需 profile、工具权限和结构化输出能力；运行中再次失效时保留独立评估者的替代路线和产物。一次探针不能保证数小时后的可用性，400/403 也不能靠不加区分的重复调用解决。原来的独立性和用户授权边界仍需成立。

### 4. 让时间预算对应实际工作包，并保存最小交接

Micro 四次物理运行分别记录约 600 秒到限；超时后，主 Agent 需要重新检查修改、编译状态、测试与 handoff。后来使用窄范围 coder 继续 WP-002，继而推进 sink 和独立复核，说明已有产物可以复用。

建议在现有两个逻辑 Workflow 中按工作包安排物理执行和预算：先完成可编译的 source 交接，再放行依赖它的 sink；接近预算时保存实际修改、已完成检查、剩余项和恢复入口。预算耗尽作为一种恢复原因，不重新解释为产品失败，也不靠全局提高超时来保证完成。

对照实验应故意在 source 未完成、完成但尚未返回、评审中断三个位置终止，检查恢复是否只重做必要部分。现有合法取消和总预算限制必须继续生效。

### 5. 让 ADLC 阶段的产物要求进入完成判断

MQ 的 W1 明确要求多个 agent 产出 handoff，历史通知却是 108ms、0 agents、0 tools，execution completed 与 delivery blocked 同时出现。主线之后做了实现、测试和提交，仍应保留 ADLC 流程未完整履约的事实。

建议为 ADLC 阶段声明必须存在的交付和验收证据，让“阶段结束”带上可操作的业务结果。这个约束只适用于声明了这些产物的阶段；通用 Workflow 的纯 JavaScript、缓存恢复或无需 agent 的脚本可以合法零 agent。

当前引擎固定产生 `acceptance=unknown`，不能直接作为产品完成或失败结论。优先接通业务 verdict 与交付证据，保持引擎状态和业务状态各自的含义。

## 应保留的有效做法

- 仲裁输出 invalid 后只重跑仲裁，保留前面 discovery/design 产物。
- Micro 独立复核抓出两个产品 P1；面对并行套件失败，使用干净基线与当前修订对照，而非默认扩大修复范围。
- Meta 将 `peri-cool` 范围纠正和 CA-001/CA-002 验收缺口分开处理，并有真实失败测试后的定向修复。
- 用户改变 Docker 方向时保留任务修订边界，使旧证据失效范围可解释。

这些例子支持保留独立复核并改善其输入、预算和恢复方式。当前没有足够证据证明“reviewer 太多”或“减少审查轮次”能在相同质量约束下提升效率。

## 结局与当前状态

Sandbox 的存留根对话末尾是 round 9 准备失败和进度询问；当前 manifest 后来记录 round 9 completed，归属于后续 Codex collaboration 收口，不是本批旧 Peri Workflow 的新增 run。Micro 可见同目标记录继续到 baseline-exception 写入和 round 2 复核计划，随后用户切换新目标；当前 artifact 的 accepted 状态不能倒填到这个历史边界。

取证时另一个任务正在修改 Workflow 说明和其他生产文件。本报告用 [源码快照](source-snapshot.json) 绑定静态判断，脚本语法纠错仅作为历史机制，不把正在修复的旧说明当作新的待办。

## 交付与下一轮校验

[机器改进队列](improvement-queue.json) 给出优先级、证据、落点、反例和验证判据。建议先交付“结构化验证器 + 边界检查输入预检”的确定性回归，再做“按缺口恢复 + 工作包预算”的同初始状态对照；收益分别记录，避免多项同时变化后无法归因。

方法已由协调者补入 [agent-task-evaluator](../../../../.claude/skills/agent-task-evaluator/SKILL.md) 的 [长流程审计参考](../../../../.claude/skills/agent-task-evaluator/references/workflow-bottlenecks.md)，并由 auto-data-researcher 路由。它保留了这次实遇的误判校正：role=user 的 skill 投影、run ID 混作 message ID、只读到失败阶段就过早结束观察，以及把父运行错误归到每个子尝试。

研究脚本位于 `scripts/`。重放必须写入新的本地目录；原始对话、完整评审与运行输出保存在忽略目录 `output/adlc-bottlenecks-2026-09-13/`，提交材料仅含安全摘要、引用和指纹。

从 `side-projects/agent-defect-analyzer` 运行（示例目录须尚未包含导出产物）：

```bash
bun reports/2026-09-13-adlc-bottlenecks/scripts/export-adlc-cases.ts --out output/adlc-replay-new
python3 reports/2026-09-13-adlc-bottlenecks/scripts/analyze_adlc_bottlenecks.py --repo ../.. --out output/adlc-replay-new/journals
```

脚本复用当前源数据重导出；历史数据库或本地 audit 文件变化后，不保证重现原 hash。packet 和完整分页分别绑定各自指纹，有界 packet 的截断不阻止完整分页导出。
