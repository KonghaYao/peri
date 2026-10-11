use super::*;

/// 主 plan §8 第 3 行（A4/A6/A20）的终态断言：cron 工具 `direct: false`，
/// deferred 目录按有效配置出现。
///
/// 两个观察面分别断言，不作字面集合相等：
/// - **直连面**：`ModelRequest.tools` 不得出现任何 `mcp__cron__*`；
/// - **deferred 目录面**：`SearchExtraTools` 命中名单必须包含 cron 三工具的
///   effective name。
///
/// 观察面非空洞的正控制（否则「不含」可能只是因为 pool 里根本没有 builtin 实例）：
/// 同一次运行里 `WebSearch` 必须可见（选中的 system MCP 原名恒 direct）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn wave2_final_first_request_and_deferred_summary() {
    const CRON_TOOLS: [&str; 3] = [
        "mcp__cron__cron_register",
        "mcp__cron__cron_list",
        "mcp__cron__cron_remove",
    ];
    /// wave 1 已迁实例的正控制（恒注入、恒 direct）。
    const WEB_TOOL: &str = "WebSearch";

    let dirs = FixtureDirs::new();
    let fixture = BuiltinHostFixture::start(dirs, false).await;
    for instance in ["web", "artifact", "cron"] {
        fixture.assert_instance_connected(instance);
    }
    let faces = probe_tool_faces(
        "cron",
        fixture
            .session_context("mcp-v4-wave2-final-configured")
            .await,
        "cron",
    )
    .await;
    // 直连面：cron 工具一律 direct:false（A4）——`system_mcp_tools == []` 不提升 direct。
    for name in CRON_TOOLS {
        assert!(
            !faces.direct.iter().any(|tool| tool == name),
            "首个 LLM 请求的直连参数不得包含 {name}（A4: direct:false）: {:?}",
            faces.direct
        );
    }
    assert!(
        faces.direct.iter().any(|tool| tool == WEB_TOOL),
        "正控制：wave 1 的 direct 实例必须仍可见（否则本用例的「不含」是因为断言面空洞）: {:?}",
        faces.direct
    );
    // deferred 目录面：cron 三工具都必须在搜索/执行面可达。
    for name in CRON_TOOLS {
        assert!(
            faces.searched.iter().any(|tool| tool == name),
            "deferred 目录（搜索面）必须包含 {name}: {:?}",
            faces.searched
        );
    }
    // 裸名不得残留（A20 的 XOR 判据：迁移后只有 effective name）。
    for bare in ["cron_register", "cron_list", "cron_remove"] {
        assert!(
            !faces.searched.iter().any(|tool| tool == bare),
            "迁移后裸名 `{bare}` 不得留在搜索面（XOR 的另一半）: {:?}",
            faces.searched
        );
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// W3（V-03）终态收口：ready 闸门矩阵 / A33 注入序 / off 零注入 / wave 1 兼容
// ══════════════════════════════════════════════════════════════════════════════
