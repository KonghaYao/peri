use super::{Connection, Version};

fn foreground(sid: &str) -> Connection {
    Connection {
        version: Version::Two,
        active: Some(sid.into()),
        foreground: true,
        epoch: 7,
        ..Connection::default()
    }
}

#[test]
fn requires_action_completion_of_a_cancelled_turn_cannot_publish_running() {
    let mut connection = foreground("A");
    assert!(connection.is_current_turn("A", 7));
    // The turn is cancelled while its permission completion waits for `wire`.
    connection.epoch += 1;
    assert!(!connection.is_current_turn("A", 7));
}

#[test]
fn requires_action_completion_cannot_leak_into_a_new_turn_or_session() {
    let mut connection = foreground("A");
    // A newer foreground turn started.
    connection.epoch += 1;
    assert!(!connection.is_current_turn("A", 7));
    // The connection closed or idled.
    connection.epoch = 8;
    connection.foreground = false;
    assert!(!connection.is_current_turn("A", 8));
    // Another session became active.
    connection.foreground = true;
    connection.active = Some("B".into());
    assert!(!connection.is_current_turn("A", 8));
    assert!(connection.is_current_turn("B", 8));
}
