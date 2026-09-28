use super::*;

/// 主 plan §8 第 3 行（A4/A6/A20）的终态断言：四工具 `direct: false`，
/// deferred 目录按有效配置出现。
///
/// 两个观察面分别断言，不作字面集合相等：
/// - **直连面**：`ModelRequest.tools` 不得出现任何 `mcp__cron__*` / `mcp__lsp__*`；
/// - **deferred 目录面**：`SearchExtraTools` 命中名单必须包含 cron 三工具的
///   effective name；`mcp__lsp__LSP` 必须**当且仅当** LSP 生效配置非空时出现。
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
    const LSP_TOOL: &str = "mcp__lsp__LSP";
    /// wave 1 已迁实例的正控制（恒注入、恒 direct）。
    const WEB_TOOL: &str = "WebSearch";

    // ── A：LSP 生效配置**非空** ──────────────────────────────────────────────
    let dirs = FixtureDirs::new();
    let fixture =
        BuiltinHostFixture::start(dirs, &[lsp_server_config("wave2_configured")], false).await;
    for instance in ["web", "artifact", "cron", "lsp"] {
        fixture.assert_instance_connected(instance);
    }
    let faces = probe_tool_faces(
        "cron+lsp（配置非空）",
        fixture
            .session_context("mcp-v4-wave2-final-configured")
            .await,
        "cron lsp",
    )
    .await;
    // 直连面：四工具一律 direct:false（A4）——`system_mcp_tools == []` 不提升 direct。
    for name in CRON_TOOLS.iter().copied().chain(std::iter::once(LSP_TOOL)) {
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
    // deferred 目录面：cron 三工具 + lsp 工具都必须在搜索/执行面可达。
    for name in CRON_TOOLS.iter().copied().chain(std::iter::once(LSP_TOOL)) {
        assert!(
            faces.searched.iter().any(|tool| tool == name),
            "deferred 目录（搜索面）必须包含 {name}: {:?}",
            faces.searched
        );
    }
    // 裸名不得残留（A20 的 XOR 判据：迁移后只有 effective name）。
    for bare in ["cron_register", "cron_list", "cron_remove", "LSP"] {
        assert!(
            !faces.searched.iter().any(|tool| tool == bare),
            "迁移后裸名 `{bare}` 不得留在搜索面（XOR 的另一半）: {:?}",
            faces.searched
        );
    }

    // ── B：LSP 生效配置**为空** ⇒ 实例仍 ready，但工具面为空表 ──────────────
    let empty_dirs = FixtureDirs::new();
    let empty_fixture = BuiltinHostFixture::start(empty_dirs, &[], false).await;
    for instance in ["cron", "lsp"] {
        empty_fixture.assert_instance_connected(instance);
    }
    let empty_faces = probe_tool_faces(
        "cron（无 LSP 配置）",
        empty_fixture
            .session_context("mcp-v4-wave2-final-empty")
            .await,
        "cron lsp",
    )
    .await;
    for name in CRON_TOOLS {
        assert!(
            empty_faces.searched.iter().any(|tool| tool == name),
            "无 LSP 配置不得影响 cron 工具面（两实例工具面独立）: 缺 {name} in {:?}",
            empty_faces.searched
        );
    }
    assert!(
        !empty_faces
            .searched
            .iter()
            .any(|tool| tool.starts_with("mcp__lsp__")),
        "无 LSP 生效配置时 deferred 目录不得含 `mcp__lsp__*`（A6：可见但空）: {:?}",
        empty_faces.searched
    );
    for name in empty_faces.direct.iter() {
        assert!(
            !name.starts_with("mcp__lsp__"),
            "无 LSP 配置时直连面同样不得含 `mcp__lsp__*`: {name}"
        );
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// W3（V-03）终态收口：ready 闸门矩阵 / A33 注入序 / off 零注入 / wave 1 兼容
// ══════════════════════════════════════════════════════════════════════════════
