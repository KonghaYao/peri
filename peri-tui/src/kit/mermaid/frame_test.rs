use super::render_source;

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
fn failure_reason_is_single_line_and_bounded() {
    let failure = render_source("", 40).expect_err("empty input fails");
    assert!(!failure.reason().contains('\n'));
    assert!(failure.reason().chars().count() <= 161);
}
