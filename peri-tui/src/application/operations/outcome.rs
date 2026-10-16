//! 完成词汇：成功、失败（携带原因）与结果未知。

/// 一次在途请求的终态词汇。
///
/// 只表达终态：`Idle` / `Running` 不是完成结果，由
/// [`CompletionGate::is_pending`](super::CompletionGate::is_pending) 推导，
/// 面板不保存第二份在途标志。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OperationOutcome {
    /// 服务端确认成功。
    Succeeded,
    /// 服务端明确拒绝，或响应形状不合法：携带可展示的原因。
    Failed { reason: String },
    /// 结果未确认：请求可能已在服务端生效，本视图拿不到结论。
    ///
    /// 它既不是「已撤销」也不是成功（方案 §6.2）：允许用户事后重新查询资源现状
    /// 判断，但不得由本模块自动重发非幂等操作。
    OutcomeUnknown { reason: OutcomeUnknownReason },
}

/// 结果未知的来源。用枚举而不是字符串约定表达，让三处来源（视图取消等待、
/// 观察者消失、等待超时）在调用点可区分、可统一展示。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutcomeUnknownReason {
    /// 视图停止等待（Esc 等）：远端可能仍在执行。
    WaitCancelled,
    /// 观察者消失：面板关闭或作用域被替换，已发出的变更失去归属。
    ObserverGone,
    /// 等待超时：客户端不再等待，服务端结果未确认。
    TimedOut,
}
