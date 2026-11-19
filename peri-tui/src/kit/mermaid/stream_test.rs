use super::DiagramMode;
use super::MermaidStream;
use super::StreamUpdate;
use super::detect_mode;

use crate::kit::mermaid::frame::render_source;

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    mode: DiagramMode,
    trusted: String,
    trusted_end: usize,
    statements: usize,
    drawable: bool,
    paused: Option<String>,
    open_scope: Option<(String, usize)>,
    final_issue: Option<String>,
}

fn snapshot(update: &StreamUpdate<'_>) -> Snapshot {
    Snapshot {
        mode: update.mode,
        trusted: update.trusted.to_string(),
        trusted_end: update.trusted_end,
        statements: update.statements,
        drawable: update.drawable,
        paused: update.paused.map(str::to_string),
        open_scope: update
            .open_scope
            .map(|scope| (scope.label.to_string(), scope.statements)),
        final_issue: update.final_issue.map(str::to_string),
    }
}

fn replay_bytes(text: &str, chunk: usize) -> Snapshot {
    let mut stream = MermaidStream::new();
    let mut buffer = Vec::new();
    let mut last = None;
    for piece in text.as_bytes().chunks(chunk) {
        buffer.extend_from_slice(piece);
        if let Ok(prefix) = std::str::from_utf8(&buffer) {
            last = Some(snapshot(&stream.observe(prefix, false)));
        }
    }
    if let Ok(prefix) = std::str::from_utf8(&buffer) {
        last = Some(snapshot(&stream.observe(prefix, false)));
    }
    last.expect("at least one chunk was observed")
}

#[test]
fn flowchart_commits_only_complete_statements() {
    let mut stream = MermaidStream::new();
    let first = stream.observe("graph TD\n 用户--", false);
    assert_eq!(first.mode, DiagramMode::Flowchart);
    assert_eq!(first.trusted, "graph TD\n");
    assert_eq!(first.trusted_end, "graph TD\n".len());
    assert!(!first.drawable);

    let second = stream.observe("graph TD\n 用户-->系统\n 系统-->", false);
    assert!(second.trusted.contains("用户-->系统"));
    assert!(!second.trusted.contains("系统-->"));
    assert!(second.trusted_end <= "graph TD\n 用户-->系统\n".len());
    assert!(second.drawable);
}

#[test]
fn utf8_cuts_at_one_two_three_seven_sixty_four_and_4096_bytes_agree() {
    let body = "graph TD\n  用户-->系统\n  系统-->用户\n  用户 -- 发送;请求 --> 系统\n";
    let expected = "graph TD\n用户-->系统\n系统-->用户\n用户 -- 发送;请求 --> 系统\n";
    for chunk in [1, 2, 3, 7, 64, 4096] {
        let observed = replay_bytes(body, chunk);
        assert_eq!(observed.mode, DiagramMode::Flowchart, "chunk {chunk}");
        assert_eq!(observed.trusted, expected, "chunk {chunk}");
        assert!(
            observed.paused.is_none(),
            "chunk {chunk}: {:?}",
            observed.paused
        );
        assert_eq!(observed.statements, 4, "chunk {chunk}");
        assert!(observed.drawable, "chunk {chunk}");
    }
}

#[test]
fn arrow_split_between_chunks_is_not_committed_early() {
    let mut stream = MermaidStream::new();
    assert_eq!(
        stream.observe("graph TD\nA --", false).trusted,
        "graph TD\n"
    );
    assert_eq!(
        stream.observe("graph TD\nA --> B", false).trusted,
        "graph TD\n"
    );
    let completed = stream.observe("graph TD\nA --> B\n", false);
    assert_eq!(completed.trusted, "graph TD\nA --> B\n");
}

#[test]
fn semicolons_inside_flowchart_labels_do_not_split_statements() {
    let body = "graph TD\nA[\"x;y\"]-->B\nC -- p;q --> D\nE -->|r;s| F\n";
    let mut stream = MermaidStream::new();
    let update = stream.observe(body, false);
    assert!(
        update.paused.is_none(),
        "unexpected pause: {:?}",
        update.paused
    );
    assert_eq!(
        update.trusted,
        "graph TD\nA[\"x;y\"]-->B\nC -- p;q --> D\nE -->|r;s| F\n"
    );
    assert_eq!(update.statements, 4);
    assert_eq!(update.trusted.matches("x;y").count(), 1);
    assert_eq!(update.trusted.matches("r;s").count(), 1);
}

#[test]
fn nested_subgraphs_commit_only_after_the_outer_end() {
    let mut stream = MermaidStream::new();
    let opening = stream.observe(
        "graph TD\nA-->B\nsubgraph S[Group]\nC-->D\nsubgraph N\nE-->F\n",
        false,
    );
    assert_eq!(opening.trusted, "graph TD\nA-->B\n");
    assert_eq!(
        opening
            .open_scope
            .map(|scope| (scope.label.to_string(), scope.statements)),
        Some(("N".to_string(), 1))
    );

    let closed = stream.observe(
        "graph TD\nA-->B\nsubgraph S[Group]\nC-->D\nsubgraph N\nE-->F\nend\nend\nG-->H\n",
        false,
    );
    assert_eq!(closed.open_scope, None);
    assert_eq!(
        closed.trusted,
        "graph TD\nA-->B\nsubgraph S[Group]\nC-->D\nsubgraph N\nE-->F\nend\nend\nG-->H\n"
    );
    assert!(closed.drawable);
}

#[test]
fn incomplete_sequence_message_never_becomes_a_participant() {
    let mut stream = MermaidStream::new();
    let partial = stream.observe(
        "sequenceDiagram\nparticipant 用户\nparticipant 系统\n用户->>",
        false,
    );
    assert_eq!(
        partial.trusted,
        "sequenceDiagram\nparticipant 用户\nparticipant 系统\n"
    );
    assert!(partial.open_scope.is_none());
    assert!(partial.drawable);
    let participants_trusted = partial.trusted.to_string();

    let completed = stream.observe(
        "sequenceDiagram\nparticipant 用户\nparticipant 系统\n用户->>系统: 发送请求\n",
        false,
    );
    assert_eq!(
        completed.trusted,
        "sequenceDiagram\nparticipant 用户\nparticipant 系统\n用户->>系统: 发送请求\n"
    );
    assert!(!completed.trusted.contains("> 系统"));

    let participants_only = render_source(&participants_trusted, 60)
        .expect("participant-only prefix renders")
        .lines()
        .join("\n");
    assert!(
        !participants_only.contains('>'),
        "incomplete arrow must not become a participant: {participants_only}"
    );
}

#[test]
fn sequence_semicolon_belongs_to_the_message() {
    let mut stream = MermaidStream::new();
    let update = stream.observe("sequenceDiagram\nA->>B: x;y\n", false);
    assert!(update.paused.is_none());
    assert_eq!(update.trusted, "sequenceDiagram\nA->>B: x;y\n");
    assert_eq!(update.statements, 2);
}

#[test]
fn sequence_alt_else_end_commits_after_the_outer_end() {
    let mut stream = MermaidStream::new();
    let opening = stream.observe(
        "sequenceDiagram\nparticipant A\nparticipant B\nalt ok\nA->>B: hi\nelse no\nA->>B: ho\n",
        false,
    );
    assert_eq!(
        opening.trusted,
        "sequenceDiagram\nparticipant A\nparticipant B\n"
    );
    assert_eq!(
        opening
            .open_scope
            .map(|scope| (scope.label.to_string(), scope.statements)),
        Some(("ok".to_string(), 2))
    );

    let closed = stream.observe(
        "sequenceDiagram\nparticipant A\nparticipant B\nalt ok\nA->>B: hi\nelse no\nA->>B: ho\nend\n",
        false,
    );
    assert_eq!(closed.open_scope, None);
    assert_eq!(
        closed.trusted,
        "sequenceDiagram\nparticipant A\nparticipant B\nalt ok\nA->>B: hi\nelse no\nA->>B: ho\nend\n"
    );
}

#[test]
fn mismatched_sequence_continuation_pauses() {
    let mut stream = MermaidStream::new();
    let update = stream.observe("sequenceDiagram\nloop x\nA->>B: hi\nelse no\nend\n", false);
    let paused = update.paused.expect("mismatched else must pause");
    assert!(
        paused.contains("unrecognized"),
        "unexpected pause: {paused}"
    );
    assert_eq!(update.trusted, "sequenceDiagram\n");
}

#[test]
fn unknown_complete_statement_pauses_and_keeps_the_tail() {
    let source = "graph TD\nA-->B\nthis is not valid\nC-->D\n";
    let mut stream = MermaidStream::new();
    let update = stream.observe(source, false);
    assert_eq!(update.trusted, "graph TD\nA-->B\n");
    assert!(update.paused.is_some());
    assert_eq!(update.trusted_end, "graph TD\nA-->B\n".len());

    let later = stream.observe(&format!("{source}E-->F\n"), false);
    assert_eq!(later.trusted, "graph TD\nA-->B\n");
    assert!(later.paused.is_some());
}

#[test]
fn terminal_unclosed_scope_is_reported() {
    let mut stream = MermaidStream::new();
    let update = stream.observe("graph TD\nsubgraph S\nA-->B\n", true);
    assert_eq!(update.trusted, "graph TD\n");
    assert!(
        update
            .final_issue
            .is_some_and(|reason| reason.contains("unclosed subgraph")),
        "unexpected terminal issue: {:?}",
        update.final_issue
    );
}

#[test]
fn terminal_commits_a_final_statement_without_newline() {
    let mut stream = MermaidStream::new();
    let update = stream.observe("graph TD\nA-->B", true);
    assert_eq!(update.trusted, "graph TD\nA-->B\n");
    assert!(update.final_issue.is_none());
}

#[test]
fn source_replacement_resets_the_incremental_state() {
    let mut stream = MermaidStream::new();
    let first = stream.observe("graph TD\nA-->B\n", false);
    assert_eq!(first.trusted, "graph TD\nA-->B\n");
    let replaced = stream.observe("graph LR\nC-->D\n", false);
    assert_eq!(replaced.trusted, "graph LR\nC-->D\n");
}

#[test]
fn mode_detection_requires_a_complete_header_line() {
    assert_eq!(detect_mode("graph TD", false), DiagramMode::Unknown);
    assert_eq!(detect_mode("graph TD\n", false), DiagramMode::Flowchart);
    assert_eq!(detect_mode("flowchart LR\n", false), DiagramMode::Flowchart);
    assert_eq!(detect_mode("sequ", false), DiagramMode::Unknown);
    assert_eq!(
        detect_mode("sequenceDiagram\n", false),
        DiagramMode::Sequence
    );
    assert_eq!(detect_mode("stateDiagram-v2\n", false), DiagramMode::State);
    assert_eq!(detect_mode("classDiagram\n", false), DiagramMode::Class);
    assert_eq!(detect_mode("erDiagram\n", false), DiagramMode::Er);
    assert_eq!(detect_mode("block-beta\n", false), DiagramMode::SourceOnly);
    assert_eq!(detect_mode("pie\n", false), DiagramMode::SourceOnly);
}

#[test]
fn sequence_box_after_messages_pauses() {
    let mut stream = MermaidStream::new();
    let update = stream.observe(
        "sequenceDiagram\nA->>B: hi\nbox \"group\"\nparticipant C\nend\n",
        false,
    );
    assert!(update.paused.is_some());
    assert_eq!(update.trusted, "sequenceDiagram\nA->>B: hi\n");
}

#[test]
fn plain_dotted_and_thick_edges_are_recognized() {
    let body = "graph TD\nA -.- B\nA -.-> C\nA === D\nA ~~~ E\n";
    let mut stream = MermaidStream::new();
    let update = stream.observe(body, false);
    assert!(
        update.paused.is_none(),
        "unexpected pause: {:?}",
        update.paused
    );
    assert_eq!(
        update.trusted,
        "graph TD\nA -.- B\nA -.-> C\nA === D\nA ~~~ E\n"
    );
    assert_eq!(update.statements, 5);
}

#[test]
fn sequence_title_is_not_committed_because_the_renderer_cannot_consume_it() {
    let mut stream = MermaidStream::new();
    let update = stream.observe("sequenceDiagram\nA->>B: hi\ntitle Flow\n", false);
    assert!(update.paused.is_some());
    assert_eq!(update.trusted, "sequenceDiagram\nA->>B: hi\n");
}

#[test]
fn messages_inside_a_participant_box_pause() {
    let mut stream = MermaidStream::new();
    let update = stream.observe("sequenceDiagram\nbox \"group\"\nA->>B: hi\nend\n", false);
    assert!(update.paused.is_some());
    assert_eq!(update.trusted, "sequenceDiagram\n");
}

#[test]
fn a_single_dash_stays_part_of_the_node_id() {
    let mut stream = MermaidStream::new();
    let update = stream.observe("graph TD\nA-1 --> B\n", false);
    assert!(
        update.paused.is_none(),
        "unexpected pause: {:?}",
        update.paused
    );
    assert_eq!(update.trusted, "graph TD\nA-1 --> B\n");
}
