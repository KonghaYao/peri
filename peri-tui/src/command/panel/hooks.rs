use crate::{
    app::{App, PanelKind},
    command::Command,
    runtime::effect::Effect,
};

/// /hooks 命令：打开 Hooks 查看面板
pub struct HooksCommand;

impl Command for HooksCommand {
    fn name(&self) -> &str {
        "hooks"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-hooks-description")
    }

    fn execute(&self, _app: &mut App, _args: &str) -> Vec<Effect> {
        vec![Effect::OpenPanel(PanelKind::Hooks)]
    }
}
