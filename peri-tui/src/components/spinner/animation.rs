/// footer 活动行的帧集（16 帧，100ms/帧推进）——宿主持有，组件只按帧序号取模。
pub const BRAILLE_FRAMES: &[char] = &[
    '✳', '✴', '✵', '✶', '✷', '✸', '✹', '✺', '✻', '✼', '❃', '❊', '✼', '✻', '✺', '✸',
];

/// transcript running 符号的 braille 动画帧（10 帧，100ms/帧推进）——
/// 单字符宽度的方块点阵旋转效果（§8.2 动画；footer spinner 保持独立帧集）。
pub const BRAILLE_LOADING_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

#[must_use]
pub fn tick_to_frame(tick: u64) -> char {
    BRAILLE_FRAMES[(tick as usize) % BRAILLE_FRAMES.len()]
}

pub fn format_elapsed(elapsed_ms: u64) -> String {
    let secs = elapsed_ms / 1000;
    let mins = secs / 60;
    let secs = secs % 60;
    if mins > 0 {
        format!("{}m {}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}

#[must_use]
pub fn format_tokens(count: usize) -> String {
    if count >= 1000 {
        let k = count as f64 / 1000.0;
        if k >= 10.0 {
            format!("{:.0}k", k)
        } else {
            format!("{:.1}k", k)
        }
    } else {
        count.to_string()
    }
}

#[cfg(test)]
#[path = "animation_test.rs"]
mod tests;
