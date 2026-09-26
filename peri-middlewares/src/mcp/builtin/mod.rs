//! Builtin MCP 行为层（**唯一行为实现**）：effective name 计算、默认层注入与覆盖、
//! 注入策略、关闭集、直连性判定、声明模板查询。
//!
//! 数据侧（实例 / 原始工具名 / effective name 字面量 / 逐工具 `direct` /
//! `prompt_declaration` / 保留名表 / 归一表）在 `peri_acp_types::builtin_mcp`。
//! 本模块不得复制 `sanitize_name_component` 的名字规则：`effective_tool_name` 调用
//! `tool_bridge` 的同一规则与模板（规则一份实现、字面量一份声明，两者由
//! `mcp::builtin::tests` 的字面量测试对齐）。
//!
//! 注入点唯一（A1）：`mcp/config.rs` loader 的 step 6.5；注入策略是**显式参数**，
//! 本模块（含 `apply_builtin_overlay`）**禁止**读 env——env 只在
//! [`builtin_injection_policy_from_env`] 读一次，由 `load_merged_config_full` 调用。
//!
//! W2 起本模块追加 `mod web;` / `mod artifact;`（两个真实 `ServerHandler`，owner I-01）。

// 生产接线归 E-03 / I-02 / I-03 / S-01 / S-02（W2/W3）：W1 先落地冻结接口与 crate 内
// 测试覆盖；接线完成后应删除本属性（避免留下长期死代码豁免）。
#![allow(dead_code)]

// runtime：builtin 同进程链路（duplex + `rmcp::serve_server`）、server task 归属与
// 有界关闭（owner E-03，W2）。与两个 handler 一样是「行为」子模块，由本模块统一挂载。
pub(crate) mod runtime;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use peri_acp_types::builtin_mcp::{
    find, is_reserved_instance_name, BuiltinMcpInstance, BUILTIN_MCP_INSTANCES,
};
use peri_acp_types::plugin::{ConfigSource, McpServerConfig};
use thiserror::Error;

use crate::mcp::tool_bridge::effective_mcp_tool_name;

/// `PERI_MCP_BUILTIN` 环境变量名（紧急闸门，A2：显式运维开关，不是静默降级）。
pub(crate) const BUILTIN_INJECTION_ENV: &str = "PERI_MCP_BUILTIN";

/// 默认层注入策略（显式参数；`_with_paths` 与 overlay 都不读 env）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BuiltinInjectionPolicy {
    /// 允许注入的已实现实例名（注册表子集）。
    enabled: Vec<&'static str>,
}

impl BuiltinInjectionPolicy {
    /// 注入全部已实现实例（`PERI_MCP_BUILTIN` 缺省 / 未知值）。
    pub(crate) fn all() -> Self {
        Self {
            enabled: BUILTIN_MCP_INSTANCES
                .iter()
                .map(|instance| instance.name)
                .collect(),
        }
    }

    /// 零注入（`PERI_MCP_BUILTIN=off` / `0`）。
    ///
    /// **语义**：off 的退回态**没有** Web/Artifact 能力（middleware 提供面已删除，
    /// 不存在「回退到旧实现」这条路径），这是显式运维开关。与 `disabled: true` 的
    /// 区别：后者仍保留该实例的 builtin 配置（注册为 `Disabled`），另一实例与其它
    /// capability 不受影响。
    pub(crate) fn none() -> Self {
        Self {
            enabled: Vec::new(),
        }
    }

    /// 从 env 解析：`off` / `0` → [`Self::none`]；缺失 / 其它值 → [`Self::all`]
    /// （未知值 warn + `all`，不引入第三种未知状态）。
    pub(crate) fn from_env() -> Self {
        match std::env::var(BUILTIN_INJECTION_ENV) {
            Ok(raw) => {
                let value = raw.trim();
                if value.eq_ignore_ascii_case("off") || value == "0" {
                    Self::none()
                } else {
                    tracing::warn!(
                        env = BUILTIN_INJECTION_ENV,
                        "未知取值，按缺省语义注入全部 builtin 实例"
                    );
                    Self::all()
                }
            }
            Err(_) => Self::all(),
        }
    }

    /// 已启用的实例名（注册表顺序）。
    pub(crate) fn enabled_instances(&self) -> &[&'static str] {
        &self.enabled
    }

    /// 该实例是否在策略内（`false` ⇒ overlay 不注入、不填 `source`）。
    pub(crate) fn enables(&self, instance: &str) -> bool {
        self.enabled.contains(&instance)
    }
}

/// 从 env 构造注入策略（**唯一**读 env 的入口，只在 `load_merged_config_full` 调用一次）。
pub(crate) fn builtin_injection_policy_from_env() -> BuiltinInjectionPolicy {
    BuiltinInjectionPolicy::from_env()
}

/// 默认层 overlay 的加载期错误（typed；错误文本只含实例名，不含路径 / env / 凭据）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum BuiltinOverlayError {
    /// 保留实例名被用户配置用 `command` / `url` 接管（A3）。
    ///
    /// 必须加载期拒绝：parity 与关闭语义按名字反查，外部同名 server 一旦接管该
    /// 名字会继承「按原始名判定」的审批结果，静默移除 `mcp__*` 审批门。
    #[error("builtin 保留实例名不得被 command/url 接管: {name}")]
    ReservedBuiltinInstanceName { name: String },
    /// 禁用与 system 声明同时出现（A18）：非法关闭片段。
    ///
    /// 唯一合法的用户关闭写法是 `{"web": {"disabled": true}}`；`disabled + system_mcp`
    /// 组合今天会走到 readiness 的 `Err(SystemReadinessError::Disabled)` fatal，阻断
    /// **所有** session，因此必须在加载期拒绝。
    #[error("builtin 实例的关闭片段非法（disabled 与 system_mcp 不得同时声明）: {name}")]
    DisabledWithSystemMcp { name: String },
}

/// effective name 计算：`mcp__{sanitize(instance)}__{sanitize(original_tool)}`。
///
/// 名字规则与模板都来自 `tool_bridge`（**不得**在本模块复制）；只有已实现实例
/// 且工具在声明表内时可计算，其余返回 `None`。
pub(crate) fn effective_tool_name(instance: &str, original_tool: &str) -> Option<String> {
    let instance = find(instance)?;
    instance
        .tools
        .iter()
        .any(|tool| tool.original_name == original_tool)
        .then(|| effective_mcp_tool_name(instance.name, original_tool))
}

/// 实例全部工具的 effective name 集合（关闭集与断言共用）。
pub(crate) fn effective_tool_names(instance: &str) -> BTreeSet<String> {
    find(instance)
        .map(|instance| {
            instance
                .tools
                .iter()
                .map(|tool| effective_mcp_tool_name(instance.name, tool.original_name))
                .collect()
        })
        .unwrap_or_default()
}

/// 实例声明为 direct 的**原始工具名**（IF-D13 / IF-D9 的一致性的唯一来源）。
///
/// 未实现 / 未知名返回 `None`；返回值与注册表逐工具 `direct` 派生一致
/// （派生，不是第二份声明）。
pub(crate) fn declared_direct_tools(instance: &str) -> Option<&'static [&'static str]> {
    let instance = find(instance)?;
    direct_tools_table()
        .get(instance.name)
        .map(|names| &**names)
}

/// `(server_name, original_tool)` 是否为已实现实例声明为 direct 的工具。
pub(crate) fn is_declared_direct(server_name: &str, original_tool: &str) -> bool {
    find(server_name).is_some_and(|instance| {
        instance
            .tools
            .iter()
            .any(|tool| tool.direct && tool.original_name == original_tool)
    })
}

/// builtin 工具的提示词层声明模板（A9）。
///
/// 只暴露模板本身；渲染（`{{name}}` → effective name）仍由
/// `tool_search/declaration.rs` 的既有规则负责。
pub(crate) fn builtin_prompt_declaration(
    server_name: &str,
    original_tool: &str,
) -> Option<&'static str> {
    find(server_name)?
        .tools
        .iter()
        .find(|tool| tool.original_name == original_tool)?
        .prompt_declaration
}

/// IF-D10 的唯一判定入口：`policy_key ∈ disabled_middlewares` 的实例进入关闭集。
///
/// 下游四个过滤面（direct 注入 / deferred bridge / parent_tools / workflow agent
/// 工具面）都只调用它，不得各自硬编码实例名或 `mcp__web__` 前缀。
pub(crate) fn closed_instances(disabled_middlewares: &HashSet<String>) -> BTreeSet<String> {
    BUILTIN_MCP_INSTANCES
        .iter()
        .filter(|instance| disabled_middlewares.contains(instance.policy_key))
        .map(|instance| instance.name.to_string())
        .collect()
}

/// 该 server name 是否在关闭集内。
pub(crate) fn is_closed(server_name: &str, closed: &BTreeSet<String>) -> bool {
    closed.contains(server_name)
}

/// 默认配置层 overlay（A1：loader step 6.5 的唯一注入点）。
///
/// 六条冻结规则（IF-D3）：
/// 1. 实例名缺失 → 插入完整 builtin 条目（`protocol_version = None` 必须，否则 Auto
///    不探测 `server/discover`）；
/// 2. 实例名存在且未声明 `command`/`url` → 填 `source`，`disabled != Some(true)` 时
///    **同时**填 `system_mcp = Some(true)` 与 `system_mcp_tools = Some(声明 direct 集合)`
///    （A17：否则 `{"web": {}}` 会从 direct 静默降级为 deferred）；`disabled == Some(true)`
///    时只填 `source`（保持 `Disabled` 注册语义，不构成 system 依赖）。
///    本规则**仅对已实现实例生效**——预留未实现名不写 `command`/`url` 时不注入任何条目；
/// 3. 实例名存在且声明了 `command` 或 `url` 且是**保留实例名** → 加载期 typed error；
///    非保留名照旧不受影响；
/// 4. 结果必须通过 loader step 7 的 `validate_config`（本函数只保证自身产出合法）；
/// 5. 关闭片段形状（A18）：`disabled + system_mcp: true` 组合必须在加载期拒绝；
/// 6. direct 一致性：`system_mcp_tools` 恒等于声明为 direct 的原始工具名集合。
///
/// 错误语义：**先检查、后注入**——任一类非法输入都在任何写入之前返回（合法输入
/// 不受影响；非法输入不产生部分注入）。
///
/// 校验与注入的分工：规则 3（保留名接管）与规则 5（非法关闭片段）是**加载期校验**，
/// 与策略无关——`PERI_MCP_BUILTIN=off` 只抑制注入，不解除这两项保护（IF-D15 归一表是
/// 静态字面量，且 `disabled + system_mcp` 在任何策略下都会走到 readiness 的 fatal）；
/// 规则 1 / 2 是**注入面**，受策略控制。
pub(crate) fn apply_builtin_overlay(
    servers: &mut HashMap<String, McpServerConfig>,
    policy: &BuiltinInjectionPolicy,
) -> Result<(), BuiltinOverlayError> {
    // 规则 3：保留名接管检查（含后续波次预留名），首个错误按名字排序稳定返回。
    if let Some(err) = reserved_name_takeover(servers) {
        return Err(err);
    }
    // 规则 5（A18）：禁用与 system 声明同时出现——必须在加载期拒绝，不得落到
    // readiness 的 `Err(SystemReadinessError::Disabled)` fatal。
    if let Some(err) = disabled_with_system_mcp(servers) {
        return Err(err);
    }
    // 规则 1 / 2：只处理策略启用且**已实现**的实例。
    for instance in BUILTIN_MCP_INSTANCES {
        if !policy.enables(instance.name) {
            continue;
        }
        let Some(existing) = servers.get(instance.name) else {
            servers.insert(instance.name.to_string(), builtin_default_entry(instance));
            continue;
        };
        let disabled = existing.disabled;
        let entry = servers
            .get_mut(instance.name)
            .expect("上面刚按同一 key 取过不可变引用");
        entry.source = Some(ConfigSource::Builtin {
            instance: instance.instance.to_string(),
        });
        if disabled != Some(true) {
            entry.system_mcp = Some(true);
            entry.system_mcp_tools = Some(direct_tool_name_strings(instance));
        }
    }
    Ok(())
}

/// 保留名被 `command`/`url` 接管的首个错误（按名字排序，稳定可复核）。
fn reserved_name_takeover(
    servers: &HashMap<String, McpServerConfig>,
) -> Option<BuiltinOverlayError> {
    let mut names: Vec<&String> = servers
        .iter()
        .filter(|(name, config)| {
            is_reserved_instance_name(name) && (config.command.is_some() || config.url.is_some())
        })
        .map(|(name, _)| name)
        .collect();
    names.sort();
    names
        .first()
        .map(|name| BuiltinOverlayError::ReservedBuiltinInstanceName {
            name: (*name).clone(),
        })
}

/// A18 检查：已实现实例不得同时声明 `disabled = true` 与 `system_mcp = true`
/// （按注册表顺序取首个，确定性）。
///
/// 范围限定为 overlay 管理的**已实现**实例：预留未实现名的同形条目不由本批次管理，
/// 保持迁移前语义（既有 `validate_config` / readiness 路径）。
fn disabled_with_system_mcp(
    servers: &HashMap<String, McpServerConfig>,
) -> Option<BuiltinOverlayError> {
    BUILTIN_MCP_INSTANCES
        .iter()
        .find(|instance| {
            servers.get(instance.name).is_some_and(|config| {
                config.disabled == Some(true) && config.system_mcp == Some(true)
            })
        })
        .map(|instance| BuiltinOverlayError::DisabledWithSystemMcp {
            name: instance.name.to_string(),
        })
}

/// 规则 1 的完整 builtin 条目。
fn builtin_default_entry(instance: &BuiltinMcpInstance) -> McpServerConfig {
    McpServerConfig {
        command: None,
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        // 必须为 None：显式版本会跳过 Auto 的 `server/discover` 探测。
        protocol_version: None,
        subscriptions: None,
        system_mcp: Some(true),
        system_mcp_tools: Some(direct_tool_name_strings(instance)),
        system_mcp_timeout: None,
        source: Some(ConfigSource::Builtin {
            instance: instance.instance.to_string(),
        }),
    }
}

/// 实例声明为 direct 的原始工具名（`Vec<String>` 形态，供配置写入）。
fn direct_tool_name_strings(instance: &BuiltinMcpInstance) -> Vec<String> {
    instance
        .tools
        .iter()
        .filter(|tool| tool.direct)
        .map(|tool| tool.original_name.to_string())
        .collect()
}

/// 逐工具 `direct` 派生出的原始名切片（`declared_direct_tools` 的 `'static` 形态）。
///
/// 派生而非第二份声明：只遍历注册表。表是进程级静态（构建一次），切片由静态表持有，
/// 因此可安全返回 `'static`。
fn direct_tools_table() -> &'static HashMap<&'static str, Box<[&'static str]>> {
    static TABLE: LazyLock<HashMap<&'static str, Box<[&'static str]>>> = LazyLock::new(|| {
        BUILTIN_MCP_INSTANCES
            .iter()
            .map(|instance| {
                let names: Box<[&'static str]> = instance
                    .tools
                    .iter()
                    .filter(|tool| tool.direct)
                    .map(|tool| tool.original_name)
                    .collect();
                (instance.name, names)
            })
            .collect()
    });
    &TABLE
}

#[cfg(test)]
#[path = "builtin_test.rs"]
mod tests;

// 两个真实 `ServerHandler`（W2，owner I-01）：`web`（WebSearch / WebFetch）与
// `artifact`（artifact）。handler 工厂 `web::builtin_server_handler` 是 `runtime`
// （E-03 的 `spawn_builtin_transport`）的唯一入口。
mod artifact;
mod web;
