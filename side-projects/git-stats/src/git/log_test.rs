use super::*;

#[test]
fn test_parse_record_keeps_markers_empty_body_and_binary_numstat_separate() {
    let raw = b"\0hash1\0Alice\0alice@example.test\0feat: @@COMMIT@@\0@@NUMSTAT@@\nCo-Authored-By: Pair <pair@example.test>\n\0\n10\t5\t@@NUMSTAT@@\nfile\0-\t-\timage.bin\0\0hash2\0Bob\0bob@example.test\0chore: empty\0\0";
    let commits = parse_log_output(raw).unwrap();
    assert_eq!(commits.len(), 2, "消息和路径 marker 不能生成伪提交");
    assert_eq!(commits[0].subject, "feat: @@COMMIT@@");
    assert_eq!(commits[0].co_authors[0].name, "Pair");
    assert_eq!(commits[0].files.len(), 2);
    assert_eq!(
        (commits[0].files[0].added, commits[0].files[0].deleted),
        (10, 5)
    );
    assert_eq!(
        (commits[0].files[1].added, commits[0].files[1].deleted),
        (0, 0)
    );
    assert!(commits[1].files.is_empty(), "空提交不能借用前一条 numstat");
}

#[test]
fn test_truncated_record_is_reported_instead_of_silently_dropped() {
    let error =
        parse_log_output(b"\0hash\0Alice\0alice@example.test\0subject\0body without terminator")
            .unwrap_err();
    assert_eq!(error, "Unterminated body field");
}

#[test]
fn test_invalid_numstat_is_not_coerced_to_zero_lines() {
    let error =
        parse_log_output(b"\0hash\0Alice\0alice@example.test\0subject\0\0\ninvalid\t2\tfile\0")
            .unwrap_err();
    assert_eq!(error, "Invalid numstat line count");
}

#[test]
fn test_empty_log_has_no_commits() {
    assert!(parse_log_output(b"").unwrap().is_empty());
}
