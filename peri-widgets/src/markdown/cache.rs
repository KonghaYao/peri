//! Markdown 解析结果的 LRU 缓存
//!
//! 缓存 key = (内容哈希, 渲染宽度)，value = Text<'static>。
//! 通过全局单例暴露，渲染线程和增量解析共享同一缓存。

use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;

use lru::LruCache;
use parking_lot::Mutex;
use ratatui::text::Text;
use std::sync::LazyLock;

/// 缓存容量上限
const CACHE_CAPACITY: usize = 256;

/// 全局 Markdown 缓存单例
static MARKDOWN_CACHE: LazyLock<MarkdownCache> = LazyLock::new(MarkdownCache::new);

/// Markdown 解析结果 LRU 缓存
///
/// key = (内容哈希, 渲染宽度 u16)
/// value = Text<'static>（已解析的渲染结果）
pub struct MarkdownCache {
    cache: Mutex<LruCache<CacheKey, Text<'static>>>,
}

/// 缓存 key：内容哈希 + 渲染宽度
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CacheKey {
    content_hash: u64,
    max_width: u16,
}

impl MarkdownCache {
    /// 创建新的缓存实例
    fn new() -> Self {
        let cap = NonZeroUsize::new(CACHE_CAPACITY).expect("CACHE_CAPACITY > 0");
        Self {
            cache: Mutex::new(LruCache::new(cap)),
        }
    }

    /// 获取全局缓存单例的引用
    pub fn global() -> &'static Self {
        &MARKDOWN_CACHE
    }

    /// 查询缓存，命中返回克隆的 Text
    pub fn get(&self, content: &str, max_width: u16) -> Option<Text<'static>> {
        let key = self.make_key(content, max_width);
        let mut guard = self.cache.lock();
        guard.get(&key).cloned()
    }

    /// 插入解析结果到缓存
    pub fn put(&self, content: &str, max_width: u16, text: Text<'static>) {
        let key = self.make_key(content, max_width);
        let mut guard = self.cache.lock();
        guard.put(key, text);
    }

    /// 生成缓存 key
    fn make_key(&self, content: &str, max_width: u16) -> CacheKey {
        use std::collections::hash_map::DefaultHasher;
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        CacheKey {
            content_hash: hasher.finish(),
            max_width,
        }
    }

    /// 清空缓存（测试用）
    #[allow(dead_code)]
    pub fn clear(&self) {
        let mut guard = self.cache.lock();
        guard.clear();
    }

    /// 当前缓存条目数（测试用）
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        let guard = self.cache.lock();
        guard.len()
    }

    /// 当前缓存是否为空
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 创建指定容量的缓存实例（测试用）
    #[cfg(test)]
    pub fn new_for_test_with_capacity(cap: usize) -> Self {
        let cap = NonZeroUsize::new(cap).expect("capacity > 0");
        Self {
            cache: Mutex::new(LruCache::new(cap)),
        }
    }

    /// 创建默认容量缓存实例（测试用）
    #[cfg(test)]
    pub fn new_for_test() -> Self {
        Self::new()
    }
}
