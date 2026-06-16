use super::ws_handler::WsQuery;

#[test]
fn test_ws_query_parses_shell_and_dimensions() {
    let q = WsQuery {
        shell: Some("/bin/zsh".to_string()),
        args: Some("-l".to_string()),
        cols: Some("100".to_string()),
        rows: Some("30".to_string()),
    };
    let parsed = q.to_spawn_params();
    assert_eq!(parsed.shell, "/bin/zsh");
    assert_eq!(parsed.args, vec!["-l"]);
    assert_eq!(parsed.cols, 100);
    assert_eq!(parsed.rows, 30);
}

#[test]
fn test_ws_query_defaults_when_missing() {
    let q = WsQuery {
        shell: None,
        args: None,
        cols: None,
        rows: None,
    };
    let parsed = q.to_spawn_params();
    // shell 缺省时 fallback 到 default_shell()（env SHELL 或 /bin/bash）
    assert!(!parsed.shell.is_empty(), "shell 应有默认值");
    assert!(parsed.args.is_empty());
    assert_eq!(parsed.cols, 80);
    assert_eq!(parsed.rows, 24);
}

#[test]
fn test_ws_query_args_split_by_whitespace() {
    let q = WsQuery {
        shell: None,
        args: Some("-l  --verbose".to_string()),
        cols: None,
        rows: None,
    };
    let parsed = q.to_spawn_params();
    // 多个空格应被过滤
    assert_eq!(parsed.args, vec!["-l", "--verbose"]);
}
