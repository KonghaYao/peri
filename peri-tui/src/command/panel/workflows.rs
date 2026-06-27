use crate::{app::App, command::Command};

pub struct WorkflowsCommand;

impl Command for WorkflowsCommand {
    fn name(&self) -> &str {
        "workflows"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        "Show workflow runs and progress".to_string()
    }

    fn execute(&self, app: &mut App, _args: &str) {
        app.open_workflows_panel();
    }
}
