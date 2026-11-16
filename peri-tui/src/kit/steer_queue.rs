mod view;

#[cfg(test)]
#[path = "steer_queue/view_test.rs"]
mod tests;

#[expect(
    clippy::module_inception,
    reason = "The style guide keeps the facade declarative and names the implementation leaf after its primary type."
)]
mod steer_queue;
use self::steer_queue as queue;
pub use queue::*;
