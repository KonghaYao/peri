use super::render_final;
use super::render_source;
use crate::mermaid::stream::DiagramMode;

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

/// 流式确认后的中文标签、`<br/>` 与点线标签仍能交给排版器画成帧。
#[test]
fn streamed_labels_survive_into_the_frame() {
    let body = "graph TD\n  用户[\"中文<br/>标签\"] -.-> 系统\n  系统 -- \"引号, 标签\" --> 用户\n  用户 -. 点线文字 .-> 系统\n";
    let mut stream = crate::mermaid::stream::MermaidStream::new();
    let mut buffer = Vec::new();
    for piece in body.as_bytes().chunks(3) {
        buffer.extend_from_slice(piece);
        if let Ok(prefix) = std::str::from_utf8(&buffer) {
            stream.observe(prefix, false);
        }
    }
    let trusted = stream.observe(body, false).trusted.to_string();
    assert_eq!(
        trusted,
        "graph TD\n用户[\"中文<br/>标签\"] -.-> 系统\n系统 -- \"引号, 标签\" --> 用户\n用户 -. 点线文字 .-> 系统\n"
    );

    let rendered = render_source(&trusted, 80).expect("streamed labels render");
    let text = rendered.lines().join("\n");
    for label in ["用户", "系统", "中文"] {
        assert!(text.contains(label), "{label} missing from frame: {text}");
    }
}
