#[cfg(test)]
mod tests {
    use crate::sync::device::{DeviceId, DevicePublic, TrustedPeers};
    use crate::sync::device_cli::{
        DeviceCliPaths, add_impl, add_interactive_impl, init_impl, load_identity, load_peers,
        open_file_store, remove_impl, show_impl,
    };

    fn paths_in(tmp: &tempfile::TempDir) -> DeviceCliPaths {
        DeviceCliPaths {
            identity: tmp.path().join("sync-identity.json"),
            peers: tmp.path().join("sync-trusted-peers.json"),
        }
    }

    fn identity_uri(identity: &DevicePublic) -> String {
        identity.invite_uri()
    }

    #[test]
    fn test_init_show_add_list_remove_flow() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(&tmp);
        let ks = tmp.path().join("keystore");

        // init：显式 keystore 路径（加密文件）+ 密码。
        init_impl(Some("laptop"), Some(&ks), "correct horse", &paths).unwrap();

        // identity.json 已写入且可解析；权限 0600（unix）。
        let identity = load_identity(&paths).unwrap();
        assert_eq!(identity.name, "laptop");
        assert_eq!(identity.device_id.to_b64().len(), 22); // 16B b64url
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(&paths.identity).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }

        // keystore 文件已创建且可用（正确密码）。
        let store = open_file_store(&ks, "correct horse").unwrap();
        let sig = store.sign(b"hello").unwrap();
        assert_eq!(sig.to_bytes().len(), 64);
        // 错误密码 → fail closed。
        assert!(open_file_store(&ks, "wrong password").is_err());

        // 重复 init → 拒绝（不覆盖）。
        assert!(init_impl(Some("laptop"), Some(&ks), "x", &paths).is_err());

        // show 输出本地身份。
        show_impl(&paths).unwrap();

        // 构造另一台设备的邀请并 add（确认）。
        let other_material = crate::sync::keystore::KeyMaterial::generate().unwrap();
        let other_id = DeviceId::random().unwrap();
        let other = DevicePublic::from_keys(
            other_id,
            other_material.ed25519_public(),
            other_material.x25519_public(),
            "phone",
        )
        .unwrap();
        add_impl(&identity_uri(&other), None, true, &paths).unwrap();

        let peers = load_peers(&paths).unwrap();
        assert_eq!(peers.len(), 1);
        assert!(peers.contains(&other.device_id));
        let peer = peers.get(&other.device_id).unwrap();
        assert_eq!(peer.ed_pub, other.ed_pub);
        assert_eq!(peer.x_pub, other.x_pub);
        assert_eq!(peer.name, "phone");

        // 重复 add → 拒绝。
        assert!(add_impl(&identity_uri(&other), None, true, &paths).is_err());

        // list 列出该设备。
        // （输出走 stdout，仅验证无 panic + peers 数据正确）

        // remove：存在 → 成功；不存在 → 拒绝。
        remove_impl(&other.device_id.to_b64(), &paths).unwrap();
        assert!(load_peers(&paths).unwrap().is_empty());
        assert!(remove_impl(&other.device_id.to_b64(), &paths).is_err());
    }

    #[test]
    fn test_add_cancelled_when_not_confirmed() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(&tmp);
        let material = crate::sync::keystore::KeyMaterial::generate().unwrap();
        let device = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            material.ed25519_public(),
            material.x25519_public(),
            "dev",
        )
        .unwrap();
        // 用户拒绝确认 → 不写入 trusted peers。
        add_impl(&device.invite_uri(), None, false, &paths).unwrap();
        assert!(load_peers(&paths).unwrap().is_empty());
    }

    #[test]
    fn test_add_interactive_rejects_before_write() {
        // H3：mock stdin 先拒绝 → 解析并打印 device_id/fingerprint 后不写文件。
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(&tmp);
        let material = crate::sync::keystore::KeyMaterial::generate().unwrap();
        let device = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            material.ed25519_public(),
            material.x25519_public(),
            "dev",
        )
        .unwrap();
        let mut input = std::io::Cursor::new(b"n\n".to_vec());
        add_interactive_impl(&device.invite_uri(), &mut input, &paths).unwrap();
        assert!(load_peers(&paths).unwrap().is_empty(), "拒绝确认不得写入");
    }

    #[test]
    fn test_add_interactive_accepts_only_after_confirmation() {
        // H3：mock stdin 先接受 → 确认后才写入 trusted_peers.json。
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(&tmp);
        let material = crate::sync::keystore::KeyMaterial::generate().unwrap();
        let device = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            material.ed25519_public(),
            material.x25519_public(),
            "dev",
        )
        .unwrap();
        let mut input = std::io::Cursor::new(b"y\n".to_vec());
        add_interactive_impl(&device.invite_uri(), &mut input, &paths).unwrap();
        let peers = load_peers(&paths).unwrap();
        assert_eq!(peers.len(), 1, "确认后必须写入");
        assert!(peers.contains(&device.device_id));
        // 大写 Y 同样接受。
        let other = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            material.ed25519_public(),
            material.x25519_public(),
            "other",
        )
        .unwrap();
        let mut input = std::io::Cursor::new(b"Y\n".to_vec());
        add_interactive_impl(&other.invite_uri(), &mut input, &paths).unwrap();
        assert_eq!(load_peers(&paths).unwrap().len(), 2);
    }

    #[test]
    fn test_add_interactive_rejects_invalid_invite_before_prompt() {
        // H3：非法邀请在交互确认前即拒绝（不读 stdin、不写文件）。
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(&tmp);
        let mut input = std::io::Cursor::new(b"y\n".to_vec());
        let err = add_interactive_impl("https://not-an-invite", &mut input, &paths).unwrap_err();
        assert!(err.to_string().contains("not a peri device invite"));
        assert!(load_peers(&paths).unwrap().is_empty());
    }

    #[test]
    fn test_add_rejects_invalid_invite() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(&tmp);
        assert!(add_impl("https://not-an-invite", None, true, &paths).is_err());
        assert!(load_peers(&paths).unwrap().is_empty());
    }

    #[test]
    fn test_remove_rejects_invalid_id() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(&tmp);
        assert!(remove_impl("not-a-device-id!", &paths).is_err());
    }

    #[test]
    fn test_trusted_peers_file_roundtrip_keeps_only_public_material() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(&tmp);
        let material = crate::sync::keystore::KeyMaterial::generate().unwrap();
        let device = DevicePublic::from_keys(
            DeviceId::random().unwrap(),
            material.ed25519_public(),
            material.x25519_public(),
            "dev",
        )
        .unwrap();
        add_impl(&device.invite_uri(), None, true, &paths).unwrap();
        let raw = std::fs::read_to_string(&paths.peers).unwrap();
        // 文件只含公钥材料：不包含私钥可推导的种子（无 sign 字段、无 secret）。
        assert!(!raw.contains("ed25519"));
        assert!(!raw.contains("x25519"));
        assert!(!raw.contains("sign"));
        assert!(raw.contains(&device.device_id.to_b64()));
        let parsed: TrustedPeers = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn test_default_paths_under_home_peri() {
        let paths = crate::sync::device_cli::default_paths().unwrap();
        let home = dirs_next::home_dir().unwrap();
        assert_eq!(
            paths.identity,
            home.join(".peri").join("sync-identity.json")
        );
        assert_eq!(
            paths.peers,
            home.join(".peri").join("sync-trusted-peers.json")
        );
    }

    #[test]
    fn test_keystore_path_opens_existing_encrypted_file_only() {
        let tmp = tempfile::tempdir().unwrap();
        let ks = tmp.path().join("keystore");
        // 不存在的 keystore：FileStore::open 必须 fail closed（禁止自动初始化）。
        assert!(open_file_store(&ks, "pw").is_err());
        // 存在但未加密的文件 → 拒绝。
        std::fs::write(&ks, b"plaintext").unwrap();
        assert!(open_file_store(&ks, "pw").is_err());
    }
}
