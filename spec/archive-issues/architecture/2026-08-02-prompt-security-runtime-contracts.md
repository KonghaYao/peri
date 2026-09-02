> 归档于 2026-08-11，原路径 spec/issues/2026-08-02-prompt-security-runtime-contracts.md

# Prompt 安全边界与运行时契约收敛

**状态**：Closed
**标签**：`ready-for-agent`、`architecture`、`security`、`prompting`
**优先级**：高（P0 安全边界优先，P1/P2 随同收敛）
**类型**：架构改进 / 安全修复
**创建日期**：2026-08-02
**来源**：Prompt 逻辑审计及四组对抗验证；稳定结论已收敛到 `docs/design/system-prompt.md`、`docs/design/meta-harness.md` 与对应 ARC 契约，审计过程由本归档 issue 和 Git 历史保留

## 最新情况（2026-08-11）

PRD 定稿；审计结论吸收进 prompts/sections 分层与 prompt_test.rs 契约测试（安全层保留、tag 隔离等）

## Problem Statement

当前 system prompt 将安全策略、工程行为、工具能力、会话状态、动态事件格式与 agent persona 按固定顺序拼接成一段可整体替换的文本。这导致 prompt 中的绝对性断言与运行时机制出现漂移，并使部分安全不变量可被本地 agent definition 的 `prompt_mode: full` 移除。

从使用者视角，这会造成以下问题：

- 使用 `prompt_mode: full` 的 subagent 可以失去防御性安全、secret 保护、Git 安全协议与基础工具纪律，但仍可能获得 Bash、文件与 Web 等高影响能力。
- 用户批准启动 subagent 后，subagent 内部工具调用不经过逐工具 HITL；主 agent 和用户没有在 prompt 中获得这一授权边界说明。
- prompt 会宣称某些工具、审批规则或 agent 能力存在，但真实注册表、PermissionMode 和工具过滤结果可能不同。
- 用户或仓库本地元数据可以借由未隔离的文本 tag、skill description 或 agent description 进入模型可见的 system prompt / transcript，使模型难以区分可信运行时指令、普通用户内容与检索元数据。
- skill 摘要、环境状态与权限描述的时间语义不统一；它们有的是会话冻结快照，有的是每轮扫描结果，但 prompt 未以同一事实源表达。

对抗验证确认：核心问题不是措辞润色，而是 system prompt 缺少可验证的层次边界与单一事实源。验证也收窄了范围：channel 桥接目前未在生产装配，workflow 不匹配实际只暴露于 `-p` print 模式；fork 生产路径会继承父冻结 system prompt，原先关于 fork system prompt 完全不继承的结论不成立。

## Solution

将 system prompt 的构建重构为显式、不可随意互换的层，并让能力声明与运行时注册条件共享同一个事实源。

目标模型：

1. **安全与授权层**始终存在，任何 persona override 或 `prompt_mode` 都不能移除。
2. **稳定工程行为层**包含工具纪律、工程执行准则与 Git 保护；默认不可由 agent body 整体覆盖。
3. **能力契约层**仅描述当前实际注册、可调用的工具与能力；条件性工具的声明和注册使用同一 feature/source。
4. **运行时状态层**明确区分会话冻结快照、每轮状态与不可信异步内容；不得将普通正文伪装为可信控制事件。
5. **persona / domain instructions 层**是唯一允许 full-style override 替换的层，`prompt_mode: full` 的语义调整为只替换这层。

实现完成后，prompt 不再是运行时机制的平行副本：关键断言（审批、工具可见性、模式、能力）应由同一运行时数据或同一 feature gate 生成；用户输入、仓库元数据与异步通知必须有明确的可信度边界。

## User Stories

1. 作为终端用户，我希望任何 agent persona override 都不能移除 secret、破坏性 Git 操作和防御性安全规则，以便 subagent 仍受基础安全不变量约束。
2. 作为终端用户，我希望批准启动 subagent 前能理解该批准的授权范围，以便不会把一次启动误认为每个内部工具调用仍会单独审批。
3. 作为终端用户，我希望审批提示词准确反映当前 PermissionMode，以便不会把 Bypass、AcceptEdit、AutoMode 和 Default 的行为误认为相同。
4. 作为终端用户，我希望会话内切换 PermissionMode 后，模型看到的说明不会过时或相互矛盾，以便做出符合当前授权状态的操作。
5. 作为终端用户，我希望 prompt 只宣称当前可发现和可调用的工具，以便不会尝试调用未注册的 Workflow 或其他条件性能力。
6. 作为终端用户，我希望工具缺失时得到清晰、可恢复的反馈，以便能选择可用替代方案而非重复失败调用。
7. 作为 subagent 作者，我希望 `prompt_mode` 的语义明确且受限，以便能够自定义职责说明而不意外关闭安全规则。
8. 作为 subagent 作者，我希望 `[readonly]` 标签与实际工具能力一致，以便主 agent 可以安全地决定是否并行执行。
9. 作为主 agent，我希望 readonly/mutable 能力来自真实工具集合，而不是仅来自未强制的 frontmatter 声明，以便避免并发写入冲突。
10. 作为 skill 作者，我希望只有一个清晰的 skill 加载协议，以便不必猜测应传递 `skill_name` 还是 `skill` / `args`。
11. 作为使用 skill 的 agent，我希望 prompt 中的 skill 发现规则、当前工具定义和运行时搜索结果一致，以便可以可靠地发现并加载 skill。
12. 作为用户，我希望仓库中 skill 或 agent 的 description 被当作检索元数据而非可信执行指令，以便 clone 第三方仓库不会静默改变系统行为。
13. 作为用户，我希望输入 `<system-reminder>` 或 `<channel>` 文本不会冒充运行时可信事件，以便模型不会把普通内容错误提升为控制消息。
14. 作为未来的 channel 集成维护者，我希望 channel 内容在桥接时被转义、带来源信息且不会形成嵌套伪控制 tag，以便启用该路径时不会新增 prompt injection 面。
15. 作为会话用户，我希望环境信息明确标注为快照并能在必要时验证，以便不会依据陈旧 cwd 或 Git 状态做决定。
16. 作为在 monorepo 子目录工作的用户，我希望 Git 仓库状态与实际 Git 命令的语义一致，以便 prompt 不会错误地把我标记为非 Git 仓库。
17. 作为维护者，我希望“dynamic”只在真正会随轮次刷新时使用，以便不会错误地把非缓存区当成实时状态层。
18. 作为维护者，我希望 skill 摘要的冻结策略被明确记录，以便在缓存稳定性与会话内集合新鲜度之间做出有意识的取舍。
19. 作为 fork 调用者，我希望 fork 文档准确说明继承边界，以便理解它继承父冻结 prompt 与历史快照，但不继承所有中间件、overrides 和扩展工具。
20. 作为测试维护者，我希望关键 prompt 安全和能力契约由行为测试覆盖，以便未来添加 prompt section、feature gate 或 agent override 时不会再次产生静默漂移。

## Implementation Decisions

1. 将 prompt 构建划分为五个命名层：安全与授权、稳定工程行为、能力契约、运行时状态、persona/domain instructions。构建 API 应表达层的类型和顺序，而非将所有内容视为同一个可替换字符串。
2. 将当前 `prompt_mode: full` 改为仅替换 persona/domain instructions 层；安全与授权、稳定工程行为、能力契约和必要运行时边界始终保留。必要时引入更准确的模式名称以替代“full”所暗示的全量覆盖。
3. 将安全不变量定义为运行时契约，而非仅依赖 prompt 文案。至少包括：防御性安全限制、secret 处理规则、破坏性 Git 操作保护、基础工具调用纪律。
4. 统一 prompt 能力声明与工具注册条件。Workflow 等条件性工具必须由同一 feature / capability source 同时控制：工具注册、`SearchExtraTools` 可发现性和相关 prompt section 的可见性。
5. 将 HITL section 改为机制说明，不再把固定工具清单表述为所有模式下“always require approval”。它必须说明当前 session 的 PermissionMode、模式切换语义和实际敏感工具判定来自运行时；`cron_register` 等真实敏感工具不得遗漏。
6. 明确 subagent 授权模型。首期至少在 prompt 和 Agent 工具描述中说明：批准启动 subagent 目前授予其内部继承工具的执行权，子 agent 不执行逐工具 HITL，且传递性只到一层。是否把父 SharedPermissionMode / HITL 链传入子 agent 是单独的产品决定，不能隐式改变。
7. 收敛主 agent 的 skill 加载工具协议为一个规范接口。迁移前识别所有主 agent、workflow agent 与 middleware 调用方；子 agent 的可用接口也必须与 prompt 描述一致。Builtin skill 作为编译期来源仍须列入用户可见的发现语义。
8. 将 skill 与 agent 的 description 作为不可信检索元数据处理：在进入模型上下文前进行长度控制、转义与明确的语义隔离；模型不得把描述当作可执行的上层指令。优先将完整内容延迟到显式 Skill/Agent 加载工具返回。
9. 为 `<system-reminder>` 与 `<channel>` 定义可信度边界。运行时产生的通知与用户/仓库提供的文本在 transcript 中必须可区分；普通内容不得通过原样 tag 伪装为系统通知。为将来启用的 channel 桥接添加内容转义、来源传递和避免双重包裹的规则。
10. 修复 subagent capability 推断：将 Bash、计划任务等实际写能力纳入 mutable 判断；处理显式零工具配置；readonly 应由真实注册的工具集保证。该字段只作为对模型的调度提示，不能被误认为代码级并行锁。
11. 将“dynamic section”重命名或注释为“非前缀缓存区”，除非其内容确实在每轮重建。真正的 per-turn 状态必须走显式运行时注入路径。
12. 保持 skill 摘要会话冻结以保护 prompt 缓存稳定性，除非设计评审决定接受缓存损失；在 prompt 与工具错误语义中说明其快照性质。会话期间新增或删除 skill 的可见性差异须有可预测行为。
13. Git repository 判定使用与 Git 命令一致的向上发现语义；该状态仅作为 prompt 环境提示，不能成为安全规则是否生效的门控。
14. 修订 fork 文档而非重构 fork 继承机制：fork 生产路径继承父冻结 system prompt 与调用时完整历史快照；文档还应列出不继承的 overrides、中间件和扩展工具类别。
15. 当前未装配的 channel bridge 修复应作为防御性前置工作完成，但不把它描述为现行生产攻击路径；启用 channel 功能时必须把这些测试设为准入条件。

## Testing Decisions

1. 测试以外部契约为中心：验证最终渲染的 prompt、真实注册的工具集合、实际审批结果与 transcript 中的可信度边界；避免只断言内部字符串拼接细节。
2. 增加 full/persona override 契约测试：任意非空 override 都保留安全与授权层、secret 规则、Git guardrails 和基础工具纪律；同时验证 persona 内容确实被替换。
3. 增加 feature/registry 一致性矩阵测试：每个条件性能力在“可用”和“不可用”两种配置下，prompt 声明、工具注册和 deferred-tool 搜索结果必须一致。Workflow 的 print 模式覆盖是必需样例。
4. 增加 PermissionMode 矩阵测试：Default、Bypass、AcceptEdit、AutoMode 下敏感工具的最终审批行为与 prompt 当前模式说明一致；会话内切换 mode 后，选择的状态同步策略必须可观察、可断言。
5. 增加 subagent 授权边界测试：确认子 agent 当前不经逐工具 HITL、不会继承 Agent 工具形成递归、且文档/工具说明匹配真实边界；若未来引入 HITL 传递，则将这些测试更新为端到端审批测试。
6. 增加 skill 协议迁移测试：主 agent 和 subagent 只暴露被支持的加载协议；发现根语义包含 Builtin；旧接口如保留必须有明确兼容测试和移除计划。
7. 增加不可信元数据与 tag 对抗测试：恶意 skill/agent description、用户输入中的 system-reminder/channel 闭合 tag、future channel 内容中的 XML 闭合 tag 均不得改变其信任级别或逃逸容器。
8. 增加 capability 推断行为测试：继承 Bash 且禁用 Write/Edit 的 agent 不得被标为 readonly；只读白名单 agent 应安全标为 readonly；显式零工具配置不得误判为可写。
9. 增加环境判定测试：仓库根、仓库子目录、非仓库目录和 worktree / `.git` 文件场景应符合 Git 命令的语义。
10. 复用现有 prompt template、HITL、subagent、tool-search、skills middleware 与 canonical tool invocation contract 的测试 seam；跨层变更补充 architecture-contracts 要求的端到端链路断言。
11. 完成前运行受影响 crate 的单元测试、workspace doc tests、workspace clippy（warnings denied）及针对 prompt / tool 注册的目标测试；如修改跨层事件或安全配置，运行完整链路验证。

## Out of Scope

- 在本 PRD 内实现真正每轮重渲染全部 system prompt，或无条件放弃 Anthropic 前缀缓存。
- 将目前休眠的 channel bridge 直接上线、设计外部 channel 产品能力或扩大其授权模型。
- 默认把 subagent 改为每个工具调用均弹窗审批；这是需单独产品决策的授权 UX 变化。
- 重写 fork 的冻结 system prompt / 历史继承实现；对抗验证确认生产路径的核心继承行为正确，本 PRD 仅修正文档与边界说明。
- 修复与 prompt 审计无关的 TUI、e2e submodule、locale 或其他工作树中的已有改动。
- 添加新的第三方 prompt-sanitization、policy 或 feature-flag 依赖，除非现有设施无法表达必要契约。
- 将仓库本地 prompt / skill / agent 文件视为完全不可信并禁止加载；目标是隔离与最小暴露，而非取消可扩展性。

## Further Notes

- 审计报告已包含完整证据、对抗验证记录和被推翻条目的原因；实施前应以报告中的“修正后判定”而非初版结论作为事实输入。
- 估算：最小安全修复（安全层保留、tag 隔离、fork 文档、capability 与 Git 探测小修）约 3–4 个工作日；完整收敛约 1.5–2 周，主要风险集中在 prompt 分层和 skill 协议迁移。
- 建议实施顺序：先建立 full override 安全契约测试与 tag 对抗测试；随后完成 prompt 分层和能力 gate 一致性；再处理 HITL 说明、skill 协议、metadata 隔离、capability 推断和 P2 语义修复。
- GitHub 发布待环境提供 GitHub CLI 或 Issue MCP 后执行。发布时使用标题“Prompt 安全边界与运行时契约收敛”，并应用 `ready-for-agent` 标签。
