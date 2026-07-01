//! 全局 Atom 定义——替代部分 Effect 变体。
//!
//! 使用 ratatui-kit StoreState<T> 作为 Copy 句柄的全局状态容器。声明为
//! pub static，在组件中通过 use_store(&ATOM) 订阅。写入自动唤醒订阅组件。
//!
//! 类型别名：pub type Atom<T> = StoreState<T>（保持与设计文档一致的命名）。

use ratatui_kit::prelude::StoreState;
use peri_acp_types::view_model::ViewModel;
use std::time::Instant;

/// 类型别名：将 StoreState 映射为 Atom，保持命名一致性
pub type Atom<T> = StoreState<T>;

/// ACP 状态快照（轻量投影，不含大对象）
#[derive(Debug, Clone)]
pub struct AcpStateSnapshot {
    pub variant: u8, // 0=Idle, 1=Streaming, 2=Modal, 3=Switching
    pub view_count: usize,
    pub is_loading: bool,
    pub popup_active: bool,
    pub wizard_active: bool,
    pub at_mention_active: bool,
    pub slash_hint_active: bool,
}

impl Default for AcpStateSnapshot {
    fn default() -> Self {
        Self {
            variant: 0,
            view_count: 0,
            is_loading: false,
            popup_active: false,
            wizard_active: false,
            at_mention_active: false,
            slash_hint_active: false,
        }
    }
}

/// Session ViewModels 快照
#[derive(Debug, Clone, Default)]
pub struct ViewModelsSnapshot {
    pub committed: Vec<ViewModel>,
    pub current_turn: Vec<ViewModel>,
}

// ── 全局 Atom 声明 ──

pub static ACP_STATE: Atom<AcpStateSnapshot> = Atom::new(AcpStateSnapshot::default());
pub static VIEW_MODELS: Atom<ViewModelsSnapshot> = Atom::new(ViewModelsSnapshot::default());
pub static SCROLL_OFFSET: Atom<u16> = Atom::new(0);

/// 状态栏瞬时高亮计时器
pub static MODEL_HIGHLIGHT_UNTIL: Atom<Option<Instant>> = Atom::new(None);
pub static PROVIDER_HIGHLIGHT_UNTIL: Atom<Option<Instant>> = Atom::new(None);
pub static MODE_HIGHLIGHT_UNTIL: Atom<Option<Instant>> = Atom::new(None);

/// @mention / slash_hint / popup 激活状态
pub static AT_MENTION_ACTIVE: Atom<bool> = Atom::new(false);
pub static SLASH_HINT_ACTIVE: Atom<bool> = Atom::new(false);
pub static POPUP_ACTIVE: Atom<bool> = Atom::new(false);
