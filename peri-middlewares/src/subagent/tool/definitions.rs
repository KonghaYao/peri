//! Definition source precedence, local parsing and parent tool policy.
use crate::{
    agent_define::{AgentDefineMiddleware, AgentOverrides},
    claude_agent_parser::{parse_agent_file, ClaudeAgent, ToolsValue},
    subagent::built_in_agents::get_built_in_agent,
};
use peri_agent::tools::BaseTool;
use std::{collections::BTreeSet, path::Path};

impl super::SubAgentTool {
    pub(crate) fn load_agent_def(&self, agent_id: &str, cwd: &str) -> Result<ClaudeAgent, String> {
        self.load_agent_def_with_built_ins(agent_id, cwd, self.built_in_subagents_enabled())
    }

    /// Resume 已有 thread 时允许恢复其原 built-in definition；新建路径遵守
    /// 父 session 冻结的 MetaHarness policy。
    pub(crate) fn load_agent_def_for_resume(
        &self,
        agent_id: &str,
        cwd: &str,
    ) -> Result<ClaudeAgent, String> {
        self.load_agent_def_with_built_ins(agent_id, cwd, true)
    }

    fn built_in_subagents_enabled(&self) -> bool {
        self.parent_session
            .read()
            .as_ref()
            .map(|session| {
                session
                    .store()
                    .frozen
                    .meta_harness
                    .built_in_subagents_enabled
            })
            .unwrap_or(true)
    }

    fn load_agent_def_with_built_ins(
        &self,
        agent_id: &str,
        cwd: &str,
        include_built_ins: bool,
    ) -> Result<ClaudeAgent, String> {
        if agent_id.starts_with("mcp__") {
            return self
                .mcp_agent_registry
                .as_ref()
                .and_then(|registry| registry.cached(agent_id))
                .map(|activated| activated.definition)
                .ok_or_else(|| {
                    format!(
                        "Error: MCP agent definition '{}' is not activated in this session",
                        agent_id
                    )
                });
        }

        let project_candidates = AgentDefineMiddleware::candidate_paths(cwd, agent_id);
        if project_candidates.is_empty() {
            return Err(format!("Error: invalid agent definition ID '{}'", agent_id));
        }
        let agent_path = project_candidates.into_iter().find(|p| p.is_file());

        if let Some(path) = agent_path {
            return read_definition(&path);
        }

        if include_built_ins {
            if let Some(built_in) = get_built_in_agent(agent_id) {
                return parse_agent_file(built_in.content).ok_or_else(|| {
                    format!(
                        "Error: failed to parse built-in agent definition '{}'",
                        agent_id
                    )
                });
            }
        }

        for dir in self.plugin_agent_dirs.iter() {
            let candidates = [
                dir.join(format!("{agent_id}.md")),
                dir.join(agent_id).join("agent.md"),
            ];
            if let Some(path) = candidates.into_iter().find(|path| path.is_file()) {
                return read_definition(&path);
            }
        }

        Err(format!(
            "Error: cannot find agent definition '{}'. Check .claude/agents/ directory{}",
            agent_id,
            if include_built_ins {
                " or configured plugin agents"
            } else {
                " or configured plugin agents (built-in agents are disabled)"
            }
        ))
    }

    /// Build retry hints from the same loader and invocation context as the failed call.
    ///
    /// The error-suggestion registry intentionally does not know about agent sources: its
    /// snapshot is prompt metadata and may belong to another cwd or policy. Agent hints are
    /// therefore resolved here, after the real load failed, and every enumerated id is passed
    /// through `load_agent_def` before it can be shown to the caller.
    pub(crate) fn agent_error_with_suggestions(
        &self,
        error: &str,
        requested: Option<&str>,
        cwd: &str,
    ) -> String {
        let candidates = self.loadable_agent_ids(cwd);
        if candidates.is_empty() {
            return error.to_string();
        }

        let Some(requested) = requested else {
            let available = candidates
                .into_iter()
                .take(3)
                .collect::<Vec<_>>()
                .join(", ");
            return format!("{error}\nAvailable agent types: {available}");
        };

        let matches = peri_agent::error_suggest::matcher::fuzzy_filter(&candidates, requested)
            .into_iter()
            .take(3)
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return error.to_string();
        }
        format!("{error}\nSuggestion: did you mean {}?", matches.join(", "))
    }

    /// Enumerate only the finite, direct agent roots understood by the real loader.
    /// Candidates are sorted and then reloaded through that loader so malformed files and
    /// project shadowing never become suggestions.
    fn loadable_agent_ids(&self, cwd: &str) -> Vec<String> {
        let mut ids = BTreeSet::new();
        collect_agent_ids(&Path::new(cwd).join(".claude").join("agents"), &mut ids);
        collect_agent_ids(&Path::new(cwd).join("agents"), &mut ids);
        for dir in self.plugin_agent_dirs.iter() {
            collect_agent_ids(dir, &mut ids);
        }
        if self.built_in_subagents_enabled() {
            ids.extend(
                crate::subagent::built_in_agent_types()
                    .iter()
                    .map(|id| (*id).to_string()),
            );
        }

        ids.into_iter()
            .filter(|id| self.load_agent_def(id, cwd).is_ok())
            .collect()
    }

    pub(crate) fn overrides_from_agent_def(
        system_prompt: &str,
        tone: &Option<String>,
        proactiveness: &Option<String>,
        mode: &Option<String>,
    ) -> Option<AgentOverrides> {
        crate::subagent::fork::overrides_from_agent_def(system_prompt, tone, proactiveness, mode)
    }

    pub(crate) fn filter_tools(
        &self,
        allowed: &ToolsValue,
        disallowed: &ToolsValue,
    ) -> Vec<Box<dyn BaseTool>> {
        crate::subagent::fork::filter_tools(&self.parent_tools, allowed, disallowed)
    }
}

fn collect_agent_ids(dir: &Path, ids: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            if let Some(id) = path.file_stem().and_then(|name| name.to_str()) {
                ids.insert(id.to_string());
            }
        } else if path.is_dir() && path.join("agent.md").is_file() {
            if let Some(id) = path.file_name().and_then(|name| name.to_str()) {
                ids.insert(id.to_string());
            }
        }
    }
}

fn read_definition(path: &std::path::Path) -> Result<ClaudeAgent, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Error: failed to read agent definition file: {}", e))?;
    parse_agent_file(&content).ok_or_else(|| {
        format!(
            "Error: failed to parse agent definition file '{}'",
            path.display()
        )
    })
}
