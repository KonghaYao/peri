use super::*;

async fn wait_failed(registry: &McpSkillRegistry, token: &HandleToken) {
    for _ in 0..200 {
        if matches!(registry.discovery_state("srv"), Some(ServerDiscoveryState::Failed { handle }) if Arc::ptr_eq(&handle, token))
        {
            assert!(!registry.discovery_in_progress());
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("等待 Failed 超时: {:?}", registry.discovery_state("srv"));
}

/// [回归测试] 无 peer 因而无 cache 的当前连接仍须完成发现并暴露 Failed。
#[tokio::test]
async fn before_agent_missing_peer_without_cache_finishes_failed() {
    let pool = Arc::new(McpClientPool::new_empty());
    let empty = insert_skill_handle(&pool, "srv", vec![]);
    let handle = Arc::new(McpClientHandle {
        peer: None,
        ..empty.as_ref().clone()
    });
    pool.clients.write().insert("srv".into(), handle.clone());
    assert!(pool.resource_cache_for_handle(&handle).is_none());
    let token: HandleToken = handle;
    let registry = Arc::new(McpSkillRegistry::new());
    let commands = Arc::new(CommandRegistry::new());
    let middleware = McpMiddleware::new(pool)
        .with_skill_discovery(Some(registry.clone()), AgentCancellationToken::new())
        .with_command_registry(Some(commands.clone()));
    let mut state = AgentState::new("/tmp");
    Middleware::before_agent(&middleware, &mut state)
        .await
        .unwrap();
    wait_failed(&registry, &token).await;
    Middleware::before_agent(&middleware, &mut state)
        .await
        .unwrap();
    wait_failed(&registry, &token).await;
    assert!(state.messages().is_empty());
    assert!(commands.snapshot().is_empty());
    assert_eq!(
        commands
            .project_sources(&[("srv".into(), token)])
            .to_discover
            .len(),
        1,
        "失败来源不得在后续投影留下无人收尾的命令 Started"
    );
}

/// [回归测试] 投影后、spawn 前换代时不得把新 handle 搭配旧 token 发起发现。
#[tokio::test]
async fn before_agent_rejects_replaced_handle_before_discovery_spawn() {
    let pool = Arc::new(McpClientPool::new_empty());
    let empty = insert_skill_handle(&pool, "srv", vec![]);
    let replacement = Arc::new(McpClientHandle {
        peer: None,
        ..empty.as_ref().clone()
    });
    let registry = Arc::new(McpSkillRegistry::new());
    registry.mark_discovery_started("removed", Arc::new(0u32));
    let weak_pool = Arc::downgrade(&pool);
    let next = replacement.clone();
    registry.set_on_change(Some(Arc::new(move || {
        if let Some(pool) = weak_pool.upgrade() {
            pool.clients.write().insert("srv".into(), next.clone());
        }
    })));
    let commands = Arc::new(CommandRegistry::new());
    let middleware = McpMiddleware::new(pool.clone())
        .with_skill_discovery(Some(registry.clone()), AgentCancellationToken::new())
        .with_command_registry(Some(commands.clone()));
    let mut state = AgentState::new("/tmp");
    Middleware::before_agent(&middleware, &mut state)
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&pool.get_client("srv").unwrap(), &replacement));
    assert!(
        registry.discovery_state("srv").is_none(),
        "旧代 Started 必须清理而非 spawn"
    );
    let old_token: HandleToken = empty;
    assert_eq!(
        commands
            .project_sources(&[("srv".into(), old_token)])
            .to_discover
            .len(),
        1
    );
    registry.set_on_change(None);
    Middleware::before_agent(&middleware, &mut state)
        .await
        .unwrap();
    let token: HandleToken = replacement;
    wait_failed(&registry, &token).await;
}
