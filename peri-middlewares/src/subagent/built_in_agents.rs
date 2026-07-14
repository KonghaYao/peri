//! Built-in agent registry
//!
//! Embeds agent definition `.md` files at compile time and provides
//! lookup functions for agent discovery and content resolution.
//!
//! Built-in agents have the lowest priority — project-level `.claude/agents/`
//! definitions with the same `agent_id` always take precedence.

/// Built-in agent definitions, keyed by `agent_id` (filename stem).
///
/// Compile-time embedded via `include_str!`.
pub struct BuiltInAgent {
    /// Agent ID used as `subagent_type` parameter value
    pub agent_id: &'static str,
    /// Full file content (YAML frontmatter + markdown body)
    pub content: &'static str,
}

/// Return all built-in agent definitions.
pub fn list_built_in_agents() -> &'static [BuiltInAgent] {
    &BUILT_IN_AGENTS
}

/// Look up a built-in agent by `agent_id`. Returns `None` if not found.
pub fn get_built_in_agent(agent_id: &str) -> Option<&'static BuiltInAgent> {
    BUILT_IN_AGENTS.iter().find(|a| a.agent_id == agent_id)
}

/// 返回所有内置 subagent type 名（agent_id），用于 ToolRegistrySnapshot 构造
pub fn built_in_agent_types() -> Vec<&'static str> {
    BUILT_IN_AGENTS.iter().map(|a| a.agent_id).collect()
}

static BUILT_IN_AGENTS: [BuiltInAgent; 6] = [
    BuiltInAgent {
        agent_id: "coder",
        content: include_str!("built-in/coder.md"),
    },
    BuiltInAgent {
        agent_id: "explorer",
        content: include_str!("built-in/explorer.md"),
    },
    BuiltInAgent {
        agent_id: "general-purpose",
        content: include_str!("built-in/general-purpose.md"),
    },
    BuiltInAgent {
        agent_id: "plan",
        content: include_str!("built-in/plan.md"),
    },
    BuiltInAgent {
        agent_id: "verification",
        content: include_str!("built-in/verification.md"),
    },
    BuiltInAgent {
        agent_id: "web-researcher",
        content: include_str!("built-in/web-researcher.md"),
    },
];

#[cfg(test)]
#[path = "built_in_agents_test.rs"]
mod tests;
