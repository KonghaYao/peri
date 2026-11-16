use std::path::Path;
use std::process::{Command, Output};

use chrono::{TimeZone, Utc};
use peri_acp_types::store::ThreadStore;
use peri_acp_types::thread::{AgentStatus, ThreadMeta};
use peri_resources::sessions::SqliteThreadStore;
// 只被 `default_database_*` 两条 unix 用例使用（见下方平台说明）。
#[cfg(not(windows))]
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
    peri_command(home, cwd)
        .args(args)
        .output()
        .expect("run peri: isolated fixture workspace must remain accessible")
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("text: fixture paths and protocol text must be valid UTF-8")
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
    let value: serde_json::Value = serde_json::from_slice(&output.stderr)
        .expect("assert json error: fixture JSON must match the expected schema");
    assert_eq!(
        value
            .as_object()
            .expect("assert json error: the fixture must retain its expected variant")
            .len(),
        2
    );
}

fn assert_human_error(output: &Output, exit_code: i32, kind: &str, message: &str) {
    assert_eq!(output.status.code(), Some(exit_code));
    assert!(output.stdout.is_empty(), "stdout: {}", text(&output.stdout));
    assert_eq!(text(&output.stderr), format!("{kind}: {message}\n"));
}

fn assert_json_diagnostic(output: &Output, exit_code: i32, kind: &str, fragments: &[&str]) {
    assert_eq!(output.status.code(), Some(exit_code));
    assert!(output.stdout.is_empty(), "stdout: {}", text(&output.stdout));
    let stderr = text(&output.stderr);
    assert!(stderr.ends_with('\n'));
    assert_eq!(stderr.lines().count(), 1);
    let value: serde_json::Value = serde_json::from_slice(&output.stderr)
        .expect("assert json diagnostic: fixture JSON must match the expected schema");
    assert_eq!(
        value
            .as_object()
            .expect("assert json diagnostic: the fixture must retain its expected variant")
            .len(),
        2
    );
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(
        value["error"]
            .as_object()
            .expect("assert json diagnostic: the fixture must retain its expected variant")
            .len(),
        2
    );
    assert_eq!(value["error"]["kind"], kind);
    let message = value["error"]["message"]
        .as_str()
        .expect("assert json diagnostic: fixture must have the expected variant");
    for fragment in fragments {
        assert!(
            message.contains(fragment),
            "missing {fragment:?} in {message:?}"
        );
    }
}

fn assert_human_diagnostic(output: &Output, exit_code: i32, kind: &str, fragments: &[&str]) {
    assert_eq!(output.status.code(), Some(exit_code));
    assert!(output.stdout.is_empty(), "stdout: {}", text(&output.stdout));
    let stderr = text(&output.stderr);
    assert!(stderr.ends_with('\n'));
    assert_eq!(stderr.lines().count(), 1);
    let message = stderr
        .strip_prefix(&format!("{kind}: "))
        .unwrap_or_else(|| panic!("unexpected diagnostic: {stderr:?}"));
    for fragment in fragments {
        assert!(
            message.contains(fragment),
            "missing {fragment:?} in {message:?}"
        );
    }
}

fn fixture_cwd(relative: &str) -> String {
    std::env::temp_dir()
        .join(relative.trim_start_matches('/'))
        .to_string_lossy()
        .into_owned()
}

fn fixture_meta(id: &str, title: &str, cwd: &str) -> ThreadMeta {
    ThreadMeta {
        id: id.to_owned(),
        title: Some(title.to_owned()),
        cwd: fixture_cwd(cwd),
        created_at: Utc
            .with_ymd_and_hms(2026, 9, 4, 1, 2, 3)
            .single()
            .expect("fixture meta: the fixed fixture date and time must be valid"),
        updated_at: Utc
            .with_ymd_and_hms(2026, 9, 4, 4, 5, 6)
            .single()
            .expect("fixture meta: the fixed fixture date and time must be valid"),
        message_count: 7,
        content_size: 99,
        parent_thread_id: None,
        snapshot_at_message_id: Some(FORBIDDEN_SNAPSHOT.to_owned()),
        hidden: true,
        cancel_policy: Default::default(),
        config: Some(FORBIDDEN_CONFIG.to_owned()),
        agent_status: AgentStatus::Done,
        work_state: Default::default(),
        context_usage_tokens: None,
    }
}

fn create_database(path: &Path, metas: Vec<ThreadMeta>) {
    let runtime = tokio::runtime::Runtime::new()
        .expect("create database: fixture parameters must construct a valid value");
    runtime.block_on(async {
        let store = SqliteThreadStore::try_new(path)
            .await
            .expect("create database: fixture task and request must complete successfully");
        for meta in metas {
            store
                .create_thread(meta)
                .await
                .expect("create database: fixture task and request must complete successfully");
        }
        // 必须在子进程读取前确定性收尾：仅依赖 Drop 会让最后一次连接关闭触发的
        // WAL checkpoint 与 `-wal`/`-shm` 清理异步落在只读打开窗口内，使子进程
        // 撞上 mmap 独占锁，在负载高的机器上超出 busy 上限。
        store.close().await;
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
    let sandbox = TempDir::new().expect("human success is stdout only and escapes persisted controls: fixture parameters must construct a valid value");
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
            db.to_str().expect("human success is stdout only and escapes persisted controls: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
        ],
    );

    assert_success(&output);
    let cwd = fixture_cwd("/tmp/project\rnext");
    let escaped_cwd = cwd.escape_debug();
    assert_eq!(
        text(&output.stdout),
        format!(
            "Schema version: 1\nID: 550e8400-e29b-41d4-a716-446655440000\nTitle: safe\\n\\t\\u{{1b}}[31m\nCWD: {escaped_cwd}\nCreated at: 2026-09-04T01:02:03+00:00\nUpdated at: 2026-09-04T04:05:06+00:00\nMessage count: 7\nParent thread ID: null\nPersisted agent status: done\n"
        )
    );
    assert_forbidden_output(&output);
}

#[test]
fn json_success_is_one_exact_allowlisted_object() {
    let sandbox = TempDir::new().expect("json success is one exact allowlisted object: fixture parameters must construct a valid value");
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
            db.to_str().expect("json success is one exact allowlisted object: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );

    assert_success(&output);
    assert_eq!(text(&output.stdout).lines().count(), 1);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect(
        "json success is one exact allowlisted object: fixture JSON must match the expected schema",
    );
    assert_eq!(
        value,
        serde_json::json!({
            "schemaVersion": 1,
            "id": SESSION_ID,
            "title": "database-a",
            "cwd": fixture_cwd("/project/a"),
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
    let sandbox = TempDir::new().expect("legal non v7 uuid preserves original text lookup: fixture parameters must construct a valid value");
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
            db.to_str().expect("legal non v7 uuid preserves original text lookup: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            &uppercase_id,
            "--json",
        ],
    );
    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("legal non v7 uuid preserves original text lookup: fixture JSON must match the expected schema");
    assert_eq!(value["id"], uppercase_id);
    assert_eq!(value["title"], "uppercase");
}

#[test]
fn invalid_id_wins_over_missing_database_without_creating_it() {
    let sandbox = TempDir::new().expect("invalid id wins over missing database without creating it: fixture parameters must construct a valid value");
    let db = sandbox.path().join("missing-parent").join("threads.db");

    let output = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            db.to_str().expect("invalid id wins over missing database without creating it: fixture paths and protocol text must be valid UTF-8"),
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
    assert!(!db.parent().expect("invalid id wins over missing database without creating it: the fixture path must have its expected parent directory").exists());
    assert_forbidden_output(&output);
}

#[test]
fn explicit_database_selection_never_falls_back_to_another_database() {
    let sandbox = TempDir::new().expect("explicit database selection never falls back to another database: fixture parameters must construct a valid value");
    let db_a = sandbox.path().join("a.db");
    let db_b = sandbox.path().join("b.db");
    create_database(&db_a, vec![fixture_meta(OTHER_SESSION_ID, "only-a", "/a")]);
    create_database(&db_b, vec![fixture_meta(SESSION_ID, "only-b", "/b")]);

    let missing = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            db_a.to_str().expect("explicit database selection never falls back to another database: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );
    assert_json_diagnostic(
        &missing,
        3,
        "session_not_found",
        &["session not found", "NotApplied"],
    );

    let selected = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            db_b.to_str().expect("explicit database selection never falls back to another database: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );
    assert_success(&selected);
    let value: serde_json::Value = serde_json::from_slice(&selected.stdout).expect("explicit database selection never falls back to another database: fixture JSON must match the expected schema");
    assert_eq!(value["title"], "only-b");
    assert_eq!(value["cwd"], fixture_cwd("/b"));
}

// 默认库位置由 `dirs::home_dir()` 派生，而 Windows 上它读 Profile known-folder、
// 忽略 HOME（同 peri-resources/src/sessions/mod.rs 的默认路径用例）：注入 HOME 无法把
// 默认库指向沙箱，这条用例在 Windows 上验证不了自己的契约，只在 unix 运行。
#[cfg(not(windows))]
#[test]
#[serial]
fn default_database_reads_existing_single_store_without_modifying_history() {
    let sandbox = TempDir::new().expect("default database reads existing single store without modifying history: fixture parameters must construct a valid value");
    let home = sandbox.path().join("home");
    let cwd = sandbox.path().join("cwd");
    std::fs::create_dir_all(&cwd).expect("default database reads existing single store without modifying history: isolated fixture workspace must remain accessible");
    let db = home.join(".peri/threads/threads.db");
    create_database(&db, vec![fixture_meta(SESSION_ID, "old-history", "/old")]);
    let before = std::fs::read(&db).expect("default database reads existing single store without modifying history: isolated fixture workspace must remain accessible");

    let output = run_peri(&home, &cwd, &["meta", "session", SESSION_ID, "--json"]);

    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("default database reads existing single store without modifying history: fixture JSON must match the expected schema");
    assert_eq!(value["title"], "old-history");
    assert_eq!(value["cwd"], fixture_cwd("/old"));
    assert_eq!(std::fs::read(&db).expect("default database reads existing single store without modifying history: the fixture storage and layout must support the requested operation"), before);
    assert!(!home.join(".peri/threads/threads-v2.db").exists());
    assert_forbidden_output(&output);
}

#[test]
fn explicit_database_selection_returns_each_selected_database_value() {
    let sandbox = TempDir::new().expect("explicit database selection returns each selected database value: fixture parameters must construct a valid value");
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
                db.to_str().expect("explicit database selection returns each selected database value: fixture paths and protocol text must be valid UTF-8"),
                "meta",
                "session",
                SESSION_ID,
                "--json",
            ],
        );
        assert_success(&output);
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("explicit database selection returns each selected database value: fixture JSON must match the expected schema");
        assert_eq!(value["title"], expected_title);
    }
}

// 同 `default_database_reads_existing_single_store_without_modifying_history`：注入 HOME
// 在 Windows 无效，这里观察的沙箱主目录根本不会被默认路径解析读到。
#[cfg(not(windows))]
#[test]
#[serial]
fn missing_default_database_does_not_create_home_paths() {
    let sandbox = TempDir::new().expect("missing default database does not create home paths: fixture parameters must construct a valid value");
    let home = sandbox.path().join("unused-home");
    let cwd = sandbox.path().join("cwd");
    std::fs::create_dir_all(&cwd).expect("missing default database does not create home paths: isolated fixture workspace must remain accessible");

    let output = run_peri(&home, &cwd, &["meta", "session", SESSION_ID, "--json"]);

    assert_json_diagnostic(
        &output,
        3,
        "database_not_found",
        &["~/.peri/threads/threads.db", "session database not found"],
    );
    assert!(!home.exists());
}

#[test]
fn missing_database_and_incompatible_schema_have_stable_process_contracts() {
    let sandbox = TempDir::new().expect("missing database and incompatible schema have stable process contracts: fixture parameters must construct a valid value");
    let missing = sandbox.path().join("missing.db");
    let missing_output = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            missing.to_str().expect("missing database and incompatible schema have stable process contracts: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );
    assert_json_diagnostic(
        &missing_output,
        3,
        "database_not_found",
        &[missing.to_str().expect("missing database and incompatible schema have stable process contracts: fixture paths and protocol text must be valid UTF-8"), "session database not found"],
    );
    assert!(!missing.exists());

    let incompatible = sandbox.path().join("incompatible.db");
    std::fs::write(&incompatible, b"not a sqlite database").expect("missing database and incompatible schema have stable process contracts: isolated fixture workspace must remain accessible");
    let incompatible_output = run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            incompatible.to_str().expect("missing database and incompatible schema have stable process contracts: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );
    assert_json_diagnostic(
        &incompatible_output,
        4,
        "schema_incompatible",
        &[incompatible.to_str().expect("missing database and incompatible schema have stable process contracts: fixture paths and protocol text must be valid UTF-8"), "schema"],
    );
    assert_forbidden_output(&incompatible_output);
}

#[test]
fn ordinary_meta_values_preserve_real_process_clap_behavior() {
    let sandbox = TempDir::new().expect("ordinary meta values preserve real process clap behavior: fixture parameters must construct a valid value");
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
    let sandbox = TempDir::new().expect("json grammar failures are single stable process errors: fixture parameters must construct a valid value");
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
            "--bare",
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
    let sandbox = TempDir::new().expect("malformed prefix meta shape has stable real process errors: fixture parameters must construct a valid value");
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
    let sandbox = TempDir::new().expect("every unrelated top level option is rejected by real process: fixture parameters must construct a valid value");
    let cases: &[&[&str]] = &[
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
fn meta_path_does_not_read_settings_or_mutate_environment() {
    let sandbox = TempDir::new().expect("meta path does not read settings or mutate environment: fixture parameters must construct a valid value");
    let home = sandbox.path().join("home");
    let cwd = sandbox.path().join("cwd");
    std::fs::create_dir_all(home.join(".peri")).expect("meta path does not read settings or mutate environment: isolated fixture workspace must remain accessible");
    std::fs::create_dir_all(cwd.join(".peri")).expect("meta path does not read settings or mutate environment: isolated fixture workspace must remain accessible");
    std::fs::create_dir_all(home.join(".claude")).expect("meta path does not read settings or mutate environment: isolated fixture workspace must remain accessible");
    std::fs::write(home.join(".peri/settings.json"), "leaked").expect("meta path does not read settings or mutate environment: isolated fixture workspace must remain accessible");
    std::fs::write(cwd.join(".peri/settings.json"), "leaked").expect("meta path does not read settings or mutate environment: isolated fixture workspace must remain accessible");
    std::fs::write(home.join(".claude/settings.json"), "leaked").expect("meta path does not read settings or mutate environment: isolated fixture workspace must remain accessible");
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
            db.to_str().expect("meta path does not read settings or mutate environment: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );
    assert_success(&output);
    assert!(!text(&output.stdout).contains("leaked"));
    assert!(!text(&output.stderr).contains("leaked"));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("meta path does not read settings or mutate environment: fixture JSON must match the expected schema");
    assert_eq!(value["title"], "settings-proof");
}

#[test]
fn settings_and_environment_do_not_change_explicit_query_or_contaminate_stdout() {
    let sandbox = TempDir::new().expect("settings and environment do not change explicit query or contaminate stdout: fixture parameters must construct a valid value");
    let home = sandbox.path().join("home");
    let cwd = sandbox.path().join("cwd");
    std::fs::create_dir_all(home.join(".peri")).expect("settings and environment do not change explicit query or contaminate stdout: isolated fixture workspace must remain accessible");
    std::fs::create_dir_all(cwd.join(".peri")).expect("settings and environment do not change explicit query or contaminate stdout: isolated fixture workspace must remain accessible");
    std::fs::write(
        home.join(".peri/settings.json"),
        r#"{"env":{"PERI_META_SENTINEL":"global-setting"}}"#,
    )
    .expect("settings and environment do not change explicit query or contaminate stdout: isolated fixture workspace must remain accessible");
    std::fs::write(
        cwd.join(".peri/settings.json"),
        r#"{"env":{"PERI_META_SENTINEL":"workspace-setting"}}"#,
    )
    .expect("settings and environment do not change explicit query or contaminate stdout: isolated fixture workspace must remain accessible");
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
            db.to_str().expect("settings and environment do not change explicit query or contaminate stdout: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ])
        .output()
        .expect("settings and environment do not change explicit query or contaminate stdout: isolated fixture workspace must remain accessible");

    assert_success(&output);
    assert_eq!(text(&output.stdout).lines().count(), 1);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("settings and environment do not change explicit query or contaminate stdout: fixture JSON must match the expected schema");
    assert_eq!(value["id"], SESSION_ID);
    assert_eq!(value["title"], "stable-result");
    let combined = format!("{}{}", text(&output.stdout), text(&output.stderr));
    assert!(!combined.contains("PERI_META_SENTINEL"));
    assert!(!combined.contains("process-setting"));
    assert!(!combined.contains("workspace-setting"));
    assert!(!combined.contains("global-setting"));
    assert_forbidden_output(&output);
}

#[path = "meta_session_cli/error_contracts.rs"]
mod error_contracts;
