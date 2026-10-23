use super::*;

#[test]
fn unix_duration_threshold_and_prefixes() {
    for command in [
        "sleep 60",
        "sleep 1m",
        "sleep 1.5m",
        "sleep 30 30",
        "sleep 0.02h",
        "sleep 1d",
        "sleep infinity",
        "timeout 10 sleep 115",
        "nohup command exec env X=1 sleep 115",
        "env -u X timeout --signal TERM 10 sleep 115",
    ] {
        let rejection = reject_long_sleep(command, false, ShellPlatform::Unix).expect(command);
        assert_eq!(rejection.code, "foreground_long_sleep");
        assert!(rejection.detected_ms >= THRESHOLD_MS);
        assert!(rejection.text().contains("started: false"));
    }
    for command in [
        "sleep 59",
        "sleep .1m",
        "sleep 0.001",
        "sleep $SECONDS",
        "sleep invalid",
    ] {
        assert!(
            reject_long_sleep(command, false, ShellPlatform::Unix).is_none(),
            "{command}"
        );
    }
}

#[test]
fn command_positions_and_background_policy() {
    for command in [
        "echo hi; sleep 115",
        "sleep 115 && echo ok",
        "echo hi || sleep 115",
        "sleep 115 | cat",
        "echo hi & sleep 115",
        "echo hi\nsleep 115",
        "(sleep 115)",
        "{ sleep 115; }",
        "echo `sleep 115`",
        "echo $(sleep 115)",
    ] {
        assert!(
            reject_long_sleep(command, false, ShellPlatform::Unix).is_some(),
            "{command}"
        );
    }
    assert!(reject_long_sleep("sleep 115", true, ShellPlatform::Unix).is_some());
    assert!(reject_long_sleep("timeout 1 sleep 115", true, ShellPlatform::Unix).is_some());
    for command in [
        "sleep 115 && echo ok",
        "echo hi; sleep 115",
        "sleep 115 | cat",
        "sleep 115 &",
        "nohup sleep 60 &",
    ] {
        assert!(
            reject_long_sleep(command, true, ShellPlatform::Unix).is_none(),
            "{command}"
        );
    }
    assert!(reject_long_sleep("nohup sleep 60 &", false, ShellPlatform::Unix).is_none());
}

#[test]
fn quoted_text_comments_heredoc_and_scripts_are_not_commands() {
    for command in [
        "echo \"sleep 115\"",
        "echo 'sleep 115'",
        "grep sleep file",
        "grep 'sleep 115' file",
        "# sleep 115\necho hi",
        "echo hi # sleep 115",
        "cat <<EOF\nsleep 115\nEOF\necho ok",
        "cat <<'EOF'\nsleep 115\nEOF",
        "cat <<-EOF\n\tsleep 115\n\tEOF",
        "bash -c 'sleep 115'",
        "sh -c \"sleep 115\"",
        "echo \"$(sleep 115)\"",
    ] {
        assert!(
            reject_long_sleep(command, false, ShellPlatform::Unix).is_none(),
            "{command}"
        );
    }
    assert!(reject_long_sleep(
        "cat <<EOF\nsleep 115\nEOF\nsleep 60",
        false,
        ShellPlatform::Unix
    )
    .is_some());
}

#[test]
fn powershell_duration_forms() {
    for command in [
        "Start-Sleep 60",
        "Start-Sleep -Seconds 60",
        "sleep -s 60",
        "start-sleep -Milliseconds 60000",
        "echo hi; Start-Sleep -Seconds 115",
    ] {
        assert!(
            reject_long_sleep(command, false, ShellPlatform::Windows).is_some(),
            "{command}"
        );
    }
    for command in [
        "sleep 59",
        "Start-Sleep -Milliseconds 59999",
        "Write-Output 'sleep 115'",
    ] {
        assert!(
            reject_long_sleep(command, false, ShellPlatform::Windows).is_none(),
            "{command}"
        );
    }
}

#[tokio::test]
async fn rejected_before_execution_or_log_creation() {
    use peri_agent::tools::ToolContext;
    let dir = tempfile::tempdir().unwrap();
    let tool = crate::terminal::BashTool::new(dir.path().to_string_lossy());
    for background in [false, true] {
        let command = if background {
            "sleep 115"
        } else {
            "echo sentinel > sentinel; sleep 115"
        };
        let result = tool
            .execute(
                serde_json::json!({"command":command,"run_in_background":background}),
                ToolContext::new(&[], "."),
            )
            .await
            .unwrap();
        assert!(result.rejection.is_some());
        assert!(result.output.execution.is_none());
        assert_eq!(result.into_tool_output().is_error(), Some(true));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
