# MCP 适配 v4-part-4 主计划（wave 3：Workspace MCP 实例）

> 2026-09-28 范围更新：`local-mcp-server` 已按用户裁决退役，代码与独立构建入口已删除。本文涉及旧项目的路径、命令、比较和后续复用建议仅作为历史记录，不再作为实施或验收要求；当前 Workspace MCP 入口与验证见 [主项目代码索引](../../docs/code-index/peri-middlewares.md)。

> 日期：2026-09-27。状态：**v1 已冻结裁决，待施工**。本文是 wave 3 的裁决与施工唯一事实源。
> **实施回填（2026-09-28）**：本波（wave 3）代码已实施并验收；现场证据见 `spec/issues/2026-09-27-mcp-adaptation-v4-part-4-acceptance.md` §3.10 交付终态与 §8 门禁核对表（交付提交 `48ea61cb`；分支 `feat/mcp-adaptation-v4-part-3`）。
>
> 运行环境：worktree `/Users/konghayao/code/ai/peri-v4p3`，分支 `feat/mcp-adaptation-v4-part-3`（沿用），起点 HEAD `f61f06e4`（wave 2 收口）。
>
> 三态口径承接 part-1/2/3：「目标归属」（设计文档）≠「当前实现」（代码事实）≠「运行时证据」（命令 + exit + 计数）。本文中所有「已核实事实」均带 `仓库相对路径:行号`；未核实项一律标 `UNVERIFIED`，不得据推断施工。

---

## §1 范围

### 1.1 本波范围（用户裁决 2026-09-27：「workspace mcp」是原始需求未完成的三分之一）

把 **7 个本地工具**从 middleware 直供迁移为 `workspace` builtin MCP 实例提供：

`Read`、`Write`、`Edit`、`Glob`、`Grep`、`folder_operations`、`Bash`。

模型面名字相应变为 `mcp__workspace__Read` … `mcp__workspace__Bash`（设计文档 `docs/design/mcp-adaptation-v4-part-1.md:63`：「对 Agent 暴露时继续使用现有 MCP effective tool name」；同文件 `:52` 的 `system_mcp_tools` 示例以裸名列出同一组工具）。

### 1.2 明确排除项及**代码事实理由**（不是偏好，是当前代码里没有可下放对象）

设计文档把下列能力标为「完全下放 / 部分下放」，但侦察结论是**本波无施工对象**：

| 项 | 设计措辞 | 代码事实 | 处置 |
| --- | --- | --- | --- |
| `GitWatchMiddleware` | 「完全下放 → Workspace MCP」`:188` | **零工具面**（`peri-middlewares/src/git_watch/mod.rs` 无 `collect_tools` → `Middleware` 默认空表 `peri-agent/src/middleware/trait.rs:63-65`）；只有 `after_tool` hook + `SystemReminder` 注入（`git_watch/mod.rs:250-260`、`:171-195`）；无后台任务、无 tick、无 watcher（`:85-140` 为按需 `tokio::spawn` + 60s 节流 + 单飞） | **不在本波**。下放它需要新机制（MCP 通知或宿主事件端口），是独立设计题，登记为 wave 4 |
| `GitAttributionMiddleware` | 「部分下放」`:189` | **零工具**；「git 查询」只有一处 `rev-parse --abbrev-ref HEAD` 且结果**只写 tracing 日志**（`attribution/mod.rs:194-207`）；无 attribution 配置文件（全仓零命中） | **不在本波** |
| `TodoMiddleware` | 「部分下放」`:192` | 1 个工具 `TodoWrite`；**不存在 todo 文件**——`TodoState` 纯内存（`tools/todo.rs:20-24`），`invoke` 的 `_ctx` 未用；真正耦合是 `mpsc::Sender<Vec<TodoItem>>`（`peri-agent/src/session/exec/stage_builder/agent.rs:89`）与 `after_agent` steering（`middleware/todo.rs:64-118`） | **不在本波**（设计措辞中的「Todo 文件」在代码里无对应物） |
| `SkillsMiddleware` / `SkillPreloadMiddleware` | 「部分下放」`:202`/`:203` + Skill 工具下放 `:164` | skill roots 含 `~/.claude/skills`、`~/.peri/settings.json::skillsDir`、插件安装目录，**均在 workspace cwd 之外**（`skills/loader.rs:414-444`）；`SkillSource::Builtin` 内容是 `include_str!` 编译期常量、路径为虚拟 `<builtin>/<name>`（`skills/content.rs:20-27`）；`SkillSource::Mcp` 内容来自 `McpSkillRegistry` 缓存且**明确不读磁盘**（`skills/content.rs:15-16,28-32`） | **不在本波**。三处与「capability root = workspace」直接冲突，需独立裁决 |
| `PluginMiddleware` | 「部分下放」`:204` | 自身**不读任何文件**（`plugin/middleware.rs:30-112` 只校验内存中 `Arc<Vec<LoadedPlugin>>` 并写日志）；真实读取全在 loader，且多数指向 `~/.claude/**` | **不在本波** |
| `side-projects/local-mcp-server` | 「复用现有」`:71` | 见 §2 裁决 AW3-02 | **不在本波改动**（沿用 `2026-09-26-mcp-adaptation-v4-part-2-plan.md:79` 的边界：不改其 `Cargo.toml`、不加根 members、不写生产配置引用） |

---

## §2 裁决（AW3-01 … AW3-11，冻结）

### AW3-01 运行形态：**进程内 builtin**（`rmcp` 内存 transport）

依据：设计文档 `:26` 逐字允许「Builtin MCP … 可以使用 `rmcp` 的内存 transport，使 client/server 在同一进程内通过内存通道通信，不启动外部 MCP 进程」；wave 1（web/artifact）与 wave 2（cron/lsp）均为该形态，链路已成熟（`peri-middlewares/src/mcp/builtin/runtime.rs:345` 的 `spawn_builtin_transport_with_handler`）。

设计文档 `:77` 的「不能复用…**进程**…」约束的是 **5 个 MCP 实例之间**不得共享运行时进程/状态，与「实例是否为同进程 builtin」不是同一命题（`:26` 与 `:77` 的字面张力由本条裁决消解）。

### AW3-02 实现权威：**包装 peri 现有 7 个工具实现**，不采用 `local-mcp-server` 作为本波权威

- 工具实现位置（已核实）：6 个文件工具在 `peri-middlewares/src/tools/filesystem/{read,write,edit,glob,grep,folder}.rs`（合计 2256 行 / 178 个测试）；`Bash` 在 `peri-middlewares/src/middleware/terminal.rs:75-82`（853 行 / 49 个测试）。`middleware/filesystem.rs` 只是 46 行装配壳（`:15-28` 按 cwd 实例化 6 个工具）。
- **不采用 `local-mcp-server` 作为本波权威的决定性理由（代码事实）**：它的文件工具被 `RootDir` 限定在 workspace 根内（`side-projects/local-mcp-server/src/capability/root.rs:1-31`：目录 fd + `openat(O_NOFOLLOW)` 逐组件解析），而 peri 今日**没有** capability root——`resolve_path` 对绝对路径直接使用、不做包含性检查（`peri-middlewares/src/tools/filesystem/mod.rs:25-43`）。直接换权威会把「可读任意绝对路径」变成「只能读工作区」，属**功能性回归**，必须先有独立裁决与迁移方案。
- 不构成双重实现：本波是**包装**（在实例内复用同一份 `BaseTool` 实现），不是在 peri 内再造第二份。`local-mcp-server` 作为独立项目/独立 workspace 的现状不变。
- **保留后路**：实例边界是架构；边界之后的实现权威可在后续波次替换（换 `local-mcp-server` lib 或换子进程形态），届时模型面不变。登记为 wave 4 议题。

### AW3-03 工具面成员与名字

- 7 个工具**全部** `direct: true`。理由：迁移前这 7 个工具就在首个 LLM 请求的直连工具表内（wave 2 基线打印 `spec/issues/2026-09-26-mcp-adaptation-v4-part-3-acceptance.md` §2.3 的 18 项列表含全部 7 个），保持成员集合不变是「零语义变化」的选择。
- 注册表条目使 overlay 自动派生 `system_mcp: Some(true)` 与 `system_mcp_tools = direct 集合`（`peri-middlewares/src/mcp/builtin/mod.rs:317-334` 的 `builtin_default_entry`）。**不手写** `system_mcp_tools`。
- 匹配口径：`system_mcp_tools` 按**所属 server 的原始工具名**精确匹配（`peri-middlewares/src/mcp/system_tools.rs:8-10`），故闸门配置里出现的是裸名，模型面仍是 effective name。

### AW3-04 capability root：**本波不引入新根**

实例的 cwd 取 host cwd（与 pool `execution_cwd` 同源）。**如实登记为已知缺口**：本波结束后文件工具仍可访问 cwd 之外（`tools/filesystem/mod.rs:25-43` 无包含性检查），Bash 亦无沙箱（`middleware/terminal.rs:423` 仅 `current_dir`）。设计文档 `:31`/`:163` 的「独立 capability root」在本波**未落地**，验收记录必须以 `UNVERIFIED` 口径登记，不得宣称已验证。

### AW3-05 cwd 归属：per-host 单 cwd（沿用既有裁决与既有代码语义）

- 代码事实：pool 的 `execution_cwd` 是 `OnceLock` 一次性绑定、不可变更（`peri-middlewares/src/mcp/client.rs:71`、`:211-226` 变更即 `Err`）；`BuiltinInstanceContext.cwd` 注释明示「host 单 cwd … **不支持多 cwd**」（`peri-middlewares/src/mcp/builtin/context.rs:63-66`）。
- 与 wave 2 的 LSP pool 同口径（`peri-acp-types/src/ports.rs:349-356`）：多 cwd / 多 session 共享 host cwd，属**已裁决的功能退化**；per-session 恢复（A22/F1）不在本波。

### AW3-06 关闭键与两张表的迁移

- 新增 builtin 实例策略键 **`WorkspaceMiddleware`**，进 `BUILTIN_INSTANCE_POLICY_KEYS`（`peri-acp-types/src/meta_harness.rs:147-152`）。
- `FilesystemMiddleware`（`:114`）与 `TerminalMiddleware`（`:117`）从 `MIDDLEWARE_NAMES` **摘除**（先例：`CronMiddleware` 已摘除，同表 `:104-129` 已不含它）。
- 两表交集必须为空（测试 `peri-acp-types/src/meta_harness.rs:288-295`）；集合相等（`:266-282`）。
- `example/minimal/.peri/settings.json` 中现有的 `"FilesystemMiddleware": false` / `"TerminalMiddleware": false` 是**已知键**（known = 两表并集，`peri-acp/src/provider/config.rs:410-411`），摘除后会被拒收 → 该示例文件与 `example/minimal/README.md` 必须同批改键。

### AW3-07 归一闭合是本波的一等交付

`workspace` 进 `BUILTIN_MCP_INSTANCES` 后，两个既有归一入口自动生效：

1. 判定/匹配型 `original_tool_name_of_effective()`（`peri-acp-types/src/builtin_mcp.rs:179-185`）——permission、subagent `is_mutation_tool`、hooks matcher、TUI 参数摘要、事件 kind 投影、参数别名等自动继续工作；
2. 过滤型 `name_candidates()`（`peri-agent/src/session/tool_catalog.rs:168-174`）——subagent catalog 过滤自动继续工作。

但仍有 **8 处无归一入口的硬失效点**，必须逐一修（清单见 §3.2）。这条是「安全 + 功能」双重约束：不改则 `explorer` 的只读保证被绕过、`coder` 失去全部文件系统工具。

### AW3-08 中间态红灯纪律

本波必然经过「注册表已加、链未摘」或「链已摘、归一未闭合」的中间态。纪律：每次提交必须在 commit message 里**显式写明当前红灯集合与归属任务**；禁止用 `#[allow]`、删除断言、放宽断言换绿（沿 wave 2 §9 规则 5）。**红灯集合必须来自全量枚举**（`cargo test --workspace --lib --no-fail-fast`，逐 target 记录 `test result:` 行），不得只跑单 crate 或过滤串——W3-A 的提交只枚举了 `mcp::builtin` 过滤集，漏记了 `subagent::tests::mutation_tool_matches_original_name_policy_for_builtin_names`（`declared == 7` 断言随注册表扩到 14 而红），属本条纪律的**反面案例**，W3-B 提交必须补记并归属。

### AW3-09 `HandlerNotWired` 与既有前置断言

`BuiltinSpawnError::HandlerNotWired`（`peri-middlewares/src/mcp/builtin/runtime.rs:70-76`）的存在理由逐字为「后续波次的 workspace」；`peri-middlewares/src/mcp/builtin/dispatch_test.rs:82-101` 有一条「未接线名字集合非空」的前置断言，**接线后必然失效**，必须在同一提交里改为确定的具名断言（不得删除）。

### AW3-10 文档纪律

`docs/design/mcp-adaptation-v4-part-1.md` **不回填**（目标设计冻结）；`docs/standards/**` 的架构契约在语义变化后同步（先例：`docs/standards/architecture-contracts.md:114`）；`docs/code-index/**`、`docs/reference/mcp-ecosystem.md`、模块 `CLAUDE.md`、内置 skill 文本、`example/**` 按 `DOC-UPDATE-001` 同步。

### AW3-11 session 级 seam 冻结（用户裁决 2026-09-27：「先补 seam 再迁 Bash」）

**机制**：把 per-session 的 `TaskManager` **上提为「会话环境装配」的输入**，经 `BuiltinInstanceContext` 送达 `workspace` 实例的 `BashTool`——不新增协议面、不改池粒度、不做后置注入。

**依据（逐条为代码事实，取证见 §3.4.1 与本条注）**

1. **池与 builtin 上下文只在 `session_resources == true` 时构造**（`peri-acp/src/host/assemble.rs:345-349` 的 `if bare || !session_resources { None } else { … }`），而该形态今日**只**由 `SessionEnvironment::assemble`（`peri-acp/src/host/workspace.rs:77`）产生，每 session 一次 ⇒ **生产四路径（TUI / print / stdio / `session/new`）的 session ↔ pool = 1:1**。顶层三路径传 `session_resources = false`：那层不构造 builtin 上下文（只留一个从不 initialize 的 host-only pending pool），但产出 `workspace_assembly` 开关（`assemble.rs:648-652`）供每 session 分裂出独立会话环境。旁证注释：`peri-acp/src/host/mcp_v4_wave2_test.rs:406-410`。
2. **顺序今天已成立**：会话路径上 pool 的 `run_initialize` 被 `activation` 门控（`assemble.rs:492-495` 的 `activation.cancelled().await`；`workspace.rs:124-125` 的 `activate()` 由 `session_lifecycle.rs:655` / `:222` / `:1203` 在 `ensure_session`（`:613` / `:190` / `:1164`）**之后**调用）⇒ seam 只需把 TaskManager 的产生点上提到 `workspace.rs:77` 之前，无需改动任何时序契约。
3. **禁止后置注入（冻结的反面）**：装配面对每个新建 pool 在 `assemble.rs:376` **无条件先注入一次**，第二次注入必得 `AlreadyInjected`（`peri-middlewares/src/mcp/client.rs:249-257`；注意该路径上 `initialize_started` 仍为 false——被拒的是 `AlreadyInjected` 而非 `InitializationStarted`，设计时不得混淆）；`Arc<BuiltinInstanceContext>` 字段无内部可变性（`context.rs:64-78`），没有旁路。**任何「池建好之后再补 session 状态」的方案一律不采用。**

**形态（W3-B 落地，文件 owner 见 §4）**

| # | 落点 | 改动 |
| --- | --- | --- |
| 1 | `peri-acp/src/host/assemble.rs:116-131` `HostAssemblyInput` | 增设 session 级输入槽，**两名成员**：`task_manager` + `on_bg_complete`（第二成员见 §2.1）；**只有** `workspace.rs:64-75` 传 `Some`，顶层三路径与测试夹具传 `None` |
| 2 | `peri-middlewares/src/mcp/builtin/context.rs:64-78` | 增设字段 + `with_*` builder（照 `:82-107` 既有形态） |
| 3 | `peri-middlewares/src/mcp/builtin/dispatch.rs:121-134`、`workspace.rs:85-97` | 工厂与 handler 接收该输入；`WorkspaceMcpServer::new` 由单参变双参（A2 已预告），`BashTool` 经既有 `with_task_manager` / `with_on_bg_complete` builder 注入 |
| 4 | `peri-acp/src/session/mod.rs:402-411`、`construction.rs:97-103` | 暴露「新建 session TaskManager」入口 + 携带外部 manager 的 `ensure_session` 变体；三条会话路径同批改（`:613` / `:190` / `:1164`） |

**`None` 的语义 = 可见但退化**（handler 照常构造，`Bash` 拒绝 `run_in_background` 并走既有退化分支），**不是**「实例不可用」——`instance_input_ready` 不为 `workspace` 增加 arm。1:N 形态（`session_resources = true` 当 server root，今日仅测试在用）落在此分支，**必须进验收记录**。

**非目标（冻结）**：不引入 `_meta` / transport `extensions` 的会话标识贯通；不把池改成「同实例名多代并存」；不改 A11 / A33 的既有裁决。

### 2.1 `on_bg_complete` 保真度（AW3-11 第二成员：**冻结为携带**，2026-09-27 取证）

链上 `BashTool` 的能力源有两个；AW3-11 的两名成员各恢复一个。第二成员 `on_bg_complete` 是 **per-turn** executor 闭包（`peri-agent/src/session/exec/executor/agent_build.rs:174-179`），取证结论是**可以做成 session 级、且对 Shell 逐位等价**，因此 **AW3-11 一并携带它**，本波不留「bg 完成不唤醒」的残余退化。

**依据**

1. **路由键只有 session**：`AsyncRouter` 只持一个 `InboxHandle`（`peri-agent/src/session/async_router.rs:99-101`）；`route_bg_result`（`:131-138`）的全部动作 = 构造 `background_result_reminder` → `InboxHandle::push_system_reminder(MessageKind::Defer, source, reminder)` → `push`（`peri-acp-types/src/session/inbox.rs:147-153`）→ Defer `wakes_up()`（`peri-acp-types/src/session/queue.rs:24-26`）⇒ 入队 + `wake.notify_one()`。**不看 agent / turn**，也不直发 ACP 事件（TUI 面板事件来自 `registry.complete`）。
2. **通道存在且时序可控**：`SessionManager` 实现 `SessionAccessPort::session_inbox`（`peri-acp/src/session/access.rs:85-104`、`:124-129`）；`v2_message_queue` 在 session 构造时就位（`construction.rs:127-128`），inbox 首次调用 lazy 建并写回同一 `Arc` ⇒ 装配点（`session_lifecycle.rs:563`）**取不到**（session 到 `:613` 才注册），**必须在闭包体内 lazy resolve**。
3. **Shell 的 continuation 请求今天就是 no-op**：`peri-acp/src/host/continuation.rs:65-67` 的 kind 门槛（`kind != BgTaskKind::Agent || !continuation_armed ⇒ None`）⇒ session 级闭包只要做到 route 就与 per-turn 闭包等价，**不需要** host 的 `cont_tx`，也不需要新端口。
4. **`None` 的代价（仅当落该分支时）**：`peri-agent/src/agent/async_tasks/shell.rs:592-602` 整块跳过——无 Defer、无模型面提醒；`registry.complete()`（`:604`）照常执行 ⇒ 不卡住、不泄漏，「只是不通知」。

**最小形状**：在 `SessionEnvironment::assemble`（`peri-acp/src/host/workspace.rs:32-36`）构造闭包，捕获 `(Arc<dyn SessionAccessPort> ← host.session_manager, session_id)`，体内 `session_inbox(&sid)` → route。`OnBgCompleteFn` 已由 `peri-middlewares/src/assembly.rs:57` re-export。**禁止**在装配点急切调用 `session_inbox_for`（彼时 session 未注册 ⇒ 恒 `None`）。层位约束：`AsyncRouter` / `background_result_reminder` 位于 `peri-agent`，闭包在 `peri-acp` 构造——优先在 `peri-agent` 侧提供「由 `(SessionAccessPort, session_id)` 构造 session 级回调」的小 helper，避免跨层引用散落；落地前过 `scripts/check-layer-imports.sh`。

**登记（不得写成与 workflow 完全同形）**：workflow agent 的 Bash 今天同样是「有 manager、无 `on_bg_complete`」（`peri-middlewares/src/middleware/terminal.rs:814-823` + `assembly/workflow.rs:169-204`），但 **workspace builtin 的 Bash 由主链发起**：主链会因 `active_count > 0` 挂起，`None` 时会「被 registry watch 唤醒 → 空转一次 Receive → turn 结束且不提任务」（`peri-agent/src/agent/stages/mod.rs:816-857`）⇒ 残余差异必须进验收记录。

---

## §3 影响面清单（施工依据）

### 3.1 自动生效面（**不需要改**，但需要回归断言）

| 面 | 位置 | 为何自动生效 |
| --- | --- | --- |
| 审批判定 | `peri-middlewares/src/permission/mod.rs:58-63` | 先归一 |
| 编辑类判定（AcceptEdit） | `permission/mod.rs:88-93` | 先归一 |
| hooks matcher（两侧候选展开） | `peri-middlewares/src/hooks/matcher.rs:13-28`、`:66-70` | 两侧归一 |
| subagent catalog 过滤 | `peri-agent/src/session/tool_catalog.rs:128-158`、`:268`、`:306` | `name_candidates` |
| subagent `is_mutation_tool` | `peri-middlewares/src/subagent/mod.rs:397-404` | 先归一 |
| 事件 kind 投影 | `peri-acp/src/event/tool_projection.rs:71-84` | `:72` 先归一（**投影真值名字不被改写**，仍是 effective name，`:68-70`） |
| 参数别名兼容 | `peri-agent/src/tools/invocation.rs:133-138`、`:184-194` | `:188-189` 双侧比对 |
| TUI 参数/输入/输出摘要 | `peri-tui/src/kit/tool_display.rs:26-34`、`truncate.rs:153-157`、`:299-310` | 先归一 |
| closed 集合过滤（四个投影面） | `peri-middlewares/src/mcp/builtin/mod.rs:204-231` | 表驱动，不硬编码实例名 |

### 3.2 硬失效点（**必须改**；每条都是「无归一入口」的精确名比较）

| # | 位置 | 现状 | 后果 | 修法 |
| --- | --- | --- | --- | --- |
| N1 | `peri-middlewares/src/subagent/fork.rs:34-66`（`filter_tools`） | 对 `tool.name()` 做**大小写不敏感精确匹配**，无归一 | **安全**：`explorer.md` 的 `disallowedTools: [Write, Edit, Bash, folder_operations, …]` 不再命中 `mcp__workspace__Write` ⇒ 只读保证被绕过。**功能**：`coder.md` 的 `tools: Read, Grep, Glob, Bash, Edit, Write, TodoWrite` 全不命中 ⇒ coder 失去全部文件系统与 Bash 工具 | 在 `filter_tools` 的名字比较前展开归一候选（复用 `original_tool_name_of_effective`），使两侧都可命中；并加具名回归断言（explorer 不得持有 `mcp__workspace__Write`；coder 必须持有 `mcp__workspace__Read`） |
| N2 | `peri-middlewares/src/subagent/mod.rs:411-423`（`core_mutation_tools_fully_disallowed`） | `MUTATION_CORE` 为**裸名小写字面量**，与 `disallowed` 列表直接比较 | 用户若写 effective name 则判定失效 | 与 N1 同一归一策略 |
| N3 | `peri-middlewares/src/error_suggest/suggesters/{bash_command,glob_pattern,regex,range,path}_suggester.rs` | `ctx.tool_name != "Bash"/"Glob"/"Grep"/"Read"` 精确比较（4 处）；**漏登记的第 5 个**：`path_suggester.rs:10-18` 的 `PATH_TOOLS: [Read, Edit, Write, Glob, CreateDir, Move, Delete]` 白名单与 `:78-81` 的 `match tool_name { "Glob" => "path", _ => "file_path" }` | 失败后的修复建议**静默消失**（5 个 suggester 全部；`path_suggester` 还会取错参数字段） | 在 `error_suggest/mod.rs` 增设 `normalized_tool_name(name) -> &str`（`original_tool_name_of_effective(name).unwrap_or(name)`），5 处比较前归一；`path_suggester` 的两处都要改 |
| N4 | `peri-tui/src/kit/tool_display.rs:9-15`（`format_tool_name`） | 无归一 ⇒ 工具卡标题显示 `mcp__workspace__Bash` 而非 `Shell`、`mcp__workspace__folder_operations` 而非 `Folder` | TUI 显示退化 | 归一后取短名（与同文件 `:26-34` 的既有范式一致） |
| N5 | `peri-tui/src/kit/message_area/render/tool_card.rs:265`、`:446-477` | `data.tool_name == "Bash"` 等精确比较 | Bash 卡不再显示 `$ command` 前缀行；`— N lines` / `— N matches` / `· +N −M` 后缀全丢 | 归一 |
| N6 | `peri-tui/src/kit/acp_types/tool_card.rs:64-68`、`:83` | `matches!(tool_name, "Edit"\|"Write")` 门控 diff 视图 | **Edit/Write 不再渲染 diff** | 归一 |
| N7 | `peri-tui/src/kit/acp_types/current_turn.rs:259-267`（`has_running_bash_tool`） | `t.tool_name == "Bash"` | 运行中 Bash 不再被识别（计时/状态显示退化） | 归一 |
| N8 | `peri-middlewares/src/permission/mod.rs:143-216`（`sensitive_tool_entries`，定长 `[SensitiveToolEntry; 14]`） | 模型可见的敏感工具清单（渲染 `peri-acp/prompts/sections/10_hitl.md`）；`builtin_tool_effective_name`（`:123-133`）对未注册实例**直接 panic** | 4 个已迁移工具的条目仍是裸名（`Bash`/`folder_operations`/`Write`/`Edit`）⇒ 面板与裸名脱钩 | **原地改名，不是新增**（2026-09-27 侦察更正）：`permission/mod_test.rs:441-471` 要求**每个**条目都满足 `default_requires_approval` 为真——`Read`/`Glob`/`Grep` 归一后走免审批分支，**加进清单必红**；`:475-486` 另锁 `len()==14` 且前缀条目恰 3 项。故只把上述 4 条改名为 `builtin_tool_effective_name("workspace", …)`，计数保持 14/3；`Read`/`Glob`/`Grep` 的免审批以**反向断言**呈现。前置：T1 已落地（否则 `find("workspace")` 为 `None` ⇒ panic） |

| N9 | `peri-acp/src/session/command/rewind.rs:249`、`:266`（下游 `parse_tool_call` 在 `:279-290`） | 两处裸名精确比较：`tc.name == "Write" \|\| tc.name == "Edit"`（OpenAI 格式路径）与 `name == "Write" \|\| name == "Edit"`（Anthropic `ContentBlock::ToolUse` 路径） | **功能**：`/rewind` 的文件变更发现静默失效 ⇒ 预览/恢复丢变更（两种格式同时失效） | **在调用点归一**：`original_tool_name_of_effective(name).unwrap_or(name)` 后再比较、把**原始名**传给 `parse_tool_call`（该函数保持纯，避免双归一）；`peri-acp` 经 `peri-acp-types` 归一（既有先例 `peri-acp/src/event/tool_projection.rs:71-84`）。计划外新增（2026-09-27 侦察发现） |
| N10 | `peri-agent/src/agent/compact_v2/full.rs:415`、`:446`（`extract_recent_files` / `extract_skills_paths`） | `if tc.name == "Read"` 裸名精确比较 | **功能**：compaction 的「最近读取文件 / skills 路径」提取失效 ⇒ 压缩后上下文丢线索 | 同 N9 的归一入口（`peri-agent` 已依赖 `peri-acp-types`）。计划外新增（2026-09-27 侦察发现） |

> N8 的补充事实：`Read` / `Glob` / `Grep` 今日免审批（判定面 `permission/mod.rs:66-81` 的精确名分支不含它们），改名后**仍免审批**（归一后仍不命中需审批分支且不落 `mcp__` 前缀兜底——`workspace` 已注册时 `original_tool_name_of_effective` 命中，不再走 `:77` 的 `starts_with("mcp__")` 兜底）。该断言必须有具名测试锁定。

### 3.3 表与装配面（结构性改动）

| # | 位置 | 改动 |
| --- | --- | --- |
| T1 | `peri-acp-types/src/builtin_mcp.rs:128-153` | 追加 `WORKSPACE_TOOLS`（7 项）+ `BUILTIN_MCP_INSTANCES` 条目 `workspace` / `policy_key: "WorkspaceMiddleware"`。`BUILTIN_RESERVED_INSTANCE_NAMES`（`:159-160`）**已含** `workspace`，**不改** |
| T2 | `peri-middlewares/src/mcp/builtin/workspace.rs`（新） | `WorkspaceMcpServer` + `impl ServerHandler`（`get_info` / `list_tools` / `call_tool`，复用 `web.rs` 的三个共享 helper：`server_info` / `list_tools_of` / `invoke_tool_call`）。**不覆写 `discover`**（`dispatch.rs:49` 注释 + `runtime.rs:342-344`：覆写会让 `tools/list` 被 `-32602` 拒绝） |
| T3 | `peri-middlewares/src/mcp/builtin/dispatch.rs:41-46`、`:51-85`、`:121-134` | 枚举加 `Workspace(WorkspaceMcpServer)` 变体 + 三条转发 arm + 工厂 arm |
| T4 | `peri-middlewares/src/mcp/builtin/context.rs:64-78`、`:92-103` | `BuiltinInstanceContext` 加 `workspace: Option<WorkspaceInstanceInput>`（AW3-11：`task_manager` + `on_bg_complete` 两名成员）+ `with_workspace`。**不加 `instance_input_ready` arm**：workspace 落既有 `Some(_) => true` 分支，`None` 输入 = 可见但退化（AW3-11） |
| T5 | `peri-middlewares/src/assembly.rs:23-25` | 新输入类型加进公开 re-export（A33 约束：`peri-acp` 不得 import `peri_middlewares::mcp::builtin`） |
| T6 | `peri-acp/src/host/assemble.rs:366-376` | 追加 `.with_workspace(...)`，**必须早于** `:496` 的 `McpClientPool::run_initialize` |
| T7 | `peri-agent/src/session/factory.rs` + `peri-middlewares/src/assembly.rs` | 摘除 `ChainSlot::Filesystem` / `ChainSlot::Terminal`（先例：`ChainSlot::Cron` 的摘除）与其装配分支 |
| T8 | `peri-acp-types/src/meta_harness.rs:104-129`、`:147-152` | 见 AW3-06 |
| T9 | `peri-acp-types/src/meta_harness.rs:210-241`（`MIDDLEWARE_TOOL_NAMES`） | 7 个裸名条目**必须删**（先例裁决 IF-F5，见 `2026-09-26-mcp-adaptation-v4-part-2-sub-plan-f-instances-web-artifact.md:226-230`）。不删则 `peri-agent/src/session/exec/stage_builder/tools.rs:41-56` 会**永久误剔**任意来源注册的同名工具 |
| T10 | `peri-middlewares/src/middleware/filesystem.rs`、`middleware/terminal.rs` | 删 `FilesystemMiddleware` / `TerminalMiddleware` 类型与其 `tool_names()`；`BashTool` 与 6 个工具实现**保留并复用**（AW3-02） |

### 3.4 **P0 待核实项**（施工第一步必须取回证据，不得凭推断施工）

| # | 问题 | 为何必须先核实 | 取证据方式 |
| --- | --- | --- | --- |
| Q1 | `TaskManager` 的注入层级：`peri-middlewares/src/assembly.rs:280-287` 是**链装配**注入点（`with_task_manager` / `with_on_bg_complete`），而 `BuiltinInstanceContext` 是 **host 级**（`assemble.rs:366-376` 构造）。二者是否同源？session 独立装配路径（`peri-acp/src/host/workspace.rs:64-83`）是否会产出不同的 TaskManager？ | **Bash 的功能完整性取决于此**：今日链上的 `BashTool` 带 `task_manager`（超时可提升为后台任务、`run_in_background`、Tasks 面板），而父工具面用的是**无** `task_manager` 的 `TerminalMiddleware::build_tools(cwd)`（`assembly/preparation.rs:116-118`）。若实例拿不到同一份 TaskManager，Bash 会**静默退化**为「超时即杀进程组」 | 读 `assembly.rs` 装配上下文来源 + `peri-acp/src/host/assemble.rs` 的 `HostAssemblyInput` 构造链；输出「TaskManager 实例身份」证据（是否同一 `Arc`） |
| Q2 | `BuiltinInstanceContext` 的一次性注入是否容纳 session 级状态 | 若 TaskManager 是 session 级而 context 是 host 级，则本波需要**新的注入 seam**（或沿用既有 `set_builtin_instance_context` 的时序约束加一层） | 同上，并给出注入点在 `assemble.rs` 的确定位置 |
| Q3 | 每个工具 `BaseTool::prompt_declaration()` 的现值 | `builtin_mcp.rs` 的 `BUILTIN_MCP_INSTANCES` 要求逐工具填 `prompt_declaration`；`builtin_mcp_test.rs` 按实例断言其形状（wave 2 先例：web/artifact 为 `Some` 且含 `{{name}}`，cron/lsp 为 `None`） | 读 `tools/filesystem/*.rs` 与 `middleware/terminal.rs` 的 `prompt_declaration()` 实现 |
| Q4 | 6 个文件工具与 `BashTool` 的 `namespace()` / `is_direct()` / `timeout()` 现值 | 迁移后这些属性是否仍被消费、是否需要对齐 | 读各工具实现（`namespace` 恒 `Some("filesystem")` / `Some("execution")` 已由侦察给出，需逐条确认） |

#### 3.4.1 Q1–Q4 结论（2026-09-27 取证，HEAD `9d10c6ac`）

> 取证方式是**静态阅读**（无运行时证据）；行号以取证时的工作树为准。**Q1/Q2 是前置门，结论为「不通达」**，故用户裁决「先补 seam 再迁 Bash」——seam 的形状与冻结以 §2 AW3-11 为准。

**Q1 — 不通达（「是否同一 `Arc`」这一问法不成立：上下文根本没有该字段）**

- 链上 `BashTool` 的 TaskManager 是 **per-session**：`AcpSession.task_manager`（`peri-acp/src/session/mod.rs:115`）由 `make_task_manager()`（`peri-acp/src/session/construction.rs:103`）经 `task_manager_factory` 产出，而该工厂是 `peri-acp/src/host/assemble.rs:221-224` 的 closure —— **每次调用新建一份** `TaskManager::new()`。
- 传递链（逐段）：`peri-agent/src/session/exec/executor/agent_build.rs:169-172` → `peri-acp/src/session/access.rs:142-145` → `peri-agent/src/session/exec/stage_builder.rs:352`/`:375` → `peri-middlewares/src/assembly.rs:106-107` → `peri-middlewares/src/assembly.rs:280-283`（`TerminalMiddleware::with_task_manager`）。
- `BuiltinInstanceContext`（`peri-middlewares/src/mcp/builtin/context.rs:64-78`）字段只有 `cwd` / `cron` / `lsp` / `closed`：**没有 TaskManager 通路**；注入点 `peri-acp/src/host/assemble.rs:366-375` 处**不可见任何 session**（`session_manager` 到 `:618` 才构造）。
- 旁证：`peri-acp/src/host/workspace.rs:120` 存在**第三份**独立 TaskManager（只服务 SessionEnd 清理），与链上不同源。
- 对照事实：父工具面的 `TerminalMiddleware::build_tools(cwd)`（`peri-middlewares/src/middleware/terminal.rs:810-812`）构造的 `BashTool::new` 置 `task_manager: None`（`:88`），被 `peri-middlewares/src/assembly/preparation.rs:116-118` 用作 parent_tools。

**Q2 — 现有 seam 不能容纳 session 级状态，必须新增（seam 形状见 AW3-11）**

- `set_builtin_instance_context` 是**每池一次**的注入面：重复注入返回 `AlreadyInjected`（含同一 `Arc` 再注入）、`initialize` 开始后返回 `InitializationStarted`（`peri-middlewares/src/mcp/client.rs:249-257`；封口调用点为 `peri-middlewares/src/mcp/initialize.rs:148` / `:182`）。
- 时序：注入（`assemble.rs:376-382`）→ `spawn_background(run_initialize)`（`:492-505`）→ 环境装配（`peri-acp/src/host/requests/session_lifecycle.rs:563`）→ `ensure_session`（`:613`）。**session 的 TaskManager 诞生晚于注入点**。
- 结论：B2 若按 T6 只加 `.with_workspace(...)`，Bash 必然拿到 `task_manager: None`（丢 `run_in_background`、超时提升、`command &` 进程组注册、`begin_external_execution` 所有权跟踪——四处证据见 `middleware/terminal.rs:347-349` / `:527-548` / `:684-697` / `:415-419`）。

**Q3 — 7 项全部 `Some(...)` 且必含 `{{name}}`（`Read`/`Bash` 另含 `{{title}}`）；注册表成为迁移后声明段的唯一来源**

- 逐工具实现位置：`tools/filesystem/read.rs:116-121`、`write.rs:146-151`、`edit.rs:102-107`、`glob.rs:330-335`、`grep.rs:349-354`、`folder.rs:296-301`、`middleware/terminal.rs:291-296`；逐字文本已按 A1 冻结进 `peri-acp-types/src/builtin_mcp.rs` 的 `WORKSPACE_TOOLS`（与实现逐字一致，含字节级比对证据）。
- 消费点是**唯一**的：`peri-middlewares/src/tool_search/declaration.rs:57-60`（`tool.prompt_declaration().or_else(|| builtin_declaration(name))`），`:63-69` 按 **effective name** 查注册表；渲染规则 `:76-89`（`{{name}}` → effective name、`{{title}}` → title，缺省空串）。
- 桥侧 `McpToolBridge` **不实现** `prompt_declaration()`（覆写清单见 `peri-middlewares/src/mcp/tool_bridge.rs:222-248`）⇒ 迁移后注册表这一份是声明段唯一来源，`or_else` 分支恰好兜住。
- **未裁决项（用户已裁决：逐字保留）**：模板内的**交叉引用裸名**（`Glob` 模板的 `folder_operations` / `Bash ls`、`folder_operations` 模板的 `Bash ls`、`Bash` 模板的 "the purpose-built tools **above**"）在迁移后与模型面名字不一致，本波**不改文本**，登记为已知项。

**Q4 — 现值与迁移后消费**

| 工具 | `namespace()` | `is_direct()` | `timeout()` |
| --- | --- | --- | --- |
| `Read` / `Edit` / `Glob` / `folder_operations` | `Some("filesystem")` | `true` | trait 默认 `Some(120s)`（`peri-acp-types/src/tools.rs:581-583`） |
| `Write` / `Grep` | `Some("filesystem")` | `true` | 显式 `None`（`write.rs:183-185` / `grep.rs:461-463`） |
| `Bash` | `Some("execution")` | `true` | 显式 `None`（`middleware/terminal.rs:329-331`） |

- 迁移后 `is_direct()` 由桥取注册表 `direct`（`tool_bridge.rs:246-248`）；`timeout()` 桥恒 `None`（`:238-240`）⇒ 7 项统一为「外层无超时，唯一上界是桥内 `TOOL_CALL_TIMEOUT = 120s`」（`:60` / `:282`）。**登记为细节变更**：`Read`/`Edit`/`Glob`/`folder_operations` 由「外层 120s」变「桥内 120s」，超时错误文本与语义不同。
- `namespace()` 桥**不转发**（走 trait 默认 `None`，`peri-acp-types/src/tools.rs:626-628`）⇒ 声明段排序前缀退化；因 7 个模板都不含 `{{namespace}}`，**只有顺序变化、文本不变**。

### 3.5 已知语义变更登记（施工完成后逐条进验收记录）

| # | 变更 | 影响面 | 判定依据 |
| --- | --- | --- | --- |
| S1 | 模型面工具名由裸名变 `mcp__workspace__<Name>`（7 项） | 首个 LLM 请求直连表、搜索面、审批面、事件载荷、TUI 展示、PTC 目录、Langfuse 观测名 | AW3-03 / 设计 `:63` |
| S2 | `## Deferred Tools` 摘要面不再含这 7 个名字（`mcp__` 前缀过滤的先决效应） | 首个请求 system 文本 | 代码事实：`peri-middlewares/src/tool_search/tool_index.rs:303-322`（`:311` 过滤） |
| S3 | 关闭键从 `FilesystemMiddleware` / `TerminalMiddleware` 变为 `WorkspaceMiddleware` | MetaHarness 策略、`example/minimal/**`、文档 | AW3-06 |
| S4 | 能力面**不引入** capability root（如实登记为缺口而非「已完成」） | 安全语义 | AW3-04 |
| S5 | 多 cwd 共享 host cwd（沿用既有退化） | 多 cwd 部署 | AW3-05 |
| S6 | 工具声明模板内的**交叉引用裸名**（`Glob` 模板的 `folder_operations` / `Bash ls`、`folder_operations` 模板的 `Bash ls`、`Bash` 模板的 "the purpose-built tools **above**"）迁移后与模型面名字不一致 | 首个请求的 `## Available Tools` 声明文本、工具选择质量 | 用户裁决 2026-09-27「逐字保留，登记为已知项」；证据见 §3.4.1 Q3 |
| S7 | `timeout()` 语义变化：`Read`/`Edit`/`Glob`/`folder_operations` 由「外层 120s」变「桥内 `TOOL_CALL_TIMEOUT = 120s`」，超时错误文本与取消归属不同 | 超时错误文本、取消语义 | §3.4.1 Q4（`tool_bridge.rs:60` / `:238-240` / `:282`） |

---

## §4 施工阶段与任务表

> 每阶段结束**必须至少一次 `git commit`**（用户要求：以提交存步骤）。
> 文件 owner 唯一：同一文件在同一批次内只允许一个写者。

### W3-A 注册表与 handler 骨架（可并行两组）

| 任务 | owner 文件 | 内容 | 验证 |
| --- | --- | --- | --- |
| **A1** | `peri-acp-types/src/builtin_mcp.rs`、`builtin_mcp_test.rs` | `WORKSPACE_TOOLS`（7 项，`direct: true`）+ `BUILTIN_MCP_INSTANCES` 条目；按实例扩展既有断言（`tools_are_non_empty_and_unique_per_instance`、`policy_keys_are_unique_and_frozen`、`find_hits_only_implemented_instances`、`original_tool_name_of_effective_hits_frozen_literals`、`wave1_tools_are_all_declared_direct`） | `cargo test -p peri-acp-types --lib -- builtin_mcp::tests`（列出命中名单，禁 `0 tests`） |
| **A2** | `peri-middlewares/src/mcp/builtin/workspace.rs`（新）、`workspace_test.rs`（新）、`builtin/mod.rs` | `WorkspaceMcpServer`：构造时按 cwd 实例化 7 个工具（`ReadFileTool`…`FolderOperationsTool`、`BashTool`）；`get_info` → `server_info("peri-workspace-mcp")`；`list_tools` → `list_tools_of`；`call_tool` → `invoke_tool_call(self.tools(), cwd, &request)`；末行 `#[cfg(test)] #[path = "workspace_test.rs"] mod tests;` | `cargo test -p peri-middlewares --lib -- mcp::builtin::workspace::tests`；含线路级断言（经真实 `spawn_builtin_transport_with_handler` + `serve_client_auto`），不得只调 `list_tools` |
| **A3** | 先做 Q1–Q4 的只读取证，把结论写进本计划 §3.4 的「结论」列 | 取证 | 输出带 `路径:行号` 的结论；无结论不得进 W3-B |

### W3-B 接线与注入

| 任务 | owner 文件 | 内容 | 验证 |
| --- | --- | --- | --- |
| **B1** | `peri-middlewares/src/mcp/builtin/dispatch.rs`、`dispatch_test.rs` | 枚举变体 + 三条 arm + 工厂 arm；处理 AW3-09 的前置断言 | `cargo test -p peri-middlewares --lib -- mcp::builtin::dispatch::tests` |
| **B2** | `peri-middlewares/src/mcp/builtin/{context,dispatch,workspace}.rs` + `peri-middlewares/src/assembly.rs`、`peri-agent/src/session/*`、`peri-acp/src/host/{assemble,workspace}.rs`、`peri-acp/src/session/{mod,construction}.rs`、`peri-acp/src/host/requests/session_lifecycle.rs`、宿主测试 | **按 AW3-11 落地 session 级 seam**：`WorkspaceInstanceInput`（`task_manager` + `on_bg_complete`）+ `with_workspace` + re-export + `HostAssemblyInput` 两名成员槽 + `run_initialize` 之前的注入；TaskManager 产生点上提到 `SessionEnvironment::assemble` 之前，`ensure_session` 增外部 manager 变体（三条会话路径 `:613`/`:190`/`:1164` 同批）；`on_bg_complete` 用 §2.1 的 session 级闭包（子包 H）。**不做** `instance_input_ready` arm、不做后置注入 | `cargo test -p peri-acp --lib -- host::mcp_v4_wave2`（既有十例不得回归）+ 新增注入时序用例 |

### W3-C 摘除与归一闭合（本波风险最高的两批，**必须同批提交**）

| 任务 | owner 文件 | 内容 | 验证 |
| --- | --- | --- | --- |
| **C1** | `peri-agent/src/session/factory.rs`、`peri-middlewares/src/assembly.rs`、`peri-acp-types/src/meta_harness.rs`、`peri-middlewares/src/assembly_test.rs` | 摘除 `ChainSlot::Filesystem` / `ChainSlot::Terminal`；`MIDDLEWARE_NAMES` 摘两键、`BUILTIN_INSTANCE_POLICY_KEYS` 加 `WorkspaceMiddleware`；`MIDDLEWARE_TOOL_NAMES` 删 7 裸名；删 `FilesystemMiddleware` / `TerminalMiddleware` 类型 | `cargo test -p peri-middlewares --lib -- assembly::tests`、`cargo test -p peri-acp-types --lib -- meta_harness::tests`（红→绿，逐条列出） |
| **C2** | `peri-middlewares/src/subagent/fork.rs`、`subagent/mod.rs`、`error_suggest/{mod.rs,suggesters/*}`、`permission/mod.rs` + 各自 `_test.rs` | N1/N2/N3/N8 四处归一闭合；**N8 是「原地改名 4 项」，不是「新增 7 项」**（2026-09-27 侦察更正，详见 §3.2 N8 与 :163 补充事实：`permission/mod_test.rs:441-471` 要求每个条目 `default_requires_approval` 为真、`:475-486` 锁 `len()==14` 且前缀条目恰 3 项 ⇒ 只把 `Bash`/`folder_operations`/`Write`/`Edit` 4 条改名，计数保持；`Read`/`Glob`/`Grep` 的免审批以反向断言呈现）。本批同时认领 `subagent/mod_test.rs:521-524` 的 `declared == 7`（W3-A 后实为 14，中间态红灯）| 具名用例：explorer 不持 `mcp__workspace__Write`；coder 持 `mcp__workspace__Read`；**5 个** suggester 对 effective name 生效（含 `path_suggester` 的两处，见 §3.2 N3）；敏感清单 4 条以 effective name 出现、`Read`/`Glob`/`Grep` 不在需审批集（`default_requires_approval("mcp__workspace__Read") == false` 等） |
| **C3** | `peri-tui/src/kit/tool_display.rs`、`message_area/render/tool_card.rs`、`acp_types/tool_card.rs`、`acp_types/current_turn.rs` + 测试 | N4–N7 四处归一 | `cargo test -p peri-tui --lib`；每条硬失效点至少一条**可失败**的具名断言（先做反例实验证明断言非空转） |
| **C4** | 文档面 | `docs/code-index/{peri-middlewares,peri-acp-types,peri-agent,peri-acp,peri-tui}.md`、`docs/reference/mcp-ecosystem.md`、`docs/meta-harness.md`、`docs/design/middleware-system.md`、`docs/design/meta-harness.md`、`docs/standards/architecture-contracts.md`、`peri-middlewares/CLAUDE.md`、`peri-acp/CLAUDE.md`、`example/minimal/{README.md,.peri/settings.json}`、内置 skill/教学文本裸名 | DOC-UPDATE-001 |

| **C5** | `peri-acp/src/session/command/rewind.rs`（+ `rewind_test.rs`）、`peri-agent/src/agent/compact_v2/full.rs`（+ 测试） | N9/N10（计划外新增）：`/rewind` 的 Write/Edit 变更收集与 compaction_v2 的 Read 提取归一 | `cargo test -p peri-acp --lib -- session::command::rewind`、`cargo test -p peri-agent --lib -- agent::compact_v2`；各加**可失败**具名断言（effective name 夹具命中 + 反例 `mcp__foo__Write` 不命中） |

### W3-D 验证与收口

| 任务 | 内容 |
| --- | --- |
| **D1** | 全量门禁：`cargo build --workspace`、`cargo test --workspace --lib`、`cargo clippy --workspace --all-targets -- -D warnings`、`bash scripts/check-layer-imports.sh`、`cargo fmt --check`；四段全绿并记录 16 target 计数 |
| **D2** | **真实二进制正向验证**（优先级高于补测试，用户裁决 2026-09-26）：print 路径 + TUI 路径各一组脚本化 smoke，断言 ①首个请求工具表含 7 个 `mcp__workspace__*` 且不含裸名；②`Read`/`Write`/`Bash` 真实执行返回可核对的文本；③`SearchExtraTools` 命中 effective name；④`/mcp` 面板显示 `workspace (connected, 7 tools)` |
| **D3** | 验收记录 `spec/issues/2026-09-27-mcp-adaptation-v4-part-4-acceptance.md`：三态判定 + S1–S7 语义变更登记（含 S6 交叉引用裸名、S7 timeout 语义）+ AW3-11 的 1:N 形态与 `None` 退化登记 + 命令台账 + UNVERIFIED 清单（含 AW3-04 的 capability root） |

---

## §5 验证契约

1. **具名测试必须真实命中**：任何 `--` 过滤串必须核对实际命中函数名并列出；`0 tests` 判失败；模块前缀过滤用 `::` 结尾（先例：`--exact` 对模块前缀命中 0 例）。
2. **反例实验必做**：每条新增的「归一闭合」断言（N1–N8）都要有一次「临时破坏实现 → 断言失败 → 逐字还原（哈希/diff 为空）」的证据。
3. **禁假绿**：不得在裸名与 effective name **同时**存在的中间态下断言「迁移完成」；断言必须是 XOR 口径（恰有其一）。
4. **正向验证优先**：D2 的真实二进制证据优先于补更多单测；单测只补「硬失效点」的回归保护。
5. **证据形式**：命令原文 + exit code + `test result:` 行 + 现场打印原文；哈希用于证明还原。

---

## §6 风险与反证

| # | 风险 | 反证/缓解 |
| --- | --- | --- |
| R1 | **explorer 只读保证被绕过**（N1 未修时） | N1 是 C2 的第一优先项；提交前必须有具名断言 + 反例实验 |
| R2 | **coder 失去全部文件系统工具**（N1 未修时） | 同上；且在中间态提交里显式登记 |
| R3 | Bash 的 `TaskManager` 未通达 ⇒ 超时提升/后台任务静默退化 | Q1/Q2 是本波**前置门**；若结论是不通达，则该情形必须在验收记录登记为语义变更，或把 Bash 从本波范围移出并单独裁决 |
| R4 | 摘除 `ChainSlot::Filesystem`/`Terminal` 与 `MIDDLEWARE_TOOL_NAMES` 删条目不同批 ⇒ 误剔任意来源同名工具 | C1 同批提交；`stage_builder/tools.rs:41-56` 的行为断言必须覆盖 |
| R5 | 归一后 `Read` 从「免审批」误变「需审批」 | 必须同时满足：`workspace` 已注册（否则落 `mcp__` 兜底 `permission/mod.rs:77`）；且 `Read` 不在需审批精确名分支。需具名断言锁定 |
| R6 | `prompt_declaration` / `namespace` 形状与 `builtin_mcp_test.rs` 的按实例断言冲突 | Q3/Q4 先取证，再按实例扩写断言（wave 2 的 C-01 先例） |
| R7 | 多 writer 冲突（本波文件高度重叠） | §4 的批次划分按「文件不相交」切分；同一文件不得两个并行写者 |
| R8 | 把 AW3-04 的 capability root 缺口写成「已落地」 | 验收记录强制 UNVERIFIED 口径；`docs/reference/mcp-ecosystem.md` 的既有「未验证声明」段（`:738`）同步 |

---

## §7 交付门禁

> **实施回填（2026-09-28）**：九项的逐条判定与证据指针见 `spec/issues/2026-09-27-mcp-adaptation-v4-part-4-acceptance.md` §8（交付门禁核对表，9 项逐条 ✅；全量门禁四段的终态复跑见同文 §3.10 ③）。按本文件约定，本节不记录某一次执行的勾选状态。

- [ ] `workspace` 进 `BUILTIN_MCP_INSTANCES`，`find("workspace")` 命中，overlay 自动注入 5 个实例（web/artifact/cron/lsp/workspace）
- [ ] 首个 LLM 请求直连表含 7 个 `mcp__workspace__*`、不含 7 个裸名
- [ ] 搜索面逐工具 XOR（裸名恰不命中、effective name 恰命中）
- [ ] N1–N8 全部闭合且有反例实验证据
- [ ] `MIDDLEWARE_NAMES` 无 `FilesystemMiddleware`/`TerminalMiddleware`；`BUILTIN_INSTANCE_POLICY_KEYS` 含 `WorkspaceMiddleware`；`MIDDLEWARE_TOOL_NAMES` 无 7 裸名
- [ ] 全量门禁四段绿
- [ ] D2 真实二进制 print + TUI 双通道证据
- [ ] 验收记录完成，S1–S5 与 UNVERIFIED 清单齐备
- [ ] 文档面（C4）同步完成
