//! 文档版本与 writer 入队同步提交；实际写入确认在文档锁外等待。

use super::*;
use crate::protocol::notifications::{
    did_change_notification, did_open_notification, did_save_notification,
};

impl LspClient {
    /// 文件同步: didOpen；只有已入队的首次通知才占据该连接的缓存。
    pub async fn did_open(&self, uri: &str, language_id: &str, text: &str) -> Result<(), LspError> {
        let connection = self.ready_connection()?;
        if connection.open_files.lock().contains_key(uri) {
            return Ok(());
        }
        let permit = connection.dispatcher.reserve_notification().await?;
        let completion = {
            let mut open = connection.open_files.lock();
            if open.contains_key(uri) {
                return Ok(());
            }
            let version = open.len() as i32 + 1;
            let notification = did_open_notification(uri, language_id, version, text);
            let completion = permit.enqueue(&notification)?;
            open.insert(uri.to_string(), OpenFileInfo { version });
            completion
        };
        completion.wait().await
    }

    /// 文件同步: didChange；首次调用转 didOpen，版本顺序与入队顺序相同。
    pub async fn did_change(&self, uri: &str, text: &str) -> Result<(), LspError> {
        let connection = self.ready_connection()?;
        let permit = connection.dispatcher.reserve_notification().await?;
        let completion = {
            let mut open = connection.open_files.lock();
            let (version, notification) = if let Some(info) = open.get(uri) {
                let version = info.version + 1;
                (version, did_change_notification(uri, version, text))
            } else {
                let version = open.len() as i32 + 1;
                (
                    version,
                    did_open_notification(uri, &Self::infer_language_id(uri), version, text),
                )
            };
            let completion = permit.enqueue(&notification)?;
            open.insert(uri.to_string(), OpenFileInfo { version });
            completion
        };
        completion.wait().await
    }

    /// 文件同步: didSave。
    pub async fn did_save(&self, uri: &str) -> Result<(), LspError> {
        let notification = did_save_notification(uri, None);
        self.ready_dispatcher()?
            .send_notification(&notification)
            .await
    }
}
