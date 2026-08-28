/// The parts of message content needed by the portable emptiness policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageContentShape<'a> {
    Text(&'a str),
    Blocks(usize),
    Raw(usize),
}

/// Returns whether a message content shape has no content.
pub const fn is_message_content_empty(content: MessageContentShape<'_>) -> bool {
    match content {
        MessageContentShape::Text(text) => text.is_empty(),
        MessageContentShape::Blocks(len) | MessageContentShape::Raw(len) => len == 0,
    }
}
