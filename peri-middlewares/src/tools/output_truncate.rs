//! 按字节截断（3.0 归位 L1：实现随 bg shell 执行链迁至
//! `peri_agent::agent::async_tasks`，此处保留路径别名，避免既有调用点改动）。

pub use peri_agent::agent::async_tasks::truncate_bytes;
