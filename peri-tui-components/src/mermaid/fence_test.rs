use super::MermaidMessageRef;

#[test]
fn local_message_identity_survives_bubble_copy_and_upgrades_to_persistent_id() {
    let started = std::time::Instant::now();
    let local = MermaidMessageRef::for_bubble(None, Some(started), 11);
    assert_eq!(
        local,
        MermaidMessageRef::for_bubble(None, Some(started), 11)
    );
    assert_ne!(
        local,
        MermaidMessageRef::for_bubble(None, Some(started + std::time::Duration::from_nanos(1)), 11)
    );
    assert_eq!(
        MermaidMessageRef::for_bubble(Some("msg-1"), Some(started), 11),
        MermaidMessageRef::Persistent("msg-1".to_string())
    );
    assert_eq!(
        MermaidMessageRef::for_bubble(None, None, 42),
        MermaidMessageRef::Local(42)
    );
}
