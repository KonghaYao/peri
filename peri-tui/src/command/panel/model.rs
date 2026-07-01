use crate::{
    app::{agent, App, PanelKind},
    command::Command,
    runtime::effect::Effect,
};

pub struct ModelCommand;

impl Command for ModelCommand {
    fn name(&self) -> &str {
        "model"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-model-description")
    }

    fn execute(&self, app: &mut App, args: &str) -> Vec<Effect> {
        let alias = args.trim().to_lowercase();
        match alias.as_str() {
            "opus" | "sonnet" | "haiku" => {
                let cfg_arc = app.services.peri_config.clone();
                let mut cfg = cfg_arc.write();
                cfg.config.active_alias = alias.clone();
                let mut effects: Vec<Effect> = Vec::new();
                if let Err(e) = App::save_config(&cfg, app.services.config_path_override.as_deref())
                {
                    effects.push(Effect::ShowNotification(app.services.lc.tr_args(
                        "config-save-failed",
                        &[("error".into(), e.to_string().into())],
                    )));
                }
                if let Some(p) = agent::LlmProvider::from_config(&cfg) {
                    app.services.provider_name = p.display_name().to_string();
                    app.services.model_name = p.model_name().to_string();
                }
                if let Some(ref acp_client) = app.acp_client {
                    let acp = acp_client.clone();
                    let alias_val = alias.clone();
                    tokio::spawn(async move {
                        let _ = acp.set_config_option("model", &alias_val).await;
                    });
                }
                effects.push(Effect::Render);
                effects
            }
            _ => {
                vec![Effect::OpenPanel(PanelKind::Model)]
            }
        }
    }
}
