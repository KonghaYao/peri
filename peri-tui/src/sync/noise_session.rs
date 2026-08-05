//! 认证 Noise 握手（r2-encrypted-transfer v1）。
//!
//! 固定 `Noise_IK_25519_ChaChaPoly_SHA256`（snow 0.10 默认 resolver），两条
//! opaque 消息：`-> e, es, s, ss` / `<- e, ee, se`。prologue 绑定协议版本、
//! channel ID、双方 device ID、角色与双方 static public key。
//!
//! 安全契约（由 API 强制，非仅文档约定）：
//! - sender（initiator）预先知道 receiver 的 X25519 静态公钥（来自 trusted
//!   peers）；data key 由 initiator 在 **msg1 的加密 payload** 中携带（IK 的
//!   msg1 在 `es`/`ss` 混合后已建立加密，payload 不泄露给第三方）；
//! - responder 读完 msg1 后本地已完成密码学认证（msg1 可被任何持有自己
//!   static 私钥且知道 responder static 公钥的实体构造），**必须**以
//!   [`HandshakeSession::verify_peer`] 或 [`HandshakeSession::into_transport`]
//!   的 `expected_remote_static` 完成与 trusted peer 记录比对：不符即失败，
//!   不得进入 transport；核对完成前 [`HandshakeSession::data_key`] 不暴露；
//! - initiator 的 peer 绑定来自调用方传入的 trusted 记录
//!   （`params.responder`，`into_transport` 时同样强制核对）；
//! - data key 仅驻留进程内存，绝不写盘（见 [`crate::sync::crypto::DataKey`]）。

use anyhow::{Context, Result};
use snow::{Builder, HandshakeState, TransportState};
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::sync::canonical;
use crate::sync::crypto::DataKey;

/// 固定 Noise 模式（snow 默认 resolver 支持）。
pub const NOISE_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_SHA256";

/// 握手消息/transport 消息的缓冲区上限（远超 IK 三条消息与任何控制载荷）。
const BUF_LEN: usize = 4096;

/// msg1 payload 布局（客户端内部约定，非 wire 契约；TS Worker 只转发 opaque blob）：
/// `[0..32]` data key；`[32..64]` manifest hash（SHA-256 明文 msgpack）；
/// `[64..72]` part count（u64 BE）。纯 data key 的 32B 形态保留（兼容/测试）。
pub const MSG1_PAYLOAD_KEY_LEN: usize = 32;
pub const MSG1_PAYLOAD_MANIFEST_OFFSET: usize = 32;
pub const MSG1_PAYLOAD_COUNT_OFFSET: usize = 64;
pub const MSG1_PAYLOAD_FULL_LEN: usize = 72;

/// msg1 payload 是否合法（32B 纯 data key 或 72B 完整布局）。
pub fn is_valid_msg1_payload_len(len: usize) -> bool {
    len == MSG1_PAYLOAD_KEY_LEN || len == MSG1_PAYLOAD_FULL_LEN
}

/// 握手角色：sender 是 initiator，receiver 是 responder。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Sender,
    Receiver,
}

impl Role {
    /// canonical 角色标签（进入 prologue）。
    pub fn label(self) -> &'static str {
        match self {
            Role::Sender => "sender",
            Role::Receiver => "receiver",
        }
    }
}

/// 握手参与方绑定（device ID + 两个公钥，全部进入 prologue）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerBinding {
    /// 设备 ID（base64url-no-pad）。
    pub device_id: String,
    /// Ed25519 身份公钥。
    pub ed_pub: [u8; 32],
    /// X25519 静态公钥。
    pub x_pub: [u8; 32],
}

/// 握手上下文（双方构造时必须逐字节一致，否则认证失败）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeParams {
    /// channel ID（16 字节 base64url-no-pad 字符串）。
    pub channel_id: String,
    /// sender（initiator）绑定。
    pub initiator: PeerBinding,
    /// receiver（responder）绑定。
    pub responder: PeerBinding,
}

/// 构造握手 prologue：
/// `peri-sync/v1|handshake|<channel_id>|<init_id>|sender|<init_ed>|<init_x>|<resp_id>|receiver|<resp_ed>|<resp_x>`。
pub fn handshake_prologue(params: &HandshakeParams) -> Result<Vec<u8>> {
    Ok(canonical::context(
        "handshake",
        &[
            &params.channel_id,
            &params.initiator.device_id,
            "sender",
            &canonical::b64url_nopad(&params.initiator.ed_pub),
            &canonical::b64url_nopad(&params.initiator.x_pub),
            &params.responder.device_id,
            "receiver",
            &canonical::b64url_nopad(&params.responder.ed_pub),
            &canonical::b64url_nopad(&params.responder.x_pub),
        ],
    )?
    .into_bytes())
}

fn build_session(
    params: &HandshakeParams,
    local_x: &StaticSecret,
    remote_x: Option<&XPublicKey>,
    initiator: bool,
) -> Result<HandshakeState> {
    let pattern = NOISE_PATTERN
        .parse::<snow::params::NoiseParams>()
        .context("invalid noise pattern")?;
    let prologue = handshake_prologue(params)?;
    // snow 的 Builder 借用密钥切片，必须先绑定为长生命周期局部变量。
    let local_bytes = local_x.to_bytes();
    let remote_bytes: Option<[u8; 32]> = remote_x.map(|remote| remote.to_bytes());
    let builder = Builder::new(pattern)
        .local_private_key(&local_bytes)
        .context("invalid local static key")?
        .prologue(&prologue)
        .context("invalid prologue")?;
    let builder = match &remote_bytes {
        Some(bytes) => builder
            .remote_public_key(bytes)
            .context("invalid remote static key")?,
        None => builder,
    };
    if initiator {
        builder
            .build_initiator()
            .context("failed to build noise initiator")
    } else {
        builder
            .build_responder()
            .context("failed to build noise responder")
    }
}

/// 建立 sender（initiator）侧握手会话。
///
/// `params.responder.x_pub` 必须来自调用方已信任的 peer 记录；进入 transport
/// 时仍须以 [`HandshakeSession::into_transport`] 的 `expected_remote_static`
/// 传入同一 trusted 公钥强制核对。
pub fn initiator_session(
    params: &HandshakeParams,
    local_x: &StaticSecret,
) -> Result<HandshakeSession> {
    Ok(HandshakeSession {
        state: build_session(
            params,
            local_x,
            Some(&XPublicKey::from(params.responder.x_pub)),
            true,
        )?,
        role: Role::Sender,
        messages: 0,
        data_key: None,
        peer_verified: false,
    })
}

/// 建立 receiver（responder）侧握手会话。
///
/// responder 不需要预先知道 initiator 的静态公钥（IK 在 msg1 中认证）；读完
/// msg1 后必须以 [`HandshakeSession::verify_peer`] 或
/// [`HandshakeSession::into_transport`] 的 `expected_remote_static` 与 trusted
/// peer 记录比对，核对完成前 data key 不暴露。
pub fn responder_session(
    params: &HandshakeParams,
    local_x: &StaticSecret,
) -> Result<HandshakeSession> {
    Ok(HandshakeSession {
        state: build_session(params, local_x, None, false)?,
        role: Role::Receiver,
        messages: 0,
        data_key: None,
        peer_verified: false,
    })
}

/// 进行中的握手会话（两条消息交换后进入 transport）。
pub struct HandshakeSession {
    state: HandshakeState,
    role: Role,
    messages: u8,
    data_key: Option<DataKey>,
    /// 是否已完成与 trusted peer 记录的 static 公钥核对（见 [`Self::verify_peer`]）。
    peer_verified: bool,
}

impl HandshakeSession {
    /// 写出下一条 opaque 握手消息。
    ///
    /// initiator 的握手首条消息（msg1）必须携带 data key（前 32 字节）；完整
    /// 布局为 `data key(32) ‖ manifest hash(32) ‖ part count(8)`（见
    /// [`MSG1_PAYLOAD_FULL_LEN`]），纯 data key 的 32B 形态也接受。IK 只有
    /// 两条握手消息，msg1 是 initiator 唯一一次写（在 `es`/`ss` 混合后加密），
    /// responder 从中提取 data key 并核对 trusted 身份。
    pub fn write_message(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        if self.role == Role::Sender
            && self.messages == 0
            && !is_valid_msg1_payload_len(payload.len())
        {
            anyhow::bail!(
                "msg1 payload must be {} (data key) or {} (data key + manifest) bytes, got {}",
                MSG1_PAYLOAD_KEY_LEN,
                MSG1_PAYLOAD_FULL_LEN,
                payload.len()
            );
        }
        let mut buf = vec![0u8; BUF_LEN];
        let n = self
            .state
            .write_message(payload, &mut buf)
            .context("noise write failed")?;
        buf.truncate(n);
        self.messages += 1;
        if self.role == Role::Sender && self.messages == 1 {
            // 前置长度检查保证此转换必然成功；中间副本用 Zeroizing 包裹，
            // drop 时清零（DataKey 内部再零化一份）。
            let key = Zeroizing::new(
                <[u8; 32]>::try_from(&payload[..32]).expect("msg1 data key length checked"),
            );
            self.data_key = Some(DataKey::from_array(*key));
        }
        Ok(buf)
    }

    /// 读入下一条 opaque 握手消息；responder 从 msg1 载荷提取 data key。
    ///
    /// msg1 解密成功只完成密码学认证；身份核对由 API 强制——responder 必须以
    /// [`Self::verify_peer`] 或 [`Self::into_transport`] 的 `expected_remote_static`
    /// 与 trusted 记录比对，不符即失败；核对完成前 [`Self::data_key`] 不暴露。
    pub fn read_message(&mut self, message: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; BUF_LEN];
        let n = self
            .state
            .read_message(message, &mut buf)
            .context("noise read failed")?;
        buf.truncate(n);
        self.messages += 1;
        if self.role == Role::Receiver && self.messages == 1 {
            if !is_valid_msg1_payload_len(buf.len()) {
                anyhow::bail!(
                    "msg1 payload has invalid length: {} (expected {} or {})",
                    buf.len(),
                    MSG1_PAYLOAD_KEY_LEN,
                    MSG1_PAYLOAD_FULL_LEN
                );
            }
            // 中间副本用 Zeroizing 包裹，drop 时清零。
            let key = Zeroizing::new(
                <[u8; 32]>::try_from(&buf[..32]).expect("msg1 data key length checked"),
            );
            self.data_key = Some(DataKey::from_array(*key));
        }
        Ok(buf)
    }

    /// 握手完成后对方的 X25519 static 公钥。
    ///
    /// responder 在读完 msg1 后可用它核对 trusted peer 记录（非 trusted
    /// identity 必须中止）；该核对由 [`Self::verify_peer`] /
    /// [`Self::into_transport`] 强制完成。
    pub fn remote_static(&self) -> Option<[u8; 32]> {
        self.state.get_remote_static().map(|b| {
            let mut out = [0u8; 32];
            out.copy_from_slice(&b[..32]);
            out
        })
    }

    /// 核对握手对端与 trusted 记录中的 X25519 static 公钥一致。
    ///
    /// responder 读完 msg1 后必须调用本方法（或通过 [`Self::into_transport`]
    /// 的 `expected_remote_static` 强制核对）；核对成功后才允许访问
    /// [`Self::data_key`]。不符即失败——不得进入 transport、不得使用已提取
    /// 的 data key。核对幂等，可重复调用。
    pub fn verify_peer(&mut self, trusted_remote_static: [u8; 32]) -> Result<()> {
        let actual = self.remote_static().ok_or_else(|| {
            anyhow::anyhow!("no remote static key available to verify (handshake incomplete)")
        })?;
        if actual != trusted_remote_static {
            anyhow::bail!("remote static key does not match the trusted device");
        }
        self.peer_verified = true;
        Ok(())
    }

    /// 握手是否已完成（三条消息交换完毕）。
    pub fn is_handshake_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// 已完成握手中的 channel data key（initiator 在 msg1 中携带的载荷）。
    ///
    /// 仅当调用方已通过 [`Self::verify_peer`] 或 [`Self::into_transport`] 的
    /// `expected_remote_static` 完成 trusted 身份核对后才暴露；核对前恒为
    /// `None`（responder 侧的 msg1 载荷即使已提取也不可见）。
    pub fn data_key(&self) -> Option<&DataKey> {
        if !self.peer_verified {
            return None;
        }
        self.data_key.as_ref()
    }

    /// 完成握手，进入加密 transport。
    ///
    /// `expected_remote_static` 为 trusted 记录中的对端 X25519 静态公钥：
    /// 传入 `Some(trusted)` 时先核对 [`Self::remote_static`] 一致，不符即失败
    /// （responder 侧 mandatory：msg1 可被任何知道其 static 公钥的实体构造）；
    /// `None` 表示调用方显式跳过核对（测试专用逃生口，生产路径必须传
    /// `Some`）。未完成握手或无 data key 同样失败。
    pub fn into_transport(
        mut self,
        expected_remote_static: Option<[u8; 32]>,
    ) -> Result<TransportSession> {
        if !self.state.is_handshake_finished() {
            anyhow::bail!("handshake not finished");
        }
        if self.data_key.is_none() {
            anyhow::bail!("handshake did not carry a data key");
        }
        if let Some(expected) = expected_remote_static {
            self.verify_peer(expected)?;
        }
        let state = self
            .state
            .into_transport_mode()
            .context("noise transport init failed")?;
        Ok(TransportSession {
            state,
            data_key: self.data_key,
        })
    }
}

/// 握手完成后的加密通道（双向，Noise 帧）。
///
/// 注意：snow transport 单条消息上限 65535 字节；v1 仅用于握手后的控制消息
/// （payload part 走 R2 + AES-GCM envelope，不经过本通道）。
pub struct TransportSession {
    state: TransportState,
    data_key: Option<DataKey>,
}

impl TransportSession {
    /// 加密一条应用消息（输出带 Noise 认证开销的完整消息）。
    pub fn send(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; plaintext.len() + 64];
        let n = self
            .state
            .write_message(plaintext, &mut buf)
            .context("transport write failed")?;
        buf.truncate(n);
        Ok(buf)
    }

    /// 解密一条应用消息；任何篡改即失败。
    pub fn recv(&mut self, message: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; message.len() + 64];
        let n = self
            .state
            .read_message(message, &mut buf)
            .context("transport read failed")?;
        buf.truncate(n);
        Ok(buf)
    }

    /// channel data key（与握手会话一致）。
    ///
    /// 仅能经 [`HandshakeSession::into_transport`] 获得：生产路径传入
    /// `Some(trusted)` 时该通道在构造前已完成 trusted 身份核对。
    pub fn data_key(&self) -> Option<&DataKey> {
        self.data_key.as_ref()
    }
}
