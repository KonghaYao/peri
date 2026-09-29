use lsp_types::{Position, Range};

use super::*;

fn make_uri(path: &str) -> lsp_types::Uri {
    format!("file://{path}").parse().unwrap()
}

fn make_location(path: &str, line: u32, char: u32) -> Location {
    Location {
        uri: make_uri(path),
        range: Range::new(Position::new(line, char), Position::new(line, char + 5)),
    }
}

#[test]
fn test_format_locations_empty() {
    assert_eq!(format_locations(&[]), "No definition found.");
}

#[test]
fn test_format_locations_single() {
    let loc = make_location("/src/main.rs", 9, 4);
    let result = format_locations(&[loc]);
    assert!(result.contains("/src/main.rs:10:5"));
    assert!(result.contains("Defined in"));
}

#[test]
fn test_format_locations_multiple() {
    let locs = vec![
        make_location("/src/a.rs", 0, 0),
        make_location("/src/b.rs", 5, 10),
    ];
    let result = format_locations(&locs);
    assert!(result.contains("2 definitions"));
    assert!(result.contains("/src/a.rs:1:1"));
    assert!(result.contains("/src/b.rs:6:11"));
}

#[test]
fn test_format_definition_result_location_array() {
    let locs = vec![make_location("/src/main.rs", 9, 4)];
    let value = serde_json::to_value(&locs).unwrap();
    let result = format_definition_result(&value);
    assert!(result.contains("Defined in"));
}

#[test]
fn test_format_definition_result_empty() {
    let value = serde_json::json!([]);
    let result = format_definition_result(&value);
    assert_eq!(result, "No definition found.");
}

#[test]
fn test_format_definition_result_invalid() {
    let value = serde_json::json!({"unexpected": true});
    let result = format_definition_result(&value);
    assert_eq!(result, "No definition found.");
}

#[test]
fn test_format_references_empty() {
    let value = serde_json::json!([]);
    assert_eq!(format_references(&value), "No references found.");
}

#[test]
fn test_format_references_single_file() {
    let locs = vec![
        make_location("/src/main.rs", 0, 0),
        make_location("/src/main.rs", 5, 10),
    ];
    let value = serde_json::to_value(&locs).unwrap();
    let result = format_references(&value);
    assert!(result.contains("2 references"));
    assert!(result.contains("/src/main.rs"));
}

#[test]
fn test_format_references_multi_file() {
    let locs = vec![
        make_location("/src/a.rs", 0, 0),
        make_location("/src/b.rs", 5, 10),
    ];
    let value = serde_json::to_value(&locs).unwrap();
    let result = format_references(&value);
    assert!(result.contains("2 references across 2 files"));
}

#[test]
fn test_format_hover_no_info() {
    let value = serde_json::json!(null);
    assert_eq!(format_hover(&value), "No hover information available.");
}

#[test]
fn test_format_hover_with_markup() {
    let hover = lsp_types::Hover {
        contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
            kind: lsp_types::MarkupKind::Markdown,
            value: "fn test() -> i32".to_string(),
        }),
        range: None,
    };
    let value = serde_json::to_value(&hover).unwrap();
    let result = format_hover(&value);
    assert!(result.contains("fn test() -> i32"));
}

#[test]
fn test_format_document_symbols_empty() {
    let value = serde_json::json!([]);
    assert_eq!(
        format_document_symbols(&value),
        "No symbols found in document."
    );
}

#[test]
fn test_format_document_symbols_flat() {
    let symbols = vec![lsp_types::SymbolInformation {
        name: "main".to_string(),
        kind: lsp_types::SymbolKind::FUNCTION,
        location: make_location("/src/main.rs", 0, 0),
        container_name: None,
        #[allow(deprecated)]
        deprecated: None,
        tags: None,
    }];
    let value = serde_json::to_value(&symbols).unwrap();
    let result = format_document_symbols(&value);
    assert!(result.contains("main (Function)"));
    assert!(result.contains("Line 1"));
}

#[test]
fn test_format_workspace_symbols_empty() {
    let value = serde_json::json!([]);
    assert_eq!(
        format_workspace_symbols(&value),
        "No symbols found in workspace."
    );
}

#[test]
fn test_format_diagnostics_empty() {
    assert_eq!(format_diagnostics(&[]), "No diagnostics found.");
}

#[test]
fn test_format_diagnostics_with_entries() {
    let entries = vec![
        peri_resources::lsp::diagnostics::DiagnosticEntry {
            file_uri: "file:///src/main.rs".to_string(),
            line: 10,
            character: 5,
            severity: peri_resources::lsp::diagnostics::DiagnosticSeverity::Error,
            message: "expected `;`".to_string(),
            source: Some("rustc".to_string()),
        },
        peri_resources::lsp::diagnostics::DiagnosticEntry {
            file_uri: "file:///src/main.rs".to_string(),
            line: 15,
            character: 1,
            severity: peri_resources::lsp::diagnostics::DiagnosticSeverity::Warning,
            message: "unused variable".to_string(),
            source: None,
        },
    ];
    let result = format_diagnostics(&entries);
    assert!(result.contains("/src/main.rs:"));
    assert!(result.contains("[Error] expected `;`"));
    assert!(result.contains("[Warning] unused variable"));
}

#[test]
fn test_format_call_hierarchy_items_empty() {
    let value = serde_json::json!([]);
    assert_eq!(
        format_call_hierarchy_items(&value),
        "No call hierarchy item found at this position."
    );
}

#[test]
fn test_format_incoming_calls_empty() {
    assert_eq!(
        format_incoming_calls(&[]),
        "No incoming calls found (nothing calls this function)."
    );
}

#[test]
fn test_format_outgoing_calls_empty() {
    assert_eq!(
        format_outgoing_calls(&[]),
        "No outgoing calls found (this function calls nothing)."
    );
}

#[test]
fn test_uri_to_path_percent_decoded() {
    // 空格 percent-decode
    let uri: lsp_types::Uri = "file:///src/my%20file.rs".parse().unwrap();
    assert_eq!(uri_to_path(&uri), "/src/my file.rs");
}

#[test]
fn test_uri_to_path_chinese_decoded() {
    // 中文 percent-decode
    let uri: lsp_types::Uri = "file:///src/%E4%B8%AD%E6%96%87.rs".parse().unwrap();
    assert_eq!(uri_to_path(&uri), "/src/中文.rs");
}

#[test]
fn test_uri_to_path_windows_drive() {
    // Windows 盘符 file:///C:/ 形式：盘符保留，不丢失。
    // Unix 上保持正斜杠展示；Windows 上转反斜杠分隔（可直接用作文件路径）
    let uri: lsp_types::Uri = "file:///C:/Users/me/a.rs".parse().unwrap();
    #[cfg(windows)]
    assert_eq!(uri_to_path(&uri), "C:\\Users\\me\\a.rs");
    #[cfg(not(windows))]
    assert_eq!(uri_to_path(&uri), "/C:/Users/me/a.rs");
}

#[test]
fn test_format_locations_percent_decoded_path() {
    // 集成点：Location 的 URI 经 percent-decode 后展示为可读路径
    let loc = make_location("/src/my%20file.rs", 0, 0);
    let result = format_locations(&[loc]);
    assert!(result.contains("/src/my file.rs:1:1"), "got {result}");
}
