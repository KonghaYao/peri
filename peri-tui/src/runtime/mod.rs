//! TUI runtime -- single event channel + background collectors + main loop.
//! State machine lives in state_machine/ (P2). Here we provide the I/O skeleton.

pub mod acp_notifier;
pub mod apply_context;
pub mod effect;
pub mod event_channel;
pub mod keyboard_collector;
pub mod main_loop;
