use crate::error_suggest::matcher::{fuzzy_filter, fuzzy_filter_min, fuzzy_top_n};

#[test]
fn test_fuzzy_top_n_returns_sorted_matches() {
    let candidates: Vec<String> = vec![
        "peri-agent".into(),
        "peri-tui".into(),
        "peri-middlewares".into(),
        "langfuse-client".into(),
    ];
    let result = fuzzy_top_n(&candidates, "peri", 3);
    assert_eq!(result.len(), 3);
    let names: Vec<&str> = result.iter().map(|s| s.as_str()).collect();
    assert!(names.contains(&"peri-agent"));
    assert!(names.contains(&"peri-tui"));
    assert!(names.contains(&"peri-middlewares"));
}

#[test]
fn test_fuzzy_top_n_handles_no_matches() {
    let candidates: Vec<String> = vec!["foo".into(), "bar".into()];
    let result = fuzzy_top_n(&candidates, "zzz", 3);
    assert!(result.is_empty());
}

#[test]
fn test_fuzzy_top_n_respects_limit() {
    let candidates: Vec<String> = (0..10).map(|i| format!("candidate-{i}")).collect();
    let result = fuzzy_top_n(&candidates, "candidate", 3);
    assert_eq!(result.len(), 3);
}

#[test]
fn test_fuzzy_filter_returns_owned_strings_sorted() {
    let candidates: Vec<String> = vec![
        "src/main.rs".into(),
        "src/lib.rs".into(),
        "README.md".into(),
    ];
    let result = fuzzy_filter(&candidates, "main");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], "src/main.rs");
}

#[test]
fn test_fuzzy_filter_min_keeps_typo_candidates_above_threshold() {
    // 丢字符类拼错（dockr→docker）分数 ≥ 90，60 阈值下应保留
    let candidates: Vec<String> = vec![
        "docker".into(),
        "docer".into(),
        "dock".into(),
        "xylophone".into(),
    ];
    let result = fuzzy_filter_min(&candidates, "dockr", 60);
    assert!(
        result.contains(&"docker".to_string()),
        "高分拼错候选应保留: {result:?}"
    );
}

#[test]
fn test_fuzzy_filter_min_drops_noise_below_threshold() {
    // 短查询泛化子序列（xy→xylophone）分数 ≤ 51，60 阈值下应剔除
    let candidates: Vec<String> = vec!["xylophone".into(), "bash".into(), "ls".into()];
    let result = fuzzy_filter_min(&candidates, "xy", 60);
    assert!(result.is_empty(), "低分噪声候选应被阈值剔除: {result:?}");
}
