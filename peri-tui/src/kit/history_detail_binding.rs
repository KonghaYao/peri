//! 历史详情的中性绑定：持久条目身份、V2 正文身份与详情目标发布。
//!
//! 这些类型不属于已删除的 V1 窗口模型；详情正文只通过 V2 `_peri/history`
//! `body` 动作读取。缺少 V2 绑定的 descriptor 在详情面板显式失败。

use super::tui_render_unit::tui_hash_str;

/// 持久来源。与 S4 wire 形状一致；S4 尚无 `ancestor_thread_id`，本阶段不补造。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HistorySource {
    Own,
    Inherited {
        frozen_snapshot_id: String,
        /// 冻结前缀的直接祖先会话 id；旧事实缺失时为 None，不伪造。
        ancestor_thread_id: Option<String>,
    },
}

impl HistorySource {
    pub fn own() -> Self {
        Self::Own
    }

    pub fn inherited(frozen_snapshot_id: impl Into<String>) -> Self {
        Self::Inherited {
            frozen_snapshot_id: frozen_snapshot_id.into(),
            ancestor_thread_id: None,
        }
    }

    pub fn inherited_with_ancestor(
        frozen_snapshot_id: impl Into<String>,
        ancestor_thread_id: Option<String>,
    ) -> Self {
        Self::Inherited {
            frozen_snapshot_id: frozen_snapshot_id.into(),
            ancestor_thread_id,
        }
    }

    pub fn is_own(&self) -> bool {
        matches!(self, Self::Own)
    }

    pub fn frozen_snapshot_id(&self) -> Option<&str> {
        match self {
            Self::Own => None,
            Self::Inherited {
                frozen_snapshot_id, ..
            } => Some(frozen_snapshot_id),
        }
    }

    pub fn ancestor_thread_id(&self) -> Option<&str> {
        match self {
            Self::Own => None,
            Self::Inherited {
                ancestor_thread_id, ..
            } => ancestor_thread_id.as_deref(),
        }
    }
}

/// 持久条目身份：来源 + `entryId`（`{session}:{message}`）。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PersistentEntryKey {
    pub source: HistorySource,
    pub entry_id: String,
}

impl PersistentEntryKey {
    pub fn new(source: HistorySource, entry_id: impl Into<String>) -> Self {
        Self {
            source,
            entry_id: entry_id.into(),
        }
    }

    pub fn own(entry_id: impl Into<String>) -> Self {
        Self::new(HistorySource::Own, entry_id)
    }

    /// `entryId` 的 wire 形式本身是 `{session}:{message}`；请求仍需 sessionId。
    pub fn entry_id_session(&self) -> String {
        self.entry_id
            .rsplit_once(':')
            .map_or_else(|| self.entry_id.clone(), |(session, _)| session.to_string())
    }
}

/// 子项身份单独一层：不能用 `entryId` 代替子项。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PersistentSubItemKey {
    pub entry: PersistentEntryKey,
    pub sub_item: u32,
}

impl PersistentSubItemKey {
    pub fn new(entry: PersistentEntryKey, sub_item: u32) -> Self {
        Self { entry, sub_item }
    }
}

/// 超大条目的持久身份与详情描述符（不持正文）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailDescriptor {
    pub key: PersistentEntryKey,
    pub total_bytes: u64,
    pub chunk_size: u32,
    pub snapshot_token: String,
    pub read_ticket: String,
    /// V2 绑定：正文块走 `_peri/history` 的 `body` 动作，身份来自已交付页面。
    /// `None` = 不可读取；详情面板必须显式失败，不得调用旧协议。
    pub v2: Option<HistoryV2BodyBinding>,
}

/// 一个 V2 正文的完整请求身份；只保存已交付的 descriptor 与窗口令牌。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryV2BodyBinding {
    pub session_id: String,
    pub window_id: String,
    pub snapshot_token: String,
    pub read_ticket: String,
    pub source: peri_acp_types::history_v2::HistorySource,
    pub entry_id: String,
    pub sub_item_id: String,
    pub body_ref: String,
    /// 页面描述符声明的表示方式；正文块必须一致。
    pub representation: peri_acp_types::history_v2::HistoryRepresentation,
    /// 页面描述符声明的媒体类型；仅用于身份记录与展示校验。
    pub media_type: String,
}

impl DetailDescriptor {
    pub fn new(
        source: HistorySource,
        entry_id: impl Into<String>,
        total_bytes: u64,
        chunk_size: u32,
        snapshot_token: impl Into<String>,
        read_ticket: impl Into<String>,
    ) -> Self {
        let key = PersistentEntryKey::new(source, entry_id);
        Self {
            key,
            total_bytes,
            chunk_size,
            snapshot_token: snapshot_token.into(),
            read_ticket: read_ticket.into(),
            v2: None,
        }
    }

    /// V2 描述符：`key` 仍用真实条目身份；子项身份在 binding 里单列。
    pub fn with_v2(mut self, binding: HistoryV2BodyBinding) -> Self {
        self.v2 = Some(binding);
        self
    }
}

pub fn publish_detail_targets(targets: Vec<HistoryDetailTarget>) {
    let handle = crate::kit::atoms::HISTORY_DETAIL_TARGETS.state();
    let mut all = handle.write();
    for target in targets {
        all.retain(|existing| existing.descriptor.key != target.descriptor.key);
        all.push(target);
    }
}

pub fn clear_detail_targets() {
    crate::kit::atoms::HISTORY_DETAIL_TARGETS.set(Vec::new());
}

pub fn remove_detail_targets(keys: &std::collections::HashSet<PersistentEntryKey>) {
    let handle = crate::kit::atoms::HISTORY_DETAIL_TARGETS.state();
    handle
        .write()
        .retain(|target| !keys.contains(&target.descriptor.key));
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryDetailTarget {
    pub content_hash: u64,
    pub descriptor: DetailDescriptor,
}

/// 占位 note 的缓存键：把持久身份混入 hash，避免同尺寸占位互相命中。
pub fn placeholder_hash(key: &PersistentEntryKey, total_bytes: u64) -> u64 {
    tui_hash_str(&format!(
        "{}|{}|{}",
        key.source.frozen_snapshot_id().unwrap_or("own"),
        key.entry_id,
        total_bytes
    ))
}
