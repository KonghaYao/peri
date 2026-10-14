use super::*;

#[tokio::test]
async fn test_adopt_legacy_session_publishes_binding_and_frozen_once() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    // legacy 会话：有历史但无绑定、无 frozen（由旧版本写入）。
    let id = store
        .create_thread(ThreadMeta::new_at(cwd.as_str(), peri_time::now_wall()))
        .await
        .unwrap();

    data.adopt_legacy_session(&id, &cwd, &workspace, &frozen("legacy"))
        .await
        .unwrap();
    let snapshot = data.load_snapshot(&id).await.unwrap();
    assert_eq!(
        snapshot.binding,
        BindingState::Bound(binding_of(&workspace))
    );
    assert!(matches!(snapshot.frozen, FrozenState::Present(_)));

    // 重复接纳：既有绑定与本次一致即幂等，不覆盖、不修复。
    data.adopt_legacy_session(&id, &cwd, &workspace, &frozen("legacy"))
        .await
        .unwrap();

    // 借接纳改绑 cwd 必须失败。
    let error = data
        .adopt_legacy_session(&id, "/elsewhere", &workspace, &frozen("legacy"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::Workspace(_)
        ),
        "{error}"
    );
    let after = data.load_snapshot(&id).await.unwrap();
    assert_eq!(after.binding, BindingState::Bound(binding_of(&workspace)));
    assert_eq!(after.meta.cwd, cwd);
}

/// legacy 竞争（write-once）：先提交的候选是 winner；后提交者不带覆盖，读回仍是最先那份字节。
///
/// ACP 侧据此把「adopt 后重读的 winner」传给装配（§6.3），因此这里的读回值就是装配事实源。
#[tokio::test]
async fn test_adopt_legacy_session_keeps_the_first_winner_bytes() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let id = store
        .create_thread(ThreadMeta::new_at(cwd.as_str(), peri_time::now_wall()))
        .await
        .unwrap();
    // 第二个句柄：同一份库事实上的另一次接纳（两宿主共享同一个 store 的等价形态）。
    let second = SqliteSessionData::new(Arc::clone(&store.database));

    data.adopt_legacy_session(&id, &cwd, &workspace, &frozen("winner"))
        .await
        .unwrap();
    second
        .adopt_legacy_session(&id, &cwd, &workspace, &frozen("candidate"))
        .await
        .expect("绑定一致时接纳幂等：候选不写入，也不报冲突");

    let snapshot = second.load_snapshot(&id).await.unwrap();
    match snapshot.frozen {
        FrozenState::Present(bytes) => assert_eq!(bytes.as_str(), frozen("winner").as_str()),
        other => panic!("winner bytes must stay persisted: {other:?}"),
    }
}

#[tokio::test]
async fn test_adopt_legacy_session_preserves_canonical_metadata_and_history() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let mut meta = ThreadMeta::new_at(cwd.as_str(), peri_time::now_wall());
    meta.config = Some(r#"{"model":"legacy"}"#.to_owned());
    let id = store.create_thread(meta).await.unwrap();
    let history = payloads(2);
    data.append_history(&id, &history).await.unwrap();
    let before = data.load_snapshot(&id).await.unwrap();
    assert_eq!(before.binding, BindingState::Missing);
    assert_eq!(before.frozen, FrozenState::LegacyAbsent);
    assert_eq!(before.meta.message_count, 2);
    assert_eq!(before.meta.config.as_deref(), Some(r#"{"model":"legacy"}"#));
    let machine = data.machine_id_of(&id).await.unwrap().unwrap();

    let error = data
        .adopt_legacy_session(&id, "/elsewhere", &workspace, &frozen("legacy"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::Workspace(
                peri_acp_types::workspace::WorkspaceError::ExecutionBindingMismatch
            )
        ),
        "{error}"
    );
    let rejected = data.load_snapshot(&id).await.unwrap();
    assert_eq!(rejected.binding, BindingState::Missing);
    assert_eq!(rejected.frozen, FrozenState::LegacyAbsent);
    assert_eq!(
        serde_json::to_value(&rejected.meta).unwrap(),
        serde_json::to_value(&before.meta).unwrap()
    );
    assert_eq!(payload_bytes(&rejected.payloads), payload_bytes(&history));

    data.adopt_legacy_session(&id, &cwd, &workspace, &frozen("legacy"))
        .await
        .unwrap();
    data.adopt_legacy_session(&id, &cwd, &workspace, &frozen("candidate"))
        .await
        .unwrap();
    let snapshot = data.load_snapshot(&id).await.unwrap();
    assert_eq!(
        snapshot.binding,
        BindingState::Bound(binding_of(&workspace))
    );
    assert_eq!(data.load_binding(&id).await.unwrap(), snapshot.binding);
    assert_eq!(snapshot.frozen, FrozenState::Present(frozen("legacy")));
    assert_eq!(
        serde_json::to_value(&snapshot.meta).unwrap(),
        serde_json::to_value(&before.meta).unwrap()
    );
    assert_eq!(payload_bytes(&snapshot.payloads), payload_bytes(&history));
    assert_eq!(snapshot.flags, before.flags);
    assert!(snapshot.inherited.payloads.is_empty());
    assert!(snapshot.inherited.flags.is_empty());
    assert_eq!(data.machine_id_of(&id).await.unwrap(), Some(machine));
}

/// 已认领过、但绑定记录在迁移中丢失的历史会话：接纳补上绑定，既有 frozen 字节保持
/// 不变（write-once）——本次候选不写入，元数据与历史也不受影响。
#[tokio::test]
async fn test_adopt_legacy_session_backfills_binding_and_keeps_the_canonical_bytes() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let id = "s-unbound-frozen".to_owned();
    data.save_new_session(&session(&id, &cwd, &workspace, frozen("canonical")))
        .await
        .unwrap();
    data.append_history(&id, &payloads(2)).await.unwrap();
    sqlx::query("DELETE FROM session_bindings WHERE thread_id = ?1")
        .bind(&id)
        .execute(&store.database.pool)
        .await
        .unwrap();
    let before = data.load_snapshot(&id).await.unwrap();
    assert_eq!(before.binding, BindingState::Missing);
    assert_eq!(before.frozen, FrozenState::Present(frozen("canonical")));

    data.adopt_legacy_session(&id, &cwd, &workspace, &frozen("replacement"))
        .await
        .unwrap();
    let after = data.load_snapshot(&id).await.unwrap();
    assert_eq!(after.binding, BindingState::Bound(binding_of(&workspace)));
    assert_eq!(after.frozen, FrozenState::Present(frozen("canonical")));
    assert_eq!(
        serde_json::to_value(&after.meta).unwrap(),
        serde_json::to_value(&before.meta).unwrap()
    );
    assert_eq!(
        payload_bytes(&after.payloads),
        payload_bytes(&before.payloads)
    );
}
/// 子会话不另立归属：父会话的绑定行已经成立且身份逐字一致时才接纳（绑定身份继承、
/// 缺失的 frozen 按父的已持久字节补齐）；父会话未被本机核实（没有绑定）时如实拒绝，
/// 不代父接纳，也不写子会话的任何事实。
#[tokio::test]
async fn test_adopt_legacy_session_child_inherits_the_verified_parent_binding() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-parent", &cwd, &workspace, frozen("parent")))
        .await
        .unwrap();
    let mut child = session("s-legacy-child", &cwd, &workspace, frozen("parent"));
    child.meta.parent_thread_id = Some("s-parent".to_owned());
    data.save_child(&ChildSnapshot {
        target: child,
        parent_id: "s-parent".to_owned(),
        root_id: "s-parent".to_owned(),
        inherited: Default::default(),
    })
    .await
    .unwrap();
    // 迁移现场：子会话没有绑定行，frozen 列也没有字节。
    sqlx::query("DELETE FROM session_bindings WHERE thread_id = 's-legacy-child'")
        .execute(&store.database.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE threads SET frozen_context = NULL WHERE id = 's-legacy-child'")
        .execute(&store.database.pool)
        .await
        .unwrap();

    // 父会话未被本机核实：子会话不能自己成立归属。
    sqlx::query("DELETE FROM session_bindings WHERE thread_id = 's-parent'")
        .execute(&store.database.pool)
        .await
        .unwrap();
    let error = data
        .adopt_legacy_session(
            &"s-legacy-child".to_owned(),
            &cwd,
            &workspace,
            &frozen("parent"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::Workspace(
                peri_acp_types::workspace::WorkspaceError::ExecutionBindingMismatch
            )
        ),
        "{error}"
    );
    let refused = data
        .load_snapshot(&"s-legacy-child".to_owned())
        .await
        .unwrap();
    assert_eq!(refused.binding, BindingState::ExternalOrUnregistered);
    assert_eq!(refused.frozen, FrozenState::LegacyAbsent);

    // 父会话核实接纳后：子会话继承同一绑定身份与父的已持久字节。
    data.adopt_legacy_session(&"s-parent".to_owned(), &cwd, &workspace, &frozen("parent"))
        .await
        .unwrap();
    let parent = data.load_snapshot(&"s-parent".to_owned()).await.unwrap();
    assert_eq!(parent.binding, BindingState::Bound(binding_of(&workspace)));
    data.adopt_legacy_session(
        &"s-legacy-child".to_owned(),
        &cwd,
        &workspace,
        &frozen("parent"),
    )
    .await
    .unwrap();
    let adopted = data
        .load_snapshot(&"s-legacy-child".to_owned())
        .await
        .unwrap();
    assert_eq!(adopted.binding, parent.binding);
    assert_eq!(adopted.frozen, parent.frozen);
    assert_eq!(
        data.list_children(&"s-parent".to_owned())
            .await
            .unwrap()
            .len(),
        1
    );
}
