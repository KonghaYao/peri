#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, Verifier};
    use x25519_dalek::StaticSecret;

    use crate::sync::keystore::{
        FileStore, KEYSTORE_MAGIC, KEYSTORE_PBKDF2_ITERATIONS, KEYSTORE_SALT_LEN, KeyMaterial,
        KeystoreSource, SecretStore, default_keystore_path, resolve_source,
    };

    fn material_with_known_keys() -> KeyMaterial {
        KeyMaterial {
            ed25519: ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]),
            x25519: StaticSecret::from([9u8; 32]),
        }
    }

    #[test]
    fn test_file_store_roundtrip_and_signing() {
        let dir = tempfile::tempdir().expect("tempdir 应成功");
        let path = dir.path().join("keystore.bin");
        let material = KeyMaterial::generate().expect("生成应成功");

        FileStore::create(&path, "correct horse battery", &material).expect("创建应成功");
        let store = FileStore::open(&path, "correct horse battery").expect("打开应成功");

        // 签名一致且可验证。
        let msg = b"peri-sync/v1|create|abc|1715000000";
        let sig = store.sign(msg).expect("签名应成功");
        material
            .ed25519_public()
            .verify(msg, &sig)
            .expect("签名应由同一 Ed25519 私钥产生");

        // X25519 私钥与公钥一致。
        let x_pub_from_material = material.x25519_public();
        let x_pub_from_store = x25519_dalek::PublicKey::from(&store.x25519_private().unwrap());
        assert_eq!(x_pub_from_material.to_bytes(), x_pub_from_store.to_bytes());
    }

    #[test]
    fn test_file_store_fail_closed_missing() {
        let dir = tempfile::tempdir().expect("tempdir 应成功");
        let path = dir.path().join("missing.bin");
        let err = FileStore::open(&path, "pw").unwrap_err().to_string();
        assert!(
            err.contains("does not exist"),
            "缺失文件必须 fail closed: {err}"
        );
    }

    #[test]
    fn test_file_store_wrong_password_fails() {
        let dir = tempfile::tempdir().expect("tempdir 应成功");
        let path = dir.path().join("keystore.bin");
        FileStore::create(&path, "right", &material_with_known_keys()).expect("创建应成功");
        assert!(
            FileStore::open(&path, "wrong").is_err(),
            "错误密码必须 fail closed"
        );
    }

    #[test]
    fn test_file_store_garbage_and_magic_fails() {
        let dir = tempfile::tempdir().expect("tempdir 应成功");
        let short = dir.path().join("short.bin");
        std::fs::write(&short, b"garbage").unwrap();
        assert!(FileStore::open(&short, "pw").is_err(), "过短文件必须失败");

        let bad_magic = dir.path().join("magic.bin");
        std::fs::write(&bad_magic, vec![0xAAu8; 128]).unwrap();
        let err = FileStore::open(&bad_magic, "pw").unwrap_err().to_string();
        assert!(
            err.contains("not a peri-sync keystore"),
            "魔数不符必须失败: {err}"
        );
    }

    #[test]
    fn test_file_store_create_rejects_existing() {
        let dir = tempfile::tempdir().expect("tempdir 应成功");
        let path = dir.path().join("keystore.bin");
        FileStore::create(&path, "pw", &material_with_known_keys()).expect("创建应成功");
        assert!(
            FileStore::create(&path, "pw2", &material_with_known_keys()).is_err(),
            "已存在文件必须拒绝覆盖"
        );
    }

    #[test]
    fn test_file_store_tamper_detection() {
        let dir = tempfile::tempdir().expect("tempdir 应成功");
        let path = dir.path().join("keystore.bin");
        FileStore::create(&path, "pw", &material_with_known_keys()).expect("创建应成功");
        let mut raw = std::fs::read(&path).unwrap();

        // 篡改密文区。
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        std::fs::write(&path, &raw).unwrap();
        assert!(
            FileStore::open(&path, "pw").is_err(),
            "篡改密文必须认证失败"
        );

        // 篡改 envelope 版本字节。
        std::fs::write(&path, &raw).unwrap();
        let version_off = KEYSTORE_MAGIC.len() + KEYSTORE_SALT_LEN;
        raw[version_off] = 2;
        std::fs::write(&path, &raw).unwrap();
        // 版本字节在解密前校验，`crypto::open` 明确报告版本错误。
        // 注意：不可先 `to_string()`（只取最外层），必须用 `{:#}` 展开 anyhow 链。
        let full = format!("{:#}", FileStore::open(&path, "pw").unwrap_err());
        assert!(full.contains("version"), "版本不符必须明确报告: {full}");
    }

    #[cfg(unix)]
    #[test]
    fn test_file_store_permissions_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir 应成功");
        let path = dir.path().join("keystore.bin");
        FileStore::create(&path, "pw", &material_with_known_keys()).expect("创建应成功");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "keystore 文件权限必须为 0600");
    }

    #[test]
    fn test_key_material_debug_redacted() {
        let material = material_with_known_keys();
        let debug = format!("{material:?}");
        assert!(
            !debug.contains("BwcH"),
            "Debug 不得泄露 ed25519 seed 的 base64"
        );
        assert!(
            !debug.contains("CQkJCQ"),
            "Debug 不得泄露 x25519 secret 的 base64"
        );
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn test_file_store_debug_redacted() {
        let dir = tempfile::tempdir().expect("tempdir 应成功");
        let path = dir.path().join("keystore.bin");
        FileStore::create(&path, "pw", &material_with_known_keys()).expect("创建应成功");
        let store = FileStore::open(&path, "pw").expect("打开应成功");
        let debug = format!("{store:?}");
        assert!(debug.contains("FileStore"));
        assert!(!debug.contains("BwcH"), "Debug 不得泄露密钥材料");
    }

    #[test]
    fn test_resolve_source_all_branches() {
        let dir = tempfile::tempdir().expect("tempdir 应成功");
        let explicit = dir.path().join("explicit.bin");

        // 显式路径 → File（无论 keyring/TTY）。
        assert_eq!(
            resolve_source(Some(&explicit), true, false).unwrap(),
            KeystoreSource::File(explicit.clone())
        );
        assert_eq!(
            resolve_source(Some(&explicit), false, false).unwrap(),
            KeystoreSource::File(explicit.clone())
        );

        // keyring 可用 → Keyring。
        assert_eq!(
            resolve_source(None, true, false).unwrap(),
            KeystoreSource::Keyring
        );
        assert_eq!(
            resolve_source(None, true, true).unwrap(),
            KeystoreSource::Keyring
        );

        // keyring 不可用 + TTY → 默认加密文件回退。
        let fallback = resolve_source(None, false, true).expect("有 TTY 应回退文件");
        assert_eq!(
            fallback,
            KeystoreSource::File(default_keystore_path().unwrap())
        );

        // keyring 不可用 + 无 TTY → fail closed。
        let err = resolve_source(None, false, false).unwrap_err().to_string();
        assert!(
            err.contains("fail") || err.contains("plaintext"),
            "必须 fail closed: {err}"
        );
    }

    #[test]
    fn test_default_keystore_path_shape() {
        let path = default_keystore_path().expect("home 目录应可确定");
        assert_eq!(path.file_name().unwrap().to_str(), Some("sync-keystore"));
        assert!(path.to_string_lossy().contains(".peri"));
    }

    #[test]
    fn test_pbkdf2_iterations_sane() {
        // 计划冻结值 600k（03-plan §已冻结的安全语义）；测试下限与冻结值同步，
        // 防止未来回退到弱迭代次数。
        const {
            assert!(
                KEYSTORE_PBKDF2_ITERATIONS >= 600_000,
                "PBKDF2 迭代次数不得低于 600k"
            )
        }
    }

    #[test]
    fn test_sign_transcript_via_store() {
        use crate::sync::device::{sign_transcript, verify_transcript};
        let dir = tempfile::tempdir().expect("tempdir 应成功");
        let path = dir.path().join("keystore.bin");
        let material = material_with_known_keys();
        let store = FileStore::create(&path, "pw", &material).expect("创建应成功");
        let sig =
            sign_transcript(&store, "create", &["ch_1", "dev_2"], 1715000000).expect("签名应成功");
        let pubkey = material.ed25519_public();
        verify_transcript(&pubkey, "create", &["ch_1", "dev_2"], 1715000000, &sig)
            .expect("验证应成功");
        // 篡改任一字段/时间戳/操作均失败。
        assert!(
            verify_transcript(&pubkey, "create", &["ch_2", "dev_2"], 1715000000, &sig).is_err()
        );
        assert!(
            verify_transcript(&pubkey, "create", &["ch_1", "dev_2"], 1715000001, &sig).is_err()
        );
        assert!(verify_transcript(&pubkey, "join", &["ch_1", "dev_2"], 1715000000, &sig).is_err());
    }

    #[test]
    fn test_rfc8032_test1_fixed_vector() {
        // RFC 8032 §7.1 TEST 1：空消息签名固定向量，钉住底层 Ed25519 原语。
        let seed: [u8; 32] = [
            0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec,
            0x2c, 0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03,
            0x1c, 0xae, 0x7f, 0x60,
        ];
        let expected: [u8; 64] = [
            0xe5, 0x56, 0x43, 0x00, 0xc3, 0x60, 0xac, 0x72, 0x90, 0x86, 0xe2, 0xcc, 0x80, 0x6e,
            0x82, 0x8a, 0x84, 0x87, 0x7f, 0x1e, 0xb8, 0xe5, 0xd9, 0x74, 0xd8, 0x73, 0xe0, 0x65,
            0x22, 0x49, 0x01, 0x55, 0x5f, 0xb8, 0x82, 0x15, 0x90, 0xa3, 0x3b, 0xac, 0xc6, 0x1e,
            0x39, 0x70, 0x1c, 0xf9, 0xb4, 0x6b, 0xd2, 0x5b, 0xf5, 0xf0, 0x59, 0x5b, 0xbe, 0x24,
            0x65, 0x51, 0x41, 0x43, 0x8e, 0x7a, 0x10, 0x0b,
        ];
        let material = KeyMaterial {
            ed25519: ed25519_dalek::SigningKey::from_bytes(&seed),
            x25519: StaticSecret::from([0u8; 32]),
        };
        let sig = material.ed25519.sign(b"");
        assert_eq!(sig.to_bytes(), expected, "RFC 8032 TEST 1 向量不符");
        material
            .ed25519_public()
            .verify(b"", &sig)
            .expect("自验证应成功");
    }
}
