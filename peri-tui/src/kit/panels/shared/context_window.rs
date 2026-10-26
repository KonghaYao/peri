//! `profile.contextWindow` 输入解析（Model 面板的权威实现）。

/// 解析上下文窗口输入。
///
/// trim 后只接受 ASCII 十进制数字，解析 `u32` 且 `>0`；拒绝空值、符号、零、
/// 溢出、`128k`、小数、内部空格、Unicode 数字和多行。单位是 tokens，
/// 不根据模型名推断任何容量默认值。
pub(crate) fn parse_context_window(raw: &str) -> Option<u32> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    trimmed.parse::<u32>().ok().filter(|value| *value > 0)
}
