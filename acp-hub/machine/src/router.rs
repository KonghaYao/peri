//! 废弃模块（F6，f6-machine.md §9）：旧单机 stdio 桥接的 SessionRouter
//! （spawn+initialize 时序、session/new 透传、RouterEvent）。
//!
//! 会话表职责已由 `hub.rs`（F6 改造）吸收，spawn→binding 时序由 server 驱动
//! （§6.2），本模块保留为空壳（`lib.rs` 声明不动）。`router_test.rs` 已随废弃
//! 移除（`git rm`）。
