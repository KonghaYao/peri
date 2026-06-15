use super::*;

impl App {
    /// 打开 /config 面板
    pub fn open_config_panel(&mut self) {
        let panel = {
            let cfg_guard = self.services.peri_config.read();
            config_panel::ConfigPanel::from_config(&cfg_guard)
        };
        self.open_panel(PanelState::Config(Box::new(panel)));
    }

    /// 关闭 /config 面板
    pub fn close_config_panel(&mut self) {
        self.session_mgr
            .current_mut()
            .session_panels
            .close_if(PanelKind::Config);
    }

    /// 保存 Config 面板编辑并关闭
    pub fn config_panel_apply(&mut self) {
        let Some(panel) = self
            .session_mgr
            .current_mut()
            .session_panels
            .get_mut::<config_panel::ConfigPanel>()
        else {
            return;
        };
        let cfg = self.services.peri_config.clone();
        let mut cfg_guard = cfg.write();
        if let Err(err_msg) = panel.apply_edit(&mut cfg_guard, &self.services.lc) {
            self.session_mgr
                .current_mut()
                .messages
                .view_messages
                .push(MessageViewModel::system(err_msg));
            return;
        }
        if let Some(ref lang) = cfg_guard.config.language.clone() {
            let _ = self.services.lc.switch(lang);
        }
        if let Err(e) = Self::save_config(&cfg_guard, self.services.config_path_override.as_deref())
        {
            self.session_mgr
                .current_mut()
                .messages
                .view_messages
                .push(MessageViewModel::system(self.services.lc.tr_args(
                    "app-config-save-failed",
                    &[("error".into(), e.to_string().into())],
                )));
        } else {
            self.session_mgr
                .current_mut()
                .messages
                .view_messages
                .push(MessageViewModel::system(
                    self.services.lc.tr("app-config-saved"),
                ));
        }
        self.session_mgr
            .current_mut()
            .session_panels
            .close_if(PanelKind::Config);
    }
}
