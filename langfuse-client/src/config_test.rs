    #[test]
    fn test_batcher_config_default() {
        let config = BatcherConfig::default();
        assert_eq!(config.max_events, 50);
        assert_eq!(config.flush_interval, Duration::from_secs(10));
        assert_eq!(config.backpressure, BackpressurePolicy::DropNew);
        assert_eq!(config.max_retries, 3);
    }

    #[test]
    fn test_backpressure_default() {
        assert_eq!(BackpressurePolicy::default(), BackpressurePolicy::DropNew);
    }

    #[test]
    fn test_client_config_from_env() {
        temp_env::with_vars(
            [
                ("LANGFUSE_PUBLIC_KEY", Some("pk-test")),
                ("LANGFUSE_SECRET_KEY", Some("sk-test")),
                ("LANGFUSE_BASE_URL", Some("https://custom.langfuse.com")),
            ],
            || {
                let config = ClientConfig::from_env().unwrap();
                assert_eq!(config.public_key, "pk-test");
                assert_eq!(config.secret_key, "sk-test");
                assert_eq!(config.base_url, "https://custom.langfuse.com");
            },
        );
    }

    #[test]
    fn test_client_config_from_env_missing_key() {
        temp_env::with_vars_unset(["LANGFUSE_PUBLIC_KEY", "LANGFUSE_SECRET_KEY"], || {
            let result = ClientConfig::from_env();
            assert!(result.is_err());
            let err = result.unwrap_err();
            let msg = format!("{}", err);
            assert!(msg.contains("LANGFUSE_PUBLIC_KEY not set"), "got: {}", msg);
        });
    }

    #[test]
    fn test_client_config_default_base_url() {
        temp_env::with_vars(
            [
                ("LANGFUSE_PUBLIC_KEY", Some("pk")),
                ("LANGFUSE_SECRET_KEY", Some("sk")),
                ("LANGFUSE_BASE_URL", None),
            ],
            || {
                let config = ClientConfig::from_env().unwrap();
                assert_eq!(config.base_url, "https://cloud.langfuse.com");
            },
        );
    }
