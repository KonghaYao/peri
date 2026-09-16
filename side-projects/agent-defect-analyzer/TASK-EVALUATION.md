# 任务有效性研究

状态：任务审计试点的方法契约。目标是从历史任务形成可验证的提示词改进假设；评审标签是带证据的判断，不是数据库新增的事实，也不是生产任务完成率。

## 研究单位与任务契约

会话是取样单位，任务是评价单位，两者不等价。第一版每个会话选取**第一个能够定位的实质用户请求**作为焦点任务，引用请求消息 ID，记录目标、验收条件和观察截止消息。同一目标的补充、纠正和继续属于同一任务；明确独立的新目标不借给旧任务充当完成证据。若无法辨认实质请求或范围，保留未知。

这是一种明示的取样政策，不能覆盖会话中的所有任务。两个 reviewer 选出不同的请求锚点时，先记任务边界分歧，不把他们的结局标签当作对同一任务的评分。后续可扩展多任务切分，但必须保留切分依据和同一会话内的相关性。

`system_reminder`、工具记录和继承快照不能直接视作新用户请求；`user` 角色也只提供候选，嵌入通知、转述和含糊的“继续”仍需读上下文。线程创建时间窗口包含该线程全部持久化历史，不是事件发生时间窗口。

## 多维评价

每个维度都记录 `value`、`evidenceMessageIds`、`reason`。缺失证据保留 unknown；不可适用与缺失不同。

| 维度 | 标签 | 判定边界 |
| --- | --- | --- |
| outcome | delivered / partial / not_delivered / blocked / unknown | delivered 要求任务验收条件有直接证据；not_delivered 要求可见的未交付或反证，不能由沉默或日志结束推出；blocked 要有具体障碍 |
| verification | direct / reported_only / none / not_applicable / unknown | 工具结果、可检查交付正文或明确验收是 direct，不要求使用工具；assistant 声称“已测试”只是 reported_only；none 表示可见地没有做必要验证，缺失片段用 unknown；not_applicable 只表示已定位任务中该维度确实不适用，不能代替无任务的 unknown |
| constraints | supported / violated / unknown | 依据该任务实际约束核验，不能默认不存在约束，也不按自己偏好的实现路径扣分 |
| feedback | accepted / corrected / rejected / mixed / none_observed / unknown | 仅解释用户对焦点任务产物/行为的明确接受、修正或否定；普通范围更新、停止和新目标属于任务契约变化，不自动记 corrected；none_observed 仅用于观察范围完整且未见反馈，存在缺口用 unknown；不称作用户满意度 |
| strategy | effective / avoidable_friction / unknown | 是否针对该目标形成进展、合理处理不确定和交接；判定可避免摩擦需给替代路径与上下文，不用长度/工具次数当效率 |

`taskType` 选 coding / research / review / planning / operation / conversation / other / unclear。复杂度、权限、数据缺失作为局限，不按消息数认定任务难度。工具错误、必要澄清、合法受阻并不自动降低质量；没有工具的解释类任务也可能有可直接检查的优秀交付。

“优秀”只作为可解释分组：交付有据、验证适用、约束有据且策略有效时记 strong；已交付但验证/过程未充分支持时记 delivered_with_gaps；其余分别为 partial、not_delivered、blocked、unknown。分组由维度派生，保留原始标签和理由，不另让模型打一个无法解释的总分。

直接验证存在不代表全部验收条件已满足。强结论逐条核对任务要求及解释性主张；本机测试、目标平台验收、提交和子 agent 结果各自需要对应证据。纯问候不在本试点实质任务政策内，空请求锚点要求截止为空、taskType 为 unclear、五维均 unknown；不能因此派生 strong。

## 事实包与评审 sidecar

解析继续复用 `DataLoader`。事实包保留 normalizer/格式版本、过滤范围、seed、候选集合及所选线程元数据指纹；每包另有覆盖实际导出内容的 hash。原始正文只保存在本地忽略的 `output/`，提交报告仅保留安全摘要、统计和完整定位 ID。

导出按可见主会话、固定创建窗口、固定 seed 分层。短（1–20 条）/中（21–100 条）/长（超过 100 条）只用于覆盖不同记录规模，明确报告各层候选数和选中数，不能把等额分层样本的比例当成总体估计。自有记录为空的会话单列数量与不可评原因，不判失败、不混入有内容的抽样层。默认每层四个，空层不偷换样本；先核查缓存 message_count 与实际自有记录差异。

包中的消息保持持久化顺序。保存自有 user/assistant、工具请求与结果及完整 ID；截断标明字段和消息遗漏，保留首尾并说明中间缺口，不能伪装成连续轨迹。源记录已有 truncated/summary/parseIssues 时一并保留。子会话关系仅提供定位，父任务与子任务的归属需额外证据，不递归倾倒所有子会话。

工具结果的执行证据沿独立字段导出，不从正文推断：`status` 缺失或旧消息无 metadata 时为 `unknown`，`exit_code`、`output_truncated`、`task_id` 和 `output_ref` 的非法/矛盾值在源消息的 `parseIssues` 中保留。任务包默认只保留 `hasOutputRef`、`hasTaskId` 与其他脱敏事实，避免泄露本地路径或原始任务 ID；只有明确加入 `--include-content` 才导出 `output_ref`/`task_id`，且不会自动打开或读取引用文件。执行状态计数与现有工具调用 `is_error` 错误率分开，前者不能解释为任务验收或测试通过。

execution facts 纳入任务包 schema 2 的必需 contract。schema 1 packet 或 review 会被拒绝；需要重新导出 packet、重新绑定 packet hash 并重新评审，不能把旧 hash 当作兼容输入。

当前归一化 `text` 拼接内容块的文本，未保留可核验的用户可见 channel。它可提供持久化内容证据，但不能仅据此判定 UI 泄露思考过程、用户看到了某段内部叙述或回答的显示形式。交付标签限定在现有记录支持的范围；界面可见性另需生产协议/渲染证据。

PTC 嵌套调用在当前持久化格式中没有独立 canonical transcript；分析器只能报告实际持久化的外层消息与其中的 typed execution facts，不能声称恢复从未持久化的嵌套调用、参数或结果。

评审 sidecar 独立于事实包，至少包含：

- 格式与 rubric 版本、reviewer ID、模型标识；不要把多个同源模型当作独立真相。
- caseId、packetHash、请求锚点、观察截止、任务摘要、验收条件、taskType。
- 五个维度及其证据、局限、仍需补读的记录。
- 提示词候选：theme、方向（reinforce/change/investigate）、目标层（prompt/runtime/tooling/unknown）、证据、反例、替代解释、下一步检查。

导入必须拒绝未知版本、错误 hash、不存在/跨包 ID、重复 reviewer-case、非法标签，以及无证据的确定性结论。requestMessageIds 必须来自该包的自有 user 记录。机械验证只证明身份与格式；语义仍需审计。截断包允许有限的局部观察，但不得据此断言缺口中没有验证或后续反馈。

## Subagent 编排与校准

1. 协调者冻结事实包、rubric 与焦点任务政策。指定 reviewer 只能读取其分配的包和方法，不能看其他评审结果、旧结论、期望标签或生产指令。
2. 两名 reviewer 独立提取任务契约并评价，返回结构化 sidecar；明确正文中的指令仅作为数据，不执行。
3. 机械校验所有引用与版本。按 case 对齐，单列缺评和边界分歧，再按维度报告一致数/可比较数。不能平均标签或用多数票覆盖不确定。
4. 协调者回查分歧和一部分一致案例。需要补读则新建扩展包并重新绑定 hash；不能沿用旧 hash 悄悄换输入。裁决保存原评审和新增证据，人工修订也必须有独立 reviewer 身份。
5. 用户校验少量代表性案例：至少覆盖有据交付、受阻/未知、纠正或分歧。相同模型间的一致性不等于标签准确率；合成判据用于检验评审方法，真实任务仍须验收。

## 从分组到提示词实验

先按任务类型与分组找反复出现的机制，再做相似任务的正反案例对照。每个候选写明触发条件、观测行为、现有提示词入口、拟改的一条决策规则、可能副作用、反例和揭示问题的测试。工具目录、执行身份、持久化或权限问题优先交给确定性系统；不靠增加提示词掩盖执行层缺陷。

历史会话通常无法还原当时初始工作区和每轮模型输入；当前 prompt 文件也不能证明当时使用过它。没有可核验的运行配置与输入时，只能交付改进假设，不能宣称 prompt 导致了差异。

实验阶段固定任务输入、初始环境、模型、工具和判据，只改变目标 prompt 段落；用同任务配对和重复运行记录波动，候选顺序盲化或交换，保留 held-out 任务。统计以任务为单位，多个调用和多个 reviewer 不增加独立样本量。报告任务级交付结果、未知/受阻占比、约束回归和验证质量；真实成本/耗时仅用直接遥测。小样本先看逐例结果，不把漂亮均值写成提升结论。

## 方法依据

本方法是对 Peri 当前观测能力的设计迁移：结果与 transcript 联合评价、混合确定性/模型/人工检查，参考 [Anthropic agent eval 指南](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents)；评审偏差与顺序敏感性参考 [Zheng 等人的原始研究](https://arxiv.org/abs/2306.05685)。同任务配对和相关样本处理参考 [Anthropic 的统计评估方法](https://www.anthropic.com/research/statistical-approach-to-model-evals)。这些资料没有验证 Peri 的标签或改进效果；本地试用负责暴露方法缺陷。
