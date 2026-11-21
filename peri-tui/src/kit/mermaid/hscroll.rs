//! 内联 Mermaid 的横向偏移：按围栏身份保存纯展示状态。
//!
//! 偏移不属于预览状态机——同一份已发布帧可以在不同列被读取，不需要重新
//! 排版。版本号只用于让所属消息的行缓存失效，不参与几何缓存键。

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use peri_tui_components::scrollbar::bounded_offset;

use crate::kit::markdown::MermaidFenceKey;

/// 同时保留的围栏偏移上限；超出时先丢弃其他会话，仍满则整体清空。
/// 偏移只是阅读位置，丢弃后重新打开回到最左列，不影响图内容。
const MAX_ENTRIES: usize = 128;

#[derive(Debug, Clone, Copy)]
struct Stored {
    offset: usize,
    /// 上次观察到的帧宽；帧变窄时把偏移归零，变宽时重新可用。
    content: usize,
    /// 上次观察到的视口宽。resize 会同时改变两者，只 clamp 不归零——
    /// 归零只针对「同一视口下内容变窄」的流式更新。
    viewport: usize,
}

fn offsets() -> &'static Mutex<HashMap<MermaidFenceKey, Stored>> {
    static OFFSETS: OnceLock<Mutex<HashMap<MermaidFenceKey, Stored>>> = OnceLock::new();
    OFFSETS.get_or_init(|| Mutex::new(HashMap::new()))
}

static REVISION: AtomicU64 = AtomicU64::new(0);

/// 偏移版本；`message_revision_for_vm` 把它并入消息版本，驱动行缓存重算。
#[must_use]
pub(crate) fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

fn bump() {
    REVISION.fetch_add(1, Ordering::Relaxed);
}

fn entry<'a>(
    map: &'a mut HashMap<MermaidFenceKey, Stored>,
    key: &MermaidFenceKey,
) -> &'a mut Stored {
    if !map.contains_key(key) {
        if map.len() >= MAX_ENTRIES {
            let session = key.message.session_id.clone();
            map.retain(|existing, _| existing.message.session_id == session);
            if map.len() >= MAX_ENTRIES {
                map.clear();
            }
        }
        map.insert(
            key.clone(),
            Stored {
                offset: 0,
                content: 0,
                viewport: 0,
            },
        );
    }
    map.get_mut(key)
        .expect("the entry was inserted above when the key was absent")
}

/// 本帧生效的偏移：clamp 到当前最大值；帧宽比上次窄时归零。
///
/// 渲染路径读取，不递增版本——命中退化的偏移直接按本帧值绘制，不会自激。
#[must_use]
pub(crate) fn offset_for(key: &MermaidFenceKey, viewport: usize, content: usize) -> usize {
    let mut map = offsets().lock();
    if content <= viewport {
        // 图整体可见：清掉旧列，之后变宽从最左列重新开始。
        if let Some(stored) = map.get_mut(key) {
            stored.offset = 0;
            stored.content = content;
        }
        return 0;
    }
    let stored = entry(&mut map, key);
    if content < stored.content && viewport == stored.viewport {
        stored.offset = 0;
    }
    stored.content = content;
    stored.viewport = viewport;
    stored.offset = stored.offset.min(content.saturating_sub(viewport));
    stored.offset
}

/// 写入绝对偏移（拖拽/轨道点击）；返回是否真的改变了展示位置。
pub(crate) fn set_offset(
    key: &MermaidFenceKey,
    offset: usize,
    viewport: usize,
    content: usize,
) -> bool {
    let max_offset = content.saturating_sub(viewport);
    let next = offset.min(max_offset);
    let mut map = offsets().lock();
    if max_offset == 0 {
        if let Some(stored) = map.get_mut(key) {
            let changed = stored.offset != 0;
            stored.offset = 0;
            stored.content = content;
            stored.viewport = viewport;
            if changed {
                bump();
            }
            return changed;
        }
        return false;
    }
    let stored = entry(&mut map, key);
    let changed = stored.offset != next;
    stored.offset = next;
    stored.content = content;
    stored.viewport = viewport;
    if changed {
        bump();
    }
    changed
}

/// 相对滚动（滚轮/方向键）；`delta` 以列为单位，两端 clamp。
pub(crate) fn scroll_by(
    key: &MermaidFenceKey,
    delta: i32,
    viewport: usize,
    content: usize,
) -> bool {
    let current = offset_for(key, viewport, content);
    let next = bounded_offset(current, delta, content.saturating_sub(viewport));
    set_offset(key, next, viewport, content)
}

#[cfg(test)]
pub(crate) fn reset_for_test() {
    offsets().lock().clear();
    REVISION.store(0, Ordering::Relaxed);
}

#[cfg(test)]
#[path = "hscroll_test.rs"]
mod tests;
