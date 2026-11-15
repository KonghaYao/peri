//! v5 单出口 builtin 能力声明源（**纯数据 + 纯查表**）。
//!
//! 这是 builtin 能力、工具、资源、宿主注入、执行绑定与旧键映射的**唯一手写声明
//! 源**。父模块的 v4 实例表、保留名表与关闭键表都由 [`BUILTIN_V5_CAPABILITIES`]
//! 派生，没有第二份字面量。
//!
//! 状态：默认注入仍走 v4 六实例兼容路径，router / 代理尚未接线；迁移期由父模块
//! 把能力表投影成实例视图。`legacy_instance` / `legacy_policy_key` / `legacy_name`
//! 只服务该兼容路径与旧历史归一，M4 清理时与 v4 路径一并删除。
//!
//! 三层命名（不要混淆）：
//! - **执行身份**：`(capability, original_name)`，由 `capability_tool` 唯一定位。
//!   授权、路由和日志必须使用它，不能拿模型面名字反推身份。
//! - **v5 模型面名**：`CapabilityTool::model_name`。direct 保持原名；deferred 为
//!   `mcp__peri__<original_name>`（`BUILTIN_V5_DEFERRED_TOOL_PREFIX`）。
//! - **迁移期旧名**：`CapabilityTool::legacy_name`（`mcp__<legacy_instance>__*` 或
//!   direct 原名）。只用于旧 transcript / allowlist / prompt 文案的展示与匹配
//!   归一，不是执行别名；`resolve_model_tool` 对旧 deferred 名字显式报错。
//!
//! 唯一性、模型面命名规则、旧键映射、资源归属、能力依赖和用户 server 保留名由
//! `validate_registry` / `validate_user_server_name` 以 typed error 判定；冻结表由
//! 契约测试（`v5_test`）在 CI 覆盖，生产装配当前不重复调用。
//! 校验实现位于 [`validation`] 子模块，名字归一与执行路由位于 [`naming`] 子模块；
//! 对外 API 仍经 `v5::*` re-export，调用点不需要感知模块拆分。

use crate::tools::ContextRetention;

/// v5 单出口 server 身份：每个会话环境只注入这一个 builtin server。
pub const BUILTIN_V5_SERVER_NAME: &str = "peri";

/// v5 deferred 工具的模型面名字前缀；direct 工具不进入该命名空间。
pub const BUILTIN_V5_DEFERRED_TOOL_PREFIX: &str = "mcp__peri__";

/// v5 保留 server 名：用户配置不得用同名 `command` / `url` 接管。
///
/// 迁移期六个旧实例名仍由父模块 [`super::is_reserved_instance_name`] 判定；
/// [`is_reserved_server_name`] 合并两组事实。
pub const BUILTIN_V5_RESERVED_SERVER_NAMES: &[&str] = &[BUILTIN_V5_SERVER_NAME];

/// 宿主注入的任务归属键；仅 `task_owner_scoped` 能力接收。
pub const TASK_OWNER_META_KEY: &str = "peri/taskOwner";

/// 宿主注入的 thread 归属键；恢复查询与任务过滤共用。
pub const THREAD_OWNER_META_KEY: &str = "peri/threadOwner";

/// 宿主消费者令牌键；仅 workflow 回填通道接收。
pub const WORKFLOW_CONSUMER_META_KEY: &str = "peri/workflowConsumer";

/// 能力执行绑定（v5 §4.3.5）：绑定是实现字段，不改变能力身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityExecutionBinding {
    /// 单出口在同一 Harness 进程内直接服务。
    InProcess,
    /// 单出口代理 Sandbox Actor 暴露的 HTTP MCP descriptor。
    SandboxHttpProxy,
}

/// 执行绑定与描述符标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityExecution {
    /// 绑定形态。
    pub binding: CapabilityExecutionBinding,
    /// 绑定描述符标识；真实地址 / Sandbox Actor 由装配期注入，不写死在这里。
    pub descriptor: &'static str,
}

/// 能力状态根语义（能力私有，不与其他能力共享）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityStateRoot {
    /// 无跨调用状态（web / artifact）。
    Stateless,
    /// 会话级宿主内存，随会话关闭释放（monitor）。
    SessionHostMemory,
    /// 宿主调度器状态（cron）。
    HostScheduler,
    /// 工作流运行时状态（workflow）。
    WorkflowRuntime,
    /// Sandbox 工作区根，由 Sandbox Actor 持有（workspace）。
    SandboxWorkspace,
}

/// 凭据与状态根声明。
///
/// 只声明能力自身消费的凭据键；Sandbox 上游连接凭据归 descriptor / Sandbox
/// Actor，不在此表复制。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityState {
    /// 能力私有凭据命名空间；`None` = 该能力不读取凭据。
    pub credential_namespace: Option<&'static str>,
    /// 命名空间下的环境变量 / 存储键（只声明键名，值不进入声明表）。
    pub credential_keys: &'static [&'static str],
    /// 状态根语义；能力私有，不得跨能力复用。
    pub state_root: CapabilityStateRoot,
}

/// 宿主注入行为（v5 §4.3.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityHostInjection {
    /// 是否按直接发起会话隔离后台任务与资源。
    pub task_owner_scoped: bool,
    /// 宿主注入的任务归属键（`_meta`）；仅 scoped 能力有值。
    pub task_owner_meta_key: Option<&'static str>,
    /// 宿主注入的 thread 归属键；仅 scoped 能力有值。
    pub thread_owner_meta_key: Option<&'static str>,
    /// 宿主注入的消费者令牌键；仅 workflow 回填通道有值。
    pub consumer_meta_key: Option<&'static str>,
}

/// 能力资源面声明（v5 §4.3.2）。
///
/// `uri_prefixes` 用于 router 判定资源归属；同一 URI 不得被多个能力声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityResourceSurface {
    /// 能力拥有的资源 URI 前缀。
    pub uri_prefixes: &'static [&'static str],
    /// 静态资源 URI。
    pub static_resources: &'static [&'static str],
    /// 资源模板 URI（含 `{...}` 占位符）：与实例 `list_resource_templates` 的
    /// wire 值逐字一致，由跨 crate 契约测试对齐。
    pub resource_templates: &'static [&'static str],
    /// 宿主默认订阅（`subscriptions.resources`）；空 = 无默认订阅。
    pub default_subscriptions: &'static [&'static str],
}

/// 单个工具声明（v5 模型面）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityTool {
    /// 原始工具名：执行身份的一半，`(capability, original_name)` 全局唯一。
    pub original_name: &'static str,
    /// 模型面名字：direct 使用原名；deferred 使用 `mcp__peri__<original_name>`。
    pub model_name: &'static str,
    /// 迁移期旧模型面名字（旧 transcript / allowlist / 展示归一）；不是执行别名。
    pub legacy_name: &'static str,
    /// 直连性声明：是否进入首个 LLM 请求的直连 tools 参数。
    pub direct: bool,
    /// 模型可见性：`false` 表示 host-only（不进桥接、ToolSearch 与 LLM tools）。
    pub model_visible: bool,
    /// 提示词层声明模板；`{{name}}` / `{{title}}` 的渲染仍归消费方。
    pub prompt_declaration: Option<&'static str>,
    /// 上下文保留策略。
    pub context_retention: ContextRetention,
    /// Complete 响应是否足以结算该操作。
    pub completes_on_response: bool,
}

/// 一项 builtin 能力声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinCapability {
    /// 能力名（也是 v5 的开关身份）。
    pub name: &'static str,
    /// 迁移期旧实例身份；确定性映射与旧名归一只索引这张表。
    pub legacy_instance: &'static str,
    /// 迁移期旧 middleware 关闭键；只用于识别旧键，不承担 v5 能力开关。
    pub legacy_policy_key: &'static str,
    /// 能力依赖：被依赖能力关闭时，本能力必须同步关闭或安全失败。
    pub dependencies: &'static [&'static str],
    /// 工具表（非空；原始名与模型面名全局唯一）。
    pub tools: &'static [CapabilityTool],
    /// 资源与默认订阅。
    pub resources: CapabilityResourceSurface,
    /// 宿主注入行为。
    pub host_injection: CapabilityHostInjection,
    /// 执行绑定。
    pub execution: CapabilityExecution,
    /// 凭据与状态根。
    pub state: CapabilityState,
}

mod naming;
mod validation;

pub use naming::{
    map_legacy_tool, normalize_legacy_tool_name, resolve_display_tool_name, resolve_model_tool,
    DisplayToolName, RoutedTool, ToolNameSource,
};
pub use validation::{validate_capabilities, validate_registry, CapabilityRegistryError};

/// 名字解析 / 旧名映射错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityNameError {
    /// 未知能力。
    #[error("unknown capability: {name}")]
    UnknownCapability { name: String },
    /// 能力内无此原始工具名。
    #[error("capability `{capability}` has no tool `{original_name}`")]
    UnknownTool {
        capability: String,
        original_name: String,
    },
    /// 未知迁移期旧实例。
    #[error("unknown legacy instance: {instance}")]
    UnknownLegacyInstance { instance: String },
    /// 未知迁移期旧关闭键。
    #[error("unknown legacy builtin policy key: {key}")]
    UnknownPolicyKey { key: String },
    /// 未知 v5 能力开关。
    #[error("unknown capability switch: {key}")]
    UnknownSwitch { key: String },
    /// 资源 URI 无归属能力。
    #[error("no capability owns resource URI: {uri}")]
    UnknownResourceUri { uri: String },
    /// 未知工具名字。
    #[error("unknown tool name: {name}")]
    UnknownToolName { name: String },
    /// 名字同时命中多个声明（注册表校验失败的前置防御）。
    #[error("ambiguous tool name `{name}` matches multiple declarations")]
    AmbiguousToolName { name: String },
    /// 旧 deferred 名字只用于展示 / 匹配，不能用于执行路由。
    #[error("legacy tool name `{name}` is display-only and cannot be used for execution routing")]
    LegacyNameNotExecutable { name: String },
    /// 传入的是 v5 模型面名字，不是迁移期旧名。
    #[error("tool name `{name}` is a v5 model name, not a migration-era legacy name")]
    NotLegacyToolName { name: String },
}

mod tools;

use tools::{
    ARTIFACT_CAPABILITY_TOOLS, CRON_CAPABILITY_TOOLS, MONITOR_CAPABILITY_TOOLS,
    WEB_CAPABILITY_TOOLS, WORKFLOW_CAPABILITY_TOOLS, WORKSPACE_CAPABILITY_TOOLS,
};

const NO_RESOURCES: CapabilityResourceSurface = CapabilityResourceSurface {
    uri_prefixes: &[],
    static_resources: &[],
    resource_templates: &[],
    default_subscriptions: &[],
};

const NO_HOST_INJECTION: CapabilityHostInjection = CapabilityHostInjection {
    task_owner_scoped: false,
    task_owner_meta_key: None,
    thread_owner_meta_key: None,
    consumer_meta_key: None,
};

#[cfg(test)]
const NO_STATE: CapabilityState = CapabilityState {
    credential_namespace: None,
    credential_keys: &[],
    state_root: CapabilityStateRoot::Stateless,
};

const fn in_process(descriptor: &'static str) -> CapabilityExecution {
    CapabilityExecution {
        binding: CapabilityExecutionBinding::InProcess,
        descriptor,
    }
}

/// v5 六项能力的唯一声明表。
///
/// 能力顺序即声明与关闭诊断顺序（测试锁定）；工具顺序即 `tools/list` 的声明顺序。
pub const BUILTIN_V5_CAPABILITIES: &[BuiltinCapability] = &[
    BuiltinCapability {
        name: "workspace",
        legacy_instance: "workspace",
        legacy_policy_key: "WorkspaceMiddleware",
        dependencies: &[],
        tools: WORKSPACE_CAPABILITY_TOOLS,
        resources: CapabilityResourceSurface {
            uri_prefixes: &[
                "workspace://",
                "skill://",
                "agent://",
                "peri-instruction://",
                "peri-meta://",
            ],
            static_resources: &[
                "workspace://git/ref",
                "peri-instruction://workspace/main",
                "peri-instruction://workspace/local",
                "peri-instruction://workspace/index",
            ],
            resource_templates: &[
                "skill://user/{name}/SKILL.md",
                "skill://user/{name}/{+path}",
                "skill://global/{name}/SKILL.md",
                "skill://global/{name}/{+path}",
                "skill://project/{name}/SKILL.md",
                "skill://project/{name}/{+path}",
                "skill://plugin/{plugin_name}/{name}/SKILL.md",
                "skill://plugin/{plugin_name}/{name}/{+path}",
                "skill://builtin/{name}/SKILL.md",
                "skill://builtin/{name}/{+path}",
                "agent://user/{name}/agent.md",
                "agent://global/{name}/agent.md",
                "agent://project/{name}/agent.md",
                "agent://builtin/{name}/agent.md",
                "agent://plugin/{plugin_name}/{name}/agent.md",
            ],
            default_subscriptions: &["workspace://git/ref"],
        },
        host_injection: CapabilityHostInjection {
            task_owner_scoped: true,
            task_owner_meta_key: Some(TASK_OWNER_META_KEY),
            thread_owner_meta_key: Some(THREAD_OWNER_META_KEY),
            consumer_meta_key: None,
        },
        execution: CapabilityExecution {
            binding: CapabilityExecutionBinding::SandboxHttpProxy,
            descriptor: "sandbox://workspace/mcp",
        },
        state: CapabilityState {
            credential_namespace: None,
            credential_keys: &[],
            state_root: CapabilityStateRoot::SandboxWorkspace,
        },
    },
    BuiltinCapability {
        name: "web",
        legacy_instance: "web",
        legacy_policy_key: "WebMiddleware",
        dependencies: &[],
        tools: WEB_CAPABILITY_TOOLS,
        resources: NO_RESOURCES,
        host_injection: NO_HOST_INJECTION,
        execution: in_process("host://web"),
        state: CapabilityState {
            credential_namespace: Some("web"),
            credential_keys: &[
                "PERI_WEB_SEARCH_URL",
                "PERI_WEB_SEARCH_TOKEN",
                "PERI_WEB_FETCH_URL",
                "PERI_WEB_FETCH_TOKEN",
            ],
            state_root: CapabilityStateRoot::Stateless,
        },
    },
    BuiltinCapability {
        name: "artifact",
        legacy_instance: "artifact",
        legacy_policy_key: "ArtifactMiddleware",
        dependencies: &[],
        tools: ARTIFACT_CAPABILITY_TOOLS,
        resources: NO_RESOURCES,
        host_injection: NO_HOST_INJECTION,
        execution: in_process("host://artifact"),
        state: CapabilityState {
            credential_namespace: Some("artifact"),
            credential_keys: &["PERI_ARTIFACTS_URL", "PERI_ARTIFACTS_TOKEN"],
            state_root: CapabilityStateRoot::Stateless,
        },
    },
    BuiltinCapability {
        name: "cron",
        legacy_instance: "cron",
        legacy_policy_key: "CronMiddleware",
        dependencies: &[],
        tools: CRON_CAPABILITY_TOOLS,
        resources: CapabilityResourceSurface {
            uri_prefixes: &["cron://"],
            static_resources: &["cron://jobs", "cron://triggers"],
            resource_templates: &[],
            default_subscriptions: &[],
        },
        host_injection: NO_HOST_INJECTION,
        execution: in_process("host://cron"),
        state: CapabilityState {
            credential_namespace: None,
            credential_keys: &[],
            state_root: CapabilityStateRoot::HostScheduler,
        },
    },
    BuiltinCapability {
        name: "monitor",
        legacy_instance: "monitor",
        legacy_policy_key: "MonitorMiddleware",
        dependencies: &[],
        tools: MONITOR_CAPABILITY_TOOLS,
        resources: NO_RESOURCES,
        host_injection: CapabilityHostInjection {
            task_owner_scoped: true,
            task_owner_meta_key: Some(TASK_OWNER_META_KEY),
            thread_owner_meta_key: Some(THREAD_OWNER_META_KEY),
            consumer_meta_key: None,
        },
        execution: in_process("host://monitor"),
        state: CapabilityState {
            credential_namespace: None,
            credential_keys: &[],
            state_root: CapabilityStateRoot::SessionHostMemory,
        },
    },
    BuiltinCapability {
        name: "workflow",
        legacy_instance: "workflow",
        legacy_policy_key: "WorkflowMiddleware",
        dependencies: &[],
        tools: WORKFLOW_CAPABILITY_TOOLS,
        resources: CapabilityResourceSurface {
            uri_prefixes: &["workflow://"],
            static_resources: &[
                "workflow://background-tasks",
                "workflow://runs",
                "workflow://tasks",
            ],
            resource_templates: &[
                "workflow://runs/{runId}",
                "workflow://runs/{runId}/events",
                "workflow://runs/{runId}/tasks",
            ],
            default_subscriptions: &[],
        },
        host_injection: CapabilityHostInjection {
            task_owner_scoped: false,
            task_owner_meta_key: None,
            thread_owner_meta_key: None,
            consumer_meta_key: Some(WORKFLOW_CONSUMER_META_KEY),
        },
        execution: in_process("host://workflow"),
        state: CapabilityState {
            credential_namespace: None,
            credential_keys: &[],
            state_root: CapabilityStateRoot::WorkflowRuntime,
        },
    },
];

/// 按能力名解析声明；未登记返回 `None`。
pub fn capability(name: &str) -> Option<&'static BuiltinCapability> {
    BUILTIN_V5_CAPABILITIES
        .iter()
        .find(|candidate| candidate.name == name)
}

/// 旧实例名 → 新能力；未知实例是显式错误，不做模糊匹配。
pub fn capability_of_legacy_instance(
    instance: &str,
) -> Result<&'static BuiltinCapability, CapabilityNameError> {
    BUILTIN_V5_CAPABILITIES
        .iter()
        .find(|candidate| candidate.legacy_instance == instance)
        .ok_or_else(|| CapabilityNameError::UnknownLegacyInstance {
            instance: instance.to_owned(),
        })
}

/// 迁移期旧 middleware 关闭键 → 能力。旧键只做确定性映射，不再承担开关语义。
pub fn capability_of_legacy_policy_key(
    key: &str,
) -> Result<&'static BuiltinCapability, CapabilityNameError> {
    BUILTIN_V5_CAPABILITIES
        .iter()
        .find(|candidate| candidate.legacy_policy_key == key)
        .ok_or_else(|| CapabilityNameError::UnknownPolicyKey {
            key: key.to_owned(),
        })
}

/// 能力开关：接受 v5 能力名或迁移期旧关闭键；两者映射到同一能力。
///
/// 这是运行时关闭集与配置投影的唯一入口；不得按 server 名或工具名反推能力。
pub fn capability_of_switch(
    switch: &str,
) -> Result<&'static BuiltinCapability, CapabilityNameError> {
    if let Some(capability) = capability(switch) {
        return Ok(capability);
    }
    capability_of_legacy_policy_key(switch).map_err(|_| CapabilityNameError::UnknownSwitch {
        key: switch.to_owned(),
    })
}

/// 全部迁移期旧关闭键（顺序即声明表顺序）。
pub fn legacy_policy_keys() -> impl Iterator<Item = &'static str> + Clone {
    BUILTIN_V5_CAPABILITIES
        .iter()
        .map(|capability| capability.legacy_policy_key)
}

/// 资源 URI → 归属能力（按已声明前缀唯一判定）。
pub fn capability_of_resource_uri(
    uri: &str,
) -> Result<&'static BuiltinCapability, CapabilityNameError> {
    BUILTIN_V5_CAPABILITIES
        .iter()
        .find(|capability| {
            capability
                .resources
                .uri_prefixes
                .iter()
                .any(|prefix| uri.starts_with(prefix))
        })
        .ok_or_else(|| CapabilityNameError::UnknownResourceUri {
            uri: uri.to_owned(),
        })
}

/// 执行身份 `(capability, original_name)` → 工具声明。
pub fn capability_tool(
    capability_name: &str,
    original_name: &str,
) -> Result<&'static CapabilityTool, CapabilityNameError> {
    let capability =
        capability(capability_name).ok_or_else(|| CapabilityNameError::UnknownCapability {
            name: capability_name.to_owned(),
        })?;
    capability
        .tools
        .iter()
        .find(|tool| tool.original_name == original_name)
        .ok_or_else(|| CapabilityNameError::UnknownTool {
            capability: capability_name.to_owned(),
            original_name: original_name.to_owned(),
        })
}

/// 用户自定义 MCP server 名校验：空名与 builtin 保留名都是加载期 typed error。
pub fn validate_user_server_name(name: &str) -> Result<(), CapabilityRegistryError> {
    if name.is_empty() {
        return Err(CapabilityRegistryError::EmptyServerName);
    }
    if is_reserved_server_name(name) {
        return Err(CapabilityRegistryError::ReservedUserServerName {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// server 名是否为 builtin 保留名（v5 出口 `peri` + 迁移期六实例名）。
pub fn is_reserved_server_name(name: &str) -> bool {
    BUILTIN_V5_RESERVED_SERVER_NAMES.contains(&name) || super::is_reserved_instance_name(name)
}

/// 大小写不敏感版本：ACP 注册名历史上按 ASCII 大小写不敏感拒绝。
pub fn is_reserved_server_name_ignore_ascii_case(name: &str) -> bool {
    BUILTIN_V5_RESERVED_SERVER_NAMES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(name))
        || BUILTIN_V5_CAPABILITIES
            .iter()
            .any(|capability| capability.legacy_instance.eq_ignore_ascii_case(name))
}

#[cfg(test)]
#[path = "v5_test.rs"]
mod tests;
