---
name: learn-from-history
description: >
  从过去 N 天的对话历史中提炼经验教训——分析每次 agent 交互中的异常、失败、成功模式，
  提出 CLAUDE.md/AGENTS.md 更新建议、可凝聚为 skill 的新方向、以及现有 skill 的改进点。
  当用户说"总结历史对话"、"learn from history"、"回顾最近的对话"、"从历史中学习"、
  "分析最近的 agent 表现"、"对话历史分析"、"最近有什么可以改进的"、
  "提炼一下这几天的经验"、"历史经验总结"时立即触发。
  也适用于用户想知道"最近 agent 出了哪些问题"、"有什么可以优化的"、
  "哪些操作可以自动化"等经验提炼场景。
  只要用户想从对话历史中系统性地提取可行动的改进方向，就应使用此 skill。
---

# Learn From History

从过去 N 天的对话历史中系统性地提取经验教训。
使用 Python 提取脚本将 SQLite 消息转为精简纯文本（过滤 reasoning 块和无用工具调用细节），
再按天分配 **general-purpose agent**（必须支持写文件，不能用 explore），
最后汇总产出可执行的改进方案。

## 核心原则

1. **先提纯再分析**：提取脚本做 JSON→纯文本转换 + 过滤，agent 只读有意义的内容
2. **按天分治**：每天一个 agent，独立并行分析，最后汇总
3. **去重优先**：每个 TRAP/skill 建议必须先交叉比对现有 CLAUDE.md 和 skill 列表
4. **可行动**：输出不只是问题清单，更要是可立即执行的 Edit 建议
5. **聚焦异常**：正常的 thread 只需记录意图和模式，深挖只给有错误的 thread
6. **批量提取**：用 `extract_range.py` 一次提取整个时间段，不需要逐天手动跑
7. **默认按项目过滤**：所有操作默认限定在当前工作目录的项目中（`cwd` 过滤）。跨项目发现单独归类，不混入当前项目建议

---

## 工作流程

五阶段流水线：发现 → 提取 → 分析 → 汇总 → 输出与编辑。

### 阶段一：发现应分析的天数

**全部通过 Python 脚本**，禁止直接使用 SQL。

```bash
# 【重要】必须显式指定 --cwd，从 env 的 Working directory 获取
python3 .claude/skills/learn-from-history/scripts/extract_range.py --query-active-days --days 7 --cwd <工作目录>
# 跨项目模式
python3 .claude/skills/learn-from-history/scripts/extract_range.py --query-active-days --days 7 --all
```

脚本自动：
- 连接 SQLite 数据库
- 按当前 cwd 过滤项目（默认行为）
- 返回活跃日期列表（thread 数 + 消息数）

**跨项目模式**（如用户明确要求分析所有项目）：

```bash
python3 .claude/skills/learn-from-history/scripts/extract_range.py --query-active-days --days 7 --all
```

**边界处理**：
- 若无任何记录 → 报告"近期该项目无对话记录，可用 --all 查看所有项目"，结束
- 若脚本报数据库不存在 → 报告"未找到对话数据库"，结束
- 若返回 >7 天 → 合并消息量最小的两天为一个 agent（如 06-12+06-13 共 160KB+110KB）

### 阶段二：提取纯文本

用 `extract_range.py` **一次性**提取整个时间段的对话，脚本自动按天拆分输出文件。

```bash
# 【重要】必须显式指定 --cwd，从 env 的 Working directory 获取
python3 .claude/skills/learn-from-history/scripts/extract_range.py <开始日期> <结束日期> --cwd <工作目录>
# 跨项目模式
python3 .claude/skills/learn-from-history/scripts/extract_range.py <开始日期> <结束日期> --all
```

**默认按当前项目目录过滤**（agent 从 env 的 Working directory 获取并显式传入 `--cwd`）。如需不限项目：

```bash
python3 .claude/skills/learn-from-history/scripts/extract_range.py <开始日期> <结束日期> --all
```

提取脚本会自动：
- 解析每条消息的 JSON 结构
- 保留：用户消息全文、助手文本回复全文
- 精简：工具调用 → 一行摘要（名称 + 参数摘要 + 成功/失败）
- 跳过：reasoning/thinking 块（内部思考，体积大且对分析帮助小）
- 去重：连续 N 次相同工具调用合并为一行 `[连续 N 次]`
- 截断：超大工具输出（>2000 字符）截为前 500 + 后 200 字符
- 跳过 system 消息（通常是技能注入的，已知内容）
- 按 thread 拆分：每天一个目录，每个 thread 一个 `.txt` 文件（通常 <50KB，agent 一次 Read 即可）
- 生成 `_index.txt`：每个目录下的索引文件，列出所有 thread 文件名、消息数、错误数、标题；多项目时显示项目目录分布

**输出结构**：每天一个目录 `/tmp/learn-YYYY-MM-DD/`，内含 `_index.txt` + 若干 `{thread_id_short}.txt`。

### 阶段三：按天分析

对每个有记录的日期，派发一个 **general-purpose agent**。

**Agent 类型：`general-purpose`**（必须——explore 是只读模式，Write 工具不可用）

**并发控制 [TRAP]**：后台任务最多 3 个同时运行。分批启动：

```
批次 1: 启动 3 个 agent（run_in_background: true）
       → 等待系统通知全部完成（不要调用 AgentResult 轮询！）
批次 2: 启动下 3 个 agent
       → 等待系统通知
批次 N: 启动剩余 agent
```

**⚠️ 禁止调用 AgentResult 轮询**。系统会自动在后台任务完成时推送通知。调用 AgentResult 只会返回 "No results yet" 浪费 token，且结果会重复推送。

**Agent 数控制**：
- 每天一个 agent，上限 7 个
- 若天数 >7，合并消息量最小的两天（agent prompt 中说明"包含两天数据，请分别分析并在报告中区分"）

**每个 agent 的任务**：

```
你是 general-purpose agent。分析 /tmp/learn-YYYY-MM-DD/ 目录下的对话历史。

工作流程：
1. 先读 _index.txt 了解当天所有 thread 的概况
2. 对每个 .txt 文件，Read 其完整内容（每个文件 <50KB，一次读完）
3. 分析所有 thread 后，Write 到 /tmp/learn-summary-YYYY-MM-DD.md

分析内容：
1. 当天概览（thread 数、总消息数、主要工作目录）
2. 每个 thread 的用户意图（1-2 句话）
3. 异常事件清单（列出所有失败的/出错的事件，带 thread 引用）
4. 成功模式（agent 有效解决问题的策略）
5. 改进线索（如果 CLAUDE.md 里有某条规则，能否避免或加速某个问题？）

输出格式：见 references/analysis-template.md
```

**目录中有几个 thread 就要分析几个**——不要遗漏。每个 `.txt` 文件都是一个独立的 thread。

### 阶段四：汇总

等待所有 agent 完成（系统通知）。**不要轮询 AgentResult**——等系统推送。

读取所有 `/tmp/learn-summary-*.md` 文件，汇总生成三个维度的发现。

**先判断是否多项目**：检查各天汇报中的 `主要目录`。如果单一 cwd 过滤已生效（绝大多数情况），所有 thread 属于同一项目。如果有 `--all` 模式或多项目残留，按以下分类处理：

**维度 1：TRAP 候选**（写入 CLAUDE.md 的经验教训）

- 同一错误在多天出现 → 高优 TRAP
- agent 反复犯同样错误 → TRAP + 工具改进
- 缺失关键指引导致 agent 走弯路 → TRAP

**分类规则**：
- **项目通用 TRAP**：跨项目反复出现的模式（如 Edit old_string not found、用户纠正方向）→ 建议写入当前项目的 CLAUDE.md
- **项目特定 TRAP**：仅在一个项目出现的模式（如 Slurm 脚本规范属于 RCS 项目、AGM 路径分隔符属于 perihelion）→ 标注项目归属，仅当目标项目与当前匹配时才建议写入
- **外部项目发现**：来自非当前项目的 thread → 单独归入"外部项目发现"段落，不混入当前项目的改进建议

**维度 2：Skill 候选**（可凝聚为新 skill 的重复任务）

- 同一任务在多天出现
- 任务多步但步骤固定
- 当前消耗大量 token（探索、试错）

**维度 3：现有 Skill 改进点**

- agent 应该在触发某 skill 时未触发？
- 某 skill 指令导致不必要的步骤？
- 某 skill 缺少错误处理指引？

**去重检查**：
- 读取 CLAUDE.md + peri-tui/CLAUDE.md + peri-middlewares/CLAUDE.md
- 列出已有 skill（`Glob(".claude/skills/*/SKILL.md")`）
- 每条发现标注"新增"或"已覆盖"

**输出**：`/tmp/learn-from-history-findings.md`

### 阶段五：输出与编辑

#### 5.1 终端摘要

```
📊 历史分析完成 → spec/reviews/history-learn-YYYY-MM-DD.md

扫描范围: YYYY-MM-DD ~ YYYY-MM-DD，共 N 个 thread，覆盖 M 天
项目过滤: /path/to/current/project（或 "--all 全项目"）

TRAP 候选: X 个（高优 Y 个）
Skill 候选: P 个（高优 Q 个）
Skill 改进: R 个
外部项目发现: S 个（不纳入当前项目建议）

Top 发现:
- [TRAP] ...
- [Skill] ...
- [改进] ...

是否应用以上建议？[y: 全部应用 / s: 逐条确认 / n: 仅看报告 / l: 仅高优]
```

#### 5.2 Markdown 报告

写入 `spec/reviews/history-learn-YYYY-MM-DD.md`，包含：
1. 扫描概览（含项目过滤信息）
2. TRAP 候选详情（含 thread 引用、建议 CLAUDE.md diff；外部项目发现单独归类）
3. Skill 候选详情
4. Skill 改进详情
5. 成功模式（固化价值）
6. 统计摘要

#### 5.3 执行编辑

根据用户选择：
- **y**：逐条执行所有 Edit 操作（遵循 CLAUDE.md `[TRAP]` 格式）——仅写入当前项目
- **s**：逐条展示，等待确认后执行
- **n**：不执行，提示报告路径
- **l**：仅应用高优 TRAP

执行后不 commit，显示变更摘要。

---

## 故障模式与应对

| 场景 | 处理 |
|------|------|
| `threads.db` 不存在 | 报告并退出 |
| 过去 N 天无记录 | 报告"该项目无记录，可用 --all 查看所有项目"，结束 |
| 某天提取文件 >500KB | ~~不再可能~~（默认按 thread 拆分，每个文件 <50KB） |
| 某 agent 超时/失败 | 不影响其他 agent，报告中标注缺失的天；手动重试失败的天 |
| 天数 >7 | 合并消息量最小的两天为一个 agent，prompt 中说明 |
| 合并多天的 agent | prompt 开头说明："本文件包含 YYYY-MM-DD 和 YYYY-MM-DD 两天的数据，请在 Thread 编号中标注日期" |
| CLAUDE.md 已有相同 TRAP | 跳过，标注"已覆盖" |
| python3 不可用 | 尝试 `python`，都不行则报告退出 |
| agent 是只读的（explore） | **不要用 explore agent**。必须用 general-purpose，explore 写不了文件 |
| 后台 agent 结果重复推送 | 正常现象——系统会推送一次通知，AgentResult 调用会再推一次。只处理第一次即可 |
| 默认 cwd 过滤返回 0 条 | 提示"该项目近期无对话记录"，询问是否 `--all` 跨项目分析 |

## 资源文件

- `.claude/skills/learn-from-history/scripts/extract_range.py` — 时间段批量提取脚本（推荐使用），支持 `--query-active-days`（替代 SQL）、`--cwd` 过滤、`--all` 全项目
- `.claude/skills/learn-from-history/scripts/extract_daily.py` — 单天提取脚本（extract_range 的底层），同样支持 cwd 过滤
- `references/analysis-template.md` — agent 分析模板
