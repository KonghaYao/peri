use super::*;

impl App {
    /// 打开 /login 面板
    pub fn open_login_panel(&mut self) {
        let panel = {
            let cfg_guard = self.services.peri_config.read();
            login_panel::LoginPanel::from_config(&cfg_guard)
        };
        self.open_panel(PanelState::Login(Box::new(panel)));
    }

    /// 关闭 /login 面板（不保存）
    pub fn close_login_panel(&mut self) {
        self.session_mgr
            .current_mut()
            .session_panels
            .close_if(PanelKind::Login);
    }

    /// 选中（激活）光标处的 Provider
    pub fn login_panel_select_provider(&mut self) {
        let Some(panel) = self
            .session_mgr
            .current_mut()
            .session_panels
            .get_mut::<login_panel::LoginPanel>()
        else {
            return;
        };
        let selected_name = panel
            .providers
            .get(panel.cursor())
            .map(|p| p.display_name().to_string())
            .unwrap_or_default();
        let cfg = self.services.peri_config.clone();
        let mut cfg_guard = cfg.write();
        panel.select_provider(&mut cfg_guard);
        if !selected_name.is_empty() {
            self.session_mgr
                .current_mut()
                .messages
                .push_system_note(self.services.lc.tr_args(
                    "app-provider-activated",
                    &[("name".into(), selected_name.into())],
                ));
        }
        if let Err(e) = Self::save_config(&cfg_guard, self.services.config_path_override.as_deref())
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
        self.close_login_panel();
    }

    /// 保存 Login 面板的编辑/新建内容到 PeriConfig，自动激活并关闭面板
    pub fn login_panel_apply_edit(&mut self) {
        let Some(panel) = self
            .session_mgr
            .current_mut()
            .session_panels
            .get_mut::<login_panel::LoginPanel>()
        else {
            return;
        };
        let edit_name = panel.field_name.value();
        let is_new = matches!(panel.mode, login_panel::LoginPanelMode::New);
        let cfg = self.services.peri_config.clone();
        let mut cfg_guard = cfg.write();
        if !panel.apply_edit(&mut cfg_guard) {
            self.session_mgr
                .current_mut()
                .messages
                .view_messages
                .push(MessageViewModel::system(
                    self.services.lc.tr("app-provider-name-empty"),
                ));
            return;
        }
        let display = if edit_name.is_empty() {
            "Provider".to_string()
        } else {
            edit_name
        };
        // 自动激活保存的 provider
        panel.select_provider(&mut cfg_guard);
        let key = if is_new {
            "app-provider-created"
        } else {
            "app-provider-saved"
        };
        self.session_mgr
            .current_mut()
            .messages
            .view_messages
            .push(MessageViewModel::system(
                self.services
                    .lc
                    .tr_args(key, &[("name".into(), display.into())]),
            ));
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
        }
        if let Some(p) = agent::LlmProvider::from_config(&cfg_guard) {
            self.services.provider_name = p.display_name().to_string();
            self.services.model_name = p.model_name().to_string();
        }
        self.close_login_panel();
    }

    /// 确认删除光标处的 Provider
    pub fn login_panel_confirm_delete(&mut self) {
        let Some(panel) = self
            .session_mgr
            .current_mut()
            .session_panels
            .get_mut::<login_panel::LoginPanel>()
        else {
            return;
        };
        let cfg = self.services.peri_config.clone();
        let mut cfg_guard = cfg.write();
        let deleted_name = panel
            .providers
            .get(panel.cursor())
            .map(|p| p.display_name().to_string())
            .unwrap_or_default();
        panel.confirm_delete(&mut cfg_guard);
        if !deleted_name.is_empty() {
            self.session_mgr
                .current_mut()
                .messages
                .view_messages
                .push(MessageViewModel::system(self.services.lc.tr_args(
                    "app-provider-deleted",
                    &[("name".into(), deleted_name.into())],
                )));
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
        }
        if let Some(p) = agent::LlmProvider::from_config(&cfg_guard) {
            self.services.provider_name = p.display_name().to_string();
            self.services.model_name = p.model_name().to_string();
        }
    }
}
