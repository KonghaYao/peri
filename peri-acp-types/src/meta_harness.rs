//! Frozen MetaHarness settings and middleware owner declarations.

mod owners;
pub use owners::*;

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::LazyLock;

/// 冻结期构建的 MetaHarness 状态，随冻结载体（`FrozenContext`）传播；
/// 会话内不可变（ARC-FROZEN-001）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaHarnessState {
    /// 段落 ID → md 全文；仅含"开关 true 且文档存在"的条目。
    /// 覆盖发生在 `PromptTemplate::new` 构造期，render 无查表开销。
    pub section_overrides: HashMap<String, Arc<str>>,
    /// 装配期关闭的链槽位名与 builtin 实例策略键集合（配置中 `false` 条目）。
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

/// 迁移期 builtin 旧 MetaHarness 关闭键（**兼容 adapter，M4 删除**）。
///
/// 由 [`crate::builtin_mcp::v5::BUILTIN_V5_CAPABILITIES`] 的 `legacy_policy_key`
/// 派生；只做确定性映射，不承担 v5 能力开关身份。运行时经
/// `builtin_mcp::capability_of_switch` 映射到能力；#409 要求旧键不得 silent no-op。
///
/// - 解析期校验（`peri-config/src/app.rs::validate_meta_harness`）的「已知键」
///   集合 = [`SECTION_IDS`] ∪ [`MIDDLEWARE_NAMES`] ∪ 本表 ∪
///   [`BUILT_IN_SUBAGENTS_KEY`] ∪ v5 能力名；
/// - 「全部 middleware 关闭」保险丝的判定面 = [`MIDDLEWARE_NAMES`] ∪ 本表 ∪ 能力名。
pub static BUILTIN_INSTANCE_POLICY_KEYS: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| crate::builtin_mcp::legacy_policy_keys().collect());

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

    /// 兼容 adapter：旧关闭键必须由能力表派生，手写常量或声明表漂移即红。
    #[test]
    fn builtin_policy_keys_are_derived_from_capabilities() {
        let declared: std::collections::HashSet<&str> = crate::builtin_mcp::BUILTIN_V5_CAPABILITIES
            .iter()
            .map(|capability| capability.legacy_policy_key)
            .collect();
        let derived: std::collections::HashSet<&str> =
            BUILTIN_INSTANCE_POLICY_KEYS.iter().copied().collect();
        assert_eq!(
            derived.len(),
            BUILTIN_INSTANCE_POLICY_KEYS.len(),
            "旧关闭键不得重复: {BUILTIN_INSTANCE_POLICY_KEYS:?}"
        );
        assert_eq!(
            derived, declared,
            "旧关闭键表必须由能力表 legacy_policy_key 派生"
        );
    }

    /// 两表语义不重叠：槽位名表与 builtin 兼容键表交集为空。
    #[test]
    fn middleware_names_and_builtin_policy_keys_are_disjoint() {
        let overlap: std::collections::HashSet<&str> = MIDDLEWARE_NAMES
            .iter()
            .copied()
            .filter(|key| BUILTIN_INSTANCE_POLICY_KEYS.contains(key))
            .collect();
        assert!(
            overlap.is_empty(),
            "两表交集必须为空（同一键不得既关槽位又关能力）：{overlap:?}"
        );
    }

    /// 旧键必须确定性映射到能力，不能 silent no-op（#409 §8.6）。
    #[test]
    fn legacy_policy_keys_map_to_capabilities() {
        for capability in crate::builtin_mcp::BUILTIN_V5_CAPABILITIES.iter() {
            let mapped = crate::builtin_mcp::capability_of_switch(capability.legacy_policy_key)
                .expect("旧键必须确定性映射");
            assert_eq!(mapped.name, capability.name);
            assert_eq!(
                crate::builtin_mcp::capability_of_switch(capability.name)
                    .expect("能力名必须可作为开关")
                    .name,
                capability.name
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

    #[test]
    fn owner_contracts_have_unique_sections_and_tools() {
        let mut positions = HashSet::new();
        for &owner in ChainSlot::ALL {
            for spec in owner.sections() {
                assert_eq!(spec.owner, owner);
                assert!(positions.insert((spec.zone, spec.order)));
            }
        }
        let unique: HashSet<_> = MIDDLEWARE_TOOL_NAMES.iter().collect();
        assert_eq!(unique.len(), MIDDLEWARE_TOOL_NAMES.len());
    }

    /// R21（IF-P3-10）：cron 工具不得进入 [`MIDDLEWARE_TOOL_NAMES`]（middleware
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
    /// - 本用例在**契约常量级**断言 **`effective_name`**（`mcp__cron__*` 五枚）
    ///   不在本表内。谓词按 `name.as_str()` 精确匹配，而模型面看到
    ///   的是 effective name：effective name 一旦混进本表，迁移后的
    ///   `mcp__cron__cron_register` 会被当作 middleware 静态工具剔除（「裸名 XOR
    ///   effective name」的重命名映射随之破坏）。
    ///
    /// 原始名不在本表内已由上述谓词级用例覆盖，此处不重复；`CRON_TOOLS` 是 cron
    /// 工具名字的**唯一声明处**，逐字声明在此钉住（字面量与
    /// `mcp::builtin::effective_tool_name()` 输出的等价另由
    /// `cargo test -p peri-middlewares --lib -- mcp::builtin::tests` 锁定）。
    #[test]
    fn cron_tools_are_not_middleware_static_tools() {
        let cron = crate::builtin_mcp::find("cron").expect("cron 必须是已实现实例");
        let declared_originals: std::collections::HashSet<&str> =
            cron.tools.iter().map(|tool| tool.original_name).collect();
        let declared_effectives: std::collections::HashSet<&str> =
            cron.tools.iter().map(|tool| tool.legacy_name).collect();
        assert_eq!(
            declared_originals,
            [
                "cron_register",
                "cron_list",
                "cron_remove",
                "cron_toggle",
                "cron_acknowledge"
            ]
            .into_iter()
            .collect::<std::collections::HashSet<&str>>(),
            "cron 工具的原始名必须逐字由注册表 CRON_TOOLS 声明"
        );
        assert_eq!(
            declared_effectives,
            [
                "mcp__cron__cron_register",
                "mcp__cron__cron_list",
                "mcp__cron__cron_remove",
                "mcp__cron__cron_toggle",
                "mcp__cron__cron_acknowledge"
            ]
            .into_iter()
            .collect::<std::collections::HashSet<&str>>(),
            "cron 工具的 effective name 必须逐字由注册表 CRON_TOOLS 声明"
        );
        for tool in cron.tools {
            assert_eq!(
                crate::builtin_mcp::original_tool_name_of_effective(tool.legacy_name),
                Some(tool.original_name),
                "{} 的归一查表必须自洽（同一注册表内往返）",
                tool.legacy_name
            );
            assert_eq!(
                crate::builtin_mcp::original_tool_name_of_effective(tool.model_name),
                Some(tool.original_name),
                "{} 的 v5 模型名归一必须自洽",
                tool.model_name
            );
        }
        // 模型面名字：全注册表扫描，任一实例的模型面名字都不得落进剔除面。
        for instance in crate::builtin_mcp::BUILTIN_MCP_INSTANCES.iter() {
            for tool in instance.tools {
                assert!(
                    !MIDDLEWARE_TOOL_NAMES.contains(&tool.model_name),
                    "实例 {} 的 v5 模型面名字 {} 不得出现在 MIDDLEWARE_TOOL_NAMES 内",
                    instance.name,
                    tool.model_name
                );
                assert!(
                    !MIDDLEWARE_TOOL_NAMES.contains(&tool.legacy_name),
                    "实例 {} 的迁移期旧名 {} 不得出现在 MIDDLEWARE_TOOL_NAMES 内",
                    instance.name,
                    tool.legacy_name
                );
            }
        }
    }
}
