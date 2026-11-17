//! Composer text and cursor snapshot published at draw time.

use ratatui_kit::prelude::Atom as AtomStatic;

/// 输入区快照（§7.3 cursor 触发源）：input_area 渲染 body 每帧
/// `write_no_update` 写入（同 keepgoing_rect 模式，TUI-RENDER-001 派生缓存），
/// overlay 组件经渲染循环读取最新值。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InputSnapshot {
    /// 输入区当前全文。
    pub text: String,
    /// 光标位置（字符索引，非字节）。
    pub cursor_char: usize,
}

pub static INPUT_SNAPSHOT: AtomStatic<InputSnapshot> = AtomStatic::new(InputSnapshot::default);
