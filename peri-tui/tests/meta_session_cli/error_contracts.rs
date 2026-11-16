//! Error contracts regression scenarios.

use super::Command;
use super::OTHER_SESSION_ID;
use super::SESSION_ID;
use super::TempDir;

#[test]
fn every_error_kind_has_human_real_binary_stream_and_exit_evidence() {
    let sandbox = TempDir::new().expect("every error kind has human real binary stream and exit evidence: fixture parameters must construct a valid value");

    let grammar = super::run_peri(sandbox.path(), sandbox.path(), &["meta", "list"]);
    super::assert_human_error(
        &grammar,
        2,
        "invalid_argument",
        "invalid Meta command arguments",
    );

    let invalid = super::run_peri(
        sandbox.path(),
        sandbox.path(),
        &["meta", "session", "not-a-uuid"],
    );
    super::assert_human_error(
        &invalid,
        2,
        "invalid_session_id",
        "session ID must be a valid UUID",
    );

    let missing_path = sandbox.path().join("missing.db");
    let missing_database = super::run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            missing_path.to_str().expect("every error kind has human real binary stream and exit evidence: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
        ],
    );
    super::assert_human_diagnostic(
        &missing_database,
        3,
        "database_not_found",
        &[missing_path.to_str().expect("every error kind has human real binary stream and exit evidence: fixture paths and protocol text must be valid UTF-8"), "session database not found"],
    );

    let unreadable = super::run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            sandbox.path().to_str().expect("every error kind has human real binary stream and exit evidence: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
        ],
    );
    super::assert_human_diagnostic(
        &unreadable,
        4,
        "database_unreadable",
        &[
            sandbox.path().to_str().expect("every error kind has human real binary stream and exit evidence: fixture paths and protocol text must be valid UTF-8"),
            "session database is unreadable",
        ],
    );

    let incompatible_path = sandbox.path().join("incompatible.db");
    std::fs::write(&incompatible_path, b"not sqlite").expect("every error kind has human real binary stream and exit evidence: isolated fixture workspace must remain accessible");
    let incompatible = super::run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            incompatible_path.to_str().expect("every error kind has human real binary stream and exit evidence: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
        ],
    );
    super::assert_human_diagnostic(
        &incompatible,
        4,
        "schema_incompatible",
        &[incompatible_path.to_str().expect("every error kind has human real binary stream and exit evidence: fixture paths and protocol text must be valid UTF-8"), "schema"],
    );

    let compatible_path = sandbox.path().join("compatible.db");
    super::create_database(
        &compatible_path,
        vec![super::fixture_meta(OTHER_SESSION_ID, "other", "/other")],
    );
    let session_missing = super::run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            compatible_path.to_str().expect("every error kind has human real binary stream and exit evidence: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
        ],
    );
    super::assert_human_diagnostic(
        &session_missing,
        3,
        "session_not_found",
        &["session not found", "NotApplied"],
    );

    let corrupt_path = sandbox.path().join("corrupt-human.db");
    let mut corrupt = super::fixture_meta(SESSION_ID, "corrupt", "/corrupt");
    corrupt.message_count = usize::MAX;
    super::create_database(&corrupt_path, vec![corrupt]);
    let corrupt_output = super::run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            corrupt_path.to_str().expect("every error kind has human real binary stream and exit evidence: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
        ],
    );
    super::assert_human_diagnostic(
        &corrupt_output,
        4,
        "corrupt_session_data",
        &[
            "session data is corrupt",
            "stored session data is not readable",
        ],
    );
}

#[test]
fn human_argument_and_storage_failures_are_stderr_only() {
    let sandbox = TempDir::new().expect("human argument and storage failures are stderr only: fixture parameters must construct a valid value");
    let invalid = super::run_peri(
        sandbox.path(),
        sandbox.path(),
        &["meta", "session", "not-a-uuid"],
    );
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert_eq!(
        super::text(&invalid.stderr),
        "invalid_session_id: session ID must be a valid UUID\n"
    );

    let missing = sandbox.path().join("missing.db");
    let database = super::run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            missing.to_str().expect("human argument and storage failures are stderr only: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
        ],
    );
    assert_eq!(database.status.code(), Some(3));
    assert!(database.stdout.is_empty());
    super::assert_human_diagnostic(
        &database,
        3,
        "database_not_found",
        &[missing.to_str().expect("human argument and storage failures are stderr only: fixture paths and protocol text must be valid UTF-8"), "session database not found"],
    );
}

#[cfg(unix)]
#[test]
fn unreadable_regular_file_has_real_binary_stream_and_exit_contract() {
    use std::os::unix::fs::PermissionsExt;

    let effective_uid = Command::new("id").arg("-u").output().expect("unreadable regular file has real binary stream and exit contract: isolated fixture workspace must remain accessible");
    if super::text(&effective_uid.stdout).trim() == "0" {
        return;
    }

    let sandbox = TempDir::new().expect("unreadable regular file has real binary stream and exit contract: fixture parameters must construct a valid value");
    let db = sandbox.path().join("unreadable.db");
    super::create_database(
        &db,
        vec![super::fixture_meta(SESSION_ID, "unreadable", "/unreadable")],
    );
    let original_permissions = std::fs::metadata(&db).expect("unreadable regular file has real binary stream and exit contract: the fixture storage and layout must support the requested operation").permissions();
    let mut unreadable_permissions = original_permissions.clone();
    unreadable_permissions.set_mode(0o000);
    std::fs::set_permissions(&db, unreadable_permissions).expect("unreadable regular file has real binary stream and exit contract: the fixture storage and layout must support the requested operation");
    let output = super::run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            db.to_str().expect("unreadable regular file has real binary stream and exit contract: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );
    std::fs::set_permissions(&db, original_permissions).expect("unreadable regular file has real binary stream and exit contract: the fixture storage and layout must support the requested operation");
    super::assert_json_diagnostic(
        &output,
        4,
        "database_unreadable",
        &[db.to_str().expect("unreadable regular file has real binary stream and exit contract: fixture paths and protocol text must be valid UTF-8"), "session database is unreadable"],
    );
}

#[test]
fn corrupt_session_data_is_stderr_only_and_does_not_echo_stored_value() {
    let sandbox = TempDir::new().expect("corrupt session data is stderr only and does not echo stored value: fixture parameters must construct a valid value");
    let db = sandbox.path().join("corrupt.db");
    let mut corrupt = super::fixture_meta(SESSION_ID, "corrupt-title", "/corrupt");
    corrupt.message_count = usize::MAX;
    super::create_database(&db, vec![corrupt]);

    let output = super::run_peri(
        sandbox.path(),
        sandbox.path(),
        &[
            "--db-path",
            db.to_str().expect("corrupt session data is stderr only and does not echo stored value: fixture paths and protocol text must be valid UTF-8"),
            "meta",
            "session",
            SESSION_ID,
            "--json",
        ],
    );

    super::assert_json_diagnostic(
        &output,
        4,
        "corrupt_session_data",
        &[
            "session data is corrupt",
            "stored session data is not readable",
        ],
    );
    assert!(!super::text(&output.stderr).contains("orrupt-title"));
    assert!(!super::text(&output.stderr).contains("corrupt-title"));
    super::assert_forbidden_output(&output);
}
