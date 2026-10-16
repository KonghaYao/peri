use super::*;

fn matches_any(patterns: &[Pattern], path: &str) -> bool {
    patterns.iter().any(|pattern| pattern.matches(path))
}

#[test]
fn test_brace_alternation_expands_every_alternative() {
    let patterns = compile_glob_patterns("*.{ts,tsx}").unwrap();
    assert_eq!(patterns.len(), 2, "两个交替分支应展开成两个 pattern");
    assert!(matches_any(&patterns, "a.ts"));
    assert!(matches_any(&patterns, "a.tsx"));
    assert!(!matches_any(&patterns, "a.js"));
}

#[test]
fn test_nested_brace_alternation_expands_inner_group() {
    let patterns = compile_glob_patterns("*.{ts,{js,jsx}}").unwrap();
    assert_eq!(patterns.len(), 3, "嵌套交替应展平为三个分支");
    assert!(matches_any(&patterns, "a.jsx"));
    assert!(!matches_any(&patterns, "a.json"));
}

#[test]
fn test_alternation_groups_expand_cartesian_product() {
    let patterns = compile_glob_patterns("{a,b}.{ts,tsx}").unwrap();
    assert_eq!(patterns.len(), 4, "两个交替组应展开为笛卡尔积");
    assert!(matches_any(&patterns, "b.tsx"));
    assert!(!matches_any(&patterns, "c.ts"));
}

#[test]
fn test_plain_pattern_compiles_to_single_pattern() {
    let patterns = compile_glob_patterns("*.rs").unwrap();
    assert_eq!(patterns.len(), 1, "无交替的 pattern 不应被改写");
    assert!(matches_any(&patterns, "src/a.rs"));
}

#[test]
fn test_unclosed_brace_is_rejected() {
    let error = compile_glob_patterns("*.{ts,tsx").unwrap_err();
    assert!(error.contains("unclosed"), "应报告未闭合交替: {error}");
}

#[test]
fn test_unclosed_bracket_is_rejected() {
    let error = compile_glob_patterns("[unclosed").unwrap_err();
    assert!(
        error.contains("invalid glob pattern"),
        "应报告非法 pattern: {error}"
    );
}

#[test]
fn test_expansion_budget_is_rejected() {
    let error = compile_glob_patterns("{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}").unwrap_err();
    assert!(
        error.contains("expands past"),
        "超出展开上限应报错而不是编译上万 pattern: {error}"
    );
}

// ─── 字面花括号：没有顶层逗号的 `{…}` 不是交替 ───────────────────

#[test]
fn test_literal_brace_group_is_not_expanded() {
    // `foo{bar}` 无顶层逗号 → 不是交替，按字面匹配文件名 `foo{bar}`。
    let patterns = compile_glob_patterns("foo{bar}").unwrap();
    assert_eq!(patterns.len(), 1, "无逗号的花括号不应被展开");
    assert!(matches_any(&patterns, "foo{bar}"), "应匹配字面文件名");
    assert!(!matches_any(&patterns, "foobar"), "不应被改写成展开结果");
}

#[test]
fn test_brace_without_comma_is_literal_even_when_unclosed() {
    // 无逗号的 `{` 本来就是字面量，未闭合也不该报「未闭合交替」。
    let patterns = compile_glob_patterns("foo{bar").unwrap();
    assert_eq!(patterns.len(), 1, "无逗号的未闭合花括号仍是字面量");
    assert!(matches_any(&patterns, "foo{bar"));
}

#[test]
fn test_brace_in_character_class_is_not_expansion() {
    // 字符类里的 `{` 是类内容，此前会被当成未闭合交替而误报。
    let patterns = compile_glob_patterns("[{]foo.rs").unwrap();
    assert_eq!(patterns.len(), 1, "字符类不应触发交替展开");
    assert!(matches_any(&patterns, "{foo.rs"), "`[{{]` 应匹配字面 `{{`");
    assert!(!matches_any(&patterns, "foo.rs"));
}

#[test]
fn test_negated_character_class_brace_is_not_expansion() {
    // `[!` 开头的字符类同样整体跳过：`[!{]` 匹配除 `{` 以外的一个字符。
    let patterns = compile_glob_patterns("[!{]foo.rs").unwrap();
    assert_eq!(patterns.len(), 1, "否定字符类不应触发交替展开");
    assert!(matches_any(&patterns, "xfoo.rs"));
    assert!(!matches_any(&patterns, "{foo.rs"));
}

#[test]
fn test_comma_in_character_class_is_not_a_separator() {
    // 类内逗号是类内容，不是分支分隔符：此前会被切开并编译出非法 pattern。
    let patterns = compile_glob_patterns("x[{,}]y").unwrap();
    assert_eq!(patterns.len(), 1, "类内逗号不应切分交替");
    assert!(matches_any(&patterns, "x{y"));
    assert!(matches_any(&patterns, "x,y"));
    assert!(!matches_any(&patterns, "xy"));
}

// ─── 深度与长度预算：迭代展开，深输入只报错不爆栈 ─────────────────

#[test]
fn test_deep_literal_braces_do_not_exhaust_stack() {
    // 深嵌套但无逗号的花括号是字面量：合法 pattern，且不应让展开爆栈。
    let pattern = format!("{}x{}", "{".repeat(1000), "}".repeat(1000));
    let patterns = compile_glob_patterns(&pattern).unwrap();
    assert_eq!(patterns.len(), 1, "无逗号的深嵌套保持字面量");
    assert!(matches_any(&patterns, &pattern));
}

#[test]
fn test_deep_alternation_chain_is_rejected() {
    // 每层都带逗号的深嵌套：链长上限拦下，不会递归爆栈。
    let pattern = format!("{}a{}", "{a,".repeat(64), "}".repeat(64));
    let error = compile_glob_patterns(&pattern).unwrap_err();
    assert!(
        error.contains("chain longer than"),
        "深嵌套应被链长上限拦下: {error}"
    );
}

#[test]
fn test_long_sequential_alternation_chain_is_rejected() {
    // 大量顺序 `{a,b}`：递归版本会随组数加深调用栈，这里由链长上限在有限步内拒绝。
    let error = compile_glob_patterns(&"{a,b}".repeat(200)).unwrap_err();
    assert!(
        error.contains("chain longer than"),
        "顺序交替链应有上限: {error}"
    );
}

#[test]
fn test_oversized_pattern_is_rejected_before_expansion() {
    // 展开前先校验长度：超长输入不进展开循环。
    let error = compile_glob_patterns(&"a".repeat(MAX_PATTERN_LEN + 1)).unwrap_err();
    assert!(
        error.contains("byte limit"),
        "超长 pattern 应在展开前报错: {error}"
    );
}

#[test]
fn test_pattern_at_length_limit_still_compiles() {
    // 边界内：正好等于长度上限的 pattern 仍可用（上限判的是「超过」）。
    let pattern = "a".repeat(MAX_PATTERN_LEN);
    let patterns = compile_glob_patterns(&pattern).unwrap();
    assert_eq!(patterns.len(), 1, "上限内的 pattern 不应被拒");
    assert!(matches_any(&patterns, &pattern));
}
