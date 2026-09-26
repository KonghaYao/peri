//! builtin 默认配置层（step 6.5 overlay）的 crate 内验收：注入 / 覆盖 / 关闭 /
//! 保留名 typed error / 关闭片段反例 / 写回隔离 / hash 无关性 / direct 一致性。
//!
//! 口径来源（主 plan §3 IF-D3 + §8「默认层与覆盖语义」行）：
//! 1. 缺失 → 插入完整 builtin 条目（`protocol_version = None`）；
//! 2. 存在且无 `command`/`url` → 填 `source`，`disabled != Some(true)` 时**同时**填
//!    `system_mcp` + `system_mcp_tools`（A17）；`disabled == Some(true)` 只填 `source`；
//! 3. 保留名（`web`/`artifact`/`cron`/`lsp`/`workspace`）被 `command`/`url` 接管 →
//!    加载期 typed error（A3）；
//! 5. 关闭片段形状（A18）：`{"web": {"disabled": true, "system_mcp": true}}` 必须被
//!    加载期拒绝；唯一合法写法是只写 `disabled: true`；
//! 6. direct 一致性：`system_mcp_tools` == 注册表声明为 direct 的原始工具名集合。
//!
//! 断言只落在可观察产物（加载结果、磁盘内容、typed error 变体）；不打印 env /
//! headers / 凭据，测试只使用注入的本地路径与假 server 名 / 假 token。

use std::path::{Path, PathBuf};

use peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES;
use peri_acp_types::plugin::ConfigSource;

use crate::mcp::builtin::BuiltinInjectionPolicy;
use crate::mcp::config::{
    load_merged_config_full_with_paths, remove_server_from_config_with_paths,
    set_server_disabled_with_paths, McpConfigError, McpConfigFile,
};

/// 测试用显式全局路径（不存在 → 空全局配置）：不读真实 `~/.peri/settings.json`。
fn missing_global_path(dir: &Path) -> PathBuf {
    dir.join("global-settings.json")
}

/// 在项目级 `.mcp.json` 写入 `mcpServers` 片段。
fn project_with_servers(servers_json: &str) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude_home = dir.path().join(".claude-home");
    std::fs::create_dir_all(&claude_home).unwrap();
    let global_path = missing_global_path(dir.path());
    std::fs::write(
        cwd.join(".mcp.json"),
        format!(r#"{{"mcpServers":{servers_json}}}"#),
    )
    .unwrap();
    (dir, cwd, claude_home, global_path)
}

fn load(
    cwd: &Path,
    claude_home: &Path,
    global_path: &Path,
    policy: &BuiltinInjectionPolicy,
) -> Result<McpConfigFile, McpConfigError> {
    load_merged_config_full_with_paths(cwd, claude_home, global_path, policy)
        .map(|(config, _)| config)
}

/// 注册表声明为 direct 的原始工具名（与 overlay 的 `system_mcp_tools` 比对）。
fn declared_direct(instance: &str) -> Vec<String> {
    peri_acp_types::builtin_mcp::find(instance)
        .expect("实例必须在注册表内")
        .tools
        .iter()
        .filter(|tool| tool.direct)
        .map(|tool| tool.original_name.to_string())
        .collect()
}

/// 断言错误是「保留名接管」且只点名该实例。
fn assert_reserved_takeover(error: McpConfigError, expected: &str) {
    match &error {
        McpConfigError::ReservedBuiltinInstanceName { name } => assert_eq!(name, expected),
        other => panic!("应报 ReservedBuiltinInstanceName，实际: {other}"),
    }
    assert!(
        error.to_string().contains(expected),
        "错误文本应只含实例名: {error}"
    );
}

// ── 规则 1：缺失 → 插入完整 builtin 条目 ──────────────────────────────────────

#[test]
fn builtin_default_layer_inserts_complete_entries() {
    let (_dir, cwd, claude_home, global_path) = project_with_servers("{}");
    let merged = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();

    assert_eq!(
        merged.mcp_servers.len(),
        BUILTIN_MCP_INSTANCES.len(),
        "零用户配置时应恰有两个 builtin 条目，实际 keys: {:?}",
        merged.mcp_servers.keys().collect::<Vec<_>>()
    );
    for instance in BUILTIN_MCP_INSTANCES {
        let entry = merged
            .mcp_servers
            .get(instance.name)
            .unwrap_or_else(|| panic!("应注入 builtin 实例 {}", instance.name));
        assert_eq!(
            entry.source,
            Some(ConfigSource::Builtin {
                instance: instance.instance.to_string()
            }),
            "身份必须由 source 承载（不新增配置 key）"
        );
        assert_eq!(entry.system_mcp, Some(true), "builtin 实例是 system 依赖");
        assert_eq!(
            entry.system_mcp_tools.as_deref(),
            Some(declared_direct(instance.name).as_slice()),
            "system_mcp_tools 必须等于声明为 direct 的原始工具名集合（IF-D13/A5）"
        );
        // protocol_version 必须为 None，否则 Auto 不探测 server/discover（R2 前提）。
        assert_eq!(entry.protocol_version, None);
        assert_eq!(entry.command, None);
        assert_eq!(entry.url, None);
        assert_eq!(entry.disabled, None);
        assert_eq!(entry.system_mcp_timeout, None, "缺省 30s 由 readiness 决定");
        assert_eq!(entry.args, None);
        assert_eq!(entry.env, None);
        assert_eq!(entry.headers, None);
        assert!(entry.oauth.is_none());
        assert_eq!(entry.subscriptions, None);
    }
}

#[test]
fn builtin_overlay_is_skipped_entirely_when_policy_is_none() {
    let (_dir, cwd, claude_home, global_path) = project_with_servers("{}");
    let merged = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::none(),
    )
    .unwrap();
    assert!(
        merged.mcp_servers.is_empty(),
        "零注入策略下不得产生任何条目，实际 keys: {:?}",
        merged.mcp_servers.keys().collect::<Vec<_>>()
    );
}

// ── 规则 2：存在且无 command/url（A17）────────────────────────────────────────

#[test]
fn empty_user_entry_still_becomes_system_direct_instance() {
    // `{"web": {}}`：只填 source 是不够的——必须同时填 system_mcp 与
    // system_mcp_tools，否则会从 direct 静默降级为 deferred（A17）。
    let (_dir, cwd, claude_home, global_path) = project_with_servers(r#"{"web":{}}"#);
    let merged = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();

    let web = merged.mcp_servers.get("web").expect("web 条目应保留");
    assert_eq!(
        web.source,
        Some(ConfigSource::Builtin {
            instance: "web".to_string()
        })
    );
    assert_eq!(web.system_mcp, Some(true), "A17：可见性等价不得静默降级");
    assert_eq!(
        web.system_mcp_tools.as_deref(),
        Some(declared_direct("web").as_slice())
    );
    // 用户条目其余字段以用户值为准（不做字段级深合并）。
    assert_eq!(web.command, None);
    assert_eq!(web.disabled, None);
}

#[test]
fn user_fields_are_respected_without_deep_merge() {
    // 注：`system_mcp_timeout` / `system_mcp_tools` 必须与 `system_mcp: true` 同时声明
    // ——这是 IF-M1 的解析期契约（早于 step 6.5 overlay），overlay 不做字段级合并，
    // 因此不替用户补齐该组合。
    let (_dir, cwd, claude_home, global_path) = project_with_servers(
        r#"{"web":{"env":{"FAKE_TOKEN":"fake-value"},"system_mcp":true,"system_mcp_timeout":1500}}"#,
    );
    let merged = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    let web = merged.mcp_servers.get("web").expect("web 条目应保留");
    assert_eq!(
        web.env.as_ref().and_then(|env| env.get("FAKE_TOKEN")),
        Some(&"fake-value".to_string()),
        "用户字段以用户值为准"
    );
    assert_eq!(web.system_mcp_timeout, Some(1500), "用户超时不被覆盖");
    assert_eq!(web.system_mcp, Some(true));
}

#[test]
fn system_mcp_timeout_without_system_mcp_is_rejected_before_overlay() {
    // 解析期契约（IF-M1）先于 overlay：不构成 system 依赖的 timeout 声明是非法配置，
    // 不得被 overlay「顺手补齐」掩盖成可用实例。
    let (_dir, cwd, claude_home, global_path) =
        project_with_servers(r#"{"web":{"system_mcp_timeout":1500}}"#);
    let error = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .expect_err("timeout 缺少 system_mcp 必须在解析期被拒绝");
    assert!(
        matches!(error, McpConfigError::ParseError { .. }),
        "解析期错误，实际: {error}"
    );
}

#[test]
fn reserved_instance_without_transport_is_not_injected_when_unimplemented() {
    // 预留但未实现的名字（cron/lsp/workspace）写了空条目也不注入：否则会构造出
    // `TransportConfig::Builtin` 解析不到的条目（IF-D3 规则 2 尾句）。
    let (_dir, cwd, claude_home, global_path) = project_with_servers(r#"{"workspace":{}}"#);
    let merged = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    assert!(merged.mcp_servers.contains_key("workspace"));
    assert!(
        !matches!(
            &merged.mcp_servers["workspace"].source,
            Some(ConfigSource::Builtin { .. })
        ),
        "未实现实例不得被打上 builtin 身份"
    );
    assert_eq!(merged.mcp_servers["workspace"].system_mcp, None);
}

// ── 关闭语义：disabled 只填 source ───────────────────────────────────────────

#[test]
fn disabled_instance_keeps_builtin_transport_but_not_system_dependency() {
    let (_dir, cwd, claude_home, global_path) =
        project_with_servers(r#"{"web":{"disabled":true}}"#);
    let merged = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    let web = merged.mcp_servers.get("web").expect("web 条目应保留");
    assert_eq!(web.disabled, Some(true));
    assert_eq!(
        web.source,
        Some(ConfigSource::Builtin {
            instance: "web".to_string()
        }),
        "禁用仍保留 builtin 传输身份（注册为 Disabled，不是 Failed）"
    );
    assert_eq!(
        web.system_mcp, None,
        "disabled 条目不得构成 system 依赖（否则 readiness 会 fatal）"
    );
    assert_eq!(web.system_mcp_tools, None);
    // 另一个实例不受影响。
    let artifact = merged.mcp_servers.get("artifact").expect("artifact 应注入");
    assert_eq!(artifact.system_mcp, Some(true));
    assert_eq!(artifact.disabled, None);
}

// ── 规则 5（A18）：非法关闭片段必须加载期拒绝 ─────────────────────────────────

#[test]
fn disabled_with_system_mcp_is_rejected_at_load_time() {
    let (_dir, cwd, claude_home, global_path) =
        project_with_servers(r#"{"web":{"disabled":true,"system_mcp":true}}"#);
    let error = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .expect_err("`disabled + system_mcp` 必须在加载期被拒绝，不得落到 readiness 的 fatal");
    match &error {
        McpConfigError::BuiltinClosureFragmentInvalid { name } => assert_eq!(name, "web"),
        other => panic!("应报 BuiltinClosureFragmentInvalid，实际: {other}"),
    }
    // 错误文本只含实例名，不含路径 / env / 凭据。
    let text = error.to_string();
    assert!(text.contains("web"), "错误文本应含实例名: {text}");
    assert!(
        !text.contains(cwd.to_str().unwrap()) && !text.contains(global_path.to_str().unwrap()),
        "错误文本不得含路径: {text}"
    );
}

#[test]
fn illegal_closure_fragment_is_rejected_even_when_injection_is_off() {
    // 规则 5 是**加载期校验**，与注入策略无关：`PERI_MCP_BUILTIN=off` 只抑制注入，
    // 不解除保护（否则 off 会成为绕过非法配置的通道）。
    let (_dir, cwd, claude_home, global_path) =
        project_with_servers(r#"{"artifact":{"disabled":true,"system_mcp":true}}"#);
    let error = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::none(),
    )
    .expect_err("off 策略下非法关闭片段仍必须被拒绝");
    assert!(matches!(
        error,
        McpConfigError::BuiltinClosureFragmentInvalid { ref name } if name == "artifact"
    ));
}

// ── 规则 3（A3）：保留名不得被 command/url 接管 ───────────────────────────────

#[test]
fn reserved_instance_names_cannot_be_taken_over_by_command() {
    for name in ["web", "artifact", "cron", "lsp", "workspace"] {
        let (_dir, cwd, claude_home, global_path) =
            project_with_servers(&format!(r#"{{"{name}":{{"command":"fake-command"}}}}"#));
        let error = load(
            &cwd,
            &claude_home,
            &global_path,
            &BuiltinInjectionPolicy::all(),
        )
        .expect_err("保留名被 command 接管必须加载期 typed error");
        assert_reserved_takeover(error, name);
    }
}

#[test]
fn reserved_instance_names_cannot_be_taken_over_by_url() {
    let (_dir, cwd, claude_home, global_path) =
        project_with_servers(r#"{"web":{"url":"https://fake.invalid/mcp"}}"#);
    let error = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .expect_err("保留名被 url 接管必须加载期 typed error");
    // 不把 url 带进错误文本（认证信息可能出现在 query / userinfo 里）。
    assert!(!error.to_string().contains("fake.invalid"));
    assert_reserved_takeover(error, "web");
}

#[test]
fn reserved_name_takeover_is_rejected_even_when_injection_is_off() {
    let (_dir, cwd, claude_home, global_path) =
        project_with_servers(r#"{"artifact":{"command":"fake-command"}}"#);
    let error = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::none(),
    )
    .expect_err("off 策略下保留名接管仍必须被拒绝");
    assert_reserved_takeover(error, "artifact");
}

#[test]
fn non_reserved_instance_with_command_is_untouched() {
    // 非保留名照旧：overlay 不触碰（也不因它存在而失败）。
    let (_dir, cwd, claude_home, global_path) =
        project_with_servers(r#"{"external":{"command":"fake-command"}}"#);
    let merged = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    let external = merged
        .mcp_servers
        .get("external")
        .expect("外部 server 应保留");
    assert_eq!(external.command.as_deref(), Some("fake-command"));
    assert!(
        !matches!(&external.source, Some(ConfigSource::Builtin { .. })),
        "非 builtin 条目不得带 builtin 身份（来源仍是磁盘上的 project 层）"
    );
    assert_eq!(external.system_mcp, None);
}

// ── 写回隔离：builtin 默认条目永不被写盘 ─────────────────────────────────────

/// 磁盘上是否存在「只有 builtin 默认条目才有的字段组合」：
/// `system_mcp_tools` + 无 `command`/`url` + key 是已实现实例名。
fn builtin_only_entry_written(raw: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let servers = value
        .get("mcpServers")
        .or_else(|| value.get("config")?.get("mcpServers"))?
        .as_object()?;
    for instance in BUILTIN_MCP_INSTANCES {
        let Some(entry) = servers.get(instance.name) else {
            continue;
        };
        if entry.get("system_mcp_tools").is_some()
            && entry.get("command").is_none()
            && entry.get("url").is_none()
        {
            return Some(instance.name.to_string());
        }
    }
    None
}

#[test]
fn write_back_functions_never_persist_builtin_default_entries() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude_home = dir.path().join(".claude-home");
    std::fs::create_dir_all(&claude_home).unwrap();
    let global_path = missing_global_path(dir.path());
    let project_path = cwd.join(".mcp.json");
    std::fs::write(
        &project_path,
        r#"{"mcpServers":{"web":{"disabled":true},"plain":{"command":"fake-command"}}}"#,
    )
    .unwrap();
    let before = std::fs::read_to_string(&project_path).unwrap();

    // 1. 先按生产语义加载（注入只发生在内存）。
    let merged = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    assert!(merged.mcp_servers.contains_key("artifact"), "注入已发生");
    assert_eq!(
        std::fs::read_to_string(&project_path).unwrap(),
        before,
        "加载不得改动磁盘内容"
    );

    // 2. 两个写回函数只修改**已存在于磁盘文件中的条目**：用户条目正常写盘。
    set_server_disabled_with_paths(&cwd, &global_path, "plain", true).unwrap();
    let after_disable = std::fs::read_to_string(&project_path).unwrap();
    let written: serde_json::Value = serde_json::from_str(&after_disable).unwrap();
    assert_eq!(written["mcpServers"]["plain"]["disabled"], true);
    assert!(
        builtin_only_entry_written(&after_disable).is_none(),
        "写回不得把 builtin 默认条目写盘: {after_disable}"
    );

    remove_server_from_config_with_paths(&cwd, &global_path, "plain").unwrap();
    let after_remove = std::fs::read_to_string(&project_path).unwrap();
    assert!(
        builtin_only_entry_written(&after_remove).is_none(),
        "删除后不得出现 builtin 默认条目: {after_remove}"
    );
    assert!(
        !after_remove.contains("plain"),
        "删除应作用于磁盘上的用户条目: {after_remove}"
    );
    // 用户自己的关闭条目仍在（它不是 builtin 注入出来的）。
    let reloaded = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::none(),
    )
    .unwrap();
    assert_eq!(reloaded.mcp_servers.len(), 1);
    assert_eq!(reloaded.mcp_servers["web"].disabled, Some(true));
}

#[test]
fn write_back_is_noop_for_instance_that_only_exists_in_memory() {
    // builtin 名字只存在于内存时，写回函数找不到磁盘条目 ⇒ 不产生新条目。
    let (_dir, cwd, claude_home, global_path) = project_with_servers("{}");
    let project_path = cwd.join(".mcp.json");
    let before = std::fs::read_to_string(&project_path).unwrap();

    let merged = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    assert!(merged.mcp_servers.contains_key("web"));

    set_server_disabled_with_paths(&cwd, &global_path, "web", true).unwrap();
    remove_server_from_config_with_paths(&cwd, &global_path, "web").unwrap();
    assert_eq!(
        std::fs::read_to_string(&project_path).unwrap(),
        before,
        "内存里的 builtin 条目不得被写回函数落盘"
    );
}

// ── direct 一致性（A5/A17）：声明集合 == system_mcp_tools ────────────────────

#[test]
fn system_mcp_tools_equals_declared_direct_set_for_every_instance() {
    let (_dir, cwd, claude_home, global_path) = project_with_servers("{}");
    let merged = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    for instance in BUILTIN_MCP_INSTANCES {
        let tools = merged.mcp_servers[instance.name]
            .system_mcp_tools
            .clone()
            .expect("system_mcp_tools 必须存在");
        assert_eq!(
            tools,
            declared_direct(instance.name),
            "{} 的 system_mcp_tools 必须等于声明为 direct 的原始名集合（A5/A17）",
            instance.name
        );
        assert!(
            !tools.is_empty(),
            "{} 的 direct 集合为空 ⇒ 该实例注入后不产生任何 direct 工具",
            instance.name
        );
    }
}

// ── hash 无关性（IF-D3 / R9）：注入不改变既有 server 的 hash 去重结果 ─────────

/// 在 `claude_home` 下安装一个已启用插件（形状与 `config_test.rs` 的同名夹具一致：
/// 只有 step 4 的内容 hash 去重能改变插件 server 的存活结果）。
fn install_enabled_plugin(claude_home: &Path, name: &str, mcp_servers_json: &str) -> PathBuf {
    use crate::plugin::types::{InstallScope, InstalledPlugin, InstalledPlugins};

    let plugin_dir = claude_home
        .join("plugins")
        .join("cache")
        .join("mkt")
        .join(name)
        .join("1.0.0");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        format!(r#"{{"name":"{name}","version":"1.0.0","mcpServers":{mcp_servers_json}}}"#),
    )
    .unwrap();

    let installed = InstalledPlugins {
        version: 2,
        plugins: vec![InstalledPlugin {
            id: format!("{name}@mkt"),
            name: name.to_string(),
            version: "1.0.0".into(),
            marketplace: "mkt".into(),
            install_path: plugin_dir.clone(),
            scope: InstallScope::User,
            project_path: None,
            origin: crate::plugin::PluginOrigin::PeriInstalled,
        }],
    };
    std::fs::create_dir_all(claude_home.join("plugins")).unwrap();
    std::fs::write(
        claude_home.join("plugins").join("installed_plugins.json"),
        serde_json::to_string(&installed).unwrap(),
    )
    .unwrap();
    std::fs::write(
        claude_home.join("settings.json"),
        format!(r#"{{"enabledPlugins":["{name}@mkt"]}}"#),
    )
    .unwrap();
    plugin_dir
}

/// 除 builtin 实例名以外的 server key 集合（去重结果的观察量）。
fn non_builtin_keys(merged: &McpConfigFile) -> Vec<String> {
    let mut keys: Vec<String> = merged
        .mcp_servers
        .keys()
        .filter(|name| !BUILTIN_MCP_INSTANCES.iter().any(|i| i.name == *name))
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// step 6.5 注入**不改变**既有 server 的内容 hash 去重结果（IF-D3 / R9）。
///
/// 锁死的性质：同一份插件 + 手动配置在「注入」与「不注入」两次加载下，插件
/// server 的存活集合逐项相同（step 4 的 `manual_hashes` 只由 global / project
/// 的**用户**条目构成，builtin 条目在去重之后才进入 `merged`）。若注入位置被前移
/// 到 step 4 之前，builtin 条目会进入 `manual_hashes`，本断言即红。
#[test]
fn builtin_injection_does_not_change_existing_hash_dedup_result() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude_home = dir.path().join(".claude-home");
    std::fs::create_dir_all(&claude_home).unwrap();
    let global_path = missing_global_path(dir.path());

    // 插件声明两台：`plain-dup` 与手动配置内容完全相同（应被 hash 去重移除），
    // `sys-dup` 声明为 system_mcp（不得被去重，命名空间归属必须保留）。
    let plugin_dir = install_enabled_plugin(
        &claude_home,
        "p1",
        r#"{
            "sys-dup":{"command":"node","args":["s.js"],"system_mcp":true,"system_mcp_tools":["t"]},
            "plain-dup":{"command":"node","args":["p.js"]}
        }"#,
    );
    // 插件 server 在合并前会被注入 CLAUDE_PLUGIN_ROOT / CLAUDE_PLUGIN_DATA，
    // 手动条目写入同样的 env 才能得到相同的 hash（去重规则本身是唯一变量）。
    let plugin_env = serde_json::json!({
        "CLAUDE_PLUGIN_ROOT": plugin_dir.to_string_lossy(),
        "CLAUDE_PLUGIN_DATA": plugin_dir.join(".claude-plugin").join("data").to_string_lossy(),
    });
    let manual = serde_json::json!({"mcpServers": {
        "sys-manual": {
            "command":"node","args":["s.js"],
            "system_mcp":true,"system_mcp_tools":["t"],
            "env": plugin_env,
        },
        "plain-manual": {"command":"node","args":["p.js"],"env": plugin_env},
    }});
    std::fs::write(&global_path, serde_json::to_string(&manual).unwrap()).unwrap();

    let injected = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    let not_injected = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::none(),
    )
    .unwrap();

    // 去重确实发生（否则下面的等价断言会退化成同义反复）。
    assert!(
        !injected.mcp_servers.contains_key("plugin:p1:plain-dup"),
        "内容相同的插件 server 应被 hash 去重: {:?}",
        non_builtin_keys(&injected)
    );
    assert!(
        injected.mcp_servers.contains_key("plugin:p1:sys-dup"),
        "System 声明的插件 server 不参与去重: {:?}",
        non_builtin_keys(&injected)
    );

    // 两次加载的非 builtin 条目集合逐项相同 ⇒ 注入与去重结果无关。
    assert_eq!(
        non_builtin_keys(&injected),
        non_builtin_keys(&not_injected),
        "builtin 注入不得改变既有 server 的去重结果"
    );
    for instance in BUILTIN_MCP_INSTANCES {
        assert!(
            injected.mcp_servers.contains_key(instance.name)
                && !not_injected.mcp_servers.contains_key(instance.name),
            "两次加载的差异必须**只有** builtin 默认层（{}）",
            instance.name
        );
    }
}
