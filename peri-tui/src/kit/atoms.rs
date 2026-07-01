//! 全局 Atom 定义——替代部分 Effect 变体。
//!
//! 使用 ratatui-kit StoreState<T> 作为 Copy 句柄的全局状态容器。通过 OnceLock
//! 在运行时初始化，组件通过 use_store(&atom) 订阅。写入自动唤醒订阅组件。
//!
//! 类型别名：pub type Atom<T> = StoreState<T>（保持与设计文档一致的命名）。

use ratatui_kit::prelude::StoreState;
use peri_acp_types::view_model::ViewModel;
use std::sync::OnceLock;
use std::time::Instant;

/// 类型别名：将 StoreState 映射为 Atom，保持命名一致性
pub type Atom<T> = StoreState<T>;

/// ACP 状态快照（轻量投影，不含大对象）
#[derive(Debug, Clone, Default)]
pub struct AcpStateSnapshot {
    pub variant: u8, // 0=Idle, 1=Streaming, 2=Modal, 3=Switching
    pub view_count: usize,
    pub is_loading: bool,
    pub popup_active: bool,
    pub wizard_active: bool,
    pub at_mention_active: bool,
#[derive(Debug, Clone, Default)]
pub struct ViewModelsSnapshot {
    pub committed: Vec<ViewModel>,
    pub current_turn: Vec<ViewModel>,
}

// ── 全局 Atom 声明（OnceLock 延迟初始化） ──

pub static ACP_STATE: OnceLock<Atom<AcpStateSnapshot>> = OnceLock::new();
pub static VIEW_MODELS: OnceLock<Atom<ViewModelsSnapshot>> = OnceLock::new();
pub static SCROLL_OFFSET: OnceLock<Atom<u16>> = OnceLock::new();

/// 状态栏瞬时高亮计时器
pub static MODEL_HIGHLIGHT_UNTIL: OnceLock<Atom<Option<Instant>>> = OnceLock::new();
pub static PROVIDER_HIGHLIGHT_UNTIL: OnceLock<Atom<Option<Instant>>> = OnceLock::new();
pub static MODE_HIGHLIGHT_UNTIL: OnceLock<Atom<Option<Instant>>> = OnceLock::new();

/// @mention / slash_hint / popup 激活状态
pub static AT_MENTION_ACTIVE: OnceLock<Atom<bool>> = OnceLock::new();
pub static SLASH_HINT_ACTIVE: OnceLock<Atom<bool>> = OnceLock::new();
pub static POPUP_ACTIVE: OnceLock<Atom<bool>> = OnceLock::new();

/// 提交通道：InputArea 写入提交文本 → ACP bridge 读取并发送
pub static SUBMIT_PENDING: OnceLock<Atom<bool>> = OnceLock::new();
pub static SUBMIT_TEXT: OnceLock<Atom<String>> = OnceLock::new();

/// 初始化所有全局 Atom。
///
/// 必须在 tokio 运行时启动后、任何组件渲染前调用。
pub fn init_atoms() {
    ACP_STATE.get_or_init(|| Atom::new(AcpStateSnapshot::default()));
    VIEW_MODELS.get_or_init(|| Atom::new(ViewModelsSnapshot::default()));
    SCROLL_OFFSET.get_or_init(|| Atom::new(0));
    MODEL_HIGHLIGHT_UNTIL.get_or_init(|| Atom::new(None));
    PROVIDER_HIGHLIGHT_UNTIL.get_or_init(|| Atom::new(None));
    MODE_HIGHLIGHT_UNTIL.get_or_init(|| Atom::new(None));
    AT_MENTION_ACTIVE.get_or_init(|| Atom::new(false));
    SLASH_HINT_ACTIVE.get_or_init(|| Atom::new(false));
    POPUP_ACTIVE.get_or_init(|| Atom::new(false));
    SUBMIT_PENDING.get_or_init(|| Atom::new(false));
    SUBMIT_TEXT.get_or_init(|| Atom::new(String::new()));
}
