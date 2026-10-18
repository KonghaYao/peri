//! S5.6 窗口视口身份：发布锚点/代次，供渲染侧宽度变化后按身份重定位。
//!
//! 不发布行号，也不做行数补偿；重定位始终由 [`HistoryWindow::relocate_anchor`] 按
//! `PersistentEntryKey` 完成。

use super::{AnchorRelocation, HistoryWindow, PersistentEntryKey, WindowAnchor};

/// 渲染单元的稳定身份 token（与 `message_area::viewport_anchor` 同口径）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowUnitToken {
    pub kind: u8,
    pub hash: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowUnitMapping {
    pub token: WindowUnitToken,
    pub entry: PersistentEntryKey,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowViewport {
    pub window_generation: u64,
    pub anchor: Option<WindowAnchor>,
    pub relocation: Option<AnchorRelocation>,
    pub follow_bottom: bool,
    pub unit_token: Option<WindowUnitToken>,
}

impl WindowViewport {
    pub fn from_window(window: &HistoryWindow) -> Self {
        Self {
            window_generation: window.generation(),
            anchor: window.anchor().cloned(),
            relocation: window.anchor().map(|_| window.relocate_anchor()),
            follow_bottom: window.follow_bottom(),
            unit_token: None,
        }
    }
}

/// 把当前窗口锚点发布到 `WINDOW_VIEWPORT`。宽度变化/淘汰后渲染侧重新读取并按身份重定位。
pub fn publish_window_viewport(window: &HistoryWindow) {
    crate::kit::atoms::WINDOW_VIEWPORT.set(Some(WindowViewport::from_window(window)));
}

pub fn clear_window_viewport() {
    crate::kit::atoms::WINDOW_VIEWPORT.set(None);
    clear_window_unit_mappings();
    crate::kit::atoms::WINDOW_GAP_REQUEST.set(None);
    crate::kit::history_window::clear_detail_targets();
    crate::kit::atoms::HISTORY_INHERITED_NOTICE.set(None);
}

/// 窗口页合并后发布“渲染单元身份 → 持久条目”的映射；保留重复 token，视口侧据此
/// 识别身份歧义（不猜测）。旧页映射保留，会话 reset 清空。
pub fn publish_window_unit_mappings(mappings: Vec<WindowUnitMapping>) {
    let handle = crate::kit::atoms::WINDOW_UNIT_KEYS.state();
    handle.write().extend(mappings);
}

pub fn clear_window_unit_mappings() {
    crate::kit::atoms::WINDOW_UNIT_KEYS.set(Vec::new());
}

pub fn remove_window_unit_mappings(keys: &std::collections::HashSet<PersistentEntryKey>) {
    let handle = crate::kit::atoms::WINDOW_UNIT_KEYS.state();
    handle
        .write()
        .retain(|mapping| !keys.contains(&mapping.entry));
}

/// 真实视口发布：按渲染单元 token 查持久身份；查不到时保留 `anchor=None`，
/// 渲染侧继续使用 gap 占位/既有 `ViewportAnchor`，不按行数补偿。
pub fn publish_viewport_from_token(
    token: WindowUnitToken,
    offset_within_unit: usize,
    follow_bottom: bool,
) {
    let handle = crate::kit::atoms::WINDOW_UNIT_KEYS.state();
    let matches: Vec<_> = handle
        .read()
        .iter()
        .filter(|mapping| mapping.token == token)
        .map(|mapping| mapping.entry.clone())
        .collect();
    let anchor = (matches.len() == 1).then(|| {
        WindowAnchor::new(
            matches[0].clone(),
            None,
            super::WindowAnchorOffset::CharOffset(offset_within_unit as u64),
        )
        .follow_bottom(follow_bottom)
    });
    crate::kit::atoms::WINDOW_VIEWPORT.set(Some(WindowViewport {
        window_generation: crate::kit::atoms::HISTORY_WINDOW_GENERATION.get(),
        anchor,
        relocation: None,
        follow_bottom,
        unit_token: Some(token),
    }));
}

pub fn update_window_viewport_follow(follow_bottom: bool) {
    let handle = crate::kit::atoms::WINDOW_VIEWPORT.state();
    let mut viewport = handle.write();
    if let Some(viewport) = viewport.as_mut() {
        viewport.follow_bottom = follow_bottom;
    } else {
        *viewport = Some(WindowViewport {
            window_generation: crate::kit::atoms::HISTORY_WINDOW_GENERATION.get(),
            follow_bottom,
            ..WindowViewport::default()
        });
    }
}

/// 视口顶行没有可映射身份（gap/折叠/未映射单元）：显式发布无锚点视口，
/// 让渲染侧保持 gap 占位，不做行数补偿或跳底。
pub fn publish_viewport_unanchored(follow_bottom: bool) {
    crate::kit::atoms::WINDOW_VIEWPORT.set(Some(WindowViewport {
        window_generation: crate::kit::atoms::HISTORY_WINDOW_GENERATION.get(),
        anchor: None,
        relocation: None,
        follow_bottom,
        unit_token: None,
    }));
}
