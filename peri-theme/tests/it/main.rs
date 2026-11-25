//! 集成测试合并入口：两个文件合用一个测试二进制，减少 CI 需要链接的 target 数。
//!
//! Cargo 把含 `main.rs` 的 `tests/<dir>/` 当作一个 target，`tests/*.rs` 才各自成为
//! target；`test_loader` 的隔离 HOME 子进程按新的完整测试名重新执行。

mod test_builtin;
mod test_loader;
