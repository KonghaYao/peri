//! 历史详情的中性绑定：直接复用 V1 窗口模型的持久身份与描述符类型。
//!
//! V2 只扩展 `HistoryV2BodyBinding` 的表示元数据；两套详情走同一组类型，避免在
//! 详情面板与发布原子之间出现同构但不兼容的副本。

pub use crate::kit::history_window::{
    DetailDescriptor, HistoryDetailTarget, HistorySource, HistoryV2BodyBinding, PersistentEntryKey,
    PersistentSubItemKey, clear_detail_targets, placeholder_hash, publish_detail_targets,
    remove_detail_targets,
};
