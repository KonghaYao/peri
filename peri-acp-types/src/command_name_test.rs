//! CommandName 词法解析测试：三形态解析、最右冒号切分、超层/未知域/空段/
//! 双下划线/空白拒绝、全名小写规范化 Display roundtrip、CommandLevel serde
//! roundtrip、裸名 core/ui 展开候选。

use super::*;

// ─── 三形态解析 ────────────────────────────────────────────────────────────

#[test]
fn parse_bare_name() {
    let n = CommandName::parse("compact").unwrap();
    assert_eq!(
        n,
        CommandName::Bare {
            name: "compact".into()
        }
    );
    assert_eq!(n.name(), "compact");
    assert_eq!(n.level(), CommandLevel::Level1);
}

#[test]
fn parse_level1_explicit() {
    let n = CommandName::parse("core:compact").unwrap();
    assert_eq!(
        n,
        CommandName::Level1 {
            domain: "core".into(),
            name: "compact".into(),
        }
    );
    assert_eq!(n.name(), "compact");
    assert_eq!(n.level(), CommandLevel::Level1);

    let n = CommandName::parse("ui:history").unwrap();
    assert_eq!(
        n,
        CommandName::Level1 {
            domain: "ui".into(),
            name: "history".into(),
        }
    );
    assert_eq!(n.name(), "history");
}

#[test]
fn parse_level2_rightmost_colon() {
    // 最右冒号切分：mcp:demo:hello → domain=mcp / namespace=demo / name=hello。
    let n = CommandName::parse("mcp:demo:hello").unwrap();
    assert_eq!(
        n,
        CommandName::Level2 {
            domain: "mcp".into(),
            namespace: "demo".into(),
            name: "hello".into(),
        }
    );
    assert_eq!(n.name(), "hello");
    assert_eq!(n.level(), CommandLevel::Level2);
}

#[test]
fn parse_level2_all_domains() {
    for (input, domain) in [
        ("mcp:demo:hello", "mcp"),
        ("plugin:ecc:deploy", "plugin"),
        ("user:me:greet", "user"),
    ] {
        let n = CommandName::parse(input).unwrap();
        assert_eq!(n.level(), CommandLevel::Level2);
        match n {
            CommandName::Level2 {
                domain: d,
                namespace,
                name,
            } => {
                assert_eq!(d, domain);
                assert_eq!(namespace, input.split(':').nth(1).unwrap());
                assert_eq!(name, input.split(':').nth(2).unwrap());
            }
            other => panic!("{input} 应解析为 Level2，实际: {other:?}"),
        }
    }
}

// ─── 拒绝路径 ──────────────────────────────────────────────────────────────

#[test]
fn reject_empty_input() {
    assert_eq!(CommandName::parse(""), Err(CommandNameError::Empty));
}

#[test]
fn reject_too_many_segments() {
    assert_eq!(
        CommandName::parse("a:b:c:d"),
        Err(CommandNameError::TooManySegments)
    );
    assert_eq!(
        CommandName::parse("core:compact:extra:x"),
        Err(CommandNameError::TooManySegments)
    );
    // 同时超层 + 含空段时，段数上限检查优先（规则③先于④）。
    assert_eq!(
        CommandName::parse("mcp::demo:hello"),
        Err(CommandNameError::TooManySegments)
    );
}

#[test]
fn reject_empty_segments() {
    for input in [":a", "a:", "a::b", "core:", ":core:compact", "mcp:demo:"] {
        assert_eq!(
            CommandName::parse(input),
            Err(CommandNameError::EmptySegment),
            "{input} 应拒绝"
        );
    }
}

#[test]
fn reject_double_underscore() {
    // 废弃词法整体拒绝：mcp 前缀遗留形态 + 同类双下划线形态。
    for input in ["mcp__s__k", "mcp__demo__hello", "plugin__x__y", "a__b"] {
        assert_eq!(
            CommandName::parse(input),
            Err(CommandNameError::DoubleUnderscore),
            "{input} 应拒绝"
        );
    }
}

#[test]
fn reject_unknown_domain() {
    // 未知域：1 层与 2 层形态都拒绝。
    assert_eq!(
        CommandName::parse("foo:bar"),
        Err(CommandNameError::UnknownDomain("foo".into()))
    );
    assert_eq!(
        CommandName::parse("foo:bar:baz"),
        Err(CommandNameError::UnknownDomain("foo".into()))
    );
    // 设计 §55 无域省略简写（demo:hello 废弃）。
    assert_eq!(
        CommandName::parse("demo:hello"),
        Err(CommandNameError::UnknownDomain("demo".into()))
    );
    // 第一等级域在 2 层形态不合法（core 不在第二等级域集合内）。
    assert_eq!(
        CommandName::parse("core:compact:extra"),
        Err(CommandNameError::UnknownDomain("core".into()))
    );
}

#[test]
fn reject_missing_namespace() {
    // 第二等级域缺 namespace 的 1 层形态（mcp:hello / plugin:deploy）对
    // 外部来源非法（设计 §54：namespace 显式标记不可省略）。
    assert_eq!(
        CommandName::parse("mcp:hello"),
        Err(CommandNameError::MissingNamespace {
            domain: "mcp".into()
        })
    );
    assert_eq!(
        CommandName::parse("plugin:deploy"),
        Err(CommandNameError::MissingNamespace {
            domain: "plugin".into()
        })
    );
}

#[test]
fn reject_whitespace_in_segment() {
    // 空白字符非法（防与 args 分离混淆）：开头 / 中间 / 结尾。
    assert_eq!(
        CommandName::parse(" core:compact"),
        Err(CommandNameError::IllegalCharacter(' '))
    );
    assert_eq!(
        CommandName::parse("core:comp act"),
        Err(CommandNameError::IllegalCharacter(' '))
    );
    assert_eq!(
        CommandName::parse("core:compact "),
        Err(CommandNameError::IllegalCharacter(' '))
    );
    assert_eq!(
        CommandName::parse("mcp:demo:hello\tworld"),
        Err(CommandNameError::IllegalCharacter('\t'))
    );
}

// ─── 全名小写规范化 / Display roundtrip ────────────────────────────────────

#[test]
fn full_name_is_lowercased() {
    assert_eq!(
        CommandName::parse("MCP:Demo:Hello").unwrap().full_name(),
        "mcp:demo:hello"
    );
    assert_eq!(
        CommandName::parse("Core:Compact").unwrap().full_name(),
        "core:compact"
    );
    assert_eq!(
        CommandName::parse("UI:History").unwrap().full_name(),
        "ui:history"
    );
    assert_eq!(
        CommandName::parse("SKILL-X").unwrap().full_name(),
        "skill-x"
    );
    // 直接构造大写字段也保证键不变（full_name 输出前再归一）。
    assert_eq!(
        CommandName::Level2 {
            domain: "MCP".into(),
            namespace: "Demo".into(),
            name: "Hello".into(),
        }
        .full_name(),
        "mcp:demo:hello"
    );
}

#[test]
fn display_roundtrip() {
    for input in [
        "compact",
        "core:compact",
        "ui:history",
        "mcp:demo:hello",
        "plugin:ecc:deploy",
        "user:me:greet",
    ] {
        let n = CommandName::parse(input).unwrap();
        let text = n.to_string();
        assert_eq!(
            CommandName::parse(&text),
            Ok(n),
            "Display roundtrip: {input}"
        );
    }
}

#[test]
fn display_is_lowercased_full_name() {
    let n = CommandName::parse("MCP:Demo:Hello").unwrap();
    assert_eq!(n.to_string(), "mcp:demo:hello");
    assert_eq!(
        CommandName::Bare {
            name: "Compact".into()
        }
        .to_string(),
        "compact"
    );
}

// ─── 等级 / 展开候选 / FromStr / serde ──────────────────────────────────────

#[test]
fn level_by_shape() {
    assert_eq!(
        CommandName::parse("compact").unwrap().level(),
        CommandLevel::Level1
    );
    assert_eq!(
        CommandName::parse("core:compact").unwrap().level(),
        CommandLevel::Level1
    );
    assert_eq!(
        CommandName::parse("ui:history").unwrap().level(),
        CommandLevel::Level1
    );
    assert_eq!(
        CommandName::parse("mcp:demo:hello").unwrap().level(),
        CommandLevel::Level2
    );
    assert_eq!(
        CommandName::parse("plugin:ecc:deploy").unwrap().level(),
        CommandLevel::Level2
    );
    assert_eq!(
        CommandName::parse("user:me:greet").unwrap().level(),
        CommandLevel::Level2
    );
}

#[test]
fn bare_level1_keys_expands_bare_name() {
    assert_eq!(
        CommandName::parse("compact").unwrap().bare_level1_keys(),
        Some(["core:compact".to_string(), "ui:compact".to_string()])
    );
    // 非裸名形态不展开。
    assert_eq!(
        CommandName::parse("core:compact")
            .unwrap()
            .bare_level1_keys(),
        None
    );
    assert_eq!(
        CommandName::parse("mcp:demo:hello")
            .unwrap()
            .bare_level1_keys(),
        None
    );
    // 直接构造大写字段也保证展开键小写。
    assert_eq!(
        CommandName::Bare {
            name: "Compact".into()
        }
        .bare_level1_keys(),
        Some(["core:compact".to_string(), "ui:compact".to_string()])
    );
}

#[test]
fn parse_via_from_str() {
    let n: CommandName = "mcp:demo:hello".parse().unwrap();
    assert_eq!(n, CommandName::parse("mcp:demo:hello").unwrap());
    assert!(matches!(
        "a:b:c:d".parse::<CommandName>(),
        Err(CommandNameError::TooManySegments)
    ));
    assert!(matches!(
        "".parse::<CommandName>(),
        Err(CommandNameError::Empty)
    ));
}

#[test]
fn serde_roundtrip() {
    // 仅 CommandLevel 为 wire 契约成员（计划步骤 2 代码形态）：
    // CommandName / CommandNameError 无 serde，属内存形态。
    let l = CommandLevel::Level2;
    let json = serde_json::to_string(&l).unwrap();
    assert_eq!(serde_json::from_str::<CommandLevel>(&json).unwrap(), l);
}
