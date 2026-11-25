//! 集成测试合并入口：两个文件合用一个测试二进制，减少 CI 需要链接的 target 数。
//!
//! Cargo 把含 `main.rs` 的 `tests/<dir>/` 当作一个 target，`tests/*.rs` 才各自成为
//! target；两者都以 `CARGO_BIN_EXE_peri` 启动真实进程，合并后仍各自使用隔离的 HOME 与端口。

mod acp_registry_auth;
mod print_settlement;
