# 提示词方向与实验卡

状态：由真实案例形成的候选，尚未执行生产提示词 A/B。历史记录未关联每轮实际 prompt、模型配置和初始环境，因此下列入口只说明当前可以在哪里实验，不证明历史问题由该段提示词引起。

## P01：关键解释与验收范围保持一致

**观察。** Windows 测试修复案例的本机目标测试通过，但解释把目录名限制扩展到了 macOS；本轮 Darwin 临时目录实验提供了一个反例。MCP 工具使用案例成功转换了 CSV，却额外断言某个 skill 可由 SkillTool 加载，后续实际调用失败。两例的主体操作有成果，附带解释仍可能误导后续行动。案例及消息定位见 [03-findings.md](03-findings.md)。

**候选变化。** 在 `peri-acp/prompts/sections/03_doing_tasks.md` 已有“可验证目标”规则处试验一条补充：

> Before declaring a requested result complete, match it to evidence for that result and environment. Distinguish observed behavior from explanations you have not checked; keep an unverified platform or capability claim provisional.

这是候选文本，不是已部署规则。优先改一处，避免在多个段落重复叠加。当前段落 SHA-256 为 `bd1969c53c55488c35ac29cfb66f434731086c2cb7bebb38e5f36c86515c42e3`；执行实验时还需保存完整渲染输入的 hash，而非只保存这一段。

**受控任务。** 开发集覆盖本机检查与目标平台不一致、只有 MCP resource 的同名 skill、尚未完成提交、已经完成的纯研究交付。这里的合成方法试用只能校准评审；执行策略实验需另外提供固定工具响应或隔离工作树，由 agent 实际推进任务。另备两个未用于改词的同机制任务作为留出集。

**设计。** 同任务 baseline/candidate 配对，各运行 3 次，模型、工具版本、初始状态、预算一致；顺序交换，交付内容盲评。六个任务共 18 对只是探索预算，先发布逐任务结果，不据此宣称统计显著。候选 hash 由实际候选完整输入生成，不预填。

**主要判据。** 硬验收条件完成情况不下降；不再把本机检查写成目标环境验收，不把未发现的能力写成可用，不把待提交写成已提交。解释类任务不被迫运行无关工具。额外核验次数、真实成本和多余澄清作为副作用记录。

**反例与替代解释。** Rust 工具链升级有直接更新与 workspace 检查记录，说明全面检查在全局工具链变更中可以合理；不应把规则变成“永不作推断”或“每句话都调用工具”。工具目录时效或 skill 发现机制也可能导致能力差异，应与模型越过证据作解释分开处理。

**推进条件。** 逐例减少无据完成/能力断言，且没有交付退化和权限回归，才进入更大试用；若只是报告变得保守、实际任务未改善，继续修订或撤回。

## P02：验证命令匹配变更范围与验收条件

**观察。** minimal 示例配置任务运行根级 `bun test`，120 秒后转入后台，随后返回无关目录失败；用户纠正了测试范围解释。相对地，Rust 工具链升级的 workspace 检查与其影响范围一致，EXDEV 修复也有三个受影响安装器的目标测试。

**候选变化。** 仍在 `03_doing_tasks.md` 的现有测试发现规则处，只替换该条的验证选择部分：

> Select checks from the affected module's documented commands and the user's acceptance criteria. Broaden them when the change crosses that boundary or the required checks demand it; explain what each result actually covers.

当前规则已经要求读 README/发现测试框架。实验需要判断补充“范围”是否有增益，而不是把旧规则换个说法就宣称改进。

**受控任务。** 固定一个多项目 fixture：示例 JSON/README 变更、影响所有项目的工具链变更、三个安装器共同修复。各自有可执行的目标检查；根级测试另外包含一个确定的无关失败。用两个新模块布局作留出反例，防止记住目录名。

**设计与判据。** baseline/candidate 同任务配对，各 3 次，保存真正运行的命令、cwd、退出状态、改动路径与最终报告。选择了必要检查、能指出未覆盖条件且没有错误归因才算通过；单纯命令更少不算提升。广泛变更漏跑规定门禁直接判回归。

**替代解释与副作用。** 项目文档缺少 canonical 命令、旧测试迁移未更新，也能造成选错范围；优先修路由。规则过窄可能漏掉跨模块回归，不能推广成“只跑局部测试”。

## O01：任务与执行证据的观测补充

状态：数据/运行时能力候选，不是提示词补丁。无助手记录的 UI 请求、289 个空根会话、长记录缺口、子任务只在父会话返回简短结果，都限制了有效性判断。

下一步先核查现有存储是否已经持有可验证的 task/request、提交/取消/终态、subagent 请求与结果关联，以及冻结 prompt/model 身份；复用事实持有者导出这些字段。缺少时再设计契约，不能从“会话结束”“用户说继续”或调用次数推导完成状态。正文可见 channel 也需要协议/渲染证据，不从归一化文本猜测。

验收应使用可控 fixture 区分：尚未开始、用户取消、执行失败、完成但未保存反馈、子任务失败后恢复。观测完善后重评相同案例，测量 unknown 是否因新增事实而减少；减少 unknown 本身不是 Agent 质量提升。

## 保留的成功行为

子 agent 首次模型请求失败后，父任务恢复执行并收到 hello；补读关联子会话还看到了 `sleep 1` 和成功结果。这一例支持保留基于明确失败的恢复与结果核查，不支持把每次工具错误都计作坏任务，也不支持硬编码某个模型作为通用回退。

当前 subagent 指令入口是 `peri-acp/prompts/sections/11_subagent.md`，本轮 SHA-256 为 `eae7554f148db079bf25ffb7986dc37c58958543232e63f2b3358ce3f63c4361`。本轮没有修改该文件。
