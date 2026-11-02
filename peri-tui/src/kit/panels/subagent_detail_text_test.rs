use super::*;

#[test]
fn newline_heavy_history_keeps_a_sparse_index_and_only_materializes_visible_rows() {
    let mut text = "\n".repeat(512 * 1024 - 8);
    text.push_str("last row");
    let layout = TextLayout::new(text.into(), 80);
    assert_eq!(layout.height(), 512 * 1024 - 7);
    assert!(layout.checkpoints.capacity() * std::mem::size_of::<usize>() <= 128 * 1024);
    let visible = layout.lines(layout.height() - 10..layout.height());
    assert_eq!(visible.len(), 10);
    assert_eq!(visible.last().unwrap().to_string(), "last row");
    assert!(layout.lines(layout.height()..usize::MAX).is_empty());
}

#[test]
fn sparse_seek_preserves_graphemes_newlines_and_filters_controls() {
    let text = format!("{}中👩‍💻e\u{301}\u{1b}[\r\nlast\n", "ab\n".repeat(70));
    let layout = TextLayout::new(text.into(), 3);
    let visible: Vec<_> = layout.lines(70..100).iter().map(Line::to_string).collect();
    assert_eq!(visible, ["中", "👩‍💻e\u{301}", "[", "las", "t", ""]);
    assert!(layout.lines(0..100).iter().all(|line| line.width() <= 3));
}

#[test]
fn replacing_a_layout_releases_the_previous_body() {
    let text: Arc<str> = "old body".into();
    let weak = Arc::downgrade(&text);
    let mut layout = TextLayout::new(text, 20);
    assert_eq!(layout.height(), 1);
    layout = TextLayout::new("new body".into(), 20);
    assert!(weak.upgrade().is_none());
    assert_eq!(layout.lines(0..1)[0].to_string(), "new body");
}
