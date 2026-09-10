use super::*;

#[test]
fn right_align_col_keeps_wide_screen_value() {
    // 宽屏（≥100 列）：保持 40 列右对齐目标
    assert_eq!(right_align_col(100), 40);
    assert_eq!(right_align_col(120), 40);
}

#[test]
fn right_align_col_shrinks_on_narrow_screen() {
    // 窄屏：收缩到右列可容纳宽度（面板 - 45% 左列 - 分隔线 - 4 余量）
    assert_eq!(right_align_col(80), 39); // 80-36-1-4
    assert_eq!(right_align_col(60), 28); // 60-27-1-4
    assert_eq!(right_align_col(50), 23); // 50-22-1-4
    assert_eq!(right_align_col(40), 17); // 40-18-1-4
}

#[test]
fn right_align_col_never_underflows() {
    // 极窄：saturating 保证不为负
    assert_eq!(right_align_col(10), 1); // 10-4-1-4
    assert_eq!(right_align_col(0), 0); // 0*45/100=0 → 0-0-1 saturating → 0
}

#[test]
fn pad_fits_align_column() {
    // pad + key + value = align（值右边缘对齐），且 pad 不为负
    let align = right_align_col(60);
    let key_len = 9; // "    Model "
    let value_len = 12; // "gpt-5.6-luna"
    let pad = align.saturating_sub(key_len + value_len);
    assert_eq!(pad + key_len + value_len, align);
    assert!(pad < 32, "行宽应不超过右列可视宽度");
}

#[path = "model/commit_test.rs"]
mod commit;
