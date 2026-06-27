    #[tokio::test]
    async fn test_no_file_no_op() {
        let mw = AgentsMdMiddleware::new();
        let mut state = AgentState::new("/nonexistent/path");
        let result = mw.before_agent(&mut state).await;
        assert!(result.is_ok());
        assert!(contribution(&mw).is_none());
        assert_eq!(state.messages().len(), 0);
    }

    #[tokio::test]
    async fn test_with_file() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let agents_md = dir.path().join("AGENTS.md");
        std::fs::write(&agents_md, "# Project Guide\nDo things correctly.").unwrap();

        let mw = AgentsMdMiddleware::new();
        let mut state = AgentState::new(dir.path().to_str().unwrap());
        mw.before_agent(&mut state).await.unwrap();

        assert_eq!(state.messages().len(), 0, "before_agent 不应再 prepend 消息");
        let contribution = contribution(&mw).unwrap();
        assert!(contribution.contains("Project Guide"));
    }

    #[tokio::test]
    async fn test_priority_agents_over_claude() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "agents content").unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "claude content").unwrap();

        let mw = AgentsMdMiddleware::new();
        let mut state = AgentState::new(dir.path().to_str().unwrap());
        mw.before_agent(&mut state).await.unwrap();

        let contribution = contribution(&mw).unwrap();
        assert!(contribution.contains("agents content"));
    }

    #[tokio::test]
    async fn test_contribution_not_prepended_to_state() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "system instructions").unwrap();

        let mw = AgentsMdMiddleware::new();
        let mut state = AgentState::new(dir.path().to_str().unwrap());
        state.add_message(BaseMessage::human("user question"));
        mw.before_agent(&mut state).await.unwrap();

        // before_agent 不应向 state 写入消息，只缓存到 prompt_contribution
        assert_eq!(state.messages().len(), 1);
        assert!(matches!(state.messages()[0], BaseMessage::Human { .. }));
        let contribution = contribution(&mw).unwrap();
        assert!(contribution.contains("system instructions"));
    }

    #[tokio::test]
    async fn test_excludes_matching_file_skipped() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let claude_md = dir.path().join("CLAUDE.md");
        std::fs::write(&claude_md, "should be excluded").unwrap();

        let mw = AgentsMdMiddleware::new().with_excludes(vec![format!("{}", claude_md.display())]);
        let mut state = AgentState::new(dir.path().to_str().unwrap());
        mw.before_agent(&mut state).await.unwrap();

        assert!(
            contribution(&mw).is_none(),
            "excluded file should not be loaded"
        );
    }

    #[tokio::test]
    async fn test_excludes_non_matching_file_loaded() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "should be loaded").unwrap();

        let mw = AgentsMdMiddleware::new().with_excludes(vec!["**/node_modules/**".to_string()]);
        let mut state = AgentState::new(dir.path().to_str().unwrap());
        mw.before_agent(&mut state).await.unwrap();

        let contribution = contribution(&mw).unwrap();
        assert!(contribution.contains("should be loaded"));
    }

    // ── CLAUDE.local.md tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_local_md_only() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.local.md"), "local only content").unwrap();

        let mw = AgentsMdMiddleware::new();
        let mut state = AgentState::new(dir.path().to_str().unwrap());
        mw.before_agent(&mut state).await.unwrap();

        let contribution = contribution(&mw).unwrap();
        assert!(contribution.contains("local only content"));
    }

    #[tokio::test]
    async fn test_claude_md_and_local_merged() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "main content").unwrap();
        std::fs::write(dir.path().join("CLAUDE.local.md"), "local content").unwrap();

        let mw = AgentsMdMiddleware::new();
        let mut state = AgentState::new(dir.path().to_str().unwrap());
        mw.before_agent(&mut state).await.unwrap();

        let contribution = contribution(&mw).unwrap();
        assert!(contribution.contains("main content"));
        assert!(contribution.contains("local content"));
    }

    #[tokio::test]
    async fn test_local_md_empty_not_appended() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "main content").unwrap();
        std::fs::write(dir.path().join("CLAUDE.local.md"), "   \n  ").unwrap();

        let mw = AgentsMdMiddleware::new();
        let mut state = AgentState::new(dir.path().to_str().unwrap());
        mw.before_agent(&mut state).await.unwrap();

        let contribution = contribution(&mw).unwrap();
        assert!(contribution.contains("main content"));
        assert!(!contribution.contains("local"));
    }

    // ── @import tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_import_simple() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let imported = dir.path().join("rules.md");
        std::fs::write(&imported, "imported rules").unwrap();
        std::fs::write(
            dir.path().join("CLAUDE.md"),
            "header\n<!-- @import rules.md -->\nfooter",
        )
        .unwrap();

        let mw = AgentsMdMiddleware::new();
        let mut state = AgentState::new(dir.path().to_str().unwrap());
        mw.before_agent(&mut state).await.unwrap();

        let content = contribution(&mw).unwrap();
        assert!(content.contains("header"));
        assert!(content.contains("imported rules"));
        assert!(content.contains("footer"));
        assert!(!content.contains("@import"));
    }

    #[tokio::test]
    async fn test_import_nested() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let sub_dir = dir.path().join("sub");
        std::fs::create_dir_all(&sub_dir).unwrap();
        let inner = sub_dir.join("inner.md");
        std::fs::write(&inner, "inner content").unwrap();
        let outer = dir.path().join("outer.md");
        std::fs::write(&outer, "outer <!-- @import sub/inner.md --> end").unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "<!-- @import outer.md -->").unwrap();

        let mw = AgentsMdMiddleware::new();
        let mut state = AgentState::new(dir.path().to_str().unwrap());
        mw.before_agent(&mut state).await.unwrap();

        let content = contribution(&mw).unwrap();
        assert!(content.contains("inner content"));
    }

    #[test]
    fn test_import_max_depth() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let imported = dir.path().join("deep.md");
        std::fs::write(&imported, "deep content").unwrap();
        let content = "<!-- @import deep.md -->".to_string();
        let mut visited = HashSet::new();
        // depth 0 should return original content
        let result = resolve_imports(&content, dir.path(), 0, &mut visited);
        assert!(result.contains("@import"));
    }

    #[test]
    fn test_import_cycle_detection() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let a = dir.path().join("a.md");
        let b = dir.path().join("b.md");
        std::fs::write(&a, "<!-- @import b.md -->").unwrap();
        std::fs::write(&b, "<!-- @import a.md -->").unwrap();

        let main = dir.path().join("main.md");
        std::fs::write(&main, "<!-- @import a.md -->").unwrap();

        let mut visited = HashSet::new();
        visited.insert(main.clone());
        // Should not panic or infinite loop
        let result = resolve_imports(
            &std::fs::read_to_string(&main).unwrap(),
            dir.path(),
            3,
            &mut visited,
        );
        // a.md's @import b.md should be resolved, but b.md's @import a.md should be kept as-is (cycle)
        assert!(!result.is_empty());
    }

    #[test]
    fn test_import_nonexistent_file() {
        let content = "<!-- @import nonexistent.md -->";
        let mut visited = HashSet::new();
        let result = resolve_imports(content, Path::new("/tmp"), 3, &mut visited);
        assert!(
            result.contains("@import"),
            "nonexistent file should keep original placeholder"
        );
    }

    #[test]
    fn test_import_invalid_format() {
        let content = "<!-- @import no closing tag";
        let mut visited = HashSet::new();
        let result = resolve_imports(content, Path::new("/tmp"), 3, &mut visited);
        assert!(
            result.contains("@import"),
            "invalid format should preserve original text"
        );
    }
