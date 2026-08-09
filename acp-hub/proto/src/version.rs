//! 版本常量（§4.1 / §5.3 / §13.1）。
//!
//! 真相来源以本 crate 实现为准：`CHAT_DOC_SCHEMA_VERSION` 由架构 §5.3 明示，
//! 其余数值为设计决策（见 `docs/plans/f1-proto.md` §11）。

/// 线协议版本（§13.1：`instance/hello` 携带，版本不匹配拒绝连接）。
///
/// 【决策】数值取 1；版本不匹配时的拒绝语义在 server auth 模块。
pub const PROTOCOL_VERSION: u32 = 1;

/// Chat Doc 结构版本（§5.3 明示，真相来源以本 crate 实现为准）。
pub const CHAT_DOC_SCHEMA_VERSION: u32 = 1;

/// Control Doc 结构版本（§5.4 未给数值，【决策】取 1）。
pub const CONTROL_DOC_SCHEMA_VERSION: u32 = 1;

/// Registry Doc 结构版本（§5.5 未给数值，【决策】取 1）。
pub const REGISTRY_DOC_SCHEMA_VERSION: u32 = 1;

/// y-sync update 编码版本（§4.1「固定 update 编码版本 v1」）。
pub const Y_UPDATE_ENCODING_VERSION: u32 = 1;
