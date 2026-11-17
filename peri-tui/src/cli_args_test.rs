//! Tests for cli_args module.

use super::{OutputFormat, PluginScope};

#[test]
fn test_output_format_parse() {
    // 合法值
    assert_eq!(
        "text".parse::<OutputFormat>().expect(
            "output format parse: the fixture value must satisfy its target representation"
        ),
        OutputFormat::Text
    );
    assert_eq!(
        "json".parse::<OutputFormat>().expect(
            "output format parse: the fixture value must satisfy its target representation"
        ),
        OutputFormat::Json
    );
    assert_eq!(
        "stream-json".parse::<OutputFormat>().expect(
            "output format parse: the fixture value must satisfy its target representation"
        ),
        OutputFormat::StreamJson
    );
    // 非法值返回中文错误
    let err = "xml"
        .parse::<OutputFormat>()
        .expect_err("output format parse: the invalid fixture must be rejected");
    assert!(err.contains("未知的输出格式"), "错误消息应包含中文提示");
    assert!(err.contains("xml"));
}

#[test]
fn test_plugin_scope_parse() {
    assert_eq!(
        "user"
            .parse::<PluginScope>()
            .expect("plugin scope parse: the fixture value must satisfy its target representation"),
        PluginScope::User
    );
    assert_eq!(
        "project"
            .parse::<PluginScope>()
            .expect("plugin scope parse: the fixture value must satisfy its target representation"),
        PluginScope::Project
    );
    assert_eq!(
        "local"
            .parse::<PluginScope>()
            .expect("plugin scope parse: the fixture value must satisfy its target representation"),
        PluginScope::Local
    );
    // 非法值
    let err = "global"
        .parse::<PluginScope>()
        .expect_err("plugin scope parse: the invalid fixture must be rejected");
    assert!(err.contains("未知的插件范围"));

    // From<PluginScope> for InstallScope 转换
    assert_eq!(
        peri_acp_types::plugin::InstallScope::from(PluginScope::User),
        peri_acp_types::plugin::InstallScope::User
    );
    assert_eq!(
        peri_acp_types::plugin::InstallScope::from(PluginScope::Project),
        peri_acp_types::plugin::InstallScope::Project
    );
    assert_eq!(
        peri_acp_types::plugin::InstallScope::from(PluginScope::Local),
        peri_acp_types::plugin::InstallScope::Local
    );
}
