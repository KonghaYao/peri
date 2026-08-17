use std::hash::{Hash, Hasher};

// ---------------------------------------------------------------------------
// Hash 辅助函数
// ---------------------------------------------------------------------------

/// 内容哈希——rebuild 时用于检测是否需重新渲染。
pub fn tui_hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// 滚动哈希的乘法因子（奇数，保证乘法可逆，避免信息丢失）。
const HASH_ROLL_MUL: u64 = 0x9E37_79B9_7F4A_7C15;
/// 组合哈希的乘法因子——与滚动因子区分，降低结构相关性。
const HASH_COMBINE_MUL: u64 = 0xC2B2_AE3D_27D4_EB4F;

/// 对文本按字节做滚动哈希。
///
/// 分块无关：`tui_hash_roll("ab") == tui_hash_roll_update(tui_hash_roll_update(0, "a"), "b")`，
/// 因此流式追加时增量维护与一次性全量计算产出相同值——相同内容必然产生相同 hash，
/// 且增量路径不需要保留 chunk 边界历史。
pub fn tui_hash_roll(text: &str) -> u64 {
    let mut h: u64 = 0;
    for &b in text.as_bytes() {
        h = h.wrapping_mul(HASH_ROLL_MUL).wrapping_add(u64::from(b));
    }
    h
}

/// 滚动哈希的增量更新：在已有滚动值 `h` 上追加 `chunk` 的字节。
pub fn tui_hash_roll_update(mut h: u64, chunk: &str) -> u64 {
    for &b in chunk.as_bytes() {
        h = h.wrapping_mul(HASH_ROLL_MUL).wrapping_add(u64::from(b));
    }
    h
}

/// 将两个 u64 哈希值确定性地组合为一个（内容敏感）。
pub fn tui_hash_combine(h: u64, x: u64) -> u64 {
    h.wrapping_mul(HASH_COMBINE_MUL).wrapping_add(x)
}
