//! RouteEntry 契约测试：构造 + level() 推导（core/ui → Level1，其余 → Level2）、
//! provenance 组合（source × lifecycle）、CommandSource domain()/namespace()/
//! level() 五域断言、CommandLifecycle 四态、CommandEntryKind serde wire 形态
//! 锁定、UiCommandSpec serde 往返。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use super::*;
use crate::command::command_handler::CommandOutcome;
use crate::command::CommandContext;

// ─── 测试 fixture ──────────────────────────────────────────────────────────

/// 不触发执行的假 handler（仅满足 RouteEntry 的 `Arc<dyn CommandHandler>` 类型要求）。
struct FakeHandler;

#[async_trait]
impl CommandHandler for FakeHandler {
    async fn execute(&self, _ctx: CommandContext) -> CommandOutcome {
        unimplemented!("测试不触发执行，仅构造用")
    }
}

/// 最小 RouteEntry fixture（调用方可覆盖需要的字段）。
fn entry(source: CommandSource) -> RouteEntry {
    RouteEntry {
        fullname: "core:compact".to_string(),
        aliases: vec![],
        description: "测试条目".to_string(),
        kind: CommandEntryKind::Command,
        category: None,
        args_schema: None,
        handler: Arc::new(FakeHandler),
        provenance: CommandProvenance {
            source,
            lifecycle: CommandLifecycle::Connected,
        },
    }
}

// ─── RouteEntry 构造 + level() 推导 ────────────────────────────────────────

#[test]
fn route_entry_level_core_ui_is_level1() {
    assert_eq!(entry(CommandSource::Core).level(), CommandLevel::Level1);
    assert_eq!(entry(CommandSource::Ui).level(), CommandLevel::Level1);
}

#[test]
fn route_entry_level_mcp_plugin_user_is_level2() {
    assert_eq!(
        entry(CommandSource::Mcp {
            server: "demo".into()
        })
        .level(),
        CommandLevel::Level2
    );
    assert_eq!(
        entry(CommandSource::Plugin { name: "ecc".into() }).level(),
        CommandLevel::Level2
    );
    assert_eq!(
        entry(CommandSource::User { name: "me".into() }).level(),
        CommandLevel::Level2
    );
}

#[test]
fn route_entry_fullname_and_kind_fields() {
    let e = entry(CommandSource::Core);
    assert_eq!(e.fullname, "core:compact");
    assert_eq!(e.kind, CommandEntryKind::Command);
    assert_eq!(e.description, "测试条目");
    assert_eq!(e.provenance.lifecycle, CommandLifecycle::Connected);
}

// ─── provenance 组合（source × lifecycle）──────────────────────────────────

#[test]
fn provenance_carries_source_and_lifecycle() {
    // 静态条目：core 域 + Connected。
    let prov = CommandProvenance {
        source: CommandSource::Core,
        lifecycle: CommandLifecycle::Connected,
    };
    assert_eq!(prov.source, CommandSource::Core);
    assert_eq!(prov.lifecycle, CommandLifecycle::Connected);

    // 动态注入生命周期转换：发现中 → 已发现 → 断连清理。
    let prov = CommandProvenance {
        source: CommandSource::Mcp {
            server: "demo".into(),
        },
        lifecycle: CommandLifecycle::Discovering,
    };
    assert_eq!(prov.lifecycle, CommandLifecycle::Discovering);
    let prov = CommandProvenance {
        lifecycle: CommandLifecycle::Discovered,
        ..prov
    };
    assert_eq!(prov.lifecycle, CommandLifecycle::Discovered);
    let prov = CommandProvenance {
        lifecycle: CommandLifecycle::Disconnecting,
        ..prov
    };
    assert_eq!(prov.lifecycle, CommandLifecycle::Disconnecting);
}

// ─── CommandSource：五域 domain / namespace / level ────────────────────────

#[test]
fn command_source_domain_all_five() {
    assert_eq!(CommandSource::Core.domain(), "core");
    assert_eq!(CommandSource::Ui.domain(), "ui");
    assert_eq!(
        CommandSource::Mcp {
            server: "demo".into()
        }
        .domain(),
        "mcp"
    );
    assert_eq!(
        CommandSource::Plugin { name: "ecc".into() }.domain(),
        "plugin"
    );
    assert_eq!(CommandSource::User { name: "me".into() }.domain(), "user");
}

#[test]
fn command_source_namespace() {
    // 第二等级：namespace = 来源域内标识（server / 插件名，设计 §58）。
    assert_eq!(
        CommandSource::Mcp {
            server: "demo".into()
        }
        .namespace(),
        Some("demo")
    );
    assert_eq!(
        CommandSource::Plugin { name: "ecc".into() }.namespace(),
        Some("ecc")
    );
    assert_eq!(
        CommandSource::User { name: "me".into() }.namespace(),
        Some("me")
    );
    // 第一等级：无 namespace。
    assert_eq!(CommandSource::Core.namespace(), None);
    assert_eq!(CommandSource::Ui.namespace(), None);
}

#[test]
fn command_source_level_all_five() {
    assert_eq!(CommandSource::Core.level(), CommandLevel::Level1);
    assert_eq!(CommandSource::Ui.level(), CommandLevel::Level1);
    assert_eq!(
        CommandSource::Mcp {
            server: "demo".into()
        }
        .level(),
        CommandLevel::Level2
    );
    assert_eq!(
        CommandSource::Plugin { name: "ecc".into() }.level(),
        CommandLevel::Level2
    );
    assert_eq!(
        CommandSource::User { name: "me".into() }.level(),
        CommandLevel::Level2
    );
}

// ─── CommandLifecycle 四态 ─────────────────────────────────────────────────

#[test]
fn command_lifecycle_four_states_distinct() {
    // 设计 §148：已连接 / 发现中 / 已发现 / 断连清理。
    let states = [
        CommandLifecycle::Connected,
        CommandLifecycle::Discovering,
        CommandLifecycle::Discovered,
        CommandLifecycle::Disconnecting,
    ];
    for (i, a) in states.iter().enumerate() {
        for (j, b) in states.iter().enumerate() {
            assert_eq!(a == b, i == j, "四态两两互异");
        }
    }
}

// ─── CommandEntryKind serde（snake_case wire 形态锁定）─────────────────────

#[test]
fn entry_kind_serializes_snake_case() {
    assert_eq!(
        serde_json::to_string(&CommandEntryKind::Command).unwrap(),
        r#""command""#
    );
    assert_eq!(
        serde_json::to_string(&CommandEntryKind::Skill).unwrap(),
        r#""skill""#
    );
    assert_eq!(
        serde_json::to_string(&CommandEntryKind::McpSkill).unwrap(),
        r#""mcp_skill""#
    );
    assert_eq!(
        serde_json::to_string(&CommandEntryKind::Panel).unwrap(),
        r#""panel""#
    );
}

#[test]
fn entry_kind_deserializes_snake_case_and_roundtrips() {
    for kind in [
        CommandEntryKind::Command,
        CommandEntryKind::Skill,
        CommandEntryKind::McpSkill,
        CommandEntryKind::Panel,
    ] {
        let json = serde_json::to_string(&kind).unwrap();
        let back: CommandEntryKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, kind, "kind {json} 往返应相等");
    }
}

// ─── UiCommandSpec serde（Phase 3 caps 上送明细的 wire 形态）────────────────

#[test]
fn ui_command_spec_serde_roundtrip_with_args() {
    let spec = UiCommandSpec {
        name: "history".to_string(),
        aliases: vec!["h".to_string(), "hist".to_string()],
        description: "查看历史".to_string(),
        args: Some(ArgsSchema::default()),
    };
    let json = serde_json::to_string(&spec).unwrap();
    let back: UiCommandSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(back, spec);
}

#[test]
fn ui_command_spec_minimal_defaults() {
    // 缺省字段：aliases 默认空、args 省略（skip_serializing_if）。
    let spec: UiCommandSpec =
        serde_json::from_str(r#"{"name":"history","description":"查看历史"}"#).unwrap();
    assert!(spec.aliases.is_empty());
    assert_eq!(spec.args, None);

    let v = serde_json::to_value(&spec).unwrap();
    assert_eq!(
        v,
        json!({"name": "history", "description": "查看历史", "aliases": []}),
        "args=None 时应省略该键"
    );
}
