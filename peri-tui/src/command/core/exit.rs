use crate::{app::App, command::Command, runtime::effect::Effect};

pub struct ExitCommand;

impl Command for ExitCommand {
    fn name(&self) -> &str {
        "exit"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-exit-description")
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["quit"]
    }

    fn execute(&self, app: &mut App, _args: &str) -> Vec<Effect> {
        app.global_ui.quit_requested = true;
        vec![]
    }
}
