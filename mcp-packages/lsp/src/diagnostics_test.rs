use super::*;
use lsp_types::{Diagnostic, Position, Range};

fn make_params(
    uri: &str,
    diagnostics: Vec<(u32, u32, DiagnosticSeverity, &str)>,
) -> PublishDiagnosticsParams {
    PublishDiagnosticsParams {
        uri: format!("file://{uri}").parse().unwrap(),
        diagnostics: diagnostics
            .into_iter()
            .map(|(line, col, severity, msg)| Diagnostic {
                range: Range {
                    start: Position {
                        line,
                        character: col,
                    },
                    end: Position {
                        line,
                        character: col + 5,
                    },
                },
                severity: Some(match severity {
                    DiagnosticSeverity::Error => LspDiagnosticSeverity::ERROR,
                    DiagnosticSeverity::Warning => LspDiagnosticSeverity::WARNING,
                    DiagnosticSeverity::Information => LspDiagnosticSeverity::INFORMATION,
                    DiagnosticSeverity::Hint => LspDiagnosticSeverity::HINT,
                }),
                message: msg.to_string(),
                source: Some("test".to_string()),
                ..Default::default()
            })
            .collect(),
        version: None,
    }
}

#[test]
fn test_handle_diagnostics_basic() {
    let registry = DiagnosticsRegistry::new();
    let params = make_params(
        "/test.rs",
        vec![
            (0, 0, DiagnosticSeverity::Error, "error1"),
            (1, 0, DiagnosticSeverity::Warning, "warn1"),
        ],
    );
    registry.handle_publish_diagnostics(&params);

    let all = registry.get_all();
    assert_eq!(all.len(), 2);

    let summary = registry.summary();
    assert_eq!(summary.errors, 1);
    assert_eq!(summary.warnings, 1);
    assert_eq!(summary.files_with_errors, 1);
}

#[test]
fn test_deduplication() {
    let registry = DiagnosticsRegistry::new();
    let params = make_params(
        "/test.rs",
        vec![
            (0, 0, DiagnosticSeverity::Error, "error1"),
            (1, 0, DiagnosticSeverity::Warning, "warn1"),
        ],
    );
    registry.handle_publish_diagnostics(&params);

    // 再次推送相同诊断
    registry.handle_publish_diagnostics(&params);

    // 应该只有 2 条（不重复）
    let all = registry.get_all();
    assert_eq!(all.len(), 2);
}

#[test]
fn test_per_file_limit() {
    let registry = DiagnosticsRegistry::new();
    let diagnostics: Vec<(u32, u32, DiagnosticSeverity, &str)> = (0..20)
        .map(|i| (i, 0, DiagnosticSeverity::Error, "error"))
        .collect();
    let params = make_params("/test.rs", diagnostics);
    registry.handle_publish_diagnostics(&params);

    let all = registry.get_all();
    assert_eq!(all.len(), MAX_DIAGNOSTICS_PER_FILE);
}

#[test]
fn test_clear_diagnostics() {
    let registry = DiagnosticsRegistry::new();
    let params = make_params(
        "/test.rs",
        vec![(0, 0, DiagnosticSeverity::Error, "error1")],
    );
    registry.handle_publish_diagnostics(&params);
    assert_eq!(registry.get_all().len(), 1);

    // 发送空诊断清除
    let clear_params = PublishDiagnosticsParams {
        uri: "file:///test.rs".parse().unwrap(),
        diagnostics: vec![],
        version: None,
    };
    registry.handle_publish_diagnostics(&clear_params);
    assert!(registry.get_all().is_empty());
}

#[test]
fn test_clear_all() {
    // clear_all 清空当前诊断与统计
    let registry = DiagnosticsRegistry::new();
    registry.handle_publish_diagnostics(&make_params(
        "/test.rs",
        vec![(0, 0, DiagnosticSeverity::Error, "error1")],
    ));
    assert_eq!(registry.get_all().len(), 1);

    registry.clear_all();
    assert!(registry.get_all().is_empty());
    assert_eq!(registry.summary().total(), 0);
}

#[test]
fn test_publish_after_clear_all_repopulates() {
    // clear_all 清空后，重新发布同集合应恢复全量状态（无去重缓存残留）
    let registry = DiagnosticsRegistry::new();
    let params = make_params(
        "/test.rs",
        vec![
            (0, 0, DiagnosticSeverity::Error, "error1"),
            (1, 0, DiagnosticSeverity::Warning, "warn1"),
        ],
    );
    registry.handle_publish_diagnostics(&params);
    registry.clear_all();
    assert!(registry.get_all().is_empty(), "clear_all 后应为空");

    registry.handle_publish_diagnostics(&params);
    let all = registry.get_all();
    assert_eq!(all.len(), 2, "clear_all 后重新发布应恢复全量");
    assert_eq!(registry.summary().errors, 1);
    assert_eq!(registry.summary().warnings, 1);
}

#[test]
fn test_severity_sorting() {
    let registry = DiagnosticsRegistry::new();
    let params = make_params(
        "/test.rs",
        vec![
            (3, 0, DiagnosticSeverity::Hint, "hint"),
            (0, 0, DiagnosticSeverity::Error, "error"),
            (2, 0, DiagnosticSeverity::Information, "info"),
            (1, 0, DiagnosticSeverity::Warning, "warn"),
        ],
    );
    registry.handle_publish_diagnostics(&params);

    let all = registry.get_all();
    assert_eq!(all[0].severity, DiagnosticSeverity::Error);
    assert_eq!(all[1].severity, DiagnosticSeverity::Warning);
    assert_eq!(all[2].severity, DiagnosticSeverity::Information);
    assert_eq!(all[3].severity, DiagnosticSeverity::Hint);
}

#[test]
fn test_partial_overlap_keeps_full_set() {
    let registry = DiagnosticsRegistry::new();
    // 第一次发布 [error1, error2]
    registry.handle_publish_diagnostics(&make_params(
        "/test.rs",
        vec![
            (0, 0, DiagnosticSeverity::Error, "error1"),
            (1, 0, DiagnosticSeverity::Error, "error2"),
        ],
    ));
    // 第二次发布部分重叠集合 [error2, warn3]
    registry.handle_publish_diagnostics(&make_params(
        "/test.rs",
        vec![
            (1, 0, DiagnosticSeverity::Error, "error2"),
            (2, 0, DiagnosticSeverity::Warning, "warn3"),
        ],
    ));

    // current 应以服务器发布的全量为准：保留 error2、新增 warn3，error1 已被移除
    let all = registry.get_all();
    assert_eq!(all.len(), 2);
    let messages: Vec<&str> = all.iter().map(|e| e.message.as_str()).collect();
    assert!(messages.contains(&"error2"));
    assert!(messages.contains(&"warn3"));
    assert!(!messages.contains(&"error1"));
}

#[test]
fn test_identical_set_no_stale_diagnostics() {
    let registry = DiagnosticsRegistry::new();
    registry.handle_publish_diagnostics(&make_params(
        "/test.rs",
        vec![
            (0, 0, DiagnosticSeverity::Error, "error1"),
            (1, 0, DiagnosticSeverity::Error, "error2"),
        ],
    ));
    // 部分重叠更新
    registry.handle_publish_diagnostics(&make_params(
        "/test.rs",
        vec![
            (1, 0, DiagnosticSeverity::Error, "error2"),
            (2, 0, DiagnosticSeverity::Warning, "warn3"),
        ],
    ));
    // 再次推送完全相同集合：无新增，但仍应以全量刷新 current，不残留陈旧诊断
    registry.handle_publish_diagnostics(&make_params(
        "/test.rs",
        vec![
            (1, 0, DiagnosticSeverity::Error, "error2"),
            (2, 0, DiagnosticSeverity::Warning, "warn3"),
        ],
    ));

    let all = registry.get_all();
    assert_eq!(all.len(), 2);
    let messages: Vec<&str> = all.iter().map(|e| e.message.as_str()).collect();
    assert!(messages.contains(&"error2"));
    assert!(messages.contains(&"warn3"));
}
