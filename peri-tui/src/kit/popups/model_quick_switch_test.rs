//! Tests

use super::*;

fn make_cfg() -> crate::config::PeriConfig {
    let mut cfg = crate::config::PeriConfig::default();
    cfg.config.providers.push(crate::config::ProviderConfig {
        id: "anthropic".to_string(),
        name: Some("Anthropic".to_string()),
        models: crate::config::ProviderModels {
            opus: "claude-opus".to_string(),
            sonnet: "claude-sonnet".to_string(),
            haiku: "claude-haiku".to_string(),
            fable: String::new(),
        },
        ..Default::default()
    });
    cfg.config.profiles.opus.model = Some("custom-opus".to_string());
    cfg.config.profiles.haiku.effort = "high".to_string();
    cfg
}

#[test]
fn test_quick_switch_rows_default_profile() {
    let rows = quick_switch_rows(&make_cfg());
    assert_eq!(rows.len(), 4);
    // fable 档位：模型空 → 回退 provider.models 的 fable → opus 映射
    assert_eq!(rows[0].alias, "fable");
    assert_eq!(rows[0].model, "claude-opus");
}

#[test]
fn test_quick_switch_rows_profile_model_wins() {
    let rows = quick_switch_rows(&make_cfg());
    // opus：profile.model 优先于 provider.models 映射
    assert_eq!(rows[1].alias, "opus");
    assert_eq!(rows[1].model, "custom-opus");
}

#[test]
fn test_quick_switch_rows_no_provider_fallback_to_alias() {
    // 无 provider 时 model 回退 alias 名
    let cfg = crate::config::PeriConfig::default();
    let rows = quick_switch_rows(&cfg);
    assert_eq!(rows.len(), 4);
    for row in &rows {
        assert_eq!(row.model, row.alias);
    }
}

/// 行布局契约：四档行数必须与 PROFILE_KEYS 匹配，
/// 鼠标 hover/点击反推行号依赖该不变量。
#[test]
fn test_row_layout_contract() {
    assert_eq!(ROW_COUNT, PROFILE_KEYS.len());
}

#[test]
fn test_truncate_str_short() {
    assert_eq!(truncate_str("hello", 10), "hello");
}

#[test]
fn test_truncate_str_long() {
    // 总显示宽度 ≤ max_width：内容 4 宽 + 省略号 1 宽 = 5
    assert_eq!(truncate_str("hello world", 5), "hell…");
}

#[test]
fn test_truncate_str_cjk() {
    // 中文字符 1 char = 2 显示宽度：内容 2 宽 + 省略号 = 3 ≤ 4
    assert_eq!(truncate_str("你好世界朋友", 4), "你…");
    // 恰好容纳一个完整中文字符 + 省略号
    assert_eq!(truncate_str("你好世界", 5), "你好…");
}

// ── 定位 ────────────────────────────────────────────────────────────────

#[test]
fn test_popup_width_fits_content() {
    let rows = quick_switch_rows(&make_cfg());
    let w = popup_width(&rows);
    assert!((POPUP_WIDTH_MIN..=POPUP_WIDTH_MAX).contains(&w));
    // 内容宽度（最宽行 + padding）应不超过弹窗宽度
    let max_content = rows
        .iter()
        .map(|r| format!(" {} {} {}", "❯", r.alias, r.model).as_str().width())
        .max()
        .unwrap();
    assert!(w as usize >= max_content + 4);
}

#[test]
fn test_position_at_anchor_above() {
    // 锚点在屏幕中部：弹窗显示在锚点上方，gap 1 行
    let (x, y) = position_at_anchor(40, 30, 60, POPUP_HEIGHT, 120, 40);
    assert_eq!(x, 42);
    assert_eq!(y, 23); // 30 - 6 - 1
}

#[test]
fn test_position_at_anchor_flip_below_when_no_room() {
    // 锚点太靠上（y=5 < 6+1）：翻转到锚点下方
    let (x, y) = position_at_anchor(10, 5, 60, POPUP_HEIGHT, 120, 40);
    assert_eq!(y, 7); // 5 + 2
    assert_eq!(x, 12);
}

#[test]
fn test_position_at_anchor_clamp_right_edge() {
    // 锚点贴近右缘：x clamp 到 term_w - w
    let (x, _) = position_at_anchor(110, 30, 60, POPUP_HEIGHT, 120, 40);
    assert_eq!(x, 60); // 120 - 60
}

#[test]
fn test_position_at_anchor_stays_in_screen_at_bottom() {
    // 锚点贴屏幕底（最后一行）：弹窗仍在屏内，不溢出
    let (_, y) = position_at_anchor(10, 39, 60, POPUP_HEIGHT, 120, 40);
    assert_eq!(y, 32); // 39 - 6 - 1
    // 锚点贴右缘：x clamp 到 term_w - w
    let (x, _) = position_at_anchor(119, 30, 60, POPUP_HEIGHT, 120, 40);
    assert_eq!(x, 60);
}

// ── hover/点击行号反推 ──────────────────────────────────────────────────

fn area() -> Rect {
    Rect::new(42, 20, 60, POPUP_HEIGHT)
}

#[test]
fn test_row_index_at_hits() {
    // 内容区从 area.y + 1（top border 之下）起：四档行在 area.y+1 ..= area.y+4
    let a = area();
    assert_eq!(row_index_at(a.y + 1, a.x + 5, &a), Some(0)); // fable
    assert_eq!(row_index_at(a.y + 2, a.x + 5, &a), Some(1)); // opus
    assert_eq!(row_index_at(a.y + 3, a.x + 5, &a), Some(2)); // sonnet
    assert_eq!(row_index_at(a.y + 4, a.x + 5, &a), Some(3)); // haiku
}

#[test]
fn test_row_index_at_misses() {
    let a = area();
    // top border / bottom border
    assert_eq!(row_index_at(a.y, a.x + 5, &a), None); // top border
    assert_eq!(row_index_at(a.y + 5, a.x + 5, &a), None); // bottom border
    // 水平区域外
    assert_eq!(row_index_at(a.y + 3, a.x - 1, &a), None);
    assert_eq!(row_index_at(a.y + 3, a.x + a.width, &a), None);
}
