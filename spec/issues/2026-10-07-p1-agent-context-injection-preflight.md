# P1：Agent 上下文注入上线前静态排查问题清单

状态：待处理（排查记录，未修复）。日期：2026-10-07。

范围：对 12 条 Agent 上下文注入路径的只读静态排查——段落装配、Frozen 冻结输入、动态后缀 `prompt_contribution`、项目指引（AgentsMd）、Skills、Plugin、消息级增强（@mention/@image/预载/完成提醒/recall）、System Reminder、SubAgent 继承与 Agent 定义、MCP 工具与资源、Compact 摘要回填、Hook 输入替换。无阻断级缺陷，下列问题按严重等级排列。

前提：排查期间工作树处于在飞状态（多项未提交改动），部分结论未获运行验证；修复前以最新代码复核，并对标注「待实测」的条目先补验证。

方案备注（2026-10-07）：以下逐项备注是基于当前代码的静态复核与建议，尚未实施或获得运行验收；原问题记录保留，复核有差异时在条目下明确说明。验收项均为后续实施要求，不代表已通过。跨项共同修复只保留一个实现权威，需产品确认的取舍不在本次文档修改中视为已批准。

## 高

### H1 子 Agent 收不到动态贡献（项目指引 / 技能摘要 / 延迟工具）

- 现象：`prompt_contribution` provider 只装在主链与 workflow；子 Agent LLM（`peri-acp/src/host/prompt/models.rs` 的 subagent factory）只有 `with_session_id`，无 provider。子 Agent 的 system 面来自 `system_builder` 重渲染或 fork frozen 回退，均不含贡献；子链 `before_agent` 照常执行、贡献写入后无人读取。
- 影响：子 Agent 不知道 CLAUDE.md 约定、看不到技能目录与 deferred 工具；v2 迁移回归（迁移前 agents_md 经 `prepend_message` 子链可见）；违反 ARC-FROZEN-001「SubAgent 复用冻结数据」。
- 待实测确认（建议补一条捕获子 Agent 请求体的断言）后修复。

**方案备注**
- **推荐**：将子 Agent 的 `AgentModelBridge` 装配移到已拿到子链的 Agent session factory；ACP 的 model factory 只负责模型解析与创建。与主链一致，通过请求时 provider 读取 `chain.collect_prompt_contributions()`，不在 `before_agent` 之前拍一次贡献快照。定义型、fork、前台/后台、live resume 与冷恢复共用该入口。
- **边界**：继承父会话冻结数据，但贡献只能由子链实际能力生成，不能复制父动态后缀或重新读工作区。身份 system 只装配一次，不再同时从 transcript System 消息与 bridge 注入；旧持久消息如何归一化与 M3 一起处理，不泛化删除所有历史 System 消息。fork 继承身份与禁用 Agent/Cron/Workflow 等能力的投影规则由 H2 明确。
- **复核/依赖**：`host/prompt/models.rs` 缺 provider、`subagent/tool/mod.rs` 子链会产生贡献的静态证据成立；`subagent_setup.rs` 生产还把 `frozen_system_prompt` 置为 `None`，fork 可能实际走重渲染，需一并核对。与 M3、M4 共用装配修复，H2 决定段落裁剪。
- **验收**：复用 `host/executor_flow_test.rs` 的请求捕获模型，但必须装生产子链而非空链替身；对定义型/fork、前后台、resume/cold 捕获最终 provider 请求，断言项目指引、技能摘要及允许的 deferred 目录各出现一次，关闭/未继承能力不出现。先保留失败证据，再实施。

### H2 子 Agent / workflow 的 prompt 声明它不具备的能力

- 现象：子链不装配 Permission/HITL/SubAgent 持有者，但 prompt 由同一 `build_collected_sections` 生成 → 子 Agent 收到 10_hitl、12_ask_user、11_subagent（含 "Agent 工具"声明，与同段"子代理不继承 Agent 工具"自相矛盾）。workflow 的 10_hitl 排除逻辑（`host/workflow_agent.rs`）因生产恒传主冻结 prompt、fallback 不执行而失效。
- 影响：模型可能等待永不到来的审批、调用不存在的工具；违反「middleware 缺席 ⇒ 段落消失」约定（`docs/design/system-prompt.md`）。

**方案备注**
- **推荐**：由主链、子链、workflow 的实际装配结果及同一 session-local policy 派生段落集合，统一供 prompt 渲染消费；不维护第二份按执行类型硬编码的排除表。除 holder 是否存在，还要检查 holder 的有效模式：workflow 当前装有 `PermissionMiddleware::disabled()`，存在不等于需要审批说明。
- **边界/取舍**：推荐继承冻结内容与身份，而按子执行环境收缩能力声明；缺 Agent/HITL/AskUser 能力就不声明可调用，不靠正文补一句“不可用”抵消前文。对 fork 的“继承父冻结 system”应明确为冻结输入的确定性投影，而非重新探测或复制错误能力声明；若现契约要求逐字继承，实施前先批准这项语义澄清。不要通过拆解渲染后字符串删除段落，需保留段落结构或可确定重建的数据。
- **验收**：覆盖主链、定义型子链、fork、workflow 有/无 agentType 与各关闭位；对拍实际工具、权限模式与最终 system 声明。特别断言生产传入冻结 prompt 时仍经过正确投影，不能只测不会运行的 fallback。与 M11 共用段落来源及对拍测试。

### H3 运行环境未冻结，重渲染路径每次重新探测

- 现象：`platform` / `os_version` / `is_git_repo` 无冻结字段；`PromptEnv::with_frozen_date` 在调用时重新探测（`peri-acp/src/prompt/mod.rs`），主重渲染与 workflow 均走此路径，与同文件注释「消费同一份、不在调用时各自 detect」矛盾。
- 影响：会话中途 `.git` 出现/消失即产生 prompt 漂移（ARC-FROZEN-001 前缀稳定）；macOS 上逐 turn 起 `sw_vers` 子进程。

**方案备注**
- **复核修正**：`07_runtime` 属于 Uncached 区，因此当前证据支持“重渲染时冻结环境描述漂移”，不能直接归因为 Cached 前缀失效；`sw_vers` 是每次走重渲染时调用，不是所有 turn 必然调用。
- **推荐**：将 `platform/os_version/is_git_repo` 作为环境快照随 `FrozenContext` 和版本化 snapshot 持久化。初次内容准入时从选定执行/工具环境取得一次；主重渲染、子 Agent 与 workflow 仅消费快照，去掉生产路径 `with_frozen_date → detect`。远端工具环境不得被本地计算宿主探测值冒充。
- **旧数据策略**：推荐旧 snapshot 保留已有 system 字节，缺少的结构化环境值标记 unavailable；需要派生新 prompt 时显式提示限制，不伪造旧环境，也不在每次恢复时重探。若选择一次性迁移补齐，须单独批准迁移语义，不覆盖 write-once 原快照。
- **验收**：冻结后改变 Git 状态/探测器返回值，主、子与 workflow 重渲染仍稳定；用探测计数器验证每个新快照只采集一次，冷恢复不调用探测；覆盖旧版本、未知版本与远端环境身份。

### H4 Hook 安全三项（`peri-middlewares/src/hooks/`）

- `permissionDecision: "deny"` 解析后落入 `_ => Ok(tool_call.clone())`（`action_resolver.rs` / `dispatcher.rs`）：按 Claude Code 兼容字段写「拒绝」的 hook 实际 fail-open。
- `$ARGUMENTS` 明文替换进 command 字符串后经 `bash -c` 执行（`variables.rs` + `executor.rs`），HookInput（含用户 prompt 与 tool 输出）可驱动命令注入。
- cwd 级 `.claude/settings.json` hooks 默认加载并在首个 prompt 执行（`loader.rs` + `assembly.rs`），无工作区信任门、无审批。

**方案备注**
- **拒绝决策**：在 parser/dispatcher 的统一决策归并处处理 deny，优先级高于 `updatedInput`；当前二者同时返回时 parser 还可能先取 updatedInput 而丢掉 deny。deny 必须让执行计数为零；ask 必须交既有审批端口，端口不可用则明确拒绝，不能放行；allow 不得越过宿主权限上限。保留判定与反馈的不同字段，避免单个 action 枚举吞掉组合输出。
- **参数隔离**：推荐废除 HookInput 向 shell command 的 `$ARGUMENTS` 文本替换，使用现有 stdin JSON 作为数据通道；需要额外参数时使用结构化 argv。已配置 shell 脚本属于受信代码，但用户 prompt/tool output 永远只是数据。不把“统一加引号”当作适配任意 shell 上下文的安全修复。旧写法须给可定位的迁移错误与说明，不能静默改变含义。
- **信任门**：由宿主在 hooks 装配/执行准入处消费显式 workspace trust；信任记录及路径/来源身份由配置 MCP 数据面持有，不在核心新增读盘。未信任项目/local 来源不执行，非交互客户端须显式授权，否则保持关闭并告知；用户全局配置不自动授权项目或项目声明的插件 hook。信任需绑定工作区和执行来源，支持撤销与来源变化重新确认；权限审批与工作区信任是两道不同边界。
- **验收/取舍**：覆盖 deny+updatedInput、ask 无审批端口、非法决策、含引号/换行的良性 JSON 原样传输、未信任/受信任/撤销/非交互入口，断言不该执行时进程与工具调用数均为零。信任交互、持久授权范围与旧 hook 命令迁移需批准；测试和日志不记录 prompt、tool output 或凭据正文。

### H5 app-only（`model:false`）工具经 deferred 面仍可见且可执行

- 现象：direct 面正确排除，但 deferred 索引只按 `!is_direct()` 选取，`ExecuteExtraTool` 亦无可见性检查（`tool_search/middleware.rs`、`tool_search/execute_tool.rs`）。
- 影响：server 声明的「仅 app 可用」能力可被模型检索并调用；现有测试名声称覆盖但只断言了 direct 面。

**方案备注**
- **推荐**：把 `visible_to_model()` 作为所有模型入口的否决条件，`is_direct()` 仅区分 direct/deferred。修正 `tool_search/middleware.rs` 的索引、元工具描述与检索投影；`ExecuteExtraTool` 在目标解析后、实际调用前再检验当前可见性，覆盖猜名、别名和旧索引绕过。
- **边界**：过滤落在模型入口，不把 app-only 工具从 App 正常调用所需的底层注册表删掉；子 Agent/workflow 同样从各自已过滤工具视图派生。现有 system_mcp 提升已拒绝 app-only，无需另造规则。
- **验收**：同一 app-only 工具在 direct、deferred 列表、精确检索、别名检索均不可见；按名强行调用失败且 MCP wire 调用数为零；App 合法调用和 model-visible deferred 正向用例仍成功。

### H6 Compact `summary_max_tokens` 无条件覆盖 provider 输出上限

- 现象：摘要请求 `with_max_tokens(config.summary_max_tokens)`（默认 16000），provider 侧无 clamp（`compact_v2/full.rs`）。
- 影响：profile 输出上限低于 16000 的模型上 Full 每次失败、达到阈值后本轮终止；与 ARC-COMPACT-001「保持单次输出上限不变」不符。

**方案备注**
- **复核**：触发条件是摘要请求覆盖值超过实际模型允许的输出预算；`profile.max_tokens` 是配置值，不等于已经探测过的物理能力。低值配置下是否被 provider 拒绝仍需 wire/假 provider 验证，不能仅据配置比较断言远端必然失败。
- **推荐**：先按现契约保持所选模型已解析的单次输出上限及 thinking 配置，不让 `summary_max_tokens` 无条件覆写该上限；通过 Model/bridge 的只读预算接口向摘要器提供有效值，摘要长度目标与请求预算使用同一来源，续写沿用首轮预算。若 `summary_max_tokens` 保留为摘要长度目标，其目标须受有效输出预算约束，并明确字段语义。
- **取舍**：若产品希望它成为独立硬上限，再批准 `min(summary_max_tokens, resolved_output_limit)` 语义及契约更新；不能把这一变化伪装成现有“保持上限”规则。零值/无法解析上限要明确报配置错误或沿用模型默认，不任意填 16000；通用 adapter 是否允许 request override 是更大契约，不在此顺手全局 clamp。
- **验收**：捕获低/高输出预算、显式摘要配置、首轮与续写请求，断言 max_tokens 合法且续写不变、thinking 不变、提示中的目标与有效预算一致；provider 拒绝时保留既有 Full 错误/终止语义，不无限重试。

### H7 消息级注入覆盖不齐

- @mention 与技能预载只挂 `before_agent`，同 loop 中途批次（steering / SDK 追加）静默失效；同类附件注入（image）已挂 `before_input` 修复，二者未跟进（`agent/stages/mod.rs`、`at_mention/mod.rs`、`subagent/skill_preload.rs`）。
- @mention 注入无字节上限（仅有行数截断），单行超大文件可整段进入上下文（`file_observation.rs`）。

**方案备注**
- **输入生命周期**：抽取本批输入处理逻辑，`before_agent` 处理首批、`before_input` 处理中途批次；二者消费 `input_message_ids`，不扫描历史最后一条 Human 充当新输入。空批次不注入。Agent 定义显式声明的预载仍只在初始化执行，不能把它机械搬到每个 input hook 重复加载；用户 `/skill` 按新消息处理。
- **预算**：`file_observation.rs` 已有 32 MiB 文件读取门限，缺的是模型可见正文的字节预算，因此原“无字节上限”应限定为输出面。provider 同时约束读取量、行数和 UTF-8 输出字节；宿主为整批 mention/预载设总预算，防多个合法文件累加。超限带截断/未载入说明及可继续读取的位置，不能无标记裁掉内容。
- **验收**：同一 loop 首批、中途 steering、SDK 追加、多 Human 同批及纯工具续跑，逐条断言对应内容只注入一次；重放不重读旧文件。覆盖单行大文本、多字节边界、多个文件总量、Workspace 关闭/断连无本机回落。预载预算与去重见 M8。

### H8 System Reminder 的 audiences / delivery 无强制投递点

- 现象：模型投递与客户端下发均不过滤受众；`ReminderFilter` 无生产调用（`session/transcript.rs`、`session/event_sink.rs`）。
- 影响：声明 `[Tui]`/`[Diagnostics]` 或 `DiagnosticOnly` 的 reminder 仍可能进模型上下文；Model-only 内容（如 recall）会下发给客户端。契约边界靠生产者自律。

**方案备注**
- **复核修正**：durable work 的 `delivery.rs` / `work_recovery.rs` 已按 `model_visible` 过滤；缺口是 legacy drain、transcript 模型投影、ACP live/replay 及文本 fallback 未统一。TUI 结构化入口已有过滤，但不能替代服务端的投递约束。
- **推荐**：复用 `ReminderFilter` 形成按目标 audience 判定的单一规则，在模型投影和各客户端出口强制调用，fallback 也必须在过滤之后。持久化保留 canonical reminder，不因 Model 不可见而删掉其它受众需要的记录；Diagnostics 只接收诊断 DTO，不顺带输出正文或任意 metadata。
- **语义**：`Required` 只在声明受众内不可被普通偏好屏蔽，不是广播；`DiagnosticOnly` 默认不进模型/TUI。投递偏好不决定任务是否执行或是否获授权。队列调度的 Required/Defer 与 reminder delivery 的 Required 分别建模，修正合法 TUI-only 提醒被 `Required && !model_visible` 拒绝的情况，不能以扩大受众绕过校验。
- **配套/验收**：审计 goal/todo/hook 等生产者，若产品要求客户端提示，应显式声明 Tui，不能继续依赖泄漏式下发；stdio 接线见 M13。覆盖各 audience×delivery 在模型、结构化客户端、fallback、replay 的矩阵，尤其 Model-only recall 不出现在客户端 wire、Tui-only 不触发模型推理。

## 中

- M1 贡献通道拼接缺陷：`collect_prompt_contributions` 无分隔符直接串接 → attribution 尾句与 deferred 列表粘连；贡献文本含 reserved boundary token 时计数异常 → Anthropic 静默关闭 system 缓存且无告警。
  - **方案备注｜推荐**：收集器统一对非空贡献用 `\n\n` 连接，provider 统一调用 `combine_system_prompt_with_dynamic`；调用方不再补分隔符或手工复制组合算法。保留来源标识，便于定位非法贡献。
  - **校验**：动态贡献不得含 reserved boundary token；在来源准入/组合边界返回带来源的可定位错误并记录结构化诊断，不静默剥离内容或继续关缓存。覆盖输入采用 L3 的显式拒绝覆盖策略。Anthropic 重复 marker 时关闭缓存本身是既有 fail-closed 防线，应保留，不修成猜测分界。
  - **验收**：空/单个/多个贡献、无尾换行、多字节文本及非法 marker；最终请求正文分段正确、marker 恰为一个，合法静态 block 缓存正常，非法输入有诊断且不发歧义请求。
- M2 本地 agent 定义 frontmatter `model` 未白名单校验、无转义逐字进入 `{{available_agents}}`（同源 `description` 已排除注入）；未知档位静默回退父模型。
  - **方案备注｜推荐**：本地、插件、远端与工具参数共用 `MODEL_TIERS` 的 typed 解析，区分未指定/`inherit` 与非法值；非法定义不进入可执行目录，点名启动返回明确错误，不静默选择父模型。可隔离坏条目并报告诊断，不必让整个会话因一份坏定义失败。
  - **边界**：目录 renderer 只消费验证后的 tier，id/标签也必须是有界单行数据，不能把原始 YAML 值写入 prompt。fork 与 resume 保持既有 model 参数忽略语义，不因统一解析意外改变；warning 记录 agent/source 与错误类别，不输出原始可疑值或秘密。
  - **验收**：所有来源的合法档位、inherit、大小写规范化、未知值及换行/括号输入；断言非法值既不能拆出新目录行也不能触发父模型调用。远端已有校验，合并重复名单而非再添一份。
- M3 冷恢复 bake 父会话冻结 prompt 作为子 Agent system（`host/cold_execution.rs`），`ChildResumeMetadata.persona` 为死字段 → 冷恢复与 live/resume 的 system 面不一致，fork 可能重复注入。
  - **方案备注｜推荐**：明确区分父冻结来源与子 Agent 的有效身份；创建时持久化子身份及可重建其能力投影的版本化数据，恢复只读原数据。live/resume/cold 都经 H1 的同一 bridge 装配，删除 cold 路径单独 bake 父 system 的旁路，动态贡献仍由恢复后的子链供给。
  - **旧数据**：`persona` 不能在修复前当普通死字段删掉；先确认它与历史 System 消息的含义，按 metadata 版本做一次确定归一化，无法确定身份时明确阻止该执行恢复（历史可读）。不写新旧双份身份，不通过重新加载已变化的 agent 定义猜身份。
  - **验收（待实测）**：同一子任务分别 live、内存 resume、进程重启后 cold 捕获请求，对比 system 身份与贡献；定义型 persona 不丢、fork 不重复、旧 metadata 可解释且未知版本安全失败。仅比较内存字段不足以关闭本项。
- M4 workflow 路径系列：contribution 收集时机早于 `before_agent` 且手工拼接绕过 `combine_system_prompt_with_dynamic`；冻结摘要为空时退化为每轮重渲染；local-only 指令被 `if let Some` 丢弃。
  - **方案备注｜复核**：提前收集会漏掉 Skills 在 `before_agent` 生成的贡献；workflow 当前不装 ToolSearch，不应承诺修复后自动获得 deferred。手工拼接在 base 已有一个 marker 时可能恰好同结果，但仍绕过统一规则。“冻结摘要为空→每轮重渲染”本次未获证：`Some("")` 不触发 `unwrap_or_else`；保留待补原复现，已确认 agentType 分支会重渲染。
  - **推荐**：复用 H1 请求时贡献 provider，删除 workflow 手工拼接；按 H2/H3 用自身能力投影及冻结环境渲染。`AgentsMdMiddleware` 接受独立 main/local 输入，任一非空均应贡献；None 与显式空快照不可混同，空值不授权重新扫描。
  - **验收**：有/无 agentType、Some 空/非空摘要、local-only、技能目录及关闭位矩阵；捕获首个和后续请求，断言贡献时机、单一 marker、冻结输入不重读。未证实的重渲染子项在补证前不单独扩展修复。
- M5 legacy 会话首次接纳发生在执行环境建立前 → 项目指令/技能/目录永久为空，仅日志可见（需产品裁决）。
  - **方案备注｜推荐**：保留原子 write-once 接纳，拆出“执行资格检查→资源环境 bootstrap→内容准入→定稿冻结与 binding 原子提交→发布可执行 session”。bootstrap 只提供读资源能力，不启动 Agent/项目 hook，也不把用于装配的临时候选冻结成最终空快照；竞争失败必须丢弃候选并读取 winner。
  - **裁决**：推荐 legacy 首次可执行接纳获得与新会话同等的资源采集机会，而不是永久接受空内容。资源确实不可用时按现有准入契约报错或显式降级并暴露缺口，不能把“尚未装配”当成“目录为空”。只读历史与异执行环境查询不触发采集/接纳。
  - **验收**：legacy 首次恢复含项目指引/技能、并发接纳 winner 一致、资源失败不提交半成品、重启后不再采集；异机和只读历史不探测本机同名路径。入口核对 `legacy_session.rs`、`session/prepared.rs` 与 `session_restore.rs`。
- M6 `PluginMiddleware` 实为零注入（仅写日志），但文档称关闭该键即关闭插件整体注入；实测关闭只摘日志，插件 skills/agents/hooks/MCP 全部照旧（能力闭合契约失真，需裁决）。
  - **方案备注｜推荐裁决**：兑现现有关闭契约，不把文档降格为“仅关闭日志”。从同一 frozen/session-local disabled policy 派生插件注入关闭位，在来源汇入前阻断 plugin skill/agent roots、commands、hooks 与 MCP 配置贡献；主、子、workflow、冷恢复均消费同一决定。
  - **边界**：按插件来源过滤，不凭工具名或路径前缀误删用户自己配置的同名能力。共享连接/缓存不能替关闭的会话继续暴露插件面；安装、卸载、marketplace 等管理面保留，不等同于执行注入面。冻结接纳前也要应用策略，避免先读插件内容再藏目录。
  - **验收**：关闭后首个模型请求、deferred 检索/猜名执行、技能/agent 资源、slash 命令、hook 执行及继承面全部缺席；非插件能力不受影响，冷恢复与原冻结决定一致。原文“实测”保留为先前记录，本轮仅静态复核。
- M7 MCP 面：工具输出仅行数上限无字节上限；tool description 无长度限制；非 builtin 的 `disabled + system_mcp` 组合每 turn fatal；ACP 连接缓存 origin 缺连接身份。
  - **方案备注｜输出与描述**：`mcp/client/output_store.rs` 为模型可见文本增加 UTF-8 字节预算，行数/字节先到先截，预算包含截断提示；全文仍经既有 Workspace output store 保留，落存失败必须告知，不能给虚假回查地址。bridge 对 description 设单项预算、deferred 渲染设总预算；不裁坏 JSON schema，超预算条目标记截断/不可装载并给出明确原因。阈值应由实际上下文预算与边界测试确定，不把建议数值写成已测性能结论。
  - **配置冲突**：在共享 `McpServerConfig::validate` 及最终配置合并准入处统一拒绝 `disabled && system_mcp`，覆盖 builtin/普通/global/project/plugin 及配置更新，不在每次 Reason 才 fatal，也不静默选择其中一个开关。
  - **缓存隔离**：ACP 来源的 cache origin 纳入声明会话、连接身份与 generation；同一连接代内复用，重连换代/更换声明时失效，不跨会话按同名 server 命中。身份由 pool 传递，不能把凭据明文放进键或日志；旧缓存自然 miss，不新增兼容读取旁路。
  - **验收**：单行大输出、多字节/提示长度边界、大描述与总目录预算；非法配置在准入期一次失败、运行期不反复报错；两个会话同名 ACP server、同会话连接换代均隔离，合法同代命中。
- M8 Skills 面：`13_skills.md` 声称目录「refreshed each turn」与实现不符；preload 无条数/字节上限、不去重；条目级字段非法被静默丢弃。
  - **方案备注｜目录语义**：推荐工具在调用时读取会话 registry 的当前投影，正文继续走统一 activation（digest/frontmatter/stale 校验）；静态摘要保持 frozen，工具目录不依赖 `before_agent` 的旧副本。prompt 改为准确说明“当前已同步的 registry 投影”，不承诺每 turn 主动扫描/网络刷新；若暂不改工具数据源，先把文案标明真实快照时机，不能声称实时。
  - **预载预算**：与 H7 共用输入生命周期；按 canonical skill 身份（来源+URI，必要时含 revision）批内去重，保持首次出现顺序，设条数、单项及整批字节预算，并限制实际读取量。显式名单超限产生一一配对的 SkillTool 缺口回执，不加载截断后仍冒充完整的指令；启发式 `/token` 保留不制造未知技能错误的既有规则，已识别技能因预算未加载则明确提示。不同输入批次的主动重载不被全会话去重误吞。
  - **非法条目**：在 discovery/activation 边界区分非法条目与空目录，隔离坏条目并记录来源、字段与错误类别，不输出原始字段正文；不得静默把“部分失败”呈现成完整成功。只读复核已确认预载与缓存问题，非法字段具体分支在实施前补夹具定位。
  - **验收**：同一 loop registry 增删、同名不同源、重复别名、空/未就绪目录、坏条目混合好条目、多项累计超限；断言工具结果反映当前投影、frozen 摘要不变、合成调用/结果配对完整。
- M9 项目指引：list→read 的 TOCTOU 会使 `session/new` 整体失败；`@import` 无总量预算；local 读取失败无日志。
  - **方案备注｜一致性**：把 list 仅作为发现提示，不当作正文已锁定的证明。对已列出后消失/变更的资源采用一次有界重采集；仍不一致则返回明确的内容准入错误并允许重试，不提交半套冻结数据。若 provider 增加 snapshot/revision 读取能力，可一次取得 main/local/依赖及 digest，但这属于协议方案，不能假定现有资源读取已原子化。是否允许缺失后以空指引继续需产品确认，默认不静默放宽。
  - **导入预算**：provider 的现有单文件、深度与 import 数量限制保留，再增加覆盖整棵展开树的累计读取量和最终文本预算（含根正文、local 与占位提示）；递归共享预算，达到上限保留有界占位并 warn，不先生成巨串再截。候选优先级、防环与 canonical 越界限制不变，宿主不读盘兜底。
  - **诊断/验收**：`scan_local/read_bounded_text` 区分正常不存在与读取/编码/超限错误，仅后者记录结构化诊断。用可控 provider 模拟 list 后删除、修改与持续抖动；验证失败不留下 frozen、重试能成功；覆盖宽导入树、重复/循环、local-only、读取拒绝及 UTF-8 边界。
- M10 Hook 其他：`additionalContext`/`initialUserMessage`/`systemMessage` 解析后无人消费；PostToolBatch block 使整轮 Err 而非回注模型；异步 hook 结果一律丢弃；子 agent 生命周期 hook 绕过 dispatcher。
  - **方案备注｜字段路由**：为每个事件建立输出支持矩阵：`additionalContext` 作为有界、带 hook 来源的 Model reminder；`systemMessage` 作为客户端提示；`initialUserMessage` 仅在 SessionStart 受控准入一次并保留 hook 来源，不伪装为用户亲自输入。暂不支持的字段必须明确诊断，不能“解析成功即假装生效”。复用 H8 的投递规则与 M13 的稳定事件身份。
  - **阻断反馈**：区分 PostToolBatch 的 Block 与 `continue:false`。前者在工具结果提交后回注有界反馈，让模型修正，不把已发生工具执行伪装成 ToolRejected；后者通过显式停止意图回到 Receive 唯一退出口。沿用防循环计数，真实执行/解析异常仍显式报错。
  - **异步语义**：异步结果不追溯改变已执行的授权决定；推荐先明确为非决策 hook，记录完成/失败/取消及不支持输出的诊断，不能只把所有结果当 Allow。若需要异步 additionalContext，再单独定义 durable 投递、去重和会话结束行为，不用裸后台消息追加。
  - **生命周期统一**：SubagentStart/Stop 走同一个 dispatcher，复用 matcher、条件、once、超时、取消和进程树 owner；提供真实父/子身份，不再旁路裸 spawn。生命周期事件是否允许阻断需明确，默认保留非阻断语义而不是让任意 action 自动生效。
  - **验收**：组合输出不互相覆盖、context/提示送到正确受众、初始消息不重复；Block 后下一次模型请求可见反馈、停止意图不发额外请求；异步失败可观察；子生命周期 matcher/once/条件/取消与主链一致。
- M11 段落装配守护缺口：无跨持有者 order 唯一性测试；`Cached` 区无「纯静态」校验；生产不消费链收集（gate 双轨）。
  - **方案备注｜推荐**：与 H2 一次收敛段落来源：生产渲染消费实际装配描述/链收集的段落，不继续在 `session/frozen.rs` 维护独立 holder 名单。冻结先于完整链时，把共同的装配决策作为纯输入传给两者，不为渲染另起含副作用的链。
  - **守护**：跨 holder 验证 section id 与 `(zone, order)` 唯一，冲突在构造期显式失败并指出来源；Cached 仅接收静态模板，检查其不得包含动态环境/能力占位符，合法冻结覆盖也须遵守分区约束。不能只用 `Builtin` 类型判“纯静态”，模板可能仍含动态占位符。
  - **验收**：主/子/workflow 及关闭组合的 presence/parity；故意构造重复 id/order、Cached 动态占位符须失败；不同 cwd/date/catalog 渲染时 Cached 前缀字节稳定，Uncached 允许按冻结输入确定变化。
- M12 Compact 契约 Verify 的两个 adversarial 套件为红且不在 CI；摘要以 user 角色回填，但主 system prompt 无 reminder 权威级别声明。
  - **方案备注｜验证闭环**：先用仓库 patched Cargo 入口实际运行 `compact_pressure_adversarial_test` 与 `compact_session_adversarial_test`，记录失败用例与根因；本轮未重跑，不能把先前“为红”当最新结果。区分夹具环境与实现违反契约，分别修复，禁止 skip/削弱断言换绿；修复后把这两套加入 CI 执行集合，与 ARC-COMPACT-001 Verify 对齐。
  - **权威声明**：主 runtime 段说明真实 reminder 由 harness envelope/来源识别，不能凭用户正文中的同名标签提权；Compact 摘要是历史交接资料，不是用户新请求，不能覆盖现行系统/开发者约束、审批边界或用户后续更正。提醒可报告运行状态，但其中引用的工具/用户/文件文本不自动成为指令。保留当前 user-role 承载方式，不为解决措辞问题直接提到 system 角色。
  - **验收**：两套真实执行且 CI 命中非零测试；加入真 envelope 与用户伪造标签、摘要引用指令与后续用户纠正的行为夹具，并检查最终 prompt/投影。声明变更只对新冻结快照生效，旧会话不强制重写；不得将字符串存在断言宣称为模型必然遵循的证明。
- M13 System Reminder 其他：cron 路径 `expect` panic（超长 prompt）；无 `delivery_id` 的 reminder 去重失效；`StdioEventSink` 未实现 reminder 推送。
  - **方案备注｜cron**：`continuation.rs` 不再对外来长度使用 `expect`，创建/更新任务及触发消费两端验证可承载预算；metadata 只放事件身份、长度等摘要，不重复保存完整 prompt。超长新任务明确拒绝并给出限制，历史超长任务产生可观察的投递失败且保留待处理证据，不能静默截掉任务指令后照常执行。若选择分片，必须先设计有序、原子接纳与完整性校验，不能临时拆消息绕过限制。
  - **去重身份**：可重试事件在创建时分配并持久化稳定 delivery_id，或由来源+业务事件键派生（cron 使用 task+本次 firing 身份，完成通知使用终态事件身份）；重试复用，新轮次/新触发必须新身份。没有稳定键的一次性通知也应在首次发布前生成并随事件保存，不能每次重试现场生成，更不能按正文去重。
  - **stdio 出口**：推荐补齐 `StdioEventSink` 与 replay 的显式 reminder 映射，先按 H8 过滤，再根据 session caps 发结构化事件或有界客户端摘要；若某类客户端明确不承载，应显式声明并可诊断，不能继承默认 no-op 冒充成功。确认标准事件与专用 push 不双发。
  - **验收**：body/metadata 边界及多字节超限无 panic、不丢触发；同事件重试只投递一次、不同触发同文案各一次；stdio live/replay 有/无 caps 均可观察，Model-only 内容 wire 零泄漏。

## 低

- L1 注释/契约漂移一批（段落声明、frozen 注释、插件命令占位、compact 文档与契约矛盾、子 Agent 死字段）。
  - **方案备注｜处置**：逐条关联功能修复，不把目标行为降格成错误实现：段落注释随 H2/M11、frozen 注释随 H3、子身份字段随 M3 更新。`micro-compact.md` 的“最多续写两次”按现契约纠正为“最多续写一次，共两次请求”，预算文字随 H6 同步。
  - **特别复核**：插件命令 placeholder 的 `Inject(String::new())` 会覆盖用户原文，并非仅注释失真；应复用既有真实透传语义，或明确返回“不支持”且不伪报已执行，不能宣称空串会自动 fall-through。选择须以命令产品语义为准，单独补“不吞输入/不假成功”测试。
  - **验收**：按 DOC-UPDATE-001 核对受影响 code-index/设计/契约的单一事实源；功能与说明同批一致，doc comment 变更运行对应 doc tests。不能仅用改注释关闭尚未修复的行为问题。
- L2 空段落静默丢弃无日志；Skills 空目录与「未就绪」不可区分（模型拿到内部错误串）。
  - **方案备注｜推荐**：区分有意为空的可选 persona/language 与必需段落异常为空；前者可记 debug，后者在准入/渲染校验中显式诊断并按来源拒绝或回退内置，不统一刷 warn。日志只含 section id/source/状态，不输出段落正文。
  - **Skills 状态**：将“未装配/初始化中/读取失败/Ready(empty)/Ready(nonempty)”显式区分；合法空目录返回 `[]`，未就绪或失败给稳定可操作错误，不泄露 `before_agent may not have run` 等内部实现串。与 M8 一起去掉会把空投影转 None 的歧义缓存，超时不可冒充空目录，能力关闭不可提示无限重试。
  - **验收**：各状态与可选/必需空段矩阵；空目录不报错、错误有来源与类别、未就绪不返回假空成功，正常空 persona 不产生告警噪声。
- L3 段落覆盖文本无大小预算，不校验 boundary token 与未知占位符。
  - **方案备注｜推荐**：在 meta 覆盖冻结准入点统一校验单段/总字节预算、reserved boundary token 和模板占位符；renderer 与校验共用已知占位符表，避免两份名单漂移。Cached 段另遵守 M11 的静态约束。
  - **错误策略**：非法可选覆盖拒绝应用并保留内置段，向客户端/诊断明确指出段落与错误类别，遵循既有可选覆盖降级语义；不静默截断、剥 marker 或整段消失。推荐未知 `{{name}}` 视为模板错误，并提供明确的字面量表达方式；这会影响已有自定义文本，实施前确认语法与旧配置迁移策略，不临时添加一套 strict/lenient 双轨。
  - **恢复与验收**：旧 frozen 正文不以新规则自动重写，按 snapshot 版本处理并提示无效控制标记；正常旧快照仍可读。覆盖预算边界、总量累加、marker、未知/字面量占位符、空内容与多字节文本；新快照最终 marker 唯一、拒绝原因可见、内置段保留。
- L4 死代码/无消费者项（`FrozenState::Unsupported`、`ptl_max_retries`、`SkillSource::Global`）。
  - **方案备注｜先分类**：无生产构造点不等于无契约作用；先查 Rust 调用方、序列化、配置入口与持久格式，能删除的内部残留直接删除，不为保留死代码新增 deprecated shim。若确有对外兼容义务，明确格式版本、拒绝/迁移规则与移除条件，不能只因“是 pub/serde”就无限保留。
  - **逐项建议**：`FrozenState::Unsupported` 与 snapshot decoder 的 UnsupportedVersion 选择一个错误权威，推荐在拥有格式知识的 decoder 保留未知版本显式错误，删无生产价值的平行状态/分支前核对 Store trait 消费者；不要让存储层为制造枚举使用而理解 ACP snapshot 格式。`ptl_max_retries` 无运行作用则从内部配置模型、默认值与文档删除，对旧配置给明确迁移诊断，不添加无效果参数往返测试。`SkillSource::Global` 旧本地 skillsDir 来源既已退出，应核对历史载荷后删除旧来源分支，但保留合法的开放 URI `ResourceScope::Global`，不能误删远端 scope。
  - **验收**：删除后全仓引用与编译检查，未知 frozen 版本仍安全失败且历史可查询；旧配置/持久夹具获得明确解释而非悄悄换语义；Global URI 正常，已退出的本地根不复活。`ChildResumeMetadata.persona` 不在此清理，交 M3 处理。

## 验证局限

- 全部结论为只读静态排查；仅个别路径跑了定向单测（hooks 180 passed、system_reminder 29 passed、prompt_cache 3 passed 等），无端到端 wire 验证。
- H1、M3 标记为待实测：建议先用捕获子 Agent 请求体的测试确认再修复。
- 排查期间工作树在飞，符号位置可能已位移。
- 本次补方案仅做静态复核与文档检查，未运行 Rust 测试、真实 provider 请求或攻击复现；上述历史测试结果不代表本次工作树通过。后续验收统一按 `docs/standards/testing.md`，Cargo 使用 `./scripts/cargo-rmcp-patched.sh` 入口，并确认过滤器实际命中测试。
- 实施依赖：H1/M3/M4 共用模型装配；H2/M11 共用能力与段落事实源；H7/M8 共用输入批次处理；H8/M13 共用投递边界；M1/L3 共用 prompt 控制标记校验。H4、H5 的安全阻断可先独立落地，不必等待全部重构。

## A 组实施状态（2026-10-07，分支 fix/context-preflight-a-prompt-20261007）

本组范围：H2/H3/M1/M11/L3 + L1 相关段落/frozen 注释与权威文档。**不代表本 issue 25 条完成**；B/C/D/E/F 组未动。

- **H2：部分完成（子侧未闭环）**。已完成：段落可见性由执行面能力事实投影（`peri-agent::middleware::SectionCapabilities` + `peri-middlewares::prompt_policy` 单一权威；主/子/workflow 三条链与真实装配由 parity 测试对拍）；`PermissionMiddleware` 有效模式参与判定（`disabled()` 实例不声明 10_hitl，workflow 装配与投影共用 `for_workflow` / `workflow_approval_active` 同一规则）；workflow 生产 system prompt 两处构造点改为按能力投影重建；子 Agent 渲染面（ACP `system_builder`，定义型/fork 生产路径）按子链能力投影，10_hitl/12_ask_user/11_subagent 缺席。**未完成（属 B）**：子 Agent 最终请求面（`FrozenContext.system_prompt` 仍继承父字节，bridge/消息组合的最终 system）与子身份恰好一次装配，须由 H1/M3 在生产子链上捕获 wire 请求证明——修复前不得宣称 H2 全完成。
- **H3：完成**。`platform`/`os_version`/`is_git_repo` 冻结为 `FrozenRuntimeEnv`（`FrozenContext` + snapshot V1 可选加性字段）；内容准入按**有效 Workspace 来源**判定（`McpClientPool::workspace_source` 覆盖会话声明含持久 owner 装载与部署/全局/项目/插件合并配置；准备输入的会话声明同时参与）：显式远端 Workspace ⇒ `None`（unavailable，显式标记 + warn，不探测宿主，探测计数为 0 的证据见 `prepared_test.rs`）；本地执行环境 ⇒ 准入恰好探测一次并随冻结持久化；重渲染只消费快照，旧快照缺字段 = unavailable。
- **M1：完成**。`MiddlewareChain::collect_prompt_contributions` 统一空行分隔并校验 reserved boundary token（带来源错误），provider 边界显式失败；GitAttribution 去掉自带前导分隔符；workflow 手工拼接仅补准入错误处理（请求时 provider 化归 B/M4）。
- **M11：完成**。section id 与 `(zone, order)` 唯一、`Cached` 段纯静态在构造期显式失败并指出来源；移除「重复 ID 后者覆盖 / 同序号稳定排序」兜底（测试改为显式失败断言 + Cached 前缀字节稳定断言）。
- **L3：完成**。覆盖文本在冻结准入统一校验单段/总字节预算、reserved marker、未知占位符、空覆盖与 Cached 动态占位符，非法项拒绝应用并保留内置段（结构化诊断、不入持久快照）；字面量 `\{{`/`\}}` 转义，渲染与校验共用占位符表；旧快照正文不自动改写，超总预算只诊断（`audit_total_override_budget`，解码路径 warn）。
- **L1（本组相关）：完成**。段落/frozen 注释、`docs/code-index/peri-acp.md`、`docs/design/system-prompt.md` 同步。

验证（`./scripts/cargo-rmcp-patched.sh test --locked ...`，日志与退出码见交接）：`peri-acp --lib`、`peri-agent --lib`、`peri-acp-types --lib`、`peri-model --lib` 全绿；`peri-middlewares --lib` 存在基线既有失败（子代理委派/workspace fixture 的 `Blocked: child resources …`，与本次改动前后失败集合逐条一致）；定向过滤器（prompt / frozen_snapshot / prompt_cache_boundary / middleware chain / system_cache / parity / runtime_env / capability matrix）均非零命中且通过。

## B 组实施状态（2026-10-07，分支 fix/context-preflight-b-children-20261007）

本组范围：H1/M3/M4/M5。**不代表本 issue 25 条完成**；A/C/D/E/F 组未动；18 条旧 middleware subagent fixture 仍失败（不在本组宣称修复；hook/permission 工作不属于本组）。

- **H1：关键矩阵已捕获（ACP cold 仍在途）**。实现：子 Agent bridge 在 session factory 的子链装配点构造（身份 + 请求时 provider 读 `collect_prompt_contributions`），`SubagentLlmSource::Prebuilt` 旁路删除；定义型/fork/前台/后台/live resume 共用该入口。验收：`peri-acp/src/host/executor_flow_child_chain_test.rs`（真实 `assemble::child_chain_assembler` → `SubagentChainAssemblerImpl` + 真实 durable 子会话 + `CapturePromptModel`，非空链替身）——定义型首请求中项目指令/技能摘要/延迟工具目录/子身份各恰一次、模型面工具目录含延迟入口而 `Agent`/`AskUserQuestion`/`Workflow` 不在、fork 前台携带父上下文且身份仍恰一次、定义型与 fork 后台同契约、关闭 `AgentsMdMiddleware`/`SkillsMiddleware`/`ToolSearch` 后对应贡献与入口缺席、live resume（父侧新委派 invocation）保持契约并携带子会话历史与追加指令。**未完成**：ACP 层冷恢复（进程重启）请求捕获仍缺；现有冷恢复证据是 peri-agent `cold_test` 的真实 HTTP 请求体捕获（v1 persona 身份、父字节不冒充）与宿主内存 frozen 同源修复，命令 / 再次 prompt 路径的请求断言待该 cold 夹具补齐后并入。
- **M3：完成**。`ChildResumeMetadata::resolved_identity` 版本锚定（v2 `identity_system`，None = 写入方确定无身份；v1 必须写入方 `persona`，缺失/空白 = 不可解释 ⇒ 阻止执行恢复而历史可读；删除首 own System 位置启发式；父 `system_prompt` 不参与身份判定；不引入「与父字节相等即拒绝」判据）。`ChildResumeMetadata::frozen_context` 成为执行 / resume / 宿主内存投影唯一映射；`host/cold_execution.rs` 的 `SessionState.frozen` 不再解持久 blob 当会话 frozen（blob 仅 digest 锚），v1 `runtime_env` 取该子会话自己的冻结快照，不探本地、不重写持久数据。
- **M4：完成**（本分支前序提交）：workflow/AgentsMd 请求时贡献与 main/local 独立输入。
- **M5：完成**。legacy 首次接纳顺序为「执行资格 → 仅资源 bootstrap（有效 servers 候选在池 OnceLock 前定格）→ 内容读取 → 定稿 frozen → write-once 原子接纳（winner 语义）→ 失败统一排空」；owner 声明的持久绑定移到 cwd/frozen/identity 校验通过之后（AlreadyLive 排空候选环境且不回写），失败请求不污染不可变声明、成功请求不再丢失声明。
- **验证**：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib child_chain_tests`、`... -p peri-acp --lib "host::requests"`、`... -p peri-agent --lib "subagent::factory::cold"`、`... -p peri-agent --lib resume` 均非零命中且通过；日志见 `/tmp/b-recovery-final-20261007/logs/`。
