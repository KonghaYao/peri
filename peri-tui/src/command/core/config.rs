use crate::{
    app::{App, PanelKind},
    command::Command,
    runtime::effect::Effect,
};

pub struct ConfigCommand;

impl Command for ConfigCommand {
    fn name(&self) -> &str {
        "config"
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["settings"]
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-config-description")
    }

    fn execute(&self, _app: &mut App, _args: &str) -> Vec<Effect> {
        vec![Effect::OpenPanel(PanelKind::Config)]
    }
}
