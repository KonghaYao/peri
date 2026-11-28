//! 集成测试合并入口：十三个文件合用一个测试二进制，减少 CI 需要链接的 target 数。
//!
//! Cargo 把含 `main.rs` 的 `tests/<dir>/` 当作一个 target，`tests/*.rs` 才各自成为
//! target；用例名因此带 `<module>::` 前缀，nextest 的进程隔离组按新名字匹配。

mod actor_runtime_test;
mod http_contract_test;
mod items_pagination_test;
mod mcp_injection_test;
mod openai_hosted_test;
mod rivet_actor_test;
mod session_resources_test;
mod state_store_failure_test;
mod state_store_none_delete_test;
mod state_store_support;
mod state_store_test;
mod streaming_test;
mod subagents_test;
mod telemetry_test;
