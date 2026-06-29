use crate::{
    app::{App, PanelKind},
    command::Command,
    runtime::effect::Effect,
};

pub struct WorkflowsCommand;

impl Command for WorkflowsCommand {
    fn name(&self) -> &str {
        "workflows"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        "Show workflow runs and progress".to_string()
    }

    fn execute(&self, _app: &mut App, _args: &str) -> Vec<Effect> {
        vec![Effect::OpenPanel(PanelKind::Workflow)]
    }
}
