use crate::{
    app::{App, PanelKind},
    command::Command,
    runtime::effect::Effect,
};

pub struct MemoryCommand;

impl Command for MemoryCommand {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-memory-description")
    }

    fn execute(&self, _app: &mut App, _args: &str) -> Vec<Effect> {
        vec![Effect::OpenPanel(PanelKind::Memory)]
    }
}
