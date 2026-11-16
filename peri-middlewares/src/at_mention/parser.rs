use serde::{Deserialize, Serialize};

#[cfg(test)]
#[path = "parser_test.rs"]
mod tests;

#[path = "parser/at_mention.rs"]
mod at_mention;
pub use at_mention::extract_at_mentions;
pub use at_mention::AtMention;
