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

/// 回归测试：Windows 上 `command` 含空格时，不能被 PowerShell 单引号
/// 包围成字符串字面量。否则 `powershell -Command "'ping ...'"` 会把
/// `'ping ...'` 当作字符串 expression 直接 echo 出来，而不是执行命令。
///
/// 触发场景：Bash 工具调用 `shell_command("ping -n 60 127.0.0.1", &[])`，
/// 测试期望 1s 超时返回 Err，实际返回 Ok("ping -n 60 127.0.0.1\r\n")。
#[test]
fn test_shell_command_windows_command_not_string_literal() {
    let cmd = shell_command("ping -n 60 127.0.0.1", &[]);
    let formatted = format!("{cmd:?}");
    #[cfg(windows)]
    {
        // 错误形态：command 被单引号包围（PowerShell 字符串字面量）
        assert!(
            !formatted.contains("'ping -n 60 127.0.0.1'"),
            "command 被错误地用 PowerShell 单引号包围成字符串字面量，会导致 -Command echo 出字符串而非执行命令: {formatted}"
        );
    }
    #[cfg(not(windows))]
    {
        let _ = &formatted;
    }
}

/// 回归测试：Windows 上 args 仍应被 PowerShell 单引号 escape，
/// 防止 `$` `` ` `` `(` `)` `{` `}` `;` `|` `&` `@` `#` 等 metacharacter
/// 被 PowerShell 解析为代码（与 commit b689cc39 的安全意图一致）。
#[test]
fn test_shell_command_windows_args_still_escaped() {
    let cmd = shell_command("echo", &["$HOME", "a;b"]);
    let formatted = format!("{cmd:?}");
    #[cfg(windows)]
    {
        // 含 $ 或 ; 的 args 应被单引号包围成 PowerShell 字面量
        assert!(
            formatted.contains("'$HOME'"),
            "含 $ 的 arg 应被 PowerShell 单引号 escape: {formatted}"
        );
        assert!(
            formatted.contains("'a;b'"),
            "含 ; 的 arg 应被 PowerShell 单引号 escape: {formatted}"
        );
    }
    #[cfg(not(windows))]
    {
        let _ = &formatted;
    }
}
