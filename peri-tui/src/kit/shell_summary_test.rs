use super::*;

fn chain(command: &str) -> String {
    display_for(command)
        .unwrap_or_else(|| panic!("命令应可解析: {command:?}"))
        .tool_chain
}

#[test]
fn strips_cd_and_reports_pipeline_tools() {
    assert_eq!(
        chain("cd /Users/mino/code/peri && grep -n \"^target\\|/target\" .gitignore | head -5"),
        "grep | head"
    );
}

#[test]
fn keeps_subcommand_for_multi_level_clis() {
    assert_eq!(chain("git -C repo status --short"), "git status");
    assert_eq!(chain("git --git-dir=.git status"), "git status");
    assert_eq!(chain("cargo +nightly test -p peri-tui"), "cargo test");
    assert_eq!(chain("cargo --manifest-path=Cargo.toml test"), "cargo test");
    assert_eq!(chain("npm --prefix web run build"), "npm run");
    assert_eq!(chain("kubectl -n prod get pods"), "kubectl get");
}

#[test]
fn drops_environment_assignments_and_wrappers() {
    assert_eq!(chain("FOO=bar cargo test -p x | tail"), "cargo test | tail");
    assert_eq!(chain("sudo -u root cargo test"), "cargo test");
    assert_eq!(chain("timeout -s KILL 30 cargo test"), "cargo test");
    assert_eq!(
        chain("TMPH=$(mktemp -d) && mkdir -p \"$TMPH/.peri\""),
        "mkdir"
    );
}

#[test]
fn does_not_split_inside_quotes_or_escapes() {
    assert_eq!(chain("echo \"a && b\" | cat"), "echo | cat");
    assert_eq!(
        chain("grep \"^target\\|/target\" .gitignore | head"),
        "grep | head"
    );
}

#[test]
fn stops_before_heredoc_body() {
    let display = display_for("cat <<'EOF'\nhello\nEOF").expect("heredoc 命令");
    assert_eq!(display.tool_chain, "cat");
    assert_eq!(display.command, "cat <<'EOF'\nhello\nEOF");
}

#[test]
fn follows_xargs_into_the_real_program() {
    assert_eq!(
        chain("find . -name '*.rs' -print0 | xargs -0 grep -n foo"),
        "find | xargs grep"
    );
}

#[test]
fn sequence_and_pipeline_connectors_are_distinct() {
    assert_eq!(
        chain("cargo build && cargo test | tail"),
        "cargo build ; cargo test | tail"
    );
}

#[test]
fn caps_visible_segments_and_keeps_last_count() {
    assert_eq!(chain("a && b && c && d && e"), "a ; b ; c ; d … +1");
}

#[test]
fn complex_control_flow_falls_back_to_first_line() {
    assert_eq!(
        chain("if [ -f x ]; then echo hi; fi"),
        "if [ -f x ]; then echo hi; fi"
    );
}

#[test]
fn command_without_tool_stage_falls_back() {
    assert_eq!(chain("cd /tmp"), "cd /tmp");
    assert_eq!(chain("source .env"), "source .env");
}

#[test]
fn unexpected_syntax_falls_back_without_dropping_the_command() {
    let display = display_for("grep \"unterminated").expect("命令仍应展示");
    assert_eq!(display.tool_chain, "grep \"unterminated");
    assert_eq!(display.command, "grep \"unterminated");
}

#[test]
fn empty_command_has_no_display() {
    assert!(display_for("   \n  ").is_none());
}

#[test]
fn full_command_is_preserved_verbatim() {
    let command = "cd /tmp && grep -n target Cargo.toml | head -5";
    let display = display_for(command).expect("命令");
    assert_eq!(display.tool_chain, "grep | head");
    assert_eq!(display.command, command);
}
