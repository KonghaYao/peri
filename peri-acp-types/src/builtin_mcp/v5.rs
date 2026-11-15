//! v5 单出口 builtin 能力声明源（**纯数据 + 纯查表**）。
//!
//! 状态：本模块只提供 v5 声明、映射与加载期校验；默认注入仍走父模块的 v4
//! `BUILTIN_MCP_INSTANCES`，router / 代理尚未接线。迁移期两张表并存，测试锁定
//! 「同一能力、同一工具集合、同一 direct / prompt 语义」。
//!
//! 三层命名（不要混淆）：
//! - **执行身份**：`(capability, original_name)`，由 `capability_tool` 唯一定位。
//!   授权、路由和日志必须使用它，不能拿模型面名字反推身份。
//! - **v5 模型面名**：`CapabilityTool::model_name`。direct 保持原名；deferred 为
//!   `mcp__peri__<original_name>`（`BUILTIN_V5_DEFERRED_TOOL_PREFIX`）。
//! - **迁移期旧名**：父模块 v4 表的 `effective_name`（`mcp__<instance>__*` 或
//!   direct 原名）。只用于旧 transcript / allowlist / prompt 文案的展示与匹配
//!   归一，不是执行别名；`resolve_model_tool` 对旧 deferred 名字显式报错。
//!
//! 唯一性、模型面命名规则和用户 server 保留名由 `validate_registry` /
//! `validate_user_server_name` 在加载期校验（typed error）。本模块不改变默认
//! 行为，也不替代父模块的 `effective_name` 计算或归一表。

use crate::tools::ContextRetention;

use super::{BuiltinMcpInstance, BuiltinMcpTool, BUILTIN_MCP_INSTANCES};

/// v5 单出口 server 身份：每个会话环境只注入这一个 builtin server。
pub const BUILTIN_V5_SERVER_NAME: &str = "peri";

/// v5 deferred 工具的模型面名字前缀；direct 工具不进入该命名空间。
pub const BUILTIN_V5_DEFERRED_TOOL_PREFIX: &str = "mcp__peri__";

/// v5 保留 server 名：用户配置不得用同名 `command` / `url` 接管。
///
/// 迁移期六个旧实例名仍由父模块 [`super::is_reserved_instance_name`] 判定；
/// [`is_reserved_server_name`] 合并两组事实。
pub const BUILTIN_V5_RESERVED_SERVER_NAMES: &[&str] = &[BUILTIN_V5_SERVER_NAME];

/// 六项能力的冻结名字（顺序即声明表顺序）。
pub const BUILTIN_V5_CAPABILITY_NAMES: &[&str] = &[
    "workspace",
    "web",
    "artifact",
    "cron",
    "monitor",
    "workflow",
];

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
    /// 资源模板 URI（含 `{...}` 占位符）。
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
    /// 能力名（也是 v5 前的实例名）。
    pub name: &'static str,
    /// 迁移期旧实例身份；确定性映射与旧名归一只索引这张表。
    pub legacy_instance: &'static str,
    /// 迁移期旧 middleware 关闭键；只用于识别旧键，不承担 v5 能力开关。
    pub legacy_policy_key: &'static str,
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

/// 执行路由结果：模型面名字解析出的唯一 `(capability, tool)`。
///
/// 这是 router 的入口类型；旧 `mcp__<instance>__*` 名字不会构造出本类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutedTool {
    /// 能力名（执行身份的一半）。
    pub capability: &'static str,
    /// 工具声明（原始名是执行身份的另一半）。
    pub tool: &'static CapabilityTool,
}

/// 展示 / 匹配归一结果。**不是执行身份**：执行路由必须使用
/// [`capability_tool`] 或 [`resolve_model_tool`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayToolName {
    /// 能力名。
    pub capability: &'static str,
    /// 原始工具名。
    pub original_name: &'static str,
    /// v5 模型面名字（展示用）。
    pub model_name: &'static str,
    /// 迁移期旧 effective name。
    pub legacy_effective_name: &'static str,
    /// 命中的名字来源。
    pub source: ToolNameSource,
}

/// 展示归一命中的名字来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolNameSource {
    /// 只命中 v5 模型面名字（deferred `mcp__peri__*`）。
    ModelName,
    /// 只命中迁移期旧名字（旧 deferred `mcp__<instance>__*`）。
    LegacyName,
    /// direct 工具的新旧名字相同。
    SharedDirectName,
}

/// 注册表加载期错误（重复 / 冲突 / 保留名）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityRegistryError {
    /// 能力名不得为空。
    #[error("capability name is empty")]
    EmptyCapabilityName,
    /// 能力名重复。
    #[error("duplicate capability name: {name}")]
    DuplicateCapabilityName { name: String },
    /// 每项能力至少声明一个工具。
    #[error("capability `{capability}` has no tools")]
    EmptyCapabilityTools { capability: String },
    /// 工具原始名不得为空。
    #[error("capability `{capability}` has an empty tool name")]
    EmptyToolName { capability: String },
    /// 迁移期旧实例身份重复会让旧名映射不确定。
    #[error("duplicate legacy instance `{instance}` in capabilities `{first}` and `{second}`")]
    DuplicateLegacyInstance {
        instance: String,
        first: String,
        second: String,
    },
    /// 原始工具名必须全局唯一（v5 §4.1.6）。
    #[error(
        "duplicate original tool name `{original_name}` in capabilities `{first}` and `{second}`"
    )]
    DuplicateOriginalToolName {
        original_name: String,
        first: String,
        second: String,
    },
    /// 模型面名必须全局唯一。
    #[error("duplicate model tool name `{model_name}` in capabilities `{first}` and `{second}`")]
    DuplicateModelToolName {
        model_name: String,
        first: String,
        second: String,
    },
    /// direct / deferred 的模型面命名规则不一致。
    #[error(
        "capability `{capability}` tool `{original_name}` has model name `{model_name}`; expected `{expected}`"
    )]
    InvalidModelToolName {
        capability: String,
        original_name: String,
        model_name: String,
        expected: String,
    },
    /// 原始名占用 MCP 命名空间会与外部 server 名字冲突。
    #[error(
        "capability `{capability}` tool `{original_name}` uses reserved MCP namespace `mcp__`"
    )]
    ReservedToolName {
        capability: String,
        original_name: String,
    },
    /// 用户 server 名不得为空。
    #[error("user-defined MCP server name is empty")]
    EmptyServerName,
    /// 用户 server 名不得占用 builtin 保留名。
    #[error("user-defined MCP server name `{name}` is reserved for builtin `peri` / migration instances")]
    ReservedUserServerName { name: String },
}

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
/// 顺序与 [`BUILTIN_V5_CAPABILITY_NAMES`] 一致；工具顺序即 `tools/list` 的声明
/// 顺序（与 v4 表逐项一致，由测试锁定）。
pub const BUILTIN_V5_CAPABILITIES: &[BuiltinCapability] = &[
    BuiltinCapability {
        name: "workspace",
        legacy_instance: "workspace",
        legacy_policy_key: "WorkspaceMiddleware",
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
                "skill://{scope}/{name}/{path}",
                "skill://plugin/{plugin}/{name}/{path}",
                "agent://{scope}/{name}/agent.md",
                "peri-meta://workspace/{section_id}",
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

/// 模型面名字 → 执行路由身份。**只接受 v5 模型面名字**。
///
/// 旧 deferred 名字（`mcp__<instance>__*`）返回
/// [`CapabilityNameError::LegacyNameNotExecutable`]，显式拒绝执行别名。
pub fn resolve_model_tool(model_name: &str) -> Result<RoutedTool, CapabilityNameError> {
    let mut matched = None;
    for capability in BUILTIN_V5_CAPABILITIES {
        for tool in capability.tools {
            if tool.model_name != model_name {
                continue;
            }
            if matched.is_some() {
                return Err(CapabilityNameError::AmbiguousToolName {
                    name: model_name.to_owned(),
                });
            }
            matched = Some(RoutedTool {
                capability: capability.name,
                tool,
            });
        }
    }
    if let Some(routed) = matched {
        return Ok(routed);
    }
    if !legacy_tool_matches(model_name).is_empty() {
        return Err(CapabilityNameError::LegacyNameNotExecutable {
            name: model_name.to_owned(),
        });
    }
    Err(CapabilityNameError::UnknownToolName {
        name: model_name.to_owned(),
    })
}

/// 展示 / 匹配归一：接受 v5 模型面名字或迁移期旧名。
///
/// 结果类型 [`DisplayToolName`] 不得用于执行路由；未知名字返回显式错误。
pub fn resolve_display_tool_name(name: &str) -> Result<DisplayToolName, CapabilityNameError> {
    let mut model_matches = Vec::new();
    for capability in BUILTIN_V5_CAPABILITIES {
        for tool in capability.tools {
            if tool.model_name == name {
                model_matches.push((capability, tool));
            }
        }
    }
    if model_matches.len() > 1 {
        return Err(CapabilityNameError::AmbiguousToolName {
            name: name.to_owned(),
        });
    }
    if let Some((capability, tool)) = model_matches.first().copied() {
        let legacy_effective_name =
            legacy_effective_name_for(capability.legacy_instance, tool.original_name)?;
        return Ok(DisplayToolName {
            capability: capability.name,
            original_name: tool.original_name,
            model_name: tool.model_name,
            legacy_effective_name,
            source: if tool.direct {
                ToolNameSource::SharedDirectName
            } else {
                ToolNameSource::ModelName
            },
        });
    }

    let legacy_matches = legacy_tool_matches(name);
    if legacy_matches.len() > 1 {
        return Err(CapabilityNameError::AmbiguousToolName {
            name: name.to_owned(),
        });
    }
    if let Some((legacy_instance, legacy_tool)) = legacy_matches.first().copied() {
        let capability = capability_of_legacy_instance(legacy_instance.instance)?;
        let tool = capability
            .tools
            .iter()
            .find(|tool| tool.original_name == legacy_tool.original_name)
            .ok_or_else(|| CapabilityNameError::UnknownTool {
                capability: capability.name.to_owned(),
                original_name: legacy_tool.original_name.to_owned(),
            })?;
        return Ok(DisplayToolName {
            capability: capability.name,
            original_name: tool.original_name,
            model_name: tool.model_name,
            legacy_effective_name: legacy_tool.effective_name,
            // direct 原名的新旧拼写相同才会走到 SharedDirect；历史
            // `mcp__<instance>__*` 拼接（含 direct 工具）一律是 Legacy。
            source: if name == tool.model_name {
                ToolNameSource::SharedDirectName
            } else {
                ToolNameSource::LegacyName
            },
        });
    }

    Err(CapabilityNameError::UnknownToolName {
        name: name.to_owned(),
    })
}

/// 迁移期旧名字 → 展示归一结果；v5-only 名字显式返回
/// [`CapabilityNameError::NotLegacyToolName`]。
pub fn normalize_legacy_tool_name(name: &str) -> Result<DisplayToolName, CapabilityNameError> {
    let resolved = resolve_display_tool_name(name)?;
    if resolved.source == ToolNameSource::ModelName {
        return Err(CapabilityNameError::NotLegacyToolName {
            name: name.to_owned(),
        });
    }
    Ok(resolved)
}

/// 旧 `(instance, original_name)` → 新能力 + 新模型面名字；映射失败显式报错。
pub fn map_legacy_tool(
    instance: &str,
    original_name: &str,
) -> Result<DisplayToolName, CapabilityNameError> {
    let capability = capability_of_legacy_instance(instance)?;
    let tool = capability
        .tools
        .iter()
        .find(|tool| tool.original_name == original_name)
        .ok_or_else(|| CapabilityNameError::UnknownTool {
            capability: capability.name.to_owned(),
            original_name: original_name.to_owned(),
        })?;
    let legacy_effective_name = legacy_effective_name_for(instance, original_name)?;
    Ok(DisplayToolName {
        capability: capability.name,
        original_name: tool.original_name,
        model_name: tool.model_name,
        legacy_effective_name,
        source: if tool.direct {
            ToolNameSource::SharedDirectName
        } else {
            ToolNameSource::LegacyName
        },
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

/// 校验冻结的 v5 声明表。
pub fn validate_registry() -> Result<(), CapabilityRegistryError> {
    validate_capabilities(BUILTIN_V5_CAPABILITIES)
}

/// 校验任意能力表：重复 / 冲突 / 模型面命名规则 / 保留命名空间。
///
/// 独立成泛型入口是为了让测试能用合成表覆盖失败分支；生产加载期调用
/// [`validate_registry`]。
pub fn validate_capabilities(
    capabilities: &[BuiltinCapability],
) -> Result<(), CapabilityRegistryError> {
    use std::collections::HashMap;

    let mut capability_names = HashMap::new();
    let mut legacy_instances = HashMap::new();
    let mut original_names: HashMap<&str, &str> = HashMap::new();
    let mut model_names: HashMap<&str, &str> = HashMap::new();
    let mut reserved_names = Vec::new();

    for capability in capabilities {
        if capability.name.is_empty() {
            return Err(CapabilityRegistryError::EmptyCapabilityName);
        }
        if capability_names.insert(capability.name, ()).is_some() {
            return Err(CapabilityRegistryError::DuplicateCapabilityName {
                name: capability.name.to_owned(),
            });
        }
        if let Some(first) = legacy_instances.insert(capability.legacy_instance, capability.name) {
            return Err(CapabilityRegistryError::DuplicateLegacyInstance {
                instance: capability.legacy_instance.to_owned(),
                first: first.to_owned(),
                second: capability.name.to_owned(),
            });
        }
        if capability.tools.is_empty() {
            return Err(CapabilityRegistryError::EmptyCapabilityTools {
                capability: capability.name.to_owned(),
            });
        }
        for tool in capability.tools {
            if tool.original_name.is_empty() {
                return Err(CapabilityRegistryError::EmptyToolName {
                    capability: capability.name.to_owned(),
                });
            }
            if tool.original_name.starts_with("mcp__") {
                // 保留命名空间检查放在全局唯一性之后：先让「direct 原名抢占
                // deferred 模型面」这类跨能力冲突得到确定的 DuplicateModelToolName。
                reserved_names.push((capability.name, tool.original_name));
            }
            let expected = expected_model_name(tool);
            if tool.model_name != expected {
                return Err(CapabilityRegistryError::InvalidModelToolName {
                    capability: capability.name.to_owned(),
                    original_name: tool.original_name.to_owned(),
                    model_name: tool.model_name.to_owned(),
                    expected,
                });
            }
            if let Some(first) = original_names.get(tool.original_name) {
                return Err(CapabilityRegistryError::DuplicateOriginalToolName {
                    original_name: tool.original_name.to_owned(),
                    first: (*first).to_owned(),
                    second: capability.name.to_owned(),
                });
            }
            original_names.insert(tool.original_name, capability.name);
            if let Some(first) = model_names.get(tool.model_name) {
                return Err(CapabilityRegistryError::DuplicateModelToolName {
                    model_name: tool.model_name.to_owned(),
                    first: (*first).to_owned(),
                    second: capability.name.to_owned(),
                });
            }
            model_names.insert(tool.model_name, capability.name);
        }
    }
    if let Some((capability, original_name)) = reserved_names.first().copied() {
        return Err(CapabilityRegistryError::ReservedToolName {
            capability: capability.to_owned(),
            original_name: original_name.to_owned(),
        });
    }
    Ok(())
}

fn expected_model_name(tool: &CapabilityTool) -> String {
    if tool.direct {
        tool.original_name.to_owned()
    } else {
        format!("{BUILTIN_V5_DEFERRED_TOOL_PREFIX}{}", tool.original_name)
    }
}

fn legacy_effective_name_for(
    instance: &str,
    original_name: &str,
) -> Result<&'static str, CapabilityNameError> {
    let legacy =
        super::find(instance).ok_or_else(|| CapabilityNameError::UnknownLegacyInstance {
            instance: instance.to_owned(),
        })?;
    legacy
        .tools
        .iter()
        .find(|tool| tool.original_name == original_name)
        .map(|tool| tool.effective_name)
        .ok_or_else(|| CapabilityNameError::UnknownTool {
            capability: instance.to_owned(),
            original_name: original_name.to_owned(),
        })
}

fn legacy_tool_matches(name: &str) -> Vec<(&'static BuiltinMcpInstance, &'static BuiltinMcpTool)> {
    BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| {
            instance
                .tools
                .iter()
                .filter(move |tool| {
                    // 与旧归一入口同口径：既接受冻结的 effective name，也接受
                    // 历史 `mcp__<instance>__<original>` 拼接（direct 工具亦可有
                    // 这种旧拼写）。只做精确归一，不折叠大小写。
                    let namespaced = name
                        .strip_prefix("mcp__")
                        .and_then(|rest| rest.strip_prefix(instance.name))
                        .and_then(|rest| rest.strip_prefix("__"));
                    tool.effective_name == name || namespaced == Some(tool.original_name)
                })
                .map(move |tool| (instance, tool))
        })
        .collect()
}

#[cfg(test)]
#[path = "v5_test.rs"]
mod tests;
