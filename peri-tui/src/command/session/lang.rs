use crate::{app::App, command::Command};

pub struct LangCommand;

impl Command for LangCommand {
    fn name(&self) -> &str {
        "lang"
    }

    fn description(&self, lc: &crate::i18n::LcRegistry) -> String {
        lc.tr("command-lang-description")
    }

    fn execute(&self, app: &mut App, args: &str) {
        let lang = args.trim();
        if lang.is_empty() {
            let available = app.services.lc.available_langs();
            let current = app.services.lc.current_lang();
            let langs_display: Vec<String> = available
                .iter()
                .map(|l| {
                    if *l == current {
                        format!("{} (current)", l)
                    } else {
                        l.to_string()
                    }
                })
                .collect();
            let msg = format!(
                "{}\n{}",
                app.services.lc.tr_args(
                    "lang-available",
                    &[("langs".into(), langs_display.join(", ").into(),)]
                ),
                langs_display
                    .iter()
                    .map(|s| format!("  /lang {}", s))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            app.active_mut().messages.push_system_note(msg);
            return;
        }

        match app.services.lc.switch(lang) {
            Ok(()) => {
                {
                    let mut cfg_guard = app.services.peri_config.write();
                    cfg_guard.config.language = Some(lang.to_string());
                    let _ =
                        App::save_config(&cfg_guard, app.services.config_path_override.as_deref());
                }
                app.request_rebuild();
                let msg = app
                    .services
                    .lc
                    .tr_args("lang-switched", &[("lang".into(), lang.into())]);
                app.active_mut().messages.push_system_note(msg);
            }
            Err(_) => {
                let msg = app
                    .services
                    .lc
                    .tr_args("lang-unsupported", &[("lang".into(), lang.into())]);
                app.active_mut().messages.push_system_note(msg);
            }
        }
    }
}
