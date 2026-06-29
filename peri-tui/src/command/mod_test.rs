

    // ── StubCommand ──

    struct StubCommand {
        n: &'static str,
        called: Arc<AtomicBool>,
        last_args: Arc<parking_lot::Mutex<String>>,
        aliases_vec: Vec<&'static str>,
    }

    impl Command for StubCommand {
        fn name(&self) -> &str {
            self.n
        }
        fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
            "stub".to_string()
        }
        fn aliases(&self) -> Vec<&str> {
            self.aliases_vec.clone()
        }
        fn execute(&self, _app: &mut App, args: &str) -> Vec<Effect> {
            self.called.store(true, Ordering::Relaxed);
            *self.last_args.lock() = args.to_string();
            vec![]
        }
    }

    fn make_lc() -> crate::i18n::LcRegistry {
        crate::i18n::LcRegistry::default()
    }

    fn make_stub(
        name: &'static str,
    ) -> (
        StubCommand,
        Arc<AtomicBool>,
        Arc<parking_lot::Mutex<String>>,
    ) {
        make_stub_with_aliases(name, vec![])
    }

    fn make_stub_with_aliases(
        name: &'static str,
        aliases: Vec<&'static str>,
    ) -> (
        StubCommand,
        Arc<AtomicBool>,
        Arc<parking_lot::Mutex<String>>,
    ) {
        let called = Arc::new(AtomicBool::new(false));
        let last_args = Arc::new(parking_lot::Mutex::new(String::new()));
        (
            StubCommand {
                n: name,
                called: called.clone(),
                last_args: last_args.clone(),
                aliases_vec: aliases,
            },
            called,
            last_args,
        )
    }

    async fn headless_app() -> App {
        App::new_headless(80, 24).await.0
    }

    // ── dispatch 精确匹配 ──

    #[tokio::test]
    async fn test_dispatch_exact_match() {
        let mut r = CommandRegistry::new();
        let (stub, called, _) = make_stub("model");
        r.register(Box::new(stub));
        let mut app = headless_app().await;
        assert!(
            r.dispatch(&mut app, "/model").is_some(),
            "exact match should return Some"
        );
        assert!(called.load(Ordering::Relaxed), "command should be called");
    }

    #[tokio::test]
    async fn test_dispatch_no_match() {
        let mut r = CommandRegistry::new();
        let (stub, _, _) = make_stub("model");
        r.register(Box::new(stub));
        let mut app = headless_app().await;
        assert!(
            r.dispatch(&mut app, "/unknown").is_none(),
            "unknown command should return None"
        );
    }

    // ── 前缀唯一匹配 ──

    #[tokio::test]
    async fn test_dispatch_prefix_unique() {
        let mut r = CommandRegistry::new();
        let (stub, called, _) = make_stub("model");
        r.register(Box::new(stub));
        let mut app = headless_app().await;
        assert!(
            r.dispatch(&mut app, "/mo").is_some(),
            "unique prefix should return Some"
        );
        assert!(
            called.load(Ordering::Relaxed),
            "command should be called via prefix"
        );
    }

    #[tokio::test]
    async fn test_dispatch_prefix_ambiguous() {
        let mut r = CommandRegistry::new();
        let (stub1, called1, _) = make_stub("model");
        let (stub2, called2, _) = make_stub("mock");
        r.register(Box::new(stub1));
        r.register(Box::new(stub2));
        let mut app = headless_app().await;
        assert!(
            r.dispatch(&mut app, "/m").is_none(),
            "ambiguous prefix should return None"
        );
        assert!(!called1.load(Ordering::Relaxed));
        assert!(!called2.load(Ordering::Relaxed));
    }

    // ── 参数传递 ──

    #[tokio::test]
    async fn test_dispatch_with_args() {
        let mut r = CommandRegistry::new();
        let (stub, _, last_args) = make_stub("model");
        r.register(Box::new(stub));
        let mut app = headless_app().await;
        r.dispatch(&mut app, "/model opus");
        assert_eq!(*last_args.lock(), "opus", "args should be passed correctly");
    }

    // ── 辅助方法（纯逻辑，无需 App）──

    #[test]
    fn test_match_prefix_returns_matching() {
        let mut r = CommandRegistry::new();
        let (s1, _, _) = make_stub("model");
        let (s2, _, _) = make_stub("mock");
        let (s3, _, _) = make_stub("clear");
        r.register(Box::new(s1));
        r.register(Box::new(s2));
        r.register(Box::new(s3));
        let matches = r.match_prefix("mo", &make_lc());
        assert_eq!(matches.len(), 2, "should match 'model' and 'mock'");
    }

    #[test]
    fn test_list_returns_all() {
        let mut r = CommandRegistry::new();
        let (s1, _, _) = make_stub("a");
        let (s2, _, _) = make_stub("b");
        let (s3, _, _) = make_stub("c");
        r.register(Box::new(s1));
        r.register(Box::new(s2));
        r.register(Box::new(s3));
        assert_eq!(r.list(&make_lc()).len(), 3, "list should return all 3 commands");
    }

    #[tokio::test]
    async fn test_dispatch_empty_prefix() {
        let mut r = CommandRegistry::new();
        let (s1, _, _) = make_stub("model");
        let (s2, _, _) = make_stub("clear");
        r.register(Box::new(s1));
        r.register(Box::new(s2));
        let mut app = headless_app().await;
        // "/" → empty name, all commands match → ambiguous → false
        assert!(
            r.dispatch(&mut app, "/").is_none(),
            "empty prefix should return None when ambiguous"
        );
    }

    // ── 别名匹配 ──

    #[tokio::test]
    async fn test_alias_exact_match() {
        let mut r = CommandRegistry::new();
        let (stub, called, _) = make_stub_with_aliases("clear", vec!["reset", "new"]);
        r.register(Box::new(stub));
        let mut app = headless_app().await;
        assert!(
            r.dispatch(&mut app, "/reset").is_some(),
            "alias exact match should return Some"
        );
        assert!(called.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_alias_no_match() {
        let mut r = CommandRegistry::new();
        let (stub, _, _) = make_stub("model");
        r.register(Box::new(stub));
        let mut app = headless_app().await;
        assert!(
            r.dispatch(&mut app, "/reset").is_none(),
            "no alias should return None"
        );
    }

    #[tokio::test]
    async fn test_name_priority_over_alias() {
        let mut r = CommandRegistry::new();
        let (s1, called1, _) = make_stub("reset");
        let (s2, called2, _) = make_stub_with_aliases("clear", vec!["reset"]);
        r.register(Box::new(s1));
        r.register(Box::new(s2));
        let mut app = headless_app().await;
        assert!(r.dispatch(&mut app, "/reset").is_some());
        assert!(called1.load(Ordering::Relaxed), "name exact should win");
        assert!(!called2.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_alias_prefix_match() {
        let mut r = CommandRegistry::new();
        let (stub, called, _) = make_stub_with_aliases("clear", vec!["reset"]);
        r.register(Box::new(stub));
        let mut app = headless_app().await;
        assert!(
            r.dispatch(&mut app, "/res").is_some(),
            "alias prefix unique match should return Some"
        );
        assert!(called.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_alias_prefix_ambiguous() {
        let mut r = CommandRegistry::new();
        let (s1, called1, _) = make_stub_with_aliases("clear", vec!["reset"]);
        let (s2, called2, _) = make_stub("real");
        r.register(Box::new(s1));
        r.register(Box::new(s2));
        let mut app = headless_app().await;
        assert!(
            r.dispatch(&mut app, "/re").is_none(),
            "ambiguous alias prefix should return None"
        );
        assert!(!called1.load(Ordering::Relaxed));
        assert!(!called2.load(Ordering::Relaxed));
    }

    #[test]
    fn test_match_prefix_covers_aliases() {
        let mut r = CommandRegistry::new();
        let (s, _, _) = make_stub_with_aliases("clear", vec!["reset"]);
        r.register(Box::new(s));
        let matches = r.match_prefix("res", &make_lc());
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, "clear");
    }

    #[test]
    fn test_list_includes_aliases() {
        let mut r = CommandRegistry::new();
        let (s, _, _) = make_stub_with_aliases("clear", vec!["reset", "new"]);
        r.register(Box::new(s));
        let list = r.list(&make_lc());
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, "clear");
        assert_eq!(list[0].2, vec!["reset".to_string(), "new".to_string()]);
    }

    #[test]
    fn test_no_alias_backward_compat() {
        let mut r = CommandRegistry::new();
        let (s, _, _) = make_stub("model");
        r.register(Box::new(s));
        let list = r.list(&make_lc());
        assert_eq!(list[0].2, Vec::<String>::new());
        let matches = r.match_prefix("mo", &make_lc());
        assert_eq!(matches.len(), 1);
    }
