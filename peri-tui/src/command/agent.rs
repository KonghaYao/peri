use crate::app::{App, MessageViewModel};
use crate::command::Command;
use crate::runtime::effect::Effect;

pub struct AgentCommand;

impl Command for AgentCommand {
    fn name(&self) -> &str {
        "agent"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-agent-description").into()
    }

    fn execute(&self, app: &mut App, args: &str) -> Vec<Effect> {
        let lc = &app.services.lc;
        let id = args.trim();
        if id.is_empty() {
            // 清除 agent_id
            app.set_agent_id(None);
            app.session_mgr.current_mut().messages.view_messages.push(MessageViewModel::system(
                lc.tr("command-agent-reset").to_string(),
            ));
        } else {
            app.set_agent_id(Some(id.to_string()));
            let name = peri_middlewares::format_agent_id(id);
            app.session_mgr.current_mut().messages.view_messages.push(MessageViewModel::system(
                lc.tr_args(
                    "command-agent-switched",
                    &[
                        ("name".into(), name.into()),
                        ("id".into(), id.to_string().into()),
                    ],
                ),
            ));
        }
        vec![]
    }
}
