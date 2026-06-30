use crate::runtime::effect::Effect;
use crate::{app::App, command::Command};

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
            return vec![Effect::PushSystemNote(
                lc.tr("command-bg-usage").to_string(),
            )];
        }
        // Pass through to executor — keep /bg prefix so ACP executor intercepts it
        // Cron #26 step 7e.7: UserBubble 路由到 v2 state.view。
        let prompt = format!("/bg {}", args);
        app.session_mgr
            .current_mut()
            .messages
            .push_user_bubble(prompt.clone());
        app.submit_message(prompt);
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("bg_test.rs");
}
