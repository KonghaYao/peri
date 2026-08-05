#[cfg(test)]
mod tests {
    use crate::sync::limits::{
        CODE_COLLISION_MAX_RETRIES, CODE_LOOKUP_RATE_LIMIT_PER_MIN, CODE_REGISTER_MAX_PER_MIN,
        MAX_DEVICE_NAME_CHARS, MAX_MANIFEST_BYTES, MAX_PART_BYTES, MAX_PARTS_PER_CHANNEL,
        MAX_PAYLOAD_BYTES, SIGNATURE_SKEW_SECS, TTL_CREATED_SECS, TTL_JOINED_SECS, TTL_READY_SECS,
        TTL_TOMBSTONE_SECS, validate_device_name, validate_manifest, validate_part_size,
    };

    #[test]
    fn test_manifest_budget_boundaries() {
        // 恰好等于上限：允许。
        validate_manifest(MAX_PARTS_PER_CHANNEL, MAX_PAYLOAD_BYTES).expect("边界内应通过");
        validate_manifest(0, 0).expect("空 manifest 应通过");
        // 超过任一上限：拒绝。
        assert!(validate_manifest(MAX_PARTS_PER_CHANNEL + 1, 0).is_err());
        assert!(validate_manifest(0, MAX_PAYLOAD_BYTES + 1).is_err());
        assert!(validate_manifest(MAX_PARTS_PER_CHANNEL + 1, MAX_PAYLOAD_BYTES + 1).is_err());
    }

    #[test]
    fn test_part_size_boundaries() {
        validate_part_size(MAX_PART_BYTES).expect("等于上限应通过");
        assert!(validate_part_size(MAX_PART_BYTES + 1).is_err());
    }

    #[test]
    fn test_device_name_boundaries() {
        assert!(validate_device_name("x").is_ok());
        let name = "x".repeat(MAX_DEVICE_NAME_CHARS);
        assert!(validate_device_name(&name).is_ok());
        let long = "x".repeat(MAX_DEVICE_NAME_CHARS + 1);
        assert!(validate_device_name(&long).is_err());
        assert!(validate_device_name("").is_err());
        // 按字符数而非字节数（CJK 名称）。
        assert!(validate_device_name(&"设".repeat(MAX_DEVICE_NAME_CHARS)).is_ok());
        assert!(validate_device_name(&"设".repeat(MAX_DEVICE_NAME_CHARS + 1)).is_err());
    }

    #[test]
    fn test_frozen_contract_constants() {
        // plan 冻结的数值：改动必须经 review。
        assert_eq!(SIGNATURE_SKEW_SECS, 300);
        assert_eq!(TTL_CREATED_SECS, 600); // created +10min
        assert_eq!(TTL_JOINED_SECS, 300); // join/handshake +5min
        assert_eq!(TTL_READY_SECS, 3600); // ready +1h
        assert_eq!(TTL_TOMBSTONE_SECS, 3600); // tombstone +1h
        assert_eq!(CODE_REGISTER_MAX_PER_MIN, 2);
        assert_eq!(CODE_COLLISION_MAX_RETRIES, 1);
        const { assert!(CODE_LOOKUP_RATE_LIMIT_PER_MIN >= 15) }
        const { assert!(MAX_MANIFEST_BYTES > 0) }
    }
}
