//! 协议层（Feature F5）：ACPChannel 入站规范化 + Translator 出站翻译（§6.1）。
//!
//! 定位：server 侧的**唯一协议边界**（架构 §6.1）。instance 透明转发原始 ACP
//! 帧，server 在此规范化为 [`NormalizedEvent`]（state 层定义），聚合层只消费
//! 规范化事件；客户端 Action 经 [`Translator`] 翻译为 ACP JSON-RPC 下发。
//!
//! 本层为**纯函数层**（零 I/O、零状态依赖，除 [`Translator`] 的 rpcId 分配
//! 计数器）：binding 校验与持久化在调用方（channel 层 RelayEventHandler /
//! CommandCoordinator）。
//!
//! 脱敏纪律（§9.3）：本层不产生日志；字段提取只做结构校验，不记录正文。
//!
//! 设计稿：`docs/plans/f5-channel-control.md` §3–§4；权威：`docs/architecture.md`
//! §6.1/§4.3/§9.3。

mod acp_channel;
mod translator;

pub use acp_channel::{
    extract_agent_config, extract_session_id, AcpChannel, DropReason, NormalizeOutcome,
    PermissionRequestFields, PERMISSION_TIMEOUT,
};
pub use translator::{validate_cwd, OutboundCtx, OutboundMessage, TranslateError, Translator};
