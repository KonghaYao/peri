//! Tests

use super::*;
use serial_test::serial;

/// 测试辅助：构造代码行 Vec。
fn make_lines(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// 测试辅助：清空全局缓存（每个测试隔离）。
fn reset_cache() {
    HIGHLIGHT_CACHE.write().clear();
}

#[test]
#[serial]
fn test_highlight_cache_hit_on_same_input() {
    reset_cache();
    let lines = make_lines(&["let x = 1;", "let y = 2;"]);
    // 第一次 miss
    let (r1, hit1) = highlight_code_block_with_hit("rust", &lines);
    assert!(!hit1, "首次调用应 miss");
    assert!(r1.is_some(), "rust 应能高亮");
    // 第二次相同输入 → hit
    let (r2, hit2) = highlight_code_block_with_hit("rust", &lines);
    assert!(hit2, "相同 (lang, lines) 第二次应命中缓存");
    assert_eq!(r1, r2, "命中缓存应返回一致结果");
}

#[test]
#[serial]
fn test_highlight_cache_miss_on_different_lang() {
    reset_cache();
    let lines = make_lines(&["let x = 1;"]);
    let (_, hit1) = highlight_code_block_with_hit("rust", &lines);
    assert!(!hit1);
    // 同 content 不同 lang → miss
    let (_, hit2) = highlight_code_block_with_hit("python", &lines);
    assert!(!hit2, "不同 lang 应 miss");
}

#[test]
#[serial]
fn test_highlight_cache_miss_on_different_content() {
    reset_cache();
    let lines_a = make_lines(&["let x = 1;"]);
    let lines_b = make_lines(&["let x = 2;"]);
    let (_, hit1) = highlight_code_block_with_hit("rust", &lines_a);
    assert!(!hit1);
    let (_, hit2) = highlight_code_block_with_hit("rust", &lines_b);
    assert!(!hit2, "同 lang 不同 content 应 miss");
}

#[test]
#[serial]
fn test_highlight_cache_none_result_cached() {
    reset_cache();
    // 未知 lang（"totally-not-a-real-lang"）→ find_syntax_by_token 返回 None
    let lines = make_lines(&["some code"]);
    let (r1, hit1) = highlight_code_block_with_hit("totally-unknown-lang-xyz", &lines);
    assert!(!hit1);
    assert!(r1.is_none(), "未知 lang 应返回 None");
    // 第二次：None 结果也应被缓存（避免反复调 find_syntax_by_token）
    let (r2, hit2) = highlight_code_block_with_hit("totally-unknown-lang-xyz", &lines);
    assert!(hit2, "None 结果也应命中缓存");
    assert!(r2.is_none());
}

#[test]
#[serial]
fn test_highlight_cache_lru_eviction() {
    reset_cache();
    // 填满 32 条
    for i in 0..32 {
        let lines = make_lines(&[&format!("line {i}")]);
        let (_, hit) = highlight_code_block_with_hit("rust", &lines);
        assert!(!hit, "第 {i} 条首次插入应 miss");
    }
    // 第 33 条 → 淘汰最旧（line 0）
    let lines_33 = make_lines(&["line 32"]);
    let (_, hit_33) = highlight_code_block_with_hit("rust", &lines_33);
    assert!(!hit_33, "第 33 条应 miss");
    // 验证 line 0 已被淘汰（重新查应 miss）
    let lines_0 = make_lines(&["line 0"]);
    let (_, hit_0) = highlight_code_block_with_hit("rust", &lines_0);
    assert!(!hit_0, "被淘汰的最旧条目重查应 miss");
    // 验证 line 31 仍在缓存
    let lines_31 = make_lines(&["line 31"]);
    let (_, hit_31) = highlight_code_block_with_hit("rust", &lines_31);
    assert!(hit_31, "未淘汰的条目应命中");
}

#[test]
#[serial]
fn test_hash_raw_lines_distinguishes_adjacent_splits() {
    // 防御性测试：分隔符 0xA 应让 ["ab","c"] 与 ["a","bc"] 产生不同 hash
    let h1 = hash_raw_lines(&make_lines(&["ab", "c"]));
    let h2 = hash_raw_lines(&make_lines(&["a", "bc"]));
    assert_ne!(h1, h2, "相邻串拼接歧义应被分隔符消除");
}

#[test]
#[serial]
fn test_hash_raw_lines_same_input_same_hash() {
    let h1 = hash_raw_lines(&make_lines(&["fn main() {}", "    println!(\"hi\");"]));
    let h2 = hash_raw_lines(&make_lines(&["fn main() {}", "    println!(\"hi\");"]));
    assert_eq!(h1, h2);
}
