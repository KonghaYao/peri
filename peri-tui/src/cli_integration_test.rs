//! CLI 参数解析集成测试

use clap::Parser;

#[derive(Parser)]
#[command(name = "peri")]
struct TestCli {
    #[arg(short = 'a', long = "approve")]
    approve: bool,
    #[arg(short = 'p', long = "print")]
    print: Option<Option<String>>,
    #[arg(long = "output-format", visible_alias = "outputFormat")]
    output_format: Option<String>,
    #[arg(long = "max-turns", visible_alias = "maxTurns")]
    max_turns: Option<u32>,
    #[arg(long = "bare")]
    bare: bool,
    #[arg(long = "permission-mode", visible_alias = "permissionMode")]
    permission_mode: Option<String>,
    #[arg(long = "dangerously-skip-permissions")]
    skip_permissions: bool,
    #[arg(long = "model")]
    model: Option<String>,
    #[arg(long = "effort")]
    effort: Option<String>,
    #[arg(short = 'c', long = "continue")]
    cont: bool,
    #[arg(short = 'r', long = "resume")]
    resume: Option<Option<String>>,
    #[arg(long = "session-id", visible_alias = "sessionId")]
    session_id: Option<String>,
    #[arg(short = 'n', long = "name")]
    session_name: Option<String>,
    #[arg(long = "no-session-persistence")]
    no_session_persistence: bool,
    #[arg(long = "allowedTools", visible_alias = "allowed-tools")]
    allowed_tools: Option<Vec<String>>,
    #[arg(long = "disallowedTools", visible_alias = "disallowed-tools")]
    disallowed_tools: Option<Vec<String>>,
    #[arg(long = "settings")]
    settings: Option<String>,
}

#[test]
fn test_print_with_prompt() {
    let cli = TestCli::try_parse_from(["peri", "-p", "hello world"]);
    assert!(cli.is_ok());
    let cli = cli.unwrap();
    assert_eq!(cli.print, Some(Some("hello world".to_string())));
}

#[test]
fn test_print_without_prompt() {
    let cli = TestCli::try_parse_from(["peri", "-p"]);
    assert!(cli.is_ok());
    let cli = cli.unwrap();
    assert_eq!(cli.print, Some(None));
}

#[test]
fn test_output_format_aliases() {
    let cli = TestCli::try_parse_from(["peri", "--output-format", "json"]);
    assert!(cli.is_ok());
    let cli = TestCli::try_parse_from(["peri", "--outputFormat", "json"]);
    assert!(cli.is_ok());
}

#[test]
fn test_permission_mode_aliases() {
    let cli = TestCli::try_parse_from(["peri", "--permission-mode", "bypass"]);
    assert!(cli.is_ok());
    let cli = TestCli::try_parse_from(["peri", "--permissionMode", "bypass"]);
    assert!(cli.is_ok());
}

#[test]
fn test_allowed_tools() {
    let cli = TestCli::try_parse_from(["peri", "--allowedTools", "Bash", "--allowedTools", "Edit"]);
    assert!(cli.is_ok());
    let cli = cli.unwrap();
    assert_eq!(
        cli.allowed_tools,
        Some(vec!["Bash".to_string(), "Edit".to_string()])
    );
}

#[test]
fn test_resume_with_value() {
    let cli = TestCli::try_parse_from(["peri", "-r", "abc-123"]);
    assert!(cli.is_ok());
    let cli = cli.unwrap();
    assert_eq!(cli.resume, Some(Some("abc-123".to_string())));
}

#[test]
fn test_resume_without_value() {
    let cli = TestCli::try_parse_from(["peri", "-r"]);
    assert!(cli.is_ok());
    let cli = cli.unwrap();
    assert_eq!(cli.resume, Some(None));
}

#[test]
fn test_combined_model_effort() {
    let cli = TestCli::try_parse_from(["peri", "--model", "sonnet", "--effort", "high"]);
    assert!(cli.is_ok());
    let cli = cli.unwrap();
    assert_eq!(cli.model, Some("sonnet".to_string()));
    assert_eq!(cli.effort, Some("high".to_string()));
}

#[test]
fn test_disallowed_tools_alias() {
    let cli = TestCli::try_parse_from(["peri", "--disallowed-tools", "WebFetch"]);
    assert!(cli.is_ok());
    let cli = cli.unwrap();
    assert_eq!(cli.disallowed_tools, Some(vec!["WebFetch".to_string()]));
}
