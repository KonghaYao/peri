use crate::{app::App, command::Command, runtime::effect::Effect};

/// /setup 命令 —— 打开 Setup 向导全屏面板
pub struct SetupCommand;

impl Command for SetupCommand {
    fn name(&self) -> &str {
        "setup"
    }

    fn description(&self, lc: &crate::i18n::LcRegistry) -> String {
        lc.tr("command-setup-description")
    }

    fn execute(&self, app: &mut App, _args: &str) -> Vec<Effect> {
        app.open_setup_wizard();
        vec![]
    }
}
