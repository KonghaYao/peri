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
fn test_truncate_by_width_keeps_combining_marks() {
    // combining mark（e + U+0301 组合重音）是单个 grapheme，宽度 1，
    // 不会被从基字符与重音之间切开
    let e_acute = "e\u{301}"; // é
    let s = format!("{e_acute}{}", "a".repeat(29)); // 1 + 29 = 30 列
    assert_eq!(truncate_by_width(&s, 30), s);
    // 预算 30 恰好放不下第 30 个 a 之后的省略号场景：29 个 a + é 填满 30 列
    assert_eq!(
        truncate_by_width(&s, 30),
        format!("{e_acute}{}", "a".repeat(29))
    );
    // 预算 25：重音序列作为一个整体保留，不被切开
    let t = truncate_by_width(&s, 25);
    assert!(
        t.starts_with(e_acute),
        "combining mark 不得从基字符处切断: {t:?}"
    );
    assert!(t.ends_with('…'));
    use unicode_width::UnicodeWidthStr;
    assert!(t.width() <= 26, "输出宽度不超预算+省略号");
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

// ── wrap_by_width（§6.1 用户 prompt 视觉行折行）──────────────────────────

#[test]
fn test_wrap_by_width_short_line_unchanged() {
    assert_eq!(wrap_by_width("hello", 40), vec!["hello"]);
    // 恰好等于宽度：单行不折
    assert_eq!(wrap_by_width("hello", 5), vec!["hello"]);
}

#[test]
fn test_wrap_by_width_cjk_double_width() {
    // 20 个汉字 = 40 列；每行 5 个汉字（10 列）→ 4 行
    let text = "测".repeat(20);
    let lines = wrap_by_width(&text, 10);
    assert_eq!(lines.len(), 4);
    for l in &lines {
        assert_eq!(l.chars().count(), 5, "每行 5 个汉字");
    }
    // 内容不丢：拼接还原
    assert_eq!(lines.concat(), text);
}

#[test]
fn test_wrap_by_width_emoji_zwj_not_split() {
    // 👨‍👩‍👧‍👦 显示宽 2 列（ZWJ 序列），5 列一行放 2 个，3 个 → 2 行
    let fam = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}";
    let text = format!("{fam}{fam}{fam}");
    let lines = wrap_by_width(&text, 5);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].graphemes(true).count(), 2);
    assert_eq!(lines[1].graphemes(true).count(), 1);
    // 拼接还原（ZWJ 序列未被切开）
    assert_eq!(lines.concat(), text);
}

#[test]
fn test_wrap_by_width_combining_mark_kept_together() {
    // e + combining acute = 1 列；宽度 3 → 每行 3 个字符
    let text = "e\u{301}".repeat(10);
    let lines = wrap_by_width(&text, 3);
    assert_eq!(lines.len(), 4); // 10 个字符，每行 3 个 → 3+3+3+1
    assert_eq!(lines[0], "e\u{301}e\u{301}e\u{301}");
    assert_eq!(lines.concat(), text);
}

#[test]
fn test_wrap_by_width_ascii_word_split_no_content_loss() {
    let text = "a".repeat(100);
    let lines = wrap_by_width(&text, 10);
    assert_eq!(lines.len(), 10);
    for l in &lines {
        assert_eq!(l.len(), 10);
    }
    assert_eq!(lines.concat(), text);
}

#[test]
fn test_wrap_by_width_multiline_input() {
    // 调用方（render_user_bubble_lines）按 `\n` 分行后逐行 wrap——
    // wrap 自身对换行符按 grapheme 处理（宽度 0），不拆两行。
    let text = "ab\ncd";
    let lines = wrap_by_width(text, 40);
    assert_eq!(lines.len(), 1, "换行符保留在行内，由调用方分行");
}

#[test]
fn test_wrap_by_width_zero_width_returns_original() {
    assert_eq!(wrap_by_width("x", 0), vec!["x"]);
}

#[test]
fn test_wrap_by_width_empty_returns_single_empty_line() {
    // render 侧（reasoning_visual_lines / render_user_bubble_lines）依赖 flat_map
    // 后过滤 trim 空行——wrap 自身对空串产出单空行，不 panic。
    assert_eq!(wrap_by_width("", 10), vec![""]);
    assert_eq!(wrap_by_width("", 1), vec![""]);
}
