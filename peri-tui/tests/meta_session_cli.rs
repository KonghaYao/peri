use std::path::Path;
use std::process::{Command, Output};

use chrono::{TimeZone, Utc};
use peri_acp_types::store::ThreadStore;
use peri_acp_types::thread::{AgentStatus, ThreadMeta};
use peri_resources::sessions::SqliteThreadStore;
use serial_test::serial;
use tempfile::TempDir;

const SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const OTHER_SESSION_ID: &str = "6ba7b810-9dad-41d1-80b4-00c04fd430c8";
const FORBIDDEN_CONFIG: &str = "forbidden-config-secret";
const FORBIDDEN_CONTEXT: &str = "forbidden-cached-context";
const FORBIDDEN_SNAPSHOT: &str = "forbidden-snapshot-id";

fn peri_command(home: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_peri"));
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("PERI_CONFIG_FILE")
        .env_remove("PERI_META_TEST_INTERNAL_ERROR");
    command
}

fn run_peri(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    peri_command(home, cwd).args(args).output().unwrap()
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap()
}

fn assert_success(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        text(&output.stderr)
    );
    assert!(output.stderr.is_empty(), "stderr: {}", text(&output.stderr));
    assert!(!output.stdout.is_empty());
}

fn assert_json_error(output: &Output, exit_code: i32, kind: &str, message: &str) {
    assert_eq!(output.status.code(), Some(exit_code));
    assert!(output.stdout.is_empty(), "stdout: {}", text(&output.stdout));
    assert_eq!(
        text(&output.stderr),
        format!(
            "{{\"schemaVersion\":1,\"error\":{{\"kind\":\"{kind}\",\"message\":\"{message}\"}}}}\n"
        )
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 2);
}

fn assert_human_error(output: &Output, exit_code: i32, kind: &str, message: &str) {
    assert_eq!(output.status.code(), Some(exit_code));
    assert!(output.stdout.is_empty(), "stdout: {}", text(&output.stdout));
    assert_eq!(text(&output.stderr), format!("{kind}: {message}\n"));
}

fn fixture_meta(id: &str, title: &str, cwd: &str) -> ThreadMeta {
    ThreadMeta {
        id: id.to_owned(),
        title: Some(title.to_owned()),
        cwd: cwd.to_owned(),
        created_at: Utc.with_ymd_and_hms(2026, 9, 4, 1, 2, 3).unwrap(),
        updated_at: Utc.with_ymd_and_hms(2026, 9, 4, 4, 5, 6).unwrap(),
        message_count: 7,
        content_size: 99,
        parent_thread_id: None,
        snapshot_at_message_id: Some(FORBIDDEN_SNAPSHOT.to_owned()),
        hidden: true,
        cancel_policy: Default::default(),
        config: Some(FORBIDDEN_CONFIG.to_owned()),
        cached_context: Some(FORBIDDEN_CONTEXT.to_owned()),
        agent_status: AgentStatus::Done,
    }
}

fn create_database(path: &Path, metas: Vec<ThreadMeta>) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let store = SqliteThreadStore::new(path).await.unwrap();
        for meta in metas {
            store.create_thread(meta).await.unwrap();
        }
    });
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
}

fn assert_forbidden_output(output: &Output) {
    let combined = format!("{}{}", text(&output.stdout), text(&output.stderr));
    for forbidden in [FORBIDDEN_CONFIG, FORBIDDEN_CONTEXT, FORBIDDEN_SNAPSHOT] {
        assert!(
            !combined.contains(forbidden),
            "leaked forbidden value: {forbidden}"
        );
    }
    for forbidden_name in [
        "config",
        "cachedContext",
        "snapshotAtMessageId",
        "contentSize",
        "hidden",
        "cancelPolicy",
        "messages",
        "prompt",
        "token",
    ] {
        assert!(
            !combined.contains(forbidden_name),
            "leaked forbidden field: {forbidden_name}"
        );
    }
}

#[test]
fn human_success_is_stdout_only_and_escapes_persisted_controls() {
    let sandbox = TempDir::new().unwrap();
    let db = sandbox.path().join("threads.db");
    create_database(
        &db,
        vec![fixture_meta(
            SESSION_ID,
            "safe\n\t\u{1b}[31m",
            "/tmp/project\rnext",
        )],
    );

    let output = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            db.to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
        ],
    );

    assert_success(&output);
    assert_eq!(
        text(&output.stdout),
        "Schema version: 1\nID: 550e8400-e29b-41d4-a716-446655440000\nTitle: safe\\n\\t\\u{1b}[31m\nCWD: /tmp/project\\rnext\nCreated at: 2026-09-04T01:02:03+00:00\nUpdated at: 2026-09-04T04:05:06+00:00\nMessage count: 7\nParent thread ID: null\nPersisted agent status: done\n"
    );
    assert_forbidden_output(&output);
}

#[test]
fn json_success_is_one_exact_allowlisted_object() {
    let sandbox = TempDir::new().unwrap();
    let db = sandbox.path().join("threads.db");
    create_database(
        &db,
        vec![fixture_meta(SESSION_ID, "database-a", "/project/a")],
    );

    let output = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            db.to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );

    assert_success(&output);
    assert_eq!(text(&output.stdout).lines().count(), 1);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "schemaVersion": 1,
            "id": SESSION_ID,
            "title": "database-a",
            "cwd": "/project/a",
            "createdAt": "2026-09-04T01:02:03+00:00",
            "updatedAt": "2026-09-04T04:05:06+00:00",
            "messageCount": 7,
            "parentThreadId": null,
            "persistedAgentStatus": "done"
        })
    );
    assert_forbidden_output(&output);
}

#[test]
fn legal_non_v7_uuid_preserves_original_text_lookup() {
    let sandbox = TempDir::new().unwrap();
    let db = sandbox.path().join("uppercase-id.db");
    let uppercase_id = SESSION_ID.to_ascii_uppercase();
    create_database(
        &db,
        vec![fixture_meta(&uppercase_id, "uppercase", "/uppercase")],
    );

    let output = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            db.to_str().unwrap(),
            "meta",
            "session",
            &uppercase_id,
            "--json",
        ],
    );
    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["id"], uppercase_id);
    assert_eq!(value["title"], "uppercase");
}

#[test]
fn invalid_id_wins_over_missing_database_without_creating_it() {
    let sandbox = TempDir::new().unwrap();
    let db = sandbox.path().join("missing-parent").join("threads.db");

    let output = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            db.to_str().unwrap(),
            "meta",
            "session",
            "not-a-uuid",
            "--json",
        ],
    );

    assert_json_error(
        &output,
        2,
        "invalid_session_id",
        "session ID must be a valid UUID",
    );
    assert!(!db.exists());
    assert!(!db.parent().unwrap().exists());
    assert_forbidden_output(&output);
}

#[test]
fn explicit_database_selection_never_falls_back_to_another_database() {
    let sandbox = TempDir::new().unwrap();
    let db_a = sandbox.path().join("a.db");
    let db_b = sandbox.path().join("b.db");
    create_database(&db_a, vec![fixture_meta(OTHER_SESSION_ID, "only-a", "/a")]);
    create_database(&db_b, vec![fixture_meta(SESSION_ID, "only-b", "/b")]);

    let missing = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            db_a.to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );
    assert_json_error(&missing, 3, "session_not_found", "session was not found");

    let selected = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            db_b.to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );
    assert_success(&selected);
    let value: serde_json::Value = serde_json::from_slice(&selected.stdout).unwrap();
    assert_eq!(value["title"], "only-b");
    assert_eq!(value["cwd"], "/b");
}

#[test]
#[serial]
fn default_database_uses_only_isolated_home() {
    let sandbox = TempDir::new().unwrap();
    let home = sandbox.path().join("home");
    let cwd = sandbox.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let db = home.join(".peri/threads/threads.db");
    create_database(
        &db,
        vec![fixture_meta(SESSION_ID, "default-home", "/isolated")],
    );

    let output = run_peri(&home, &cwd, &["meta", "session", SESSION_ID, "--json"]);

    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["title"], "default-home");
    assert_eq!(value["cwd"], "/isolated");
    assert_forbidden_output(&output);
}

#[test]
fn explicit_database_selection_returns_each_selected_database_value() {
    let sandbox = TempDir::new().unwrap();
    let db_a = sandbox.path().join("a-same-id.db");
    let db_b = sandbox.path().join("b-same-id.db");
    create_database(&db_a, vec![fixture_meta(SESSION_ID, "selected-a", "/a")]);
    create_database(&db_b, vec![fixture_meta(SESSION_ID, "selected-b", "/b")]);

    for (db, expected_title) in [(&db_a, "selected-a"), (&db_b, "selected-b")] {
        let output = run_peri(
            sandbox.path(),
            sandbox.path(),
            &[
                "--db-path",
                db.to_str().unwrap(),
                "meta",
                "session",
                SESSION_ID,
                "--json",
            ],
        );
        assert_success(&output);
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["title"], expected_title);
    }
}

#[test]
#[serial]
fn missing_default_database_does_not_create_home_paths() {
    let sandbox = TempDir::new().unwrap();
    let home = sandbox.path().join("unused-home");
    let cwd = sandbox.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();

    let output = run_peri(&home, &cwd, &["meta", "session", SESSION_ID, "--json"]);

    assert_json_error(
        &output,
        3,
        "database_not_found",
        "thread database was not found",
    );
    assert!(!home.exists());
}

#[test]
fn missing_database_and_incompatible_schema_have_stable_process_contracts() {
    let sandbox = TempDir::new().unwrap();
    let missing = sandbox.path().join("missing.db");
    let missing_output = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            missing.to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );
    assert_json_error(
        &missing_output,
        3,
        "database_not_found",
        "thread database was not found",
    );
    assert!(!missing.exists());

    let incompatible = sandbox.path().join("incompatible.db");
    std::fs::write(&incompatible, b"not a sqlite database").unwrap();
    let incompatible_output = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            incompatible.to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );
    assert_json_error(
        &incompatible_output,
        4,
        "schema_incompatible",
        "thread database schema is incompatible",
    );
    assert_forbidden_output(&incompatible_output);
}

#[test]
fn ordinary_meta_values_preserve_real_process_clap_behavior() {
    let sandbox = TempDir::new().unwrap();
    let help_cases: &[&[&str]] = &[
        &["-p", "meta", "--help"],
        &["--print", "meta", "--help"],
        &["-r", "meta", "--help"],
        &["--resume", "meta", "--help"],
        &["--help", "meta", "session", SESSION_ID, "--json"],
    ];

    for args in help_cases {
        let output = run_peri(sandbox.path(), sandbox.path(), args);
        assert_eq!(output.status.code(), Some(0), "args: {args:?}");
        assert!(!output.stdout.is_empty(), "args: {args:?}");
        assert!(output.stderr.is_empty(), "args: {args:?}");
        assert!(
            text(&output.stdout).contains("Usage: peri"),
            "args: {args:?}"
        );
    }

    let error_cases: &[&[&str]] = &[
        &["--model", "meta", "--definitely-invalid"],
        &["--model", "meta", "--effort"],
        &["--model", "meta", "--settings"],
        &["--db-path", "meta", "session", SESSION_ID, "--json"],
        &[
            "--definitely-invalid",
            "value",
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
        &[
            "acp",
            "--definitely-invalid",
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    ];
    for args in error_cases {
        let output = run_peri(sandbox.path(), sandbox.path(), args);
        assert_eq!(output.status.code(), Some(2), "args: {args:?}");
        assert!(output.stdout.is_empty(), "args: {args:?}");
        assert!(!output.stderr.is_empty(), "args: {args:?}");
        assert!(
            !text(&output.stderr).contains("invalid Meta command arguments"),
            "args: {args:?}"
        );
        assert!(text(&output.stderr).starts_with("error:"), "args: {args:?}");
        assert!(text(&output.stderr).contains("--help"), "args: {args:?}");
    }
}

#[test]
fn json_grammar_failures_are_single_stable_process_errors() {
    let sandbox = TempDir::new().unwrap();
    let cases: &[&[&str]] = &[
        &["meta", "session", "--json"],
        &[
            "meta",
            "session",
            SESSION_ID,
            "--include",
            "messages",
            "--json",
        ],
        &["meta", "list", "--json"],
        &["--print=prompt", "meta", "session", SESSION_ID, "--json"],
        &["-p", "meta", "session", SESSION_ID, "--json"],
        &["--print", "meta", "session", SESSION_ID, "--json"],
        &["-r", "meta", "session", SESSION_ID, "--json"],
        &["--resume", "meta", "session", SESSION_ID, "--json"],
        &[
            "--definitely-invalid",
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
        &[
            "--db-path",
            "--definitely-invalid",
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
        &[
            "--approve",
            "--definitely-invalid",
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
        &[
            "--db-path",
            "--definitely-invalid",
            "--another-invalid",
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    ];

    for args in cases {
        let output = run_peri(sandbox.path(), sandbox.path(), args);
        assert_json_error(
            &output,
            2,
            "invalid_argument",
            "invalid Meta command arguments",
        );
    }
}

#[test]
fn malformed_prefix_meta_shape_has_stable_real_process_errors() {
    let sandbox = TempDir::new().unwrap();
    let cases: &[&[&str]] = &[
        &["--definitely-invalid", "meta", "session", SESSION_ID],
        &[
            "--db-path",
            "--definitely-invalid",
            "meta",
            "session",
            SESSION_ID,
        ],
    ];

    for args in cases {
        let human = run_peri(sandbox.path(), sandbox.path(), args);
        assert_human_error(
            &human,
            2,
            "invalid_argument",
            "invalid Meta command arguments",
        );

        let mut json_args = args.to_vec();
        json_args.push("--json");
        let json = run_peri(sandbox.path(), sandbox.path(), &json_args);
        assert_json_error(
            &json,
            2,
            "invalid_argument",
            "invalid Meta command arguments",
        );
    }
}

#[test]
fn every_unrelated_top_level_option_is_rejected_by_real_process() {
    let sandbox = TempDir::new().unwrap();
    let cases: &[&[&str]] = &[
        &["--approve"],
        &["--print=prompt"],
        &["--output-format", "json"],
        &["--max-turns", "1"],
        &["--bare"],
        &["--permission-mode", "default"],
        &["--dangerously-skip-permissions"],
        &["--model", "sonnet"],
        &["--effort", "high"],
        &["--continue"],
        &["--resume=session"],
        &["--session-id", SESSION_ID],
        &["--name", "name"],
        &["--no-session-persistence"],
        &["--allowedTools", "Bash"],
        &["--disallowedTools", "Edit"],
        &["--settings", "{}"],
        &["--config-file", "/does/not/exist"],
    ];

    for unrelated in cases {
        let mut args = unrelated.to_vec();
        args.extend_from_slice(&["meta", "session", SESSION_ID, "--json"]);
        let output = run_peri(sandbox.path(), sandbox.path(), &args);
        assert_json_error(
            &output,
            2,
            "invalid_argument",
            "invalid Meta command arguments",
        );
    }
}

#[test]
fn every_error_kind_has_human_real_binary_stream_and_exit_evidence() {
    let sandbox = TempDir::new().unwrap();

    let grammar = run_peri(sandbox.path(), sandbox.path(), &["meta", "list"]);
    assert_human_error(
        &grammar,
        2,
        "invalid_argument",
        "invalid Meta command arguments",
    );

    let invalid = run_peri(
        sandbox.path(),
        sandbox.path(),
        &["meta", "session", "not-a-uuid"],
    );
    assert_human_error(
        &invalid,
        2,
        "invalid_session_id",
        "session ID must be a valid UUID",
    );

    let missing_path = sandbox.path().join("missing.db");
    let missing_database = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            missing_path.to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
        ],
    );
    assert_human_error(
        &missing_database,
        3,
        "database_not_found",
        "thread database was not found",
    );

    let unreadable = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            sandbox.path().to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
        ],
    );
    assert_human_error(
        &unreadable,
        4,
        "database_unreadable",
        "thread database could not be opened for reading",
    );

    let incompatible_path = sandbox.path().join("incompatible.db");
    std::fs::write(&incompatible_path, b"not sqlite").unwrap();
    let incompatible = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            incompatible_path.to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
        ],
    );
    assert_human_error(
        &incompatible,
        4,
        "schema_incompatible",
        "thread database schema is incompatible",
    );

    let compatible_path = sandbox.path().join("compatible.db");
    create_database(
        &compatible_path,
        vec![fixture_meta(OTHER_SESSION_ID, "other", "/other")],
    );
    let session_missing = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            compatible_path.to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
        ],
    );
    assert_human_error(
        &session_missing,
        3,
        "session_not_found",
        "session was not found",
    );

    let corrupt_path = sandbox.path().join("corrupt-human.db");
    let mut corrupt = fixture_meta(SESSION_ID, "corrupt", "/corrupt");
    corrupt.message_count = usize::MAX;
    create_database(&corrupt_path, vec![corrupt]);
    let corrupt_output = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            corrupt_path.to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
        ],
    );
    assert_human_error(
        &corrupt_output,
        4,
        "corrupt_session_data",
        "stored session metadata is corrupt",
    );
}

#[test]
fn human_argument_and_storage_failures_are_stderr_only() {
    let sandbox = TempDir::new().unwrap();
    let invalid = run_peri(
        sandbox.path(),
        sandbox.path(),
        &["meta", "session", "not-a-uuid"],
    );
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert_eq!(
        text(&invalid.stderr),
        "invalid_session_id: session ID must be a valid UUID\n"
    );

    let missing = sandbox.path().join("missing.db");
    let database = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            missing.to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
        ],
    );
    assert_eq!(database.status.code(), Some(3));
    assert!(database.stdout.is_empty());
    assert_eq!(
        text(&database.stderr),
        "database_not_found: thread database was not found\n"
    );
}

#[cfg(unix)]
#[test]
fn unreadable_regular_file_has_real_binary_stream_and_exit_contract() {
    use std::os::unix::fs::PermissionsExt;

    let effective_uid = Command::new("id").arg("-u").output().unwrap();
    if text(&effective_uid.stdout).trim() == "0" {
        return;
    }

    let sandbox = TempDir::new().unwrap();
    let db = sandbox.path().join("unreadable.db");
    create_database(
        &db,
        vec![fixture_meta(SESSION_ID, "unreadable", "/unreadable")],
    );
    let original_permissions = std::fs::metadata(&db).unwrap().permissions();
    let mut unreadable_permissions = original_permissions.clone();
    unreadable_permissions.set_mode(0o000);
    std::fs::set_permissions(&db, unreadable_permissions).unwrap();
    let output = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            db.to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );
    std::fs::set_permissions(&db, original_permissions).unwrap();
    assert_json_error(
        &output,
        4,
        "database_unreadable",
        "thread database could not be opened for reading",
    );
}

#[test]
fn corrupt_session_data_is_stderr_only_and_does_not_echo_stored_value() {
    let sandbox = TempDir::new().unwrap();
    let db = sandbox.path().join("corrupt.db");
    let mut corrupt = fixture_meta(SESSION_ID, "corrupt-title", "/corrupt");
    corrupt.message_count = usize::MAX;
    create_database(&db, vec![corrupt]);

    let output = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            db.to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );

    assert_json_error(
        &output,
        4,
        "corrupt_session_data",
        "stored session metadata is corrupt",
    );
    assert!(!text(&output.stderr).contains("orrupt-title"));
    assert!(!text(&output.stderr).contains("corrupt-title"));
    assert_forbidden_output(&output);
}

#[test]
fn meta_path_does_not_read_settings_or_mutate_environment() {
    let sandbox = TempDir::new().unwrap();
    let home = sandbox.path().join("home");
    let cwd = sandbox.path().join("cwd");
    std::fs::create_dir_all(home.join(".peri")).unwrap();
    std::fs::create_dir_all(cwd.join(".peri")).unwrap();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(home.join(".peri/settings.json"), "leaked").unwrap();
    std::fs::write(cwd.join(".peri/settings.json"), "leaked").unwrap();
    std::fs::write(home.join(".claude/settings.json"), "leaked").unwrap();
    let db = sandbox.path().join("threads.db");
    create_database(
        &db,
        vec![fixture_meta(SESSION_ID, "settings-proof", "/isolated")],
    );

    let output = run_peri(
        &home,
        &cwd,
        &[
            "--db-path",
            db.to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );
    assert_success(&output);
    assert!(!text(&output.stdout).contains("leaked"));
    assert!(!text(&output.stderr).contains("leaked"));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["title"], "settings-proof");
}

#[test]
fn settings_and_environment_do_not_change_explicit_query_or_contaminate_stdout() {
    let sandbox = TempDir::new().unwrap();
    let home = sandbox.path().join("home");
    let cwd = sandbox.path().join("cwd");
    std::fs::create_dir_all(home.join(".peri")).unwrap();
    std::fs::create_dir_all(cwd.join(".peri")).unwrap();
    std::fs::write(
        home.join(".peri/settings.json"),
        r#"{"env":{"PERI_META_SENTINEL":"global-setting"}}"#,
    )
    .unwrap();
    std::fs::write(
        cwd.join(".peri/settings.json"),
        r#"{"env":{"PERI_META_SENTINEL":"workspace-setting"}}"#,
    )
    .unwrap();
    let db = sandbox.path().join("explicit.db");
    create_database(
        &db,
        vec![fixture_meta(SESSION_ID, "stable-result", "/stable")],
    );

    let mut command = peri_command(&home, &cwd);
    let output = command
        .env("PERI_META_SENTINEL", "process-setting")
        .args([
            "--db-path",
            db.to_str().unwrap(),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ])
        .output()
        .unwrap();

    assert_success(&output);
    assert_eq!(text(&output.stdout).lines().count(), 1);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["id"], SESSION_ID);
    assert_eq!(value["title"], "stable-result");
    let combined = format!("{}{}", text(&output.stdout), text(&output.stderr));
    assert!(!combined.contains("PERI_META_SENTINEL"));
    assert!(!combined.contains("process-setting"));
    assert!(!combined.contains("workspace-setting"));
    assert!(!combined.contains("global-setting"));
    assert_forbidden_output(&output);
}
