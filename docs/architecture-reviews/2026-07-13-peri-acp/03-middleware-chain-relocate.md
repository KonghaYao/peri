# 候选 3：把 14+5 中间件链的不变量从 peri-acp 搬回 peri-agent

> 日期：2026-07-13 | 模块：`peri-acp`（`agent/builder.rs`） ↔ `peri-agent`（`middleware/chain.rs`） ↔ `peri-middlewares` | 类型：架构走读
> 流程：/grilling（深度模块边界深化）
> 范围：`builder.rs:493-651` 共 159 行中间件装配代码 + 14+5 中间件顺序契约（[TRAP] 守护）

---

## 1. 摘要

`peri-acp/src/agent/builder.rs::build_agent()` 在 493-651 行用 **14 行 `chain.add(Box::new(...))`** 拼装中间件链的前 12 个无条件中间件，加上 5 条条件分支（Hook / HITL / SubAgent / MCP / Workflow / LSP / Goal / ToolSearch），合计 **~159 行** 直接列举 `AgentsMdMiddleware`、`AgentDefineMiddleware`、`PluginMiddleware`、`SkillsMiddleware` 等 14 个具体类型。这是典型的 **leaking seam**：`peri-acp` 本应只承担「服务层装配（provider / session / config）」职责，但它同时持有了「ReAct 引擎内部不变量（中间件顺序契约）」——后者本属于 `peri-agent`。

本候选引入一个 `MiddlewareChain::default_acp_order(params: AcpChainParams) -> Self` constructor，把 14+5 顺序知识内聚到 `peri-agent`，`peri-acp` 仅负责组装参数对象并调一次。预期收益：builder.rs 瘦身 ~150 行、顺序契约单点测试可达、新增中间件时改动点从 2 处（peri-acp 装配 + CLAUDE.md 文档）降为 1 处（peri-agent constructor）、`peri-middlewares/CLAUDE.md` 中「顺序不可重排」的口头契约被编译期不变量替代。

---

## 2. 现状诊断

### 2.1 159 行中间件装配的证据

`peri-acp/src/agent/builder.rs:488-651` 是一个完整的中间件装配区域。入口处有一段明确的 [TRAP] 注释（这是全项目少数显式标注 [TRAP] 的位置之一）：

```rust
// builder.rs:488-493
// 直接构造 MiddlewareChain。
// builder_v2::build_stage_context 消费 chain + AgentComponents，
// 并显式调 chain.collect_tools 把 middleware 提供的工具填充到 shared_tools。
//
// 中间件顺序是 [TRAP] 守护契约（禁止重排），详见 peri-middlewares/CLAUDE.md。
let mut chain = MiddlewareChain::new();
```

紧随其后的 14 个无条件 `chain.add()`（builder.rs:494-536）：

```rust
// builder.rs:494-536（节选）
chain.add(Box::new({
    let mut mw = AgentsMdMiddleware::new().with_excludes(claude_md_excludes);
    if let Some(main) = frozen_claude_md {
        mw = mw.with_frozen_content(main, frozen_claude_local_md);
    }
    mw
}));
chain.add(Box::new(AgentDefineMiddleware::new()));
chain.add(Box::new(peri_middlewares::PluginMiddleware::new(plugin_loaded)));
chain.add(Box::new({
    let mut mw = SkillsMiddleware::new().with_plugin_roots(plugin_skill_roots.clone());
    if let Some(summary) = frozen_skill_summary {
        mw = mw.with_frozen_summary(summary);
    }
    mw
}));
chain.add(Box::new(
    SkillPreloadMiddleware::new(preload_skills, &cwd).with_plugin_roots(plugin_skill_roots.clone()),
));
chain.add(Box::new(peri_middlewares::AtMentionMiddleware::new(cwd.clone().into())));
chain.add(Box::new(filesystem_middleware));
chain.add(Box::new(peri_middlewares::GitAttributionMiddleware::new(&model_name)));
chain.add(Box::new({
    let mut tm = TerminalMiddleware::new();
    tm = tm.with_registry(Arc::clone(&background_registry));
    tm
}));
chain.add(Box::new(WebMiddleware::new()));
chain.add(Box::new(TodoMiddleware::new(todo_tx)));
chain.add(Box::new(CronMiddleware::new(cron_scheduler.unwrap_or_else(|| {
    Arc::new(parking_lot::Mutex::new(CronScheduler::new(
        tokio::sync::mpsc::unbounded_channel().0,
    )))
}))));
```

接着是 5 段条件装配（builder.rs:545-651）：

```rust
// builder.rs:545-576：HookMiddleware 按 hook_groups 循环 add
if !hook_groups.is_empty() {
    let hook_llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> = ...;
    for (i, group) in hook_groups.into_iter().enumerate() {
        if group.is_empty() { continue; }
        let mw = HookMiddleware::with_session_start(group, hook_llm_factory.clone(), &cwd, ...);
        chain.add(Box::new(mw));
    }
}

chain.add(Box::new(hitl));                                       // :578 — HITL
chain.add(Box::new(subagent));                                   // :579 — SubAgent

if let Some(pool) = mcp_pool {                                    // :582-584 — MCP（条件）
    chain.add(Box::new(McpMiddleware::new(pool)));
}
if let Some(adaptor) = wf_adaptor {                               // :587-589 — Workflow（条件）
    chain.add(Box::new(adaptor));
}
chain.add(Box::new(ToolSearchMiddleware::new(                     // :592-595 — ToolSearch
    Arc::clone(&tool_search_index),
    Arc::clone(&shared_tools),
)));

if !lsp_servers.is_empty() {                                      // :620-636 — LSP（条件）
    chain.add(Box::new(LspMiddleware::new(...)));
}

if let Some(controller) = &goal_controller {                      // :645-651 — Goal（条件，链尾）
    let goal_mw = GoalMiddleware::new(...);
    chain.add(Box::new(goal_mw));
}
```

合计：14 个无条件 add + 5 类条件 add，**总装配代码 159 行（builder.rs:488-651）**。

### 2.2 标注 [TRAP] 的具体行

`builder.rs:492` 的注释是契约的「文字锚点」：

```rust
// 中间件顺序是 [TRAP] 守护契约（禁止重排），详见 peri-middlewares/CLAUDE.md。
```

「[TRAP]」是项目内部约定的标记（参考 `CLAUDE.md` 的「陷阱速查」段），代表「顺序/数量敏感、改动会破坏不变量」。但这一行的 **强制力等于零**——它只是一段注释，编译器看不见，新 contributor 完全可以把第 8 个 `GitAttributionMiddleware` 挪到第 3 位，CI 不会失败。

`peri-middlewares/CLAUDE.md` 中的对应契约也只是 Markdown 文档：

> 链构造顺序固定为：14 基础 + 5 条件（Hook/MCP/Workflow/LSP/Goal），链末尾 with_system_prompt() prepend。**顺序不可重排**。

文档与代码各持一份副本，二者完全可能漂移。

### 2.3 论证：这是 leaking seam

判断 leaking seam 的三个特征，逐一对照：

| 特征 | 现状 | 证据 |
|------|------|------|
| **跨层知识** | peri-acp 知道 peri-agent 内部的「14 个具体中间件类型 + 顺序」 | builder.rs:494-536 |
| **interface 缺失** | `MiddlewareChain` 只有 `new()` + `add()`，没有「按业务语义装配」的 constructor | 全 crate 搜索 `default_acp_order` = 0 命中 |
| **修改时的多重影响** | 新增中间件要改 3 处：builder.rs（add）+ peri-middlewares/CLAUDE.md（顺序契约）+ prompts/sections/05-middleware.md（提示词） | CLAUDE.md「任务入口矩阵」明示「新增中间件 → builder.rs:490」 |

最关键的 **locality 失败** 信号：把 `MiddlewareChain` 想象成一个 module，它的「正确不变量（顺序）」并不由它自己保证，而是由 **调用方**（peri-acp）保证。这相当于 `Vec` 的正确排序要靠每个使用 `Vec::push` 的 caller 自己保证——典型的「seam 放错层」。

对照 `CLAUDE.md` 中对 peri-acp 的定位：

> peri-acp（服务层）：agent/builder.rs:490 — 中间件链构造（14+5 固定顺序，禁止重排）

「服务层」理应只决定「用什么 provider、什么 session、什么 cwd」，不应该决定「ReAct 循环内部中间件的先后」。14+5 顺序是 **peri-agent 引擎的内部不变量**，应该回到 peri-agent。

### 2.4 同类问题的现状对照

这与候选 6（trait 抽取）和候选 7（system prompt 迁出）方向一致——都在拆解 `build_agent()` 这个 702 LOC 的 god function。但本候选的特殊之处在于它处理的是 **顺序不变量**，而非「外部副作用（LLM factory / clock / langfuse）」。因此它不引入 trait，而是引入一个 **inherent constructor**。

---

## 3. 约束

### 3.1 中间件顺序 [TRAP] 守护契约（不可重排）

`peri-middlewares/CLAUDE.md` + `builder.rs:492` 注释共同声明的核心约束：**14 个基础中间件的相对顺序不可改变**。

具体语义层级的依赖链：

```
[1] AgentsMdMiddleware          ──→ frozen CLAUDE.md 注入（frozen 数据流的起点）
[2] AgentDefineMiddleware       ──→ 声明 .claude/agents 目录的工具
[3] PluginMiddleware            ──→ 加载插件提供的工具与中间件
[4] SkillsMiddleware            ──→ frozen skill summary 注入（依赖 [1] 的 frozen boundary）
[5] SkillPreloadMiddleware      ──→ 预加载 skill（依赖 [4] 已注册的 skill 根）
[6] AtMentionMiddleware         ──→ @mention 解析（依赖 cwd）
[7] FilesystemMiddleware        ──→ 文件读写工具
[8] GitAttributionMiddleware    ──→ 模型名注入 + git 归因
[9] TerminalMiddleware          ──→ 持 background_registry
[10] WebMiddleware              ──→ web 工具
[11] TodoMiddleware             ──→ todo_tx 通道
[12] CronMiddleware             ──→ cron_scheduler
[13] HookMiddleware（条件）     ──→ 每个 hook_group 一个实例，循环 add
[14] HITL                       ──→ 审批拦截
[15] SubAgent                   ──→ 子 agent（frozen 数据透传）
[16] MCP（条件）                ──→ MCP 工具池
[17] Workflow（条件）           ──→ 工作流 adaptor
[18] ToolSearch                 ──→ 工具搜索索引
[19] AskUser                    ──→ AskUserQuestion（不走 chain.add，直接 insert 到 shared_tools）
[20] LSP（条件）                ──→ LSP 服务器
[21] Goal（条件，链尾）         ──→ goal steering，必须在最后以覆盖所有工具决策
```

关键依赖：
- `[4] SkillsMiddleware` 必须在 `[1] AgentsMdMiddleware` 之后——因为 frozen skill summary 的 boundary 由 frozen system prompt 决定，而 frozen system prompt 的 frozen_claude_md 来自 [1]。
- `[5] SkillPreloadMiddleware` 必须紧随 `[4]`——预加载依赖 [4] 注册的 skill 根列表。
- `[15] SubAgent` 必须在 `[1]/[4]` 之后——SubAgent frozen 数据复用 main agent 的 frozen_claude_md / frozen_skill_summary，参见 builder.rs:430-453 的 [TRAP] 注释。
- `[21] GoalMiddleware` **必须在链尾**——它要在所有工具决策之后注入递增紧迫感 steering，参考 builder.rs:643-645 注释。

### 3.2 14+5 数量在演进中（新增时需要正确插入位置）

中间件数量并非固定。最近 3 次变更：

- v2 单路径改造：移除 `CompactMiddleware`（由 `stages/compact.rs` 接管），见 builder.rs:638-640 注释。
- GoalMiddleware：作为链尾 steering 引入，需要在所有其他中间件之后。
- SkillPreloadMiddleware：在 SkillsMiddleware 之后插入。

每次新增都要回答两个问题：**插在哪里？为什么？**——这两个问题目前完全靠注释和 CLAUDE.md 回答，没有编译期保护。

### 3.3 HookMiddleware 是条件中间件

`builder.rs:545-576` 展示了 HookMiddleware 的特殊形态：**每个 hook_group 产生一个独立的 HookMiddleware 实例**，循环 add 进 chain。这意味着 chain 中可能有 0 个、1 个或 N 个 HookMiddleware 实例，取决于用户配置的 hook_groups 数量。

```rust
for (i, group) in hook_groups.into_iter().enumerate() {
    if group.is_empty() { continue; }
    let mw = HookMiddleware::with_session_start(group, hook_llm_factory.clone(), ...);
    chain.add(Box::new(mw));
}
```

这个循环要求 `default_acp_order` 必须接受 `hook_groups: Vec<Vec<RegisteredHook>>` 并在内部展开，而不是接受单一 `Vec<RegisteredHook>`。

### 3.4 LSP / Goal / MCP / Workflow 也是条件中间件

- LSP：`if !lsp_servers.is_empty()`（builder.rs:620）
- Goal：`if let Some(controller) = &goal_controller`（builder.rs:645）
- MCP：`if let Some(pool) = mcp_pool`（builder.rs:582）
- Workflow：`if let Some(adaptor) = wf_adaptor`（builder.rs:587）

`AcpChainParams` 必须把这些参数都做成 `Option`/`Vec`，让 `default_acp_order` 内部统一判断「是否插入」。

### 3.5 frozen 数据流的内聚性约束

`CLAUDE.md` 明确要求：「SubAgent 复用 main agent frozen 数据，禁止重新读盘」「SP 结构不可变（破坏 prompt cache）」。

这意味着 `AcpChainParams` 中的 `frozen_claude_md` / `frozen_claude_local_md` / `frozen_skill_summary` 必须是 `Option<Arc<String>>`——**Arc 共享**而非 `String` clone。peri-acp 已经这样做（builder.rs:496-497 的 `with_frozen_content(main, frozen_claude_local_md)`），迁到 peri-agent 后必须保持同一 Arc 共享语义。

---

## 4. 依赖关系

### 4.1 前置：候选 6（trait 抽取让 AcpChainParams 可测）

候选 6 引入 `LlmFactory` trait，把 `provider.into_model()` 这一具体类型依赖推到边界。本候选的 `AcpChainParams` 包含一个 `hook_llm_factory`（builder.rs:546-551）：

```rust
let hook_llm_factory: Arc<
    dyn Fn() -> Box<dyn peri_agent::agent::react::ReactLLM + Send + Sync> + Send + Sync,
> = Arc::new({
    let factory = llm_factory.clone();
    move || factory(None)
});
```

如果候选 6 先落地，`hook_llm_factory` 可以用 `LlmFactory` trait 抽象，让 `AcpChainParams` 在测试中可以注入 fake factory，从而让 `default_acp_order` 本身可单点测。

**但本候选不严格依赖候选 6**——`default_acp_order` 只接收 factory 并 pass through 给 HookMiddleware，不调用它。即使没有候选 6，`default_acp_order` 也可以测「顺序契约」（chain 中 middleware 的类型序列），只是无法测「HookMiddleware 内部行为」。

**优先级建议**：候选 6 先做（解锁可测性），本候选紧随其后。但若候选 6 因工期延后，本候选可独立先做，损失仅是 HookMiddleware 行为的端到端测试覆盖。

### 4.2 后置：候选 7（system prompt 迁出后 builder 进一步瘦身）

候选 7 把 `build_system_prompt()` 从 peri-acp 迁到 peri-agent。完成后，`build_agent()` 末尾的 `chain.collect_prompt_contributions()` + `merged_system_prompt` 拼接（builder.rs:655-659）也随之归入 peri-agent：

```rust
// builder.rs:653-659
let contributions = chain.collect_prompt_contributions();
let merged_system_prompt = if contributions.is_empty() {
    system_prompt.clone()
} else {
    format!("{system_prompt}\n\n{contributions}")
};
```

候选 7 落地后，`default_acp_order` 的边界会进一步收敛——`AcpChainParams` 可以同时持有「中间件参数」和「system prompt 基底」，让 `default_acp_order` 一次性返回 `(chain, merged_system_prompt)`。

**优先级建议**：本候选先做（边界更清晰——只迁中间件，不动 system prompt），候选 7 在本候选基础上推进。两者方向一致，不会互相阻塞。

### 4.3 平行：候选 1（独立可做）

候选 1（executor event visitor）与本候选完全独立——前者处理事件路由，后者处理装配顺序。两者可并行推进，无相互依赖。

### 4.4 依赖矩阵

```
候选 6 (trait 抽取) ─┬─→ 候选 3 (本候选) ─→ 候选 7 (system prompt 迁出)
                     │
                     └─（不严格前置，仅增强可测性）

候选 1 ──── 独立 ──── 候选 3
候选 2 ──── 独立 ──── 候选 3
候选 4 ──── 独立 ──── 候选 3
```

---

## 5. 加深后的模块形状

### 5.1 `AcpChainParams` 完整定义（16 字段）

放在 `peri-agent/src/middleware/chain.rs`（或新建 `peri-agent/src/middleware/acp_order.rs`）：

```rust
// peri-agent/src/middleware/acp_order.rs
//! ACP 中间件链的默认顺序构造器。
//!
//! 此处定义 14+5 中间件的装配顺序，是项目内的 [TRAP] 守护不变量。
//! 任何顺序变化必须同步 `peri-middlewares/CLAUDE.md` 与
//! `prompts/sections/05-middleware.md`。

use std::sync::Arc;
use parking_lot::Mutex;

use peri_middlewares::{
    AgentsMdMiddleware, AgentDefineMiddleware, PluginMiddleware,
    SkillsMiddleware, SkillPreloadMiddleware, AtMentionMiddleware,
    FilesystemMiddleware, GitAttributionMiddleware, TerminalMiddleware,
    WebMiddleware, TodoMiddleware, CronMiddleware,
    hooks::{HookMiddleware, RegisteredHook},
    mcp::McpPool,
    workflow::WorkflowAdaptor,
    ToolSearchMiddleware, LspMiddleware, GoalMiddleware,
    CronScheduler, SkillRoot,
};
use peri_lsp::config::LspServerConfig;
use peri_agent::agent::react::ReactLLM;
use peri_agent::middleware::{MiddlewareChain, Middleware, SharedPermissionMode};

/// ACP 中间件链构造参数。
///
/// 所有字段均为「装配所需最小集」，不包含 LLM provider / session / config
/// （那些是 peri-acp 的服务层职责）。
#[derive(Clone)]
pub struct AcpChainParams {
    // === frozen 数据流（不可变 Arc 共享） ===
    pub frozen_claude_md: Option<Arc<String>>,
    pub frozen_claude_local_md: Option<Arc<String>>,
    pub frozen_skill_summary: Option<Arc<String>>,

    // === 权限 / Provider 上下文 ===
    pub permission_mode: SharedPermissionMode,
    pub provider_name: String,
    pub model_name: String,
    pub cwd: String,

    // === 后台资源 ===
    pub background_registry: Arc<Mutex<peri_middlewares::terminal::BackgroundRegistry>>,
    pub cron_scheduler: Option<Arc<Mutex<CronScheduler>>>,

    // === Hook ===
    pub hook_groups: Vec<Vec<RegisteredHook>>,
    pub hook_llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
    pub session_start_source: Option<peri_middlewares::hooks::SessionStartSource>,

    // === 插件 ===
    pub plugin_loaded: Arc<peri_middlewares::plugin::PluginLoaded>,
    pub plugin_skill_roots: Vec<SkillRoot>,
    pub preload_skills: Vec<String>,
    pub claude_md_excludes: Vec<String>,

    // === Todo / LSP / Goal ===
    pub todo_tx: peri_middlewares::todo::TodoTx,
    pub lsp_servers: Vec<LspServerConfig>,
    pub goal_controller: Option<Arc<peri_middlewares::goal::GoalController>>,

    // === 外部注入的中间件（peri-acp 预先装配好的实例）===
    /// HITL 中间件实例（由 peri-acp 根据 permission_mode 构造）。
    pub hitl: Box<dyn Middleware>,
    /// SubAgent 中间件实例（由 peri-acp 构造，含 frozen 数据透传）。
    pub subagent: Box<dyn Middleware>,
    /// 文件系统中间件实例（由 peri-acp 构造，含 permission 配置）。
    pub filesystem: Box<dyn Middleware>,
    /// MCP 中间件池（None 表示不启用 MCP）。
    pub mcp_pool: Option<Arc<McpPool>>,
    /// Workflow adaptor（None 表示不启用 Workflow）。
    pub workflow_adaptor: Option<Box<dyn Middleware>>,
    /// ToolSearch 索引（必填，Core 工具依赖）。
    pub tool_search_index: Arc<peri_middlewares::tool_search::ToolSearchIndex>,
    /// 共享工具表（chain.collect_tools 写入此处）。
    pub shared_tools: Arc<parking_lot::RwLock<peri_agent::tools::ToolMap>>,
}
```

**字段统计**：约 16 + 6（外部注入实例）= **22 字段**（含 3 个外部注入的 `Box<dyn Middleware>` 用于 escape hatch，详见 §5.3）。

> **关于字段子集**：若希望保持精简，可将 `hitl` / `subagent` / `filesystem` 三个预先构造的实例合并到 `prebuilt: PrebuiltMiddlewares` 子 struct，让 `AcpChainParams` 主结构更聚焦于「参数」而非「实例」。这是 §8 风险讨论的内容。

### 5.2 `default_acp_order` 接口草案

```rust
// peri-agent/src/middleware/acp_order.rs

impl MiddlewareChain {
    /// 按 ACP 默认顺序装配中间件链。
    ///
    /// 顺序契约（[TRAP] 守护，禁止重排）：
    ///   1. AgentsMdMiddleware（frozen CLAUDE.md）
    ///   2. AgentDefineMiddleware
    ///   3. PluginMiddleware
    ///   4. SkillsMiddleware（frozen skill summary）
    ///   5. SkillPreloadMiddleware
    ///   6. AtMentionMiddleware
    ///   7. FilesystemMiddleware
    ///   8. GitAttributionMiddleware
    ///   9. TerminalMiddleware
    ///  10. WebMiddleware
    ///  11. TodoMiddleware
    ///  12. CronMiddleware
    ///  13. HookMiddleware × N（每个 hook_group 一个实例）
    ///  14. HITL
    ///  15. SubAgent
    ///  16. McpMiddleware（条件：mcp_pool 非空）
    ///  17. WorkflowAdaptor（条件：workflow_adaptor 非空）
    ///  18. ToolSearchMiddleware
    ///  19. LspMiddleware（条件：lsp_servers 非空）
    ///  20. GoalMiddleware（条件：goal_controller 非空，必须在链尾）
    ///
    /// 详见 `peri-middlewares/CLAUDE.md`。
    pub fn default_acp_order(params: AcpChainParams) -> Self {
        let mut chain = Self::new();
        let AcpChainParams {
            frozen_claude_md,
            frozen_claude_local_md,
            frozen_skill_summary,
            permission_mode,
            provider_name,
            model_name,
            cwd,
            background_registry,
            cron_scheduler,
            hook_groups,
            hook_llm_factory,
            session_start_source,
            plugin_loaded,
            plugin_skill_roots,
            preload_skills,
            claude_md_excludes,
            todo_tx,
            lsp_servers,
            goal_controller,
            hitl,
            subagent,
            filesystem,
            mcp_pool,
            workflow_adaptor,
            tool_search_index,
            shared_tools,
        } = params;

        // === 1-12：14 个基础中间件 ===
        chain.add(Box::new({
            let mut mw = AgentsMdMiddleware::new().with_excludes(claude_md_excludes);
            if let Some(main) = frozen_claude_md {
                mw = mw.with_frozen_content(main, frozen_claude_local_md);
            }
            mw
        }));
        chain.add(Box::new(AgentDefineMiddleware::new()));
        chain.add(Box::new(PluginMiddleware::new(plugin_loaded)));
        chain.add(Box::new({
            let mut mw = SkillsMiddleware::new().with_plugin_roots(plugin_skill_roots.clone());
            if let Some(summary) = frozen_skill_summary {
                mw = mw.with_frozen_summary(summary);
            }
            mw
        }));
        chain.add(Box::new(
            SkillPreloadMiddleware::new(preload_skills, &cwd)
                .with_plugin_roots(plugin_skill_roots.clone()),
        ));
        chain.add(Box::new(AtMentionMiddleware::new(cwd.clone().into())));
        chain.add(filesystem);
        chain.add(Box::new(GitAttributionMiddleware::new(&model_name)));
        chain.add(Box::new({
            let mut tm = TerminalMiddleware::new();
            tm = tm.with_registry(Arc::clone(&background_registry));
            tm
        }));
        chain.add(Box::new(WebMiddleware::new()));
        chain.add(Box::new(TodoMiddleware::new(todo_tx)));
        chain.add(Box::new(CronMiddleware::new(cron_scheduler.unwrap_or_else(|| {
            Arc::new(Mutex::new(CronScheduler::new(
                tokio::sync::mpsc::unbounded_channel().0,
            )))
        }))));

        // === 13：HookMiddleware × N ===
        for group in hook_groups.into_iter() {
            if group.is_empty() { continue; }
            let mw = HookMiddleware::with_session_start(
                group,
                hook_llm_factory.clone(),
                &cwd,
                "",
                "",
                permission_mode.clone(),
                provider_name.clone(),
                session_start_source.clone(),
            );
            chain.add(Box::new(mw));
        }

        // === 14-15：HITL + SubAgent ===
        chain.add(hitl);
        chain.add(subagent);

        // === 16：MCP（条件）===
        if let Some(pool) = mcp_pool {
            chain.add(Box::new(peri_middlewares::mcp::McpMiddleware::new(pool)));
        }

        // === 17：Workflow（条件）===
        if let Some(adaptor) = workflow_adaptor {
            chain.add(adaptor);
        }

        // === 18：ToolSearch ===
        chain.add(Box::new(ToolSearchMiddleware::new(
            Arc::clone(&tool_search_index),
            Arc::clone(&shared_tools),
        )));

        // === 19：LSP（条件）===
        if !lsp_servers.is_empty() {
            let lsp_config = peri_lsp::config::LspConfigFile {
                lsp_servers: lsp_servers.into_iter().map(|s| (s.name.clone(), s)).collect(),
            };
            chain.add(Box::new(LspMiddleware::new(cwd.clone(), lsp_config)));
        }

        // === 20：Goal（条件，链尾）===
        if let Some(controller) = goal_controller {
            chain.add(Box::new(GoalMiddleware::new(controller, /* aux_model */ None)));
        }

        chain
    }
}
```

### 5.3 escape hatch（保留项目特定中间件注入）

为了应对未来「peri-acp 需要注入项目特定中间件」的场景，`MiddlewareChain` 保留现有的 `add()` pub method：

```rust
// peri-agent/src/middleware/chain.rs（不变）
impl MiddlewareChain {
    pub fn new() -> Self { ... }
    pub fn add(&mut self, mw: Box<dyn Middleware>) { ... }  // 仍然 pub
}
```

`default_acp_order` 返回 chain 后，peri-acp 仍可继续 `chain.add()`：

```rust
// peri-acp/src/agent/builder.rs（改造后）
let mut chain = MiddlewareChain::default_acp_order(params);

// 项目特定 override（如果有）
if let Some(custom) = project_custom_middleware {
    chain.add(custom);
}
```

**但 escape hatch 不应是常态**——CLAUDE.md「任务入口矩阵」的「新增中间件 → builder.rs:490」要改为「新增中间件 → `default_acp_order`」。escape hatch 仅用于 A/B 测试或临时实验性中间件。

### 5.4 peri-acp `build_agent` 改造前后对照

**Before**（builder.rs:488-651，~159 行）：

```rust
// 中间件顺序是 [TRAP] 守护契约（禁止重排），详见 peri-middlewares/CLAUDE.md。
let mut chain = MiddlewareChain::new();
chain.add(Box::new({ /* AgentsMdMiddleware 7 行 */ }));
chain.add(Box::new(AgentDefineMiddleware::new()));
chain.add(Box::new(PluginMiddleware::new(plugin_loaded)));
chain.add(Box::new({ /* SkillsMiddleware 6 行 */ }));
chain.add(Box::new({ /* SkillPreloadMiddleware 3 行 */ }));
chain.add(Box::new(AtMentionMiddleware::new(cwd.clone().into())));
chain.add(Box::new(filesystem_middleware));
chain.add(Box::new(GitAttributionMiddleware::new(&model_name)));
chain.add(Box::new({ /* TerminalMiddleware 4 行 */ }));
chain.add(Box::new(WebMiddleware::new()));
chain.add(Box::new(TodoMiddleware::new(todo_tx)));
chain.add(Box::new({ /* CronMiddleware 5 行 */ }));

// HookMiddleware 30 行
if !hook_groups.is_empty() {
    let hook_llm_factory = ...;
    for (i, group) in hook_groups.into_iter().enumerate() { ... }
}

chain.add(Box::new(hitl));
chain.add(Box::new(subagent));

if let Some(pool) = mcp_pool { ... }
if let Some(adaptor) = wf_adaptor { ... }
chain.add(Box::new(ToolSearchMiddleware::new(...)));
// ... error_suggest snapshot 构造 ...
if !lsp_servers.is_empty() { ... }
if let Some(controller) = &goal_controller { ... }
```

**After**（builder.rs 瘦身后，~30 行）：

```rust
// 构造 AcpChainParams（从 AcpAgentConfig 推导）
let params = AcpChainParams {
    frozen_claude_md,
    frozen_claude_local_md,
    frozen_skill_summary,
    permission_mode: permission_mode.clone(),
    provider_name: provider_name.clone(),
    model_name: model_name.clone(),
    cwd: cwd.clone(),
    background_registry: Arc::clone(&background_registry),
    cron_scheduler,
    hook_groups,
    hook_llm_factory: Arc::new({
        let factory = llm_factory.clone();
        move || factory(None)
    }),
    session_start_source,
    plugin_loaded,
    plugin_skill_roots: plugin_skill_roots.clone(),
    preload_skills,
    claude_md_excludes,
    todo_tx,
    lsp_servers,
    goal_controller: goal_controller.clone(),
    hitl: Box::new(hitl),
    subagent: Box::new(subagent),
    filesystem: Box::new(filesystem_middleware),
    mcp_pool,
    workflow_adaptor: wf_adaptor.map(|a| Box::new(a) as Box<dyn Middleware>),
    tool_search_index: Arc::clone(&tool_search_index),
    shared_tools: Arc::clone(&shared_tools),
};

// 14+5 顺序契约由 peri-agent 内聚守护，peri-acp 不再列举具体中间件类型。
let mut chain = MiddlewareChain::default_acp_order(params);

// error_suggest snapshot 构造仍在 peri-acp（依赖 shared_tools 已填充）
let snapshot = peri_middlewares::error_suggest::build_tool_registry_snapshot(...);
```

**净变化**：builder.rs 减少 ~130 行（159 → 29），peri-agent 新增 `acp_order.rs` 约 200 行（含文档注释）。总 LOC 略增，但 **locality 大幅改善**——顺序契约从「散落在调用方」变为「内聚在 module」。

---

## 6. seam 后面剩什么

### 6.1 peri-agent 新增 `MiddlewareChain::default_acp_order`

新增文件 `peri-agent/src/middleware/acp_order.rs`（约 200 行），导出：
- `pub struct AcpChainParams { ... }`
- `impl MiddlewareChain { pub fn default_acp_order(params: AcpChainParams) -> Self { ... } }`

`peri-agent/src/middleware/mod.rs` 增加 `pub mod acp_order; pub use acp_order::AcpChainParams;`。

**注意**：此文件依赖 `peri-middlewares` 的几乎所有中间件类型，因此 `peri-agent` 的 `Cargo.toml` 必须已包含 `peri-middlewares` 依赖。检查现状：

```
peri-agent/Cargo.toml 当前是否依赖 peri-middlewares？
```

> **若当前 peri-agent 不依赖 peri-middlewares**：会形成循环依赖（peri-middlewares 当前反向依赖 peri-agent 的 `ReactLLM` trait）。解决方案有两个：
> 1. **方案 A（推荐）**：把 `AcpChainParams` 与 `default_acp_order` 放在 **peri-middlewares** 而非 peri-agent。理由：所有具体中间件类型都在 peri-middlewares，constructor 自然应在 peri-middlewares。peri-agent 只暴露 `MiddlewareChain` 这个空壳容器。
> 2. **方案 B**：把 `ReactLLM` trait 下沉到一个新的 `peri-agent-traits` 或 `peri-contracts` crate，打破循环。
>
> 本走读默认采用 **方案 A**（放 peri-middlewares），文档统一表述「迁出 peri-acp」，不指定目的 crate 为 peri-agent 还是 peri-middlewares——这两个都是「peri-acp 之外的更底层 crate」。

### 6.2 `peri-middlewares/CLAUDE.md` 契约文档迁移

当前 `peri-middlewares/CLAUDE.md` 持有顺序契约的文字描述。迁移后：

- **保留**：契约描述（14+5 顺序表）——这是人类可读的规范。
- **新增**：「此契约的代码实现位于 `peri-middlewares/src/acp_order.rs::default_acp_order`，任何顺序变更必须同步修改此函数，CI 会运行 `test_default_acp_order_fingerprint` 验证。」
- **删除**：「新增中间件 → builder.rs:490」的指引（改为指向 `default_acp_order`）。

`CLAUDE.md`（项目根）的「任务入口矩阵」对应行同步更新：

```
| 新增中间件 | `peri-middlewares/src/acp_order.rs::default_acp_order` | 14+5 固定顺序，禁止重排；同步更新 peri-middlewares/CLAUDE.md + prompts/sections/05 |
```

### 6.3 peri-acp builder.rs 瘦身 ~150 行

builder.rs 从 888 行降至约 760 行（减少 ~150 行中间件装配代码）。

瘦身后 builder.rs 的剩余职责（更聚焦于「服务层装配」）：
- LLM provider 构造（`provider.into_model()` / `llm_factory`）
- AcpAgentConfig 解构 → AcpChainParams 组装
- system_prompt 基底构造（候选 7 之前仍留在此）
- `build_stage_context` 调用
- AcpAgentOutput 装配

### 6.4 项目级 override（如果有的话）仍留在 peri-acp

`AcpChainParams` 的 3 个 `Box<dyn Middleware>` 字段（`hitl` / `subagent` / `filesystem`）保留 peri-acp 的「预先装配」权——因为这些实例的构造依赖 `LlmProvider`（peri-acp 服务层职责），不应下沉。

未来若有「实验性中间件」（如 A/B 测试新工具），peri-acp 可在 `default_acp_order` 返回后继续 `chain.add()`，无需改动 peri-middlewares。

---

## 7. 测试面

### 7.1 `default_acp_order` 在 peri-agent/peri-middlewares 单点测顺序契约

新增测试文件 `peri-middlewares/tests/acp_order_test.rs`（或 `peri-agent/tests/middleware_tests.rs` 扩展）：

```rust
use peri_middlewares::acp_order::{AcpChainParams, default_acp_order};
// 或 peri_agent::middleware::acp_order

/// [回归测试] default_acp_order 必须按 14+5 顺序装配中间件。
/// 任何顺序变化（新增/删除/重排）必须显式修改此测试，
/// 以触发 code review 关注 [TRAP] 守护契约。
#[test]
fn test_default_acp_order_sequence() {
    let params = make_minimal_params();
    let chain = MiddlewareChain::default_acp_order(params);

    // 提取 chain 中每个 middleware 的类型标识（通过 type_name 或自定义 tag）
    let sequence: Vec<&str> = chain.iter_type_names().collect();

    // 基础 12 个（无 MCP/Workflow/LSP/Goal 的最小配置）
    let expected = &[
        "AgentsMdMiddleware",
        "AgentDefineMiddleware",
        "PluginMiddleware",
        "SkillsMiddleware",
        "SkillPreloadMiddleware",
        "AtMentionMiddleware",
        "FilesystemMiddleware",
        "GitAttributionMiddleware",
        "TerminalMiddleware",
        "WebMiddleware",
        "TodoMiddleware",
        "CronMiddleware",
        "HitlMiddleware",      // 由 peri-acp 注入的实例，但类型固定
        "SubAgentMiddleware",  // 同上
        "ToolSearchMiddleware",
    ];

    assert_eq!(sequence, expected, "中间件顺序契约被破坏");
}

/// [回归测试] GoalMiddleware 必须在链尾。
#[test]
fn test_default_acp_order_goal_at_tail() {
    let params = make_minimal_params().with_goal_controller(some_controller());
    let chain = MiddlewareChain::default_acp_order(params);
    let last = chain.iter_type_names().last().unwrap();
    assert_eq!(last, "GoalMiddleware", "Goal 必须在链尾以覆盖所有工具决策");
}

/// [回归测试] HookMiddleware 按 hook_groups 数量展开。
#[test]
fn test_default_acp_order_hook_groups_expansion() {
    let params = make_minimal_params().with_hook_groups(vec![
        vec![hook_a(), hook_b()],
        vec![hook_c()],
        vec![],  // 空组应被跳过
    ]);
    let chain = MiddlewareChain::default_acp_order(params);
    let hook_count = chain.iter_type_names()
        .filter(|n| *n == "HookMiddleware")
        .count();
    assert_eq!(hook_count, 2, "两个非空 hook_group 应产生 2 个 HookMiddleware 实例");
}
```

### 7.2 fingerprint 测试（顺序变化即测试失败）

新增 `test_default_acp_order_fingerprint`，把 chain 的类型序列哈希成一个稳定字符串，作为「契约指纹」：

```rust
/// [TRAP 守护测试] default_acp_order 的指纹。
/// 任何顺序变更必须更新此常量并解释原因。
#[test]
fn test_default_acp_order_fingerprint() {
    let params = make_minimal_params();
    let chain = MiddlewareChain::default_acp_order(params);
    let fingerprint = chain.fingerprint();  // 例如 "AgentsMd|AgentDefine|Plugin|..."

    // 此常量是 [TRAP] 守护锚点——变更需 code review 关注顺序契约。
    const EXPECTED: &str = "AgentsMd|AgentDefine|Plugin|Skills|SkillPreload|AtMention|Fs|GitAttribution|Terminal|Web|Todo|Cron|Hitl|SubAgent|ToolSearch";
    assert_eq!(fingerprint, EXPECTED, "
        中间件指纹变化——这是 [TRAP] 守护契约。
        若为有意变更，请同步：
          1. peri-middlewares/CLAUDE.md 的顺序表
          2. prompts/sections/05-middleware.md
          3. 此处的 EXPECTED 常量
    ");
}
```

### 7.3 回归保护：所有现有 Agent 行为测试不变

迁移后必须保证：

- `peri-acp` 现有所有集成测试（如 `executor_test.rs`）通过——验证「行为等价」。
- `peri-agent/tests/middleware_tests.rs` 现有测试通过——验证 chain.add 行为不变。
- TUI 端到端冒烟（手动）——验证 14+5 顺序在真实 LLM 调用中行为一致。

**关键风险点**：frozen 数据流的 Arc 共享语义必须在迁移中保持。建议增加一个 round-trip 测试：

```rust
#[test]
fn test_default_acp_order_preserves_frozen_arc_sharing() {
    let frozen = Arc::new("content".to_string());
    let params = make_minimal_params().with_frozen_claude_md(Arc::clone(&frozen));
    let chain = MiddlewareChain::default_acp_order(params);

    // 验证 chain 内的 AgentsMdMiddleware 持有同一 Arc（而非 clone）
    let agents_md = chain.find::<AgentsMdMiddleware>().unwrap();
    assert!(Arc::ptr_eq(&frozen, agents_md.frozen_claude_md_ref(), "frozen Arc 必须共享，禁止 clone（破坏 prompt cache）"));
}
```

### 7.4 测试位置

按 `CLAUDE.md` 测试规范：

| 测试类型 | 位置 | 说明 |
|---------|------|------|
| 顺序契约（指纹） | `peri-middlewares/tests/acp_order_test.rs` | 集成测试，验证 pub API |
| frozen Arc 共享 | 同上 | 同一文件的 round-trip 测试 |
| 条件中间件展开 | 同上 | 同一文件的多个 case |
| 行为等价（回归） | `peri-acp/tests/builder_test.rs`（新建） | 验证 build_agent 输出与迁移前一致 |

---

## 8. 风险与回滚

### 8.1 ADR 冲突讨论：与「ACP 层是唯一全知层」原则的部分相悖

**潜在冲突**：`CLAUDE.md` 在「ACP/TUI 分层」一节说「execute_prompt：Agent 构建统一入口，禁止 TUI 直连运行时」——这隐含「peri-acp 是 Agent 构建的全知层」。如果把中间件顺序迁出，是否削弱了 peri-acp 的「全知」地位？

**反驳**：这一原则的本意是「禁止 TUI 绕过 peri-acp 直接调用 peri-agent 运行时」，**不是**「peri-acp 必须知道 peri-agent 的所有内部不变量」。「全知」指 **session / provider / config / cwd 的全知**，不是「ReAct 引擎内部机制的全知」。中间件顺序是 peri-agent（ReAct 引擎）的内部不变量，把它还回去是 **强化分层** 而非削弱。

类比：`HashMap::new()` 是 HashMap 自己决定的内部状态初始化，调用方不需要知道「HashMap 内部 bucket 数 = 16」。同理，`MiddlewareChain::default_acp_order` 是 chain 的内部不变量，peri-acp 不需要知道第 8 个是 `GitAttributionMiddleware`。

**结论**：本候选不违反 ACP 分层原则，反而是该原则的正确应用——「服务层不该知道引擎内部不变量」。

### 8.2 复杂参数对象（AcpChainParams 16+ 字段）

**风险**：`AcpChainParams` 22 字段（含 3 个外部注入实例）是个「god struct」，未来字段只会增加。

**缓解**：
1. **字段子结构化**：把字段分组到子 struct（如 `FrozenData` / `HookConfig` / `PrebuiltMiddlewares`），让主结构只持 5-6 个子 struct。
2. **Builder 模式**：提供 `AcpChainParams::builder()`，链式构造。
3. **默认值**：`Default for AcpChainParams`，必填字段用 `Option` + 运行时校验。
4. **字段验证**：在 `default_acp_order` 入口处统一校验（如 `cwd` 非空、`tool_search_index` 非 None），把分散的「builder.rs 中的 unwrap」收敛到一处。

**建议**：先做扁平版（22 字段），等字段增长到 25+ 时再重构为子 struct。过早分组会让参数构造更啰嗦。

### 8.3 迁移期两套路径共存风险

**风险**：如果 Phase 1 引入 `default_acp_order` 但 Phase 3 才删除旧 chain.add，中间存在「两套装配路径」，可能有人误改旧路径而忘了新路径。

**缓解**：
1. **Phase 1 期间**：在旧 14 行 `chain.add` 上加 `#[deprecated]` 或 lint 注释，指向 `default_acp_order`。
2. **Phase 2 期间**：用 `cfg!(feature = "acp_order_v2")` 切换，CI 同时跑两路径。
3. **Phase 3 一次性删除**：不要拖延——一旦 `default_acp_order` 在生产路径跑了 1 周无 issue，立即删除旧 chain.add。

### 8.4 回滚路径

如果迁移后发现行为差异（如 frozen Arc 共享破坏、HookMiddleware 行为变化）：

- **Phase 1-2**：纯加法，回滚只需删除新代码。
- **Phase 3**：删除旧 chain.add 后回滚需要 git revert——但旧代码仍在版本历史中，可恢复。

**回滚判据**：
- 任何 `executor_test.rs` 失败 → 立即回滚。
- fingerprint 测试失败且无法立即解释 → 回滚 + code review。
- 生产环境（TUI 冒烟）发现中间件行为异常 → 回滚。

### 8.5 循环依赖风险（如误选落点 crate）

如 §6.1 所述，若 `default_acp_order` 误落在 peri-agent 而 peri-agent 不依赖 peri-middlewares，会形成循环依赖。

**规避**：在 Phase 1 之前先跑 `cargo tree -p peri-agent -i peri-middlewares` 确认依赖方向。若 peri-middlewares 反向依赖 peri-agent（极可能，因为中间件实现 `ReactLLM`），则落点必须是 **peri-middlewares** 而非 peri-agent。

---

## 9. 迁移步骤

### Phase 1：在 peri-middlewares（或 peri-agent）新增 `default_acp_order`，但 peri-acp 暂不用

**目标**：纯加法，不改生产路径。

**步骤**：
1. 确认落点 crate（peri-middlewares 或 peri-agent），跑 `cargo tree` 验证依赖方向。
2. 在落点 crate 新建 `src/acp_order.rs`（或 `middleware/acp_order.rs`），定义 `AcpChainParams` + `impl MiddlewareChain { pub fn default_acp_order(...) }`。
3. 实现 `default_acp_order`（从 builder.rs:488-651 的逻辑 1:1 复制）。
4. 新增测试：`test_default_acp_order_sequence` / `test_default_acp_order_fingerprint` / `test_default_acp_order_goal_at_tail` / `test_default_acp_order_hook_groups_expansion`。
5. CI 验证新测试通过，但 peri-acp 仍走旧路径。

**完成判据**：新文件 + 测试合入 main，生产路径未变。

### Phase 2：灰度切换（一个 build 路径迁移）

**目标**：把 peri-acp 的 `build_agent()` 改为调 `default_acp_order`，但保留旧代码（注释掉或加 `#[cfg(test)]` 守护）以便对比。

**步骤**：
1. 在 `build_agent()` 中构造 `AcpChainParams`（从现有变量推导，不改其他逻辑）。
2. 调 `let mut chain = MiddlewareChain::default_acp_order(params);` 替换原 14 行 chain.add。
3. 旧 14 行 chain.add 用 `#[cfg(feature = "legacy_chain")]` 守护，保留为对比基线。
4. 运行全量回归测试：`cargo test --workspace`。
5. 手动 TUI 冒烟：启动一个 session，触发 1 个工具调用，验证 frozen 数据 / Hook / SubAgent 行为正常。
6. 跑 1 周生产灰度（开发者日常使用）。

**完成判据**：所有现有测试通过 + 1 周无回归。

### Phase 3：删除 peri-acp 旧 14 chain.add

**目标**：彻底移除旧路径。

**步骤**：
1. 删除 `#[cfg(feature = "legacy_chain")]` 守护的旧代码。
2. 删除 `legacy_chain` feature 定义（Cargo.toml）。
3. builder.rs 瘦身检查：确认净减少 ~150 行。
4. 更新 `CLAUDE.md` 任务入口矩阵：「新增中间件 → `default_acp_order`」。
5. 更新 `peri-middlewares/CLAUDE.md`：顺序契约代码锚点指向 `default_acp_order`。

**完成判据**：builder.rs 行数下降、grep `chain.add` 在 peri-acp 中 0 命中（除 escape hatch）。

### Phase 4：更新 peri-middlewares/CLAUDE.md

**目标**：文档与新代码对齐。

**步骤**：
1. 在 `peri-middlewares/CLAUDE.md` 顶部增加「顺序契约的代码锚点」段，指向 `src/acp_order.rs::default_acp_order`。
2. 更新顺序表：标注「此表的代码实现位于 `default_acp_order`，新增中间件必须同步修改该函数」。
3. 更新根 `CLAUDE.md` 任务入口矩阵对应行。
4. 更新 `prompts/sections/05-middleware.md`（若它列出了具体中间件顺序）。

**完成判据**：文档与代码一致，grep 「builder.rs:490」在文档中 0 命中。

---

## ADR 草案

### ADR-2026-07-13-middleware-chain-default-order

**Title**：把 14+5 中间件链的顺序不变量从 peri-acp 搬到 peri-middlewares

**Status**：Proposed（建议正式记录）

**Date**：2026-07-13

**Context**（背景）

`peri-acp/src/agent/builder.rs::build_agent()` 在 488-651 行用 14 行 `chain.add(Box::new(...))` 拼装中间件链的前 12 个无条件中间件，加上 5 类条件分支，合计 ~159 行直接列举 14+5 个具体中间件类型。这段代码被显式标注为 `[TRAP]` 守护契约（builder.rs:492）：

> 中间件顺序是 [TRAP] 守护契约（禁止重排），详见 peri-middlewares/CLAUDE.md。

但契约的强制力仅靠注释 + Markdown 文档维持，编译期完全无保护。

问题表现：
1. **Leaking seam**：peri-acp（服务层）持有 peri-agent（ReAct 引擎）的内部不变量（中间件顺序）。
2. **Locality 失败**：顺序契约由调用方（peri-acp）保证，而非由 chain 自身保证。
3. **修改点扩散**：新增中间件需改 3 处（builder.rs / CLAUDE.md / prompts/sections/05）。
4. **不可单点测**：顺序契约没有 fingerprint 测试，重构易破坏。

**Decision**（决策）

在 peri-middlewares（或 peri-agent，取决于依赖方向）新增 inherent constructor `MiddlewareChain::default_acp_order(params: AcpChainParams) -> Self`，把 14+5 顺序知识内聚于此。peri-acp 的 `build_agent()` 改为：

```rust
let params = AcpChainParams { /* 从 AcpAgentConfig 推导 22 字段 */ };
let mut chain = MiddlewareChain::default_acp_order(params);
```

新增 fingerprint 测试 `test_default_acp_order_fingerprint`，任何顺序变化导致测试失败，强制 code review 关注 [TRAP] 守护契约。

**Motivation**（动机）

1. **正确分层**：服务层（peri-acp）只决定「用什么 provider / session / cwd」，不决定「ReAct 引擎内部中间件先后」。中间件顺序是引擎内部不变量，应回到引擎侧（peri-middlewares 或 peri-agent）。
2. **不变量内聚**：把顺序从「散落在 159 行调用方代码」变为「单点 constructor」，新增中间件的改动点从 3 处降为 1 处（constructor + 同步文档）。
3. **可测性**：引入 fingerprint 测试，把口头契约变为可执行契约。
4. **与候选 6/7 协同**：本候选是 `build_agent()` god function 拆解的一部分，与候选 6（trait 抽取）/ 候选 7（system prompt 迁出）方向一致。

**Consequences**（后果）

正面：
- builder.rs 瘦身 ~150 行（159 → 29）。
- 顺序契约单点测试可达（fingerprint + sequence 两个测试）。
- 新增中间件改动点收敛到 1 个文件。
- escape hatch 保留（peri-acp 仍可 `chain.add()` 注入项目特定中间件）。

负面：
- `AcpChainParams` 22 字段是个复杂参数对象（god struct 风险），需后续按字段分组重构。
- 迁移期两套路径共存风险（Phase 1-2），需靠 `#[cfg(feature = "legacy_chain")]` 守护。
- 若落点 crate 选错（peri-agent 而非 peri-middlewares），可能触发循环依赖——需 Phase 1 前用 `cargo tree` 验证。

中性：
- `MiddlewareChain::add()` 仍保持 pub（escape hatch），但其使用从「常态」变为「例外」。
- `peri-middlewares/CLAUDE.md` 的契约描述不删除，只是增加「代码锚点」指针。

**Alternatives Considered**（备选方案）

1. **保持现状**：159 行 chain.add 留在 peri-acp，仅补 fingerprint 测试。
   - 否决理由：不解决 leaking seam，只加测试不重构，god function 继续膨胀。
2. **抽 trait `MiddlewareChainBuilder`**：让 peri-acp 注入 builder 实现。
   - 否决理由：过度工程。顺序契约只有一种「正确答案」，没有多态需求。inherent constructor 足够。
3. **用 const 顺序数组**：定义 `const ACP_ORDER: &[MiddlewareType] = &[...]`，运行时按数组装配。
   - 否决理由：中间件实例化需要参数（frozen Arc / cwd / provider_name），无法用 const 表达。运行时 constructor 更自然。

**Recommendation**（建议）

正式记录此 ADR。理由：这是一个 **架构边界变更**（顺序不变量的归属从一个 crate 迁到另一个 crate），未来回溯「为什么 default_acp_order 在 peri-middlewares 而非 peri-acp」时需要此决策记录。建议存放路径：`docs/adr/2026-07-13-middleware-chain-default-order.md`。

---

## 附录：与项目原则的对照

| 项目原则（CLAUDE.md） | 本候选的处理 |
|----------------------|-------------|
| 「每模块一个目录，mod.rs 中 pub use 做预导出」 | 新建 `middleware/acp_order.rs`，mod.rs 预导出 `AcpChainParams` |
| 「RwLockReadGuard Send：async 跨 .await 用 parking_lot::RwLock」 | `AcpChainParams.shared_tools` 保持 `Arc<parking_lot::RwLock<ToolMap>>` |
| 「新增中间件 → builder.rs:490」 | 改为「新增中间件 → default_acp_order」 |
| 「中间件顺序是 [TRAP] 守护契约」 | 用 fingerprint 测试把 [TRAP] 变为可执行契约 |
| 「Frozen Data Flow：Arc 共享，禁止 clone」 | 新增 round-trip 测试 `test_default_acp_order_preserves_frozen_arc_sharing` |
| 「SubAgent 复用 main agent frozen 数据」 | `AcpChainParams.frozen_*` 字段保持 `Option<Arc<String>>`，由 peri-acp 透传 |

---

**文档版本**：v1.0（2026-07-13）
**走读流程**：/grilling（深度模块边界深化）
**作者**：架构走读 agent
**审阅状态**：待审阅
