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
