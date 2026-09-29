//! W1 资源面经 dispatch 的转发取证（`dispatch_test.rs` 的子模块——拆分原因与
//! `dispatch_factory_test.rs` 同：父文件按 STD-SIZE-001 保持拆分粒度）。
//!
//! W1 用例仍由本模块夹具经 `with_resources` 给出 handler（那波不启用宿主投递），断言的是
//! 分发层转发语义。**W4a（2026-09-29）后生产工厂 arm 已接线**：`ctx.workspace_resources`
//! 为 `Some` 时 dispatch 调 `with_resources`、为 `None` 时保持「资源面未接线」，两条语义
//! 各有具名用例（`w4a_*`），且都走 `connect_via_dispatch` 的**生产工厂路径**。

use super::*;
use rmcp::model::{ReadResourceRequestParams, ResourceContents};

/// W4a 覆盖面用的段落 ID（∈ 契约 `SECTION_IDS`）与其正文哨兵。
const W4A_META_SECTION: &str = "01_intro";
const W4A_META_URI: &str = "peri-meta://workspace/01_intro";
/// 正文哨兵：逐字比对用（含换行，证明字节不被 trim）。
const W4A_META_BODY: &str = "W4A-META-OVERRIDE-LINE-1\nW4A-META-OVERRIDE-LINE-2\n";

/// 建一个只含 `.peri/meta/{section}.md` 的 cwd，返回其文本路径。
fn w4a_meta_cwd(dir: &tempfile::TempDir) -> String {
    let meta_dir = dir.path().join(".peri").join("meta");
    std::fs::create_dir_all(&meta_dir).expect("建 .peri/meta");
    std::fs::write(
        meta_dir.join(format!("{W4A_META_SECTION}.md")),
        W4A_META_BODY,
    )
    .expect("写覆盖文档");
    dir.path().to_string_lossy().to_string()
}

// ══════════════════════════════════════════════════════════════════════════════
// W1：资源面经 dispatch 的完整转发与其余实例的诚实退化（plan §8.1 W1 行）
// ══════════════════════════════════════════════════════════════════════════════

/// W1 验收：`BuiltinServerHandler` 对 workspace 资源面的**完整转发**（`resources/list`、
/// `resources/templates/list`、`skills/list|get`），以及其余 builtin **不错误声明
/// resources**（能力位缺失 + 方法级诚实拒绝，而不是伪装成空成功）。
///
/// 证据边界（诚实声明）：资源输入由本夹具直接经 `with_resources` 装配（W1 的夹具形态，
/// 用于覆盖技能根这类生产 W4a 尚未装载的输入）；本用例断言的是分发层转发语义，不是
/// 「生产已把本地技能改经 resources 读取」（那是 W4 的消费切换）。生产工厂路径的装载语义
/// 由下面的 `w4a_*` 两条覆盖。
#[tokio::test]
async fn w1_workspace_resource_surface_forwards_through_dispatch() {
    let fixture = CrossFixture::new();
    let skills = tempfile::tempdir().expect("tempdir");
    let skill_dir = skills.path().join("alpha");
    std::fs::create_dir_all(&skill_dir).expect("建技能目录");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: alpha\ndescription: W1 dispatch 转发夹具\n---\n正文\n",
    )
    .expect("写 SKILL.md");
    let entry_uri = "skill://project/alpha/SKILL.md";

    let handler = BuiltinServerHandler::Workspace(
        WorkspaceMcpServer::new(fixture.ctx.cwd.clone(), None).with_resources(
            WorkspaceResourcesInput::new()
                .with_skill_root(ResourceRoot::new(skills.path(), ResourceScope::Project))
                .with_disable_bundled(true),
        ),
    );
    let pair = connect_handler("workspace", handler).await;
    let peer = pair.peer();

    // resources/list：既有 git ref 与夹具技能同批出现。
    let resources = peer
        .list_all_resources()
        .await
        .expect("resources/list 必须成功");
    let uris: Vec<String> = resources
        .iter()
        .map(|resource| resource.uri.clone())
        .collect();
    assert!(
        uris.contains(&"workspace://git/ref".to_string()),
        "既有 git ref 资源必须保留：{uris:?}"
    );
    assert!(
        uris.contains(&entry_uri.to_string()),
        "技能资源必须经 dispatch 可见：{uris:?}"
    );

    // resources/templates/list：trait 默认实现是空表 ⇒ 非空即证明转发到了 workspace handler。
    let templates = peer
        .list_resource_templates(None)
        .await
        .expect("resources/templates/list 必须成功");
    assert!(
        templates
            .resource_templates
            .iter()
            .any(|template| template.uri_template == "skill://project/{name}/SKILL.md"),
        "技能模板必须来自 workspace handler：{:?}",
        templates.resource_templates
    );

    // skills/list：custom 面的默认语义是 -32601 ⇒ 正常响应即证明转发；条目形状按契约锁定。
    let result = peer
        .send_request(ClientRequest::CustomRequest(CustomRequest::new(
            "skills/list",
            None,
        )))
        .await
        .expect("skills/list 必须经 dispatch 成功");
    let value = match result {
        ServerResult::CustomResult(custom) => custom.0,
        other => panic!("期望 CustomResult，实际：{other:?}"),
    };
    assert_eq!(
        value["skills"][0]["uri"].as_str(),
        Some(entry_uri),
        "skills/list 首项必须是夹具技能：{value}"
    );

    // skills/get 未知 URI：MCPP 约定 -32602（不是空成功，也不是通用内部错误）。
    let error = peer
        .send_request(ClientRequest::CustomRequest(CustomRequest::new(
            "skills/get",
            Some(json!({ "uri": "skill://project/nope/SKILL.md" })),
        )))
        .await
        .expect_err("未知 skills/get URI 必须失败");
    match error {
        ServiceError::McpError(error) => {
            assert_eq!(
                error.code,
                ErrorCode::INVALID_PARAMS,
                "未知技能按约定回 -32602"
            );
        }
        other => panic!("未知技能期望 McpError，实际 {other:?}"),
    }

    pair.shutdown().await;

    // 其余实例（web）：不错误声明 resources；未接线的方法面按 -32601 诚实拒绝。
    let web = connect_via_dispatch("web", &fixture.ctx).await;
    let web_peer = web.peer();
    let info = web_peer
        .peer_info()
        .expect("modern 握手后 peer_info 必须是 Some");
    assert!(
        info.capabilities.resources.is_none(),
        "web 不得声明 resources 能力（否则资源发现会打到错误实例）"
    );
    let error = web_peer
        .send_request(ClientRequest::CustomRequest(CustomRequest::new(
            "skills/list",
            None,
        )))
        .await
        .expect_err("web 的 skills/* 必须失败");
    match error {
        ServiceError::McpError(error) => {
            assert_eq!(
                error.code,
                ErrorCode::METHOD_NOT_FOUND,
                "方法不支持必须回 -32601"
            );
        }
        other => panic!("web 的 skills/* 期望 McpError，实际 {other:?}"),
    }
    web.shutdown().await;
}

// ══════════════════════════════════════════════════════════════════════════════
// W4a：资源面输入经**生产工厂路径**装载（`ctx.workspace_resources` 两个取值）
// ══════════════════════════════════════════════════════════════════════════════

/// W4a：`ctx.workspace_resources` 为 `Some` 时，生产工厂 arm 把输入转交给
/// `WorkspaceMcpServer::with_resources` —— `peri-meta://` 段落覆盖经 dispatch 可见、可读、
/// 逐字返回；既有 git ref 面保留；技能面（本波不接）保持关闭。
///
/// 证据边界（诚实声明）：本用例断言的是**工厂装载语义**（输入来自上下文、一次装配），
/// 不是「宿主会话经 MCP 消费覆盖正文」的端到端结论——后者在 `peri-acp` 的
/// `requests_meta_resources_test` 用例中以生产 `session/new` 路径取证。
#[tokio::test]
async fn w4a_workspace_resources_from_context_make_meta_surface_visible() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = w4a_meta_cwd(&dir);
    let ctx = BuiltinInstanceContext::new(cwd)
        .with_workspace_resources(WorkspaceResourcesInput::new().with_disable_bundled(true));
    let pair = connect_via_dispatch("workspace", &ctx).await;
    let peer = pair.peer();

    let resources = peer
        .list_all_resources()
        .await
        .expect("resources/list 必须成功");
    let uris: Vec<String> = resources
        .iter()
        .map(|resource| resource.uri.clone())
        .collect();
    assert!(
        uris.contains(&"workspace://git/ref".to_string()),
        "既有 git ref 资源面必须保留：{uris:?}"
    );
    assert!(
        uris.contains(&W4A_META_URI.to_string()),
        "装卸载后 peri-meta:// 必须可见：{uris:?}"
    );
    assert!(
        !uris.iter().any(|uri| uri.starts_with("skill://")),
        "W4a 只接 meta 面：技能面必须保持关闭（无 skill 根 + builtin 关闭）：{uris:?}"
    );

    // 覆盖文档逐字可读（换行与末尾不 trim）。
    let read = peer
        .read_resource(ReadResourceRequestParams::new(W4A_META_URI))
        .await
        .expect("覆盖文档必须可读");
    match read.contents.first() {
        Some(ResourceContents::TextResourceContents { text, .. }) => {
            assert_eq!(text, W4A_META_BODY, "覆盖正文必须逐字返回");
        }
        other => panic!("期望文本正文，实际 {other:?}"),
    }

    pair.shutdown().await;
}

/// W4a：`ctx.workspace_resources` 为 `None` 时保持「资源面未接线」（本槽位之前的既有
/// 行为）——`resources/list` 只有 git ref，`peri-meta://` 不可得；`None` 与「空目录集」
/// 必须可区分（skills/* 仍是 -32601，不是空成功）。
#[tokio::test]
async fn w4a_workspace_without_resources_input_keeps_provider_unwired() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = w4a_meta_cwd(&dir);
    // 文档在盘且可读，但上下文不装载资源输入 ⇒ 资源面未接线。
    let ctx = BuiltinInstanceContext::new(cwd);
    let pair = connect_via_dispatch("workspace", &ctx).await;
    let peer = pair.peer();

    let resources = peer
        .list_all_resources()
        .await
        .expect("resources/list 必须成功");
    let uris: Vec<String> = resources
        .iter()
        .map(|resource| resource.uri.clone())
        .collect();
    assert!(
        uris.contains(&"workspace://git/ref".to_string()),
        "既有 git ref 资源面必须保留：{uris:?}"
    );
    assert!(
        !uris.iter().any(|uri| uri.starts_with("peri-meta://")),
        "未装载资源输入时不得出现 peri-meta://（None = 未接线）：{uris:?}"
    );

    let error = peer
        .send_request(ClientRequest::CustomRequest(CustomRequest::new(
            "skills/list",
            None,
        )))
        .await
        .expect_err("未接线时 skills/* 必须失败");
    match error {
        ServiceError::McpError(error) => {
            assert_eq!(
                error.code,
                ErrorCode::METHOD_NOT_FOUND,
                "未接线必须回 -32601，而不是空列表"
            );
        }
        other => panic!("未接线的 skills/* 期望 McpError，实际 {other:?}"),
    }

    pair.shutdown().await;
}
