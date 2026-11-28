use ratatui::text::Span;

use super::*;
use crate::test_support::{session, with_approvals, with_environment, with_metadata};

#[test]
fn status_buckets_follow_status_then_state() {
    assert_eq!(
        bucket_of("in_progress", "running"),
        StatusBucket::InProgress
    );
    assert_eq!(
        bucket_of("requires_action", "waiting"),
        StatusBucket::RequiresAction
    );
    assert_eq!(bucket_of("failed", "failed"), StatusBucket::Failed);
    assert_eq!(bucket_of("idle", "idle"), StatusBucket::Idle);
    // Unknown status falls back to the durable state.
    assert_eq!(bucket_of("", "running"), StatusBucket::InProgress);
    assert_eq!(bucket_of("", "creating"), StatusBucket::InProgress);
    assert_eq!(
        bucket_of("", "requires_action"),
        StatusBucket::RequiresAction
    );
    assert_eq!(bucket_of("", "failed"), StatusBucket::Failed);
    assert_eq!(bucket_of("", "weird"), StatusBucket::Idle);
    assert_ne!(bucket_of("idle", "idle"), StatusBucket::All);

    assert_eq!(StatusBucket::All.label(), "All");
    assert_eq!(StatusBucket::InProgress.label(), "In progress");
    assert_eq!(StatusBucket::RequiresAction.label(), "Requires action");
    assert_eq!(StatusBucket::Failed.label(), "Failed");
    assert_eq!(StatusBucket::Idle.label(), "Idle");
    assert_eq!(StatusBucket::Failed.tone(), StatusTone::Error);
    assert_eq!(StatusBucket::RequiresAction.tone(), StatusTone::Warning);
    assert_eq!(StatusBucket::InProgress.tone(), StatusTone::Ok);
    assert_eq!(StatusBucket::All.tone(), StatusTone::Muted);
}

#[test]
fn counts_cover_the_loaded_set_only() {
    let sessions = vec![
        session("a", "in_progress", "running", Some("gpt-5"), 10),
        session("b", "requires_action", "waiting", None, 10),
        session("c", "failed", "failed", None, 10),
        session("d", "idle", "idle", None, 10),
        session("e", "idle", "idle", None, 10),
    ];
    let counts = StatusCounts::of(&sessions);
    assert_eq!(counts.all, 5);
    assert_eq!(counts.get(StatusBucket::All), 5);
    assert_eq!(counts.get(StatusBucket::InProgress), 1);
    assert_eq!(counts.get(StatusBucket::RequiresAction), 1);
    assert_eq!(counts.get(StatusBucket::Failed), 1);
    assert_eq!(counts.get(StatusBucket::Idle), 2);
    assert_eq!(StatusCounts::of(&[]).all, 0);
}

#[test]
fn search_is_case_insensitive_over_id_model_metadata_and_environment() {
    let sessions = vec![
        session("sess-Abc", "idle", "idle", Some("GPT-5-mini"), 10),
        with_metadata(
            session("worker-1", "idle", "idle", None, 10),
            &[("Team", "Platform")],
        ),
        with_environment(
            session("env-holder", "idle", "idle", None, 10),
            "openai-prod",
        ),
        session("unrelated", "idle", "idle", None, 10),
    ];
    assert!(matches_search(&sessions[0], "SESS-abc"));
    assert!(matches_search(&sessions[0], "gpt-5"));
    assert!(matches_search(&sessions[1], "platform"));
    assert!(matches_search(&sessions[1], "team="));
    assert!(matches_search(&sessions[2], "openai-prod"));
    assert!(!matches_search(&sessions[3], "gpt-5"));
    assert!(matches_search(&sessions[3], "   "));

    let found = filtered_indices(&sessions, StatusBucket::All, "idle")
        .into_iter()
        .map(|index| sessions[index].id.as_str())
        .collect::<Vec<_>>();
    // Only the status-independent text matches: "idle" is not a search field.
    assert!(found.is_empty());

    let filtered = filtered_indices(&sessions, StatusBucket::All, "sess");
    assert_eq!(filtered, vec![0]);
    let bucket_filtered = filtered_indices(&sessions, StatusBucket::Idle, "env");
    assert_eq!(bucket_filtered, vec![2]);
}

#[test]
fn relative_time_covers_every_unit_and_missing_values() {
    assert_eq!(relative_time(0, 1_000), "—");
    assert_eq!(relative_time(-5, 1_000), "—");
    assert_eq!(relative_time(1_000, 1_000), "just now");
    assert_eq!(relative_time(1_000, 1_001), "just now");
    assert_eq!(relative_time(1_000, 1_042), "42s ago");
    assert_eq!(relative_time(1_000, 1_000 + 3 * 60), "3m ago");
    assert_eq!(relative_time(1_000, 1_000 + 5 * 3_600), "5h ago");
    assert_eq!(relative_time(1_000, 1_000 + 3 * 86_400), "3d ago");
    // A clock skew that makes the timestamp look like the future is safe.
    assert_eq!(relative_time(2_000, 1_000), "just now");
}

#[test]
fn session_cell_keeps_both_parts_within_the_column() {
    let wide = session("session-0123456789", "idle", "idle", Some("gpt-5-mini"), 10);
    let (id, model) = session_cell(&wide, 40);
    assert_eq!(id, "session-0123456789");
    assert_eq!(model, " gpt-5-mini");
    assert!(Span::raw(format!("{id}{model}")).width() <= 40);

    let (id, model) = session_cell(&wide, 24);
    assert!(!model.is_empty());
    assert!(Span::raw(format!("{id}{model}")).width() <= 24);

    let (id, model) = session_cell(&wide, 12);
    assert!(!id.is_empty());
    assert!(
        model.is_empty(),
        "the model is dropped before the id shrinks"
    );
    assert!(Span::raw(&id).width() <= 12);

    let (id, model) = session_cell(&wide, 0);
    assert!(id.is_empty() && model.is_empty());

    let no_model = session("plain", "idle", "idle", None, 10);
    let (id, model) = session_cell(&no_model, 20);
    assert_eq!((id.as_str(), model.as_str()), ("plain", ""));

    // A truncated id keeps its tail, so ids sharing a timestamp prefix differ.
    let first = session("lt-20261128-003321", "idle", "idle", None, 10);
    let second = session("lt-20261128-003322", "idle", "idle", None, 10);
    let (a, _) = session_cell(&first, 12);
    let (b, _) = session_cell(&second, 12);
    assert_ne!(a, b, "{a} vs {b}");
    assert!(a.ends_with("21"), "{a}");
    assert!(Span::raw(a.as_str()).width() <= 12);

    assert_eq!(fit_column("abcdefgh", 4), "abc…");
    assert_eq!(fit_column("ab", 4), "ab");
    assert_eq!(fit_column("ab", 0), "");
}

#[test]
fn identity_middle_truncation_keeps_both_ends() {
    assert_eq!(middle_truncate("abcdefghij", 6), "abc…ij");
    assert_eq!(middle_truncate("abc", 9), "abc");
    assert_eq!(middle_truncate("abc", 0), "");
    assert_eq!(middle_truncate("abc", 1), "…");
    assert_eq!(middle_truncate("abcdef", 3), "a…f");
    // Wide characters are never split.
    assert_eq!(middle_truncate("会话标识符", 5), "会…符");
}

#[test]
fn metadata_and_approvals_summaries_are_compact() {
    let empty = session("a", "idle", "idle", None, 10);
    assert_eq!(metadata_summary(&empty), "—");
    assert_eq!(approvals_summary(approvals_count(&empty)), "—");

    let rich = with_metadata(
        session("b", "idle", "idle", None, 10),
        &[("alpha", "1"), ("beta", "2"), ("gamma", "3")],
    );
    assert_eq!(metadata_summary(&rich), "alpha=1, beta=2, +1");

    let two = with_approvals(rich.clone(), 2);
    assert_eq!(approvals_count(&two), 2);
    assert_eq!(approvals_summary(2), "2 pending");
    assert_eq!(approvals_summary(1), "1 pending");

    let mut mixed = two;
    mixed
        .required_actions
        .push(serde_json::json!({ "type": "other" }));
    assert_eq!(approvals_count(&mixed), 2, "only MCP approvals count");
}

#[test]
fn columns_follow_the_shell_breakpoints() {
    assert_eq!(columns(140, 109), Column::ALL.to_vec());
    assert_eq!(columns(120, 93), Column::ALL.to_vec());
    assert_eq!(
        columns(119, 94),
        vec![
            Column::Session,
            Column::Status,
            Column::Approvals,
            Column::LastActive
        ]
    );
    assert_eq!(
        columns(100, 71),
        vec![
            Column::Session,
            Column::Status,
            Column::Approvals,
            Column::LastActive
        ]
    );
    assert_eq!(
        columns(80, 55),
        vec![
            Column::Session,
            Column::Status,
            Column::Metadata,
            Column::LastActive
        ]
    );
    // The narrow tier drops approvals only when the pane cannot fit it.
    assert_eq!(
        columns(80, 30),
        vec![Column::Session, Column::Status, Column::LastActive]
    );
    assert_eq!(
        columns(60, 58),
        vec![Column::Session, Column::Status, Column::LastActive]
    );

    for (terminal, pane) in [(140, 109), (120, 93), (100, 71), (90, 65), (80, 55)] {
        let plan = columns(terminal, pane);
        let widths = column_widths(&plan, pane);
        let gaps = u16::try_from(plan.len() - 1).expect("gaps");
        let total: u16 = widths.iter().sum::<u16>() + gaps;
        assert!(
            total <= pane,
            "columns must fit: {total} > {pane} at {terminal}"
        );
        assert!(widths.iter().all(|width| *width > 0));
        if let Some(index) = plan.iter().position(|column| *column == Column::Metadata) {
            assert!(
                widths[index] >= 10,
                "metadata column must stay readable at {terminal}"
            );
        }
    }

    assert_eq!(Column::Session.header(), "Session");
    assert_eq!(Column::Metadata.header(), "Metadata");
}
