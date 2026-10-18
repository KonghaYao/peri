use super::*;
use peri_agent::tools::{EffectiveToolError, EffectiveToolErrorCode};

#[test]
fn test_agent_schema_only_promises_supported_inputs() {
    let tool = make_subagent_tool(vec![]);
    let params = tool.parameters();
    let props = params["properties"].as_object().unwrap();
    assert!(!props.contains_key("name"));
    assert!(!props.contains_key("isolation"));
    assert!(props["description"]["description"]
        .as_str()
        .unwrap()
        .contains("仅在省略 prompt"));
    assert!(props["cwd"]["description"]
        .as_str()
        .unwrap()
        .contains("canonicalization"));
    for text in [
        props["model"]["description"].as_str().unwrap(),
        tool.description(),
    ] {
        assert!(text.contains("当前磁盘上的 Agent 定义"));
        assert!(text.contains("不保留先前的 model override"));
        assert!(text.contains("参数也被忽略"));
        assert!(!text.contains("original execution context"));
    }
}

/// [回归测试] 非 null isolation 曾被静默忽略；所有派发路径须在副作用前拒绝。
#[tokio::test]
async fn test_isolation_rejected_before_any_dispatch() {
    let host = HostFixture::open("fixture-isolation-rejection").await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let tool = host.bind(SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            crate::subagent::test_support::fixture_source(Arc::new(EchoLLM), "fixture-scripted")
        }),
        host.cwd.clone(),
    ));
    let before = host
        .fixture
        .facade()
        .list_children(&host.parent_id)
        .await
        .unwrap();
    for mode in [
        serde_json::json!({"subagent_type": "test-agent"}),
        serde_json::json!({"fork": true}),
        serde_json::json!({"resume_thread_id": uuid::Uuid::now_v7().to_string()}),
    ] {
        for isolation in [
            serde_json::json!("worktree"),
            serde_json::json!(1),
            serde_json::json!({}),
            serde_json::json!(false),
        ] {
            let mut input = mode.clone();
            input["prompt"] = serde_json::json!("不得执行");
            input["run_in_background"] = serde_json::json!(true);
            input["isolation"] = isolation;
            let error = tool.invoke(input, host.context(&[])).await.unwrap_err();
            let typed = error.downcast_ref::<EffectiveToolError>().unwrap();
            assert_eq!(typed.code, EffectiveToolErrorCode::InvalidInput);
            assert!(typed.message.contains("isolation is not supported"));
        }
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        host.fixture
            .facade()
            .list_children(&host.parent_id)
            .await
            .unwrap()
            .len(),
        before.len()
    );
}

/// [回归测试] 显式不同目录不能只改变元数据而沿用父 Workspace。
#[tokio::test]
async fn test_cwd_rejected_before_spawn_fork_and_resume() {
    let dir = tempdir().unwrap();
    let other = tempdir().unwrap();
    let mut tool = make_subagent_tool(vec![]);
    tool.parent_cwd = dir.path().to_string_lossy().into_owned();
    for mode in [
        serde_json::json!({"subagent_type": "test-agent"}),
        serde_json::json!({"fork": true}),
        serde_json::json!({"resume_thread_id": uuid::Uuid::now_v7().to_string()}),
    ] {
        for (cwd, message) in [
            (other.path().to_path_buf(), "must resolve to the parent cwd"),
            (
                dir.path().join("missing"),
                "cannot resolve explicit Agent.cwd",
            ),
        ] {
            let mut input = mode.clone();
            input["prompt"] = serde_json::json!("不得执行");
            input["cwd"] = serde_json::json!(cwd);
            let error = tool
                .invoke(input, peri_agent::tools::ToolContext::new(&[], "."))
                .await
                .unwrap_err();
            let typed = error.downcast_ref::<EffectiveToolError>().unwrap();
            assert_eq!(typed.code, EffectiveToolErrorCode::InvalidInput);
            assert!(typed.message.contains(message), "{error}");
        }
    }
}

#[tokio::test]
async fn test_equivalent_explicit_cwd_and_non_string_tolerance() {
    let dir = tempdir().unwrap();
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    let mut tool = make_subagent_tool(vec![]);
    tool.parent_cwd = dir.path().to_string_lossy().into_owned();
    for cwd in [
        format!("{}/", tool.parent_cwd),
        format!("{}/nested/..", tool.parent_cwd),
    ] {
        assert_eq!(
            tool.validate_cwd(Some(&cwd)).await.unwrap(),
            tool.parent_cwd
        );
    }
    #[cfg(unix)]
    {
        let link = dir.path().join("alias");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        assert_eq!(
            tool.validate_cwd(link.to_str()).await.unwrap(),
            tool.parent_cwd
        );
    }
    for input in [
        serde_json::json!({}),
        serde_json::json!({"cwd": false}),
        serde_json::json!({"cwd": null}),
    ] {
        let parsed = super::super::invocation::InvocationArgs::parse(&input).unwrap();
        assert!(parsed.cwd.is_none());
        assert_eq!(
            tool.validate_cwd(parsed.cwd.as_deref()).await.unwrap(),
            tool.parent_cwd
        );
    }
    let parsed =
        super::super::invocation::InvocationArgs::parse(&serde_json::json!({"cwd": ""})).unwrap();
    assert_eq!(parsed.cwd.as_deref(), Some(""));
    assert!(tool.validate_cwd(parsed.cwd.as_deref()).await.is_err());
}
