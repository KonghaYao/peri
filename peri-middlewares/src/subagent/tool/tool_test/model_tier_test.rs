use super::*;

/// Agent 工具 schema 应声明 `model` 参数（string，可选），并列出全部可用档位
#[test]
fn test_agent_parameters_declares_model_tier() {
    let t = make_subagent_tool(vec![]);
    let params = t.parameters();
    assert!(
        params["properties"]["model"]["type"] == "string",
        "model 应为 string 类型参数"
    );
    let desc = params["properties"]["model"]["description"]
        .as_str()
        .unwrap();
    for tier in ["inherit", "haiku", "sonnet", "opus", "fable"] {
        assert!(
            desc.contains(tier),
            "model 描述应列出档位 {}: {}",
            tier,
            desc
        );
    }
}

/// 记录 llm_factory 收到的 model alias 的工具构造（每次 subagent 装配调用一次）
fn make_recording_subagent_tool(
    parent_tools: Vec<Arc<dyn BaseTool>>,
    aliases: Arc<std::sync::Mutex<Vec<Option<String>>>>,
) -> SubAgentTool {
    let aliases_clone = Arc::clone(&aliases);
    SubAgentTool::new(
        Arc::new(parent_tools),
        None,
        Arc::new(move |alias: Option<&str>| {
            aliases_clone
                .lock()
                .unwrap()
                .push(alias.map(|s| s.to_string()));
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(EchoLLM),
                "fixture-scripted",
            )
        }),
        "/tmp".to_string(),
    )
}

fn write_test_agent_with_model(dir: &tempfile::TempDir, model: &str) {
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("test-agent.md"),
        format!(
            "---\nname: test-agent\ndescription: A test agent\nmodel: {}\n---\n\nYou are a test agent.\n",
            model
        ),
    )
    .unwrap();
}

/// model 参数覆盖 frontmatter：定义声明 sonnet，调用传 haiku → llm_factory 收到 "haiku"
#[tokio::test]
async fn test_agent_model_override_replaces_frontmatter() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "sonnet");
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let host = DurableHost::open_in(dir.path(), "fixture-model-tier").await;
    let t = with_agent_face(
        make_recording_subagent_tool(vec![], Arc::clone(&aliases)),
        dir.path(),
    )
    .await;
    let t = host.bind(t);
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "hello",
                "model": "haiku",
                "cwd": dir.path().to_str().unwrap()
            }),
            host.context(&[]),
        )
        .await
        .unwrap();
    assert!(result.contains("echo"), "应正常执行: {}", result);
    let recorded = aliases.lock().unwrap();
    assert_eq!(
        recorded.as_slice(),
        &[Some("haiku".to_string())],
        "调用参数 model 应覆盖 frontmatter"
    );
}

/// model: "inherit" → 继承父模型（llm_factory 收到 None），覆盖 frontmatter
#[tokio::test]
async fn test_agent_model_inherit_uses_parent_model() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "sonnet");
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let host = DurableHost::open_in(dir.path(), "fixture-model-tier").await;
    let t = with_agent_face(
        make_recording_subagent_tool(vec![], Arc::clone(&aliases)),
        dir.path(),
    )
    .await;
    let t = host.bind(t);
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "hello",
                "model": "inherit",
                "cwd": dir.path().to_str().unwrap()
            }),
            host.context(&[]),
        )
        .await
        .unwrap();
    assert!(result.contains("echo"), "应正常执行: {}", result);
    let recorded = aliases.lock().unwrap();
    assert_eq!(recorded.as_slice(), &[None], "inherit 应继承父模型");
}

/// 省略 model（或传空串 / 纯空白占位符）→ 保持 agent 定义 frontmatter model
#[tokio::test]
async fn test_agent_model_omitted_keeps_frontmatter() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "sonnet");
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let host = DurableHost::open_in(dir.path(), "fixture-model-tier").await;
    let t = with_agent_face(
        make_recording_subagent_tool(vec![], Arc::clone(&aliases)),
        dir.path(),
    )
    .await;
    let t = host.bind(t);
    for model in [None, Some(""), Some("   ")] {
        let mut input = serde_json::json!({
            "subagent_type": "test-agent",
            "prompt": "hello",
            "cwd": dir.path().to_str().unwrap()
        });
        if let Some(m) = model {
            input["model"] = serde_json::Value::String(m.to_string());
        }
        let invocation = host.fresh_invocation("omitted").await;
        let result = t
            .invoke(input, host.context_with(&[], invocation))
            .await
            .unwrap();
        assert!(result.contains("echo"), "应正常执行: {}", result);
    }
    let recorded = aliases.lock().unwrap();
    assert_eq!(
        recorded.as_slice(),
        &[
            Some("sonnet".to_string()),
            Some("sonnet".to_string()),
            Some("sonnet".to_string())
        ],
        "省略/空/空白 model 应保持 frontmatter 定义"
    );
}

/// 省略 model + frontmatter "inherit"（或空串）→ llm_factory 收到 None（父模型）
#[tokio::test]
async fn test_agent_model_omitted_inherit_or_empty_frontmatter() {
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    for fm in ["inherit", ""] {
        let dir = tempdir().unwrap();
        write_test_agent_with_model(&dir, fm);
        let host = DurableHost::open_in(dir.path(), "fixture-model-tier").await;
        let t = host.bind(
            with_agent_face(
                make_recording_subagent_tool(vec![], Arc::clone(&aliases)),
                dir.path(),
            )
            .await,
        );
        let invocation = host.fresh_invocation("inherit").await;
        let result = t
            .invoke(
                serde_json::json!({
                    "subagent_type": "test-agent",
                    "prompt": "hello",
                    "cwd": dir.path().to_str().unwrap()
                }),
                host.context_with(&[], invocation),
            )
            .await
            .unwrap();
        assert!(result.contains("echo"), "应正常执行: {}", result);
    }
    let recorded = aliases.lock().unwrap();
    assert_eq!(
        recorded.as_slice(),
        &[None, None],
        "frontmatter inherit/空 应继承父模型"
    );
}

/// 档位大小写不敏感："HAIKU" → "haiku"；"InHerit" → 父模型（None）
#[tokio::test]
async fn test_agent_model_case_insensitive() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let host = DurableHost::open_in(dir.path(), "fixture-model-tier").await;
    let t = with_agent_face(
        make_recording_subagent_tool(vec![], Arc::clone(&aliases)),
        dir.path(),
    )
    .await;
    let t = host.bind(t);
    for model in ["HAIKU", "InHerit"] {
        let invocation = host.fresh_invocation("case").await;
        let result = t
            .invoke(
                serde_json::json!({
                    "subagent_type": "test-agent",
                    "prompt": "hello",
                    "model": model,
                    "cwd": dir.path().to_str().unwrap()
                }),
                host.context_with(&[], invocation),
            )
            .await
            .unwrap();
        assert!(result.contains("echo"), "应正常执行: {}", result);
    }
    let recorded = aliases.lock().unwrap();
    assert_eq!(
        recorded.as_slice(),
        &[Some("haiku".to_string()), None],
        "档位应大小写不敏感归一"
    );
}

/// fork + 未知档位：model 被宽容忽略且不校验（与 resume 忽略语义一致）
#[tokio::test]
async fn test_agent_model_unknown_ignored_on_fork() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages.write().push(BaseMessage::human("Hello"));
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let host = DurableHost::open("fixture-model-tier-fork").await;
    let t = host.bind(
        make_recording_subagent_tool(vec![], Arc::clone(&aliases))
            .with_parent_messages(parent_messages),
    );
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "do the thing",
                "model": "turbo"
            }),
            host.context(&[]),
        )
        .await
        .expect("fork + 未知档位应成功（model 被宽容忽略）");
    assert!(result.contains("echo"), "fork 应正常执行: {}", result);
    let recorded = aliases.lock().unwrap();
    assert_eq!(recorded.as_slice(), &[None], "fork 恒继承父模型");
}

/// 未知档位拒绝（不静默回退父模型），且不调用 llm_factory
#[tokio::test]
async fn test_agent_model_unknown_rejected() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let host = DurableHost::open_in(dir.path(), "fixture-model-tier").await;
    let t = with_agent_face(
        make_recording_subagent_tool(vec![], Arc::clone(&aliases)),
        dir.path(),
    )
    .await;
    let t = host.bind(t);
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "hello",
                "model": "turbo",
                "cwd": dir.path().to_str().unwrap()
            }),
            host.context(&[]),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("invalid model tier"),
        "未知档位应报错而非静默回退: {}",
        err_msg
    );
    assert!(
        err_msg.contains("inherit, haiku, sonnet, opus, fable"),
        "错误信息应列出可用档位: {}",
        err_msg
    );
    assert!(
        aliases.lock().unwrap().is_empty(),
        "未知档位不应调用 llm_factory"
    );
}

/// fork 忽略 model：fork 调用携带 model 仍成功，llm_factory 收到 None（父模型）
#[tokio::test]
async fn test_agent_model_ignored_on_fork() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages.write().push(BaseMessage::human("Hello"));
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let host = DurableHost::open("fixture-model-tier-fork").await;
    let t = host.bind(
        make_recording_subagent_tool(vec![], Arc::clone(&aliases))
            .with_parent_messages(parent_messages),
    );
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "do the thing",
                "model": "haiku"
            }),
            host.context(&[]),
        )
        .await
        .expect("fork + model 应成功（model 被忽略）");
    assert!(result.contains("echo"), "fork 应正常执行: {}", result);
    let recorded = aliases.lock().unwrap();
    assert_eq!(recorded.as_slice(), &[None], "fork 恒继承父模型");
}

/// resume 不允许 model 覆盖：恢复按 thread title 重建（frontmatter sonnet），
/// 调用传 model 被忽略且不报错（与 subagent_type/fork 同款宽容语义）；
/// 未知档位 "turbo" 同样被宽容忽略，不做校验
#[tokio::test]
async fn test_resume_thread_id_ignores_model_field() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "sonnet");
    let store = SessionFixture::open_in(dir.path()).await;
    let cwd = store.workspace_cwd();
    let parent_id = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .expect("建立父会话失败");
    // 父会话句柄：resume 路径经它校验「owning parent session」
    let parent = peri_agent::session::Session::new(
        std::sync::Arc::from(cwd.as_str()),
        peri_agent::session::FrozenContext::builder().build(),
        Some(parent_id.clone()),
    );
    let id = uuid::Uuid::now_v7().to_string();
    // resume 的可信 invocation 必须登记在 owning parent 上（夹具按同一契约写入）。
    let invocation_id = "resume-model-tier-invocation";
    preset_resumable_child(
        &store,
        &id,
        "test-agent",
        Some(parent_id.as_str()),
        Some(invocation_id),
        vec![BaseMessage::human("旧消息 1"), BaseMessage::ai("旧回答 1")],
    )
    .await;

    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let mut t = with_agent_face(
        make_recording_subagent_tool(vec![], Arc::clone(&aliases))
            .with_session_resources(store.facade())
            .with_parent_thread_id(parent_id.clone())
            .with_parent_session(parent.clone()),
        dir.path(),
    )
    .await;
    t.parent_cwd = cwd.clone();
    // resume 的子链宿主来自 owning parent session：给该 session 挂生产 host。
    // （与 store 共享同一 SQLite 文件；此处只需耐久承载面。）
    let durable_face = DurableHost::open_in(dir.path(), "fixture-model-tier-resume").await;
    durable_face.attach_session_host(&parent);
    let mut ctx = peri_agent::tools::ToolContext::new(&[], &cwd);
    ctx.invocation_id = Some(invocation_id.to_string());
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "model": "turbo",
                "cwd": cwd.clone(),
            }),
            ctx,
        )
        .await
        .expect("resume + model 应容错恢复而非报错");
    assert!(
        result.contains(&format!("child_thread_id: {}", id)),
        "完成文本应带 child_thread_id: {}",
        result
    );
    let recorded = aliases.lock().unwrap();
    assert_eq!(
        recorded.as_slice(),
        &[Some("sonnet".to_string())],
        "resume 应保持原定义模型，不被调用参数覆盖"
    );
}

/// 后台定义型路径应用 model 覆盖（execute_bg.rs 透传）
#[tokio::test]
async fn test_agent_model_override_applies_to_background() {
    use peri_agent::agent::events::ExecutorEvent;
    use tokio::sync::mpsc;

    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());

    let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases))
        .with_task_manager(Arc::clone(&registry))
        .with_bg_event_sender(bg_tx);
    let host = DurableHost::open_in(dir.path(), "fixture-model-tier").await;
    let t = host.bind(with_agent_face(t, dir.path()).await);

    let invoke_msg = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "bg task",
                "model": "haiku",
                "run_in_background": true,
                "cwd": dir.path().to_str().unwrap()
            }),
            host.context(&[]),
        )
        .await
        .expect("bg 应启动");
    assert!(
        invoke_msg.contains("Background task"),
        "应返回后台任务启动消息: {}",
        invoke_msg
    );
    // llm_factory 在 invoke_background 装配阶段同步调用（spawn 之前）
    {
        let recorded = aliases.lock().unwrap();
        assert_eq!(
            recorded.as_slice(),
            &[Some("haiku".to_string())],
            "bg 定义型路径应应用 model 覆盖"
        );
    }

    // 等待 BackgroundTaskCompleted，避免任务悬挂
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, bg_rx.recv()).await {
            Ok(Some(ExecutorEvent::BackgroundTaskCompleted(_))) => break,
            Ok(_) => {}
            _ => break,
        }
    }
}

/// 宽容解码不能将字符串布尔值当成分支开关，也不能把非字符串 model 当覆盖值。
#[tokio::test]
async fn test_agent_invoke_wrong_optional_types_keep_definition_and_parent_cwd() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "sonnet");
    let aliases = Arc::default();
    let host = DurableHost::open_in(dir.path(), "fixture-model-tier-optional").await;
    let mut tool = host.bind(
        with_agent_face(
            make_recording_subagent_tool(vec![], Arc::clone(&aliases)),
            dir.path(),
        )
        .await,
    );
    tool.parent_cwd = dir.path().to_str().unwrap().to_string();
    let result = tool
        .invoke(
            serde_json::json!({
                "prompt": "  preserve spaces  ",
                "subagent_type": "test-agent",
                "resume_thread_id": 42,
                "model": ["opus"],
                "cwd": false,
                "fork": "true",
                "run_in_background": "true",
                "name": {"ignored": true},
                "description": ["ignored"],
                "isolation": 1
            }),
            host.context(&[]),
        )
        .await
        .unwrap();
    // durable 路径返回带 child_thread_id 前缀（session_resources 存在时），
    // 执行文本仍须原样保留 prompt 前导空格。
    assert!(
        result.ends_with("echo:   preserve spaces"),
        "prompt 前导空格必须原样进入执行（返回文本沿用末尾 trim）：{result}"
    );
    assert!(
        result.starts_with("child_thread_id: "),
        "durable 输出应带可恢复的 child_thread_id 前缀：{result}"
    );
    assert_eq!(
        aliases.lock().unwrap().as_slice(),
        &[Some("sonnet".to_string())]
    );
}

/// [M2] frontmatter 未知档位 + 点名启动：定义被隔离出可执行目录，点名启动
/// 必须显式失败且不调用 llm_factory（不得静默回退父模型；坏定义只影响自身，
/// 不阻断会话其余能力）。
#[tokio::test]
async fn test_agent_frontmatter_unknown_tier_is_rejected_on_pointed_launch() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "turbo");
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let host = DurableHost::open_in(dir.path(), "fixture-model-tier-invalid").await;
    let t = with_agent_face(
        make_recording_subagent_tool(vec![], Arc::clone(&aliases)),
        dir.path(),
    )
    .await;
    let t = host.bind(t);

    let error = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "hello",
                "cwd": dir.path().to_str().unwrap()
            }),
            host.context(&[]),
        )
        .await
        .expect_err("未知 frontmatter 档位必须显式失败");

    let message = error.to_string();
    assert!(
        message.contains("test-agent"),
        "错误必须可定位到被点名的定义（隔离而非静默）: {message}"
    );
    assert!(
        aliases.lock().unwrap().is_empty(),
        "非法档位不得调用 llm_factory（不静默回退父模型）"
    );
}

/// [M2] frontmatter 档位大小写归一后仍可点名启动（`SONNET` → sonnet）。
#[tokio::test]
async fn test_agent_frontmatter_tier_case_normalized_on_launch() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "SONNET");
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let host = DurableHost::open_in(dir.path(), "fixture-model-tier-case").await;
    let t = with_agent_face(
        make_recording_subagent_tool(vec![], Arc::clone(&aliases)),
        dir.path(),
    )
    .await;
    let t = host.bind(t);

    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "hello",
                "cwd": dir.path().to_str().unwrap()
            }),
            host.context(&[]),
        )
        .await
        .expect("合法档位（大小写变体）应正常启动");
    assert!(result.contains("echo"), "应正常执行: {result}");
    assert_eq!(
        aliases.lock().unwrap().as_slice(),
        &[Some("sonnet".to_string())],
        "frontmatter 档位应小写归一后交给工厂"
    );
}
