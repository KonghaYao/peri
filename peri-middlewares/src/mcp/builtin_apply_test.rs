//! builtin 默认配置层（step 6.5 overlay）的 crate 内验收：注入 / 覆盖 / 关闭 /
//! 保留名 typed error / 关闭片段反例 / 写回隔离 / hash 无关性 / direct 一致性。
//!
//! 口径来源（主 plan §3 IF-D3 + §8「默认层与覆盖语义」行）：
//! 1. 缺失 → 插入完整 builtin 条目（协议自动协商）；
//! 2. 存在且无 `command`/`url` → 填 `source`，`disabled != Some(true)` 时**同时**填
//!    `system_mcp` + `system_mcp_tools`（A17）；`disabled == Some(true)` 只填 `source`；
//! 3. 保留名（`web`/`artifact`/`cron`/`workspace`）被 `command`/`url` 接管 →
//!    加载期 typed error（A3）；
//! 5. 关闭片段形状（A18）：`{"web": {"disabled": true, "system_mcp": true}}` 必须被
//!    加载期拒绝；唯一合法写法是只写 `disabled: true`；
//! 6. direct 一致性：`system_mcp_tools` == 注册表声明为 direct 的原始工具名集合。
//!
//! 断言只落在可观察产物（加载结果、磁盘内容、typed error 变体）；不打印 env /
//! headers / 凭据，测试只使用注入的本地路径与假 server 名 / 假 token。

use std::path::{Path, PathBuf};

use peri_acp_types::builtin_mcp::{BUILTIN_MCP_INSTANCES, BUILTIN_RESERVED_INSTANCE_NAMES};
use peri_acp_types::plugin::ConfigSource;

use crate::mcp::builtin::{apply_builtin_overlay, BuiltinInjectionPolicy};
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
        assert_eq!(entry.command, None);
        assert_eq!(entry.url, None);
        assert_eq!(entry.disabled, None);
        assert_eq!(entry.system_mcp_timeout, None, "缺省 30s 由 readiness 决定");
        assert_eq!(entry.args, None);
        assert_eq!(entry.env, None);
        assert_eq!(entry.headers, None);
        assert!(entry.oauth.is_none());
        // 规则 7：默认订阅只挂在 `workspace`（git ref 资源）；其余实例不订阅。
        if instance.name == "workspace" {
            assert_eq!(
                entry.subscriptions,
                crate::mcp::builtin::default_subscriptions_for("workspace"),
                "规则 7：workspace 默认订阅 git ref 资源"
            );
        } else {
            assert_eq!(
                entry.subscriptions, None,
                "{} 不应有默认订阅",
                instance.name
            );
        }
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
    //
    // v4-part-3（IF-P3-01 / 主 plan §8 第 1 行）：同构断言覆盖四个已实现实例——
    // `{"cron": {}}` 与 `{"web": {}}` 走同一条「规则 2：存在且无
    // command/url」分支，差别只在 direct 集合（cron 为空集，零工具提升）。夹具按名字
    // 逐一构造，因此每个实例都必须各自证明一次。
    for instance in BUILTIN_MCP_INSTANCES {
        let name = instance.name;
        let (_dir, cwd, claude_home, global_path) =
            project_with_servers(&format!(r#"{{"{name}":{{}}}}"#));
        let merged = load(
            &cwd,
            &claude_home,
            &global_path,
            &BuiltinInjectionPolicy::all(),
        )
        .unwrap();

        let entry = merged
            .mcp_servers
            .get(name)
            .unwrap_or_else(|| panic!("{name} 条目应保留"));
        assert_eq!(
            entry.source,
            Some(ConfigSource::Builtin {
                instance: instance.instance.to_string()
            }),
            "{name}：空用户条目必须被填上 builtin 身份"
        );
        assert_eq!(
            entry.system_mcp,
            Some(true),
            "{name}：A17：可见性等价不得静默降级"
        );
        assert_eq!(
            entry.system_mcp_tools.as_deref(),
            Some(declared_direct(name).as_slice()),
            "{name}：system_mcp_tools 必须等于声明为 direct 的原始名集合"
        );
        // 用户条目其余字段以用户值为准（不做字段级深合并）。
        assert_eq!(entry.command, None);
        assert_eq!(entry.disabled, None);
    }
}

/// 规则 7（Git Watch 下沉，D-5）：`workspace` 的默认订阅随默认条目注入；
/// 其余实例不注入订阅。
#[test]
fn workspace_default_subscription_is_injected_by_overlay() {
    let mut map: std::collections::HashMap<String, peri_acp_types::plugin::McpServerConfig> =
        std::collections::HashMap::new();
    apply_builtin_overlay(&mut map, &BuiltinInjectionPolicy::all()).expect("空配置必须可注入");

    let workspace = map.get("workspace").expect("workspace 条目必须存在");
    assert_eq!(
        workspace.subscriptions,
        crate::mcp::builtin::default_subscriptions_for("workspace"),
        "workspace 必须带默认订阅（git ref 资源）"
    );
    for instance in BUILTIN_MCP_INSTANCES {
        if instance.name == "workspace" {
            continue;
        }
        assert!(
            map.get(instance.name)
                .expect("实例条目必须存在")
                .subscriptions
                .is_none(),
            "{} 不应有默认订阅",
            instance.name
        );
    }
}

/// 规则 7：用户显式写 `subscriptions`（含空配置）优先于默认注入——空配置在连接期经
/// `!is_empty()` 过滤后等价于「不订阅」，是用户关闭 git 提醒的细粒度开关。
#[test]
fn user_subscriptions_win_over_default_injection() {
    // 显式空配置：保持为空（不回落默认订阅）。
    let (_dir, cwd, claude_home, global_path) =
        project_with_servers(r#"{"workspace":{"subscriptions":{}}}"#);
    let merged = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    let workspace = merged
        .mcp_servers
        .get("workspace")
        .expect("workspace 条目应保留");
    let subscriptions = workspace
        .subscriptions
        .as_ref()
        .expect("空配置也是显式配置（Some(empty)），不得被替换为默认订阅");
    assert!(
        subscriptions.is_empty(),
        "空覆盖必须保持为空（连接期视为不订阅）"
    );

    // 显式非空配置：逐字保留用户值。
    let (_dir, cwd, claude_home, global_path) =
        project_with_servers(r#"{"workspace":{"subscriptions":{"resources":["custom://uri"]}}}"#);
    let merged = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    let workspace = merged
        .mcp_servers
        .get("workspace")
        .expect("workspace 条目应保留");
    assert_eq!(
        workspace
            .subscriptions
            .as_ref()
            .map(|s| s.resources.clone()),
        Some(vec!["custom://uri".to_string()]),
        "用户显式订阅优先于默认注入"
    );
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
fn every_reserved_instance_is_implemented_and_injected_with_builtin_identity() {
    // 前提变更（wave 3）：本用例原名
    // `reserved_instance_without_transport_is_not_injected_when_unimplemented`，断言
    // 「**预留但未实现**的名字（cron/workspace）写了空条目也不注入」。wave 3 落地后
    // `BUILTIN_RESERVED_INSTANCE_NAMES` 的名字**全部已实现**，注册表里不存在
    // 「预留但未实现」这一类别，原断言的论域是空集——它对当前事实不再有可失败性。
    //
    // 因此改为**确定性的强断言**（不删除断言、不放宽为「只要不 panic」）：保留名表**逐项**
    // 都在注册表内，且用户只写空条目 `{}` 时 overlay 必须补齐**完整 builtin 身份**。
    // 与同批 `builtin_test.rs::overlay_injects_every_reserved_instance` 同口径，区别在观测面：
    // 那条走 `apply_builtin_overlay` 的内存 map，本条走**完整加载链**（磁盘 `.mcp.json`
    // → 合并 → overlay），因此能同时锁住「空条目经文件加载后仍不被降级为 deferred」。
    // 每个保留名都写成空条目；`project_with_servers` 收的是 `mcpServers` 对象**本身**
    // （含自身的花括号），故这里补外层 `{...}`。
    let fragment = format!(
        "{{{}}}",
        BUILTIN_RESERVED_INSTANCE_NAMES
            .iter()
            .map(|name| format!(r#""{name}":{{}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let (_dir, cwd, claude_home, global_path) = project_with_servers(&fragment);
    let merged = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();

    assert_eq!(
        merged.mcp_servers.len(),
        BUILTIN_RESERVED_INSTANCE_NAMES.len(),
        "保留名逐项已实现 ⇒ 产物键集必须恰等于保留名表（不多不少）: {:?}",
        merged.mcp_servers.keys().collect::<Vec<_>>()
    );
    for name in BUILTIN_RESERVED_INSTANCE_NAMES {
        assert!(
            peri_acp_types::builtin_mcp::find(name).is_some(),
            "{name} 必须已在注册表内（wave 3 后不存在「预留但未实现」的名字）"
        );
        let entry = merged
            .mcp_servers
            .get(*name)
            .unwrap_or_else(|| panic!("{name} 已实现 ⇒ 写了空条目的保留名必须被注入"));
        assert_eq!(
            entry.source,
            Some(ConfigSource::Builtin {
                instance: (*name).to_string()
            }),
            "{name} 必须带 builtin 传输身份"
        );
        assert_eq!(entry.system_mcp, Some(true), "{name} 必须是 system 依赖");
        assert_eq!(
            entry.system_mcp_tools,
            Some(declared_direct(name)),
            "{name} 的 system_mcp_tools 必须等于注册表声明的 direct 集合（A17：\
             否则 `{{\"{name}\": {{}}}}` 会从 direct 静默降级为 deferred）"
        );
        assert_eq!(entry.disabled, None, "{name} 未声明 disabled ⇒ 默认启用");
        entry.validate().expect("注入条目必须通过契约校验");
    }
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
    // M7 后拒绝位置前移到**共享** `McpServerConfig::validate`（覆盖 builtin/普通/
    // global/project/plugin 与配置更新），因此这里报的是该规则的 typed 错误；
    // overlay 的 `BuiltinClosureFragmentInvalid` 保留为同一规则的实例级第二道防线。
    match &error {
        McpConfigError::InvalidServer {
            server_name,
            source: peri_acp_types::plugin::McpServerConfigValidationError::DisabledWithSystemMcp,
        } => assert_eq!(server_name, "web"),
        McpConfigError::BuiltinClosureFragmentInvalid { name } => assert_eq!(name, "web"),
        // 全局 settings 在解析期即拒绝（共享 validate），错误文本仍是固定规则正文。
        McpConfigError::ParseError { source, .. } => assert!(source
            .to_string()
            .contains("disabled = true cannot be combined with system_mcp = true")),
        other => panic!("应报 disabled+system_mcp 的加载期拒绝，实际: {other}"),
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
    let rejected = match &error {
        McpConfigError::InvalidServer {
            server_name,
            source: peri_acp_types::plugin::McpServerConfigValidationError::DisabledWithSystemMcp,
        } => server_name.clone(),
        McpConfigError::BuiltinClosureFragmentInvalid { name } => name.clone(),
        McpConfigError::ParseError { source, .. } => {
            assert!(
                source
                    .to_string()
                    .contains("disabled = true cannot be combined with system_mcp = true"),
                "非法关闭片段必须在加载期被拒绝，实际: {error}"
            );
            "artifact".to_string()
        }
        other => panic!("非法关闭片段必须在加载期被拒绝，实际: {other}"),
    };
    assert_eq!(rejected, "artifact", "实际错误: {error}");
}

// ── 规则 3（A3）：保留名不得被 command/url 接管 ───────────────────────────────

#[test]
fn reserved_instance_names_cannot_be_taken_over_by_command() {
    for name in ["web", "artifact", "cron", "workspace"] {
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
fn remote_workspace_requires_global_or_host_selection_and_no_tool_manifest() {
    let (_dir, cwd, claude_home, global_path) =
        project_with_servers(r#"{"workspace":{"url":"https://fake.invalid/mcp"}}"#);
    let error = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .expect_err("a project cannot select a trusted remote workspace");
    assert!(matches!(
        error,
        McpConfigError::UntrustedWorkspaceSource { .. }
    ));
    assert!(!error.to_string().contains("fake.invalid"));

    std::fs::remove_file(cwd.join(".mcp.json")).unwrap();
    std::fs::write(
        &global_path,
        r#"{"mcpServers":{"workspace":{"url":"https://fake.invalid/mcp"}}}"#,
    )
    .unwrap();
    let merged = load(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    let workspace = &merged.mcp_servers["workspace"];
    assert!(matches!(
        workspace.source,
        Some(ConfigSource::WorkspaceRemote)
    ));
    assert_eq!(workspace.system_mcp, Some(true));
    assert!(workspace.system_mcp_tools.is_none());
    assert!(workspace.subscriptions.is_none());

    let closed = std::collections::BTreeSet::from(["workspace".to_string()]);
    assert!(!crate::mcp::builtin::is_closed_source(
        "workspace",
        workspace.source.as_ref(),
        &closed,
    ));
}

#[test]
fn reserved_instance_names_cannot_be_taken_over_by_url() {
    // 其余三个保留名仍拒绝 URL 接管；workspace 的项目 URL 拒绝与全局 URL
    // 允许由 remote_workspace_requires_global_or_host_selection_and_no_tool_manifest 覆盖。
    for name in ["web", "artifact", "cron"] {
        let (_dir, cwd, claude_home, global_path) = project_with_servers(&format!(
            r#"{{"{name}":{{"url":"https://fake.invalid/mcp"}}}}"#
        ));
        let error = load(
            &cwd,
            &claude_home,
            &global_path,
            &BuiltinInjectionPolicy::all(),
        )
        .expect_err("保留名被 url 接管必须加载期 typed error");
        // 不把 url 带进错误文本（认证信息可能出现在 query / userinfo 里）。
        assert!(
            !error.to_string().contains("fake.invalid"),
            "{name}：错误文本不得回显 url，实际: {error}"
        );
        assert_reserved_takeover(error, name);
    }
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
        // `system_mcp_tools == []` 是 wave 2 的**合法终态**（cron 零 direct、零工具
        // 提升，A4/A5），语义是「保留 1R readiness 约束，但不做必需工具校验」，不是
        // 「实例注入后无人可用」。因此这里按实例分别断言非空 / 空，而不是一律非空。
        match instance.name {
            "web" | "artifact" => assert!(
                !tools.is_empty(),
                "{} 的 direct 集合为空 ⇒ 该实例注入后不产生任何 direct 工具",
                instance.name
            ),
            "cron" => assert!(
                tools.is_empty(),
                "{} 的 direct 集合必须为空（A4：cron 工具一律 deferred）",
                instance.name
            ),
            // workspace（wave 3 落地，AW3-03：7 项一律 direct，且 7 项都来自 `BaseTool`
            // 实现，无 deferred 项）。期望值**从注册表派生**，不在此处第二份硬编码名单：
            // 上方已把 `tools` 与注册表派生的 direct 集合逐项比对，这里锁住的是「全部声明
            // 工具都是 direct」这条独立事实（若将来有 workspace 工具被降级为 deferred，
            // 本条会红，而 `is_direct()` 的运行时面也会同步变化）。
            "workspace" => {
                assert!(
                    !tools.is_empty(),
                    "workspace 的 direct 集合为空 ⇒ 该实例注入后不产生任何 direct 工具"
                );
                assert_eq!(
                    tools.len(),
                    instance.tools.iter().filter(|tool| tool.direct).count(),
                    "workspace 的 direct 集合必须覆盖其全部已声明工具（AW3-03：7 项）"
                );
                assert!(
                    instance.tools.iter().all(|tool| tool.direct),
                    "workspace 不得有 deferred 工具（AW3-03：7 项一律 direct）"
                );
            }
            other => panic!("新增实例 {other} 必须在本测试内显式登记 direct 语义"),
        }
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
