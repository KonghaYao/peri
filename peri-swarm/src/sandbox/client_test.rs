//! Pure checks for the Bun bridge client (S8a3a). The HTTP path is exercised by
//! `tests/openai_hosted_test.rs` against a fake Bun service.

use super::{is_valid_logical_env_id, BunSandboxClient, SandboxClientConfig};
use url::Url;

fn config() -> SandboxClientConfig {
    SandboxClientConfig {
        base_url: Url::parse("http://127.0.0.1:8081").expect("url"),
        token: Some("outer-secret".to_owned()),
    }
}

#[test]
fn logical_env_id_pattern_matches_the_bun_service() {
    for valid in ["env_abc", "a", "A9._-", &"x".repeat(128)] {
        assert!(is_valid_logical_env_id(valid), "{valid}");
    }
    for invalid in ["", "_leading", "-leading", "with/slash", &"x".repeat(129)] {
        assert!(!is_valid_logical_env_id(invalid), "{invalid}");
    }
}

#[test]
fn debug_never_renders_the_outer_token() {
    let client = BunSandboxClient::new(config());
    let rendered = format!("{client:?}");
    assert!(!rendered.contains("outer-secret"));
    assert!(rendered.contains("<redacted>"));
    assert!(rendered.contains("SandboxClientConfig"));
}
