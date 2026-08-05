#[cfg(test)]
mod tests {
    use crate::sync::canonical::{b64url_nopad, context, timestamp_within_skew, transcript};

    #[test]
    fn test_transcript_fixed_vector() {
        // 固定向量：字段顺序、分隔符与时间戳必须逐字节稳定（TS Worker 复刻）。
        assert_eq!(
            transcript("create", &["ch_abc", "dev_xyz"], 1715000000).unwrap(),
            "peri-sync/v1|create|ch_abc|dev_xyz|1715000000"
        );
        assert_eq!(
            transcript("join", &["code_1", "ch_abc"], 0).unwrap(),
            "peri-sync/v1|join|code_1|ch_abc|0"
        );
    }

    #[test]
    fn test_transcript_empty_fields() {
        assert_eq!(
            transcript("revoke", &[], 42).unwrap(),
            "peri-sync/v1|revoke|42"
        );
    }

    #[test]
    fn test_transcript_rejects_malformed_input() {
        // 入站校验以错误返回而非 panic（Low-4）：空 op、空字段、含 `|`
        // 的 op/字段/尾部一律拒绝。
        assert!(transcript("", &["a"], 1).is_err(), "空 op 必须拒绝");
        assert!(
            transcript("op|evil", &["a"], 1).is_err(),
            "op 含 `|` 必须拒绝"
        );
        assert!(transcript("op", &[""], 1).is_err(), "空字段必须拒绝");
        assert!(transcript("op", &["a", ""], 1).is_err(), "空字段必须拒绝");
        assert!(
            transcript("op", &["a|b"], 1).is_err(),
            "字段含 `|` 必须拒绝"
        );
        assert!(context("op", &["a", ""]).is_err(), "context 空字段必须拒绝");
        assert!(context("op", &[]).is_ok(), "无字段 context 合法");
        assert!(context("op", &["a", "b"]).is_ok(), "合法输入不受影响");
    }

    #[test]
    fn test_context_fixed_vector() {
        assert_eq!(
            context("payload", &["c1", "3", "HASH"]).unwrap(),
            "peri-sync/v1|payload|c1|3|HASH"
        );
        assert_eq!(
            context("keystore", &["salt"]).unwrap(),
            "peri-sync/v1|keystore|salt"
        );
    }

    #[test]
    fn test_transcript_tamper_detection() {
        let base = transcript("create", &["ch_abc", "dev_xyz"], 1715000000).unwrap();
        // 任意字段/操作/时间戳变化都必须改变 transcript。
        assert_ne!(
            base,
            transcript("create", &["ch_abz", "dev_xyz"], 1715000000).unwrap()
        );
        assert_ne!(
            base,
            transcript("create", &["ch_abc", "dev_xyy"], 1715000000).unwrap()
        );
        assert_ne!(
            base,
            transcript("join", &["ch_abc", "dev_xyz"], 1715000000).unwrap()
        );
        assert_ne!(
            base,
            transcript("create", &["ch_abc", "dev_xyz"], 1715000001).unwrap()
        );
    }

    #[test]
    fn test_b64url_nopad_fixed_vector() {
        assert_eq!(b64url_nopad(b"hello"), "aGVsbG8");
        assert_eq!(b64url_nopad(b""), "");
        assert_eq!(b64url_nopad(&[0xFB, 0xFF, 0xFE]), "-__-"); // URL-safe 字符集，无 padding
        assert_eq!(b64url_nopad(&[0xFB, 0xFF, 0xFE, 0x00]), "-__-AA");
    }

    #[test]
    fn test_timestamp_within_skew_boundaries() {
        // 恰好 ±300 秒边界内为真。
        assert!(timestamp_within_skew(1000, 1300, 300));
        assert!(timestamp_within_skew(1000, 700, 300));
        assert!(timestamp_within_skew(1000, 1000, 300));
        // 超过边界为假。
        assert!(!timestamp_within_skew(1000, 1301, 300));
        assert!(!timestamp_within_skew(1000, 699, 300));
        // u64 下溢安全。
        assert!(timestamp_within_skew(0, 0, 0));
        assert!(!timestamp_within_skew(0, 1, 0));
    }
}
