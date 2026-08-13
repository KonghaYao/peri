//! acp-hub-proto —— acp-hub 共享协议 crate。
//!
//! 承载三个二进制（server / instance / tui）共享的全部线协议类型与密码原语
//! （权威来源：[`docs/architecture.md`](https://github.com/acp-hub/acp-hub/blob/main/docs/architecture.md)，
//! 实现细节见 `docs/plans/f1-proto.md`）：
//!
//! - **帧模型**（[`frame`]）：`Frame` 枚举（serde tag `"t"`）+ 解析入口
//!   [`Frame::parse`]，未知 tag 与已知 tag 反序列化失败可区分；
//! - **Action/Ack 信封**（[`action`]、[`ack`]）：方法面 8 种 action、
//!   两阶段 Ack（`accepted`/`committed`/`duplicate`）与稳定错误码（§4.3/§4.4）；
//! - **instance 协议**（[`instance`]）：server ↔ instance 9 帧（§4.5）；
//! - **连接生命周期**（[`conn`]）：`auth`/`auth_response`/`ready`/`keep_alive`/
//!   `pong`、`DocId` 与关闭码常量（§4.6/§4.7/§9.2）；
//! - **y-sync envelope**（[`ysync`]）：subscribe/unsubscribe/update/sync/awareness
//!   （update 为 S→C 单向，§5.6）；
//! - **M1 帧集白名单**（[`whitelist`]）：全量 tag 注册表 + M1 收窄 + 方向约束
//!   （§4.8）；
//! - **HMAC 双向认证原语**（[`hmac`]）：HKDF 密钥派生、MAC 输入规范化、常量
//!   时间校验——纯函数，无 I/O（§9.2 顾问3 线格式）；
//! - **Y.Doc schema 类型镜像**（[`schema`]）：Chat/Control/Registry 三 Doc
//!   的 Rust 类型（§5.3–5.5），不持有 yrs 句柄；
//! - **版本与协议参数常量**（[`version`]、[`protocol`]）。
//!
//! 序列化约定：线协议结构一律 `#[serde(rename_all = "camelCase")]`（文档 JSON
//! 示例均为 camelCase）；时间字段为 RFC3339 字符串；错误码/枚举语义与架构
//! 文档逐项对应，不引入文档外的协议。

pub mod ack;
pub mod action;
pub mod conn;
pub mod event;
pub mod frame;
pub mod hmac;
pub mod instance;
pub mod protocol;
pub mod schema;
pub mod session;
pub mod version;
pub mod whitelist;
pub mod ysync;

// 公开面收敛：线协议帧类型从 frame 层直接可用。
pub use ack::{AckStatus, ActionAck, ActionError, ErrorCode};
pub use action::{ActionEnvelope, CancelChatPayload, CloseChatPayload, CreateChatPayload};
pub use conn::{Auth, AuthResponse, DocId, KeepAlive, Pong, Ready};
pub use event::EventFrame;
pub use frame::{Frame, FrameTag, ProtoError};
pub use instance::{
    BufferedFrame, InstanceBufferSync, InstanceEvent, InstanceHeartbeat, InstanceHello,
    InstanceKill, InstanceKillAck, InstanceProcessExit, InstanceSpawn, InstanceSpawnAck,
};
pub use session::SessionListFrame;
pub use version::{
    CHAT_DOC_SCHEMA_VERSION, PROTOCOL_VERSION, REGISTRY_DOC_SCHEMA_VERSION,
    SESSION_DOC_SCHEMA_VERSION, Y_UPDATE_ENCODING_VERSION,
};
pub use whitelist::{
    m1_allows, m1_allows_action_type, m1_check, Direction, M1Check, Role, M1_ACTION_TYPES,
};
pub use ysync::{YsyncAwareness, YsyncSubscribe, YsyncSync, YsyncUnsubscribe, YsyncUpdate};
