use crate::runtime::effect::Effect;
use crate::{app::App, command::Command, ui::message_view::MessageViewModel};

pub struct BgCommand;

impl Command for BgCommand {
    fn name(&self) -> &str {
        "bg"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-bg-description")
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["background"]
    }

    fn execute(&self, app: &mut App, args: &str) -> Vec<Effect> {
        let lc = &app.services.lc;
        let args = args.trim();
        if args.is_empty() {
            let vm = MessageViewModel::system(lc.tr("command-bg-usage").to_string());
            app.session_mgr
                .current_mut()
                .messages
                .view_messages
                .push(vm);
            app.render_rebuild();
            return vec![];
        }
        // Pass through to executor — keep /bg prefix so ACP executor intercepts it
        app.submit_message(format!("/bg {}", args));
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("bg_test.rs");
}
