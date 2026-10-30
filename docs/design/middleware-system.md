# Middleware 系统设计

> 状态：现行设计
>
> 链序事实源为 `peri-agent/src/session/factory.rs::production_blueprint`，装配实现为
> `peri-middlewares/src/assembly.rs`；强制顺序契约见 ARC-MIDDLEWARE-001。

## 1. 设计原则

1. **切面即中间件**：一个切面就是一个中间件。每个切面封装一个独立的外部能力——hook 实现 + 工具声明 + System Prompt 贡献的自包含单元。不再有「中间件容器套切面」的嵌套结构。
2. **单一职责**：每个切面只做一件事。后台任务的统一操作入口见 [Session 异步任务架构](session-async-tasks.md)。
3. **声明式注册**：切面通过声明方式注册 hooks、tools、依赖关系。声明在编译期可检查完整性。
4. **固定顺序，不可重排**：切面在链中的排列顺序是契约。同一 hook 点上多个切面按声明顺序执行。[TRAP]
5. **State 为唯一共享上下文**：切面间不直接通信——所有状态变更通过 State 传递。

---

## 2. 总体架构

链由 `production_blueprint()` 的槽位按声明顺序装配，每个槽位构造一个中间件实例（Hook 槽位按非空 hook group 展开多实例）。ReAct 循环在固定检查点触发对应 hook；Compact 属 `peri-agent` 的 stages 层，不在链内。槽位顺序与挂载点见 §3，hook 契约见 `peri-agent/src/middleware/trait.rs`。

### 2.1 切面（中间件）——能力的封装单元

一个切面是单一能力的声明。它即是一个中间件：

| 声明项 | 说明 |
|--------|------|
| **Hook 挂载** | 声明在哪些生命周期点执行逻辑。hook 事实源为 `peri-agent/src/middleware/trait.rs`，执行顺序由 `peri-agent/src/middleware/chain.rs` 与 stages 中的 runner 锁定；设计文档不复制动态 hook inventory |
| **工具声明** | 声明提供哪些工具，Executor 统一收集注册 |
| **System Prompt 贡献** | 声明需要追加到 System Prompt 的文本片段 |
| **条件守卫** | 声明注册条件（如「仅当权限模式非 Bypass 时注册」） |

切面可以不挂载任何 hook——纯工具提供者（如 `HumanInTheLoopMiddleware`，只声明 `AskUserQuestion` 工具与 `12_ask_user` 段落）零 hook。

### 2.2 执行模型

链是扁平的——直接遍历切面：

```
Hook 触发（如 before_model）
  → 遍历链中的切面（声明顺序）
    → 执行切面在该 hook 上的挂载逻辑
```

- 遇错即停——后续切面跳过，错误向上传播
- 批量处理（before_tools_batch）：切面声明支持批量模式时 Engine 自动批量化，否则退化为逐条调用；批量返回值必须与输入等长，长度不符时链 fail closed（把本次仍未拒绝的调用置为 `MiddlewareError` 并停止后续切面）

### 2.3 工具收集

Executor 遍历链中所有切面的 `collect_tools` 声明，统一收集：

```
for aspect in chain:
  for tool in aspect.collect_tools(cwd):
    collect(tool)
```

- 工具工厂接收 cwd 参数动态创建
- `collect_tools` 阶段按 production chain 顺序收集；每个 fresh stage 的
  `build_session_tool_view` 将其合并进 session-local `BTreeMap`，同名 middleware
  工具按后注册覆盖，宿主级 `shared_tools` 不被改写
- `AskUserQuestion` 与其他 middleware 工具一致，由
  `HumanInTheLoopMiddleware::collect_tools()` 提供；关闭该 middleware 后即从
  当前 session-local 工具视图消失

---

## 3. 切面注册表

生产蓝本即 `MIDDLEWARE_NAMES` 的 17 个槽位（`ChainSlot` 与之一一对应）；Hook 槽位可按非空 hook group 展开为多个实例，MCP / Goal 槽位还受运行时依赖约束。顺序事实源是
`peri-agent/src/session/factory.rs::production_blueprint`：

> 注：Compact 不在中间件链中，由 `peri-agent::agent::stages::compact` 在 ReAct 循环内处理。详见 §6。

| # | 中间件 | 挂载 Hook | 提供工具 | Prompt 贡献 | 条件 |
|---|------|----------|---------|------------|------|
| 1 | DefaultSystemPromptMiddleware | — | — | 基础 prompt sections | — |
| 2 | LangMiddleware | — | — | language section | — |
| 3 | AgentsMdMiddleware | before_agent | — | CLAUDE.md contribution（正文来自 `peri-instruction://workspace/{main\|local}` 冻结快照；纯 adapter、零 FS） | — |
| 4 | PluginMiddleware | before_agent | — | — | — |
| 5 | SkillsMiddleware | before_agent | Skill/DiscoverSkills | Skills contribution（`McpSkillRegistry` 投影；正文经统一 activation 读 MCP，宿主零 FS） | — |
| 6 | SkillPreloadMiddleware | before_agent, before_input | — | — | — |
| 7 | AtMentionMiddleware | before_input | — | — | — |
| 8 | ImageMiddleware | before_input | — | — | — |
| 9 | GitAttributionMiddleware | before_agent, before_tool, after_tool | — | Git Attribution contribution | — |
| 10 | TodoMiddleware | after_agent | TodoWrite | — | — |
| 11 | HookMiddleware | before_agent, before_tool, after_tool, after_tools_batch, after_agent | — | — | 非空 hook groups；可展开多实例 |
| 12 | PermissionMiddleware | before_tools_batch, before_tool | — | 10_hitl section | — |
| 13 | HumanInTheLoopMiddleware | — | AskUserQuestion | 12_ask_user section | — |
| 14 | SubAgentMiddleware | before_agent, before_reason_catalog | Agent（+AgentResultTool） | 11_subagent section | — |
| 15 | McpMiddleware | before_agent, before_model, before_reason_catalog, before_react_start | MCP 工具（动态）；builtin 实例的 direct 工具 | — | mcp_pool 非空 |
| 16 | `SearchMiddleware` 槽位（类型 `ToolSearchMiddleware`，关闭键 `ToolSearch`） | before_agent, before_reason_catalog | SearchExtraTools/ExecuteExtraTool | deferred inventory + direct declarations contribution | 默认装配 |
| 17 | GoalMiddleware | after_agent | Goal（deferred） | — | goal_controller 非空 |

**脚注**：
- **#4 plugin**：`PluginMiddleware` 在 `before_agent` hook 中执行插件兼容性校验（name/version/manifest 字段完整性）。
- **#9 git_attribution**：`before_tool` 暂存 Write/Edit 旧文件内容，`after_tool` 计算贡献字符数；`prompt_contribution()` 声明 Co-Authored-By 指令。
- **#12/#13**：审批与提问是独立能力。`PermissionMiddleware` 负责审批；`HumanInTheLoopMiddleware::collect_tools()` 使用原始 broker 提供 `AskUserQuestion`。
- **#14 subagent**：`SubAgentMiddleware` 提供 `Agent`，TaskManager 可用时额外提供 `AgentResultTool`；后台任务生命周期遵循 [Session 异步任务架构](session-async-tasks.md)。
- **#17 goal**：`GoalTool` 是 deferred tool，仅通过 `SearchExtraTools` → `ExecuteExtraTool` 访问；`after_agent` 注入 steering 并触发自驱续跑。
- **builtin MCP 实例不是链槽位**：Web 搜索 / 抓取、artifact 上传、cron 调度、7 个文件/终端工具与 workflow 编排由 `mcp` 槽位（#15）客户端侧的同进程 builtin MCP 实例（`web` / `artifact` / `cron` / `workspace` / `workflow`）提供；实例、原始工具名与逐工具 direct 的单一事实源是 `peri-acp-types/src/builtin_mcp.rs::BUILTIN_MCP_INSTANCES`。关闭键为策略键 `WebMiddleware` / `ArtifactMiddleware` / `CronMiddleware` / `WorkspaceMiddleware` / `WorkflowMiddleware`（`BUILTIN_INSTANCE_POLICY_KEYS`，与只含链槽位名的 `MIDDLEWARE_NAMES` 是两张表），机制见 [meta-harness.md](meta-harness.md)。

---

## 4. 顺序约束

链中切面的排列顺序即依赖关系。不需要单独的 `requires` 字段——排在前面的先执行，排在后面的依赖前面的产出：

- `AgentsMdMiddleware` 排在 `PermissionMiddleware` 之前——先注入上下文，再审批
- `PermissionMiddleware` 排在 `SubAgentMiddleware` 之前——其 `before_tools_batch` 在 Agent 工具实际执行前完成审批
- 信息注入切面（AgentsMd、Skills）排在 `GoalMiddleware` 之前——goal 追踪需要上下文已就绪
- 工具提供切面（Todo、Mcp 等）排在 `ToolSearchMiddleware` 之前——工具索引需完整工具列表

顺序即契约。新增切面时按依赖关系插入对应位置。

---

## 5. System Prompt 贡献

切面通过声明替代 prepend_message 模式：

- 切面声明 `prompt_contribution()` 文本片段（返回 `Option<String>`）
- frozen/base system prompt 在 session snapshot 中保持不变
- 主 Agent 的 `AgentModelBridge` 在每个 `ModelRequest` 构造时，从与
  `StageContext` 共享的同一 `Arc<MiddlewareChain>` 同步收集一次当前贡献
- 非空贡献按 `base + "\n\n" + contributions` 只追加到当次请求；空贡献不改变 base
- `before_agent` 先于首个 Reason，因此 ToolSearch 的首轮 cache 已就绪；
  provider 返回 owned `String`，不会持有锁跨越模型 await
- 不再进入 `state.messages`

**当前贡献者列表**（`prompt_contribution()` 返回 `Some` 的中间件）：
| 中间件 | 贡献内容 |
|--------|---------|
| AgentsMdMiddleware (#3) | CLAUDE.md 摘要 |
| SkillsMiddleware (#5) | Skills 摘要 |
| GitAttributionMiddleware (#9) | Co-Authored-By 指令 |
| ToolSearchMiddleware (#16) | 当前 deferred inventory 与 direct-tool declarations |

---

## 6. 与其他模块的关系

| 模块 | 关系 |
|------|------|
| **Session** | `session/new` 时决定切面注册集合，链按此构建 |
| **ReAct 循环** | hook 点嵌入 ReAct 循环固定检查点 |
| **System Prompt** | 切面通过声明贡献，不通过 prepend_message |
| **工具系统** | 切面声明 tools，Executor 统一收集 |
| **Compact** | 不在中间件链中。自动 compact 由 `peri-agent::agent::stages::compact` 在 RCRA 循环中处理；当前策略与回收目标以 `docs/design/micro-compact.md` 和 `ContextPressure::target_reclaim_tokens()` 为事实源 |
| **Plugin** | `PluginMiddleware`（#4）是基础中间件，在 `before_agent` hook 中执行插件兼容性校验。插件扩展的 Skills 经宿主装配把插件根（带 `plugin_name`）并入 `WorkspaceResourcesInput`，由 workspace 资源面提供（`SkillsMiddleware` 不持有插件根）；Hooks 通过 `HookMiddleware`（#11，可多实例）注入 |
