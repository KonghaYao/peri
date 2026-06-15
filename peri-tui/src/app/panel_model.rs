use super::*;

impl App {
    /// 打开 /model 面板
    pub fn open_model_panel(&mut self) {
        let panel = {
            let cfg_guard = self.services.peri_config.read();
            ModelPanel::from_config(&cfg_guard)
        };
        self.open_panel(PanelState::Model(panel));
    }

    /// 关闭 /model 面板（不保存）
    pub fn close_model_panel(&mut self) {
        self.session_mgr
            .current_mut()
            .session_panels
            .close_if(PanelKind::Model);
    }

    /// 确认选择并保存（Enter 键）：写入 active_alias + effort，更新状态栏
    pub fn model_panel_confirm(&mut self) {
        let alias_label;
        let effort;
        {
            let Some(panel) = self
                .session_mgr
                .current_mut()
                .session_panels
                .get::<ModelPanel>()
            else {
                return;
            };
            alias_label = panel.active_tab.label().to_string();
            effort = panel.buf_thinking_effort.clone();
            let mut cfg_guard = self.services.peri_config.write();
            panel.apply_to_config(&mut cfg_guard);
        }
        let effort_display = match effort.as_str() {
            "low" => "Low",
            "high" => "High",
            "xhigh" => "XHigh",
            "max" => "Max",
            _ => "Medium",
        };
        self.session_mgr
            .current_mut()
            .messages
            .push_system_note(self.services.lc.tr_args(
                "app-model-switched",
                &[
                    ("alias".into(), alias_label.clone().into()),
                    ("effort".into(), effort_display.into()),
                ],
            ));
        {
            let cfg_guard = self.services.peri_config.read();
            if let Err(e) =
                Self::save_config(&cfg_guard, self.services.config_path_override.as_deref())
            {
                self.session_mgr
                    .current_mut()
                    .messages
                    .push_system_note(self.services.lc.tr_args(
                        "app-config-save-failed",
                        &[("error".into(), e.to_string().into())],
                    ));
            }
            if let Some(p) = agent::LlmProvider::from_config(&cfg_guard) {
                self.services.provider_name = p.display_name().to_string();
                self.services.model_name = p.model_name().to_string();
            }
        }
        self.session_mgr
            .current_mut()
            .session_panels
            .close_if(PanelKind::Model);

        // 通过 ACP 协议同步模型和思考度设置到 Server
        if let Some(ref acp_client) = self.acp_client {
            let acp = acp_client.clone();
            let alias = alias_label.clone().to_lowercase();
            let effort_val = effort.clone();
            tokio::spawn(async move {
                let _ = acp.set_config_option("model", &alias).await;
                let _ = acp.set_config_option("thinking_effort", &effort_val).await;
            });
        }
    }
}
