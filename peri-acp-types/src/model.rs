//! 模型契约 re-export（`peri_model::Model` 透传）。
//!
//! ACP 装配面签名（`host/stage_builder.rs` 的 `auxiliary_model` 透传参数，
//! `StageBuildFn` 契约镜像——与 peri-agent 正式 `build_stage_context` /
//! `StageBuildRequest.auxiliary_model` 同型）经本模块引用，避免 ACP 直接
//! 持有 `peri_model::` 路径（依赖门边 3 只拦直接引用）。契约层依赖
//! peri-model 为既有设计（见 `command.rs` 的 `auxiliary_model` 先例）。
//!
//! 随 StageBuildFn 签名改造 / v1 退役时可收敛。

pub use peri_model::Model;
