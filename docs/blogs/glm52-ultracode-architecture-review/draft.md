# GLM 5.2 实测——独立修复 23 个架构弱点与 4478 行变更零回归

> **[Claude Code Best](https://github.com/claude-code-best/claude-code)**——Claude Code 的开源复刻，社区维护的多 Provider Agent 终端工具。<https://github.com/claude-code-best/claude-code>

6028d1d——一个 commit，79 个文件，4478 行新增，1529 行删除，23 个架构弱点全部修复。cargo build 全绿，cargo test 2445 passed，cargo clippy 零 warning。这个提交的主体代码由 GLM 5.2 独立完成，审查由 ultracode（Claude Code Best 的并行代码审查工作流，把大任务拆成多路 agent 并发执行）三路并行执行。

审修分离——ultracode 拆成三路 agent 并行审查 79 个文件的架构变更，查出 23 个弱点并给出改进方案，GLM 5.2 拿到方案后逐个修复。审的时候单 agent 只处理自己分配到的 crate，修的时候要跨 5 个文件保持一致性——两条链路都没有人工介入，全自动闭环。

## 单 Agent 上下文窗口的审查上限

这个 commit 跨了 6 个 crate——peri-agent、peri-langfuse、peri-acp、peri-middlewares、peri-tui。单个 agent 的注意力窗口装不下 79 个文件的完整上下文。串行审查审到第 40 个文件时上下文窗口已无法容纳前 20 个文件里的关键字段名、签名变更和交叉依赖关系——不用审完，中途就失效了。ultracode 的处理方式是把审查任务按 crate 拆成三路并行——

- peri-agent 路径专注核心 agent 层的变更，线程元数据强类型、工具调度、LLM 适配器
- peri-acp 路径专注 ACP 服务层的变更，execute_prompt 参数对象化、SessionManager 三合一、FrozenSessionData 重构
- peri-tui 路径专注客户端层的变更，ServiceRegistry 配置共享、BgTaskState 值对象、PanelState 宏调度

三条路径在同一轮次派发，并行执行，耗时约等于最慢的那一个。每条路径都拿到了自己那条链路的完整 diff，不需要切上下文。审查结果各自独立输出，最后汇总交叉验证。

实际效果超出了预期。peri-agent 路径验出了强类型枚举的 `FromStr` 大小写敏感性是否与 SQLite 的 `DEFAULT 'cascade'` 语句一致——这个细节需要同时解析 `types.rs` 和 `sqlite_store.rs` 两个文件的变更才能检出。peri-acp 路径验出了 56 参函数的参数对象化后三个调用点的字段映射是否完全对齐——TUI、stdio、print 三条执行路径的 `PromptExecutionContext` 构造有一处不一致就是运行时错误。peri-tui 路径验出了 98 处字段替换的语义一致性——`agent_done_pending_bg` 改成 `bg_task_state.agent_done_pending` 不仅是变量名搜索替换，需要确认每个访问点的读/写语义没被 `reset_for_new_round()` 引入的新逻辑覆盖。这些东西让一个 agent 串行审查，要么漏掉跨文件的交叉问题，要么审到后半程上下文超出有效区间，前期分析结果被挤出窗口。

## 23 个架构级弱点的实际分布

弱点分三级。P1 是正确性问题，不改会出运行时 bug。P2 是设计模式问题，不改会积累技术债，每次新增功能都要手动处理非法状态。P3 是清理项，不改不致命但会出现在每一次代码审查的 diff 里，拖慢所有人的阅读速度。以下选取三个代表不同级别的典型案例。

**P1-w5a——rewind 恢复文件编辑时 UTF-8 边界 panic。** 用户执行 `/rewind` 回退 Edit 操作，revert_files 函数用 `content.find(new_string)` 找到字节索引，再用 `&content[..idx]` 做字节切片——把 `new_string` 前面的内容切出来。中文、emoji、日文假名这类多字节字符可能在 `idx` 位置切成半个字符，panic。修改只用了一行——`content.replacen(new_string, old_string, 1)` 把第一次出现的 new_string 替换回 old_string。`replacen` 内部走字符级操作，不碰字节边界。这个修复本身很小，但堆栈位置在跨 5 个 crate 的调用链深层，不改的话用户 `/rewind` 到有中文的文件编辑就会崩。

**P2-w12——cancel_policy 和 agent_status 两个字段用裸 String 类型。** ThreadMeta 是 agent 会话的元数据结构体，存在 SQLite 里、在文件系统和线程间传播。`cancel_policy` 接受 cascade 和 independent 两个值，`agent_status` 接受 active、done、cancelled、error 四个值——但类型是 `String`，意味着任何地方都能往里面塞 "running"、"unknown"、"abc"，SQLite 写入路径不会拦，读取路径也不会报错，静静地 fallback 到默认值。持续存在的后果是非法状态落库，后续任何基于 `agent_status` 做路由判断的代码（比如 Goal 续跑检测）都会拿到一个不该存在的值。

GLM 5.2 把这两个字段改成了强类型枚举 `CancelPolicy` 和 `AgentStatus`——`FromStr` 解析非法值直接返回 `Err`，SQLite 绑定时用 `as_str()` 序列化，`ThreadMetaParseError` 用 thiserror 定义明确的错误变体。改的不止是类型定义，SQLite 读写路径、文件系统存储、反序列化兼容（`#[serde(default)]` 保证旧 JSON 缺字段不崩）、15 个新测试——全链路同步。

**P2-w3——execute_prompt 函数签名 56 个位置参数。** 三个调用点各自传 56 个实参，顺序一错就类型不匹配，函数体内部还需要从参数列表里阅读上下文推导每个参数的实际用途。GLM 5.2 把这 56 个参数收敛为 `PromptExecutionContext` 结构体，30 个命名字段分 4 组——session/transport 基础数据、per-turn 输入内容、middleware 资源、缓存对象。调用点从传 56 个位置实参变成构造一个结构体——字段名就是文档，编辑器自动补全不会漏传。函数体内部进一步拆出 `TurnConfig`、`InterceptRequest`、`SpawnPumpRequest`、`BuildAgentRequest`、`CollectRequest` 5 个子参数对象，把原来的巨型函数重构为 4 阶段编排器——intercept → spawn pump → build-and-execute → collect。行为逻辑完全不变，三个调用点——TUI、stdio、cli-print——的字段映射全部正确。

## 跨文件修复的链路一致性

GLM 5.2 在执行层面，三种类型的修复难度不同但共同点都是跨文件操作——改一个地方必须同步更新关联文件，一个地方漏掉就是编译失败或者运行时语义错误。

**ThreadMeta 强类型重构跨越 5 个文件。** 枚举定义在 `thread/types.rs`，SQLite 读写路径在 `sqlite_store.rs`——`meta_from_row` 里用 `FromStr` 解析 DB 字符串、`create_thread` 和 `update_thread` 里用 `as_str()` 绑参。文件系统存储路径在 `filesystem.rs`——`update_thread_status` 增加 `FromStr` 校验。导出在 `thread/mod.rs`——新增 `AgentStatus`、`CancelPolicy`、`ThreadMetaParseError` 三个公开类型。测试在 `sqlite_store_test.rs`——新增非法状态字符串被拒绝的测试。15 个类型单元测试在 `types_test.rs`——覆盖默认值、合法/非法 FromStr、serde 往返、缺字段反序列化兼容。GLM 5.2 在这 5 个文件上的改动是连带式的——改了类型定义后，所有引用 `meta.cancel_policy` 和 `meta.agent_status` 的地方自动从 `&String` 变成枚举访问，编译器会报类型不匹配推着你逐个修。真正的难点不在改，在知道要改哪些文件，以及在 `#[serde(default)]` 和 `Default` derive 之间选对兼容策略。

**Parameter Object 重构对齐三个调用点。** `PromptExecutionContext` 的定义在 `peri-acp/src/session/executor.rs`，三个调用点分别在 `acp_server/prompt.rs`（TUI 路径）、`acp_stdio/session/prompt_exec.rs`（stdio 路径）、`cli_print.rs`（print 路径）。各自有 22-25 个字段，大部分字段名相同但个别字段在不同路径上的语义有细微差别——TUI 路径有 `bg_results` 和 `session_manager`，stdio 路径有 `session_manager` 但 `bg_results` 是空向量，print 路径两者都没有。如果 GLM 5.2 只是机械地把 56 个位置参数复制粘贴成结构体字段，三条路径的差异很容易被抹平。但实际代码里三条路径各自定义了完整的 `PromptExecutionContext`，各字段根据路径特点独立赋值，没有出现跨路径的字段混用。

**933 行 Rewind 测试覆盖从 ASCII 到 emoji 的完整边界。** 不只是测 happy path。Write 操作回退——文件存在时 `remove_file` 删除，文件不存在时静默跳过不产生警告。Edit 操作回退——ASCII 场景正常替换，CJK 多字节场景确保 `replacen` 不触发字节边界 panic，emoji（4 字节）+ 中文（3 字节）+ ASCII 混合场景验证混合字节宽度下替换正确。new_string 在文件中未找到——告警不崩溃。文件缺失——告警不崩溃。逆序双编辑回退——先 Edit 后再次 Edit，两次变更按倒序恢复。工具配对验证——空历史、正常配对、孤立 ToolUse、孤立 ToolResult、Anthropic 格式、混合格式全部不 panic。execute——无效参数、未找到目标消息、尾部截断、中间截断、头部截断为空。两个已知行为如实记录——`ai_from_blocks` 构造的消息在 `tool_calls` 和 `content_blocks` 双路径被计数两次（测试直接断言 `len() == 2` 并注明原因）。这份测试的密度和质量不输人手写的——覆盖了之前完全没有测试覆盖的 0→933 行。

## 20 条硬约束的逐条遵守

项目指令文件 CLAUDE.md（Claude Code 生态中定义 agent 行为约束的 Markdown 配置）里有 20+ 条硬约束——每条都对应一个写错就会导致运行时 bug 的场景，GLM 5.2 在这次提交里对每条都做了正确的处理。

deferred error 模式——多工具并发执行时，P3/P4 错误路径不能提前 return，否则后续 tool_result 缺失导致孤立 ToolUse。GLM 5.2 在 `tool_dispatch.rs` 的修复里保留了完整的 deferred_error 收集 + 循环结束后统一判断的模式，没有引入提前 return。

cleanup_prepended 循环外执行——before_agent 注入的 system 消息必须在循环外无条件 cleanup，不能用 `?` 传播跳过。GLM 5.2 在 `executor.rs` 的重构里保留了 try_break 宏（将错误捕获到变量而非通过 `?` 传播，确保循环后 cleanup 必定执行），无论 LLM 调用成功还是失败，cleanup 都会执行。

Prompt Cache 前缀稳定性——Prompt Cache 是 LLM 服务端缓存不变前缀以节省重复计算的机制，system prompt 静态部分和动态部分用 `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 分隔，非 System 消息用 `add_message` 尾部追加不使用 `prepend_message`。GLM 5.2 在 `frozen.rs` 和 `prompt/mod.rs` 的重构里保持了边界标记位置不变和消息追加顺序不变。

A tool_result must always exist for a tool_use——SubAgent 工具调用返回后，如果 state 中有 Ai 消息的 tool_calls 但没有对应的 tool_result，后续 LLM 调用会收到 400 错误。GLM 5.2 在 `compact/command.rs` 新增的 5 个契约测试里直接把这个约束固化为测试断言——compact 输出中不得出现孤立的 ToolUse 和 Tool 消息，所有 Ai 消息的 tool_calls 必须为空。

CompactMiddleware once-per-prompt 守卫——CompactMiddleware 是上下文压缩中间件，同一轮 prompt 内只能触发一次，防止压缩循环。GLM 5.2 在重构过程中保留了这个守卫逻辑。

Agent 构建统一入口——禁止在 TUI 层直接构建 ReActAgent（Agent 的思考-行动主循环），必须走 `execute_prompt`。GLM 5.2 不仅遵守了这条规则，还在 `executor.rs` 里新增了 `execute_prediction` facade，把之前 TUI 层内联的 Prediction agent 构建迁移到 ACP 层——主动消除违规而非避免新增违规。

生成代码最容易出的两个问题——一是修 A 的时候漏了 B（改了类型定义没改序列化路径，改了函数签名没改调用点），二是修 A 的同时意外修改了 B 的行为（LLM 输出中出现了对不相关逻辑的优化尝试，实际破坏了设计约束）。GLM 5.2 在这两条线上都没出问题。

## 审修闭环与零回归验证

整个流程分三阶段——ultracode 拆路审查，产出 23 个弱点清单，GLM 5.2 逐项修复。审和修是两个独立阶段，用不同的模型能力。审走宽度——三路 agent 并行扫描，每条路专注自己的 crate，不切上下文。修走深度——单个 agent 拿到修复方案后，跨 5 个文件保持一致性、遵守 20 条硬约束、写完代码补测试覆盖。

最终结果——cargo build --workspace 全绿，cargo test --workspace --lib 从 baseline 2375 涨到 2445，新增 70 个测试全部通过。cargo clippy --workspace --all-targets -- -D warnings 零 warning。79 个文件的变更，一次执行到位，没有再跑第二轮修复。

审查路径按项目模块数弹性拆解，每条路径保持独立上下文，不受其他路径的变更影响。修复阶段通过 diff 验证模型对硬约束的逐条遵守——deferred error 模式是否保留、cleanup_prepended 是否在循环外执行、Prompt Cache 前缀是否保持稳定，每条约束在 diff 里都有对应的代码行可以核对。回到开头那个 commit——79 个文件、4478 行变更、23 个弱点，一轮审修闭环，cargo build 全绿，cargo test 零回归。审修分离加并行审查的工作流不绑定特定项目，任何跨模块架构重构都可以复用。

项目地址：[github.com/claude-code-best/claude-code](https://github.com/claude-code-best/claude-code)
