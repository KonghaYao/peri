//! Tests for update module.

use super::*;

#[test]
fn script_url_without_proxy_uses_github_raw() {
    assert_eq!(
        script_url(false, false),
        "https://raw.githubusercontent.com/konghayao/peri/main/scripts/install.sh"
    );
    assert_eq!(
        script_url(false, true),
        "https://raw.githubusercontent.com/konghayao/peri/main/scripts/install.ps1"
    );
}

/// Mirrors the README "Get started" proxy example for mainland China.
#[test]
fn script_url_with_proxy_matches_readme_example() {
    assert_eq!(
        script_url(true, false),
        "https://github-proxy-5ad00a.netlify.app/konghayao/peri/raw/main/scripts/install.sh"
    );
    assert_eq!(
        script_url(true, true),
        "https://github-proxy-5ad00a.netlify.app/konghayao/peri/raw/main/scripts/install.ps1"
    );
}

#[test]
fn proxy_env_matches_readme_example() {
    assert_eq!(
        proxy_env(true),
        &[
            ("GITHUB_PROXY", "https://github-proxy-5ad00a.netlify.app"),
            (
                "GITHUB_API_PROXY",
                "https://github-proxy-5ad00a.netlify.app/api"
            ),
        ]
    );
}

#[test]
fn proxy_env_is_empty_without_proxy() {
    assert!(proxy_env(false).is_empty());
}
