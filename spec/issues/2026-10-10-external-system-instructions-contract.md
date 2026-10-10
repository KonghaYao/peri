# 外部系统指令契约（待审）

> 状态：待用户审阅的权威契约候选；尚未批准、尚未实现。审阅通过后并入
> `docs/design/system-prompt.md` 与适用的 architecture standard，再按本契约修复实现。
> 本文件作为审阅期间的单一设计文本，不把当前代码行为写成目标语义。

## 目的与边界

外部系统通过 ACP `session/new` 的 `_meta["peri.instructions"]` 扩展 Peri 的内部
System Prompt。它是**会话级外部指令**：由创建会话的客户端提供，Peri 负责准入、
冻结、持久化和在每次相关模型请求中组合。它不是用户消息、项目指令、MetaHarness
段落覆盖、middleware 的逐请求贡献，也不是工具授权配置。

客户端可以借此给 Agent 增加任务环境或行为约束，但文本不能新增工具、改变审批
策略、解除 middleware 关闭位或改变服务端能力事实。服务端仍按当前 session-local
能力目录和策略执行。外部系统指令本身是受信的客户端扩展输入；该信任只关乎将
文本送入 System Prompt，不授予其修改 Peri 运行策略的权力。

## 准入与冻结

1. 仅 `session/new` 接受该字段；字段缺席表示无扩展。字段必须是字符串，按
   UTF-8 字节计不超过现行的 64 KiB。明确给出的空字符串等同于无扩展；非法值
   整个请求失败，不创建可用会话，也不悄悄丢弃指令。
2. 指令正文按**字面文本**处理，不解析 `{{...}}` 模板、项目指令的 `@import`、
   MetaHarness section、XML 标签或其他内容约定；空白和换行不规范化。Peri 可以
   在其外部加固定的提示词分隔结构，但请求中的正文须逐字出现且仅出现一次。
3. `peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY` 的值是内部 transport
   控制字。新会话输入含该完整控制字时，在 ACP 准入处以明确的 invalid-params
   错误拒绝。不得把它交给 provider adapter 的“重复控制字降级”路径处理；
   该路径用于防御内部异常，不是外部输入的准入规则。
4. 接受后的正文作为**独立、带来源的冻结字段**存入版本化 frozen snapshot，
   与内部渲染产物分开。会话中的 turn、冷 `load`/`resume`、普通 fork 与子 Agent
   只读同一份冻结值，不从客户端、当前配置或磁盘重新获取。快照格式升为 V2，
   必须显式写入可为 `null` 的外部字段：`null` 表示无扩展，字段缺失属于损坏数据。
   解码只按版本区分 V1 与 V2，不根据字段是否存在猜测来源；损坏或未知版本仍按
   ARC-FROZEN-001 fail closed。

历史 V1 快照没有来源字段，却可能已把外部指令嵌在 `system_prompt` 中，不能把
“无字段”解释为“无外部指令”，也不能通过解析 `<agent_instructions>` 标签恢复
可信正文。对这类旧快照，普通主 Agent 续聊保留原冻结 prompt；若旧 prompt 含
`<agent_instructions>` 开始标记，需重渲染的 override、子 Agent 或 workflow
路径必须明确拒绝并要求新建会话，不得静默丢弃指令或复制整份主提示词冒充子
能力投影。该标记检测允许保守拒绝误命中；若旧 prompt 的该开始标记之后还含
保留缓存控制字，则主 Agent 请求也须在 provider 前明确拒绝，不能继续由 adapter
静默剥离正文。新格式的结构化字段不适用这些旧数据降级规则。

## 提示词组合

Peri 保留内部提示词的段落与能力投影，再把会话级外部指令作为独立的冻结扩展段
**追加在内部基础提示词之后、逐请求 middleware contribution 之前**。外部段不
属于 MetaHarness section，不受 Agent override 的段落重渲染覆盖。组合必须有
单一入口，创建期渲染与 override 后的重渲染使用相同的冻结字段和顺序，不能从
已经拼接的字符串反解析正文。

| 请求路径 | 必须看到的外部指令 |
| --- | --- |
| 主 Agent 首次请求及后续 turn | 同一冻结正文，恰一次 |
| 带 Agent override 的主 Agent 请求 | override 后的内部段落之后，恰一次 |
| 冷 `load` / `resume` 后的新请求 | 来自持久快照，恰一次 |
| 普通 fork 后的请求 | 继承 source snapshot，恰一次 |
| 由该会话派生的子 Agent / workflow Agent 请求 | 在各自能力投影后的内部提示词之后，恰一次 |

外部指令不得作为普通 `System` transcript 消息保存或继承；Compact、消息复制
和历史投影不能改变其内容或次数。子 Agent 的专属身份与会话级外部指令要分别
保留，子链移除某能力时仍以运行时能力事实为准。外部段的存在不意味着把主
Agent 已关闭的能力说明复制进子提示词。

当前子 Agent 会把整份内部身份提示词持久化为自己的 `System` 历史。实施时必须
保持这条历史**只含内部身份**，让外部字段由会话 frozen owner state 传入模型
请求组合；请求投影吸收身份历史副本时以内部身份匹配，不能拿组合后的整段文本
匹配。二级子 Agent 继承的对话历史也不能因此得到外部字段副本。

## 缓存与 wire 边界

内部 `Cached` 静态段仍位于唯一的 transport 分界之前。外部指令是会话冻结但
由客户端变化的文本，固定放在分界之后的非显式缓存区，位于请求时 contribution
之前。即使内部模板为空，也必须在外部段**之前**生成唯一结构分界；不能依赖
后续动态 contribution 才补分界。这样外部指令不会改写或扩展 Peri 的静态缓存
前缀，也不会因 Agent override 而移动。允许的输入正文中没有保留控制字；
最终 provider wire 不含控制字。比较 provider JSON 解码后的 system text 时，
外部正文的 UTF-8 字节须作为连续子串逐字出现且恰一次，顺序不变；固定的外层
分隔结构要保护正文边缘空白不被 adapter 的 block `trim()` 吃掉。支持显式
breakpoint 的 adapter 仍只接收一个结构性分界；非显式 adapter 仍消费该分界
并保持正文。

## 实施与验收范围

- ACP 准入须覆盖非字符串、超长、空值及保留控制字；拒绝时不得留下新 thread。
- frozen owner state 须能往返编码 / 解码外部字段；旧快照缺字段、冷加载、fork
  的行为明确，并核对新字段与主会话 `FrozenContext` 的单一事实源。
- 最终 `ModelRequest` 与两类 provider wire 的行为测试须覆盖：普通主请求、
  Agent override、冷恢复、fork、子 Agent、含 `{{date}}` 的字面正文，以及
  唯一缓存分界和正文恰一次。再覆盖内部模板为空时有/无逐请求贡献、正文首尾
  空白与 CRLF；测试不能只检查 frozen 字段或局部渲染函数。override 可通过
  装配注入测试覆盖，不能宣称当前普通 ACP 请求已能触发该分支。
- 子 Agent 的 transcript（含冷恢复及二级子 Agent）不得含外部正文，但最终
  wire 须恰一次；workflow 须分别覆盖默认、指定 `agentType` 及 fallback。
- V1 旧快照须覆盖：无可识别外部标记时的普通续聊；有标记时普通续聊保留字节、
  重渲染路径明确失败；标记之后含缓存控制字时在 provider 前明确失败。
- 检查 print / 嵌入入口：没有 ACP 扩展值时行为保持原样，不构造虚假的外部
  指令；后续若开放其他来源，必须复用同一准入与冻结语义。
- 实施时按 DOC-UPDATE-001 更新 `docs/design/system-prompt.md`、相关标准、
  `docs/code-index/` 与模块指引；本 active issue 在验收后转入历史。

## 当前实现与目标的差距

当前代码将正文直接追加到 `FrozenContext.system_prompt`。Agent override 分支
重新渲染并替换这个字符串，代码上会丢失外部指令；当前生产主请求入口固定把
`agent_overrides` 设为 `None`，尚未证实普通 ACP 客户端能触发此分支。子 Agent
与 workflow 的独立重渲染也没有结构化字段可用。当前准入只验证类型和长度，
保留控制字可进入 prompt，触发 adapter 多分界降级并从正文中消失。这些是实施
本契约要修复的行为，不能以现状作为验收。
