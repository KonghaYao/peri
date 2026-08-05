#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::sync::canonical;
    use crate::sync::device::{self, DeviceId};
    use crate::sync::http_client::{
        ApiErrorKind, Backoff, ExponentialBackoff, MAX_429_RETRIES, peri_sig_header,
        validate_server_url,
    };
    use crate::sync::keystore::KeyMaterial;

    #[test]
    fn test_validate_server_url_https_only() {
        // 生产路径（allow_insecure=false）：仅 https。
        assert!(validate_server_url("https://peri-sync.example.com", false).is_ok());
        assert!(validate_server_url("https://example.com/", false).is_ok());
        assert!(validate_server_url("http://example.com", false).is_err());
        assert!(validate_server_url("ws://example.com", false).is_err());
        assert!(validate_server_url("wss://example.com", false).is_err());
        assert!(validate_server_url("ftp://example.com", false).is_err());
        assert!(validate_server_url("not a url", false).is_err());
        // L1 复审修复：测试 override 只放行 http；ws/wss 即使 allow_insecure 也拒绝。
        assert!(validate_server_url("http://127.0.0.1:8787", true).is_ok());
        assert!(validate_server_url("ws://127.0.0.1:8787", true).is_err());
        assert!(validate_server_url("wss://127.0.0.1:8787", true).is_err());
        assert!(validate_server_url("https://example.com", true).is_ok());
        assert!(validate_server_url("ftp://example.com", true).is_err());
    }

    #[test]
    fn test_peri_sig_header_format_and_verification() {
        // 固定密钥材料（Ed25519 签名确定性）：同输入必须产出同一 header。
        let material = KeyMaterial::generate().expect("密钥生成应成功");
        let device_id = DeviceId::random().expect("device id 生成应成功");
        let store = TestStore { material };
        let fields = ["ch_abc", "dev_xyz"];

        let h1 = peri_sig_header(&store, &device_id, "create", &fields, 1715000000).unwrap();
        let h2 = peri_sig_header(&store, &device_id, "create", &fields, 1715000000).unwrap();
        assert_eq!(h1, h2, "Ed25519 确定性签名：同输入同输出");

        // 格式：`PeriSig <device_id> <unix_ts> <sig>`，sig 为 base64url 64 字节。
        let parts: Vec<&str> = h1.split_whitespace().collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "PeriSig");
        assert_eq!(parts[1], device_id.to_b64());
        assert_eq!(parts[2], "1715000000");
        let sig_bytes = canonical::b64url_nopad(
            &ed25519_dalek::Signature::from_slice(
                &crate::sync::http_client::b64url_decode(parts[3]).unwrap(),
            )
            .unwrap()
            .to_bytes(),
        );
        assert_eq!(sig_bytes, parts[3], "签名为 base64url-no-pad 编码");
        assert_eq!(
            crate::sync::http_client::b64url_decode(parts[3])
                .unwrap()
                .len(),
            64
        );

        // 用公钥验证签名：transcript 与字段序逐字节一致（TS 同构）。
        let sig = ed25519_dalek::Signature::from_slice(
            &crate::sync::http_client::b64url_decode(parts[3]).unwrap(),
        )
        .unwrap();
        let ed_pub = store.material.ed25519_public();
        device::verify_transcript(&ed_pub, "create", &fields, 1715000000, &sig)
            .expect("签名必须可验证");

        // 篡改字段/时间戳/op → 验证失败。
        let tampered = ed25519_dalek::Signature::from_slice(
            &crate::sync::http_client::b64url_decode(parts[3]).unwrap(),
        )
        .unwrap();
        assert!(
            device::verify_transcript(
                &ed_pub,
                "create",
                &["ch_abz", "dev_xyz"],
                1715000000,
                &tampered
            )
            .is_err()
        );
        assert!(
            device::verify_transcript(&ed_pub, "create", &fields, 1715000001, &tampered).is_err()
        );
        assert!(
            device::verify_transcript(&ed_pub, "join", &fields, 1715000000, &tampered).is_err()
        );
        // 错误公钥验证失败。
        let other = KeyMaterial::generate().unwrap();
        assert!(
            device::verify_transcript(
                &other.ed25519_public(),
                "create",
                &fields,
                1715000000,
                &tampered
            )
            .is_err()
        );
    }

    #[test]
    fn test_peri_sig_header_frozen_op_field_order() {
        // 冻结字段序（03-plan Slice 3）：签名构造必须使用与 TS 一致的顺序。
        let material = KeyMaterial::generate().unwrap();
        let device_id = DeviceId::random().unwrap();
        let store = TestStore { material };
        let ts = 1715000000u64;

        // create：channel_id, sender_device_id, expected_receiver_device_id, sender_ed_pub, sender_x_pub
        let h = peri_sig_header(
            &store,
            &device_id,
            "create",
            &["ch1", "dev1", "dev2", "EDPUB", "XPUB"],
            ts,
        )
        .unwrap();
        let sig = ed25519_dalek::Signature::from_slice(
            &crate::sync::http_client::b64url_decode(h.split_whitespace().nth(3).unwrap()).unwrap(),
        )
        .unwrap();
        let ed_pub = store.material.ed25519_public();
        assert!(
            device::verify_transcript(
                &ed_pub,
                "create",
                &["ch1", "dev1", "dev2", "EDPUB", "XPUB"],
                ts,
                &sig
            )
            .is_ok(),
            "create 字段序必须与冻结清单一致"
        );
        // 换序必须失败。
        assert!(
            device::verify_transcript(
                &ed_pub,
                "create",
                &["dev1", "ch1", "dev2", "EDPUB", "XPUB"],
                ts,
                &sig
            )
            .is_err(),
            "create 字段换序必须验证失败"
        );

        // code：channel_id, epoch, sha256(code_norm)
        let h =
            peri_sig_header(&store, &device_id, "code", &["ch1", "123", "CODEHASH"], ts).unwrap();
        let sig = ed25519_dalek::Signature::from_slice(
            &crate::sync::http_client::b64url_decode(h.split_whitespace().nth(3).unwrap()).unwrap(),
        )
        .unwrap();
        assert!(
            device::verify_transcript(&ed_pub, "code", &["ch1", "123", "CODEHASH"], ts, &sig)
                .is_ok()
        );

        // join：channel_id, code_norm, receiver_device_id, receiver_ed_pub, receiver_x_pub
        let h = peri_sig_header(
            &store,
            &device_id,
            "join",
            &["ch1", "CODENORM", "dev2", "EDPUB", "XPUB"],
            ts,
        )
        .unwrap();
        let sig = ed25519_dalek::Signature::from_slice(
            &crate::sync::http_client::b64url_decode(h.split_whitespace().nth(3).unwrap()).unwrap(),
        )
        .unwrap();
        assert!(
            device::verify_transcript(
                &ed_pub,
                "join",
                &["ch1", "CODENORM", "dev2", "EDPUB", "XPUB"],
                ts,
                &sig
            )
            .is_ok()
        );

        // msg：channel_id, seq, sha256(payload)
        let h =
            peri_sig_header(&store, &device_id, "msg", &["ch1", "1", "PAYLOADHASH"], ts).unwrap();
        let sig = ed25519_dalek::Signature::from_slice(
            &crate::sync::http_client::b64url_decode(h.split_whitespace().nth(3).unwrap()).unwrap(),
        )
        .unwrap();
        assert!(
            device::verify_transcript(&ed_pub, "msg", &["ch1", "1", "PAYLOADHASH"], ts, &sig)
                .is_ok()
        );

        // upload：channel_id, part_index, sha256(ciphertext)
        let h = peri_sig_header(&store, &device_id, "upload", &["ch1", "3", "CTHASH"], ts).unwrap();
        let sig = ed25519_dalek::Signature::from_slice(
            &crate::sync::http_client::b64url_decode(h.split_whitespace().nth(3).unwrap()).unwrap(),
        )
        .unwrap();
        assert!(
            device::verify_transcript(&ed_pub, "upload", &["ch1", "3", "CTHASH"], ts, &sig).is_ok()
        );

        // download：channel_id, part_index
        let h = peri_sig_header(&store, &device_id, "download", &["ch1", "3"], ts).unwrap();
        let sig = ed25519_dalek::Signature::from_slice(
            &crate::sync::http_client::b64url_decode(h.split_whitespace().nth(3).unwrap()).unwrap(),
        )
        .unwrap();
        assert!(device::verify_transcript(&ed_pub, "download", &["ch1", "3"], ts, &sig).is_ok());

        // confirm/revoke：channel_id
        let h = peri_sig_header(&store, &device_id, "confirm", &["ch1"], ts).unwrap();
        let sig = ed25519_dalek::Signature::from_slice(
            &crate::sync::http_client::b64url_decode(h.split_whitespace().nth(3).unwrap()).unwrap(),
        )
        .unwrap();
        assert!(device::verify_transcript(&ed_pub, "confirm", &["ch1"], ts, &sig).is_ok());
        let h = peri_sig_header(&store, &device_id, "revoke", &["ch1"], ts).unwrap();
        let sig = ed25519_dalek::Signature::from_slice(
            &crate::sync::http_client::b64url_decode(h.split_whitespace().nth(3).unwrap()).unwrap(),
        )
        .unwrap();
        assert!(device::verify_transcript(&ed_pub, "revoke", &["ch1"], ts, &sig).is_ok());
    }

    #[test]
    fn test_exponential_backoff() {
        let b = ExponentialBackoff;
        // 有 Retry-After：直接采用（封顶）。
        assert_eq!(b.delay(1, Some(5)), Duration::from_secs(5));
        assert_eq!(b.delay(3, Some(5)), Duration::from_secs(5));
        assert_eq!(b.delay(1, Some(9999)), Duration::from_secs(30)); // 封顶 30s
        assert_eq!(b.delay(1, Some(0)), Duration::from_secs(1)); // 0 也至少 1s
        // 无 Retry-After：指数退避（500ms、1s、2s…封顶 30s）。
        assert_eq!(b.delay(1, None), Duration::from_millis(500));
        assert_eq!(b.delay(2, None), Duration::from_millis(1000));
        assert_eq!(b.delay(3, None), Duration::from_millis(2000));
        assert_eq!(b.delay(7, None), Duration::from_secs(30)); // 封顶
        // 429 重试次数上限为冻结值。
        assert_eq!(MAX_429_RETRIES, 3);
    }

    #[test]
    fn test_api_error_display_redacts() {
        use crate::sync::http_client::ApiError;
        let e = ApiError::new(ApiErrorKind::Transport, "network error");
        assert_eq!(e.kind, ApiErrorKind::Transport);
        assert_eq!(e.retry_after_secs, None);
        let s = e.to_string();
        assert!(!s.contains("http"), "错误消息不得含 URL");
        let e2 = ApiError::with_retry_after(ApiErrorKind::RateLimited, "RATE_LIMITED", Some(3));
        assert_eq!(e2.retry_after_secs, Some(3));
    }

    /// 内存 SecretStore（测试用；材料 drop 前不会泄露）。
    struct TestStore {
        material: KeyMaterial,
    }

    impl crate::sync::keystore::SecretStore for TestStore {
        fn sign(&self, msg: &[u8]) -> anyhow::Result<ed25519_dalek::Signature> {
            use ed25519_dalek::Signer;
            Ok(self.material.ed25519.sign(msg))
        }

        fn x25519_private(&self) -> anyhow::Result<x25519_dalek::StaticSecret> {
            Ok(self.material.x25519.clone())
        }
    }
}
