//! 部署能力平面（唯一事实源）：声明当前部署支持哪些宿主能力，并由这些能力
//! 派生 MCP 装配的策略与拒绝口径。
//!
//! 装配点（builtin overlay、实例上下文、stdio 准入、本地 workflow、市场与图片
//! 等本地进程能力）只查询本模块；`target_os` 条件编译仅出现在依赖 Emscripten
//! 不编译的类型 / package 的定义处（handler 枚举、实例输入类型、进程所有权、
//! 模块级替身）。新增平台差异时先在此登记能力位，再由装配点查询；不得在装配点
//! 各自写 `target_os` 业务判断。

/// 本部署是否托管进程内 builtin MCP 实例（web / artifact / cron / lsp / workspace）。
pub(crate) const IN_PROCESS_BUILTINS: bool = cfg!(not(target_os = "emscripten"));

/// 本部署是否支持 stdio 子进程传输。
pub(crate) const STDIO_TRANSPORT: bool = cfg!(not(target_os = "emscripten"));

/// 部署不支持 stdio 时的统一拒绝文案（进程检查与各准入点共用）。
pub(crate) const STDIO_UNAVAILABLE: &str = "stdio subprocesses are unavailable in this deployment";

/// 该 builtin 实例在本部署能否装配（注册表内 + 平台支持）。
pub(crate) fn builtin_instance_supported(instance: &str) -> bool {
    IN_PROCESS_BUILTINS && peri_acp_types::builtin_mcp::find(instance).is_some()
}

/// 从配置启用位派生默认层注入策略：部署不支持托管时恒为 `none`。
pub(crate) fn builtin_policy(
    configured_enabled: bool,
) -> crate::mcp::builtin::BuiltinInjectionPolicy {
    if IN_PROCESS_BUILTINS && configured_enabled {
        crate::mcp::builtin::BuiltinInjectionPolicy::all()
    } else {
        crate::mcp::builtin::BuiltinInjectionPolicy::none()
    }
}
