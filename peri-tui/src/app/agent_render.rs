use super::*;

impl App {
    /// 添加系统通知并记录锚点位置。
    pub(crate) fn push_system_note(&mut self, content: String) {
        let session = self.session_mgr.current_mut();
        session.messages.push_system_note(content);
        session.messages.message_cache = None;
    }
}
