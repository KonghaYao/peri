use crate::{
    app::{App, PanelKind},
    command::Command,
    runtime::effect::Effect,
};

pub struct BetasCommand;

impl Command for BetasCommand {
    fn name(&self) -> &str {
        "betas"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        "\u{6253}\u{5f00} Beta \u{529f}\u{80fd}\u{5f00}\u{5173}\u{9762}\u{677f}".to_string()
    }

    fn execute(&self, _app: &mut App, _args: &str) -> Vec<Effect> {
        vec![Effect::OpenPanel(PanelKind::Betas)]
    }
}
