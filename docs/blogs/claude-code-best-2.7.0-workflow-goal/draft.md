# Claude Code Best 2.7.0——Dynamic Workflow 拆任务，Goal 续会话

> **[Claude Code Best](https://github.com/claude-code-best/claude-code-best)**——Claude Code 的开源复刻，社区维护的多 Provider Agent 终端工具。<https://github.com/claude-code-best/claude-code-best>

今天，Claude Code Best 源码团队干了两件事——使用最新的 GLM 5.2 用 24 小时把 ultracode 复刻成了完整的 Dynamic Workflow 引擎，moy16 参考 Codex 实现的 Goal 命令推上了主线。2.7.0 一口气发版，两个功能都在里面。

在以前，给 Claude Code 下一个大活，你会遇到两种天花板。第一种，任务太宽了——一个模块重构涉及十几处文件，一个代码审计要同时过 correctness、architecture、security 三个维度，一个 agent 盯不过来。第二种，任务太长了——几百轮对话才能跑完的事，断个网、token 用完、关了终端，前面的进度就丢了。2.7.0 的 Workflow 和 Goal，一个解宽度问题，一个解长度问题。

## Dynamic Workflow——四种原语搭出一个工作流引擎

Dynamic Workflow 是 Claude Code 官方的多 agent 编排功能。Claude Code Best 团队延续了经典的 agent 设计思路，把它做成了一个 skill——通过 `/ultracode` 触发，agent 读取 skill 里的编排手册，按 phase/agent/parallel/pipeline 四种原语生成工作流脚本。24 小时内，围绕这个 skill 搭出了完整的 workflow 引擎——独立包、独立面板、独立生命周期，不只是一个提示词技巧。

### 四个编排原语构成一份工作流脚本

引擎的核心是四种原语——phase、agent、parallel、pipeline。这四个原语构成了 workflow 的描述语言，用户通过它们把一个大任务拆成可执行的工作流脚本。

phase 是一个工作流阶段，内部可以放 agent 或 parallel 块。一个 workflow 可以有多个 phase，按顺序执行。每个 phase 有自己的名字，面板上左侧侧栏显示的就是 phase 列表，选中一个 phase 可以看到它下面所有 agent 的状态。

agent 是执行单元，指定 prompt、model tier、文件上下文和输出 schema。一个 agent 的行为就是拿到自己的提示词、拿到自己的文件，跑完一轮工具调用链，最后输出一个结构化结果。agent 之间不共享 state——每个 agent 的上下文是独立的，互不污染。

parallel 把多个 agent 放进同一组，并发执行。parallel 内的 agent 之间没有依赖关系，引擎默认最多同时跑 3 个，上限 16。如果用户想设非 3 的并发数，框架会通过 AskUserQuestion 弹一个确认——你确定要开这么多 agent 同时跑吗。

pipeline 也是多个 agent 的组合，但串行执行。前一个 agent 的输出会作为下一个 agent 的输入，适合先搜索、再筛选、最后汇总这类流水线任务。

这套原语的设计选择——用户编程式定义工作流，AI 辅助生成工作流脚本。编排权在用户手里，不是让 LLM 做不确定的决策。

### 每个 agent 跑在独立 worktree 里

Workflow 引擎为每个 agent 创建独立的 git worktree——agent 在自己的 worktree 里操作文件，互不踩脚。worktree 的 slug 用 sha256(runId:agentId) 派生，run 结束后自动清理。这个设计的代价是磁盘占用——每个 worktree 是一份完整的项目副本，但换来了文件系统层面的完全隔离。

### 结构化输出——从工具契约退回到 JSON 文本解析

workflow agent 需要返回结构化结果——一个 audit agent 要输出 JSON，里面是发现的 issues；一个 code review agent 要输出问题清单。最初的设计走的是 StructuredOutput 工具契约——agent 通过调用一个专门的 tool 来提交结构化输出。

但实际跑下来出了大问题。跑 5 个 review agent，4 个 dead。journal 显示 agent 的最后输出全是 StructuredOutput tool is not available as a deferred tool——那个工具根本没被注入到 workflow sub-agent 的工具池里。agent 尝试调用一个不存在的工具，反复尝试后最终没有产生任何结构化输出。

解决方案是彻底放弃工具契约，改用文本 JSON 解析。引擎重写了 extractStructuredOutput——用括号栈扫描 agent 的最终文本输出，找到 JSON 块，parse 出来。处理嵌套对象、字符串里的括号、转义引号，取第一个 parse 成功的 JSON 对象。不修语法（尾逗号、单引号），避免在错误位置做修改——比如 URL 里的 http:// 被正则误判为注释而跳过。

这个改动的设计判断是——工具契约在封闭系统里是安全的，但 workflow 的 agent 池不保证那个工具一定在。与其维护一套工具注入的隐性依赖，不如靠 agent 的文本输出能力。文本输出不需要额外注入任何东西。

### agent 失败后的自动重试与降级策略

一个 agent 失败退出，不能导致整个 workflow 中断。引擎的策略是——第一次 dead 或非 abort throw 时自动重试一次。如果重试成功，正常收结果。如果重试还是失败，降级为 dead，但 workflow 继续往后跑。

重试不适用于 abort 场景——abort 是用户主动 kill 的，不是异常，不重试。配置错（adapter 找不到）也不重试——重试解决不了配置问题，只会掩盖 bug。token 计费只扣一次——第一次 dead 不扣 output token，重试成功才扣。

dead agent 会带上 reason 和 detail——no-structured-output、runagent-threw、worktree-failed、unknown。detail 字段保留 agent 最后输出文本的前 200 个字符，方便在日志和面板里一眼看到 agent 最后输出了什么。

### 面板三区布局和精确中断

/workflows 面板把 workflow 的运行状态可视化。顶部是 run tabs，切换不同的 workflow run。左侧是 phase 侧栏，显示当前 run 的所有 phase 及其状态——running、done、terminal。右侧是 agent 列表，每个 agent 显示名称、状态、运行时间。

中断系统有两级——x 键杀当前选中的 agent，K 键杀整个 workflow。都有 Dialog 二次确认，防止误触。abort 信号通过 claudeCodeBackend 桥接到 runAgent 内部的 abortController——这个桥接是后来修的，最初 x 键按下后 abort 信号到不了内部的 fetch 调用，agent 继续跑。

workflow 终态会落盘到 state.json，进程重启后可以按 runId 取回结果。跨进程 resume 不在 scope 内，但同进程内跨重启已经够用。

## Goal——一个状态机让 agent 跨会话接着跑

Workflow 解的是任务太宽的问题，Goal 解的是任务太长的问题。一个 agent 单次会话跑了 80 轮，断网——之前几十轮的进度，Goal 用自动 pause 和 resume 兜住，带着上下文回到原位继续。

### 五种状态构成的有限状态机

Goal 的状态只有五种——active、paused、max_turns、complete、blocked。状态转移路径非常有限——active 可以转到 paused（断网、用户中断）、max_turns（150 轮封顶）、complete（验收通过）、blocked（连续 3 次受阻）。paused 只能回到 active。max_turns 只能回到 active（用户手动放行）。complete 和 blocked 是终态。

这组状态转移是对 agent 能否继续执行这个判断的建模。active 代表能继续，paused 代表现在不能继续但等下可以，max_turns 代表已经执行了太久需要人类确认，complete 和 blocked 代表不用再继续了。

关键的设计约束是——到达 blocked 的门槛是连续 3 次 continuation turn 碰到同一个障碍物。第一次碰到不算 blocked，agent 换个方式也许就绕过去了。连续三次同样的问题，才判定为真的 blocked。这个阈值是刻意的——太低会把暂时困难误判为死路，太高会让 agent 反复触发同一障碍物而不自知。

### continuation prompt——框架主动注入续跑指令

续跑不是 agent 自发触发的——agent 没有持久化上下文追踪机制。续跑的驱动力来自框架——agent 每轮跑完进入空闲状态时，框架检测到有 active goal，自动注入一条 continuation prompt。

这条 prompt 包含目标描述、已用时间、token 消耗、已跑轮数。然后是三条核心指令——不要缩小目标范围、完成前必须先过 Completion Audit、遇到障碍物先别急着标记 blocked。prompt 用 `<goal-steering>` XML 标签包裹，让 agent 能区分框架注入的引导和用户消息。

这个设计选择——框架主动投喂而非依赖 agent 主动性——是因为 agent 被设计成用户说一句才动一次。没有用户消息驱动，agent 不会自己凭空干活。Goal 其实是在模拟一个用户——每次 agent 干完，悄悄塞一条接着干的消息进去。

### Completion Audit——不证明完成就当没完成

agent 容易在输出中声称完成，但实际未经验证。Goal 的 Completion Audit 强制 agent 走一套严格的验证流程。

从目标推导出具体的需求清单。对每条需求，找权威证据——测试输出、文件内容、命令结果。测试和 manifest 算证据，但必须是确认覆盖到相关需求的才行。没有证据的、证据模糊的都算未完成。验收的逻辑是证明完成，而不是没找到剩余工作就默认完成。

这个设计的实际效果是——agent 在标记 complete 之前，会真正扫一遍相关文件，确认修改已经落地、测试已经通过。不是靠感觉，是靠证据。

### token 预算和 max_turns 双重限制

Goal 的 agent 如果没有任何限制，理论上能无限续跑下去——每次执行完，框架投喂 continuation prompt，agent 继续执行，循环往复。两层限制防止无限循环。

token budget 是用户设定的消耗上限。budget 耗尽时，框架注入一条 budget_limit prompt，告诉 agent 停止所有实质性工作，只做进度总结。agent 不能再调用工具、不能编辑文件，只能说明完成了什么、还剩什么。

max_turns 是连续续跑的轮数上限，默认 150 轮。到了上限，goal 转入 max_turns 状态，agent 停下来。用户需要手动放行——输入 `/goal continue`，轮数计数器归零，agent 继续。防止 agent 在一个无限的续跑循环里消耗资源，尤其是当目标本身就不合理的时候。

### 跨会话容错——断网 pause，重启 resume

容错策略覆盖了三种常见中断场景。断网时 goal 自动转入 paused 状态。用户再次连接到同一个 session 时，goal 检测到 paused 状态，自动 resume，注入 continuation prompt 继续干活。

terminal 异常终止——比如进程被 kill 了——goal 状态持久化到 session storage 里。下次启动同 session 时，从存储恢复 goal 状态，检测如果是 paused 则自动 resume。

resume 路径只接受 paused 状态——complete 和 blocked 是终态，不能 resume。max_turns 状态也不能自动 resume，必须走 `/goal continue` 手动放行。状态转移被明确编码，不留给 agent 自行判断的空间。

Goal 的实现遵循了一个原则——状态转移是框架的职责，agent 只是状态的执行者。agent 可以请求标记 complete 或 blocked（通过 GoalTool），但框架审计后才真正改变状态。决策权和执行权分离。

## Workflow 拆宽度，Goal 续长度

2.7.0 之前，Claude Code Best 是一个单 agent 单会话的工具。你能下的任务的复杂度上限，取决于一个 agent 能盯住多少文件和一次会话能持续多少轮。这两个天花板的后果是——复杂任务是可见不可接的，你知道 agent 搞不定，就不会下那个活。

Workflow 和 Goal 把两块天花板各自往上推了一级。Workflow 让一个任务可以横向拆成多个 agent，并行或串行。Goal 让一个 agent 可以纵向跨会话续跑，不怕中断。两个功能独立工作，各自解决一个维度的问题——同时用的时候，一个 workflow 里每个 agent 各自带着 goal 续跑，宽和长的边界都打开了。

Workflow 靠用户编程定义编排，Goal 靠框架状态机驱动续跑，决策权和控制权都在用户和框架手里。LLM 是执行单元，不是决策者——这个定位贯穿了两个功能的设计。

项目地址：[github.com/claude-code-best/claude-code-best](https://github.com/claude-code-best/claude-code-best)
