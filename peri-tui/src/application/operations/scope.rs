//! 操作作用域：请求归属于哪个会话边界。

/// 面板操作的作用域身份：活动会话 id + 前端重置代次。
///
/// 只承载「这次请求属于哪个会话边界」，不与客户端 lifecycle generation、渲染发布
/// 版本、请求代次或服务端 execution generation 合并成单一「万能代次」（方案 §6.1）；
/// 请求代次由 [`super::CompletionGate`] 单独维护。
///
/// 取值归调用方：原语不读 atoms，`kit` 侧的投影消费者（面板/装配层）把当前值
/// 构造进来，依赖方向保持 `kit → application`。
#[derive(Clone, Debug, Default)]
pub(crate) struct OperationScope {
    session_id: String,
    reset: u64,
}

impl OperationScope {
    pub(crate) fn new(session_id: impl Into<String>, reset: u64) -> Self {
        Self {
            session_id: session_id.into(),
            reset,
        }
    }

    /// 活动会话 id：请求参数与日志用。
    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// 手写比较而非 derive：作用域身份就是这两项，写出来即声明语义，也避免
/// 「字段只在派生实现里被读到」这种隐式使用。
impl PartialEq for OperationScope {
    fn eq(&self, other: &Self) -> bool {
        self.session_id == other.session_id && self.reset == other.reset
    }
}

impl Eq for OperationScope {}
