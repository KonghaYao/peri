use crate::{
    app::{App, MessageViewModel},
    command::Command,
    runtime::effect::Effect,
};

pub struct HelpCommand;

impl Command for HelpCommand {
    fn name(&self) -> &str {
        "help"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-help-description")
    }

    fn execute(&self, app: &mut App, _args: &str) -> Vec<Effect> {
        let lc = &app.services.lc;
        // 使用启动时预计算的列表（command_registry 在 dispatch 时已被 std::mem::take 取出）
        let mut lines = vec![lc.tr("help-available-commands")];
        for (name, desc, aliases) in &app.session_mgr.current_mut().commands.command_help_list {
            let alias_str = if aliases.is_empty() {
                String::new()
            } else {
                lc.tr_args(
                    "help-alias-prefix",
                    &[("aliases".into(), aliases.join(", /").into())],
                )
            };
            lines.push(format!("  /{:<10} {}{}", name, desc, alias_str));
        }

        // Skills 说明
        let skills_count = app.session_mgr.current_mut().commands.skills.len();
        lines.push("".to_string());
        if skills_count > 0 {
            lines.push(lc.tr_args(
                "help-skills-count",
                &[("count".into(), skills_count.to_string().into())],
            ));
        } else {
            lines.push(lc.tr("help-skills-empty"));
        }

        // 全局快捷键提示
        lines.push("".to_string());
        lines.push(lc.tr_args(
            "help-shortcuts",
            &[(
                "model_key".into(),
                crate::event::keyboard::cycle_model_label().into(),
            )],
        ));

        let vm = MessageViewModel::system(lines.join("\n"));
        app.session_mgr
            .current_mut()
            .messages
            .view_messages
            .push(vm);
        app.render_rebuild();
        vec![]
    }
}
