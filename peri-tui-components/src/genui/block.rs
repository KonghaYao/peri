//! Stable identity for a rendered OpenUI block.

use std::hash::{Hash, Hasher};

use peri_genui::ParseResult;

/// 同一源码与入口的重复块共享状态：当前 Markdown 段没有独立的持久块 ID，
/// 只能用源码 + 语句 ID 的稳定哈希区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GenUiBlockId(u64);

impl GenUiBlockId {
    #[must_use]
    pub fn new(source: &str, parsed: &ParseResult) -> Self {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        source.hash(&mut hash);
        parsed
            .root
            .as_ref()
            .and_then(|root| root.statement_id.as_deref())
            .hash(&mut hash);
        Self(hash.finish())
    }
}
