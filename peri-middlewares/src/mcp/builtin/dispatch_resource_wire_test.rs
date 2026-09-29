//! W1 资源面经 dispatch 的转发取证（`dispatch_test.rs` 的子模块——拆分原因与
//! `dispatch_factory_test.rs` 同：父文件按 STD-SIZE-001 保持拆分粒度）。
//!
//! 资源输入只能由本模块夹具经 `with_resources` 装配（W1 不启用宿主投递，生产工厂 arm
//! 不接线资源面），因此这里不走 `connect_via_dispatch` 的工厂路径，而用父模块的
//! [`connect_handler`] 直接给出 handler。

use super::*;

// ══════════════════════════════════════════════════════════════════════════════
// W1：资源面经 dispatch 的完整转发与其余实例的诚实退化（plan §8.1 W1 行）
// ══════════════════════════════════════════════════════════════════════════════

/// W1 验收：`BuiltinServerHandler` 对 workspace 资源面的**完整转发**（`resources/list`、
/// `resources/templates/list`、`skills/list|get`），以及其余 builtin **不错误声明
/// resources**（能力位缺失 + 方法级诚实拒绝，而不是伪装成空成功）。
///
/// 证据边界（诚实声明）：资源输入由本夹具直接经 `with_resources` 装配——W1 **不启用宿主
/// 投递**，生产工厂 arm 不接线资源面；本用例断言的是分发层转发语义，不是「生产已把本地
/// 技能改经 resources 读取」（那是 W2/W4 的消费切换）。
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
