//! 已交付页面的正文描述符定位。
//!
//! 正文分块、预算、contentVersion 与表示校验由详情面板
//! (`kit::panels::history_detail`) 承担；窗口模型只负责从已交付条目中找到
//! opaque 子项的描述符，不保存正文状态。

use peri_acp_types::history_v2::{HistoryBodyDescriptor, HistorySource};

use super::HistoryV2Window;
use super::validate::{sub_item_descriptor, sub_item_id};

/// 定位一个可外置正文的展示子项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2BodyLocator {
    pub source: HistorySource,
    pub entry_id: String,
    pub sub_item_id: String,
}

impl HistoryV2Window {
    /// 占位展示所需的描述符（总字节/块大小/表示），不要求正文已请求。
    pub fn body_descriptor(&self, locator: &HistoryV2BodyLocator) -> Option<HistoryBodyDescriptor> {
        let entry = self
            .items
            .iter()
            .find(|entry| entry.entry_id == locator.entry_id && entry.source == locator.source)?;
        let sub_item = entry
            .sub_items
            .iter()
            .find(|item| sub_item_id(item) == locator.sub_item_id)?;
        sub_item_descriptor(sub_item).cloned()
    }
}
