use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::command::command_handler::{CommandHandler, CommandOutcome};
use peri_acp_types::command::command_route::{
    CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource, RouteEntry,
};
use peri_acp_types::command::{CommandContext, CommandResult, PromptStopReason};
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::PeriCaps;

use super::commands::{build_available_commands_update, register_ui_entries, ui_route_entries};
use crate::session::command::register_builtins;

// ─── 辅助 ───────────────────────────────────────────────────────────────────

/// 假 handler：仅占位（投影测试只断言 RouteEntry 元数据，不触发执行）。
struct FakeHandler;

#[async_trait]
impl CommandHandler for FakeHandler {
    async fn execute(&self, _ctx: CommandContext) -> CommandOutcome {
        CommandOutcome::Done(CommandResult {
            messages: Vec::new(),
            stop_reason: PromptStopReason::EndTurn,
            feedback: None,
        })
    }
}

/// 内置注册表（register_builtins：core:compact / core:bg / core:clear /
/// core:rewind / core:loop，全部 kind=Command、core 域）。
fn builtin_registry() -> CommandRegistry {
    let reg = CommandRegistry::new();
    register_builtins(&reg);
    reg
}

fn core_entry(fullname: &str, kind: CommandEntryKind) -> Arc<RouteEntry> {
    Arc::new(RouteEntry {
        fullname: fullname.into(),
        aliases: Vec::new(),
        description: format!("desc of {fullname}"),
        kind,
        category: None,
        args_schema: None,
        handler: Arc::new(FakeHandler),
        provenance: CommandProvenance {
            source: CommandSource::Core,
            lifecycle: CommandLifecycle::Discovered,
        },
    })
}

fn mcp_entry(fullname: &str, server: &str) -> Arc<RouteEntry> {
    Arc::new(RouteEntry {
        fullname: fullname.into(),
        aliases: Vec::new(),
        description: format!("desc of {fullname}"),
        kind: CommandEntryKind::McpSkill,
        category: None,
        args_schema: None,
        handler: Arc::new(FakeHandler),
        provenance: CommandProvenance {
            source: CommandSource::Mcp {
                server: server.into(),
            },
            lifecycle: CommandLifecycle::Discovered,
        },
    })
}

fn plugin_entry(fullname: &str, plugin: &str) -> Arc<RouteEntry> {
    Arc::new(RouteEntry {
        fullname: fullname.into(),
        aliases: Vec::new(),
        description: format!("desc of {fullname}"),
        kind: CommandEntryKind::Command,
        category: None,
        args_schema: None,
        handler: Arc::new(FakeHandler),
        provenance: CommandProvenance {
            source: CommandSource::Plugin {
                name: plugin.into(),
            },
            lifecycle: CommandLifecycle::Discovered,
        },
    })
}

// ─── 投影纯函数（Phase 3 步骤 3/7）──────────────────────────────────────────

/// 无 cap（外部客户端）：availableCommands = 仅注册表投影条目（基座 5 条
/// 内置），无 ui / skill / mcp 条目；每条 name = Level1 裸名、_meta.periKind /
/// periLevel 恒有（任务书 Step 7 断言面）。
#[test]
fn test_update_no_caps_projects_registry_entries_only() {
    let reg = builtin_registry();
    let caps = PeriCaps::default();
    let update = build_available_commands_update(&reg.snapshot(), &caps);
    let value = serde_json::to_value(&update).unwrap();
    let commands = value["availableCommands"].as_array().unwrap();

    assert_eq!(commands.len(), 5, "无 cap 时仅注册表投影（基座 5 条内置）");
    let names: Vec<&str> = commands
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for base in ["compact", "bg", "clear", "rewind", "loop"] {
        assert!(names.contains(&base), "基座条目 {base} 应存在: {names:?}");
    }
    for c in commands {
        let name = c["name"].as_str().unwrap();
        assert!(
            !name.contains(':'),
            "name 应为 Level1 裸名（无域前缀），实际: {}",
            c["name"]
        );
        assert_eq!(c["_meta"]["periKind"], "command", "内置命令 kind = command");
        assert_eq!(c["_meta"]["periLevel"], 1, "core 域 level = 1");
        // 基座 category 全 None → 不附加；args 未声明者不附加（core:bg
        // 亦已声明无参 schema，P2-5 与 compact/clear 对齐，下方单独断言）；
        // aliases 按命令实现声明注入
        assert!(
            c["_meta"].get("periCategory").is_none(),
            "基座条目不得附加 periCategory（全 None）"
        );
        assert_eq!(
            c["_meta"].get("periArgs").is_some(),
            name == "clear" || name == "compact" || name == "rewind" || name == "bg",
            "clear/compact/rewind/bg 已声明 args schema（Phase 5 Step 3-5 + P2-5），实际: {name}"
        );
    }
    let by_name = |n: &str| {
        commands
            .iter()
            .find(|c| c["name"] == n)
            .unwrap_or_else(|| panic!("条目 {n} 应存在"))
    };
    assert_eq!(
        by_name("compact")["_meta"]["periAliases"],
        serde_json::json!(["compress"]),
        "内置命令 aliases 应注入 periAliases"
    );
    assert_eq!(
        by_name("clear")["_meta"]["periAliases"],
        serde_json::json!(["cls", "reset"])
    );
    // Phase 5 Step 3：clear 无参命令，投影附加空 schema（三维度全空）
    assert_eq!(
        by_name("clear")["_meta"]["periArgs"],
        serde_json::json!({"positionals": [], "named": [], "flags": []}),
        "clear 已声明 ArgsSchema::default()，投影应附加空 schema"
    );
    // P2-5：bg 与 compact/clear 对齐（Some(ArgsSchema::default())），投影同样
    // 附加空 schema（free-form 参数零校验语义不变）。
    assert_eq!(
        by_name("bg")["_meta"]["periArgs"],
        serde_json::json!({"positionals": [], "named": [], "flags": []}),
        "bg 已声明 ArgsSchema::default()，投影应附加空 schema"
    );
    assert_eq!(
        by_name("rewind")["_meta"]["periAliases"],
        serde_json::json!(["undo"])
    );
    assert!(
        by_name("loop")["_meta"].get("periAliases").is_none(),
        "无 aliases 的条目不得附加 periAliases"
    );
    // 无 ui 条目（未协商 ui_commands → 无 panel 条目）
    assert!(
        commands.iter().all(|c| c["_meta"]["periKind"] != "panel"),
        "无 ui 明细协商时不得出现 panel 条目"
    );
    // caps.skill_names=false → 无 skillNames key；mcpSkillNames 键退役（Phase 6
    // D1：kind 已入条目级 _meta.periKind，update 级镜像键不再写入）
    assert!(value["_meta"].get("skillNames").is_none());
    assert!(value["_meta"].get("mcpSkillNames").is_none());
}

/// 全 cap（TUI 内部路径）：模拟调用点准备动作（ui 注册 + 注册表投影）——
/// 基座 + caps 明细注册的 ui 条目（`ui:<name>` 全名 + periKind=panel）。
/// ui:clear 与内置 core:clear 第一等级裸名互斥（设计 §63/§64），注册被纯拒绝，
/// 不覆盖不静默（warn）——其余 10 条 ui 明细全部注册成功。
#[test]
fn test_update_all_enabled_includes_default_ui_details() {
    let reg = builtin_registry();
    let caps = PeriCaps::all_enabled();
    assert_eq!(caps.ui_commands.len(), 11, "默认 ui 明细应为 11 条");

    // 模拟调用点（P1-1 拆分后形态）：ui 注册为独立步骤（on_change 挂载前
    // 一次性），投影直接取注册表 snapshot（Phase 6 A4）。
    register_ui_entries(&caps, &reg);
    let update = build_available_commands_update(&reg.snapshot(), &caps);
    let value = serde_json::to_value(&update).unwrap();
    let commands = value["availableCommands"].as_array().unwrap();
    let names: Vec<&str> = commands
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();

    for base in ["compact", "bg", "clear", "rewind", "loop"] {
        assert!(names.contains(&base), "基座条目 {base} 应存在: {names:?}");
    }
    for ui in [
        "help", "context", "cost", "mode", "effort", "history", "agents", "rename", "lang", "exit",
    ] {
        assert!(names.contains(&ui), "ui 明细 {ui} 应注册并投影: {names:?}");
    }
    // ui:clear 与 core:clear 第一等级裸名互斥，注册被纯拒绝（warn）——投影
    // 中仅存在 core:clear 的裸名条目（kind=command），无 panel 形态的 clear。
    assert!(
        !commands
            .iter()
            .any(|c| c["name"] == "clear" && c["_meta"]["periKind"] == "panel"),
        "ui:clear 与 core:clear 第一等级裸名冲突，注册应被纯拒绝（warn）"
    );
    assert_eq!(commands.len(), 15, "基座 5 + ui 注册成功 10 = 15 条");

    // 断言每条条目 _meta.periKind / periLevel 存在、name = 投影名（Level1
    // 裸名——core/ui 域均无域前缀，域归属只经 periKind 区分）
    for c in commands {
        let name = c["name"].as_str().unwrap();
        let meta = &c["_meta"];
        assert!(meta.get("periKind").is_some(), "{name} 缺 periKind");
        assert!(meta.get("periLevel").is_some(), "{name} 缺 periLevel");
        assert!(
            !name.contains(':'),
            "Level1（core/ui 域）name 应为裸名，实际: {name}"
        );
        if meta["periKind"] == "panel" {
            assert_eq!(meta["periLevel"], 1, "ui 域 level = 1");
        } else {
            assert_eq!(meta["periKind"], "command");
        }
    }
    // 全 cap：skillNames 镜像 key 出现（无 Skill 条目 → 空数组）
    assert_eq!(value["_meta"]["skillNames"], serde_json::json!([]));
}

/// P1-1 回归：register_ui_entries 幂等——同 fullname 已存在即跳过（不 warn、
/// 不触发 on_change、不重复登记）。session/load 复用进程内 registry 重放
/// 注册时不再刷「命令注册冲突」warn。
#[test]
fn test_register_ui_entries_idempotent() {
    let reg = builtin_registry();
    let caps = PeriCaps::all_enabled();

    register_ui_entries(&caps, &reg);
    let names: Vec<String> = reg.snapshot().iter().map(|e| e.fullname.clone()).collect();
    assert!(
        names.iter().any(|n| n == "ui:help"),
        "首次注册应写入 ui 条目"
    );
    assert!(
        !names.iter().any(|n| n == "ui:clear"),
        "ui:clear 与 core:clear 裸名互斥，首次即被纯拒绝"
    );
    let n1 = names.len();

    // 重放（session/load 路径）：已注册条目全部跳过，注册表不变
    register_ui_entries(&caps, &reg);
    let n2 = reg.snapshot().len();
    assert_eq!(n1, n2, "重放注册不得改变注册表内容");
}

/// _meta 可选字段注入：非空 aliases / Some(category) / Some(args_schema) →
/// periAliases / periCategory / periArgs 附加。
#[test]
fn test_update_meta_injects_aliases_category_args() {
    let entry = Arc::new(RouteEntry {
        fullname: "core:demo".into(),
        aliases: vec!["d".into(), "dem".into()],
        description: "Demo".into(),
        kind: CommandEntryKind::Command,
        category: Some("utility".into()),
        args_schema: Some(Default::default()),
        handler: Arc::new(FakeHandler),
        provenance: CommandProvenance {
            source: CommandSource::Core,
            lifecycle: CommandLifecycle::Connected,
        },
    });
    let caps = PeriCaps::default();
    let update = build_available_commands_update(&[entry], &caps);
    let value = serde_json::to_value(&update).unwrap();
    let cmd = &value["availableCommands"][0];
    assert_eq!(cmd["name"], "demo");
    assert_eq!(cmd["_meta"]["periAliases"], serde_json::json!(["d", "dem"]));
    assert_eq!(cmd["_meta"]["periCategory"], "utility");
    assert!(cmd["_meta"]["periArgs"].is_object(), "periArgs 应为 object");
}

/// Phase 6 D1 断言重写：投影 = snapshot 全量（内置 + 本地 + MCP + 插件条目），
/// 不做按名去重——`core:hello` 与 `demo:hello` **共存**（键空间不相交 =
/// 键唯一性而非按名去重；Level1 裸名 `hello` 与 Level2 全名 `demo:hello`
/// 投影名互不冲突）；条目级 periKind / periLevel 正确；skillNames 仅
/// core 域 Skill 条目裸名；mcpSkillNames 键退役（任何情况不出现）。
#[test]
fn test_update_projects_snapshot_entries_with_kinds() {
    let entries: Vec<Arc<RouteEntry>> = vec![
        core_entry("core:hello", CommandEntryKind::Command),
        core_entry("core:my-skill", CommandEntryKind::Skill),
        mcp_entry("demo:hello", "demo"),
        mcp_entry("demo:world", "demo"),
        plugin_entry("plugin:ecc:deploy", "ecc"),
    ];
    let caps = PeriCaps::all_enabled();
    let update = build_available_commands_update(&entries, &caps);
    let value = serde_json::to_value(&update).unwrap();
    let commands = value["availableCommands"].as_array().unwrap();

    // 投影 = snapshot 全量（5 条全进，无去重、无合并）
    assert_eq!(commands.len(), 5, "投影 = snapshot 全量（无去重）");
    let names: Vec<&str> = commands
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for expected in [
        "hello",
        "my-skill",
        "demo:hello",
        "demo:world",
        "plugin:ecc:deploy",
    ] {
        assert!(names.contains(&expected), "{expected} 应投影: {names:?}");
    }
    // Level1 裸名 'hello' 与 Level2 全名 'demo:hello' 共存：域信息经
    // periKind / periLevel 下发，投影名不冲突（键唯一性而非按名去重）
    assert!(
        names.contains(&"hello") && names.contains(&"demo:hello"),
        "hello 与 demo:hello 应共存: {names:?}"
    );

    let by_name = |n: &str| {
        commands
            .iter()
            .find(|c| c["name"] == n)
            .unwrap_or_else(|| panic!("条目 {n} 应存在"))
    };
    assert_eq!(by_name("hello")["_meta"]["periKind"], "command");
    assert_eq!(by_name("hello")["_meta"]["periLevel"], 1);
    assert_eq!(by_name("my-skill")["_meta"]["periKind"], "skill");
    assert_eq!(by_name("demo:hello")["_meta"]["periKind"], "mcp_skill");
    assert_eq!(
        by_name("demo:hello")["_meta"]["periLevel"],
        2,
        "mcp 域 level = 2"
    );
    assert_eq!(
        by_name("plugin:ecc:deploy")["_meta"]["periKind"],
        "command",
        "插件条目 kind = command（plugin/user 域暂归 Command）"
    );
    assert_eq!(
        by_name("plugin:ecc:deploy")["_meta"]["periLevel"],
        2,
        "plugin 域 level = 2"
    );

    // update 级 meta：skillNames 仅 core 域 Skill 条目**裸名**（与条目级
    // name 形态一致，不重复携带域前缀）；mcpSkillNames 键退役——kind 已入
    // 条目级 periKind，Hub 按条目级 kind 消费。
    assert_eq!(
        value["_meta"]["skillNames"],
        serde_json::json!(["my-skill"]),
        "skillNames 仅 core 域 Skill 条目裸名"
    );
    assert!(
        value["_meta"].get("mcpSkillNames").is_none(),
        "mcpSkillNames 键退役，不再写入"
    );
}

/// skillNames 门控保留（caps.skill_names=false → 无 key，Phase 6 D1 语义
/// 不变）；mcpSkillNames 退役后无任何 update 级 mcp 镜像键——mcp 条目
/// 分类只经条目级 periKind 下发。
#[test]
fn test_update_skill_names_gated_by_caps() {
    let entries: Vec<Arc<RouteEntry>> = vec![
        core_entry("core:my-skill", CommandEntryKind::Skill),
        mcp_entry("demo:hello", "demo"),
    ];
    let caps = PeriCaps {
        skill_names: false,
        ..PeriCaps::all_enabled()
    };
    let update = build_available_commands_update(&entries, &caps);
    let value = serde_json::to_value(&update).unwrap();
    assert!(
        value["_meta"].get("skillNames").is_none(),
        "caps.skill_names=false 时不得有 skillNames"
    );
    assert_eq!(
        value["availableCommands"].as_array().unwrap().len(),
        2,
        "mcp 条目不受 skill_names 门控影响，照常投影"
    );
    assert!(
        value["_meta"].get("mcpSkillNames").is_none(),
        "mcpSkillNames 键退役，不再写入"
    );
}

/// Phase 6 D1：本地 skill 与内置同名 → 仅内置条目存在——注册表键唯一性
/// （A2 register 冲突纯拒绝）保证同名不共存；投影 = snapshot 全量，不合并、
/// 不去重（「本地优先按名去重」合并逻辑已删除）。
#[test]
fn test_update_local_skill_same_name_as_builtin_only_builtin_exists() {
    let reg = builtin_registry();
    // 本地 skill core:compact（与内置 core:compact 同名）注册 → 冲突纯拒绝
    let err = reg.register((*core_entry("core:compact", CommandEntryKind::Skill)).clone());
    assert!(err.is_err(), "同名注册应被注册表冲突拒绝（键唯一性）");

    let caps = PeriCaps::all_enabled();
    let update = build_available_commands_update(&reg.snapshot(), &caps);
    let value = serde_json::to_value(&update).unwrap();
    let commands = value["availableCommands"].as_array().unwrap();
    assert_eq!(
        commands.len(),
        5,
        "投影 = snapshot 全量：同名 skill 未入表，仅内置 5 条"
    );
    let names: Vec<&str> = commands
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"compact"));
    let compact = commands.iter().find(|c| c["name"] == "compact").unwrap();
    assert_eq!(
        compact["_meta"]["periKind"], "command",
        "同名冲突后仅内置条目存在（kind = command，无 Skill 条目）"
    );
}

// ─── 条目构造（ui / 本地 skill 桥接 / mcp 桥接）─────────────────────────────

/// caps.ui_commands 明细 → ui 域 RouteEntry：fullname = `ui:<name>`（小写）、
/// kind = Panel、category = "ui"、args_schema 透传、provenance = Ui + Connected、
/// handler = UiDelegatePlaceholder（Delegate 回 TUI）。
#[test]
fn test_ui_route_entries_from_caps_details() {
    let caps = PeriCaps::all_enabled();
    let entries = ui_route_entries(&caps);
    assert_eq!(
        entries.len(),
        11,
        "默认明细 11 条全部构造（注册冲突由注册表裁决）"
    );

    let help = entries
        .iter()
        .find(|e| e.fullname == "ui:help")
        .expect("ui:help 应构造");
    assert_eq!(help.kind, CommandEntryKind::Panel);
    assert_eq!(help.category.as_deref(), Some("ui"));
    assert!(help.args_schema.is_none(), "默认明细无 args schema");
    assert_eq!(
        help.provenance.source,
        CommandSource::Ui,
        "provenance.source = Ui"
    );
    assert_eq!(
        help.provenance.lifecycle,
        CommandLifecycle::Connected,
        "静态 ui 条目恒 Connected"
    );
    assert_eq!(help.fullname, "ui:help");
}
