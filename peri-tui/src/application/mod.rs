//! 应用层：面板共享的应用语义与能力边界。
//!
//! 依赖方向单向：`kit`（视图、状态容器、atoms）依赖本层类型；本层不依赖
//! `crate::kit`，也就不读 atoms——作用域、目标等取值由调用方构造后传入。
//! 目标位置见方案 §8（`application/operations/` 承接分散的 ticket、超时与完成检查）。

pub(crate) mod operations;
