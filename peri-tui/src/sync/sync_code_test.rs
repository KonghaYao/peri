#[cfg(test)]
mod tests {
    use crate::sync::sync_code::{
        ALPHABET, CODE_CHARS, MAX_CODE_VALUE, SyncCode, epoch, normalize,
    };

    /// 所有位置均为 1 的值（对应码 "11111111"）。
    const ALL_ONES: u64 = 0x0008_4210_8421;

    #[test]
    fn test_roundtrip_display_and_parse() {
        for value in [0u64, 1, ALL_ONES, MAX_CODE_VALUE - 1] {
            let code = SyncCode::from_value(value).expect("值应在 40-bit 范围内");
            let display = code.display();
            assert_eq!(display.len(), CODE_CHARS + 1, "显示格式应为 XXXX-XXXX");
            assert_eq!(display.as_bytes()[4], b'-');
            assert_eq!(
                SyncCode::parse(&display).expect("解析应成功").value(),
                value,
                "display 应可往返"
            );
            assert_eq!(
                SyncCode::parse(&code.normalized())
                    .expect("解析应成功")
                    .value(),
                value,
                "normalized 应可往返"
            );
        }
    }

    #[test]
    fn test_display_format() {
        assert_eq!(SyncCode::from_value(0).unwrap().display(), "0000-0000");
        assert_eq!(
            SyncCode::from_value(ALL_ONES).unwrap().display(),
            "1111-1111"
        );
    }

    #[test]
    fn test_normalize_case_and_hyphens() {
        assert_eq!(normalize("7m4k-p9xq").unwrap(), "7M4KP9XQ");
        assert_eq!(normalize("7M4K-P9XQ").unwrap(), "7M4KP9XQ");
        assert_eq!(normalize("  7M4KP9XQ  ").unwrap(), "7M4KP9XQ");
        assert_eq!(normalize("12345678").unwrap(), "12345678");
    }

    #[test]
    fn test_normalize_ambiguity_mapping() {
        // O→0、I/L→1，且输出必须是规范字母表字符。
        assert_eq!(normalize("o0o0-1i1l").unwrap(), "00001111");
        assert_eq!(normalize("iiiiiiii").unwrap(), "11111111");
        assert_eq!(normalize("llllllll").unwrap(), "11111111");
        // 语义等价：解析结果相同。
        assert_eq!(
            SyncCode::parse("o0o0-1i1l").unwrap().value(),
            SyncCode::parse("0000-1111").unwrap().value()
        );
        assert_eq!(SyncCode::parse("llllllll").unwrap().value(), ALL_ONES);
    }

    #[test]
    fn test_rejects_invalid_input() {
        // U 拒绝。
        assert!(normalize("U2345678").is_err());
        assert!(normalize("7M4K-P9XU").is_err());
        assert!(SyncCode::parse("7M4K-P9XU").is_err());
        // 长度不符。
        assert!(normalize("1234567").is_err());
        assert!(normalize("123456789").is_err());
        assert!(normalize("1234-5678-9").is_err());
        // 内部空格/其它非法字符。
        assert!(normalize("1234 5678").is_err());
        assert!(normalize("!2345678").is_err());
        assert!(SyncCode::parse("").is_err());
    }

    #[test]
    fn test_from_value_bounds() {
        assert!(SyncCode::from_value(0).is_ok());
        assert!(SyncCode::from_value(MAX_CODE_VALUE - 1).is_ok());
        assert!(SyncCode::from_value(MAX_CODE_VALUE).is_err());
        assert!(SyncCode::from_value(MAX_CODE_VALUE + 1).is_err());
    }

    #[test]
    fn test_generate_bounds_and_uniqueness() {
        let a = SyncCode::generate().expect("生成应成功");
        let b = SyncCode::generate().expect("生成应成功");
        assert!(a.value() < MAX_CODE_VALUE);
        assert!(b.value() < MAX_CODE_VALUE);
        assert_ne!(a.value(), b.value(), "40-bit 随机碰撞概率可忽略");
    }

    #[test]
    fn test_epoch_boundaries() {
        // 30 秒 epoch 边界。
        assert_eq!(epoch(0), 0);
        assert_eq!(epoch(29), 0);
        assert_eq!(epoch(30), 1);
        assert_eq!(epoch(59), 1);
        assert_eq!(epoch(60), 2);
        assert_eq!(epoch(90), 3);
    }

    #[test]
    fn test_alphabet_excludes_i_l_o_u() {
        let s = String::from_utf8(ALPHABET.to_vec()).unwrap();
        assert_eq!(s.len(), 32);
        for c in ['I', 'L', 'O', 'U'] {
            assert!(!s.contains(c), "Crockford 字母表不得包含 {c}");
        }
    }
}
