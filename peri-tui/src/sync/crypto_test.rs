#[cfg(test)]
mod tests {
    use crate::sync::{crypto, protocol};

    // ── crypto 测试 ──

    #[test]
    fn test_derive_key_deterministic() {
        let key1 = crypto::derive_key("123456");
        let key2 = crypto::derive_key("123456");
        assert_eq!(key1, key2, "相同配对码应产生相同密钥");
    }

    #[test]
    fn test_derive_key_different_codes() {
        let key1 = crypto::derive_key("111111");
        let key2 = crypto::derive_key("222222");
        assert_ne!(key1, key2, "不同配对码应产生不同密钥");
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = crypto::derive_key("482917");
        let plaintext = b"Hello, world! This is a test message.";
        let encrypted = crypto::encrypt(plaintext, &key);
        assert_eq!(
            encrypted.len(),
            crypto::IV_LEN + plaintext.len() + 16,
            "密文长度 = IV + 明文 + AuthTag"
        );
        let decrypted = crypto::decrypt(&encrypted, &key).expect("解密应成功");
        assert_eq!(decrypted, plaintext, "解密结果应与原文一致");
    }

    #[test]
    fn test_decrypt_wrong_key_fails() {
        let key1 = crypto::derive_key("111111");
        let key2 = crypto::derive_key("222222");
        let encrypted = crypto::encrypt(b"secret", &key1);
        let result = crypto::decrypt(&encrypted, &key2);
        assert!(result.is_err(), "错误密钥解密应失败");
    }

    #[test]
    fn test_decrypt_truncated_data_fails() {
        let key = crypto::derive_key("123456");
        let truncated = vec![0u8; 5]; // 小于 IV_LEN (12)
        let result = crypto::decrypt(&truncated, &key);
        assert!(result.is_err(), "密文过短应返回错误");
    }

    // ── protocol 测试 ──

    #[test]
    fn test_ws_message_serde_request_pair() {
        let msg = protocol::WsMessage::RequestPair;
        let json = serde_json::to_string(&msg).expect("序列化应成功");
        assert!(json.contains(r#""type":"request_pair""#));
        let deserialized: protocol::WsMessage =
            serde_json::from_str(&json).expect("反序列化应成功");
        assert!(matches!(deserialized, protocol::WsMessage::RequestPair));
    }

    #[test]
    fn test_ws_message_serde_join_pair() {
        let msg = protocol::WsMessage::JoinPair {
            pair_code: "482917".into(),
        };
        let json = serde_json::to_string(&msg).expect("序列化应成功");
        assert!(json.contains("482917"));
        let deserialized: protocol::WsMessage =
            serde_json::from_str(&json).expect("反序列化应成功");
        match deserialized {
            protocol::WsMessage::JoinPair { pair_code } => {
                assert_eq!(pair_code, "482917");
            }
            _ => panic!("应反序列化为 JoinPair"),
        }
    }

    #[test]
    fn test_ws_message_serde_data_chunk() {
        let msg = protocol::WsMessage::DataChunk {
            seq: 1,
            data: vec![0xAB, 0xCD, 0xEF],
        };
        let json = serde_json::to_string(&msg).expect("序列化应成功");
        let deserialized: protocol::WsMessage =
            serde_json::from_str(&json).expect("反序列化应成功");
        match deserialized {
            protocol::WsMessage::DataChunk { seq, data } => {
                assert_eq!(seq, 1);
                assert_eq!(data, vec![0xAB, 0xCD, 0xEF]);
            }
            _ => panic!("应反序列化为 DataChunk"),
        }
    }

    #[test]
    fn test_ws_message_serde_error() {
        let msg = protocol::WsMessage::Error {
            code: "PAIR_INVALID".into(),
            message: "无效或已过期的配对码".into(),
        };
        let json = serde_json::to_string(&msg).expect("序列化应成功");
        let deserialized: protocol::WsMessage =
            serde_json::from_str(&json).expect("反序列化应成功");
        match deserialized {
            protocol::WsMessage::Error { code, message } => {
                assert_eq!(code, "PAIR_INVALID");
                assert!(message.contains("配对码"));
            }
            _ => panic!("应反序列化为 Error"),
        }
    }

    #[test]
    fn test_sync_package_rmp_serde() {
        use protocol::{FileEntry, FilesItem, SettingsItem, SyncItems, SyncPackage};
        let items = SyncItems {
            settings: Some(SettingsItem {
                content: r#"{"key": "value"}"#.into(),
                claude_content: None,
            }),
            skills: Some(FilesItem {
                files: vec![FileEntry {
                    path: "my-skill/SKILL.md".into(),
                    content: b"# My Skill".to_vec(),
                }],
            }),
            mcp: None,
            plugins: None,
        };
        let pkg = SyncPackage {
            version: 1,
            timestamp: 1715000000,
            items,
        };
        let packed = rmp_serde::to_vec(&pkg).expect("MessagePack 序列化应成功");
        let unpacked: SyncPackage =
            rmp_serde::from_slice(&packed).expect("MessagePack 反序列化应成功");
        assert_eq!(unpacked.version, 1);
        assert_eq!(unpacked.timestamp, 1715000000);
        assert!(unpacked.items.settings.is_some());
        let skills = unpacked.items.skills.expect("skills 应存在");
        assert_eq!(skills.files.len(), 1);
        assert_eq!(skills.files[0].path, "my-skill/SKILL.md");
    }
}

// ── 新协议 API（v1）测试：data-key + versioned AAD envelope ──
#[cfg(test)]
mod v1_tests {
    use crate::sync::crypto::{
        DataKey, ENVELOPE_HEADER_LEN, ENVELOPE_VERSION, IV_LEN, open, payload_aad, seal,
    };

    const CHANNEL: &str = "CH1234567890abcdef";
    const MANIFEST_HASH: [u8; 32] = [0xAB; 32];

    #[test]
    fn test_payload_aad_fixed_vector() {
        let aad = payload_aad(CHANNEL, 5, &MANIFEST_HASH).unwrap();
        assert_eq!(
            String::from_utf8(aad).unwrap(),
            format!(
                "peri-sync/v1|payload|{CHANNEL}|5|{}",
                crate::sync::canonical::b64url_nopad(&MANIFEST_HASH)
            )
        );
        // part index 变化必须改变 AAD。
        assert_ne!(
            payload_aad(CHANNEL, 5, &MANIFEST_HASH).unwrap(),
            payload_aad(CHANNEL, 6, &MANIFEST_HASH).unwrap()
        );
        // channel 变化必须改变 AAD。
        assert_ne!(
            payload_aad(CHANNEL, 5, &MANIFEST_HASH).unwrap(),
            payload_aad("OTHER", 5, &MANIFEST_HASH).unwrap()
        );
    }

    #[test]
    fn test_seal_open_roundtrip() {
        let key = DataKey::random().expect("data key 生成应成功");
        let aad = payload_aad(CHANNEL, 3, &MANIFEST_HASH).unwrap();
        let plaintext = b"hello peri-sync v1";
        let envelope = seal(&key, &aad, plaintext);
        assert_eq!(envelope.len(), ENVELOPE_HEADER_LEN + plaintext.len() + 16);
        assert_eq!(envelope[0], ENVELOPE_VERSION);
        let opened = open(&key, &aad, &envelope).expect("解密应成功");
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn test_open_wrong_key_fails() {
        let key1 = DataKey::random().unwrap();
        let key2 = DataKey::random().unwrap();
        let aad = payload_aad(CHANNEL, 0, &MANIFEST_HASH).unwrap();
        let envelope = seal(&key1, &aad, b"secret");
        assert!(open(&key2, &aad, &envelope).is_err(), "错误密钥必须失败");
    }

    #[test]
    fn test_open_wrong_aad_fails() {
        let key = DataKey::random().unwrap();
        let envelope = seal(
            &key,
            &payload_aad(CHANNEL, 1, &MANIFEST_HASH).unwrap(),
            b"secret",
        );
        // 不同 part index / channel / manifest hash 的 AAD 均不能解开。
        assert!(
            open(
                &key,
                &payload_aad(CHANNEL, 2, &MANIFEST_HASH).unwrap(),
                &envelope
            )
            .is_err()
        );
        assert!(
            open(
                &key,
                &payload_aad("OTHER", 1, &MANIFEST_HASH).unwrap(),
                &envelope
            )
            .is_err()
        );
        let other_hash = [0xCD; 32];
        assert!(
            open(
                &key,
                &payload_aad(CHANNEL, 1, &other_hash).unwrap(),
                &envelope
            )
            .is_err()
        );
    }

    #[test]
    fn test_open_tampered_ciphertext_fails() {
        let key = DataKey::random().unwrap();
        let aad = payload_aad(CHANNEL, 0, &MANIFEST_HASH).unwrap();
        let mut envelope = seal(&key, &aad, b"payload");
        let last = envelope.len() - 1;
        envelope[last] ^= 0x01;
        assert!(open(&key, &aad, &envelope).is_err(), "篡改密文必须认证失败");
    }

    #[test]
    fn test_open_bad_version_fails() {
        let key = DataKey::random().unwrap();
        let aad = payload_aad(CHANNEL, 0, &MANIFEST_HASH).unwrap();
        let mut envelope = seal(&key, &aad, b"payload");
        envelope[0] = ENVELOPE_VERSION + 1;
        let err = open(&key, &aad, &envelope).unwrap_err().to_string();
        assert!(err.contains("version"), "版本错误必须明确报告");
    }

    #[test]
    fn test_open_too_short_fails() {
        let key = DataKey::random().unwrap();
        let aad = payload_aad(CHANNEL, 0, &MANIFEST_HASH).unwrap();
        assert!(open(&key, &aad, &[]).is_err());
        assert!(open(&key, &aad, &[ENVELOPE_VERSION]).is_err());
        assert!(open(&key, &aad, &[ENVELOPE_VERSION; ENVELOPE_HEADER_LEN - 1]).is_err());
    }

    #[test]
    fn test_data_key_shape_and_debug_redaction() {
        let key = DataKey::random().unwrap();
        assert_eq!(key.as_array().len(), 32);
        // Debug 必须脱敏（精确匹配，不泄露任何密钥材料）。
        assert_eq!(format!("{key:?}"), "DataKey(\"[REDACTED]\")");
        let key2 = DataKey::from_array(*key.as_array());
        assert_eq!(key.as_array(), key2.as_array());
    }

    #[test]
    fn test_envelope_layout_constants() {
        assert_eq!(ENVELOPE_HEADER_LEN, 1 + IV_LEN);
        assert_eq!(IV_LEN, 12);
    }
}
