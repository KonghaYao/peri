use std::future::Future;
use std::time::Duration;

pub use std::time::Instant;

#[cfg(target_os = "emscripten")]
#[path = "timer/delay.rs"]
mod delay;
#[cfg(target_os = "emscripten")]
use delay as js;

#[cfg(test)]
#[path = "timer_test.rs"]
mod tests;

#[path = "timer/interval.rs"]
mod interval;
pub use interval::interval;
pub use interval::sleep;
pub use interval::sleep_until;
pub use interval::timeout;
pub use interval::timeout_at;
pub use interval::Elapsed;
pub use interval::Interval;
pub use interval::MissedTickBehavior;
