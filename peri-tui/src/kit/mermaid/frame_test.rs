use super::render_final;
use super::render_source;
use crate::kit::mermaid::stream::DiagramMode;

#[test]
fn renders_flowchart_and_sequence_to_unicode_lines() {
    let flow = render_source("graph TD\nA[Start] --> B[End]\n", 60)
        .expect("flowchart source is supported");
    let text = flow.lines().join("\n");
    assert!(text.contains("Start"), "flowchart label missing: {text}");
    assert!(text.contains("End"), "flowchart label missing: {text}");
    assert!(
        text.contains('┌') || text.contains('╭'),
        "box drawing missing"
    );

    let sequence =
        render_source("sequenceDiagram\nA->>B: hi\n", 60).expect("sequence source is supported");
    let text = sequence.lines().join("\n");
    assert!(text.contains("hi"), "message label missing: {text}");
    assert!(text.contains("──"), "message arrow missing: {text}");
}

#[test]
fn header_only_is_not_a_publishable_frame() {
    let failure = render_source("graph TD\n", 60).expect_err("empty graph is not a frame");
    assert!(
        failure.reason().contains("empty"),
        "unexpected reason: {}",
        failure.reason()
    );
}

#[test]
fn rendering_is_deterministic_for_the_same_input_and_width() {
    let source = "graph LR\nA --> B\nB --> C\n";
    let first = render_source(source, 80).expect("first render succeeds");
    let second = render_source(source, 80).expect("second render succeeds");
    assert_eq!(first.lines(), second.lines());
}

#[test]
fn closed_fan_out_is_valid_and_renders() {
    let source = "graph LR\nA & B --> C & D\n";
    let rendered = render_final(source, DiagramMode::Flowchart, 80)
        .expect("the upstream renderer accepts fan-out in a closed fence");
    let text = rendered.lines().join("\n");
    for node in ["A", "B", "C", "D"] {
        assert!(text.contains(node), "{node} missing from fan-out: {text}");
    }
}

#[test]
fn closed_unterminated_statement_is_still_rejected() {
    let failure = render_final("graph LR\nA[\"x\n", DiagramMode::Flowchart, 80)
        .expect_err("an unterminated label is not a valid closed diagram");
    assert!(
        failure.reason().contains("unterminated"),
        "unexpected reason: {}",
        failure.reason()
    );
}

#[test]
fn failure_reason_is_single_line_and_bounded() {
    let failure = render_source("", 40).expect_err("empty input fails");
    assert!(!failure.reason().contains('\n'));
    assert!(failure.reason().chars().count() <= 161);
}
