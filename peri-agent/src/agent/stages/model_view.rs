//! 单轮执行准备范围内的模型视图复用。
//!
//! Reason 的准备阶段会对同一份 transcript 反复构建模型视图：准备检查、`before_model`
//! 之后复检、正式请求快照；每轮迭代的 Compact 阶段在判断是否 Skip 前还会先 refresh。
//! 这些检查点之间输入通常完全未变，却各付一次全量投影与正文复制的成本。本模块只在
//! **内容修订确实未变**时复用同一份已渲染视图（`Arc` 共享，不复制正文）。
//!
//! 调用方划定作用域：每轮迭代入口 [`ModelViewScope::reset`]（覆盖本轮 Compact 检查
//! 与随后的 Reason 准备），取到正式请求快照后 [`ModelViewScope::release`]。缓存因此
//! 不跨迭代常驻（Compact 会用 `std::mem::take` 整体换出 transcript，跨执行复用等于
//! 拿旧实例的内容猜新实例）。
//!
//! 生命周期一律按「同一时刻至多一份整份正文存活」约束，三条约束分别由三处实现：
//! 1. 键不匹配时先丢旧项、再渲染新视图（[`ModelViewScope::view`]）；
//! 2. 只有确实要构造 Full 摘要视图时释放（`compact_v2::FullSummaryNotice`，由
//!    `stages::compact` 传入）；无变化路径不释放，缓存继续复用；
//! 3. 退出准备作用域统一释放，hook 错误与提前取消等返回路径都不留正文
//!    （[`ModelViewScope::release_guard`]）。

#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::agent::compact_v2::projection::ProviderCapabilities;
use crate::error::AgentResult;
use crate::messages::BaseMessage;
use crate::session::transcript::{MessageTranscript, TranscriptRevision};

/// 已渲染的模型视图缓存项。
struct CachedModelView {
    revision: TranscriptRevision,
    capabilities: ProviderCapabilities,
    /// 保护集合/诊断版本；与 transcript 修订共同决定是否可复用。
    protected_version: u64,
    messages: Arc<Vec<BaseMessage>>,
    /// 测试观察点：本项持有一份整份正文，随缓存项一起释放（只需析构副作用）。
    #[cfg(test)]
    _live: LiveViewHandle,
}

/// 执行准备范围内的模型视图复用槽。
///
/// ## 复用边界（刻意保守）
///
/// 1. 只有 transcript 实例身份、修订号与 provider 能力三者全部相同才复用。修订号按
///    内容变更自增（见 `MessageTranscript::view_revision`），实例身份区分「换出后
///    重建的同名 transcript」——重建后修订号从头开始，只比修订号会误复用旧内容。
/// 2. 只缓存已渲染的模型视图，**不缓存 token 估算**：工具目录可能在检查点之间变化
///    （Reason 会整体替换 working tools），估算必须按当时的工具集重算。
/// 3. 视图不跨 Act 常驻：正式请求快照持有自己的 `Arc` 之后调用
///    [`ModelViewScope::release`]，避免整份正文副本在工具执行期间继续被引用。
/// 4. 只服务模型受众（经 `render_persisted_llm_view`），与 TUI 快照
///    （`MessageTranscript::visible_snapshot`）不共用缓存。
#[derive(Default)]
pub struct ModelViewScope {
    cached: Mutex<Option<CachedModelView>>,
    /// 实际重新渲染次数（测试观察点：证明修订未变时没有重建视图）。
    #[cfg(test)]
    renders: std::sync::atomic::AtomicUsize,
    /// 槽内视图正文句柄数与其峰值（测试观察点：重建路径上同时存活的正文至多一份）。
    #[cfg(test)]
    liveness: Arc<ViewLiveness>,
}

/// 测试观察点：同一时刻由本槽持有的视图正文句柄数与峰值。
///
/// 峰值直接对应「重建期间两份整份正文同时存活」：旧项未先释放就写入新项时，
/// 写入瞬间两份正文同时存在，峰值升到 2。
#[cfg(test)]
#[derive(Default)]
struct ViewLiveness {
    live: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
}

/// 与一份槽内视图正文同生共死的计数句柄。
#[cfg(test)]
struct LiveViewHandle(Arc<ViewLiveness>);

#[cfg(test)]
impl LiveViewHandle {
    fn acquire(liveness: &Arc<ViewLiveness>) -> Self {
        let live = liveness.live.fetch_add(1, Ordering::Relaxed) + 1;
        liveness.peak.fetch_max(live, Ordering::Relaxed);
        Self(Arc::clone(liveness))
    }
}

#[cfg(test)]
impl Drop for LiveViewHandle {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::Relaxed);
    }
}

impl ModelViewScope {
    /// 取给定 transcript 当前修订的模型视图；修订与能力均未变时复用已渲染结果。
    ///
    /// 调用方必须已经持有 transcript 读锁（`&MessageTranscript`），这样「读修订 →
    /// 命中判断 → 渲染 → 写回」在同一个锁周期内完成：不会把渲染期间被写入的新内容
    /// 当作旧修订缓存，也不需要跨 `await` 持锁。
    ///
    /// 键不匹配时旧视图已经不可复用，必须先把它移出槽再渲染：否则旧正文会一直存活
    /// 到新视图渲染结束，重建期间两份整份正文同时驻留。
    #[cfg(test)]
    pub(crate) fn view(
        &self,
        transcript: &MessageTranscript,
        capabilities: &ProviderCapabilities,
    ) -> AgentResult<Arc<Vec<BaseMessage>>> {
        self.view_inner(
            transcript,
            capabilities,
            &crate::session::async_result_mailbox::ProtectedResults::default(),
        )
    }

    /// 带待处理结果保护集合的视图；保护/诊断版本参与缓存键，canonical 修订
    /// 未变但 Ready/Blocked/Covered 切换时也必须重建。
    pub(crate) fn view_with_protected(
        &self,
        transcript: &MessageTranscript,
        capabilities: &ProviderCapabilities,
        protected: &crate::session::async_result_mailbox::ProtectedResults,
    ) -> AgentResult<Arc<Vec<BaseMessage>>> {
        self.view_inner(transcript, capabilities, protected)
    }

    fn view_inner(
        &self,
        transcript: &MessageTranscript,
        capabilities: &ProviderCapabilities,
        protected: &crate::session::async_result_mailbox::ProtectedResults,
    ) -> AgentResult<Arc<Vec<BaseMessage>>> {
        let revision = transcript.view_revision();
        let protected_version = protected.version;

        if let Some(hit) = self.take_matching(revision, capabilities, protected_version) {
            return Ok(hit);
        }

        let messages = Arc::new(
            crate::agent::compact_v2::projection::render_persisted_llm_view_with_protected(
                transcript,
                capabilities,
                protected,
            )?,
        );
        #[cfg(test)]
        self.renders.fetch_add(1, Ordering::Relaxed);
        self.store(
            revision,
            capabilities.clone(),
            protected_version,
            Arc::clone(&messages),
        );
        Ok(messages)
    }

    /// 清空缓存槽（每轮迭代入口调用，保证缓存不跨轮常驻）。
    pub(crate) fn reset(&self) {
        *self.cached.lock() = None;
    }

    /// 释放缓存视图；调用方必须已经持有自己的 `Arc`（本次请求的 messages 快照）。
    ///
    /// 释放只丢弃本槽的引用，已取出的视图仍然有效。
    pub(crate) fn release(&self) {
        *self.cached.lock() = None;
    }

    /// 取作用域守卫：守卫析构时释放缓存槽。
    ///
    /// 准备阶段的退出路径不止一条（hook 错误、提前取消、取快照失败），在入口持有
    /// 守卫即可让全部路径统一释放，不必在每个返回点手写 [`Self::release`]。
    pub(crate) fn release_guard(&self) -> ModelViewReleaseGuard<'_> {
        ModelViewReleaseGuard(self)
    }

    /// 测试观察点：实际渲染次数。
    #[cfg(test)]
    pub(crate) fn render_count(&self) -> usize {
        self.renders.load(Ordering::Relaxed)
    }

    /// 测试观察点：缓存槽是否仍持有视图。
    #[cfg(test)]
    pub(crate) fn is_cached(&self) -> bool {
        self.cached.lock().is_some()
    }

    /// 测试观察点：槽内视图正文句柄数的峰值（重建路径上必须至多为 1）。
    #[cfg(test)]
    pub(crate) fn peak_live_views(&self) -> usize {
        self.liveness.peak.load(Ordering::Relaxed)
    }

    /// 命中则返回共享视图；未命中时**当场丢弃**槽内旧视图并返回 `None`。
    fn take_matching(
        &self,
        revision: TranscriptRevision,
        capabilities: &ProviderCapabilities,
        protected_version: u64,
    ) -> Option<Arc<Vec<BaseMessage>>> {
        let mut cached = self.cached.lock();
        match cached.as_ref() {
            Some(entry)
                if entry.revision == revision
                    && entry.capabilities == *capabilities
                    && entry.protected_version == protected_version =>
            {
                Some(Arc::clone(&entry.messages))
            }
            _ => {
                // 槽为空或键不匹配：旧视图已不可复用，退出锁区前丢弃其正文。
                *cached = None;
                None
            }
        }
    }

    fn store(
        &self,
        revision: TranscriptRevision,
        capabilities: ProviderCapabilities,
        protected_version: u64,
        messages: Arc<Vec<BaseMessage>>,
    ) {
        #[cfg(test)]
        let live = LiveViewHandle::acquire(&self.liveness);
        *self.cached.lock() = Some(CachedModelView {
            revision,
            capabilities,
            protected_version,
            messages,
            #[cfg(test)]
            _live: live,
        });
    }
}

/// 退出准备作用域时释放缓存槽的守卫（见 [`ModelViewScope::release_guard`]）。
pub(crate) struct ModelViewReleaseGuard<'a>(&'a ModelViewScope);

impl Drop for ModelViewReleaseGuard<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}
