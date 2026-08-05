#[cfg(test)]
mod tests {
    use crate::sync::crypto::DataKey;
    use crate::sync::noise_session::{
        HandshakeParams, PeerBinding, Role, handshake_prologue, initiator_session,
        responder_session,
    };
    use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};

    /// binding 的 x_pub 必须与 `local_key(secret_byte)` 的实际公钥一致，
    /// 否则握手必然认证失败（这正是 `test_wrong_*_static_key` 要覆盖的错误场景）。
    fn binding(device_id: &str, secret_byte: u8) -> PeerBinding {
        let secret = StaticSecret::from([secret_byte; 32]);
        PeerBinding {
            device_id: device_id.to_string(),
            ed_pub: [secret_byte; 32],
            x_pub: XPublicKey::from(&secret).to_bytes(),
        }
    }

    fn params(channel_id: &str) -> HandshakeParams {
        HandshakeParams {
            channel_id: channel_id.to_string(),
            initiator: binding("init-device", 0x11),
            responder: binding("resp-device", 0x22),
        }
    }

    fn local_key(byte: u8) -> StaticSecret {
        StaticSecret::from([byte; 32])
    }

    fn pub_of(byte: u8) -> [u8; 32] {
        XPublicKey::from(&local_key(byte)).to_bytes()
    }

    #[test]
    fn test_prologue_structure_and_binding() {
        // prologue 必须绑定协议版本、channel、双方 device ID、角色与双方公钥。
        let p1 = params("ch_1");
        let prologue = String::from_utf8(handshake_prologue(&p1).unwrap()).unwrap();
        assert!(prologue.starts_with("peri-sync/v1|handshake|"));
        assert!(prologue.contains("|ch_1|"));
        assert!(prologue.contains("|init-device|sender|"));
        assert!(prologue.contains("|resp-device|receiver|"));
        let init_x = crate::sync::canonical::b64url_nopad(&p1.initiator.x_pub);
        let resp_x = crate::sync::canonical::b64url_nopad(&p1.responder.x_pub);
        let init_ed = crate::sync::canonical::b64url_nopad(&p1.initiator.ed_pub);
        let resp_ed = crate::sync::canonical::b64url_nopad(&p1.responder.ed_pub);
        assert!(prologue.contains(&format!("|{init_ed}|{init_x}|")));
        assert!(prologue.contains(&format!("|{resp_ed}|{resp_x}")));
        // 通道 ID 变化 → prologue 变化。
        assert_ne!(
            handshake_prologue(&params("ch_1")).unwrap(),
            handshake_prologue(&params("ch_2")).unwrap()
        );
    }

    fn run_full_handshake(channel_id: &str) -> (DataKey, DataKey) {
        let params = params(channel_id);
        let mut init = initiator_session(&params, &local_key(0x11)).expect("initiator 应可构建");
        let mut resp = responder_session(&params, &local_key(0x22)).expect("responder 应可构建");
        assert_eq!(init.role(), Role::Sender);
        assert_eq!(resp.role(), Role::Receiver);

        // msg1: initiator 在加密 payload 中携带 data key → responder。
        // IK 只有两条握手消息，msg1 是 initiator 唯一一次写。
        let data_key = DataKey::random().expect("data key 生成应成功");
        let m1 = init
            .write_message(data_key.as_array())
            .expect("msg1 写入应成功");
        let payload1 = resp.read_message(&m1).expect("msg1 读取应成功");
        assert_eq!(payload1.len(), 32);
        assert_eq!(payload1, data_key.as_array().as_slice());
        // responder 从 msg1 学到 initiator 的 static 公钥，可用于 trusted peer 核对。
        assert_eq!(
            resp.remote_static(),
            Some(pub_of(0x11)),
            "responder 必须学到 initiator 的 X25519 static 公钥"
        );
        // 未完成 trusted 核对前，data key 不得暴露（API 强制）。
        assert!(resp.data_key().is_none(), "未核对身份前不得暴露 data key");

        // msg2: responder → initiator。
        let m2 = resp.write_message(&[]).expect("msg2 写入应成功");
        assert!(init.read_message(&m2).expect("msg2 读取应成功").is_empty());

        assert!(init.is_handshake_finished());
        assert!(resp.is_handshake_finished());

        // 进入 transport 前 API 强制核对：双方以 trusted 记录中的 static 公钥
        // 传入 into_transport，任何不符都会在此失败。
        let init_t = init
            .into_transport(Some(params.responder.x_pub))
            .expect("initiator transport 应成功");
        let resp_t = resp
            .into_transport(Some(params.initiator.x_pub))
            .expect("responder transport 应成功");
        let init_key = init_t.data_key().expect("initiator 应持有 data key");
        let resp_key = resp_t.data_key().expect("responder 应持有 data key");
        assert_eq!(init_key.as_array(), resp_key.as_array());
        (init_key.clone(), resp_key.clone())
    }

    #[test]
    fn test_full_handshake_exchanges_data_key() {
        let (init_key, resp_key) = run_full_handshake("ch_roundtrip");
        assert_eq!(init_key.as_array(), resp_key.as_array());
        assert_eq!(init_key.len(), 32);
    }

    #[test]
    fn test_transport_roundtrip_and_tamper() {
        let params = params("ch_transport");
        let mut init = initiator_session(&params, &local_key(0x11)).unwrap();
        let mut resp = responder_session(&params, &local_key(0x22)).unwrap();

        let key = DataKey::random().unwrap();
        let m1 = init.write_message(key.as_array()).unwrap();
        resp.read_message(&m1).unwrap();
        let m2 = resp.write_message(&[]).unwrap();
        init.read_message(&m2).unwrap();

        // transport 测试必须传入正确 trusted 公钥。
        let mut init_t = init
            .into_transport(Some(params.responder.x_pub))
            .expect("initiator transport 应成功");
        let mut resp_t = resp
            .into_transport(Some(params.initiator.x_pub))
            .expect("responder transport 应成功");

        // 双向加密。
        let c1 = init_t.send(b"hello from sender").unwrap();
        assert_eq!(resp_t.recv(&c1).unwrap(), b"hello from sender");
        let c2 = resp_t.send(b"hello from receiver").unwrap();
        assert_eq!(init_t.recv(&c2).unwrap(), b"hello from receiver");

        // 篡改 transport 消息必须认证失败。
        let mut tampered = c1;
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert!(resp_t.recv(&tampered).is_err());
    }

    #[test]
    fn test_peer_mismatch_channel_id_fails() {
        let params_a = params("ch_A");
        let mut init = initiator_session(&params_a, &local_key(0x11)).unwrap();
        // responder 使用不同 channel_id → prologue 不一致 → msg1 认证失败。
        let mut resp = responder_session(&params("ch_B"), &local_key(0x22)).unwrap();
        let m1 = init.write_message(&[0u8; 32]).unwrap();
        assert!(
            resp.read_message(&m1).is_err(),
            "channel ID 不一致必须认证失败"
        );
    }

    #[test]
    fn test_peer_mismatch_device_ids_fails() {
        let mut params_a = params("ch_same");
        let params_b = params("ch_same");
        params_a.initiator.device_id = "evil-device".to_string();
        let mut init = initiator_session(&params_a, &local_key(0x11)).unwrap();
        let mut resp = responder_session(&params_b, &local_key(0x22)).unwrap();
        let m1 = init.write_message(&[0u8; 32]).unwrap();
        assert!(
            resp.read_message(&m1).is_err(),
            "device ID 不一致必须认证失败"
        );
    }

    #[test]
    fn test_wrong_initiator_static_key_fails() {
        // IK 的 msg1 认证只证明发送者持有自己声明的 static 私钥且知道
        // responder 的 static 公钥；用错误 static 私钥构造的 msg1 在密码学上
        // 仍然有效，responder 解密成功。身份核对由 API 强制：以 trusted 记录
        // 公钥调用 into_transport(Some(..)) 必须失败，且核对前 data key 不暴露。
        let params = params("ch_static");
        let mut init = initiator_session(&params, &local_key(0x99)).unwrap();
        let mut resp = responder_session(&params, &local_key(0x22)).unwrap();
        let m1 = init.write_message(&[0u8; 32]).unwrap();
        let payload = resp
            .read_message(&m1)
            .expect("IK msg1 可解密：本地已完成密码学认证");
        assert_eq!(payload.len(), 32);
        // msg1 携带的 static 公钥与 trusted 记录中的 initiator x_pub 不符。
        assert_ne!(resp.remote_static(), Some(params.initiator.x_pub));
        // 未完成 trusted 核对前，data key 不得暴露（即使已从 msg1 载荷提取）。
        assert!(resp.data_key().is_none(), "未核对身份前不得暴露 data key");
        // 显式核对同样失败。
        assert!(
            resp.verify_peer(params.initiator.x_pub).is_err(),
            "与 trusted 记录不符必须核对失败"
        );
        // 完成握手（responder 写 msg2、initiator 读）后，强制核对进入
        // transport → 必须失败，不得使用已提取的 data key。
        let m2 = resp.write_message(&[]).unwrap();
        init.read_message(&m2).unwrap();
        assert!(resp.is_handshake_finished());
        // （TransportSession 不实现 Debug，用 match 取错误而非 unwrap_err。）
        let err = match resp.into_transport(Some(params.initiator.x_pub)) {
            Ok(_) => panic!("身份不符必须拒绝进入 transport"),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains("does not match"),
            "身份不符必须拒绝进入 transport: {err}"
        );
    }

    #[test]
    fn test_into_transport_none_skips_verification() {
        // None = 调用方显式跳过核对（测试专用逃生口，生产路径必须传 Some）。
        // 跳过时 transport 照常可用，由调用方承担身份判断责任。
        let params = params("ch_skip");
        let mut init = initiator_session(&params, &local_key(0x99)).unwrap();
        let mut resp = responder_session(&params, &local_key(0x22)).unwrap();
        let m1 = init.write_message(&[0u8; 32]).unwrap();
        resp.read_message(&m1).unwrap();
        let m2 = resp.write_message(&[]).unwrap();
        init.read_message(&m2).unwrap();
        let resp_t = resp
            .into_transport(None)
            .expect("显式跳过核对应可用（测试专用）");
        assert_eq!(resp_t.data_key().unwrap().as_array(), &[0u8; 32]);
    }

    #[test]
    fn test_wrong_responder_static_key_fails() {
        // initiator 配置了错误的 responder 公钥 → 消息无法被 responder 解开。
        let mut params_wrong = params("ch_static2");
        params_wrong.responder.x_pub = [0xEE; 32];
        let mut init = initiator_session(&params_wrong, &local_key(0x11)).unwrap();
        let mut resp = responder_session(&params("ch_static2"), &local_key(0x22)).unwrap();
        let m1 = init.write_message(&[0u8; 32]).unwrap();
        assert!(
            resp.read_message(&m1).is_err(),
            "错误 peer 公钥必须认证失败"
        );
    }

    #[test]
    fn test_tampered_msg1_fails() {
        let params = params("ch_tamper");
        let mut init = initiator_session(&params, &local_key(0x11)).unwrap();
        let mut resp = responder_session(&params, &local_key(0x22)).unwrap();
        let mut m1 = init.write_message(&[0u8; 32]).unwrap();
        let last = m1.len() - 1;
        m1[last] ^= 0x01;
        assert!(resp.read_message(&m1).is_err(), "篡改 msg1 必须失败");
    }

    #[test]
    fn test_msg1_data_key_must_be_32_or_72_bytes() {
        let params = params("ch_keylen");
        let mut init = initiator_session(&params, &local_key(0x11)).unwrap();
        let mut resp = responder_session(&params, &local_key(0x22)).unwrap();
        // msg1 是 initiator 的握手首条消息：payload 必须是 32（纯 data key）
        // 或 72（data key + manifest hash + part count）字节。
        assert!(
            init.write_message(&[0u8; 16]).is_err(),
            "msg1 payload 长度非法必须拒绝"
        );
        assert!(init.write_message(&[0u8; 33]).is_err());
        assert!(init.write_message(&[0u8; 71]).is_err());
        // 长度检查在 snow 调用前完成；合法 32 字节消息不受影响。
        let m1 = init.write_message(&[0u8; 32]).unwrap();
        assert!(resp.read_message(&m1).is_ok(), "合法 msg1 应可读取");
    }

    #[test]
    fn test_msg1_full_payload_layout_roundtrip() {
        // 完整 msg1 布局：data key(32) ‖ manifest hash(32) ‖ part count(8)。
        // 布局是客户端内部约定（TS Worker 只转发 opaque blob），receiver 从
        // read_message 的返回值提取 manifest hash 与 part count。
        use crate::sync::noise_session::{
            MSG1_PAYLOAD_COUNT_OFFSET, MSG1_PAYLOAD_FULL_LEN, MSG1_PAYLOAD_KEY_LEN,
            MSG1_PAYLOAD_MANIFEST_OFFSET,
        };
        let params = params("ch_full_payload");
        let mut init = initiator_session(&params, &local_key(0x11)).unwrap();
        let mut resp = responder_session(&params, &local_key(0x22)).unwrap();

        let data_key = DataKey::random().unwrap();
        let manifest_hash = [0xAB; 32];
        let part_count = 7u64;
        let mut payload = Vec::with_capacity(MSG1_PAYLOAD_FULL_LEN);
        payload.extend_from_slice(data_key.as_array());
        payload.extend_from_slice(&manifest_hash);
        payload.extend_from_slice(&part_count.to_be_bytes());
        assert_eq!(payload.len(), MSG1_PAYLOAD_FULL_LEN);

        let m1 = init.write_message(&payload).expect("72B msg1 写入应成功");
        let opened = resp.read_message(&m1).expect("72B msg1 读取应成功");
        assert_eq!(opened.len(), MSG1_PAYLOAD_FULL_LEN);
        // data key 前 32 字节；responder 提取的 key 与 initiator 一致。
        assert_eq!(
            &opened[..MSG1_PAYLOAD_KEY_LEN],
            data_key.as_array().as_slice()
        );
        // manifest hash 与 part count 原样可达。
        assert_eq!(
            &opened[MSG1_PAYLOAD_MANIFEST_OFFSET..MSG1_PAYLOAD_COUNT_OFFSET],
            manifest_hash
        );
        assert_eq!(
            u64::from_be_bytes(
                opened[MSG1_PAYLOAD_COUNT_OFFSET..MSG1_PAYLOAD_FULL_LEN]
                    .try_into()
                    .unwrap()
            ),
            part_count
        );
        // 核对 trusted 身份后 data key 可用且等于 msg1 携带值。
        assert!(resp.data_key().is_none());
        resp.verify_peer(params.initiator.x_pub).unwrap();
        assert_eq!(resp.data_key().unwrap().as_array(), data_key.as_array());
    }

    #[test]
    fn test_into_transport_before_finish_fails() {
        let params = params("ch_early");
        let init = initiator_session(&params, &local_key(0x11)).unwrap();
        assert!(
            init.into_transport(None).is_err(),
            "未完成握手不得进入 transport"
        );
    }

    #[test]
    fn test_data_key_debug_redacted() {
        let key = DataKey::random().unwrap();
        // Debug 必须脱敏（精确匹配，不泄露任何密钥材料）。
        assert_eq!(format!("{key:?}"), "DataKey(\"[REDACTED]\")");
    }

    #[test]
    fn test_remote_static_matches_initiator_binding() {
        let params = params("ch_remote");
        let mut init = initiator_session(&params, &local_key(0x11)).unwrap();
        let mut resp = responder_session(&params, &local_key(0x22)).unwrap();
        let m1 = init.write_message(&[0u8; 32]).unwrap();
        resp.read_message(&m1).unwrap();
        // initiator 侧配置的 responder static。
        assert_eq!(init.remote_static(), Some(params.responder.x_pub));
        assert_eq!(resp.remote_static(), Some(params.initiator.x_pub));
        // 以 trusted 记录核对成功后，data key 才对 responder 暴露。
        assert!(resp.data_key().is_none(), "核对前不得暴露 data key");
        resp.verify_peer(params.initiator.x_pub)
            .expect("trusted 核对应成功");
        assert_eq!(
            resp.data_key().unwrap().as_array(),
            &[0u8; 32],
            "核对成功后 data key 应可用"
        );
        // 核对幂等：重复核对同一 trusted 公钥仍成功。
        resp.verify_peer(params.initiator.x_pub)
            .expect("重复核对应成功");
    }
}
