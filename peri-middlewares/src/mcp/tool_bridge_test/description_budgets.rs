// ── M7：单项工具 description 字节预算 ────────────────────────────────────────

/// 外部 server 的巨型 description 必须被截到预算内，且截断标记计入预算。
#[test]
fn oversized_tool_description_is_bounded_within_budget() {
    let raw = "d".repeat(MAX_TOOL_DESCRIPTION_BYTES * 3 + 7);
    let tool = make_tool("huge", Some(raw.as_str()));
    let handle = make_disconnected_handle("fs");
    let bridge = McpToolBridge::new("fs", &tool, handle);
    let description = bridge.description();
    assert!(
        description.len() <= MAX_TOOL_DESCRIPTION_BYTES,
        "description 必须落在预算内: {} > {}",
        description.len(),
        MAX_TOOL_DESCRIPTION_BYTES
    );
    assert!(
        description.ends_with(&format!(
            "… [description truncated: {} bytes total]",
            raw.len() + "[MCP:fs] ".len()
        )),
        "截断标记必须给出总长度: {}",
        &description[description.len().saturating_sub(80)..]
    );
    assert!(description.starts_with("[MCP:fs] ddd"));
}

/// 多字节 description 的截断必须落在 UTF-8 边界上。
#[test]
fn oversized_multibyte_description_truncates_on_char_boundary() {
    let raw = "描".repeat(MAX_TOOL_DESCRIPTION_BYTES);
    let tool = make_tool("huge", Some(raw.as_str()));
    let handle = make_disconnected_handle("fs");
    let bridge = McpToolBridge::new("fs", &tool, handle);
    let description = bridge.description();
    assert!(description.len() <= MAX_TOOL_DESCRIPTION_BYTES);
    assert!(!description.contains('\u{FFFD}'));
    let marker_at = description.find('…').unwrap();
    assert_eq!(
        &description[..marker_at].len() % 3,
        0,
        "正文截断必须落在 3 字节字符边界"
    );
}

/// 预算内的 description 逐字保持既有格式。
#[test]
fn small_tool_description_is_unchanged() {
    let tool = make_tool("read_file", Some("Read a file"));
    let handle = make_disconnected_handle("fs");
    let bridge = McpToolBridge::new("fs", &tool, handle);
    assert_eq!(bridge.description(), "[MCP:fs] Read a file");
}
