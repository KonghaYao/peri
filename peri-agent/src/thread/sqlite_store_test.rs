    async fn make_store() -> (SqliteThreadStore, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let store = SqliteThreadStore::new(dir.path().join("test.db"))
            .await
            .unwrap();
        (store, dir)
    }

    #[tokio::test]
    async fn test_create_append_load() {
        let (store, _dir) = make_store().await;
        let meta = ThreadMeta::new("/tmp");
        let id = store.create_thread(meta).await.unwrap();

        let msgs = vec![BaseMessage::human("Hello"), BaseMessage::ai("Hi there")];
        store.append_messages(&id, &msgs).await.unwrap();

        let loaded = store.load_messages(&id).await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].content(), "Hello");
        assert_eq!(loaded[1].content(), "Hi there");
    }

    #[tokio::test]
    async fn test_list_threads_order() {
        let (store, _dir) = make_store().await;

        let m1 = ThreadMeta::new("/a");
        let id1 = store.create_thread(m1).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        let m2 = ThreadMeta::new("/b");
        let id2 = store.create_thread(m2).await.unwrap();

        // 给 id2 追加消息，更新 updated_at
        store
            .append_messages(&id2, &[BaseMessage::human("msg")])
            .await
            .unwrap();

        let list = store.list_threads().await.unwrap();
        assert_eq!(list.len(), 2);
        // id2 updated_at 更新，应排在第一位
        assert_eq!(list[0].id, id2);
        assert_eq!(list[1].id, id1);
    }

    #[tokio::test]
    async fn test_delete_thread_cascade() {
        let (store, _dir) = make_store().await;
        let meta = ThreadMeta::new("/tmp");
        let id = store.create_thread(meta).await.unwrap();
        store
            .append_messages(&id, &[BaseMessage::human("msg")])
            .await
            .unwrap();

        store.delete_thread(&id).await.unwrap();

        // 消息应该被级联删除
        let msgs = store.load_messages(&id).await;
        // 线程不存在时 load_messages 应返回空（因为 SELECT 无结果）
        assert!(msgs.unwrap().is_empty());

        // 元数据应不存在
        let meta_result = store.load_meta(&id).await;
        assert!(meta_result.is_err());
    }

    #[tokio::test]
    async fn test_message_order_after_multiple_appends() {
        let (store, _dir) = make_store().await;
        let meta = ThreadMeta::new("/tmp");
        let id = store.create_thread(meta).await.unwrap();

        store
            .append_messages(&id, &[BaseMessage::human("msg1")])
            .await
            .unwrap();
        store
            .append_messages(&id, &[BaseMessage::ai("reply1")])
            .await
            .unwrap();
        store
            .append_messages(&id, &[BaseMessage::human("msg2")])
            .await
            .unwrap();

        let loaded = store.load_messages(&id).await.unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].content(), "msg1");
        assert_eq!(loaded[1].content(), "reply1");
        assert_eq!(loaded[2].content(), "msg2");
    }

    #[tokio::test]
    async fn test_title_auto_set() {
        let (store, _dir) = make_store().await;
        let meta = ThreadMeta::new("/tmp");
        let id = store.create_thread(meta).await.unwrap();

        store
            .append_messages(&id, &[BaseMessage::human("这是一条测试消息")])
            .await
            .unwrap();

        let loaded_meta = store.load_meta(&id).await.unwrap();
        assert!(loaded_meta.title.is_some());
        assert!(loaded_meta.title.unwrap().contains("这是一条测试消息"));
    }

    #[tokio::test]
    async fn test_update_title() {
        let (store, _dir) = make_store().await;
        let meta = ThreadMeta::new("/tmp");
        let id = store.create_thread(meta).await.unwrap();

        store.update_title(&id, "new title").await.unwrap();
        let loaded = store.load_meta(&id).await.unwrap();
        assert_eq!(loaded.title.as_deref(), Some("new title"));
    }

    #[tokio::test]
    async fn test_update_title_updates_timestamp() {
        let (store, _dir) = make_store().await;
        let meta = ThreadMeta::new("/tmp");
        let id = store.create_thread(meta).await.unwrap();

        let before = store.load_meta(&id).await.unwrap().updated_at;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        store.update_title(&id, "updated").await.unwrap();
        let after = store.load_meta(&id).await.unwrap().updated_at;
        assert!(
            after > before,
            "updated_at should be newer after update_title"
        );
    }

    // ── 新增：子线程创建和列表 ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_child_thread_create_and_list() {
        let (store, _dir) = make_store().await;
        // 创建父线程
        let parent_meta = ThreadMeta::new("/project");
        let parent_id = store.create_thread(parent_meta).await.unwrap();

        // 创建子线程
        let mut child_meta = ThreadMeta::new("/project");
        child_meta.parent_thread_id = Some(parent_id.clone());
        child_meta.hidden = true;
        let child_id = store.create_thread(child_meta).await.unwrap();

        // list_child_threads 应返回子线程
        let children = store.list_child_threads(&parent_id).await.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, child_id);
        assert_eq!(children[0].parent_thread_id.as_deref(), Some(parent_id.as_str()));

        // 子线程的 meta 应正确读取 parent_thread_id 和 hidden
        let child_meta_loaded = store.load_meta(&child_id).await.unwrap();
        assert_eq!(child_meta_loaded.parent_thread_id.as_deref(), Some(parent_id.as_str()));
        assert!(child_meta_loaded.hidden);
    }

    #[tokio::test]
    async fn test_session_threads_recursive() {
        let (store, _dir) = make_store().await;
        // L1 根线程
        let l1_id = store.create_thread(ThreadMeta::new("/root")).await.unwrap();
        // L2 子线程
        let mut l2_meta = ThreadMeta::new("/root");
        l2_meta.parent_thread_id = Some(l1_id.clone());
        l2_meta.hidden = true;
        let l2_id = store.create_thread(l2_meta).await.unwrap();
        // L3 孙线程
        let mut l3_meta = ThreadMeta::new("/root");
        l3_meta.parent_thread_id = Some(l2_id.clone());
        l3_meta.hidden = true;
        let l3_id = store.create_thread(l3_meta).await.unwrap();

        // 从 L1 根出发应递归获取全部 3 级
        let session = store.list_session_threads(&l1_id).await.unwrap();
        assert_eq!(session.len(), 3);
        let ids: Vec<&str> = session.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&l1_id.as_str()));
        assert!(ids.contains(&l2_id.as_str()));
        assert!(ids.contains(&l3_id.as_str()));
    }

    #[tokio::test]
    async fn test_update_thread_status() {
        use crate::thread::AgentStatus;
        let (store, _dir) = make_store().await;
        let id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();

        // 默认 active
        let meta = store.load_meta(&id).await.unwrap();
        assert_eq!(meta.agent_status, AgentStatus::Active);

        // 更新为 done
        store.update_thread_status(&id, "done").await.unwrap();
        let meta = store.load_meta(&id).await.unwrap();
        assert_eq!(meta.agent_status, AgentStatus::Done);

        // 更新为 error
        store.update_thread_status(&id, "error").await.unwrap();
        let meta = store.load_meta(&id).await.unwrap();
        assert_eq!(meta.agent_status, AgentStatus::Error);
    }

    #[tokio::test]
    async fn test_update_thread_status_rejects_illegal_string() {
        // 关键约束：非法状态字符串不应静默 fallback，必须返回错误
        let (store, _dir) = make_store().await;
        let id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
        let result = store.update_thread_status(&id, "running").await;
        assert!(result.is_err(), "非法 agent_status 字符串应被拒绝");
        // 状态保持不变（active）
        let meta = store.load_meta(&id).await.unwrap();
        assert_eq!(meta.agent_status, crate::thread::AgentStatus::Active);
    }

    #[tokio::test]
    async fn test_load_context_without_parent() {
        let (store, _dir) = make_store().await;
        let id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();

        let msgs = vec![
            BaseMessage::human("hello"),
            BaseMessage::ai("world"),
            BaseMessage::human("how are you"),
        ];
        store.append_messages(&id, &msgs).await.unwrap();

        // 无父线程，load_context 应返回自身全部消息
        let ctx = store.load_context(&id).await.unwrap();
        assert_eq!(ctx.len(), 3);
        assert_eq!(ctx[0].content(), "hello");
        assert_eq!(ctx[1].content(), "world");
        assert_eq!(ctx[2].content(), "how are you");

        // 第二次调用应命中缓存（cached_context 已写入）
        let ctx2 = store.load_context(&id).await.unwrap();
        assert_eq!(ctx2.len(), 3);
    }

    #[tokio::test]
    async fn test_load_context_with_snapshot() {
        let (store, _dir) = make_store().await;
        // 父线程 + 3 条消息
        let parent_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
        let parent_msgs = vec![
            BaseMessage::human("p1"),
            BaseMessage::ai("p2"),
            BaseMessage::human("p3"),
        ];
        store.append_messages(&parent_id, &parent_msgs).await.unwrap();

        // 快照截止到第 2 条消息（p2）的 message_id
        let parent_loaded = store.load_messages(&parent_id).await.unwrap();
        let snapshot_msg_id = parent_loaded[1].id().as_uuid().to_string();

        // 更新父线程的 snapshot_at_message_id
        let mut parent_meta = store.load_meta(&parent_id).await.unwrap();
        parent_meta.snapshot_at_message_id = Some(snapshot_msg_id.clone());
        store.update_meta(&parent_id, parent_meta).await.unwrap();

        // 创建子线程
        let mut child_meta = ThreadMeta::new("/tmp");
        child_meta.parent_thread_id = Some(parent_id.clone());
        child_meta.hidden = true;
        let child_id = store.create_thread(child_meta).await.unwrap();

        let child_msgs = vec![BaseMessage::human("c1"), BaseMessage::ai("c2")];
        store.append_messages(&child_id, &child_msgs).await.unwrap();

        // load_context 应返回：父线程前 2 条 + 子线程全部 2 条 = 4 条
        let ctx = store.load_context(&child_id).await.unwrap();
        assert_eq!(ctx.len(), 4, "应包含父线程快照 2 条 + 子线程 2 条");
        assert_eq!(ctx[0].content(), "p1");
        assert_eq!(ctx[1].content(), "p2");
        assert_eq!(ctx[2].content(), "c1");
        assert_eq!(ctx[3].content(), "c2");
    }

    #[tokio::test]
    async fn test_cached_context_invalidation() {
        let (store, _dir) = make_store().await;
        let id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
        store
            .append_messages(&id, &[BaseMessage::human("hello")])
            .await
            .unwrap();

        // 首次加载产生缓存
        let ctx = store.load_context(&id).await.unwrap();
        assert_eq!(ctx.len(), 1);

        // 验证缓存已写入
        let meta = store.load_meta(&id).await.unwrap();
        assert!(meta.cached_context.is_some());

        // 清除缓存
        store.invalidate_context_cache(&id).await.unwrap();
        let meta = store.load_meta(&id).await.unwrap();
        assert!(meta.cached_context.is_none(), "清除缓存后 cached_context 应为 None");

        // 再次加载仍然正常工作（从零重建）
        let ctx2 = store.load_context(&id).await.unwrap();
        assert_eq!(ctx2.len(), 1);
        assert_eq!(ctx2[0].content(), "hello");
    }

    #[tokio::test]
    async fn test_list_threads_excludes_hidden() {
        let (store, _dir) = make_store().await;

        // 创建普通线程
        let visible_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();

        // 创建 hidden 的子 agent 线程
        let mut hidden_meta = ThreadMeta::new("/tmp");
        hidden_meta.parent_thread_id = Some(visible_id.clone());
        hidden_meta.hidden = true;
        let _hidden_id = store.create_thread(hidden_meta).await.unwrap();

        // list_threads 只返回非 hidden 的线程
        let list = store.list_threads().await.unwrap();
        assert_eq!(list.len(), 1, "hidden 线程不应出现在列表中");
        assert_eq!(list[0].id, visible_id);
    }

    #[tokio::test]
    async fn test_load_context_three_level_nesting() {
        let (store, _dir) = make_store().await;

        // L1 根线程：3 条消息，快照到第 2 条
        let l1_id = store.create_thread(ThreadMeta::new("/project")).await.unwrap();
        let l1_msgs = vec![
            BaseMessage::human("L1-a"),
            BaseMessage::ai("L1-b"),
            BaseMessage::human("L1-c"),
        ];
        store.append_messages(&l1_id, &l1_msgs).await.unwrap();
        let l1_loaded = store.load_messages(&l1_id).await.unwrap();
        let l1_snap = l1_loaded[1].id().as_uuid().to_string();
        let mut l1_meta = store.load_meta(&l1_id).await.unwrap();
        l1_meta.snapshot_at_message_id = Some(l1_snap);
        store.update_meta(&l1_id, l1_meta).await.unwrap();

        // L2 子线程：2 条消息，快照到第 1 条
        let mut l2_meta = ThreadMeta::new("/project");
        l2_meta.parent_thread_id = Some(l1_id.clone());
        l2_meta.hidden = true;
        let l2_id = store.create_thread(l2_meta).await.unwrap();
        let l2_msgs = vec![BaseMessage::human("L2-a"), BaseMessage::ai("L2-b")];
        store.append_messages(&l2_id, &l2_msgs).await.unwrap();
        let l2_loaded = store.load_messages(&l2_id).await.unwrap();
        let l2_snap = l2_loaded[0].id().as_uuid().to_string();
        let mut l2_meta_loaded = store.load_meta(&l2_id).await.unwrap();
        l2_meta_loaded.snapshot_at_message_id = Some(l2_snap);
        store.update_meta(&l2_id, l2_meta_loaded).await.unwrap();

        // L3 孙线程：1 条消息，无快照
        let mut l3_meta = ThreadMeta::new("/project");
        l3_meta.parent_thread_id = Some(l2_id.clone());
        l3_meta.hidden = true;
        let l3_id = store.create_thread(l3_meta).await.unwrap();
        let l3_msgs = vec![BaseMessage::human("L3-a")];
        store.append_messages(&l3_id, &l3_msgs).await.unwrap();

        // load_context(L3) 应返回：L1 快照 2 条 + L2 快照 1 条 + L3 全部 1 条 = 4 条
        let ctx = store.load_context(&l3_id).await.unwrap();
        assert_eq!(ctx.len(), 4, "三层嵌套应返回 L1(2) + L2(1) + L3(1) = 4 条消息");
        assert_eq!(ctx[0].content(), "L1-a");
        assert_eq!(ctx[1].content(), "L1-b");
        assert_eq!(ctx[2].content(), "L2-a");
        assert_eq!(ctx[3].content(), "L3-a");
    }
