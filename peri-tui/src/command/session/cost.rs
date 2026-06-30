use crate::{
    app::{App, PanelKind},
    command::Command,
    runtime::effect::Effect,
};

pub struct CostCommand;

impl Command for CostCommand {
    fn name(&self) -> &str {
        "cost"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-cost-description")
    }

    fn execute(&self, _app: &mut App, _args: &str) -> Vec<Effect> {
        vec![Effect::OpenPanel(PanelKind::Status)]
    }
}
