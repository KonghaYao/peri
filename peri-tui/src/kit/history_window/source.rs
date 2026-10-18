//! 持久来源与条目身份。

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
