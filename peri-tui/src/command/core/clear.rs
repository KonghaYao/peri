use crate::{app::App, command::Command, runtime::effect::Effect};

pub struct ClearCommand;

impl Command for ClearCommand {
    fn name(&self) -> &str {
        "clear"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-clear-description")
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["reset", "new"]
    }

    fn execute(&self, app: &mut App, _args: &str) -> Vec<Effect> {
        app.new_thread();
        vec![]
    }
}
