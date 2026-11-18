//! Stable middleware contracts. Configuration validation and all execution profiles
//! consume these declarations without linking a middleware implementation.
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PromptSectionZone {
    Cached,
    Uncached,
    Environment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionSpec {
    pub id: &'static str,
    pub owner: ChainSlot,
    pub zone: PromptSectionZone,
    pub order: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionProfile {
    Main,
    Child,
    Workflow { broker: bool, permission_mode: bool },
}

// Scope marks the executors that may consume this owner. Base and language are
// inherited declarations on derived executors. MCP is inherited as bound tools.
macro_rules! declare_owners {
    ($( $owner:ident($name:literal, $child:literal, $workflow:literal)
        [$($tool:literal),*] { $($section:ident($id:literal, $zone:ident, $order:literal)),* } ),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum ChainSlot { $($owner),* }

        impl ChainSlot {
            /// Registration order is the public hook ordering contract.
            pub const ALL: &'static [Self] = &[$(Self::$owner),*];
            pub const fn name(self) -> &'static str {
                match self { $(Self::$owner => $name),* }
            }
            pub const fn tool_names(self) -> &'static [&'static str] {
                match self { $(Self::$owner => &[$($tool),*]),* }
            }
            pub const fn sections(self) -> &'static [SectionSpec] {
                match self { $(Self::$owner => &[$(sections::$section),*]),* }
            }
            fn available_in(self, profile: ExecutionProfile) -> bool {
                match profile {
                    ExecutionProfile::Main => true,
                    ExecutionProfile::Child => match self { $(Self::$owner => $child),* },
                    ExecutionProfile::Workflow { broker, .. } => match self {
                        Self::AskUser if !broker => false,
                        $(Self::$owner => $workflow),*
                    },
                }
            }
        }

        pub mod sections {
            use super::{ChainSlot, PromptSectionZone, SectionSpec};
            $($(pub const $section: SectionSpec = SectionSpec {
                id: $id, owner: ChainSlot::$owner, zone: PromptSectionZone::$zone, order: $order,
            };)*)*
        }
        pub const MIDDLEWARE_NAMES: &[&str] = &[$($name),*];
        pub const MIDDLEWARE_TOOL_NAMES: &[&str] = &[$($($tool,)*)*];
        pub const SECTION_IDS: &[&str] = &[$($(sections::$section.id,)*)*];
    };
}

declare_owners! {
    DefaultSystemPrompt("DefaultSystemPromptMiddleware", true, true) [] {
        INTRO("01_intro", Cached, 1), SYSTEM("02_system", Cached, 2),
        TASKS("03_doing_tasks", Cached, 3), ACTIONS("04_actions", Cached, 4),
        TOOLS("05_using_tools", Cached, 5), STYLE("06_tone_style", Cached, 6),
        RUNTIME("07_runtime", Uncached, 1), ENV("env", Environment, 0),
        PERSONA("persona", Uncached, 0)
    },
    Lang("LangMiddleware", true, true) [] { LANGUAGE("language", Uncached, 8) },
    AgentsMd("AgentsMdMiddleware", true, true) [] {},
    Plugin("PluginMiddleware", false, false) [] {},
    Skills("SkillsMiddleware", true, true) ["SkillTool","DiscoverSkillsTool"] { SKILLS("13_skills", Uncached, 6) },
    SkillPreload("SkillPreloadMiddleware", true, true) [] {},
    AtMention("AtMentionMiddleware", false, false) [] {},
    Image("ImageMiddleware", false, false) [] {},
    GitAttribution("GitAttributionMiddleware", false, true) [] {},
    Todo("TodoMiddleware", true, true) ["TodoWrite"] {},
    Hook("HookMiddleware", false, false) [] {},
    Permission("PermissionMiddleware", false, true) [] { APPROVAL("10_hitl", Uncached, 3) },
    AskUser("HumanInTheLoopMiddleware", false, true) ["AskUserQuestion"] { ASK_USER("12_ask_user", Uncached, 5) },
    SubAgent("SubAgentMiddleware", false, false) ["Agent","AgentResult"] { SUBAGENT("11_subagent", Uncached, 4) },
    Mcp("McpMiddleware", true, true) ["DiscoverMCP","mcp_read_resource"] {},
    // ToolSearch 一词同时是 middleware 关闭键与搜索工具名（一个词两层身份）；
    // `"ToolSearch": false` 同时下线 middleware 与其 meta 工具。
    ToolSearch("ToolSearch", true, false) ["ToolSearch","ToolInvoke"] {},
    Goal("GoalMiddleware", false, false) ["goal"] {},
}

impl ChainSlot {
    pub fn enabled(self, disabled: &HashSet<String>) -> bool {
        !disabled.contains(self.name())
    }
}

/// Materialized from the frozen session policy, with a profile that can only
/// remove owners. No settings I/O or mutable registry participates in this step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssemblyPlan {
    enabled: [bool; ChainSlot::ALL.len()],
    profile: ExecutionProfile,
}

impl AssemblyPlan {
    pub fn new(disabled: &HashSet<String>, profile: ExecutionProfile) -> Self {
        let mut enabled = [false; ChainSlot::ALL.len()];
        for &owner in ChainSlot::ALL {
            enabled[owner as usize] = owner.available_in(profile) && owner.enabled(disabled);
        }
        Self { enabled, profile }
    }

    pub fn contains(&self, owner: ChainSlot) -> bool {
        self.enabled[owner as usize]
    }

    pub fn declares_sections(&self, owner: ChainSlot) -> bool {
        if let (
            ChainSlot::Permission,
            ExecutionProfile::Workflow {
                broker,
                permission_mode,
            },
        ) = (owner, self.profile)
        {
            return self.contains(owner) && broker && permission_mode;
        }
        self.contains(owner)
    }
}
