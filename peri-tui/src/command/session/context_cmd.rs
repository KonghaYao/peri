use crate::{app::App, command::Command, runtime::effect::Effect};

pub struct ContextCommand;

impl Command for ContextCommand {
    fn name(&self) -> &str {
        "context"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-context-description")
    }

    fn execute(&self, _app: &mut App, _args: &str) -> Vec<Effect> {
        vec![]
    }
}
