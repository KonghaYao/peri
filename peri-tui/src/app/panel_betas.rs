use super::*;

impl App {
    /// 打开 /betas 面板
    pub fn open_betas_panel(&mut self) {
        let panel = {
            let cfg_guard = self.services.peri_config.read();
            crate::app::betas_panel::BetasPanel::from_config(&cfg_guard)
        };
        self.open_panel(PanelState::Betas(panel));
    }

    /// 关闭 /betas 面板
    pub fn close_betas_panel(&mut self) {
        self.global_panels.close_if(PanelKind::Betas);
    }
}
