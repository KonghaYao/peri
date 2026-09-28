use super::*;
use peri_acp_types::builtin_mcp::{find, original_tool_name_of_effective};

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
fn test_summarize_input_agent_shows_type_and_task() {
    let input = serde_json::json!({
        "cwd": "/Users/konghayao/code/ai/perihelion",
        "prompt": "追踪 Agent 调用显示链路",
        "subagent_type": "explorer",
    });
    assert_eq!(
        summarize_input("Agent", &input),
        "explorer 追踪 Agent 调用显示链路"
    );
    assert_eq!(
        summarize_input("Task", &input),
        "explorer 追踪 Agent 调用显示链路"
    );

    let no_prompt = serde_json::json!({
        "cwd": "/tmp",
        "description": "只读调查",
        "subagent_type": "plan",
    });
    assert_eq!(summarize_input("Agent", &no_prompt), "plan 只读调查");

    let type_only = serde_json::json!({ "cwd": "/tmp", "subagent_type": "plan" });
    assert_eq!(summarize_input("Agent", &type_only), "plan");
}

#[test]
fn test_summarize_input_agent_shows_fork_and_resume_modes() {
    let fork = serde_json::json!({
        "fork": true,
        "prompt": "审查当前实现",
        "subagent_type": "explorer",
    });
    assert_eq!(summarize_input("Agent", &fork), "fork 审查当前实现");

    let resume = serde_json::json!({
        "resume_thread_id": "thread-1",
        "fork": true,
        "prompt": "继续验证",
        "subagent_type": "explorer",
    });
    assert_eq!(summarize_input("Agent", &resume), "resume 继续验证");
}

#[test]
fn test_summarize_input_agent_compatibility_fallbacks() {
    let prompt_only = serde_json::json!({ "prompt": "普通任务" });
    assert_eq!(summarize_input("Agent", &prompt_only), "普通任务");

    let invalid_selectors = serde_json::json!({
        "fork": "true",
        "prompt": "普通任务",
        "resume_thread_id": 42,
        "subagent_type": false,
    });
    assert_eq!(summarize_input("Agent", &invalid_selectors), "普通任务");

    let empty = serde_json::json!({ "cwd": "/tmp" });
    assert_eq!(summarize_input("Agent", &empty), "(empty input)");
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

#[test]
fn test_summarize_output_read_preserves_canonical_truncation_semantics() {
    let output = "     1\talpha\n     2\tbeta\n[Output truncated: 12000 bytes total; showing lines 1..=2 of 800; continue reading with offset=3]";
    assert_eq!(summarize_output("Read", output), "3 lines · truncated");
}

#[test]
fn test_summarize_output_complete_read_is_not_marked_truncated() {
    let output = "     1\talpha\n     2\tbeta";
    assert_eq!(summarize_output("Read", output), "2 lines");
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

// ── A4 / A8 匹配型归一：builtin 一等工具的 effective name 走同一分支 ────────────

#[test]
fn tui_web_tools_still_summarize_after_migration() {
    // 输入摘要：effective name（模型面名字）与迁移前的裸名产出**同一**摘要
    let input = serde_json::json!({ "query": "rust async" });
    let effective = summarize_input("mcp__web__WebSearch", &input);
    assert_eq!(effective, summarize_input("WebSearch", &input));
    assert_eq!(effective, r#"query: "rust async""#);

    // WebSearch query 仍按 60 字符截断（通用兜底是 120）；「只留 query」也未被通用化
    let long = serde_json::json!({ "query": "q".repeat(80) });
    assert_eq!(
        summarize_input("mcp__web__WebSearch", &long),
        format!(r#"query: "{}...""#, "q".repeat(60))
    );

    // WebFetch 输入：无 url 时是专用分支的 "(empty input)"——通用兜底会回显首个 KV
    let no_url = serde_json::json!({ "unexpected": "v" });
    assert_eq!(
        summarize_input("mcp__web__WebFetch", &no_url),
        "(empty input)"
    );
    assert_eq!(
        summarize_input(
            "mcp__web__WebFetch",
            &serde_json::json!({ "url": "https://example.com/a" })
        ),
        "url: https://example.com/a"
    );

    // artifact 输入：file_path 专用分支（通用兜底会带 `path: ` 前缀与 JSON 引号）
    let file = serde_json::json!({ "file_path": "peri-tui/src/lib.rs" });
    assert_eq!(
        summarize_input("mcp__artifact__artifact", &file),
        "peri-tui/src/lib.rs"
    );

    // 输出折叠：WebFetch 仍走专用折叠（行数 · 字节数 + 保留正文），不走通用 200 字符截断
    let output = "line1\nline2\nhttps://example.com/page";
    let folded = summarize_output("mcp__web__WebFetch", output);
    assert_eq!(folded, summarize_output("WebFetch", output));
    assert!(folded.contains("3 lines"), "行数折叠: {folded:?}");
    assert!(
        folded.contains("https://example.com/page"),
        "专用折叠必须保留正文（URL）: {folded:?}"
    );
    assert!(folded.contains("bytes"), "字节数折叠: {folded:?}");
}

// ── wave 2（cron / lsp）注册表新增：新 effective name 必须复用**既有**摘要分支 ──────

/// wave 2 新增的 builtin 实例在注册表中的**实例键**（不是 effective name 副本）。
const WAVE2_INSTANCE_KEYS: [&str; 2] = ["cron", "lsp"];

/// 按原始工具名给出代表性参数：键名取自 `peri-middlewares` 各自 `BaseTool::parameters()`。
/// 未登记的名字直接 panic —— 注册表条目变动时必须回来复核本回归。
fn wave2_args_by_original_name(original_name: &str) -> serde_json::Value {
    match original_name {
        "cron_register" => {
            serde_json::json!({ "expression": "*/5 * * * *", "prompt": "check status" })
        }
        "cron_list" => serde_json::json!({}),
        "cron_remove" => serde_json::json!({ "id": "task-1" }),
        // 只用 operation（不带 file_path）：通用兜底对 file_path 会回显 path，而 path
        // 会被全局 DISPLAY_CWD 精简，掺入与本回归无关的进程级状态。
        "LSP" => serde_json::json!({ "operation": "documentSymbol" }),
        other => panic!("wave 2 有未登记的工具，需复核本回归: {other}"),
    }
}

#[test]
fn cron_lsp_effective_names_reuse_existing_summaries() {
    // S-03：summarize_input / summarize_output 的既有归一入口（原样优先 → IF-D15
    // 归一后重试）对新注册表条目必须同样生效，**不新增第二份归一实现**；
    // 等价值用可观察输出（返回值）比较。
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    for key in WAVE2_INSTANCE_KEYS {
        let instance = find(key).unwrap_or_else(|| panic!("wave 2 实例必须在注册表中: {key}"));
        for tool in instance.tools {
            pairs.push((tool.effective_name, tool.original_name));
        }
    }
    // 从注册表取数（不在此复制 effective name 字面量）：cron 三项 + lsp 一项。
    assert_eq!(pairs.len(), 4, "wave 2 新增条目数: {pairs:?}");

    for (effective, original) in pairs {
        // 归一入口的前置条件：新 effective name 必须能被 IF-D15 反查命中；命中不了
        // ⇒ 重试分支拿不到原始名，归一入口对新名字失效。
        assert_eq!(
            original_tool_name_of_effective(effective),
            Some(original),
            "IF-D15 必须命中 wave 2 新条目: {effective}"
        );
        let input = wave2_args_by_original_name(original);
        assert_eq!(
            summarize_input(effective, &input),
            summarize_input(original, &input),
            "effective name 必须复用 {original} 的既有输入摘要分支: {effective}"
        );
        // 输出摘要同分支：4 个新条目都没有专有输出分支，两侧都走通用 200 字符截断。
        let output = "line1\nline2\nhttps://example.com/page";
        assert_eq!(
            summarize_output(effective, output),
            summarize_output(original, output),
            "effective name 必须复用 {original} 的既有输出摘要分支: {effective}"
        );
    }

    // 判别点：LSP 是 4 个新条目中唯一命中**专有输入摘要分支**的（operation 截断 40）。
    // 归一入口失效 ⇒ 落到通用兜底（首个 KV 回显 `operation: documentSymbol`），
    // 下面两条断言随之变红。
    let lsp_effective = find("lsp")
        .expect("lsp 实例必须在注册表中")
        .tools
        .iter()
        .find(|tool| tool.original_name == "LSP")
        .expect("lsp 实例必须声明 LSP 工具")
        .effective_name;
    let lsp_args = wave2_args_by_original_name("LSP");
    let lsp = summarize_input(lsp_effective, &lsp_args);
    assert_eq!(lsp, "documentSymbol", "LSP 专有分支未被复用: {lsp:?}");
    assert_ne!(
        lsp,
        summarize_input("mcp__unknown__LSP", &lsp_args),
        "专有分支必须与通用兜底在输出上可区分"
    );
}

#[test]
fn tui_unknown_mcp_names_fall_back_to_generic() {
    // 反证：未知 / 外部 `mcp__*` 两次都不命中 ⇒ 走通用路径，与迁移前逐位一致。
    let input = serde_json::json!({ "file_path": "peri-tui/src/lib.rs" });
    assert_eq!(
        summarize_input("mcp__foo__bar", &input),
        r#"path: "peri-tui/src/lib.rs""#
    );
    // 精确匹配：前缀相似的名字不被归一
    assert_eq!(
        summarize_input("mcp__web__WebSearchExtra", &input),
        r#"path: "peri-tui/src/lib.rs""#
    );

    // 输出侧同样走通用截断（不是 Edit/Write 的 "N lines changed"，也不是 WebFetch 折叠）
    let output = "a\nb\nc\nd";
    assert_eq!(summarize_output("mcp__foo__bar", output), "a\nb\nc\nd");
}

#[test]
fn tui_has_no_hardcoded_effective_name() {
    // A4 / A8：名字字面量只在 peri-acp-types 的 builtin 声明表存一份——按名分支
    // 必须经 IF-D15 归一 helper 进入 builtin 名字空间，不得自建第二张反查表。
    let src = include_str!("truncate.rs");
    assert!(
        src.contains("original_tool_name_of_effective"),
        "truncate.rs 必须经 IF-D15 归一 helper"
    );
    for forbidden in ["mcp__web__", "mcp__artifact__"] {
        assert!(
            !src.contains(forbidden),
            "truncate.rs 不得硬编码 effective name: {forbidden}"
        );
    }
}
