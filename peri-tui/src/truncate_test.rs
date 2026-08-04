use super::*;

#[test]
fn test_truncate_text_short() {
    assert_eq!(truncate_text("hello", 10), "hello");
}

#[test]
fn test_truncate_text_exact() {
    assert_eq!(truncate_text("hello", 5), "hello");
}

#[test]
fn test_truncate_text_long() {
    assert_eq!(truncate_text("abcdefghij", 5), "abcde...");
}

#[test]
fn test_truncate_text_cjk() {
    assert_eq!(truncate_text("你好世界", 2), "你好...");
}

#[test]
fn test_truncate_by_width_short_and_exact() {
    assert_eq!(truncate_by_width("hello", 10), "hello");
    // 恰好宽度不截断
    assert_eq!(truncate_by_width("hello", 5), "hello");
}

#[test]
fn test_truncate_by_width_ascii() {
    // 恰好 10 列不截断（与 truncate_text 语义一致：len <= max 返回原串）
    assert_eq!(truncate_by_width("abcdefghij", 10), "abcdefghij");
    // 11 个 ASCII = 11 列 → 恰好填满预算的 10 个字符保留，省略号追加（共 11 列）
    assert_eq!(truncate_by_width("abcdefghijk", 10), "abcdefghij…");
}

#[test]
fn test_truncate_by_width_cjk() {
    // CJK 双宽：16 汉字 = 32 列，恰好不截断
    assert_eq!(truncate_by_width(&"字".repeat(16), 32), "字".repeat(16));
    // 17 汉字 = 34 列 → 截断到 16 字 + 省略号 = 33 列
    assert_eq!(
        truncate_by_width(&"字".repeat(17), 32),
        "字".repeat(16) + "…"
    );
    // 输出宽度不超预算
    use unicode_width::UnicodeWidthStr;
    assert!(truncate_by_width(&"字".repeat(40), 32).width() <= 33);
}

#[test]
fn test_truncate_by_width_mixed_ascii_cjk() {
    // "ab" (2) + 15 汉字 (30) = 32 列，恰好不截断
    let s = format!("ab{}", "字".repeat(15));
    assert_eq!(truncate_by_width(&s, 32), s);
    // 再加 1 汉字 = 34 列 → 截断，省略号替换最后 1 列
    let t = truncate_by_width(&format!("ab{}", "字".repeat(16)), 32);
    assert_eq!(t, format!("ab{}…", "字".repeat(15)));
}

#[test]
fn test_truncate_by_width_keeps_emoji_zwj_sequence() {
    // ZWJ 序列（家庭 emoji）作为整体 grapheme，宽度 2，不会被从中间切开
    let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
    let s = format!("{family}{}", "a".repeat(30)); // 2 + 30 = 32 列
    // 预算恰好容纳整个 emoji 序列 + 30 个 a，不截断
    assert_eq!(truncate_by_width(&s, 32), s);
    // 预算 31：emoji(2) + 29 个 a 恰好填满，第 30 个 a 放不下 → 省略号替换
    assert_eq!(
        truncate_by_width(&s, 31),
        format!("{family}{}…", "a".repeat(29))
    );
}

#[test]
fn test_summarize_input_grep_unified_quoted_format() {
    // 关键不变量：streaming 与 view-commit 通道共享此 helper，
    // 同一工具调用必须显示相同格式（带引号）
    let input = serde_json::json!({ "pattern": "TODO" });
    assert_eq!(summarize_input("Grep", &input), r#"pattern: "TODO""#);
    assert_eq!(summarize_input("Glob", &input), r#"pattern: "TODO""#);
}

#[test]
fn test_summarize_input_web_search_quoted_format() {
    let input = serde_json::json!({ "query": "rust async" });
    assert_eq!(
        summarize_input("WebSearch", &input),
        r#"query: "rust async""#
    );
}

#[test]
fn test_summarize_input_read_fallback_path() {
    let input = serde_json::json!({ "path": "/tmp/bar.rs" });
    assert_eq!(summarize_input("Read", &input), "/tmp/bar.rs");
}

#[test]
fn test_summarize_input_empty_object() {
    let input = serde_json::json!({});
    assert_eq!(summarize_input("Read", &input), "(empty input)");
}

#[test]
fn test_summarize_input_non_object_fallback() {
    // 非 Object 的 JSON value 走 `to_string()` 兜底（JSON 字符串带引号）
    let input = serde_json::json!("raw string");
    assert_eq!(summarize_input("Read", &input), "\"raw string\"");
}

#[test]
fn test_shorten_path_for_display_strips_cwd_prefix() {
    let cwd = "/Users/konghayao/code/ai/perihelion";
    assert_eq!(
        shorten_path_for_display(
            "/Users/konghayao/code/ai/perihelion/peri-model/src/protocol/mod.rs",
            cwd,
        ),
        "peri-model/src/protocol/mod.rs"
    );
}

#[test]
fn test_shorten_path_for_display_cwd_with_trailing_separator() {
    assert_eq!(
        shorten_path_for_display("/proj/src/main.rs", "/proj/"),
        "src/main.rs"
    );
}

#[test]
fn test_shorten_path_for_display_keeps_non_cwd_paths() {
    let cwd = "/Users/konghayao/code/ai/perihelion";
    // 非 cwd 前缀的绝对路径保持原样
    assert_eq!(shorten_path_for_display("/tmp/foo.rs", cwd), "/tmp/foo.rs");
    // 相对路径保持原样
    assert_eq!(shorten_path_for_display("src/main.rs", cwd), "src/main.rs");
}

#[test]
fn test_shorten_path_for_display_edge_cases() {
    // 空 cwd → 原样
    assert_eq!(shorten_path_for_display("/a/b.rs", ""), "/a/b.rs");
    // 根目录 cwd → 原样（避免所有绝对路径被裁剪）
    assert_eq!(shorten_path_for_display("/a/b.rs", "/"), "/a/b.rs");
    // path == cwd → 原样（避免空串）
    assert_eq!(shorten_path_for_display("/proj", "/proj"), "/proj");
    // 前缀边界：/project 不是 /proj 的前缀路径
    assert_eq!(
        shorten_path_for_display("/project/x.rs", "/proj"),
        "/project/x.rs"
    );
    // Windows 分隔符
    assert_eq!(
        shorten_path_for_display("C:\\proj\\src\\main.rs", "C:\\proj"),
        "src\\main.rs"
    );
}

#[test]
fn test_summarize_output_empty() {
    assert_eq!(summarize_output("Bash", ""), "");
    assert_eq!(summarize_output("Bash", "   "), "");
}

#[test]
fn test_summarize_output_edit_long_collapses_to_line_count() {
    let output = "line1\nline2\nline3\nline4\nline5";
    assert_eq!(summarize_output("Edit", output), "5 lines changed");
}
