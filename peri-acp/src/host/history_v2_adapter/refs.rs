//! V2 后端内部引用编码：region、稳定条目 id 与 inherited 冻结快照身份。
//!
//! `beforeCursor`/`afterCursor`/`bodyRef` 对服务层与客户端都是不透明字符串。own 沿用
//! 裸 message id；inherited 必须同时携带冻结快照与祖先会话，且显式带 `i:` 前缀，
//! 避免把 inherited 位置误判为 own（反之亦然）。

use base64::Engine as _;
use peri_acp_types::history_common::{HistoryInheritedSnapshot, HistorySource as WindowSource};
use peri_acp_types::history_v2::{HistoryInheritedSource, HistorySource as V2Source};
use peri_acp_types::messages::MessageId;
use peri_acp_types::thread::ThreadId;

/// own 历史的排序 region。
pub(super) const OWN_REGION: &str = "own";
/// inherited 历史的排序 region；必须与 own 保持稳定先后（inherited < own）。
pub(super) const INHERITED_REGION: &str = "inherited";

const INHERITED_PREFIX: &str = "i:";

/// 子会话冻结前缀的稳定身份。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct InheritedRef {
    snapshot_id: String,
    ancestor_session_id: String,
}

impl InheritedRef {
    /// V2 来源必须同时有冻结快照与直接祖先；旧数据缺祖先时不伪造。
    pub(super) fn from_v2(source: &HistoryInheritedSource) -> Option<Self> {
        Self::new(
            source.snapshot_id.clone(),
            source.ancestor_session_id.clone(),
        )
    }

    pub(super) fn from_snapshot(snapshot: &HistoryInheritedSnapshot) -> Option<Self> {
        Self::new(
            snapshot.snapshot_id.clone(),
            snapshot.ancestor_thread_id.clone()?,
        )
    }

    fn new(snapshot_id: String, ancestor_session_id: String) -> Option<Self> {
        if snapshot_id.is_empty() || ancestor_session_id.is_empty() {
            return None;
        }
        Some(Self {
            snapshot_id,
            ancestor_session_id,
        })
    }

    pub(super) fn window_source(&self) -> WindowSource {
        WindowSource::inherited_with_ancestor(
            self.snapshot_id.clone(),
            Some(ThreadId::from(self.ancestor_session_id.clone())),
        )
    }

    pub(super) fn v2_source(&self) -> V2Source {
        V2Source::Inherited(HistoryInheritedSource {
            snapshot_id: self.snapshot_id.clone(),
            ancestor_session_id: self.ancestor_session_id.clone(),
        })
    }
}

/// 解析后的条目引用；region 由类型显式表达。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum EntryRef {
    Own(MessageId),
    Inherited {
        source: InheritedRef,
        entry_id: MessageId,
    },
}

/// own cursor/bodyRef：稳定的裸 message id。
pub(super) fn encode_own_ref(entry_id: MessageId) -> String {
    entry_id.as_uuid().to_string()
}

/// inherited cursor/bodyRef：`i:{snapshot_base64}:{ancestor_base64}:{message_uuid}`。
pub(super) fn encode_inherited_ref(source: &InheritedRef, entry_id: MessageId) -> String {
    format!(
        "{INHERITED_PREFIX}{}:{}:{}",
        base64(source.snapshot_id.as_bytes()),
        base64(source.ancestor_session_id.as_bytes()),
        entry_id.as_uuid(),
    )
}

/// 只接受本适配器产生的两种形状；其他文本一律 `None`，由调用方映射为 cursor 错误。
pub(super) fn parse_entry_ref(raw: &str) -> Option<EntryRef> {
    let Some(rest) = raw.strip_prefix(INHERITED_PREFIX) else {
        return uuid::Uuid::parse_str(raw)
            .ok()
            .map(MessageId::from)
            .map(EntryRef::Own);
    };
    let mut parts = rest.splitn(3, ':');
    let snapshot = decode(parts.next()?)?;
    let ancestor = decode(parts.next()?)?;
    let entry_id = uuid::Uuid::parse_str(parts.next()?)
        .ok()
        .map(MessageId::from)?;
    let source = InheritedRef::new(snapshot, ancestor)?;
    Some(EntryRef::Inherited { source, entry_id })
}

fn base64(value: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}

fn decode(value: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .ok()?;
    String::from_utf8(bytes).ok()
}
