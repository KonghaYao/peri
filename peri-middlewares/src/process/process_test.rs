use crate::process::shell_command;

#[test]
fn test_shell_command_unix_bash_c() {
    let cmd = shell_command("echo", &["hello"]);
    let formatted = format!("{cmd:?}");
    #[cfg(unix)]
    {
        assert!(
            formatted.contains("bash"),
            "expected bash, got: {formatted}"
        );
        assert!(
            formatted.contains("-c"),
            "expected -c flag, got: {formatted}"
        );
    }
    #[cfg(windows)]
    {
        assert!(
            formatted.contains("powershell"),
            "expected powershell, got: {formatted}"
        );
        assert!(
            formatted.contains("-Command"),
            "expected -Command flag, got: {formatted}"
        );
        assert!(
            formatted.contains("-NoProfile"),
            "expected -NoProfile flag, got: {formatted}"
        );
    }
}

#[test]
fn test_shell_command_no_args() {
    let cmd = shell_command("ls", &[]);
    let formatted = format!("{cmd:?}");
    #[cfg(unix)]
    {
        assert!(
            formatted.contains("bash"),
            "expected bash, got: {formatted}"
        );
        assert!(
            formatted.contains("ls"),
            "expected 'ls' in command, got: {formatted}"
        );
    }
    #[cfg(windows)]
    {
        assert!(
            formatted.contains("powershell"),
            "expected powershell, got: {formatted}"
        );
        assert!(
            formatted.contains("ls"),
            "expected 'ls' in command, got: {formatted}"
        );
    }
}

#[test]
fn test_shell_command_multi_args() {
    let cmd = shell_command("npx", &["-y", "@anthropic/mcp-server"]);
    let formatted = format!("{cmd:?}");
    #[cfg(unix)]
    {
        assert!(
            formatted.contains("bash"),
            "expected bash, got: {formatted}"
        );
        assert!(
            formatted.contains("npx"),
            "expected 'npx', got: {formatted}"
        );
    }
    #[cfg(windows)]
    {
        assert!(
            formatted.contains("powershell"),
            "expected powershell, got: {formatted}"
        );
        assert!(
            formatted.contains("npx"),
            "expected 'npx', got: {formatted}"
        );
        // 多参数应被拼接到命令字符串中
        assert!(
            formatted.contains("@anthropic/mcp-server"),
            "expected @anthropic/mcp-server in command, got: {formatted}"
        );
    }
}
