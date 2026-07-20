//! Hook system integration.
//!
//! Hook middleware is configured through `SessionContext::hook_groups`.
//! Each group becomes a separate `HookMiddleware` instance in the agent pipeline.
//!
//! Hooks are event-driven callbacks (Command/Prompt/Http/Agent) for 14 event types,
//! provided by `peri_middlewares::hooks`.

pub use peri_middlewares::hooks::types::RegisteredHook;
