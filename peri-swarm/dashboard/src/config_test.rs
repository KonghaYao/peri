use std::fs;
use std::path::Path;

use super::*;

const SECRET: &str = "it-api-key-0123456789-do-not-log";
const OBSERVABILITY_SECRET: &str = "oo-password-9876543210-do-not-log";

fn observability() -> ObservabilityConfig {
    ObservabilityConfig::new(
        "http://127.0.0.1:5080/",
        " peri ",
        " dashboard ",
        OBSERVABILITY_SECRET,
    )
    .expect("observability config")
}

#[test]
fn config_path_is_under_the_home_directory() {
    let path = config_path_in(Path::new("/tmp/example-home"));
    assert_eq!(
        path,
        Path::new("/tmp/example-home/.config/peri-swarm-dashboard/config.toml")
    );
}

#[test]
fn base_url_is_normalized_to_an_origin() {
    assert_eq!(
        normalize_base_url("http://127.0.0.1:8080/").unwrap(),
        "http://127.0.0.1:8080"
    );
    assert_eq!(
        normalize_base_url("  http://127.0.0.1:8080/v1  ").unwrap(),
        "http://127.0.0.1:8080"
    );
    assert_eq!(
        normalize_base_url("http://127.0.0.1:8080/v1/").unwrap(),
        "http://127.0.0.1:8080"
    );
    assert_eq!(
        normalize_base_url("https://swarm.example.com:443/v1").unwrap(),
        "https://swarm.example.com"
    );
    assert_eq!(
        normalize_base_url("http://[::1]:8080").unwrap(),
        "http://[::1]:8080"
    );
}

#[test]
fn base_url_rejects_unsupported_shapes() {
    assert_eq!(normalize_base_url("   "), Err(BaseUrlError::Empty));
    assert_eq!(normalize_base_url("not a url"), Err(BaseUrlError::Invalid));
    assert_eq!(
        normalize_base_url("ftp://swarm.example.com"),
        Err(BaseUrlError::UnsupportedScheme)
    );
    assert_eq!(
        normalize_base_url("http://user:pass@swarm.example.com"),
        Err(BaseUrlError::UserInfo)
    );
    assert_eq!(
        normalize_base_url("http://swarm.example.com/?token=1"),
        Err(BaseUrlError::QueryOrFragment)
    );
    assert_eq!(
        normalize_base_url("http://swarm.example.com/#section"),
        Err(BaseUrlError::QueryOrFragment)
    );
    assert_eq!(
        normalize_base_url("http://swarm.example.com/api"),
        Err(BaseUrlError::UnsupportedPath)
    );
    assert_eq!(
        normalize_base_url("http://swarm.example.com/v1/agents"),
        Err(BaseUrlError::UnsupportedPath)
    );
}

#[test]
fn config_requires_a_non_empty_api_key() {
    assert!(matches!(
        Config::new("http://127.0.0.1:8080", "  "),
        Err(ConfigError::EmptyApiKey)
    ));
    let config = Config::new("http://127.0.0.1:8080/v1", SECRET).unwrap();
    assert_eq!(config.base_url(), "http://127.0.0.1:8080");
    assert_eq!(config.api_key(), SECRET);
}

#[test]
fn toml_roundtrip_writes_private_directory_and_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/config.toml");
    let store = ConfigStore::at(&path);
    let config = Config::new("http://127.0.0.1:8080/v1/", SECRET).unwrap();

    store.save(&config).unwrap();
    let loaded = store.load().unwrap().unwrap();
    assert_eq!(loaded.config(), &config);
    assert!(loaded.warnings().is_empty());

    #[cfg(unix)]
    {
        let file_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let dir_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
    }
}

#[test]
fn saving_again_restores_private_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let config = Config::new("http://127.0.0.1:8080", SECRET).unwrap();
    store.save(&config).unwrap();

    #[cfg(unix)]
    {
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();
        store.save(&config).unwrap();
        let mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn missing_file_loads_as_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::at(dir.path().join("missing.toml"));
    assert!(store.load().unwrap().is_none());
}

#[test]
fn corrupt_file_is_reported_without_echoing_its_contents() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let broken =
        format!("base_url = \"http://127.0.0.1:8080\"\napi_key = \"{SECRET}\"\nbroken = [\n");
    fs::write(store.path(), &broken).unwrap();

    let error = store.load().unwrap_err();
    assert!(matches!(error, ConfigError::Corrupt { .. }));
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains(SECRET));
    // Loading must not repair or overwrite the file.
    assert_eq!(fs::read_to_string(store.path()).unwrap(), broken);
}

#[test]
fn invalid_base_url_in_file_is_reported_safely() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::at(dir.path().join("config.toml"));
    fs::write(
        store.path(),
        format!("base_url = \"http://127.0.0.1:8080/admin\"\napi_key = \"{SECRET}\"\n"),
    )
    .unwrap();

    let error = store.load().unwrap_err();
    assert!(matches!(error, ConfigError::Invalid { .. }));
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains(SECRET));
    assert!(
        rendered.contains("base URL"),
        "unexpected error: {rendered}"
    );
}

#[test]
fn unknown_fields_are_rejected_as_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::at(dir.path().join("config.toml"));
    fs::write(
        store.path(),
        format!(
            "base_url = \"http://127.0.0.1:8080\"\napi_key = \"{SECRET}\"\nprofile = \"other\"\n"
        ),
    )
    .unwrap();

    assert!(matches!(store.load(), Err(ConfigError::Corrupt { .. })));
}

#[cfg(unix)]
#[test]
fn loose_permissions_are_a_warning_and_are_not_fixed_on_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let config = Config::new("http://127.0.0.1:8080", SECRET).unwrap();
    store.save(&config).unwrap();
    fs::set_permissions(store.path(), fs::Permissions::from_mode(0o640)).unwrap();

    let loaded = store.load().unwrap().unwrap();
    assert_eq!(
        loaded.warnings(),
        &[ConfigWarning::PermissionsTooOpen { mode: 0o640 }]
    );
    let mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o640, "load must not silently chmod the config file");
}

#[test]
fn debug_redacts_the_api_key() {
    let config = Config::new("http://127.0.0.1:8080", SECRET).unwrap();
    let rendered = format!("{config:?}");
    assert!(!rendered.contains(SECRET));
    assert!(rendered.contains("<redacted>"));
    assert!(rendered.contains("http://127.0.0.1:8080"));
}

#[test]
fn delete_reports_whether_the_file_existed() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let config = Config::new("http://127.0.0.1:8080", SECRET).unwrap();
    store.save(&config).unwrap();

    assert!(store.delete().unwrap());
    assert!(!store.path().exists());
    assert!(!store.delete().unwrap());
}

#[test]
fn config_without_observability_is_unchanged_and_loads() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::at(dir.path().join("config.toml"));
    // Exact shape written before the observability section existed.
    fs::write(
        store.path(),
        format!("base_url = \"http://127.0.0.1:8080\"\napi_key = \"{SECRET}\"\n"),
    )
    .unwrap();

    let loaded = store.load().unwrap().unwrap();
    assert_eq!(loaded.config().base_url(), "http://127.0.0.1:8080");
    assert_eq!(loaded.config().api_key(), SECRET);
    assert!(loaded.config().observability().is_none());
}

#[test]
fn observability_roundtrips_through_the_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::at(dir.path().join("config.toml"));
    let config = Config::new("http://127.0.0.1:8080/v1", SECRET)
        .unwrap()
        .with_observability(observability());
    store.save(&config).unwrap();

    let text = fs::read_to_string(store.path()).unwrap();
    assert!(text.contains("[observability]"), "{text}");
    assert!(text.contains("http://127.0.0.1:5080"));

    let loaded = store.load().unwrap().unwrap();
    assert_eq!(loaded.config(), &config);
    let observability = loaded
        .config()
        .observability()
        .expect("observability section");
    assert_eq!(observability.base_url(), "http://127.0.0.1:5080");
    assert_eq!(observability.org(), "peri");
    assert_eq!(observability.user(), "dashboard");
    assert_eq!(observability.password(), OBSERVABILITY_SECRET);

    #[cfg(unix)]
    {
        let file_mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
        let dir_mode = fs::metadata(store.path().parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
    }
}

#[test]
fn unknown_observability_fields_are_rejected_as_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::at(dir.path().join("config.toml"));
    fs::write(
        store.path(),
        format!(
            "base_url = \"http://127.0.0.1:8080\"\napi_key = \"{SECRET}\"\n\
             [observability]\nbase_url = \"http://127.0.0.1:5080\"\norg = \"default\"\n\
             user = \"dashboard\"\npassword = \"{OBSERVABILITY_SECRET}\"\nextra = 1\n"
        ),
    )
    .unwrap();

    assert!(matches!(store.load(), Err(ConfigError::Corrupt { .. })));
}

#[test]
fn invalid_observability_url_reports_without_echoing_the_password() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::at(dir.path().join("config.toml"));
    fs::write(
        store.path(),
        format!(
            "base_url = \"http://127.0.0.1:8080\"\napi_key = \"{SECRET}\"\n\
             [observability]\nbase_url = \"http://user:pass@127.0.0.1:5080\"\n\
             org = \"default\"\nuser = \"dashboard\"\npassword = \"{OBSERVABILITY_SECRET}\"\n"
        ),
    )
    .unwrap();

    let error = store.load().unwrap_err();
    assert!(matches!(
        error,
        ConfigError::Invalid {
            reason: InvalidConfig::Observability(ObservabilityConfigError::UserInfo),
            ..
        }
    ));
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains(SECRET));
    assert!(!rendered.contains(OBSERVABILITY_SECRET));
    assert!(!rendered.contains("pass@"));
}

#[test]
fn observability_validation_rejects_unsupported_shapes() {
    assert_eq!(
        normalize_observability_base_url("   "),
        Err(ObservabilityConfigError::EmptyBaseUrl)
    );
    assert_eq!(
        normalize_observability_base_url("not a url"),
        Err(ObservabilityConfigError::InvalidBaseUrl)
    );
    assert_eq!(
        normalize_observability_base_url("ftp://obs.example.com"),
        Err(ObservabilityConfigError::UnsupportedScheme)
    );
    assert_eq!(
        normalize_observability_base_url("http://user:pass@obs.example.com"),
        Err(ObservabilityConfigError::UserInfo)
    );
    assert_eq!(
        normalize_observability_base_url("http://obs.example.com/?token=1"),
        Err(ObservabilityConfigError::QueryOrFragment)
    );
    assert_eq!(
        normalize_observability_base_url("http://obs.example.com/#frag"),
        Err(ObservabilityConfigError::QueryOrFragment)
    );
    assert_eq!(
        normalize_observability_base_url("http://obs.example.com/openobserve/").unwrap(),
        "http://obs.example.com/openobserve"
    );

    assert_eq!(
        ObservabilityConfig::new("http://127.0.0.1:5080", "  ", "user", "pw"),
        Err(ObservabilityConfigError::EmptyOrg)
    );
    assert_eq!(
        ObservabilityConfig::new("http://127.0.0.1:5080", "default", "  ", "pw"),
        Err(ObservabilityConfigError::EmptyUser)
    );
    assert_eq!(
        ObservabilityConfig::new("http://127.0.0.1:5080", "default", "user", "  "),
        Err(ObservabilityConfigError::EmptyPassword)
    );
}

#[test]
fn debug_redacts_both_secrets() {
    let config = Config::new("http://127.0.0.1:8080", SECRET)
        .unwrap()
        .with_observability(observability());
    let rendered = format!("{config:?}");
    assert!(!rendered.contains(SECRET));
    assert!(!rendered.contains(OBSERVABILITY_SECRET));
    assert!(rendered.contains("<redacted>"));
    assert!(rendered.contains("http://127.0.0.1:8080"));
    assert!(rendered.contains("http://127.0.0.1:5080"));
    assert!(rendered.contains("peri"));

    let observability = observability();
    let rendered = format!("{observability:?}");
    assert!(!rendered.contains(OBSERVABILITY_SECRET));
    assert!(rendered.contains("dashboard"));
}
