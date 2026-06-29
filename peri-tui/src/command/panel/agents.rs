use std::{collections::HashSet, path::Path};

use crate::{
    app::{App, PanelKind},
    command::Command,
    runtime::effect::Effect,
};

/// /agents 命令：打开 agent 选择弹窗
pub struct AgentsCommand;

impl Command for AgentsCommand {
    fn name(&self) -> &str {
        "agents"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-agents-description")
    }

    fn execute(&self, _app: &mut App, _args: &str) -> Vec<Effect> {
        vec![Effect::OpenPanel(PanelKind::Agent)]
    }
}

/// 从各个路径扫描可用的 agent 定义文件
///
/// 扫描顺序即优先级：project > global > alt，相同 id 保留最先出现的。
fn list_available_agents(cwd: &str) -> Vec<AgentItem> {
    let mut agents = Vec::new();
    let mut seen = HashSet::new();

    // 1. 项目级: .claude/agents/{id}/agent.md 或 .claude/agents/{id}.md（最高优先级）
    let project_agents = Path::new(cwd).join(".claude").join("agents");
    scan_dir_for_agents(&project_agents, &mut agents, &mut seen);

    // 2. 全局用户级: ~/.claude/agents/
    if let Some(home) = dirs_next::home_dir() {
        let global_agents = home.join(".claude").join("agents");
        scan_dir_for_agents(&global_agents, &mut agents, &mut seen);
    }

    // 3. 备选路径: agents/{id}/agent.md 或 agents/{id}.md
    let alt_agents = Path::new(cwd).join("agents");
    scan_dir_for_agents(&alt_agents, &mut agents, &mut seen);

    agents
}

/// 扫描目录下的 agent 文件，跳过已见 id（保持调用顺序优先级）
fn scan_dir_for_agents(dir: &Path, agents: &mut Vec<AgentItem>, seen: &mut HashSet<String>) {
    if !dir.is_dir() {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let agent_md = path.join("agent.md");
            if agent_md.is_file() {
                if let Some(id) = path.file_name().and_then(|n| n.to_str()) {
                    if seen.insert(id.to_string()) {
                        let (name, description) = parse_agent_info(&agent_md, id);
                        agents.push(AgentItem {
                            id: id.to_string(),
                            name,
                            description,
                        });
                    }
                }
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if seen.insert(stem.to_string()) {
                    let (name, description) = parse_agent_info(&path, stem);
                    agents.push(AgentItem {
                        id: stem.to_string(),
                        name,
                        description,
                    });
                }
            }
        }
    }
}

fn parse_agent_info(path: &Path, fallback_id: &str) -> (String, String) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => {
            return (
                peri_middlewares::format_agent_id(fallback_id),
                String::new(),
            )
        }
    };

    if let Some(agent) = peri_middlewares::parse_agent_file(&content) {
        return (agent.frontmatter.name, agent.frontmatter.description);
    }

    (
        peri_middlewares::format_agent_id(fallback_id),
        String::new(),
    )
}

/// Agent 项
#[derive(Debug, Clone)]
pub struct AgentItem {
    pub id: String,
    pub name: String,
    pub description: String,
}
