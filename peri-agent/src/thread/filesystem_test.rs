    fn make_meta(cwd: &str) -> ThreadMeta {
        ThreadMeta::new(cwd)
    }

    #[tokio::test]
    async fn test_create_and_load_thread() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");

        let id = store.create_thread(meta.clone()).await.unwrap();
        assert_eq!(id, meta.id);

        let loaded = store.load_meta(&id).await.unwrap();
        assert_eq!(loaded.id, meta.id);
        assert_eq!(loaded.cwd, "/test");
    }

    #[tokio::test]
    async fn test_append_and_load_messages() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");
        let id = store.create_thread(meta).await.unwrap();

        let msgs = vec![BaseMessage::human("Hello"), BaseMessage::ai("World")];
        store.append_messages(&id, &msgs).await.unwrap();

        let loaded = store.load_messages(&id).await.unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[tokio::test]
    async fn test_append_empty_messages_noop() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");
        let id = store.create_thread(meta).await.unwrap();

        store.append_messages(&id, &[]).await.unwrap();
        let loaded = store.load_messages(&id).await.unwrap();
        assert!(loaded.is_empty());
    }

    #[tokio::test]
    async fn test_message_count_updates() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");
        let id = store.create_thread(meta).await.unwrap();

        let msgs = vec![BaseMessage::human("msg1")];
        store.append_messages(&id, &msgs).await.unwrap();

        let loaded = store.load_meta(&id).await.unwrap();
        assert_eq!(loaded.message_count, 1);
    }

    #[tokio::test]
    async fn test_title_extracted_from_first_human() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");
        let id = store.create_thread(meta).await.unwrap();

        let msgs = vec![BaseMessage::human("This is my question about Rust")];
        store.append_messages(&id, &msgs).await.unwrap();

        let loaded = store.load_meta(&id).await.unwrap();
        assert_eq!(
            loaded.title.as_deref(),
            Some("This is my question about Rust")
        );
    }

    #[tokio::test]
    async fn test_list_threads_sorted_by_updated_at() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());

        let meta1 = make_meta("/a");
        let id1 = meta1.id.clone();
        store.create_thread(meta1).await.unwrap();

        let meta2 = make_meta("/b");
        let id2 = meta2.id.clone();
        store.create_thread(meta2).await.unwrap();

        let list = store.list_threads().await.unwrap();
        assert_eq!(list.len(), 2);
        // Second created should be first (most recent updated_at)
        assert_eq!(list[0].id, id2);
        assert_eq!(list[1].id, id1);
    }

    #[tokio::test]
    async fn test_delete_thread() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");
        let id = store.create_thread(meta).await.unwrap();

        store.delete_thread(&id).await.unwrap();

        let list = store.list_threads().await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn test_update_meta() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");
        let id = store.create_thread(meta).await.unwrap();

        let mut updated = store.load_meta(&id).await.unwrap();
        updated.title = Some("new title".into());
        store.update_meta(&id, updated.clone()).await.unwrap();

        let loaded = store.load_meta(&id).await.unwrap();
        assert_eq!(loaded.title.as_deref(), Some("new title"));
    }

    #[tokio::test]
    async fn test_content_size_in_list() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let meta = make_meta("/test");
        let id = store.create_thread(meta).await.unwrap();

        let msgs = vec![BaseMessage::human("Hello world")];
        store.append_messages(&id, &msgs).await.unwrap();

        let list = store.list_threads().await.unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].content_size > 0);
    }

    #[tokio::test]
    async fn test_load_messages_nonexistent_thread() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let msgs = store
            .load_messages(&"nonexistent".to_string())
            .await
            .unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_extract_title_from_text() {
        let msgs = vec![BaseMessage::human("Hello world")];
        assert_eq!(extract_title(&msgs), Some("Hello world".to_string()));
    }

    #[test]
    fn test_extract_title_truncates_50_chars() {
        let long: String = "a".repeat(100);
        let msgs = vec![BaseMessage::human(long.as_str())];
        let title = extract_title(&msgs).unwrap();
        assert_eq!(title.chars().count(), 50);
    }

    #[test]
    fn test_extract_title_empty_messages() {
        let msgs: Vec<BaseMessage> = vec![];
        assert!(extract_title(&msgs).is_none());
    }

    #[tokio::test]
    async fn test_delete_messages_since_truncates_jsonl() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let id = store.create_thread(make_meta("/test")).await.unwrap();

        let msgs = vec![
            BaseMessage::human("m1"),
            BaseMessage::human("m2"),
            BaseMessage::human("m3"),
            BaseMessage::human("m4"),
        ];
        store.append_messages(&id, &msgs).await.unwrap();

        let target_id = msgs[1].id();
        store.delete_messages_since(&id, &target_id).await.unwrap();

        let loaded = store.load_messages(&id).await.unwrap();
        assert_eq!(loaded.len(), 2, "delete_messages_since 应保留 target 及之前");
        assert_eq!(loaded[0].id(), msgs[0].id());
        assert_eq!(loaded[1].id(), msgs[1].id());

        // meta.message_count 应同步刷新
        let meta = store.load_meta(&id).await.unwrap();
        assert_eq!(meta.message_count, 2);
    }

    #[tokio::test]
    async fn test_delete_messages_since_unknown_id_is_noop() {
        let dir = tempdir().unwrap();
        let store = FilesystemThreadStore::new(dir.path());
        let id = store.create_thread(make_meta("/test")).await.unwrap();

        store
            .append_messages(&id, &[BaseMessage::human("only")])
            .await
            .unwrap();

        let ghost = crate::messages::MessageId::new();
        store.delete_messages_since(&id, &ghost).await.unwrap();

        let loaded = store.load_messages(&id).await.unwrap();
        assert_eq!(loaded.len(), 1, "未知 message_id 应为 no-op");
    }
