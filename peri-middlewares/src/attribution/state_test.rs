use super::AttributionState;

#[test]
fn test_track_change_entirely_new_file() {
    let mut state = AttributionState::new("claude".to_string());
    state.track_change("main.rs", "", "fn main() {}");
    let contrib = state.contributions.get("main.rs").unwrap();
    assert_eq!(contrib.claude_chars, "fn main() {}".len());
}

#[test]
fn test_track_change_entirely_deleted() {
    let mut state = AttributionState::new("claude".to_string());
    state.track_change("main.rs", "fn main() {}", "");
    let contrib = state.contributions.get("main.rs").unwrap();
    assert_eq!(contrib.claude_chars, "fn main() {}".len());
}

#[test]
fn test_track_change_append_only() {
    let mut state = AttributionState::new("claude".to_string());
    state.track_change("main.rs", "fn main() {}", "fn main() {}\nfn bar() {}");
    let contrib = state.contributions.get("main.rs").unwrap();
    assert_eq!(contrib.claude_chars, "\nfn bar() {}".len());
}

#[test]
fn test_track_change_middle_modification() {
    let mut state = AttributionState::new("claude".to_string());
    state.track_change("main.rs", "let a = 1", "let b = 2");
    let contrib = state.contributions.get("main.rs").unwrap();
    assert_eq!(contrib.claude_chars, "b = 2".len());
}

#[test]
fn test_track_change_same_length_replace() {
    let mut state = AttributionState::new("claude".to_string());
    state.track_change("main.rs", "Esc", "esc");
    let contrib = state.contributions.get("main.rs").unwrap();
    // prefix: none (E≠e), suffix: "sc" matches, 仅首字符变更
    assert_eq!(contrib.claude_chars, 1);
}

#[test]
fn test_track_change_cumulative() {
    let mut state = AttributionState::new("claude".to_string());
    state.track_change("main.rs", "", "fn a() {}");
    state.track_change("main.rs", "fn a() {}", "fn a() {}\nfn b() {}");
    let contrib = state.contributions.get("main.rs").unwrap();
    assert_eq!(
        contrib.claude_chars,
        "fn a() {}".len() + "\nfn b() {}".len()
    );
}

#[test]
fn test_track_change_cjk() {
    let mut state = AttributionState::new("claude".to_string());
    state.track_change(
        "main.rs",
        "let 你好 = \"世界\"",
        "let こんにちは = \"世界\"",
    );
    let contrib = state.contributions.get("main.rs").unwrap();
    // 变化区域: "你好" (2 chars) → "こんにちは" (5 chars)，max = 5
    assert_eq!(contrib.claude_chars, 5);
}

#[test]
fn test_co_authored_by_format() {
    let state = AttributionState::new("claude-sonnet-4-20250514".to_string());
    assert_eq!(
        state.co_authored_by(),
        "Co-Authored-By: claude-sonnet-4-20250514 <noreply@anthropic.com>"
    );
}
