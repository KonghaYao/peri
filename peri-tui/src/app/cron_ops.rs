impl crate::app::App {
    /// CronPanel: 请求删除当前任务（进入确认状态）
    /// v2: 面板操作已迁移至 state machine，此方法为 no-op。
    pub fn cron_panel_request_delete(&mut self) {}

    /// CronPanel: 确认删除当前任务
    /// v2: 面板操作已迁移至 state machine，此方法为 no-op。
    pub fn cron_panel_confirm_delete(&mut self) {}

    /// CronPanel: 取消删除确认
    /// v2: 面板操作已迁移至 state machine，此方法为 no-op。
    pub fn cron_panel_cancel_delete(&mut self) {}
}
