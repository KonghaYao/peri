//! 集成测试合并入口：五个文件合用一个测试二进制，减少 CI 需要链接的 target 数。
//!
//! Cargo 把含 `main.rs` 的 `tests/<dir>/` 当作一个 target，`tests/*.rs` 才各自成为
//! target；`spool_recovery` 的子进程自调用按新的完整测试名重新执行。

mod logging;
mod otel_contract;
mod spool_acceptance;
mod spool_http;
mod spool_recovery;
