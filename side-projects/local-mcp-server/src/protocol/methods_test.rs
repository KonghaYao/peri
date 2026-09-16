//! `protocol::methods` 的单元测试（WP-005）。

use std::collections::HashSet;

use super::{
    classify_unrouted, is_implemented, UnroutedRequest, IMPLEMENTED_METHODS, LEGACY_ONLY_METHODS,
    MODERN_ONLY_METHODS,
};

#[test]
fn test_method_inventory_is_unique_and_namespaced() {
    let unique: HashSet<&&str> = IMPLEMENTED_METHODS.iter().collect();
    assert_eq!(
        unique.len(),
        IMPLEMENTED_METHODS.len(),
        "方法清单不得有重复项"
    );
    for method in IMPLEMENTED_METHODS {
        assert!(
            method.contains('/') || method.chars().all(|c| c.is_ascii_lowercase()),
            "方法名必须是小写（可带 `/` 命名空间）：{method}"
        );
    }
}

#[test]
fn test_inventory_covers_every_advertised_surface() {
    // tools 能力、resources 能力、生命周期方法都必须有实现路径。
    for method in [
        "tools/list",
        "tools/call",
        "resources/list",
        "resources/read",
        "initialize",
        "ping",
        "server/discover",
    ] {
        assert!(is_implemented(method), "{method} 必须被实现");
    }
    // 两代订阅路径各有归属，且不重叠。
    for method in LEGACY_ONLY_METHODS {
        assert!(is_implemented(method), "{method} 必须被实现");
    }
    for method in MODERN_ONLY_METHODS {
        assert!(is_implemented(method), "{method} 必须被实现");
    }
    for legacy in LEGACY_ONLY_METHODS {
        assert!(
            !MODERN_ONLY_METHODS.contains(&legacy),
            "{legacy} 不能同时属于两代专属清单"
        );
    }
}

#[test]
fn test_unknown_method_is_not_found_while_bad_shape_is_invalid_params() {
    // 两个失败的归因必须不同：调用方写错方法 vs 写错参数。
    assert_eq!(
        classify_unrouted("tools/list"),
        UnroutedRequest::MalformedParams
    );
    assert_eq!(
        classify_unrouted("tools/call"),
        UnroutedRequest::MalformedParams
    );
    assert_eq!(
        classify_unrouted("sandbox/nonexistent"),
        UnroutedRequest::NotImplemented
    );
    assert!(
        !is_implemented("notifications/cancelled"),
        "通知不是请求方法"
    );
}
