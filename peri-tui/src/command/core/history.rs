use crate::{
    app::{App, PanelKind},
    command::Command,
    runtime::effect::Effect,
};

pub struct HistoryCommand;

impl Command for HistoryCommand {
    fn name(&self) -> &str {
        "history"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-history-description")
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["resume"]
    }

    fn execute(&self, app: &mut App, _args: &str) -> Vec<Effect> {
        if app.session_mgr.current_mut().ui.loading {
            return vec![Effect::PushSystemNote(
                app.services.lc.tr("history-agent-running").to_string(),
            )];
        }
        vec![Effect::OpenPanel(PanelKind::ThreadBrowser)]
    }
}
