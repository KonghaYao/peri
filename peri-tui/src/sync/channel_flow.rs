//! 新协议 channel 状态机（r2-encrypted-transfer v1，Slice 3）。
//!
//! sender：打包（scanner → msgpack → 32MiB 预算分片 → AES-GCM envelope）→
//! create → 每 30s epoch 注册同步码（≤2/min、撞码重试 1 次）→ 轮询 msg2 →
//! Noise msg1（携带 data key ‖ manifest hash ‖ part count）/msg2 relay →
//! 分片上传 → 等待 confirm（终态 404）。
//!
//! receiver：lookup → join → 轮询 msg1 → 从 trusted peers 匹配 sender
//! （msg1 只能被与真实 prologue 一致的会话解开；`into_transport(Some(..))`
//! 强制核对 remote static）→ 写 msg2 → 下载全部 part → 逐 part 验
//! AES-GCM AAD（绑定 channel/part index/manifest hash）→ staging 全量落盘 →
//! 全部成功才 commit → confirm（失败不回滚已提交文件、不自动重试）。
//!
//! 安全与日志：错误与日志不含同步码、channel ID 全文、Authorization、data key、
//! manifest hash、明文 payload 与签名（channel 一律以 SHA-256 前 8 hex 表示）。

use std::fmt::Write as _;
use std::future::Future;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::sync::crypto::{self, DataKey};
use crate::sync::device::{DeviceId, DevicePublic, TrustedPeer, TrustedPeers};
use crate::sync::http_client::{
    self, ApiClient, ApiError, ApiErrorKind, Backoff, HandshakeBody, ReqwestClient,
};
use crate::sync::keystore::SecretStore;
use crate::sync::limits;
use crate::sync::noise_session::{self, HandshakeParams, PeerBinding};
use crate::sync::protocol::{FileEntry, SyncItems, SyncPackage};
use crate::sync::{device_cli, scanner, sync_code, writer};

/// sender 等待 receiver 加入/msg2 的超时（默认与 channel created TTL 一致）。
pub struct FlowConfig {
    /// 轮询间隔（默认 3s；L2 复审修复：3–5s 避免 60/min 限流边缘）。
    pub poll_interval: Duration,
    /// 等待对端（join/msg1/msg2）与下载 404 重试的总超时
    /// （H5 复审修复：404 重试预算绑定本超时，不设固定次数）。
    pub start_timeout: Duration,
    /// 上传完成后等待 receiver confirm 的超时。
    pub confirm_wait_timeout: Duration,
    /// 时钟（unix 秒）：签名时间戳与同步码 epoch 来源。
    /// 每次请求前重新读取（H1：重发/轮询刷新签名时间戳）；测试注入可控时钟。
    pub now: Box<dyn Fn() -> u64 + Send + Sync>,
    /// part 级请求最小间隔（Medium-2 复审修复：默认 1s ⇒ ≤1 req/s，配合服务端
    /// 60/min 冻结限流；测试可设 `Duration::ZERO` 关闭节流）。
    pub min_part_interval: Duration,
}

impl Default for FlowConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(3),
            start_timeout: Duration::from_secs(limits::TTL_CREATED_SECS),
            confirm_wait_timeout: Duration::from_secs(300),
            now: Box::new(now_secs),
            min_part_interval: Duration::from_secs(1),
        }
    }
}

/// sender 侧流程输入。
pub struct SenderFlow<'a> {
    pub client: &'a dyn ApiClient,
    pub backoff: &'a dyn Backoff,
    pub store: &'a dyn SecretStore,
    pub local: &'a DevicePublic,
    pub target: &'a TrustedPeer,
    pub home_dir: &'a Path,
    pub cwd: &'a Path,
    pub items: &'a SyncItems,
    pub cfg: &'a FlowConfig,
}

/// sender 流程结果。
#[derive(Debug)]
pub struct SenderOutcome {
    pub channel_id: String,
    pub parts_uploaded: usize,
}

/// receiver 侧流程输入。
pub struct ReceiverFlow<'a> {
    pub client: &'a dyn ApiClient,
    pub backoff: &'a dyn Backoff,
    pub store: &'a dyn SecretStore,
    pub local: &'a DevicePublic,
    pub peers: &'a TrustedPeers,
    pub home_dir: &'a Path,
    pub cwd: &'a Path,
    pub cfg: &'a FlowConfig,
}

/// receiver 流程结果。
#[derive(Debug)]
pub struct ReceiverOutcome {
    pub channel_id: String,
    pub files: usize,
}

/// sender 主流程。`on_code(code, epoch, remaining_secs)` 每轮询 tick 调用一次，
/// 用于屏幕展示滚动码与倒计时（测试可捕获码）。
pub async fn run_sender(
    flow: SenderFlow<'_>,
    mut on_code: impl FnMut(sync_code::SyncCode, u64, u64),
) -> Result<SenderOutcome> {
    let local = flow.local;
    let target = flow.target;

    // ── 1. 打包：scanner → msgpack → 明文分片（65507B）→ seal 后每片 64KiB 密文 ──
    // C1 复审修复：明文分片上限 = MAX_PART_BYTES - ENVELOPE_HEADER_LEN - AEAD_TAG_LEN
    // （=65507），seal 后密文恰为 64KiB，与 TS `ct.length > maxPartBytes → 413` 对齐；
    // manifest 预算按密文总字节累计（TS `total_bytes + ct.length` 同口径）。
    let channel_id = random_channel_id()?;
    let sync_pkg = scanner::scan_all(flow.home_dir, flow.cwd, flow.items);
    let msgpack = sync_pkg.to_msgpack()?;
    if msgpack.is_empty() {
        anyhow::bail!("nothing to sync (empty package)");
    }
    let manifest_hash = sha256_bytes(&msgpack);
    let data_key = DataKey::random()?;
    let mut parts = Vec::with_capacity(msgpack.chunks(limits::MAX_PLAINTEXT_PART_BYTES).count());
    for (i, chunk) in msgpack.chunks(limits::MAX_PLAINTEXT_PART_BYTES).enumerate() {
        let aad = crypto::payload_aad(&channel_id, i as u64, &manifest_hash)?;
        parts.push(crypto::seal(&data_key, &aad, chunk));
    }
    let part_count = parts.len() as u64;
    let ciphertext_total: usize = parts.iter().map(|p| p.len()).sum();
    limits::validate_manifest(parts.len(), ciphertext_total)
        .context("payload exceeds the frozen budget (32 MiB / 512 parts)")?;

    // ── 2. create（channel_id 由 sender 生成；幂等重放不新建）──
    let create_body = http_client::CreateChannelBody {
        channel_id: channel_id.clone(),
        device_id: local.device_id.to_b64(),
        expected_device_id: target.device_id.to_b64(),
        expected_ed_pub: b64url(&target.ed_pub),
        sender_ed_pub: b64url(&local.ed_pub),
        sender_x_pub: b64url(&local.x_pub),
    };
    let create_auth = auth_header_at(
        (flow.cfg.now)(),
        flow.store,
        &local.device_id,
        "create",
        &[
            &channel_id,
            &local.device_id.to_b64(),
            &target.device_id.to_b64(),
            &b64url(&local.ed_pub),
            &b64url(&local.x_pub),
        ],
    )?;
    retry_429(flow.backoff, || {
        flow.client
            .create_channel(create_body.clone(), &create_auth)
    })
    .await
    .map_err(|e| anyhow::anyhow!("create channel failed: {e}"))?;
    tracing::info!(channel = %channel_hash(&channel_id), "channel created");

    // ── 3. Noise initiator；msg1 payload = data key ‖ manifest hash ‖ part count ──
    let params = HandshakeParams {
        channel_id: channel_id.clone(),
        initiator: PeerBinding {
            device_id: local.device_id.to_b64(),
            ed_pub: local.ed_pub,
            x_pub: local.x_pub,
        },
        responder: PeerBinding {
            device_id: target.device_id.to_b64(),
            ed_pub: target.ed_pub,
            x_pub: target.x_pub,
        },
    };
    let local_x = flow.store.x25519_private()?;
    let mut session = noise_session::initiator_session(&params, &local_x)?;
    let mut msg1_payload = Vec::with_capacity(noise_session::MSG1_PAYLOAD_FULL_LEN);
    msg1_payload.extend_from_slice(data_key.as_array());
    msg1_payload.extend_from_slice(&manifest_hash);
    msg1_payload.extend_from_slice(&part_count.to_be_bytes());
    let msg1_blob = session.write_message(&msg1_payload)?;
    let msg1_b64 = b64url(&msg1_blob);
    let msg1_auth = auth_header_at(
        (flow.cfg.now)(),
        flow.store,
        &local.device_id,
        "msg",
        &[&channel_id, "1", &b64url(&sha256_bytes(&msg1_blob))],
    )?;
    let msg1_body = HandshakeBody {
        msg: Some(msg1_b64.clone()),
    };
    retry_429(flow.backoff, || {
        flow.client
            .handshake(&channel_id, "sender", msg1_body.clone(), &msg1_auth)
    })
    .await
    .map_err(|e| anyhow::anyhow!("handshake msg1 failed: {e}"))?;
    tracing::debug!(channel = %channel_hash(&channel_id), "msg1 stored");

    // ── 4. 每 epoch 注册码 + 轮询 msg2（幂等重发 msg1 拉取）──
    let start = SystemTime::now();
    let mut current_code: Option<sync_code::SyncCode> = None;
    let mut registered_epoch: Option<u64> = None;
    let mut msg2: Option<Vec<u8>> = None;
    while msg2.is_none() {
        if start.elapsed().unwrap_or_default() > flow.cfg.start_timeout {
            anyhow::bail!("timed out waiting for receiver to join");
        }
        let now = (flow.cfg.now)();
        let epoch = sync_code::epoch(now);
        if registered_epoch != Some(epoch) {
            match register_code_with_collision_retry(&flow, &channel_id, epoch).await? {
                Some(code) => current_code = Some(code),
                // H2 复审修复：register 遇 403 = 码使命已结束（channel 已离开
                // created 状态，如 receiver 已 join），不再注册/展示码，继续轮询 msg2。
                None => current_code = None,
            }
            registered_epoch = Some(epoch);
        }
        if let Some(code) = current_code {
            let remaining = sync_code::EPOCH_SECS - (now % sync_code::EPOCH_SECS);
            on_code(code, epoch, remaining);
        }
        // H1 复审修复：每次请求前用当前时间重新构造 auth_header（刷新签名时间戳，
        // 重发/长轮询不因 >300s 偏差过期）。
        let msg1_auth = auth_header_at(
            (flow.cfg.now)(),
            flow.store,
            &local.device_id,
            "msg",
            &[&channel_id, "1", &b64url(&sha256_bytes(&msg1_blob))],
        )?;
        match retry_429(flow.backoff, || {
            flow.client
                .handshake(&channel_id, "sender", msg1_body.clone(), &msg1_auth)
        })
        .await
        {
            Ok(resp) => {
                if let Some(peer_b64) = resp.peer_msg {
                    let blob = http_client::b64url_decode(&peer_b64)
                        .context("invalid peer handshake message")?;
                    msg2 = Some(blob);
                    tracing::debug!(channel = %channel_hash(&channel_id), "msg2 received");
                }
            }
            Err(e) if e.kind == ApiErrorKind::NotFound => {
                anyhow::bail!("channel is no longer available (revoked or expired)");
            }
            Err(e) => return Err(e.into()),
        }
        if msg2.is_none() {
            tokio::time::sleep(flow.cfg.poll_interval).await;
        }
    }

    // ── 5. 处理 msg2 → transport（强制核对 trusted 公钥）──
    let m2 = msg2.expect("msg2 present");
    session
        .read_message(&m2)
        .context("msg2 authentication failed")?;
    let transport = session
        .into_transport(Some(target.x_pub))
        .context("peer verification failed")?;
    drop(transport); // v1 控制消息（manifest）走 msg1，transport 通道不传数据
    tracing::info!(channel = %channel_hash(&channel_id), "handshake complete");

    // ── 6. 分片上传（签名绑定 ciphertext hash 与 part index）──
    // Medium-2 复审修复：上传自节流 ≤1 req/s，512 parts 不突发撞 60/min 限流。
    let total = parts.len();
    let mut throttler = PartThrottler::new(flow.cfg.min_part_interval);
    for (i, part) in parts.iter().enumerate() {
        throttler.pace().await;
        let ct_hash = b64url(&sha256_bytes(part));
        let upload_auth = auth_header_at(
            (flow.cfg.now)(),
            flow.store,
            &local.device_id,
            "upload",
            &[&channel_id, &i.to_string(), &ct_hash],
        )?;
        let upload_body = http_client::UploadPartBody {
            part_index: i as u64,
            ciphertext: b64url(part),
        };
        retry_429(flow.backoff, || {
            flow.client
                .upload_part(&channel_id, upload_body.clone(), &upload_auth)
        })
        .await
        .map_err(|e| anyhow::anyhow!("part {i} upload failed: {e}"))?;
        if (i + 1) % 16 == 0 || i + 1 == total {
            tracing::info!(channel = %channel_hash(&channel_id), part = i + 1, total, "upload progress");
        }
    }
    tracing::info!(channel = %channel_hash(&channel_id), parts = total, "all parts uploaded");

    // ── 7. 等待 receiver confirm：轮询 handshake 直到终态 404 ──
    let deadline = SystemTime::now() + flow.cfg.confirm_wait_timeout;
    loop {
        // H1：轮询同样每次刷新签名时间戳。
        let msg1_auth = auth_header_at(
            (flow.cfg.now)(),
            flow.store,
            &local.device_id,
            "msg",
            &[&channel_id, "1", &b64url(&sha256_bytes(&msg1_blob))],
        )?;
        // Low-3 复审修复：confirm 轮询统一包裹 retry_429（与 msg2 轮询一致），
        // 429 走退避而非直接失败；404（终态）单独分支。
        match retry_429(flow.backoff, || {
            flow.client
                .handshake(&channel_id, "sender", msg1_body.clone(), &msg1_auth)
        })
        .await
        {
            Ok(_) => {}
            Err(e) if e.kind == ApiErrorKind::NotFound => {
                tracing::info!(channel = %channel_hash(&channel_id), "channel reached terminal state");
                break;
            }
            Err(e) => return Err(e.into()),
        }
        if SystemTime::now() > deadline {
            anyhow::bail!("timed out waiting for receiver confirmation");
        }
        tokio::time::sleep(flow.cfg.poll_interval).await;
    }

    Ok(SenderOutcome {
        channel_id,
        parts_uploaded: total,
    })
}

/// receiver 主流程。`code_input` 为用户输入的同步码（掩码输入由 CLI 壳完成）。
pub async fn run_receiver(flow: ReceiverFlow<'_>, code_input: &str) -> Result<ReceiverOutcome> {
    let local = flow.local;

    // ── 1. 归一化 + lookup（miss/过期/撤销统一 404）──
    let code_norm = sync_code::normalize(code_input)?;
    let lookup = retry_429(flow.backoff, || flow.client.lookup(&code_norm))
        .await
        .map_err(|e| anyhow::anyhow!("sync code lookup failed: {e}"))?;
    let channel_id = lookup.channel_id;
    tracing::debug!(channel = %channel_hash(&channel_id), "channel located");

    // ── 2. join（签名绑定 channel/code_norm/自身公钥）──
    let join_body = http_client::JoinBody {
        code: code_norm.clone(),
        device_id: local.device_id.to_b64(),
        ed_pub: b64url(&local.ed_pub),
        x_pub: b64url(&local.x_pub),
    };
    let join_auth = auth_header_at(
        (flow.cfg.now)(),
        flow.store,
        &local.device_id,
        "join",
        &[
            &channel_id,
            &code_norm,
            &local.device_id.to_b64(),
            &b64url(&local.ed_pub),
            &b64url(&local.x_pub),
        ],
    )?;
    retry_429(flow.backoff, || {
        flow.client.join(&channel_id, join_body.clone(), &join_auth)
    })
    .await
    .map_err(|e| anyhow::anyhow!("join failed: {e}"))?;
    tracing::info!(channel = %channel_hash(&channel_id), "joined channel");

    // ── 3. 轮询 msg1（空 msg 拉取；签名绑定 sha256(空 payload)）──
    let local_x = flow.store.x25519_private()?;
    let start = SystemTime::now();
    let msg1_blob = loop {
        if start.elapsed().unwrap_or_default() > flow.cfg.start_timeout {
            anyhow::bail!("timed out waiting for sender handshake message");
        }
        // H1：每次请求前重新构造 auth_header（刷新签名时间戳）。
        let empty_auth = auth_header_at(
            (flow.cfg.now)(),
            flow.store,
            &local.device_id,
            "msg",
            &[&channel_id, "2", &b64url(&sha256_bytes(&[]))],
        )?;
        let pull_body = HandshakeBody { msg: None };
        match retry_429(flow.backoff, || {
            flow.client
                .handshake(&channel_id, "receiver", pull_body.clone(), &empty_auth)
        })
        .await
        {
            Ok(resp) => {
                if let Some(peer_b64) = resp.peer_msg {
                    let blob = http_client::b64url_decode(&peer_b64)
                        .context("invalid peer handshake message")?;
                    break blob;
                }
            }
            Err(e) if e.kind == ApiErrorKind::NotFound => {
                anyhow::bail!("channel is no longer available (revoked or expired)");
            }
            Err(e) => return Err(e.into()),
        }
        tokio::time::sleep(flow.cfg.poll_interval).await;
    };

    // ── 4. 从 trusted peers 匹配 sender：msg1 只能被 prologue 一致的会话解开 ──
    let mut matched: Option<(TrustedPeer, noise_session::HandshakeSession, Vec<u8>)> = None;
    for peer in &flow.peers.peers {
        let params = HandshakeParams {
            channel_id: channel_id.clone(),
            initiator: PeerBinding {
                device_id: peer.device_id.to_b64(),
                ed_pub: peer.ed_pub,
                x_pub: peer.x_pub,
            },
            responder: PeerBinding {
                device_id: local.device_id.to_b64(),
                ed_pub: local.ed_pub,
                x_pub: local.x_pub,
            },
        };
        let mut session = noise_session::responder_session(&params, &local_x)?;
        match session.read_message(&msg1_blob) {
            Ok(payload) => {
                matched = Some((peer.clone(), session, payload));
                break;
            }
            Err(_) => continue,
        }
    }
    let (peer, mut session, payload) =
        matched.ok_or_else(|| anyhow::anyhow!("sender is not a trusted device — aborting"))?;
    // H4 复审修复：channel_flow 显式要求完整 72B 布局（data key ‖ manifest hash
    // ‖ part count）；32B 纯 data key 形态返回错误，禁止对 payload 越界切片。
    if payload.len() != noise_session::MSG1_PAYLOAD_FULL_LEN {
        anyhow::bail!("handshake message has unexpected payload layout");
    }
    let manifest_hash: [u8; 32] = payload
        [noise_session::MSG1_PAYLOAD_MANIFEST_OFFSET..noise_session::MSG1_PAYLOAD_COUNT_OFFSET]
        .try_into()
        .expect("slice length checked above");
    let part_count = u64::from_be_bytes(
        payload[noise_session::MSG1_PAYLOAD_COUNT_OFFSET..noise_session::MSG1_PAYLOAD_FULL_LEN]
            .try_into()
            .expect("slice length checked above"),
    );

    // ── 5. 写 msg2（空 payload）→ ready，再强制核对进入 transport ──
    // M1 复审修复：写 msg2 前先完成 trusted 身份核对（msg1 可被任何知道本设备
    // 公钥的实体构造，不得在核对前向对端暴露 msg2）。
    session
        .verify_peer(peer.x_pub)
        .context("sender identity verification failed")?;
    let m2_blob = session.write_message(&[])?;
    let m2_auth = auth_header_at(
        (flow.cfg.now)(),
        flow.store,
        &local.device_id,
        "msg",
        &[&channel_id, "2", &b64url(&sha256_bytes(&m2_blob))],
    )?;
    let m2_body = HandshakeBody {
        msg: Some(b64url(&m2_blob)),
    };
    retry_429(flow.backoff, || {
        flow.client
            .handshake(&channel_id, "receiver", m2_body.clone(), &m2_auth)
    })
    .await
    .map_err(|e| anyhow::anyhow!("handshake msg2 failed: {e}"))?;
    // msg1 可被任何知道本设备公钥的实体构造：into_transport 的
    // expected_remote_static 必须与 trusted 记录一致，不符即失败。
    let transport = session
        .into_transport(Some(peer.x_pub))
        .context("sender identity verification failed")?;
    let data_key = transport
        .data_key()
        .context("handshake did not carry a data key")?;
    tracing::info!(channel = %channel_hash(&channel_id), "sender verified");

    // ── 6. 下载全部 part（预算先行）并逐 part 验 AES-GCM AAD ──
    if part_count > limits::MAX_PARTS_PER_CHANNEL as u64 {
        anyhow::bail!("channel advertises too many parts: {part_count}");
    }
    // H5 复审修复：404 重试预算绑定 channel 总超时（start_timeout），全部 part
    // 共享同一 deadline；429 走 Backoff/重试上限，与 404 预算分离。
    // 二轮复审观察 A（Sonnet）：deadline 还必须覆盖节流总时长——默认 1s 节流 ×
    // 512 parts ≈ 511s，若仅取 start_timeout(600s) 则 404 重试余量仅 ~89s；
    // 取 max(start_timeout, 2 × 节流总时长)（100% 余量），仍远小于服务端
    // ready TTL(3600s)。测试恒为 ZERO 节流 ⇒ 退化为 start_timeout，语义不变。
    let pacing_budget = flow
        .cfg
        .min_part_interval
        .saturating_mul(part_count.saturating_add(1) as u32);
    let download_deadline = SystemTime::now()
        + flow
            .cfg
            .start_timeout
            .max(pacing_budget.saturating_mul(2));
    // Medium-2 复审修复：下载全量 part 自节流 ≤1 req/s（60/min 限流窗口内
    // 512 parts 不突发），throttler 跨 part 共享。
    let mut throttler = PartThrottler::new(flow.cfg.min_part_interval);
    let mut plaintext =
        Vec::with_capacity((part_count as usize).saturating_mul(limits::MAX_PLAINTEXT_PART_BYTES));
    for i in 0..part_count {
        // 二轮复审修复（High-1）：download auth 改由重试函数内部每次刷新，
        // 404 长重试跨 >300s 不因签名时间戳过期而 401。
        let envelope =
            fetch_part_with_retry(&flow, &channel_id, i, download_deadline, &mut throttler)
                .await
                .map_err(|e| anyhow::anyhow!("part {i} download failed: {e}"))?;
        let aad = crypto::payload_aad(&channel_id, i, &manifest_hash)?;
        let chunk = crypto::open(data_key, &aad, &envelope)
            .with_context(|| format!("part {i} failed AES-GCM authentication"))?;
        plaintext.extend_from_slice(&chunk);
        if (i + 1) % 16 == 0 || i + 1 == part_count {
            tracing::info!(
                channel = %channel_hash(&channel_id),
                part = i + 1,
                total = part_count,
                "download progress"
            );
        }
    }
    // 明文整体性复核（manifest hash 已绑定 AAD；此处双保险）。
    if sha256_bytes(&plaintext) != manifest_hash {
        anyhow::bail!("payload integrity check failed");
    }
    let pkg: SyncPackage =
        rmp_serde::from_slice(&plaintext).context("invalid sync package payload")?;

    // ── 7. staging 全量落盘 → 全部成功才 commit ──
    // M2 复审修复：staging 置于 home 同卷（~/.peri/staging-<channel_hash>），
    // 目录 0700 / 文件 0600，保证 commit 的 rename 不跨卷（EXDEV）。
    let staging_dir = staging_dir_for(flow.home_dir, &channel_hash(&channel_id));
    create_staging_dir(&staging_dir)?;
    let writes = match stage_package(flow.home_dir, flow.cwd, &pkg, &staging_dir) {
        Ok(w) => w,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(e).context("staging failed; nothing was committed");
        }
    };
    if let Err(e) = commit_files(&writes) {
        let _ = std::fs::remove_dir_all(&staging_dir);
        return Err(e).context("commit failed; already-written files are left in place");
    }
    let _ = std::fs::remove_dir_all(&staging_dir);
    tracing::info!(channel = %channel_hash(&channel_id), files = writes.len(), "payload committed");

    // ── 8. confirm（终态幂等；404 不回滚已提交文件）──
    let confirm_auth = auth_header_at(
        (flow.cfg.now)(),
        flow.store,
        &local.device_id,
        "confirm",
        &[&channel_id],
    )?;
    match retry_429(flow.backoff, || {
        flow.client.confirm(&channel_id, &confirm_auth)
    })
    .await
    {
        Ok(()) => {}
        Err(e) if e.kind == ApiErrorKind::NotFound => {
            tracing::warn!(
                channel = %channel_hash(&channel_id),
                "confirm returned 404; local files are already committed"
            );
        }
        Err(e) => return Err(e.into()),
    }

    Ok(ReceiverOutcome {
        channel_id,
        files: writes.len(),
    })
}

// ─── 码注册（30s epoch、撞码重试 1 次）─────────────────────────────────────

/// 注册本 epoch 的同步码（撞码重试 1 次）。
///
/// H2 复审修复：遇 403（channel 已离开 created 状态，如 receiver 已 join）返回
/// `Ok(None)` 表示"码使命已结束"——调用方停止注册/展示码并继续轮询 msg2，
/// 而不是传播错误退出整个 send 流程。
async fn register_code_with_collision_retry(
    flow: &SenderFlow<'_>,
    channel_id: &str,
    epoch: u64,
) -> Result<Option<sync_code::SyncCode>> {
    let mut attempts = 0u32;
    loop {
        let code = sync_code::SyncCode::generate()?;
        let code_norm = code.normalized();
        let code_hash = b64url(&sha256_bytes(code_norm.as_bytes()));
        let body = http_client::RegisterCodeBody {
            code: code.display(),
            epoch,
        };
        let auth = auth_header_at(
            (flow.cfg.now)(),
            flow.store,
            &flow.local.device_id,
            "code",
            &[channel_id, &epoch.to_string(), &code_hash],
        )?;
        match retry_429(flow.backoff, || {
            flow.client.register_code(channel_id, body.clone(), &auth)
        })
        .await
        {
            Ok(_) => return Ok(Some(code)),
            Err(e) if e.kind == ApiErrorKind::Forbidden => {
                tracing::debug!(
                    channel = %channel_hash(channel_id),
                    "code registration ended (channel no longer in created state)"
                );
                return Ok(None);
            }
            Err(e)
                if e.kind == ApiErrorKind::Collision
                    && attempts < limits::CODE_COLLISION_MAX_RETRIES =>
            {
                attempts += 1;
                tracing::warn!("sync code collision; retrying with a fresh code");
            }
            Err(e) => return Err(e.into()),
        }
    }
}

// ─── 下载重试（429 退避 + 404 在 channel 总超时内持续重试）───────────────────

/// 下载单个 part。429 由 [`retry_429`] 处理（独立预算）；404 表示尚未上传，
/// 在 `deadline`（channel 总超时）内持续重试（H5：不设固定次数）。
///
/// 二轮复审修复（High-1）：`auth` 在每次重试前重新构造——下载期间签名时间戳
/// 必须随墙钟刷新，否则 404 长重试跨 >300s 后必被服务端 401 截断。
async fn fetch_part_with_retry(
    flow: &ReceiverFlow<'_>,
    channel_id: &str,
    part_index: u64,
    deadline: SystemTime,
    throttler: &mut PartThrottler,
) -> Result<Vec<u8>, ApiError> {
    loop {
        // Medium-2：每次请求前节流（≤1 req/s），避免 512 parts 突发撞 60/min。
        throttler.pace().await;
        let auth = auth_header_at(
            (flow.cfg.now)(),
            flow.store,
            &flow.local.device_id,
            "download",
            &[channel_id, &part_index.to_string()],
        )
        .map_err(|e| ApiError::new(ApiErrorKind::Transport, format!("auth: {e}")))?;
        match retry_429(flow.backoff, || {
            flow.client.download_part(channel_id, part_index, &auth)
        })
        .await
        {
            Ok(bytes) => {
                // C1：下载回来的 envelope 不得超 64KiB 密文上限（与 TS 同口径）。
                limits::validate_part_size(bytes.len()).map_err(|_| {
                    ApiError::new(
                        ApiErrorKind::TooLarge,
                        "downloaded part exceeds the 64 KiB limit",
                    )
                })?;
                return Ok(bytes);
            }
            Err(e) if e.kind == ApiErrorKind::NotFound && SystemTime::now() < deadline => {
                tracing::debug!(
                    part = part_index,
                    "part not available yet; retrying within channel timeout"
                );
                tokio::time::sleep(flow.cfg.poll_interval).await;
            }
            Err(e) => return Err(e),
        }
    }
}

// ─── 429 指数退避重试（Retry-After 优先）──────────────────────────────────

/// part 级请求节流器（Medium-2 复审修复）。
///
/// 服务端签名端点限流为 60/min（`limits.ts` 冻结值）；512 parts 全量传输若
/// 突发连发必吃满 429。客户端自节流保证任意 60s 窗口内请求 ≤60（≤1 req/s），
/// 429 仅在服务端滚动窗口边界抖动时兜底。
#[derive(Debug)]
pub(crate) struct PartThrottler {
    last_request: Option<SystemTime>,
    min_interval: Duration,
}

impl PartThrottler {
    pub(crate) fn new(min_interval: Duration) -> Self {
        Self {
            last_request: None,
            min_interval,
        }
    }

    /// 等待至距上次请求 ≥ `min_interval`。首次调用立即返回。
    pub(crate) async fn pace(&mut self) {
        if self.min_interval.is_zero() {
            return;
        }
        if let Some(last) = self.last_request {
            let elapsed = last.elapsed().unwrap_or_default();
            if elapsed < self.min_interval {
                let sleep = self.min_interval - elapsed;
                tokio::time::sleep(sleep).await;
            }
        }
        self.last_request = Some(SystemTime::now());
    }
}

async fn retry_429<T, F, Fut>(backoff: &dyn Backoff, mut op: F) -> Result<T, ApiError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ApiError>>,
{
    let mut attempt = 0u32;
    loop {
        match op().await {
            Err(e) if e.kind == ApiErrorKind::RateLimited => {
                attempt += 1;
                if attempt > http_client::MAX_429_RETRIES {
                    return Err(e);
                }
                let delay = backoff.delay(attempt, e.retry_after_secs);
                tracing::debug!(attempt, ?delay, "rate limited; backing off");
                tokio::time::sleep(delay).await;
            }
            other => return other,
        }
    }
}

// ─── staging / commit（TRAP 路径校验复用 writer）───────────────────────────

/// staging 目录：`~/.peri/staging-<channel_hash>`（home 同卷，M2：保证 commit
/// rename 不跨卷；channel_hash 为日志用 8-hex 表示）。
pub(crate) fn staging_dir_for(home: &Path, channel_hash: &str) -> PathBuf {
    home.join(".peri").join(format!("staging-{channel_hash}"))
}

/// 创建 staging 目录（unix 下 0700）。残留同名目录（上次崩溃遗留，内容从未
/// commit）先整体清理，避免混入本次暂存。父目录（`~/.peri`）递归创建。
pub(crate) fn create_staging_dir(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("cannot clear stale staging dir {}", path.display()))?;
    }
    #[cfg(unix)]
    let created = {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(path)
    };
    #[cfg(not(unix))]
    let created = std::fs::DirBuilder::new().recursive(true).create(path);
    created.with_context(|| format!("cannot create staging dir {}", path.display()))?;
    Ok(())
}

/// 暂存文件：staging 内路径 → 目标路径（commit 时原子 rename）。
#[derive(Debug)]
pub(crate) struct StagedFile {
    pub(crate) staged: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) backup: Option<PathBuf>,
}

/// 把 SyncPackage 全量写入 staging 目录（保持 writer 的语义与 TRAP 校验）。
/// 任一项失败即整体失败：不 commit、不 confirm。
pub(crate) fn stage_package(
    home: &Path,
    cwd: &Path,
    pkg: &SyncPackage,
    staging: &Path,
) -> Result<Vec<StagedFile>> {
    let mut out = Vec::new();
    if let Some(settings) = &pkg.items.settings {
        let staged = staging.join("settings.json");
        write_staged(&staged, settings.content.as_bytes())?;
        out.push(StagedFile {
            staged,
            target: home.join(".peri").join("settings.json"),
            backup: Some(home.join(".peri").join("settings.json.bak")),
        });
        if let Some(claude) = &settings.claude_content {
            let staged = staging.join("claude-settings.json");
            write_staged(&staged, claude.as_bytes())?;
            out.push(StagedFile {
                staged,
                target: home.join(".claude").join("settings.json"),
                backup: Some(home.join(".claude").join("settings.json.bak")),
            });
        }
    }
    if let Some(skills) = &pkg.items.skills {
        let skills_base = home.join(".claude").join("skills");
        for entry in &skills.files {
            out.push(stage_file(staging, &skills_base, entry, "skills")?);
        }
    }
    if let Some(mcp) = &pkg.items.mcp {
        if let Some(global) = &mcp.global {
            let staged = staging.join("mcp-global.json");
            write_staged(&staged, global.as_bytes())?;
            out.push(StagedFile {
                staged,
                target: home.join(".mcp.json"),
                backup: None,
            });
        }
        if let Some(project) = &mcp.project {
            let staged = staging.join("mcp-project.json");
            write_staged(&staged, project.as_bytes())?;
            out.push(StagedFile {
                staged,
                target: cwd.join(".mcp.json"),
                backup: None,
            });
        }
    }
    if let Some(plugins) = &pkg.items.plugins {
        let plugins_base = home.join(".claude").join("plugins").join("cache");
        for entry in &plugins.files {
            out.push(stage_file(staging, &plugins_base, entry, "plugins")?);
        }
    }
    Ok(out)
}

/// 单个文件入 staging：TRAP 路径校验针对真实目标 base（`writer::validate_and_resolve`）。
fn stage_file(
    staging: &Path,
    target_base: &Path,
    entry: &FileEntry,
    kind: &str,
) -> Result<StagedFile> {
    let target = writer::validate_and_resolve(target_base, &entry.path)
        .map_err(|_| anyhow::anyhow!("package contains an unsafe path"))?;
    let staged = staging.join(kind).join(&entry.path);
    write_staged(&staged, &entry.content)?;
    Ok(StagedFile {
        staged,
        target,
        backup: None,
    })
}

/// 写暂存文件（unix 下 0600；父目录 0700，M2）。
fn write_staged(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_private_dirs(parent)?;
    }
    #[cfg(unix)]
    let opened = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
    };
    #[cfg(not(unix))]
    let opened = {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
    };
    let mut file =
        opened.with_context(|| format!("cannot write staged file {}", path.display()))?;
    file.write_all(bytes)?;
    Ok(())
}

/// 创建暂存目录树；最深层目录在 unix 下收紧为 0700（staging 根已 0700，
/// 中间层在 0700 根内，外部不可达）。
fn create_private_dirs(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// 把 staging 文件逐个原子移动到目标路径；settings 语义保持 `.bak` 备份。
/// 中途失败不回滚已完成的文件（plan：失败不回滚已提交文件）。
pub(crate) fn commit_files(writes: &[StagedFile]) -> Result<()> {
    commit_files_with(writes, |from, to| std::fs::rename(from, to))
}

/// 同 [`commit_files`]，但移动操作可注入（测试注入 rename 失败验证回退）。
///
/// M2 复审修复：rename 失败（如跨卷 EXDEV 或目标被占用）回退 copy + 清理
/// staged 文件——staging 与目标同卷（home 下）后 rename 正常路径不跨卷。
pub(crate) fn commit_files_with(
    writes: &[StagedFile],
    rename: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    for w in writes {
        if let Some(parent) = w.target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(backup) = &w.backup
            && w.target.exists()
        {
            std::fs::copy(&w.target, backup)?;
        }
        match rename(&w.staged, &w.target) {
            Ok(()) => {}
            Err(_) => {
                // 回退路径：copy 非原子，仅用于 rename 不可用的情况。
                std::fs::copy(&w.staged, &w.target).with_context(|| {
                    format!(
                        "rename and copy fallback both failed for {}",
                        w.target.display()
                    )
                })?;
                let _ = std::fs::remove_file(&w.staged);
            }
        }
    }
    Ok(())
}

// ─── CLI 壳（读 stdin 的密码/码输入）───────────────────────────────────────

/// `peri sync send --to <device_id>` 入口。
pub async fn run_send_cli(
    server: &str,
    keystore_path: Option<&Path>,
    target_id: &str,
) -> Result<()> {
    http_client::validate_server_url(server, false)?;
    let client = ReqwestClient::new(server)?;
    let backoff = http_client::ExponentialBackoff;
    let (local, store, peers) = load_cli_identity(keystore_path)?;
    let target_id = DeviceId::from_b64(target_id)?;
    let target = peers
        .get(&target_id)
        .ok_or_else(|| anyhow::anyhow!("device {target_id} is not in trusted peers"))?;
    let home = dirs_next::home_dir().context("failed to determine home directory")?;
    let cwd = std::env::current_dir()?;
    let items = all_items();
    let cfg = FlowConfig::default();
    let flow = SenderFlow {
        client: &client,
        backoff: &backoff,
        store: store.as_ref(),
        local: &local,
        target,
        home_dir: &home,
        cwd: &cwd,
        items: &items,
        cfg: &cfg,
    };
    let outcome = run_sender(flow, |code, _epoch, remaining| {
        print!(
            "\rSync code: {}   (rotates in {remaining}s)   ",
            code.display()
        );
        let _ = std::io::stdout().flush();
    })
    .await?;
    println!();
    println!("Sent {} parts", outcome.parts_uploaded);
    Ok(())
}

/// `peri sync receive` 入口（掩码输入同步码）。
pub async fn run_receive_cli(server: &str, keystore_path: Option<&Path>) -> Result<()> {
    http_client::validate_server_url(server, false)?;
    let client = ReqwestClient::new(server)?;
    let backoff = http_client::ExponentialBackoff;
    let (local, store, peers) = load_cli_identity(keystore_path)?;
    let home = dirs_next::home_dir().context("failed to determine home directory")?;
    let cwd = std::env::current_dir()?;
    println!("Enter the sync code shown on the sender screen:");
    let code = rpassword::read_password().context("failed to read sync code")?;
    let cfg = FlowConfig::default();
    let flow = ReceiverFlow {
        client: &client,
        backoff: &backoff,
        store: store.as_ref(),
        local: &local,
        peers: &peers,
        home_dir: &home,
        cwd: &cwd,
        cfg: &cfg,
    };
    let outcome = run_receiver(flow, &code).await?;
    println!("Synced {} files", outcome.files);
    Ok(())
}

/// 打开本地身份与 keystore（`peri sync device init` 之后）。
fn load_cli_identity(
    keystore_path: Option<&Path>,
) -> Result<(DevicePublic, Box<dyn SecretStore>, TrustedPeers)> {
    let paths = device_cli::default_paths()?;
    let identity = device_cli::load_identity(&paths)?;
    let store = device_cli::open_device_store(keystore_path, &identity)?;
    let peers = device_cli::load_peers(&paths)?;
    Ok((identity, store, peers))
}

fn all_items() -> SyncItems {
    SyncItems {
        settings: Some(Default::default()),
        skills: Some(Default::default()),
        mcp: Some(Default::default()),
        plugins: Some(Default::default()),
    }
}

// ─── 工具 ──────────────────────────────────────────────────────────────────

fn random_channel_id() -> Result<String> {
    Ok(DeviceId::random()?.to_b64())
}

fn b64url(bytes: &[u8]) -> String {
    http_client::b64url(bytes)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, data);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// channel 的日志表示：SHA-256 前 8 hex（不泄露完整 channel ID）。
fn channel_hash(channel_id: &str) -> String {
    let h = sha256_bytes(channel_id.as_bytes());
    let mut s = String::with_capacity(8);
    for b in &h[..4] {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// 构造 `Authorization: PeriSig ...` 头；`now` 为当前墙钟（unix 秒）。
///
/// H1 复审修复：重发/轮询每次请求前以当前时间重新构造（刷新签名时间戳，
/// 服务端 ±300s 偏差窗口内永不因等待而过期）。
fn auth_header_at(
    now: u64,
    store: &dyn SecretStore,
    device_id: &DeviceId,
    op: &str,
    fields: &[&str],
) -> Result<String> {
    http_client::peri_sig_header(store, device_id, op, fields, now)
}
