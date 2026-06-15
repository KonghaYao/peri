use crate::{
    app::App, command::Command, config::ThinkingConfig, ui::message_view::MessageViewModel,
};

pub struct EffortCommand;

impl Command for EffortCommand {
    fn name(&self) -> &str {
        "effort"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-effort-description")
    }

    fn execute(&self, app: &mut App, args: &str) {
        let arg = args.trim().to_lowercase();
        match arg.as_str() {
            "low" | "medium" | "high" | "xhigh" | "max" => {
                let cfg_arc = app.services.peri_config.clone();
                let mut cfg = cfg_arc.write();
                cfg.config.thinking = Some(ThinkingConfig {
                    enabled: cfg.config.thinking.as_ref().is_none_or(|t| t.enabled),
                    budget_tokens: cfg
                        .config
                        .thinking
                        .as_ref()
                        .map_or(8000, |t| t.budget_tokens),
                    effort: arg.clone(),
                    max_tokens: cfg.config.thinking.as_ref().map_or(32000, |t| t.max_tokens),
                });
                if let Err(e) = App::save_config(&cfg, app.services.config_path_override.as_deref())
                {
                    let vm = MessageViewModel::system(format!("配置保存失败: {}", e));
                    app.session_mgr
                        .current_mut()
                        .messages
                        .view_messages
                        .push(vm);
                    return;
                }
                let vm = MessageViewModel::system(format!("推理力度已设为 {}", arg));
                app.session_mgr
                    .current_mut()
                    .messages
                    .view_messages
                    .push(vm);
                if let Some(ref acp_client) = app.acp_client {
                    let acp = acp_client.clone();
                    let val = arg.clone();
                    tokio::spawn(async move {
                        let _ = acp.set_config_option("thinking_effort", &val).await;
                    });
                }
                app.render_rebuild();
            }
            _ => {
                let current_owned = app
                    .services
                    .peri_config
                    .read()
                    .config
                    .thinking
                    .as_ref()
                    .map(|t| t.effort.clone())
                    .unwrap_or_else(|| "high".to_string());
                let current = current_owned.as_str();
                let vm = MessageViewModel::system(format!(
                    "当前推理力度: {}\n用法: /effort low|medium|high|xhigh|max",
                    current
                ));
                app.session_mgr
                    .current_mut()
                    .messages
                    .view_messages
                    .push(vm);
                app.render_rebuild();
            }
        }
    }
}
