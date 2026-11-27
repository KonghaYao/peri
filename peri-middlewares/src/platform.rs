//! 部署能力平面（唯一事实源）：声明当前部署支持哪些宿主能力，并由这些能力
//! 派生 MCP 装配的策略与拒绝口径。
//!
//! 装配点（builtin overlay、实例上下文、stdio 准入、本地 workflow、市场与图片
//! 等本地进程能力）只查询本模块；新增平台差异时先在此登记能力位，再由装配点
//! 查询；不得在装配点各自写 `target_os` 业务判断。

/// 本部署是否托管进程内 builtin MCP 实例（web / artifact / cron / workspace）。
pub(crate) const IN_PROCESS_BUILTINS: bool = true;

/// 本部署是否支持 stdio 子进程传输。
pub(crate) const STDIO_TRANSPORT: bool = true;

/// 部署不支持 stdio 时的统一拒绝文案（进程检查与各准入点共用）。
pub(crate) const STDIO_UNAVAILABLE: &str = "stdio subprocesses are unavailable in this deployment";

/// 该 builtin 实例在本部署能否装配（注册表内 + 平台支持）。
pub(crate) fn builtin_instance_supported(instance: &str) -> bool {
    IN_PROCESS_BUILTINS && peri_acp_types::builtin_mcp::is_builtin_server_name(instance)
}

/// 从配置启用位派生默认层注入策略：部署不支持托管时恒为 `none`。
pub(crate) fn builtin_policy(
    configured_enabled: bool,
) -> crate::mcp::builtin::BuiltinInjectionPolicy {
    if IN_PROCESS_BUILTINS && configured_enabled {
        // Snapshot 只冻结 on/off；`legacy` 回滚值仍由唯一 env 解析点解释。
        crate::mcp::builtin::builtin_injection_policy_from_env()
    } else {
        crate::mcp::builtin::BuiltinInjectionPolicy::none()
    }
}

pub(crate) fn builtin_policy_for_mode(
    mode: peri_acp_types::builtin_mcp::BuiltinAssemblyMode,
) -> crate::mcp::builtin::BuiltinInjectionPolicy {
    if IN_PROCESS_BUILTINS {
        crate::mcp::builtin::BuiltinInjectionPolicy::from_assembly_mode(mode)
    } else {
        crate::mcp::builtin::BuiltinInjectionPolicy::none()
    }
}

/// bare 配置的 builtin 策略：仅保留 `workspace` 能力，并沿用部署装配模式。
///
/// `PERI_MCP_BUILTIN=off` 是部署级 kill switch，bare 路径也必须遵守；`legacy`
/// 回滚值在 bare 中只投影 `workspace`（bare 从不注入其他能力）。
pub(crate) fn builtin_workspace_policy(
    configured_enabled: bool,
) -> crate::mcp::builtin::BuiltinInjectionPolicy {
    let deploy_policy = builtin_policy(configured_enabled);
    builtin_workspace_policy_for_mode(deploy_policy)
}

pub(crate) fn builtin_workspace_policy_for_mode(
    deploy_policy: crate::mcp::builtin::BuiltinInjectionPolicy,
) -> crate::mcp::builtin::BuiltinInjectionPolicy {
    if deploy_policy.is_none() {
        deploy_policy
    } else if deploy_policy.is_legacy() {
        crate::mcp::builtin::BuiltinInjectionPolicy::legacy_workspace()
    } else {
        crate::mcp::builtin::BuiltinInjectionPolicy::capabilities(&["workspace"])
    }
}
