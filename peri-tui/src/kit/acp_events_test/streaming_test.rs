use super::*;

// ── has_md_block_boundary_since 单元测试 ──

#[test]
fn test_boundary_since_chars_zero_always_true() {
    assert!(
        has_md_block_boundary_since("hello", 0),
        "since_chars=0 应始终返回 true"
    );
}

#[test]
fn test_boundary_empty_string() {
    assert!(!has_md_block_boundary_since("", 1), "空字符串不应触发边界");
}

#[test]
fn test_boundary_paragraph_double_newline() {
    let text = "first paragraph\n\nsecond paragraph";
    // since_chars=0 已推送；从字符 1 开始检查应有双换行
    assert!(has_md_block_boundary_since(text, 1), "双换行应触发段落边界");
}

#[test]
fn test_boundary_code_block() {
    let text = "some text\n```rust\nfn main() {}\n```";
    // 从 "some" 开始检查
    assert!(has_md_block_boundary_since(text, 1), "代码块起止应触发边界");
}

#[test]
fn test_boundary_heading() {
    let text = "intro\n# Heading\ncontent";
    assert!(has_md_block_boundary_since(text, 1), "标题应触发边界");
}

#[test]
fn test_boundary_horizontal_rule() {
    let text = "text\n---\nmore";
    assert!(has_md_block_boundary_since(text, 1), "水平线应触发边界");
}

#[test]
fn test_boundary_no_boundary_in_tail() {
    let text = "one line of text\nanother line without boundary";
    // since_chars 越过已推送部分，尾部无边界
    let pushed = "one line of text".chars().count();
    assert!(
        !has_md_block_boundary_since(text, pushed),
        "无分隔的连续文本不应触发边界"
    );
}

// ── current_streaming_mode 测试 ──

/// 默认（未设置 streaming_mode 或 PERI_CONFIG_HANDLE 未初始化）应返回 Streaming。
#[test]
fn test_mode_default_is_streaming() {
    // PERI_CONFIG_HANDLE 在测试中未初始化 → get() 返回 None → fallback 到 Streaming
    assert!(
        matches!(current_streaming_mode(), StreamingMode::Streaming),
        "未设置 streaming_mode 时应默认 Streaming"
    );
}
