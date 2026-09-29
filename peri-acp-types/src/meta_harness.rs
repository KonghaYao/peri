//! MetaHarness 契约类型与编译期常量。
//!
//! 设计单一事实源：`docs/design/meta-harness-design.md`（§2.1-2.3）。
//! 类型位于跨层契约 crate，同时供 `peri-acp`（settings 校验 / 冻结组装 /
//! 段落覆盖）与 `peri-middlewares`（装配期过滤）使用，避免依赖环。
//!
//! - `SECTION_IDS`：系统提示词段落 ID 清单（`prompts/sections/` 去 `.md` +
//!   渲染生成段 `persona` / `language`），与持有者段落声明（
//!   `peri-middlewares` 的 `DefaultSystemPromptMiddleware` / `LangMiddleware`）
//!   及 `peri-acp/src/prompt/mod.rs` 的 `GATED_SECTIONS` 数组 ID 完全一致
//!   （有测试锁定，见 `prompt_test.rs`）。
//! - `MIDDLEWARE_NAMES`：装配面 middleware 的 `name()` 返回值清单
//!   （顶层链 / Workflow agent 链 / 子链并集），与 blueprint/name 映射
//!   有测试锁定（见 `assembly_test.rs`）。

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

/// 冻结期构建的 MetaHarness 状态，随冻结载体（`FrozenContext`）传播；
/// 会话内不可变（ARC-FROZEN-001）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaHarnessState {
    /// 段落 ID → md 全文；仅含"开关 true 且文档存在"的条目。
    /// 覆盖发生在 `PromptTemplate::new` 构造期，render 无查表开销。
    pub section_overrides: HashMap<String, Arc<str>>,
    /// 装配期关闭的 middleware 名集合（配置中 `false` 条目）。
    pub disabled_middlewares: HashSet<String>,
    /// 是否允许使用 compile-time 内置 subagent definitions。
    ///
    /// 默认开启以保持向后兼容；该值在 session 创建时冻结，catalog 与实际
    /// definition fallback 必须共同遵守。
    pub built_in_subagents_enabled: bool,
}

impl Default for MetaHarnessState {
    fn default() -> Self {
        Self {
            section_overrides: HashMap::new(),
            disabled_middlewares: HashSet::new(),
            built_in_subagents_enabled: true,
        }
    }
}

/// MetaHarness 行为策略键；与 section / middleware 键共享配置 map。
pub const BUILT_IN_SUBAGENTS_KEY: &str = "BuiltInSubagents";

/// 系统提示词段落 ID 清单（`prompts/sections/` 文件名去 `.md`）。
///
/// 波 4 演进（C2/C3）后：基础段（01-06 / 07_runtime / persona / language）
/// 由 `DefaultSystemPromptMiddleware` / `LangMiddleware` 持有，gated 段
/// （10_hitl / 11_subagent / 13_skills）由功能 middleware 持有（见
/// `SECTION_HOLDER_MIDDLEWARE`），15_channel 由 `GATED_SECTIONS` 数组
/// 持有（无持有者，gate 恒 false）；段落 ID 仍是覆盖与持有权迁移的定位
/// 键；`persona` / `language` 为渲染生成段 ID（可经
/// `.peri/meta/persona.md` / `.peri/meta/language.md` 覆盖）。与持有者
/// 段落声明 + `GATED_SECTIONS` 数组 ID 并集完全一致且无重复（由
/// `prompt_test.rs` 锁定）。
///
/// 2026-08-15 职责拆分后：`10_hitl`（审批机制）归 `PermissionMiddleware`，
/// `12_ask_user`（提问纪律）归新 `HumanInTheLoopMiddleware`（持有
/// `AskUserQuestion` 工具）。
pub const SECTION_IDS: &[&str] = &[
    "01_intro",
    "02_system",
    "03_doing_tasks",
    "04_actions",
    "05_using_tools",
    "06_tone_style",
    "07_runtime",
    "10_hitl",
    "11_subagent",
    "12_ask_user",
    "13_skills",
    "15_channel",
    "persona",
    "language",
];

/// 装配面 **链槽位** middleware 名清单（`Middleware::name()` 返回值）。
///
/// 覆盖全部装配入口：顶层链（`assembly.rs` 21 注册点）、Workflow agent 链、
/// SubAgent 子链。`false` 条目键必须在此集合内，否则解析期校验
/// warn 后忽略。
///
/// 2026-08-15 职责拆分：`PermissionMiddleware`（原审批职责，持有
/// `10_hitl`）；`HumanInTheLoopMiddleware` 旧名由新"提问"middleware 接管
/// （持有 `AskUserQuestion` 工具 + `12_ask_user` 段落）。配置键
/// `"HumanInTheLoopMiddleware": false` 语义随之从"关审批"漂移为"关提问"
/// （纯破坏性改名，见 `spec/issues/2026-08-15-permission-hitl-split.md`）。
///
/// **v4-part-2（A7）：本表只含链槽位名**。`WebMiddleware` /
/// `ArtifactMiddleware` 已不是链槽位（Web / Artifact 迁移为 builtin MCP 实例，
/// 链槽位与挂载点删除），它们的 MetaHarness 关闭键改由
/// [`BUILTIN_INSTANCE_POLICY_KEYS`] 承载——两表并集才是「已知 MetaHarness 键
/// 全集」（`assembly_test.rs` 与 `provider/config.rs` 都按该并集判定）。
///
/// **v4-part-3（A8）：`LspMiddleware` → `LspSyncMiddleware`**。LSP 工具面迁到
/// builtin 实例后，原槽位名改由薄同步中间件占用（本表条目随之改名）；
/// `LspMiddleware` 本身成为 builtin 实例的关闭键，落
/// [`BUILTIN_INSTANCE_POLICY_KEYS`]。
///
/// **v4-part-4（W3-C1）：`FilesystemMiddleware` / `TerminalMiddleware` 摘除**。7 个
/// 文件/终端工具（Read / Write / Edit / Glob / Grep / folder_operations / Bash）迁为
/// builtin `workspace` 实例后，两者的链槽位（`ChainSlot::Filesystem` /
/// `ChainSlot::Terminal`）与其 middleware 类型一并删除，关闭键改由
/// `"WorkspaceMiddleware"`（落 [`BUILTIN_INSTANCE_POLICY_KEYS`]）承载——与
/// Web / Artifact / cron / lsp 的既有形态一致。
///
/// **v4 wave 4（Git Watch 下沉）：`GitWatchMiddleware` 摘除**。git ref 变化改由
/// builtin `workspace` 实例的 `workspace://git/ref` 资源 + MCP 2026-07-28 订阅回传
/// （宿主的提醒映射内置，无配置面）。本键因此成为**未知键**：解析期 warn + 忽略
/// （既有判例，同 `FilesystemMiddleware` / `TerminalMiddleware`）；关闭该能力的办法是
/// `"WorkspaceMiddleware": false`（关实例）或实例配置的 `subscriptions` 覆盖（关订阅）。
pub const MIDDLEWARE_NAMES: &[&str] = &[
    "DefaultSystemPromptMiddleware",
    "LangMiddleware",
    "AgentsMdMiddleware",
    "PluginMiddleware",
    "SkillsMiddleware",
    "SkillPreloadMiddleware",
    "AtMentionMiddleware",
    "ImageMiddleware",
    "GitAttributionMiddleware",
    "TodoMiddleware",
    "HookMiddleware",
    "PermissionMiddleware",
    "HumanInTheLoopMiddleware",
    "SubAgentMiddleware",
    "McpMiddleware",
    "WorkflowMiddleware",
    "PtcMiddleware",
    "ToolSearch",
    "LspSyncMiddleware",
    "GoalMiddleware",
];

/// builtin MCP 实例的 MetaHarness 关闭键（A7：与 [`MIDDLEWARE_NAMES`] 分离的第二张表）。
///
/// 每个键对应 [`crate::builtin_mcp::BUILTIN_MCP_INSTANCES`] 中一个实例的
/// `policy_key`（`"WebMiddleware": false` / `"ArtifactMiddleware": false` ⇒ 关闭
/// 对应 builtin 实例的工具面）。这些实例不再是链槽位，但关闭语义**不得因此
/// 消失**（`docs/standards/architecture-contracts.md` 的 ARC-CAPABILITY-CLOSURE-001：
/// 键仍存在却不再生效即能力闭合退化），因此：
///
/// - 解析期校验（`peri-acp/src/provider/config.rs::validate_meta_harness`）的
///   「已知键」集合 = [`SECTION_IDS`] ∪ [`MIDDLEWARE_NAMES`] ∪ 本表 ∪
///   [`BUILT_IN_SUBAGENTS_KEY`]；
/// - 「全部 middleware 关闭」保险丝的判定面 = [`MIDDLEWARE_NAMES`] ∪ 本表
///   （否则「只剩两个 builtin 策略键为 false」不再触发全关告警）。
///
/// 与声明表的对齐由本文件的 `mod tests` 直接断言集合相等（常量漂移或声明表
/// 漂移即红）；`assembly_test.rs` 另断言槽位名与本表交集为空。
///
/// v4-part-4（W3-C1）：`WorkspaceMiddleware` 随 `workspace` 实例加入——7 个文件/终端
/// 工具（Read / Write / Edit / Glob / Grep / folder_operations / Bash）的唯一提供面，
/// 关闭语义与 Web / Artifact 逐位同构。
pub const BUILTIN_INSTANCE_POLICY_KEYS: &[&str] = &[
    "WebMiddleware",
    "ArtifactMiddleware",
    "CronMiddleware",
    "LspMiddleware",
    "WorkspaceMiddleware",
];

/// 段落 → 持有 middleware 名映射表（设计 §3.1.1 拆分持有契约 3）。
///
/// 契约 3（gate 原子迁移，C3 落地）：gated 段落移交给功能 middleware 后，
/// gate 判定从 `PromptFeatures::detect` 硬编码简化为"持有该段的 middleware
/// 是否在链上"。本表是判定映射的事实源（装配期投影经
/// `peri_agent::middleware::project_enabled_sections` 消费；收集机制天然
/// 承担同一判定——能收集到段落即持有者已装配，C1 决策记录 D5）。
///
/// `15_channel` 无对应 middleware（gate 恒 false 直至未来 channel middleware
/// 装配，见 `SECTION_IDS` 注释），未入表；基础段（01-06 / 07_runtime /
/// persona / language）gate = 持有者是否装配，由收集机制天然承担
/// （收集即装配，见 `peri_agent::middleware::prompt_sections`），不入表。
pub const SECTION_HOLDER_MIDDLEWARE: &[(&str, &str)] = &[
    ("10_hitl", "PermissionMiddleware"),
    ("11_subagent", "SubAgentMiddleware"),
    ("12_ask_user", "HumanInTheLoopMiddleware"),
    ("13_skills", "SkillsMiddleware"),
];

/// 全部 middleware 静态工具名并集（`collect_tools`/`build_tools` 返回值）。
///
/// 用途：session/turn 级工具视图剔除"middleware 静态工具名且不在当前链
/// 工具集合"的条目（设计 §2.5 关闭语义的防御面，决策记录见
/// `spec/issues/2026-08-14-meta-harness-tool-view-exclusion.md`）。与各
/// middleware 实现的工具名由 `assembly_test.rs` 锁定一致。
///
/// **事实核查（2026-08-15 更新）**：2026-08-15 职责拆分（`spec/issues/
/// 2026-08-15-permission-hitl-split.md`）后，宿主级共享 registry
/// （`shared_tools`）生产路径写入点归零——`AskUserQuestion` 移入新
/// `HumanInTheLoopMiddleware` 的 `collect_tools`（随关闭而消失），其余
/// middleware 工具（含本清单全部条目与 MCP 动态工具）从不进入共享
/// registry，只经 `chain.collect_tools()` 进入每 turn 重建的本地视图。
/// 因此本清单是纯防御性剔除面：当前永不命中，但若将来注册面变化
/// （middleware 工具写入 shared_tools），清单必须同步扩展——新增
/// middleware 工具名须加入此处。
///
/// MCP 动态 bridge 工具（`mcp__{server}__{tool}`）与 `McpResourceTool`/
/// `DiscoverMCP` 同样不进入共享 registry，无需也无法静态枚举；禁用
/// McpMiddleware 后当前链无 MCP 工具，本地视图天然不含（每 turn 重建），
/// 无跨 session 残留路径。
///
/// **v4-part-2（A7/IF-D7 B 节）：已删除 `WebFetch` / `WebSearch` / `artifact`
/// 三个裸名。** 三者已迁移为 builtin MCP 实例（模型面名字 `mcp__web__WebFetch` /
/// `mcp__web__WebSearch` / `mcp__artifact__artifact`，见
/// [`crate::builtin_mcp::BUILTIN_MCP_INSTANCES`]），以 MCP bridge 形态经
/// `chain.collect_tools()` 进入本地视图，**从不**进入共享 registry，因此不在本
/// 清单内是正确状态。后果（必须显式记录，见 `build_session_tool_view` 文档）：
/// 若非 middleware 路径（plugin / 外部注册）往共享表写入这三个裸名，本防御面
/// **不再**剔除它们——这正是「剔除面只覆盖 middleware 静态工具」的应有语义。
/// builtin 能力的关闭由 builtin 关闭集（`BUILTIN_INSTANCE_POLICY_KEYS`）在装配期
/// 过滤，与本清单无关。
///
/// **v4-part-3（A8/IF-P3-10）：已删除 `LSP` 裸名。** 工具面迁到 builtin 实例
/// （模型面名字 `mcp__lsp__LSP`，见 [`crate::builtin_mcp::BUILTIN_MCP_INSTANCES`]），
/// `ChainSlot::Lsp` 上的 `LspSyncMiddleware` 只做文档同步、`collect_tools` 为空，
/// 因此 LSP 不再是任何 middleware 的静态工具（语义与上一段三个裸名相同）。
///
/// **v4-part-4（W3-C1）：已删除 7 个 workspace 裸名**（`Read` / `Write` / `Edit` /
/// `Glob` / `Grep` / `folder_operations` / `Bash`）。7 个工具迁为 builtin `workspace`
/// 实例（模型面名字 `mcp__workspace__*`，见
/// [`crate::builtin_mcp::BUILTIN_MCP_INSTANCES`]），以 MCP bridge 形态经
/// `chain.collect_tools()` 进入本地视图，**从不**进入共享 registry，因此不在本
/// 清单内是正确状态（与上一段 Web / Artifact 三个裸名同构）。后果同样必须显式
/// 记录：若**非 middleware 路径**（plugin / 外部注册）往共享表写入这 7 个裸名，
/// 本防御面**不再**剔除它们；builtin 能力的关闭由 `BUILTIN_INSTANCE_POLICY_KEYS`
/// 的 `"WorkspaceMiddleware"` 在装配期过滤，与本清单无关。
pub const MIDDLEWARE_TOOL_NAMES: &[&str] = &[
    // SkillsMiddleware
    "SkillTool",
    "DiscoverSkillsTool",
    // HumanInTheLoopMiddleware（新：提问通道）
    "AskUserQuestion",
    // SubAgentMiddleware
    "Agent",
    "AgentResult",
    // WorkflowMiddleware
    "Workflow",
    // TodoMiddleware
    "TodoWrite",
    // ToolSearch
    "ToolSearch",
    "SearchExtraTools",
    "ExecuteExtraTool",
    // GoalMiddleware
    "goal",
    // McpMiddleware（静态部分）
    "DiscoverMCP",
    "mcp_read_resource",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for id in SECTION_IDS {
            assert!(seen.insert(*id), "duplicate section id: {id}");
        }
    }

    #[test]
    fn middleware_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for name in MIDDLEWARE_NAMES {
            assert!(seen.insert(*name), "duplicate middleware name: {name}");
        }
    }

    /// A7 两表形态：`BUILTIN_INSTANCE_POLICY_KEYS` 必须恰为声明表的 `policy_key`
    /// 集合（常量漂移、声明表漂移、重复键三种情况都在此变红）。
    #[test]
    fn builtin_instance_policy_keys_match_declaration_table() {
        let declared: std::collections::HashSet<&str> = crate::builtin_mcp::BUILTIN_MCP_INSTANCES
            .iter()
            .map(|instance| instance.policy_key)
            .collect();
        let constant: std::collections::HashSet<&str> =
            BUILTIN_INSTANCE_POLICY_KEYS.iter().copied().collect();
        assert_eq!(
            constant.len(),
            BUILTIN_INSTANCE_POLICY_KEYS.len(),
            "builtin 策略键不得重复: {BUILTIN_INSTANCE_POLICY_KEYS:?}"
        );
        assert_eq!(
            constant, declared,
            "BUILTIN_INSTANCE_POLICY_KEYS 必须等于声明表的 policy_key 集合"
        );
    }

    /// A7 两表语义不重叠：槽位名表与 builtin 策略键表交集为空（并集即「已知键全集」）。
    ///
    /// 交集非空意味着同一个键有两条语义（既关槽位又关实例），关闭面无法判定。
    #[test]
    fn middleware_names_and_builtin_policy_keys_are_disjoint() {
        for key in BUILTIN_INSTANCE_POLICY_KEYS {
            assert!(
                !MIDDLEWARE_NAMES.contains(key),
                "builtin 策略键 {key} 不得再出现在链槽位名表（A7 的两表分离）"
            );
        }
    }

    /// A7 关闭语义不降级：两表并集必须覆盖 builtin 实例的关闭键，
    /// 即 `"WebMiddleware": false` 之类的键仍是「已知键」（解析期不被当未知键丢弃）。
    #[test]
    fn known_key_union_covers_builtin_policy_keys() {
        let union: std::collections::HashSet<&str> = MIDDLEWARE_NAMES
            .iter()
            .chain(BUILTIN_INSTANCE_POLICY_KEYS.iter())
            .copied()
            .collect();
        for instance in crate::builtin_mcp::BUILTIN_MCP_INSTANCES {
            assert!(
                union.contains(instance.policy_key),
                "builtin 实例 {} 的关闭键 {} 必须落在「已知键全集」内",
                instance.name,
                instance.policy_key
            );
        }
    }

    #[test]
    fn default_state_is_empty() {
        let state = MetaHarnessState::default();
        assert!(state.section_overrides.is_empty());
        assert!(state.disabled_middlewares.is_empty());
        assert!(state.built_in_subagents_enabled);
    }

    /// 契约 3 映射表一致性：段落 ID 必须是合法 `SECTION_IDS`，持有者必须是
    /// 合法 `MIDDLEWARE_NAMES`（C1 建表时锁定，段落实体迁移时更新）。
    #[test]
    fn section_holder_middleware_refers_to_valid_ids() {
        for (id, holder) in SECTION_HOLDER_MIDDLEWARE {
            assert!(
                SECTION_IDS.contains(id),
                "SECTION_HOLDER_MIDDLEWARE 段落 ID {id} 不在 SECTION_IDS 中"
            );
            assert!(
                MIDDLEWARE_NAMES.contains(holder),
                "SECTION_HOLDER_MIDDLEWARE 持有者 {holder} 不在 MIDDLEWARE_NAMES 中"
            );
        }
        // 段落与持有者均无重复
        let mut seen_ids = std::collections::HashSet::new();
        let mut seen_holders = std::collections::HashSet::new();
        for (id, holder) in SECTION_HOLDER_MIDDLEWARE {
            assert!(seen_ids.insert(*id), "duplicate section id in map: {id}");
            assert!(
                seen_holders.insert(*holder),
                "duplicate middleware holder in map: {holder}"
            );
        }
    }

    /// R21（IF-P3-10）：cron 三工具不得进入 [`MIDDLEWARE_TOOL_NAMES`]（middleware
    /// 静态工具的防御剔除面）；其原始名与 effective name 逐字由 builtin 注册表
    /// （`crate::builtin_mcp` 的 `CRON_TOOLS`）声明。
    ///
    /// **与 `tools_test.rs:175` 的分工边界（两处不是重复实现）**：
    ///
    /// - `peri-agent/src/session/exec/stage_builder/tools_test.rs:175
    ///   migrated_naked_names_are_no_longer_excluded` 在**谓词行为级**断言
    ///   `build_session_tool_view` 的剔除结果：遍历 `BUILTIN_MCP_INSTANCES`，断言每个
    ///   工具的 **`original_name`** 不在本表内，并用 `NamedTool` 桩证明裸名不再被剔除
    ///   ——回答「剔除谓词是否按裸名放行」。
    /// - 本用例在**契约常量级**断言 **`effective_name`**（`mcp__cron__*` 三枚 +
    ///   `mcp__lsp__LSP`）不在本表内。谓词按 `name.as_str()` 精确匹配，而模型面看到
    ///   的是 effective name：effective name 一旦混进本表，迁移后的
    ///   `mcp__cron__cron_register` 会被当作 middleware 静态工具剔除（「裸名 XOR
    ///   effective name」的重命名映射随之破坏）。
    ///
    /// 原始名不在本表内已由上述谓词级用例覆盖，此处不重复；`CRON_TOOLS` 是 cron
    /// 三工具名字的**唯一声明处**，逐字声明在此钉住（字面量与
    /// `mcp::builtin::effective_tool_name()` 输出的等价另由
    /// `cargo test -p peri-middlewares --lib -- mcp::builtin::tests` 锁定）。
    #[test]
    fn cron_tools_are_not_middleware_static_tools() {
        let cron = crate::builtin_mcp::find("cron").expect("cron 必须是已实现实例");
        let declared_originals: std::collections::HashSet<&str> =
            cron.tools.iter().map(|tool| tool.original_name).collect();
        let declared_effectives: std::collections::HashSet<&str> =
            cron.tools.iter().map(|tool| tool.effective_name).collect();
        assert_eq!(
            declared_originals,
            ["cron_register", "cron_list", "cron_remove"]
                .into_iter()
                .collect::<std::collections::HashSet<&str>>(),
            "cron 三工具的原始名必须逐字由注册表 CRON_TOOLS 声明"
        );
        assert_eq!(
            declared_effectives,
            [
                "mcp__cron__cron_register",
                "mcp__cron__cron_list",
                "mcp__cron__cron_remove"
            ]
            .into_iter()
            .collect::<std::collections::HashSet<&str>>(),
            "cron 三工具的 effective name 必须逐字由注册表 CRON_TOOLS 声明"
        );
        for tool in cron.tools {
            assert_eq!(
                crate::builtin_mcp::original_tool_name_of_effective(tool.effective_name),
                Some(tool.original_name),
                "{} 的归一查表必须自洽（同一注册表内往返）",
                tool.effective_name
            );
        }
        // effective name 面：全注册表扫描（含 `mcp__lsp__LSP`），任一实例的模型面
        // 名字都不得落进剔除面。
        for instance in crate::builtin_mcp::BUILTIN_MCP_INSTANCES {
            for tool in instance.tools {
                assert!(
                    !MIDDLEWARE_TOOL_NAMES.contains(&tool.effective_name),
                    "实例 {} 的 effective name {} 不得出现在 MIDDLEWARE_TOOL_NAMES 内",
                    instance.name,
                    tool.effective_name
                );
            }
        }
    }
}
