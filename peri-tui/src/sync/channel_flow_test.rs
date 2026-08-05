#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use anyhow::Result;
    use async_trait::async_trait;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serial_test::serial;

    use crate::sync::channel_flow::{
        FlowConfig, ReceiverFlow, ReceiverOutcome, SenderFlow, SenderOutcome, commit_files,
        commit_files_with, create_staging_dir, run_receiver, run_sender, stage_package,
        staging_dir_for,
    };
    use crate::sync::crypto::{self, DataKey};
    use crate::sync::device::{DeviceId, DevicePublic, TrustedPeer, TrustedPeers};
    use crate::sync::http_client::{
        ApiClient, ApiError, ApiErrorKind, Backoff, CreateChannelBody, CreateChannelResponse,
        ExpiresAtResponse, HandshakeBody, HandshakeResponse, JoinBody, LookupResponse,
        RegisterCodeBody, StateResponse, UploadPartBody, UploadPartResponse,
    };
    use crate::sync::keystore::{KeyMaterial, SecretStore};
    use crate::sync::limits;
    use crate::sync::noise_session::{HandshakeParams, PeerBinding, initiator_session};
    use crate::sync::protocol::{
        FileEntry, FilesItem, McpItem, SettingsItem, SyncItems, SyncPackage,
    };
    use crate::sync::sync_code::{self, SyncCode};

    // ─── 测试基件 ────────────────────────────────────────────────────────────

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    enum MockState {
        #[default]
        Created,
        Paired,
        Ready,
        Transferring,
        Confirmed,
        Revoked,
        Expired,
    }

    fn state_str(s: MockState) -> String {
        match s {
            MockState::Created => "created".into(),
            MockState::Paired => "paired".into(),
            MockState::Ready => "ready".into(),
            MockState::Transferring => "transferring".into(),
            MockState::Confirmed => "confirmed".into(),
            MockState::Revoked => "revoked".into(),
            MockState::Expired => "expired".into(),
        }
    }

    #[derive(Default)]
    struct MockChannel {
        device_id: String,
        expected_device_id: String,
        receiver_device_id: Option<String>,
        state: MockState,
        msg1: Option<Vec<u8>>,
        msg2: Option<Vec<u8>>,
        parts: BTreeMap<u64, Vec<u8>>,
    }

    #[derive(Default)]
    struct MockInner {
        channel: Option<MockChannel>,
        codes: HashMap<String, String>,
        register_calls: u32,
        collision_on_register: bool,
        /// H2：第 N 次注册成功后模拟 receiver 加入（channel → Paired）。
        auto_join_after_registers: u32,
        rate_limit_remaining: u32,
        rate_limit_hits: u32,
        /// 二轮复审（Low-3）：channel 进入终态后，前 N 次 sender 角色 handshake
        /// 注入 429（模拟 confirm 轮询被限流；仅命中 sender 轮询，不影响 receiver）。
        rate_limit_sender_confirm: u32,
        tamper_part: Option<u64>,
        drop_downloads_after: Option<u32>,
        /// H5：前 N 次 download 返回 404（慢速上传，channel 状态不变）。
        slow_downloads: u32,
        download_calls: u32,
        /// H1：每次 handshake 请求的签名时间戳（auth header 第 3 段）。
        handshake_auth_ts: Vec<u64>,
        /// 二轮复审（High-1）：每次 download 请求的签名时间戳。
        download_auth_ts: Vec<u64>,
    }

    /// 内存版 Worker：忠实复刻 Slice 2 状态机关键语义（幂等/终态 404/撞码 409）。
    #[derive(Clone)]
    struct MockServer {
        inner: Arc<Mutex<MockInner>>,
    }

    impl MockServer {
        fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new(MockInner::default())),
            }
        }

        fn seed(
            &self,
            channel_id: &str,
            sender: &DevicePublic,
            receiver: &DevicePublic,
            code_norm: &str,
            msg1: Vec<u8>,
            parts: Vec<(u64, Vec<u8>)>,
        ) {
            let mut inner = self.inner.lock().unwrap();
            inner
                .codes
                .insert(code_norm.to_string(), channel_id.to_string());
            inner.channel = Some(MockChannel {
                device_id: sender.device_id.to_b64(),
                expected_device_id: receiver.device_id.to_b64(),
                receiver_device_id: Some(receiver.device_id.to_b64()),
                state: MockState::Paired,
                msg1: Some(msg1),
                msg2: None,
                parts: parts.into_iter().collect(),
            });
        }

        fn set_collision_on_register(&self) {
            self.inner.lock().unwrap().collision_on_register = true;
        }

        fn set_rate_limit(&self, n: u32) {
            self.inner.lock().unwrap().rate_limit_remaining = n;
        }

        fn set_rate_limit_sender_confirm(&self, n: u32) {
            self.inner.lock().unwrap().rate_limit_sender_confirm = n;
        }

        fn set_tamper_part(&self, part: u64) {
            self.inner.lock().unwrap().tamper_part = Some(part);
        }

        fn set_drop_downloads_after(&self, n: u32) {
            self.inner.lock().unwrap().drop_downloads_after = Some(n);
        }

        fn set_slow_downloads(&self, n: u32) {
            self.inner.lock().unwrap().slow_downloads = n;
        }

        fn set_auto_join_after_registers(&self, n: u32) {
            self.inner.lock().unwrap().auto_join_after_registers = n;
        }

        fn register_calls(&self) -> u32 {
            self.inner.lock().unwrap().register_calls
        }

        fn download_calls(&self) -> u32 {
            self.inner.lock().unwrap().download_calls
        }

        fn handshake_auth_ts(&self) -> Vec<u64> {
            self.inner.lock().unwrap().handshake_auth_ts.clone()
        }

        fn download_auth_ts(&self) -> Vec<u64> {
            self.inner.lock().unwrap().download_auth_ts.clone()
        }

        fn rate_limit_hits(&self) -> u32 {
            self.inner.lock().unwrap().rate_limit_hits
        }

        fn is_confirmed(&self) -> bool {
            self.inner
                .lock()
                .unwrap()
                .channel
                .as_ref()
                .map(|c| c.state == MockState::Confirmed)
                .unwrap_or(false)
        }

        fn take_rate_limit(&self) -> Option<ApiError> {
            let mut inner = self.inner.lock().unwrap();
            if inner.rate_limit_remaining > 0 {
                inner.rate_limit_remaining -= 1;
                inner.rate_limit_hits += 1;
                Some(ApiError::with_retry_after(
                    ApiErrorKind::RateLimited,
                    "RATE_LIMITED",
                    Some(0),
                ))
            } else {
                None
            }
        }
    }

    #[async_trait]
    impl ApiClient for MockServer {
        async fn create_channel(
            &self,
            body: CreateChannelBody,
            _auth: &str,
        ) -> Result<CreateChannelResponse, ApiError> {
            if let Some(e) = self.take_rate_limit() {
                return Err(e);
            }
            let mut inner = self.inner.lock().unwrap();
            if let Some(existing) = &inner.channel {
                if existing.device_id == body.device_id
                    && existing.expected_device_id == body.expected_device_id
                {
                    return Ok(CreateChannelResponse {
                        channel_id: body.channel_id,
                        expires_at: 1,
                    });
                }
                return Err(ApiError::new(ApiErrorKind::Conflict, "CONFLICT"));
            }
            inner.channel = Some(MockChannel {
                device_id: body.device_id,
                expected_device_id: body.expected_device_id,
                receiver_device_id: None,
                state: MockState::Created,
                msg1: None,
                msg2: None,
                parts: BTreeMap::new(),
            });
            Ok(CreateChannelResponse {
                channel_id: body.channel_id,
                expires_at: 1,
            })
        }

        async fn register_code(
            &self,
            channel_id: &str,
            body: RegisterCodeBody,
            _auth: &str,
        ) -> Result<ExpiresAtResponse, ApiError> {
            if let Some(e) = self.take_rate_limit() {
                return Err(e);
            }
            let mut inner = self.inner.lock().unwrap();
            inner.register_calls += 1;
            if inner.collision_on_register && inner.register_calls == 1 {
                return Err(ApiError::new(ApiErrorKind::Collision, "COLLISION"));
            }
            let code_norm = sync_code::normalize(&body.code)
                .map_err(|_| ApiError::new(ApiErrorKind::BadRequest, "BAD_REQUEST"))?;
            // H2：复刻 TS 语义——channel 离开 created 状态后注册码一律 403。
            if let Some(ch) = &inner.channel
                && !matches!(ch.state, MockState::Created)
            {
                return Err(ApiError::new(ApiErrorKind::Forbidden, "FORBIDDEN"));
            }
            if let Some(existing) = inner.codes.get(&code_norm) {
                if existing == channel_id {
                    return Ok(ExpiresAtResponse { expires_at: 1 });
                }
                return Err(ApiError::new(ApiErrorKind::Collision, "COLLISION"));
            }
            inner.codes.insert(code_norm, channel_id.to_string());
            // 注册成功后模拟 receiver 加入（H2 测试场景）。
            if inner.auto_join_after_registers > 0 {
                inner.auto_join_after_registers -= 1;
                if let Some(ch) = inner.channel.as_mut()
                    && ch.state == MockState::Created
                {
                    ch.state = MockState::Paired;
                    ch.receiver_device_id = Some("auto-joiner".to_string());
                }
            }
            Ok(ExpiresAtResponse { expires_at: 1 })
        }

        async fn lookup(&self, code_norm: &str) -> Result<LookupResponse, ApiError> {
            if let Some(e) = self.take_rate_limit() {
                return Err(e);
            }
            let inner = self.inner.lock().unwrap();
            match inner.codes.get(code_norm) {
                Some(cid) => Ok(LookupResponse {
                    channel_id: cid.clone(),
                    valid_until: 1,
                }),
                None => Err(ApiError::new(ApiErrorKind::NotFound, "NOT_FOUND")),
            }
        }

        async fn join(
            &self,
            _channel_id: &str,
            body: JoinBody,
            _auth: &str,
        ) -> Result<StateResponse, ApiError> {
            if let Some(e) = self.take_rate_limit() {
                return Err(e);
            }
            let mut inner = self.inner.lock().unwrap();
            let ch = inner
                .channel
                .as_mut()
                .ok_or_else(|| ApiError::new(ApiErrorKind::NotFound, "NOT_FOUND"))?;
            if matches!(
                ch.state,
                MockState::Confirmed | MockState::Revoked | MockState::Expired
            ) {
                return Err(ApiError::new(ApiErrorKind::NotFound, "NOT_FOUND"));
            }
            if ch.expected_device_id != body.device_id {
                return Err(ApiError::new(ApiErrorKind::Forbidden, "FORBIDDEN"));
            }
            match &ch.receiver_device_id {
                None => {
                    ch.receiver_device_id = Some(body.device_id.clone());
                    ch.state = MockState::Paired;
                    Ok(StateResponse {
                        state: "paired".into(),
                        expires_at: 1,
                    })
                }
                Some(existing) if existing == &body.device_id => Ok(StateResponse {
                    state: state_str(ch.state),
                    expires_at: 1,
                }),
                Some(_) => Err(ApiError::new(ApiErrorKind::Forbidden, "FORBIDDEN")),
            }
        }

        async fn handshake(
            &self,
            _channel_id: &str,
            role: &str,
            body: HandshakeBody,
            auth: &str,
        ) -> Result<HandshakeResponse, ApiError> {
            if let Some(e) = self.take_rate_limit() {
                return Err(e);
            }
            let mut inner = self.inner.lock().unwrap();
            // H1：记录每次请求的签名时间戳（PeriSig <id> <ts> <sig> 第 3 段）。
            let ts = auth
                .split_whitespace()
                .nth(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            inner.handshake_auth_ts.push(ts);
            // 二轮复审（Low-3）：channel 已终态后，sender 的 confirm 轮询注入 429
            // （须先于下方终态 404 判定，否则轮询只会看到 404）。只命中 sender 角色，
            // receiver 的请求不受影响。
            if role == "sender"
                && inner.rate_limit_sender_confirm > 0
                && let Some(ch) = &inner.channel
                && matches!(
                    ch.state,
                    MockState::Confirmed | MockState::Revoked | MockState::Expired
                )
            {
                inner.rate_limit_sender_confirm -= 1;
                inner.rate_limit_hits += 1;
                return Err(ApiError::with_retry_after(
                    ApiErrorKind::RateLimited,
                    "RATE_LIMITED",
                    Some(0),
                ));
            }
            let ch = inner
                .channel
                .as_mut()
                .ok_or_else(|| ApiError::new(ApiErrorKind::NotFound, "NOT_FOUND"))?;
            if matches!(
                ch.state,
                MockState::Confirmed | MockState::Revoked | MockState::Expired
            ) {
                return Err(ApiError::new(ApiErrorKind::NotFound, "NOT_FOUND"));
            }
            if role == "sender" {
                if let Some(msg) = &body.msg {
                    let blob = URL_SAFE_NO_PAD
                        .decode(msg)
                        .map_err(|_| ApiError::new(ApiErrorKind::BadRequest, "BAD_REQUEST"))?;
                    match &ch.msg1 {
                        None => ch.msg1 = Some(blob),
                        Some(existing) if existing != &blob => {
                            return Err(ApiError::new(ApiErrorKind::Conflict, "CONFLICT"));
                        }
                        Some(_) => {}
                    }
                }
                let peer_msg = ch.msg2.as_ref().map(|m| URL_SAFE_NO_PAD.encode(m));
                Ok(HandshakeResponse {
                    peer_msg,
                    state: state_str(ch.state),
                    expires_at: 1,
                })
            } else {
                if ch.receiver_device_id.is_none() {
                    return Err(ApiError::new(ApiErrorKind::Forbidden, "FORBIDDEN"));
                }
                if let Some(msg) = &body.msg {
                    let blob = URL_SAFE_NO_PAD
                        .decode(msg)
                        .map_err(|_| ApiError::new(ApiErrorKind::BadRequest, "BAD_REQUEST"))?;
                    match &ch.msg2 {
                        None => ch.msg2 = Some(blob),
                        Some(existing) if existing != &blob => {
                            return Err(ApiError::new(ApiErrorKind::Conflict, "CONFLICT"));
                        }
                        Some(_) => {}
                    }
                    if ch.msg1.is_some() && ch.msg2.is_some() && ch.state == MockState::Paired {
                        ch.state = MockState::Ready;
                    }
                }
                let peer_msg = ch.msg1.as_ref().map(|m| URL_SAFE_NO_PAD.encode(m));
                Ok(HandshakeResponse {
                    peer_msg,
                    state: state_str(ch.state),
                    expires_at: 1,
                })
            }
        }

        async fn upload_part(
            &self,
            _channel_id: &str,
            body: UploadPartBody,
            _auth: &str,
        ) -> Result<UploadPartResponse, ApiError> {
            if let Some(e) = self.take_rate_limit() {
                return Err(e);
            }
            let mut inner = self.inner.lock().unwrap();
            let ch = inner
                .channel
                .as_mut()
                .ok_or_else(|| ApiError::new(ApiErrorKind::NotFound, "NOT_FOUND"))?;
            if !matches!(ch.state, MockState::Ready | MockState::Transferring) {
                return Err(ApiError::new(ApiErrorKind::Forbidden, "FORBIDDEN"));
            }
            let data = URL_SAFE_NO_PAD
                .decode(&body.ciphertext)
                .map_err(|_| ApiError::new(ApiErrorKind::BadRequest, "BAD_REQUEST"))?;
            // C1：复刻 TS channel-do 的预算校验——
            // `ct.length > maxPartBytes → 413`；`total_bytes + ct.length > maxPayloadBytes → 413`。
            if data.len() > limits::MAX_PART_BYTES {
                return Err(ApiError::new(ApiErrorKind::TooLarge, "TOO_LARGE"));
            }
            let uploaded: usize = ch.parts.values().map(|p| p.len()).sum();
            if uploaded + data.len() > limits::MAX_PAYLOAD_BYTES {
                return Err(ApiError::new(ApiErrorKind::TooLarge, "TOO_LARGE"));
            }
            match ch.parts.get(&body.part_index) {
                Some(existing) if existing != &data => {
                    return Err(ApiError::new(ApiErrorKind::Conflict, "CONFLICT"));
                }
                Some(_) => {}
                None => {
                    ch.parts.insert(body.part_index, data);
                }
            }
            if ch.state == MockState::Ready {
                ch.state = MockState::Transferring;
            }
            Ok(UploadPartResponse {
                part_index: body.part_index,
                size: body.ciphertext.len() as u64,
            })
        }

        async fn download_part(
            &self,
            _channel_id: &str,
            part_index: u64,
            auth: &str,
        ) -> Result<Vec<u8>, ApiError> {
            if let Some(e) = self.take_rate_limit() {
                return Err(e);
            }
            let mut inner = self.inner.lock().unwrap();
            // 二轮复审（High-1）：记录每次 download 请求的签名时间戳。
            let ts = auth
                .split_whitespace()
                .nth(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            inner.download_auth_ts.push(ts);
            inner.download_calls += 1;
            // 慢速上传：前 N 次调用返回 404（channel 状态不变；H5 场景）。
            if inner.slow_downloads > 0 {
                inner.slow_downloads -= 1;
                return Err(ApiError::new(ApiErrorKind::NotFound, "NOT_FOUND"));
            }
            // 下载中断：超过阈值后 channel 置终态，此后一律 404（先于借用检查）。
            if let Some(limit) = inner.drop_downloads_after
                && inner.download_calls > limit
            {
                if let Some(ch) = inner.channel.as_mut() {
                    ch.state = MockState::Expired;
                }
                return Err(ApiError::new(ApiErrorKind::NotFound, "NOT_FOUND"));
            }
            let ch = inner
                .channel
                .as_mut()
                .ok_or_else(|| ApiError::new(ApiErrorKind::NotFound, "NOT_FOUND"))?;
            if matches!(
                ch.state,
                MockState::Confirmed | MockState::Revoked | MockState::Expired
            ) {
                return Err(ApiError::new(ApiErrorKind::NotFound, "NOT_FOUND"));
            }
            if !matches!(ch.state, MockState::Ready | MockState::Transferring) {
                return Err(ApiError::new(ApiErrorKind::Forbidden, "FORBIDDEN"));
            }
            let mut data = ch
                .parts
                .get(&part_index)
                .cloned()
                .ok_or_else(|| ApiError::new(ApiErrorKind::NotFound, "NOT_FOUND"))?;
            if inner.tamper_part == Some(part_index)
                && let Some(last) = data.last_mut()
            {
                *last ^= 0x01;
            }
            Ok(data)
        }

        async fn confirm(&self, _channel_id: &str, _auth: &str) -> Result<(), ApiError> {
            if let Some(e) = self.take_rate_limit() {
                return Err(e);
            }
            let mut inner = self.inner.lock().unwrap();
            let ch = inner
                .channel
                .as_mut()
                .ok_or_else(|| ApiError::new(ApiErrorKind::NotFound, "NOT_FOUND"))?;
            match ch.state {
                MockState::Confirmed => Ok(()),
                MockState::Ready | MockState::Transferring => {
                    ch.state = MockState::Confirmed;
                    Ok(())
                }
                _ => Err(ApiError::new(ApiErrorKind::NotFound, "NOT_FOUND")),
            }
        }

        async fn revoke(&self, _channel_id: &str, _auth: &str) -> Result<(), ApiError> {
            if let Some(e) = self.take_rate_limit() {
                return Err(e);
            }
            let mut inner = self.inner.lock().unwrap();
            let ch = inner
                .channel
                .as_mut()
                .ok_or_else(|| ApiError::new(ApiErrorKind::NotFound, "NOT_FOUND"))?;
            ch.state = MockState::Revoked;
            Ok(())
        }
    }

    /// 内存 SecretStore（与 http_client_test 相同形态）。
    struct TestStore {
        material: KeyMaterial,
    }

    impl SecretStore for TestStore {
        fn sign(&self, msg: &[u8]) -> Result<ed25519_dalek::Signature> {
            use ed25519_dalek::Signer;
            Ok(self.material.ed25519.sign(msg))
        }

        fn x25519_private(&self) -> Result<x25519_dalek::StaticSecret> {
            Ok(self.material.x25519.clone())
        }
    }

    /// 零等待退避（测试注入；429 立即重试）。
    struct ZeroBackoff;

    impl Backoff for ZeroBackoff {
        fn delay(&self, _attempt: u32, _retry_after_secs: Option<u64>) -> Duration {
            Duration::ZERO
        }
    }

    /// 记录 delay 调用次数（验证退避确实发生）。
    #[derive(Clone, Default)]
    struct RecordingBackoff {
        calls: Arc<Mutex<u32>>,
    }

    impl Backoff for RecordingBackoff {
        fn delay(&self, _attempt: u32, _retry_after_secs: Option<u64>) -> Duration {
            *self.calls.lock().unwrap() += 1;
            Duration::ZERO
        }
    }

    fn test_cfg() -> FlowConfig {
        FlowConfig {
            poll_interval: Duration::from_millis(5),
            start_timeout: Duration::from_secs(5),
            confirm_wait_timeout: Duration::from_secs(5),
            min_part_interval: Duration::ZERO,
            ..Default::default()
        }
    }

    /// 可控时钟的 FlowConfig（H1/H2 测试：推进时钟模拟时间流逝）。
    fn clocked_cfg(
        start_timeout: Duration,
        start_ts: u64,
    ) -> (FlowConfig, std::sync::Arc<std::sync::atomic::AtomicU64>) {
        use std::sync::atomic::{AtomicU64, Ordering};
        let clock = std::sync::Arc::new(AtomicU64::new(start_ts));
        let cfg = FlowConfig {
            poll_interval: Duration::from_millis(5),
            start_timeout,
            confirm_wait_timeout: Duration::from_secs(5),
            min_part_interval: Duration::ZERO,
            now: {
                let c = clock.clone();
                Box::new(move || c.load(Ordering::Relaxed))
            },
        };
        (cfg, clock)
    }

    /// 推进可控时钟（H1/H2）。
    fn advance_clock(clock: &std::sync::Arc<std::sync::atomic::AtomicU64>, to: u64) {
        use std::sync::atomic::Ordering;
        clock.store(to, Ordering::Relaxed);
    }

    async fn wait_until(mut cond: impl FnMut() -> bool) {
        for _ in 0..2000 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("condition not met in time");
    }

    fn all_items() -> SyncItems {
        SyncItems {
            settings: Some(Default::default()),
            skills: Some(Default::default()),
            mcp: Some(Default::default()),
            plugins: Some(Default::default()),
        }
    }

    fn binding_for(d: &DevicePublic) -> PeerBinding {
        PeerBinding {
            device_id: d.device_id.to_b64(),
            ed_pub: d.ed_pub,
            x_pub: d.x_pub,
        }
    }

    /// 构造 sender 侧 msg1（完整 72B payload 布局）。
    fn build_msg1(
        sender_material: &KeyMaterial,
        sender: &DevicePublic,
        receiver: &DevicePublic,
        channel_id: &str,
        data_key: &DataKey,
        manifest_hash: &[u8; 32],
        part_count: u64,
    ) -> Vec<u8> {
        let params = HandshakeParams {
            channel_id: channel_id.to_string(),
            initiator: binding_for(sender),
            responder: binding_for(receiver),
        };
        let local_x = sender_material.x25519.clone();
        let mut session = initiator_session(&params, &local_x).unwrap();
        let mut payload = Vec::with_capacity(noise_session_full_len());
        payload.extend_from_slice(data_key.as_array());
        payload.extend_from_slice(manifest_hash);
        payload.extend_from_slice(&part_count.to_be_bytes());
        session.write_message(&payload).unwrap()
    }

    fn noise_session_full_len() -> usize {
        crate::sync::noise_session::MSG1_PAYLOAD_FULL_LEN
    }

    /// 打包明文 → (msg1, parts)。分片上限与实现一致（C1：明文 65507B/片）。
    fn seal_plaintext(
        sender_material: &KeyMaterial,
        sender: &DevicePublic,
        receiver: &DevicePublic,
        channel_id: &str,
        plaintext: &[u8],
    ) -> (Vec<u8>, Vec<(u64, Vec<u8>)>) {
        let manifest_hash = sha256_test(plaintext);
        let data_key = DataKey::random().unwrap();
        let part_count = plaintext.chunks(limits::MAX_PLAINTEXT_PART_BYTES).count() as u64;
        let parts = plaintext
            .chunks(limits::MAX_PLAINTEXT_PART_BYTES)
            .enumerate()
            .map(|(i, c)| {
                let aad = crypto::payload_aad(channel_id, i as u64, &manifest_hash).unwrap();
                (i as u64, crypto::seal(&data_key, &aad, c))
            })
            .collect();
        let msg1 = build_msg1(
            sender_material,
            sender,
            receiver,
            channel_id,
            &data_key,
            &manifest_hash,
            part_count,
        );
        (msg1, parts)
    }

    // ─── happy path fixture ──────────────────────────────────────────────────

    struct Fixture {
        mock: MockServer,
        code: String,
        sender_outcome: SenderOutcome,
        receiver_outcome: ReceiverOutcome,
        receiver_home: PathBuf,
        _tmp: tempfile::TempDir,
    }

    /// 完整双端流程：真实 Noise + 真实 crypto + mock HTTP。
    async fn run_happy_path_fixture() -> Fixture {
        run_happy_path_fixture_with(20_000).await
    }

    /// 同 [`run_happy_path_fixture`]，但可指定 SKILL.md 大小（行数）。
    async fn run_happy_path_fixture_with(skill_lines: usize) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let sender_home = tmp.path().join("sender-home");
        let receiver_home = tmp.path().join("receiver-home");
        let cwd = tmp.path().join("cwd");
        std::fs::create_dir_all(&sender_home).unwrap();
        std::fs::create_dir_all(&receiver_home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        std::fs::create_dir_all(sender_home.join(".peri")).unwrap();
        std::fs::write(
            sender_home.join(".peri/settings.json"),
            r#"{"marker":"SENSITIVE_MARKER_abc"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(sender_home.join(".claude/skills/demo")).unwrap();
        // ~140 KB → 3 parts（覆盖 64KiB 分片路径）；大尺寸版本覆盖 512 parts 满预算。
        std::fs::write(
            sender_home.join(".claude/skills/demo/SKILL.md"),
            "# Demo\n".repeat(skill_lines),
        )
        .unwrap();

        let sender_mat = KeyMaterial::generate().unwrap();
        let receiver_mat = KeyMaterial::generate().unwrap();
        let sender_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            sender_mat.ed25519_public(),
            sender_mat.x25519_public(),
            "sender",
        )
        .unwrap();
        let receiver_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            receiver_mat.ed25519_public(),
            receiver_mat.x25519_public(),
            "receiver",
        )
        .unwrap();
        let target_peer = TrustedPeer::from_device(&receiver_ident, 1);
        let mut receiver_peers = TrustedPeers::default();
        receiver_peers
            .add(TrustedPeer::from_device(&sender_ident, 1))
            .unwrap();

        let mock = MockServer::new();
        let backoff = ZeroBackoff;
        let sender_store = TestStore {
            material: sender_mat,
        };
        let receiver_store = TestStore {
            material: receiver_mat,
        };
        let cfg = test_cfg();
        let items = all_items();

        let code_holder = Arc::new(Mutex::new(None::<String>));
        let code_cb = {
            let ch = code_holder.clone();
            move |code: SyncCode, _epoch: u64, _remaining: u64| {
                *ch.lock().unwrap() = Some(code.display());
            }
        };
        let wait_code = code_holder.clone();

        let (sender_res, receiver_res) = tokio::join!(
            run_sender(
                SenderFlow {
                    client: &mock,
                    backoff: &backoff,
                    store: &sender_store,
                    local: &sender_ident,
                    target: &target_peer,
                    home_dir: &sender_home,
                    cwd: &cwd,
                    items: &items,
                    cfg: &cfg,
                },
                code_cb,
            ),
            async {
                let code = loop {
                    if let Some(c) = wait_code.lock().unwrap().clone() {
                        break c;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                };
                // 小写 + 连字符输入 → 客户端归一化。
                let code_lower = code.to_lowercase();
                run_receiver(
                    ReceiverFlow {
                        client: &mock,
                        backoff: &backoff,
                        store: &receiver_store,
                        local: &receiver_ident,
                        peers: &receiver_peers,
                        home_dir: &receiver_home,
                        cwd: &cwd,
                        cfg: &cfg,
                    },
                    &code_lower,
                )
                .await
            }
        );

        let sender_outcome = sender_res.expect("sender flow 应成功");
        let receiver_outcome = receiver_res.expect("receiver flow 应成功");
        let code = code_holder.lock().unwrap().clone().expect("code captured");
        Fixture {
            mock,
            code,
            sender_outcome,
            receiver_outcome,
            receiver_home,
            _tmp: tmp,
        }
    }

    /// 预置 channel（Paired + msg1 + parts），只跑 receiver 的种子场景。
    struct Seeded {
        mock: MockServer,
        code_norm: String,
        receiver_ident: DevicePublic,
        receiver_peers: TrustedPeers,
        receiver_store: TestStore,
        receiver_home: PathBuf,
        cwd: PathBuf,
        _tmp: tempfile::TempDir,
    }

    fn seed_receiver_scenario(receiver_trusts_sender: bool, plaintext: &[u8]) -> Seeded {
        let tmp = tempfile::tempdir().unwrap();
        let receiver_home = tmp.path().join("receiver-home");
        let cwd = tmp.path().join("cwd");
        std::fs::create_dir_all(&receiver_home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        let sender_mat = KeyMaterial::generate().unwrap();
        let receiver_mat = KeyMaterial::generate().unwrap();
        let sender_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            sender_mat.ed25519_public(),
            sender_mat.x25519_public(),
            "sender",
        )
        .unwrap();
        let receiver_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            receiver_mat.ed25519_public(),
            receiver_mat.x25519_public(),
            "receiver",
        )
        .unwrap();

        let code = SyncCode::generate().unwrap();
        let code_norm = code.normalized();
        let channel_id = DeviceId::random().unwrap().to_b64();
        let (msg1, parts) = seal_plaintext(
            &sender_mat,
            &sender_ident,
            &receiver_ident,
            &channel_id,
            plaintext,
        );

        let mock = MockServer::new();
        mock.seed(
            &channel_id,
            &sender_ident,
            &receiver_ident,
            &code_norm,
            msg1,
            parts,
        );
        let mut receiver_peers = TrustedPeers::default();
        if receiver_trusts_sender {
            receiver_peers
                .add(TrustedPeer::from_device(&sender_ident, 1))
                .unwrap();
        }
        Seeded {
            mock,
            code_norm,
            receiver_ident,
            receiver_peers,
            receiver_store: TestStore {
                material: receiver_mat,
            },
            receiver_home,
            cwd,
            _tmp: tmp,
        }
    }

    fn small_plaintext_package() -> Vec<u8> {
        let pkg = SyncPackage {
            version: 1,
            timestamp: 1,
            items: SyncItems {
                settings: Some(SettingsItem {
                    content: r#"{"k":"v"}"#.into(),
                    claude_content: None,
                }),
                skills: None,
                mcp: None,
                plugins: None,
            },
        };
        rmp_serde::to_vec(&pkg).unwrap()
    }

    /// 测试本地 SHA-256（与 channel_flow 实现一致）。
    fn sha256_test(data: &[u8]) -> [u8; 32] {
        let digest = ring::digest::digest(&ring::digest::SHA256, data);
        let mut out = [0u8; 32];
        out.copy_from_slice(digest.as_ref());
        out
    }

    // ─── 测试 ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_happy_path_end_to_end() {
        let fixture = run_happy_path_fixture().await;
        // 3 parts（140KB / 64KiB）上传；2 个文件落盘（settings + SKILL.md）。
        assert_eq!(fixture.sender_outcome.parts_uploaded, 3);
        assert_eq!(fixture.receiver_outcome.files, 2);
        // 接收端文件内容一致。
        let settings =
            std::fs::read_to_string(fixture.receiver_home.join(".peri/settings.json")).unwrap();
        assert!(settings.contains("SENSITIVE_MARKER_abc"));
        let skill =
            std::fs::read_to_string(fixture.receiver_home.join(".claude/skills/demo/SKILL.md"))
                .unwrap();
        assert!(skill.starts_with("# Demo"));
        // channel 终态 confirmed。
        assert!(fixture.mock.is_confirmed());
    }

    #[tokio::test]
    async fn test_code_collision_retries_once() {
        let tmp = tempfile::tempdir().unwrap();
        let sender_home = tmp.path().join("sender-home");
        let cwd = tmp.path().join("cwd");
        std::fs::create_dir_all(&sender_home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        let sender_mat = KeyMaterial::generate().unwrap();
        let receiver_mat = KeyMaterial::generate().unwrap();
        let sender_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            sender_mat.ed25519_public(),
            sender_mat.x25519_public(),
            "sender",
        )
        .unwrap();
        let receiver_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            receiver_mat.ed25519_public(),
            receiver_mat.x25519_public(),
            "receiver",
        )
        .unwrap();
        let target_peer = TrustedPeer::from_device(&receiver_ident, 1);

        let mock = MockServer::new();
        mock.set_collision_on_register();
        let backoff = ZeroBackoff;
        let store = TestStore {
            material: sender_mat,
        };
        let items = all_items();
        let mut cfg = test_cfg();
        cfg.start_timeout = Duration::from_millis(300); // 无人 join，等待超时是预期

        let result = run_sender(
            SenderFlow {
                client: &mock,
                backoff: &backoff,
                store: &store,
                local: &sender_ident,
                target: &target_peer,
                home_dir: &sender_home,
                cwd: &cwd,
                items: &items,
                cfg: &cfg,
            },
            |_code, _epoch, _remaining| {},
        )
        .await;
        // 无人 join → 超时退出；但撞码必须已重试一次（共 2 次注册调用）。
        assert!(result.is_err());
        assert_eq!(mock.register_calls(), 2, "撞码必须恰好重试 1 次");
    }

    #[tokio::test]
    async fn test_rate_limited_429_backs_off_and_retries() {
        let seeded = seed_receiver_scenario(true, &small_plaintext_package());
        seeded.mock.set_rate_limit(2); // 前 2 个请求 429
        let backoff = RecordingBackoff::default();
        let cfg = test_cfg();
        let flow = ReceiverFlow {
            client: &seeded.mock,
            backoff: &backoff,
            store: &seeded.receiver_store,
            local: &seeded.receiver_ident,
            peers: &seeded.receiver_peers,
            home_dir: &seeded.receiver_home,
            cwd: &seeded.cwd,
            cfg: &cfg,
        };
        let outcome = run_receiver(flow, &seeded.code_norm)
            .await
            .expect("429 退避后应成功");
        assert_eq!(outcome.files, 1);
        assert_eq!(seeded.mock.rate_limit_hits(), 2);
        assert_eq!(*backoff.calls.lock().unwrap(), 2, "每次 429 都必须调用退避");
        assert!(seeded.mock.is_confirmed());
    }

    #[tokio::test]
    async fn test_download_interrupted_aborts_before_commit() {
        let seeded = seed_receiver_scenario(true, &small_plaintext_package());
        // 第一次 download 即返回 404 且 channel 置终态（模拟清理/过期）。
        seeded.mock.set_drop_downloads_after(0);
        let backoff = ZeroBackoff;
        let cfg = test_cfg();
        let flow = ReceiverFlow {
            client: &seeded.mock,
            backoff: &backoff,
            store: &seeded.receiver_store,
            local: &seeded.receiver_ident,
            peers: &seeded.receiver_peers,
            home_dir: &seeded.receiver_home,
            cwd: &seeded.cwd,
            cfg: &cfg,
        };
        let err = run_receiver(flow, &seeded.code_norm).await.unwrap_err();
        assert!(
            err.to_string().contains("download failed"),
            "下载中断必须报错: {err}"
        );
        // 未 commit（目标文件不存在）且未 confirm。
        assert!(!seeded.receiver_home.join(".peri/settings.json").exists());
        assert!(!seeded.mock.is_confirmed());
    }

    #[tokio::test]
    async fn test_aad_tamper_aborts_before_commit() {
        let seeded = seed_receiver_scenario(true, &small_plaintext_package());
        seeded.mock.set_tamper_part(0); // part 0 密文被篡改
        let backoff = ZeroBackoff;
        let cfg = test_cfg();
        let flow = ReceiverFlow {
            client: &seeded.mock,
            backoff: &backoff,
            store: &seeded.receiver_store,
            local: &seeded.receiver_ident,
            peers: &seeded.receiver_peers,
            home_dir: &seeded.receiver_home,
            cwd: &seeded.cwd,
            cfg: &cfg,
        };
        let err = run_receiver(flow, &seeded.code_norm).await.unwrap_err();
        assert!(
            err.to_string().contains("AES-GCM"),
            "AAD 篡改必须认证失败: {err}"
        );
        // 未 commit 未 confirm。
        assert!(!seeded.receiver_home.join(".peri/settings.json").exists());
        assert!(!seeded.mock.is_confirmed());
    }

    #[tokio::test]
    async fn test_receiver_rejects_untrusted_sender() {
        // receiver 的 trusted peers 不含 sender → 必须中止（identity 核对）。
        let seeded = seed_receiver_scenario(false, &small_plaintext_package());
        let backoff = ZeroBackoff;
        let cfg = test_cfg();
        let flow = ReceiverFlow {
            client: &seeded.mock,
            backoff: &backoff,
            store: &seeded.receiver_store,
            local: &seeded.receiver_ident,
            peers: &seeded.receiver_peers,
            home_dir: &seeded.receiver_home,
            cwd: &seeded.cwd,
            cfg: &cfg,
        };
        let err = run_receiver(flow, &seeded.code_norm).await.unwrap_err();
        assert!(
            err.to_string().contains("not a trusted device"),
            "非 trusted sender 必须中止: {err}"
        );
        // M1：任何路径下都不得向未验证的对端写入 msg2。
        assert!(
            seeded
                .mock
                .inner
                .lock()
                .unwrap()
                .channel
                .as_ref()
                .unwrap()
                .msg2
                .is_none(),
            "untrusted sender 不得收到 msg2"
        );
        assert!(!seeded.mock.is_confirmed());
    }

    #[tokio::test]
    async fn test_sender_times_out_waiting_for_receiver() {
        let tmp = tempfile::tempdir().unwrap();
        let sender_home = tmp.path().join("sender-home");
        let cwd = tmp.path().join("cwd");
        std::fs::create_dir_all(&sender_home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(sender_home.join(".peri")).unwrap();
        std::fs::write(sender_home.join(".peri/settings.json"), "{}").unwrap();

        let sender_mat = KeyMaterial::generate().unwrap();
        let receiver_mat = KeyMaterial::generate().unwrap();
        let sender_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            sender_mat.ed25519_public(),
            sender_mat.x25519_public(),
            "sender",
        )
        .unwrap();
        let receiver_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            receiver_mat.ed25519_public(),
            receiver_mat.x25519_public(),
            "receiver",
        )
        .unwrap();
        let target_peer = TrustedPeer::from_device(&receiver_ident, 1);

        let mock = MockServer::new();
        let backoff = ZeroBackoff;
        let store = TestStore {
            material: sender_mat,
        };
        let items = all_items();
        let mut cfg = test_cfg();
        cfg.start_timeout = Duration::from_millis(300);

        let result = run_sender(
            SenderFlow {
                client: &mock,
                backoff: &backoff,
                store: &store,
                local: &sender_ident,
                target: &target_peer,
                home_dir: &sender_home,
                cwd: &cwd,
                items: &items,
                cfg: &cfg,
            },
            |_code, _epoch, _remaining| {},
        )
        .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("timed out waiting"),
            "等待 receiver 必须超时: {err}"
        );
    }

    // ─── staging / commit 单元测试（commit 前 abort 路径）───────────────────

    #[test]
    fn test_stage_rejects_unsafe_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cwd = tmp.path().join("cwd");
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();

        // 路径穿越（..）与绝对路径都必须被 TRAP 校验拒绝。
        for bad in ["../evil", "/etc/passwd", "a/../../b"] {
            let pkg = SyncPackage {
                version: 1,
                timestamp: 1,
                items: SyncItems {
                    settings: None,
                    skills: Some(FilesItem {
                        files: vec![FileEntry {
                            path: bad.into(),
                            content: b"x".to_vec(),
                        }],
                    }),
                    mcp: None,
                    plugins: None,
                },
            };
            let err = stage_package(&home, &cwd, &pkg, &staging)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("unsafe path"),
                "非法路径必须被拒绝: {err}（path={bad}）"
            );
        }
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
    }

    #[test]
    fn test_stage_and_commit_files_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cwd = tmp.path().join("cwd");
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();

        let pkg = SyncPackage {
            version: 1,
            timestamp: 1,
            items: SyncItems {
                settings: Some(SettingsItem {
                    content: r#"{"a":1}"#.into(),
                    claude_content: Some(r#"{"claude":true}"#.into()),
                }),
                skills: Some(FilesItem {
                    files: vec![FileEntry {
                        path: "x/SKILL.md".into(),
                        content: b"# hi".to_vec(),
                    }],
                }),
                mcp: Some(McpItem {
                    global: Some("{}".into()),
                    project: Some("{}".into()),
                }),
                plugins: Some(FilesItem {
                    files: vec![FileEntry {
                        path: "p/plugin.md".into(),
                        content: b"plugin".to_vec(),
                    }],
                }),
            },
        };
        let writes = stage_package(&home, &cwd, &pkg, &staging).unwrap();
        // settings + claude-settings + skills 1 + mcp 2 + plugins 1 = 6
        assert_eq!(writes.len(), 6);
        commit_files(&writes).unwrap();
        assert!(home.join(".peri/settings.json").exists());
        assert!(home.join(".claude/settings.json").exists());
        assert!(home.join(".claude/skills/x/SKILL.md").exists());
        assert!(home.join(".mcp.json").exists());
        assert!(cwd.join(".mcp.json").exists());
        assert!(home.join(".claude/plugins/cache/p/plugin.md").exists());
    }

    #[test]
    fn test_commit_backs_up_existing_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cwd = tmp.path().join("cwd");
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(home.join(".peri")).unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(home.join(".peri/settings.json"), "old").unwrap();

        let pkg = SyncPackage {
            version: 1,
            timestamp: 1,
            items: SyncItems {
                settings: Some(SettingsItem {
                    content: "new".into(),
                    claude_content: None,
                }),
                ..Default::default()
            },
        };
        let writes = stage_package(&home, &cwd, &pkg, &staging).unwrap();
        commit_files(&writes).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join(".peri/settings.json")).unwrap(),
            "new"
        );
        // settings 语义保持 .bak 备份。
        assert_eq!(
            std::fs::read_to_string(home.join(".peri/settings.json.bak")).unwrap(),
            "old"
        );
    }

    // ─── 日志敏感值 ──────────────────────────────────────────────────────────

    #[derive(Clone)]
    struct LogSink(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for LogSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn with_captured_logs<T>(f: impl FnOnce() -> T) -> (T, String) {
        let sink = Arc::new(Mutex::new(Vec::<u8>::new()));
        let sink2 = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || LogSink(sink2.clone()))
            .with_ansi(false)
            .finish();
        let out = tracing::subscriber::with_default(subscriber, f);
        let logs = String::from_utf8(sink.lock().unwrap().clone()).unwrap();
        (out, logs)
    }

    #[test]
    #[serial]
    fn test_logs_redact_sensitive_values() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (fixture, logs) =
            with_captured_logs(|| rt.block_on(async { run_happy_path_fixture().await }));
        // 流程必须成功（日志是在完整传输期间捕获的）。
        assert_eq!(fixture.sender_outcome.parts_uploaded, 3);
        assert_eq!(fixture.receiver_outcome.files, 2);

        // 红线断言：日志不得包含同步码、Authorization、明文 payload 与完整 channel ID。
        assert!(!logs.contains(&fixture.code), "日志不得包含同步码全文");
        assert!(
            !logs.contains("PeriSig"),
            "日志不得包含 Authorization 签名头"
        );
        assert!(
            !logs.contains("SENSITIVE_MARKER_abc"),
            "日志不得包含明文 payload"
        );
        assert!(
            !logs.contains(&fixture.sender_outcome.channel_id)
                && !logs.contains(&fixture.receiver_outcome.channel_id),
            "日志不得包含完整 channel ID"
        );
    }

    // ─── Slice 3 复审修复回归（C1/H1/H2/H4/H5/M1/M2/L2）────────────────────

    // ─── Slice 3 二轮复审回归（High-1/Medium-2/Low-3）────────────────────────

    #[tokio::test]
    async fn test_h1_download_retry_refreshes_signature_timestamp() {
        // 二轮复审（High-1）：download 404 重试必须每次请求前刷新签名时间戳，
        // 否则 404 长重试跨 >300s（服务端 ±300s 偏差窗口）后被 401 截断。
        // 时钟推进模拟 >300s：断言推进后的 download 请求使用刷新后的时间戳。
        let seeded = seed_receiver_scenario(true, &small_plaintext_package());
        seeded.mock.set_slow_downloads(3); // 前 3 次 download 404（慢速上传场景）
        let start_ts = 1_700_000_000u64;
        let (cfg, clock) = clocked_cfg(Duration::from_secs(400), start_ts);
        let mock = seeded.mock.clone();
        let handle = {
            // cfg 的 now 闭包已持有 clock 的 Arc 克隆；seeded 字段全为 owned，
            // 一并 move 进 spawn 后再构造 flow（借用仅在块内成立）。
            let cfg = cfg;
            let seeded = seeded;
            let mock = mock.clone();
            let backoff = ZeroBackoff;
            tokio::spawn(async move {
                let seeded_flow = ReceiverFlow {
                    client: &mock,
                    backoff: &backoff,
                    store: &seeded.receiver_store,
                    local: &seeded.receiver_ident,
                    peers: &seeded.receiver_peers,
                    home_dir: &seeded.receiver_home,
                    cwd: &seeded.cwd,
                    cfg: &cfg,
                };
                run_receiver(seeded_flow, &seeded.code_norm).await
            })
        };
        // 等到第一次 download 发生（时间戳 = start_ts）。
        wait_until(|| !mock.download_auth_ts().is_empty()).await;
        assert_eq!(mock.download_auth_ts()[0], start_ts);
        // 推进时钟 >300s（模拟下载期间签名时间戳过期）。
        advance_clock(&clock, start_ts + 350);
        let outcome = handle
            .await
            .unwrap()
            .expect("download 404 重试必须跨 >300s 后仍成功");
        assert_eq!(outcome.files, 1);
        // 推进后的下载请求必须使用刷新后的时间戳（> start_ts + 300）。
        let ts = mock.download_auth_ts();
        assert!(
            ts.iter().any(|t| *t > start_ts + 300),
            "推进时钟后的 download 请求必须刷新签名时间戳: {ts:?}"
        );
        assert!(mock.is_confirmed());
    }

    #[tokio::test]
    async fn test_low3_sender_confirm_poll_survives_rate_limit() {
        // 二轮复审（Low-3）：sender 的 confirm 等待轮询统一包裹 retry_429——
        // receiver confirm 后 channel 进入终态，sender 轮询被限流（429 + Retry-After: 0）
        // 时走退避重试，随后以 404（终态）正常结束，不得把 429 当作失败。
        let tmp = tempfile::tempdir().unwrap();
        let sender_home = tmp.path().join("sender-home");
        let receiver_home = tmp.path().join("receiver-home");
        let cwd = tmp.path().join("cwd");
        std::fs::create_dir_all(&sender_home).unwrap();
        std::fs::create_dir_all(&receiver_home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(sender_home.join(".peri")).unwrap();
        std::fs::write(sender_home.join(".peri/settings.json"), "{}").unwrap();

        let sender_mat = KeyMaterial::generate().unwrap();
        let receiver_mat = KeyMaterial::generate().unwrap();
        let sender_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            sender_mat.ed25519_public(),
            sender_mat.x25519_public(),
            "sender",
        )
        .unwrap();
        let receiver_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            receiver_mat.ed25519_public(),
            receiver_mat.x25519_public(),
            "receiver",
        )
        .unwrap();
        let target_peer = TrustedPeer::from_device(&receiver_ident, 1);
        let mut receiver_peers = TrustedPeers::default();
        receiver_peers
            .add(TrustedPeer::from_device(&sender_ident, 1))
            .unwrap();

        let mock = MockServer::new();
        mock.set_rate_limit_sender_confirm(3); // 终态后 sender 轮询被限流 3 次
        let backoff = ZeroBackoff;
        let sender_store = TestStore {
            material: sender_mat,
        };
        let receiver_store = TestStore {
            material: receiver_mat,
        };
        let cfg = test_cfg();
        let items = all_items();

        let code_holder = Arc::new(Mutex::new(None::<String>));
        let code_cb = {
            let ch = code_holder.clone();
            move |code: SyncCode, _epoch: u64, _remaining: u64| {
                *ch.lock().unwrap() = Some(code.display());
            }
        };
        let wait_code = code_holder.clone();

        let (sender_res, receiver_res) = tokio::join!(
            run_sender(
                SenderFlow {
                    client: &mock,
                    backoff: &backoff,
                    store: &sender_store,
                    local: &sender_ident,
                    target: &target_peer,
                    home_dir: &sender_home,
                    cwd: &cwd,
                    items: &items,
                    cfg: &cfg,
                },
                code_cb,
            ),
            async {
                let code = loop {
                    if let Some(c) = wait_code.lock().unwrap().clone() {
                        break c;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                };
                run_receiver(
                    ReceiverFlow {
                        client: &mock,
                        backoff: &backoff,
                        store: &receiver_store,
                        local: &receiver_ident,
                        peers: &receiver_peers,
                        home_dir: &receiver_home,
                        cwd: &cwd,
                        cfg: &cfg,
                    },
                    &code.to_lowercase(),
                )
                .await
            }
        );

        let sender_outcome =
            sender_res.expect("sender flow 必须成功（confirm 轮询被限流后仍以 404 正常结束）");
        let receiver_outcome = receiver_res.expect("receiver flow 必须成功");
        assert!(sender_outcome.parts_uploaded >= 1);
        assert_eq!(receiver_outcome.files, 1);
        assert!(
            mock.rate_limit_hits() >= 1,
            "confirm 轮询必须实际命中注入的 429"
        );
        assert!(mock.is_confirmed());
    }

    #[tokio::test]
    async fn test_m2_part_throttling_paces_requests() {
        // 二轮复审（Medium-2）：min_part_interval 生效时 part 请求被节流
        // （上传循环内两次请求间隔 ≥ min_part_interval）。
        let tmp = tempfile::tempdir().unwrap();
        let sender_home = tmp.path().join("sender-home");
        let cwd = tmp.path().join("cwd");
        std::fs::create_dir_all(&sender_home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(sender_home.join(".peri")).unwrap();
        std::fs::write(sender_home.join(".peri/settings.json"), "{}").unwrap();
        std::fs::create_dir_all(sender_home.join(".claude/skills/demo")).unwrap();
        std::fs::write(
            sender_home.join(".claude/skills/demo/SKILL.md"),
            "# Demo\n".repeat(10_000), // 10 万字节 → 2 片
        )
        .unwrap();

        let sender_mat = KeyMaterial::generate().unwrap();
        let receiver_mat = KeyMaterial::generate().unwrap();
        let sender_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            sender_mat.ed25519_public(),
            sender_mat.x25519_public(),
            "sender",
        )
        .unwrap();
        let receiver_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            receiver_mat.ed25519_public(),
            receiver_mat.x25519_public(),
            "receiver",
        )
        .unwrap();
        let target_peer = TrustedPeer::from_device(&receiver_ident, 1);
        let mut receiver_peers = TrustedPeers::default();
        receiver_peers
            .add(TrustedPeer::from_device(&sender_ident, 1))
            .unwrap();

        let mock = MockServer::new();
        let backoff = ZeroBackoff;
        let sender_store = TestStore {
            material: sender_mat,
        };
        let receiver_store = TestStore {
            material: receiver_mat,
        };
        let cfg = FlowConfig {
            poll_interval: Duration::from_millis(5),
            start_timeout: Duration::from_secs(10),
            confirm_wait_timeout: Duration::from_secs(10),
            min_part_interval: Duration::from_millis(50),
            ..Default::default()
        };
        let items = all_items();

        let code_holder = Arc::new(Mutex::new(None::<String>));
        let code_cb = {
            let ch = code_holder.clone();
            move |code: SyncCode, _epoch: u64, _remaining: u64| {
                *ch.lock().unwrap() = Some(code.display());
            }
        };
        let wait_code = code_holder.clone();
        let start = std::time::Instant::now();
        let (sender_res, receiver_res) = tokio::join!(
            run_sender(
                SenderFlow {
                    client: &mock,
                    backoff: &backoff,
                    store: &sender_store,
                    local: &sender_ident,
                    target: &target_peer,
                    home_dir: &sender_home,
                    cwd: &cwd,
                    items: &items,
                    cfg: &cfg,
                },
                code_cb,
            ),
            async {
                let code = loop {
                    if let Some(c) = wait_code.lock().unwrap().clone() {
                        break c;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                };
                run_receiver(
                    ReceiverFlow {
                        client: &mock,
                        backoff: &backoff,
                        store: &receiver_store,
                        local: &receiver_ident,
                        peers: &receiver_peers,
                        home_dir: &sender_home.clone(),
                        cwd: &cwd,
                        cfg: &cfg,
                    },
                    &code.to_lowercase(),
                )
                .await
            }
        );
        sender_res.expect("sender 应成功");
        receiver_res.expect("receiver 应成功");
        // 2 片上传：节流间隔 ≥50ms ⇒ 总耗时 ≥ ~100ms（未节流时 <20ms）。
        assert!(
            start.elapsed() >= Duration::from_millis(80),
            "part 请求必须被节流（>= 2 × 50ms）"
        );
        assert!(mock.is_confirmed());
    }

    #[tokio::test]
    #[ignore = "Low-4：断言恰 512 片依赖 msgpack 序列化开销（<582B）与 SKILL.md 内容尺寸，msgpack 格式或 fixture 微调即破坏；预算边界断言由 limits_test::validate_manifest(512, 32MiB) 覆盖，512 片下载路径由 test_h5 覆盖"]
    async fn test_c1_full_budget_transfer_not_413() {
        // 明文 ≈32MiB（512 满片，每片 65507B 明文 → 65536B 密文）：mock 复刻
        // TS channel-do 的预算校验（单片 ct.length > 64KiB 或累计 > 32MiB →
        // 413），传输必须不 413（C1 复审修复）。
        // "# Demo\n" 为 7 字节 × 4_791_286 = 33_539_002 字节，msgpack 后恰好
        // 跨 512 片（65507B/片）且不超 32MiB 密文预算。
        let fixture = run_happy_path_fixture_with(4_791_286).await;
        assert_eq!(
            fixture.sender_outcome.parts_uploaded, 512,
            "满预算明文必须恰好 512 片"
        );
        assert_eq!(fixture.receiver_outcome.files, 2);
        assert!(fixture.mock.is_confirmed());
        // 接收端 33.5MB 文件完整落盘。
        let skill =
            std::fs::read_to_string(fixture.receiver_home.join(".claude/skills/demo/SKILL.md"))
                .unwrap();
        assert_eq!(skill.len(), 7 * 4_791_286);
    }

    #[tokio::test]
    async fn test_h1_handshake_retry_refreshes_signature_timestamp() {
        // msg1 重发/握手轮询必须在每次请求前用当前时间重新构造 auth_header：
        // 构造签名后把时钟推进 >300s（超出服务端 ±300s 偏差窗口），
        // 断言后续轮询请求使用刷新后的时间戳。
        let tmp = tempfile::tempdir().unwrap();
        let sender_home = tmp.path().join("sender-home");
        let cwd = tmp.path().join("cwd");
        std::fs::create_dir_all(&sender_home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(sender_home.join(".peri")).unwrap();
        std::fs::write(sender_home.join(".peri/settings.json"), "{}").unwrap();

        let sender_mat = KeyMaterial::generate().unwrap();
        let receiver_mat = KeyMaterial::generate().unwrap();
        let sender_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            sender_mat.ed25519_public(),
            sender_mat.x25519_public(),
            "sender",
        )
        .unwrap();
        let receiver_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            receiver_mat.ed25519_public(),
            receiver_mat.x25519_public(),
            "receiver",
        )
        .unwrap();
        let target_peer = TrustedPeer::from_device(&receiver_ident, 1);

        let mock = MockServer::new();
        let backoff = ZeroBackoff;
        let store = TestStore {
            material: sender_mat,
        };
        let items = all_items();
        let start_ts = 1_700_000_000u64;
        let (cfg, clock) = clocked_cfg(Duration::from_secs(30), start_ts);
        // sender 在后台运行（无人 join；靠 start_timeout 超时退出）。
        let handle = {
            let mock = mock.clone();
            tokio::spawn(async move {
                run_sender(
                    SenderFlow {
                        client: &mock,
                        backoff: &backoff,
                        store: &store,
                        local: &sender_ident,
                        target: &target_peer,
                        home_dir: &sender_home,
                        cwd: &cwd,
                        items: &items,
                        cfg: &cfg,
                    },
                    |_code, _epoch, _remaining| {},
                )
                .await
            })
        };
        // 等待首次请求；推进时钟 >300s；断言后续请求的 ts 已刷新。
        wait_until(|| !mock.handshake_auth_ts().is_empty()).await;
        advance_clock(&clock, start_ts + 301);
        wait_until(|| mock.handshake_auth_ts().len() >= 4).await;
        let ts = mock.handshake_auth_ts();
        let last = *ts.last().unwrap();
        assert!(
            last >= start_ts + 301,
            "轮询必须用当前时间刷新签名时间戳: {ts:?}"
        );
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_h2_register_forbidden_after_paired_continues_polling() {
        // receiver 已 join（paired）后 epoch 变更：register 返回 403 必须视为
        // 码使命已结束，继续轮询 msg2 而非传播错误退出（H2）。
        let tmp = tempfile::tempdir().unwrap();
        let sender_home = tmp.path().join("sender-home");
        let cwd = tmp.path().join("cwd");
        std::fs::create_dir_all(&sender_home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(sender_home.join(".peri")).unwrap();
        std::fs::write(sender_home.join(".peri/settings.json"), "{}").unwrap();

        let sender_mat = KeyMaterial::generate().unwrap();
        let receiver_mat = KeyMaterial::generate().unwrap();
        let sender_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            sender_mat.ed25519_public(),
            sender_mat.x25519_public(),
            "sender",
        )
        .unwrap();
        let receiver_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            receiver_mat.ed25519_public(),
            receiver_mat.x25519_public(),
            "receiver",
        )
        .unwrap();
        let target_peer = TrustedPeer::from_device(&receiver_ident, 1);

        let mock = MockServer::new();
        // 第一次注册成功后模拟 receiver 加入（channel → Paired）。
        mock.set_auto_join_after_registers(1);
        let backoff = ZeroBackoff;
        let store = TestStore {
            material: sender_mat,
        };
        let items = all_items();
        let start_ts = 1_700_000_000u64;
        let (cfg, clock) = clocked_cfg(Duration::from_millis(300), start_ts);
        let handle = {
            let mock = mock.clone();
            tokio::spawn(async move {
                run_sender(
                    SenderFlow {
                        client: &mock,
                        backoff: &backoff,
                        store: &store,
                        local: &sender_ident,
                        target: &target_peer,
                        home_dir: &sender_home,
                        cwd: &cwd,
                        items: &items,
                        cfg: &cfg,
                    },
                    |_code, _epoch, _remaining| {},
                )
                .await
            })
        };
        // 第一次注册（auto_join → Paired）。
        wait_until(|| mock.register_calls() >= 1).await;
        // 推进 1 个 epoch → 第二次注册 → 403（码使命结束）→ 继续轮询 msg2。
        advance_clock(&clock, start_ts + 30);
        wait_until(|| mock.register_calls() >= 2).await;
        // 403 后必须继续轮询（handshake 请求持续增加），直至超时。
        let before = mock.handshake_auth_ts().len();
        wait_until(|| mock.handshake_auth_ts().len() > before).await;
        let res = handle.await.unwrap();
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains("timed out waiting"),
            "403 后必须继续轮询直至超时（而非立即失败）: {err}"
        );
        assert_eq!(mock.register_calls(), 2, "第二次注册必须已发生（返回 403）");
    }

    #[tokio::test]
    async fn test_h4_msg1_32b_payload_rejected() {
        // 32B 纯 data key 形态：noise 层可解密，但 channel_flow 必须返回布局
        // 错误（禁止对 72B 布局的越界切片；H4 复审修复）。
        let tmp = tempfile::tempdir().unwrap();
        let receiver_home = tmp.path().join("receiver-home");
        let cwd = tmp.path().join("cwd");
        std::fs::create_dir_all(&receiver_home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        let sender_mat = KeyMaterial::generate().unwrap();
        let receiver_mat = KeyMaterial::generate().unwrap();
        let sender_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            sender_mat.ed25519_public(),
            sender_mat.x25519_public(),
            "sender",
        )
        .unwrap();
        let receiver_ident = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            receiver_mat.ed25519_public(),
            receiver_mat.x25519_public(),
            "receiver",
        )
        .unwrap();

        let code = SyncCode::generate().unwrap();
        let code_norm = code.normalized();
        let channel_id = DeviceId::random().unwrap().to_b64();
        let data_key = DataKey::random().unwrap();
        let params = HandshakeParams {
            channel_id: channel_id.clone(),
            initiator: binding_for(&sender_ident),
            responder: binding_for(&receiver_ident),
        };
        let local_x = sender_mat.x25519.clone();
        let mut session = initiator_session(&params, &local_x).unwrap();
        // 32B 纯 data key payload 的 msg1（noise 层允许）。
        let msg1 = session.write_message(data_key.as_array()).unwrap();

        let mock = MockServer::new();
        mock.seed(
            &channel_id,
            &sender_ident,
            &receiver_ident,
            &code_norm,
            msg1,
            vec![],
        );
        let mut receiver_peers = TrustedPeers::default();
        receiver_peers
            .add(TrustedPeer::from_device(&sender_ident, 1))
            .unwrap();
        let backoff = ZeroBackoff;
        let cfg = test_cfg();
        let flow = ReceiverFlow {
            client: &mock,
            backoff: &backoff,
            store: &TestStore {
                material: receiver_mat,
            },
            local: &receiver_ident,
            peers: &receiver_peers,
            home_dir: &receiver_home,
            cwd: &cwd,
            cfg: &cfg,
        };
        let err = run_receiver(flow, &code_norm).await.unwrap_err();
        assert!(
            err.to_string().contains("unexpected payload layout"),
            "32B msg1 必须返回布局错误（不得 panic）: {err}"
        );
        // 未 commit 未 confirm，且未向对端写 msg2。
        assert!(!receiver_home.join(".peri/settings.json").exists());
        assert!(
            mock.inner
                .lock()
                .unwrap()
                .channel
                .as_ref()
                .unwrap()
                .msg2
                .is_none(),
            "32B msg1 不得触发 msg2 写入"
        );
        assert!(!mock.is_confirmed());
    }

    #[tokio::test]
    async fn test_h5_slow_upload_within_channel_timeout() {
        // 512 parts 慢速上传：前 20 次 download 返回 404（模拟上传未完成）。
        // 404 重试预算绑定 start_timeout（5s）而非固定次数，必须最终成功（H5）。
        let pkg = SyncPackage {
            version: 1,
            timestamp: 1,
            items: SyncItems {
                settings: Some(SettingsItem {
                    content: r#"{"k":"v"}"#.into(),
                    claude_content: None,
                }),
                skills: Some(FilesItem {
                    files: vec![FileEntry {
                        path: "big/SKILL.md".into(),
                        content: vec![b'x'; 33_539_000],
                    }],
                }),
                mcp: None,
                plugins: None,
            },
        };
        let plaintext = rmp_serde::to_vec(&pkg).unwrap();
        // 明文跨 512 片（每片 65507B），接近满预算。
        assert!(
            plaintext.len() > 511 * limits::MAX_PLAINTEXT_PART_BYTES,
            "测试前提：明文必须跨 512 片"
        );
        assert!(plaintext.len() <= 512 * limits::MAX_PLAINTEXT_PART_BYTES);
        let seeded = seed_receiver_scenario(true, &plaintext);
        seeded.mock.set_slow_downloads(20);
        let backoff = ZeroBackoff;
        let cfg = test_cfg();
        let flow = ReceiverFlow {
            client: &seeded.mock,
            backoff: &backoff,
            store: &seeded.receiver_store,
            local: &seeded.receiver_ident,
            peers: &seeded.receiver_peers,
            home_dir: &seeded.receiver_home,
            cwd: &seeded.cwd,
            cfg: &cfg,
        };
        let outcome = run_receiver(flow, &seeded.code_norm)
            .await
            .expect("慢速上传必须在 channel 总超时内完成（404 不设固定次数）");
        // settings + big/SKILL.md 两个文件。
        assert_eq!(outcome.files, 2);
        assert!(seeded.mock.is_confirmed());
        // 512 parts + 20 次慢速 404 = 532 次 download 调用。
        assert_eq!(seeded.mock.download_calls(), 512 + 20);
        // 落盘文件完整，staging 已清理。
        let written =
            std::fs::read_to_string(seeded.receiver_home.join(".claude/skills/big/SKILL.md"))
                .unwrap();
        assert_eq!(written.len(), 33_539_000);
        let has_staging = std::fs::read_dir(seeded.receiver_home.join(".peri"))
            .unwrap()
            .any(|e| {
                e.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("staging-")
            });
        assert!(!has_staging, "staging 目录必须已清理");
    }

    #[test]
    fn test_m2_staging_dir_and_file_permissions() {
        // staging 目录 0700、staged 文件 0600（unix；M2 复审修复）。
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cwd = tmp.path().join("cwd");
        let staging = staging_dir_for(&home, "deadbeef");
        create_staging_dir(&staging).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&staging).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "staging 目录必须 0700");
        }
        let pkg = SyncPackage {
            version: 1,
            timestamp: 1,
            items: SyncItems {
                settings: Some(SettingsItem {
                    content: r#"{"k":"v"}"#.into(),
                    claude_content: None,
                }),
                ..Default::default()
            },
        };
        let writes = stage_package(&home, &cwd, &pkg, &staging).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&writes[0].staged)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "staged 文件必须 0600");
        }
        #[cfg(not(unix))]
        {
            // 权限断言仅限 unix；其他平台仍执行 stage_package 验证打包成功。
            let _ = writes;
        }
    }

    #[test]
    fn test_m2_commit_falls_back_to_copy_on_rename_failure() {
        // rename 失败（注入）必须回退 copy 且清理 staged 文件（M2 复审修复）。
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cwd = tmp.path().join("cwd");
        let staging = tmp.path().join("staging");
        create_staging_dir(&staging).unwrap();
        let pkg = SyncPackage {
            version: 1,
            timestamp: 1,
            items: SyncItems {
                settings: Some(SettingsItem {
                    content: "new".into(),
                    claude_content: None,
                }),
                ..Default::default()
            },
        };
        let writes = stage_package(&home, &cwd, &pkg, &staging).unwrap();
        commit_files_with(&writes, |_, _| {
            Err(std::io::Error::other("injected rename failure"))
        })
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join(".peri/settings.json")).unwrap(),
            "new"
        );
        assert!(
            !writes[0].staged.exists(),
            "copy 回退后必须清理 staged 文件"
        );
    }

    #[test]
    fn test_l2_default_poll_interval_respects_rate_limits() {
        // L2 复审修复：轮询间隔放宽至 3–5s（默认 3s），远离 60/min 限流边缘。
        assert_eq!(FlowConfig::default().poll_interval, Duration::from_secs(3));
    }
}
