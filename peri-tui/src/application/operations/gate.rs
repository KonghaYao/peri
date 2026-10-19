//! 取值票与完成门：单面板单在途请求的过期拒收。

use tokio_util::sync::CancellationToken;

use super::scope::OperationScope;

/// 一次在途请求的取值票：作用域 + 代次 + 取消令牌。
///
/// 异步侧只需要持有它：等待时用它取消，响应回来时用它过完成门。票里不放业务
/// 载荷（插件动作、查询文本等），载荷留在各自面板，原语因此不认识业务。
#[derive(Clone)]
pub(crate) struct OperationTicket {
    scope: OperationScope,
    generation: u64,
    cancelled: CancellationToken,
}

impl OperationTicket {
    /// 发起请求时固定的作用域：请求参数、日志与响应返回后的复检都用它。
    pub(crate) fn scope(&self) -> &OperationScope {
        &self.scope
    }

    /// 请求代次。生产代码不需要读它（门自己比对），只有面板测试用来断言
    /// 「在途的到底是哪一次请求」，因此不进生产构建。
    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// 取消令牌：异步侧 `select!` 用。取消只结束本地等待。
    pub(crate) fn token(&self) -> &CancellationToken {
        &self.cancelled
    }

    /// 本地等待是否已取消（视图不再观察本次请求）。
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.is_cancelled()
    }
}

/// 单面板单在途请求的完成门。
///
/// 只保存「当前作用域 + 代次 + 在途票」三件事：不保存结果、不持有任务、不记历史，
/// 因此不会随运行时间增长。面板自己的结果状态（错误文案、搜索结果等）在通过
/// [`CompletionGate::accept`] 之后按各自响应形状落地。
#[derive(Default)]
pub(crate) struct CompletionGate {
    scope: OperationScope,
    generation: u64,
    pending: Option<OperationTicket>,
}

impl CompletionGate {
    /// 取票：取消前任在途请求、推进代次、采纳本次作用域并挂上新票。
    ///
    /// 采纳作用域是必要的：调用方可能在作用域刚变化时直接发起请求。
    pub(crate) fn begin(&mut self, scope: OperationScope) -> OperationTicket {
        self.cancel();
        self.generation = self
            .generation
            .checked_add(1)
            .expect("operation generation exhausted");
        let ticket = OperationTicket {
            scope: scope.clone(),
            generation: self.generation,
            cancelled: CancellationToken::new(),
        };
        self.scope = scope;
        self.pending = Some(ticket.clone());
        ticket
    }

    /// 过期拒收：已取消、作用域不符或不是在途代次 → false，且不改动任何状态。
    ///
    /// 调用方只有在拿到 true 之后才允许写入自己的结果状态，晚到的旧结果因此在
    /// 这里被丢弃。
    pub(crate) fn accept(&mut self, ticket: &OperationTicket) -> bool {
        if ticket.is_cancelled()
            || ticket.scope != self.scope
            || self.pending.as_ref().map(|pending| pending.generation) != Some(ticket.generation)
        {
            return false;
        }
        self.pending = None;
        true
    }

    /// 取消在途等待：此后该票不再被接受，调用方按「结果未确认」处理，不做
    /// 「业务已撤销」的断言。
    pub(crate) fn cancel(&mut self) {
        if let Some(ticket) = self.pending.take() {
            ticket.cancelled.cancel();
        }
    }

    /// 作用域切换：变化时取消在途请求并采纳新作用域，返回是否发生变化。
    ///
    /// 调用方会每帧用当前作用域反复调用（会话投影驱动），作用域相同时必须是无操作，
    /// 否则会把正在等待的请求误取消。
    pub(crate) fn reset_scope(&mut self, scope: OperationScope) -> bool {
        if self.scope == scope {
            return false;
        }
        self.cancel();
        self.scope = scope;
        true
    }

    /// 是否有在途请求。`Idle` / `Running` 由它推导，不另存副本。
    pub(crate) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// 当前作用域：日志与调用方自身的比较用。
    pub(crate) fn scope(&self) -> &OperationScope {
        &self.scope
    }

    /// 当前作用域是否等于给定作用域。
    pub(crate) fn scope_matches(&self, scope: &OperationScope) -> bool {
        self.scope == *scope
    }

    /// 在途票的代次：确认「在途的是哪一次请求」。同为测试用只读视图。
    #[cfg(test)]
    pub(crate) fn pending_generation(&self) -> Option<u64> {
        self.pending.as_ref().map(OperationTicket::generation)
    }
}

impl Drop for CompletionGate {
    /// 面板关闭、重开或作用域对象销毁时，旧票必须立即失效：取消在途等待，
    /// 晚到结果被 [`CompletionGate::accept`] 拒收。
    fn drop(&mut self) {
        self.cancel();
    }
}
