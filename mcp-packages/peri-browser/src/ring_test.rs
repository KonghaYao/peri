use super::*;

#[test]
fn an_empty_buffer_has_no_last_entry() {
    let buffer: RingBuffer<u8> = RingBuffer::new(4);
    assert!(buffer.is_empty());
    assert_eq!(buffer.len(), 0);
    assert_eq!(buffer.capacity(), 4);
    assert_eq!(buffer.last(), None);
    assert_eq!(buffer.iter().count(), 0);
}

#[test]
fn entries_are_kept_newest_wins() {
    let mut buffer = RingBuffer::new(3);
    for value in 1..=5 {
        buffer.push(value);
    }
    assert_eq!(buffer.len(), 3);
    assert_eq!(buffer.iter().copied().collect::<Vec<_>>(), vec![3, 4, 5]);
    assert_eq!(buffer.last(), Some(&5));
}

#[test]
fn iteration_is_double_ended_and_mutable() {
    let mut buffer = RingBuffer::new(4);
    buffer.push(10);
    buffer.push(20);
    assert_eq!(
        buffer.iter().rev().copied().collect::<Vec<_>>(),
        vec![20, 10]
    );
    for value in buffer.iter_mut() {
        *value += 1;
    }
    assert_eq!(buffer.iter().copied().collect::<Vec<_>>(), vec![11, 21]);
}

#[test]
fn clear_keeps_the_capacity() {
    let mut buffer = RingBuffer::new(2);
    buffer.push("a");
    buffer.push("b");
    buffer.clear();
    assert!(buffer.is_empty());
    assert_eq!(buffer.capacity(), 2);
    buffer.push("c");
    assert_eq!(buffer.last(), Some(&"c"));
}

#[test]
fn a_zero_capacity_buffer_drops_everything() {
    let mut buffer: RingBuffer<u8> = RingBuffer::new(0);
    buffer.push(1);
    assert!(buffer.is_empty());
    assert_eq!(buffer.last(), None);
}
