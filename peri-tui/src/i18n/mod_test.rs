use super::*;

#[must_use]
fn make_lc() -> LcRegistry {
    LcRegistry::new(None)
}

fn make_lc_zh() -> LcRegistry {
    LcRegistry::new(Some("zh-CN"))
}

#[test]
fn test_lc_registry_default_lang() {
    let lc = make_lc();
    assert_eq!(lc.current_lang(), "en");
}

#[test]
fn test_lc_registry_explicit_lang() {
    let lc = LcRegistry::new(Some("zh-CN"));
    assert_eq!(lc.current_lang(), "zh-CN");
}

#[test]
fn test_lc_registry_invalid_lang_fallback() {
    let lc = LcRegistry::new(Some("fr"));
    assert_eq!(lc.current_lang(), "en");
}

#[test]
fn test_lc_registry_tr_english() {
    let lc = make_lc();
    assert_eq!(lc.tr("test-hello"), "Hello, World!");
}

#[test]
fn test_lc_registry_tr_chinese() {
    let lc = make_lc_zh();
    assert_eq!(lc.tr("test-hello"), "你好，世界！");
}

#[test]
fn test_lc_registry_tr_missing_key_fallback() {
    let lc = make_lc();
    assert_eq!(lc.tr("nonexistent-key-xyz"), "nonexistent-key-xyz");
}

#[test]
fn test_lc_registry_tr_args() {
    let lc = make_lc_zh();
    let result = lc.tr_args(
        "test-greeting",
        &[("name".into(), FluentValue::from("Alice"))],
    );
    assert_eq!(result, "你好，Alice！");
}

#[test]
fn test_lc_registry_switch_success() {
    let mut lc = make_lc();
    assert!(lc.switch("zh-CN").is_ok());
    assert_eq!(lc.current_lang(), "zh-CN");
}

#[test]
fn test_lc_registry_switch_invalid() {
    let mut lc = make_lc();
    assert!(lc.switch("invalid").is_err());
}

#[test]
fn test_lc_registry_available_langs() {
    let lc = make_lc();
    let langs = lc.available_langs();
    assert!(langs.contains(&"en"));
    assert!(langs.contains(&"zh-CN"));
}

#[test]
fn test_lc_registry_cross_lang_fallback() {
    let mut lc = make_lc_zh();
    lc.switch("zh-CN")
        .expect("lc registry cross lang fallback: the switch fixture operation uses valid inputs");
    let result = lc.tr("nonexistent-key");
    assert_eq!(result, "nonexistent-key");
}
