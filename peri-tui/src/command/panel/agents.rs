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

/// Agent 项
#[derive(Debug, Clone)]
pub struct AgentItem {
    pub id: String,
    pub name: String,
    pub description: String,
}
