//! 全局原子：驱动运行时主题切换。
//!
//! 三个 Atom 覆盖从完整主题定义到 color-level 的各级消费：
//! - THEME_ATOM: 完整 ThemeDefinition（Arc 包装，非 Copy）
//! - PALETTE_ATOM: ratatui-kit Palette（Copy，直接驱动 PaletteProvider）
//! - PERI_COLORS_ATOM: PeriColors（Arc 包装，非 Copy）

use std::sync::Arc;

use ratatui_kit::prelude::{Atom, Palette};

use crate::bridge::ThemeDefinitionExt;
use crate::builtin;
use crate::peri_colors::PeriColors;
use crate::theme::ThemeDefinition;

/// 全局主题定义 Atom。
pub static THEME_ATOM: Atom<Arc<ThemeDefinition>> = Atom::new(|| Arc::new(builtin::dark_theme()));

/// 全局 ratatui-kit Palette Atom（Copy）。
pub static PALETTE_ATOM: Atom<Palette> = Atom::new(Palette::default);

/// 全局 PeriColors Atom。
pub static PERI_COLORS_ATOM: Atom<Arc<PeriColors>> = Atom::new(|| Arc::new(PeriColors::default()));

/// 一次性初始化三个 atom：从给定主题派生 Palette 和 PeriColors。
pub fn init_theme_atoms(theme: Arc<ThemeDefinition>) {
    let palette = theme.to_palette();
    let peri = Arc::new(theme.to_peri_colors());

    // 直接写入底层 state（惰性创建）
    let _ = THEME_ATOM.state();
    // Note: Atom::state() 创建 state 并返回句柄，但句柄的 set/set_value
    // 需写回具体存储。我们通过 state().set() 更新值。
    let mut theme_state = THEME_ATOM.state();
    theme_state.set(theme);

    let mut palette_state = PALETTE_ATOM.state();
    palette_state.set(palette);

    let mut peri_state = PERI_COLORS_ATOM.state();
    peri_state.set(peri);
}
