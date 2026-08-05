#[cfg(test)]
mod tests {
    use ed25519_dalek::VerifyingKey;
    use x25519_dalek::PublicKey as XPublicKey;

    use crate::sync::device::{
        DeviceId, DevicePublic, TrustedPeer, TrustedPeers, sign_transcript, verify_transcript,
    };
    use crate::sync::keystore::{FileStore, KeyMaterial};

    fn device(name: &str) -> DevicePublic {
        DevicePublic::from_keys(
            DeviceId::from_b64("AQIDBAUGBwgJCgsMDQ4PEA").unwrap(),
            VerifyingKey::from_bytes(&[0x11; 32]).unwrap(),
            XPublicKey::from([0x22; 32]),
            name,
        )
        .unwrap()
    }

    // ── DeviceId ──

    #[test]
    fn test_device_id_b64_roundtrip() {
        let id = DeviceId::from_b64("AQIDBAUGBwgJCgsMDQ4PEA").unwrap();
        assert_eq!(id.to_b64(), "AQIDBAUGBwgJCgsMDQ4PEA");
        assert_eq!(DeviceId::from_b64(&id.to_b64()).unwrap(), id);
        assert_eq!(id.to_string(), "AQIDBAUGBwgJCgsMDQ4PEA");
        // 错误长度/非法字符拒绝。
        assert!(DeviceId::from_b64("AQIDBAUGBwgJCgsMDQ4P").is_err());
        assert!(DeviceId::from_b64("!!!").is_err());
    }

    #[test]
    fn test_device_id_serde_json() {
        let id = DeviceId::random().unwrap();
        let json = serde_json::to_string(&id).unwrap();
        let back: DeviceId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
        // 非法长度失败。
        assert!(serde_json::from_str::<DeviceId>("\"AQID\"").is_err());
    }

    #[test]
    fn test_device_id_random_unique() {
        assert_ne!(DeviceId::random().unwrap(), DeviceId::random().unwrap());
    }

    // ── DevicePublic / identity.json ──

    #[test]
    fn test_device_public_serde_json_roundtrip() {
        let dev = device("macbook-pro");
        let json = serde_json::to_string(&dev).unwrap();
        let back: DevicePublic = serde_json::from_str(&json).unwrap();
        assert_eq!(back, dev);
        // identity.json 只含公钥：无任何私钥字段。
        assert!(!json.contains("secret"));
        assert!(!json.contains("private"));
    }

    #[test]
    fn test_device_name_validation() {
        assert!(
            DevicePublic::from_keys(
                DeviceId::random().unwrap(),
                VerifyingKey::from_bytes(&[1u8; 32]).unwrap(),
                XPublicKey::from([2u8; 32]),
                "",
            )
            .is_err()
        );
        let long = "x".repeat(65);
        assert!(
            DevicePublic::from_keys(
                DeviceId::random().unwrap(),
                VerifyingKey::from_bytes(&[1u8; 32]).unwrap(),
                XPublicKey::from([2u8; 32]),
                &long,
            )
            .is_err()
        );
    }

    #[test]
    fn test_key_conversions() {
        let dev = device("x");
        assert_eq!(dev.ed_verifying_key().unwrap().to_bytes(), [0x11; 32]);
        assert_eq!(dev.x_public().to_bytes(), [0x22; 32]);
    }

    // ── fingerprint ──

    #[test]
    fn test_fingerprint_shape_and_consistency() {
        let dev = device("a");
        let fp = dev.fingerprint();
        // 8 组 4 位 hex，以 '-' 连接。
        let groups: Vec<&str> = fp.split('-').collect();
        assert_eq!(groups.len(), 8);
        for g in &groups {
            assert_eq!(g.len(), 4);
            assert!(g.chars().all(|c| c.is_ascii_hexdigit()));
        }
        // 相同设备指纹一致；公钥变化则指纹变化。
        assert_eq!(device("a").fingerprint(), dev.fingerprint());
        let mut other = device("b");
        other.ed_pub[0] ^= 0x01;
        assert_ne!(other.fingerprint(), dev.fingerprint());
        let mut other2 = device("c");
        other2.x_pub[0] ^= 0x01;
        assert_ne!(other2.fingerprint(), dev.fingerprint());
        // TrustedPeer 指纹与 DevicePublic 一致。
        let peer = TrustedPeer::from_device(&dev, 1715000000);
        assert_eq!(peer.fingerprint(), fp);
    }

    // ── 邀请 URI ──

    #[test]
    fn test_invite_uri_roundtrip() {
        let dev = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            VerifyingKey::from_bytes(&[0x33; 32]).unwrap(),
            XPublicKey::from([0x44; 32]),
            "我的 Mac #1 & co%",
        )
        .unwrap();
        let uri = dev.invite_uri();
        assert!(uri.starts_with("peri://device/"));
        let parsed = DevicePublic::parse_invite_uri(&uri).expect("邀请应可解析");
        assert_eq!(parsed, dev);
    }

    #[test]
    fn test_invite_uri_simple_name() {
        let dev = device("mbp");
        let parsed = DevicePublic::parse_invite_uri(&dev.invite_uri()).unwrap();
        assert_eq!(parsed, dev);
        assert_eq!(parsed.name, "mbp");
    }

    #[test]
    fn test_invite_uri_rejects_malformed() {
        let dev = device("x");
        let uri = dev.invite_uri();
        // 非设备邀请。
        assert!(DevicePublic::parse_invite_uri("https://example.com/x").is_err());
        // 缺查询参数。
        assert!(DevicePublic::parse_invite_uri(uri.split('?').next().unwrap()).is_err());
        // 缺 ed / x / n。
        let no_ed = format!(
            "peri://device/{}?x={}&n=abc",
            dev.device_id.to_b64(),
            crate::sync::canonical::b64url_nopad(&dev.x_pub)
        );
        assert!(DevicePublic::parse_invite_uri(&no_ed).is_err());
        let no_x = format!(
            "peri://device/{}?ed={}&n=abc",
            dev.device_id.to_b64(),
            crate::sync::canonical::b64url_nopad(&dev.ed_pub)
        );
        assert!(DevicePublic::parse_invite_uri(&no_x).is_err());
        let no_n = format!(
            "peri://device/{}?ed={}&x={}",
            dev.device_id.to_b64(),
            crate::sync::canonical::b64url_nopad(&dev.ed_pub),
            crate::sync::canonical::b64url_nopad(&dev.x_pub)
        );
        assert!(DevicePublic::parse_invite_uri(&no_n).is_err());
        // ed 长度非法。
        let bad_ed = format!(
            "peri://device/{}?ed=AAAA&x={}&n=abc",
            dev.device_id.to_b64(),
            crate::sync::canonical::b64url_nopad(&dev.x_pub)
        );
        assert!(DevicePublic::parse_invite_uri(&bad_ed).is_err());
        // 空设备名。
        let empty_n = format!(
            "peri://device/{}?ed={}&x={}&n=",
            dev.device_id.to_b64(),
            crate::sync::canonical::b64url_nopad(&dev.ed_pub),
            crate::sync::canonical::b64url_nopad(&dev.x_pub)
        );
        assert!(DevicePublic::parse_invite_uri(&empty_n).is_err());
        // 非法百分号转义。
        let bad_esc = format!(
            "peri://device/{}?ed={}&x={}&n=ab%zz",
            dev.device_id.to_b64(),
            crate::sync::canonical::b64url_nopad(&dev.ed_pub),
            crate::sync::canonical::b64url_nopad(&dev.x_pub)
        );
        assert!(DevicePublic::parse_invite_uri(&bad_esc).is_err());
    }

    // ── TrustedPeers ──

    #[test]
    fn test_trusted_peers_add_remove() {
        let mut peers = TrustedPeers::default();
        assert!(peers.is_empty());
        let dev = device("mac");
        peers
            .add(TrustedPeer::from_device(&dev, 1715000000))
            .expect("添加应成功");
        assert!(peers.contains(&dev.device_id));
        assert_eq!(peers.len(), 1);
        assert_eq!(peers.get(&dev.device_id).unwrap().name, "mac");
        // 重复添加拒绝（必须先 untrust）。
        assert!(
            peers
                .add(TrustedPeer::from_device(&dev, 1715000001))
                .is_err()
        );
        // untrust 后可再信任。
        assert!(peers.remove(&dev.device_id));
        assert!(!peers.contains(&dev.device_id));
        assert!(!peers.remove(&dev.device_id));
        peers
            .add(TrustedPeer::from_device(&dev, 1715000001))
            .expect("untrust 后应可重新信任");
    }

    #[test]
    fn test_trusted_peers_load_missing_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let peers = TrustedPeers::load(&dir.path().join("nope.json")).expect("缺失文件应为空");
        assert!(peers.is_empty());
    }

    #[test]
    fn test_trusted_peers_save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted_peers.json");
        let alpha = device("alpha");
        let mut beta = device("beta");
        // `device()` 使用固定 device_id，这里必须区分两个设备。
        beta.device_id = DeviceId::random().unwrap();
        let mut peers = TrustedPeers::default();
        peers.add(TrustedPeer::from_device(&alpha, 1)).unwrap();
        peers.add(TrustedPeer::from_device(&beta, 2)).unwrap();
        peers.save(&path).expect("保存应成功");
        let loaded = TrustedPeers::load(&path).expect("加载应成功");
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains(&alpha.device_id));
        assert!(loaded.contains(&beta.device_id));
        assert_eq!(loaded.get(&beta.device_id).unwrap().name, "beta");
    }

    #[test]
    fn test_trusted_peers_load_invalid_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(TrustedPeers::load(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_trusted_peers_save_permissions_0600_and_atomic_replace() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted_peers.json");
        let mut peers = TrustedPeers::default();
        peers
            .add(TrustedPeer::from_device(&device("alpha"), 1))
            .unwrap();
        peers.save(&path).expect("保存应成功");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "trusted peers 文件权限必须为 0600");
        // 原子替换：再次保存覆盖已有文件仍成功且内容完整。
        let mut peers2 = TrustedPeers::default();
        let mut beta = device("beta");
        beta.device_id = DeviceId::random().unwrap();
        peers2.add(TrustedPeer::from_device(&beta, 2)).unwrap();
        peers2.save(&path).expect("覆盖保存应成功");
        let loaded = TrustedPeers::load(&path).expect("加载应成功");
        assert_eq!(loaded.len(), 1);
        assert!(loaded.contains(&beta.device_id));
    }

    // ── transcript 签名 ──

    fn store_with_material(material: &KeyMaterial) -> FileStore {
        let dir = tempfile::tempdir().unwrap();
        FileStore::create(&dir.path().join("ks.bin"), "pw", material).unwrap()
    }

    #[test]
    fn test_sign_verify_transcript_deterministic() {
        let material = KeyMaterial::generate().unwrap();
        let store = store_with_material(&material);
        let sig1 = sign_transcript(&store, "create", &["ch_abc", "dev_xyz"], 1715000000).unwrap();
        let sig2 = sign_transcript(&store, "create", &["ch_abc", "dev_xyz"], 1715000000).unwrap();
        assert_eq!(sig1.to_bytes(), sig2.to_bytes(), "同输入必须产生同签名");
        let pubkey = material.ed25519_public();
        verify_transcript(&pubkey, "create", &["ch_abc", "dev_xyz"], 1715000000, &sig1)
            .expect("验证应成功");
    }

    #[test]
    fn test_verify_transcript_tamper_fails() {
        let material = KeyMaterial::generate().unwrap();
        let store = store_with_material(&material);
        let sig = sign_transcript(&store, "create", &["ch_abc", "dev_xyz"], 1715000000).unwrap();
        let pubkey = material.ed25519_public();
        // 字段顺序、字段值、操作、时间戳任何变化都失败。
        assert!(
            verify_transcript(&pubkey, "create", &["ch_abz", "dev_xyz"], 1715000000, &sig).is_err()
        );
        assert!(
            verify_transcript(&pubkey, "create", &["dev_xyz", "ch_abc"], 1715000000, &sig).is_err()
        );
        assert!(
            verify_transcript(&pubkey, "create", &["ch_abc", "dev_xyz"], 1715000001, &sig).is_err()
        );
        assert!(
            verify_transcript(&pubkey, "join", &["ch_abc", "dev_xyz"], 1715000000, &sig).is_err()
        );
    }

    #[test]
    fn test_verify_transcript_wrong_key_fails() {
        let material = KeyMaterial::generate().unwrap();
        let store = store_with_material(&material);
        let sig = sign_transcript(&store, "create", &["ch"], 1).unwrap();
        let other = KeyMaterial::generate().unwrap();
        assert!(verify_transcript(&other.ed25519_public(), "create", &["ch"], 1, &sig).is_err());
    }
}
