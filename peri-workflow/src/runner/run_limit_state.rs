//! Per-run limit accounting and the first fatal failure used by terminal projection.
//! A fatal child scope-close failure shares this existing failure slot with limits;
//! task admission is fenced separately and atomically by WorkflowTaskTable.
//!
//! P6 前计数发生在 `agent/run` 分派路径（执行 task 持有 permit）；改为任务表驱动后，
//! 计数来源是任务终态结果（投递路径前置观察），读取方是消息循环
//! （`workflow/done` 的终态投影）。
//!
//! 两类事实分开保存：`breach` 是首条失败文本（旧消费者只看这一条，保持兼容）；
//! `breaches` 列出全部已触发的硬上限（scope / 上限 / 观测值 / attempt 身份 / 时点）。
//! 每次 run 的计数按稳定身份幂等：投递重试与缓存命中都不重复记账。

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use peri_acp_types::workflow::LimitBreach;
use peri_acp_types::workflow::LimitBreachAttempt;
use peri_acp_types::workflow::LimitScope;

/// 已触发的 run 级硬上限（wire 侧只带文本，这里补齐结构化数值）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunLimitBreach {
    pub scope: LimitScope,
    pub limit: Option<u64>,
    pub observed: Option<u64>,
    /// 顶层 `error` 兼容文本（首条）。
    pub message: String,
}

impl RunLimitBreach {
    #[must_use]
    pub fn new(
        scope: LimitScope,
        limit: Option<u64>,
        observed: Option<u64>,
        message: String,
    ) -> Self {
        Self {
            scope,
            limit,
            observed,
            message,
        }
    }
}

/// run 结束时的限额事实（终态投影写入 `state.json`）。
#[derive(Debug, Clone, Default)]
pub struct RunLimitReport {
    pub breaches: Vec<LimitBreach>,
    /// 按 attempt 累计的 token 用量；未采集为 `None`。
    pub token_usage: Option<u64>,
    /// 首个失败文本（顶层 `error` 的兼容来源）；与 `breaches` 可能不同源
    /// （子作用域未结清等非限额致命失败只写文本）。
    pub first_error: Option<String>,
}

/// 单 run limit 状态（消息循环与任务投递路径共享；进程内、随 run 生命周期）。
pub(crate) struct RunLimitState {
    observed_tool_calls: AtomicU64,
    /// 已计入 tool 调用的任务身份（taskId 作用域 = 单次 run）：终态回投是投递重试，
    /// 不是第二次执行，计数只认首次。集合随 run 生命周期回收，规模与 run 内任务数同阶。
    counted_tasks: parking_lot::Mutex<HashSet<String>>,
    max_tool_calls: Option<u64>,
    /// run 的 token 预算（`budgetTotal`）；`None` = 未配置。
    token_budget: Option<u64>,
    /// 按任务身份累计的 token 用量（只统计执行器上报的值）。
    observed_tokens: AtomicU64,
    /// 已计入 token 的任务身份：缓存命中没有本次执行，重投只计首次。
    counted_token_tasks: parking_lot::Mutex<HashSet<String>>,
    /// 已记录过迭代耗尽的尝试身份（与 tool 计数去重集合分开，避免互相吞掉）。
    counted_iteration_tasks: parking_lot::Mutex<HashSet<String>>,
    breach: parking_lot::Mutex<Option<String>>,
    breaches: parking_lot::Mutex<Vec<LimitBreach>>,
}

impl RunLimitState {
    pub(crate) fn new(max_tool_calls: Option<u64>, token_budget: Option<u64>) -> Self {
        Self {
            observed_tool_calls: AtomicU64::new(0),
            counted_tasks: parking_lot::Mutex::new(HashSet::new()),
            max_tool_calls,
            token_budget,
            observed_tokens: AtomicU64::new(0),
            counted_token_tasks: parking_lot::Mutex::new(HashSet::new()),
            counted_iteration_tasks: parking_lot::Mutex::new(HashSet::new()),
            breach: parking_lot::Mutex::new(None),
            breaches: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// 已记录的 breach 详情（`workflow/done` 终态投影读取）。
    pub(crate) fn breach(&self) -> Option<String> {
        self.breach.lock().clone()
    }

    /// 全部已触发的硬上限（`state.json` 的 `limit_breaches`）。
    pub(crate) fn breaches(&self) -> Vec<LimitBreach> {
        self.breaches.lock().clone()
    }

    /// 已采集的 token 用量；没有任何执行上报时为 `None`（不得显示为 0）。
    pub(crate) fn observed_tokens(&self) -> Option<u64> {
        if self.counted_token_tasks.lock().is_empty() {
            None
        } else {
            Some(self.observed_tokens.load(Ordering::SeqCst))
        }
    }

    /// 当前限额事实快照（终态投影读取一次，避免分两次读出现交错）。
    pub(crate) fn report(&self) -> RunLimitReport {
        RunLimitReport {
            breaches: self.breaches(),
            token_usage: self.observed_tokens(),
            first_error: self.breach(),
        }
    }

    /// 记录 breach（只写首次；后到的超限不覆写首个错误文本）。
    ///
    /// 用于非限额类的致命失败（如子作用域未结清）：它们不进入 `limit_breaches`。
    pub(crate) fn set_breach(&self, detail: String) {
        let mut breach = self.breach.lock();
        if breach.is_none() {
            *breach = Some(detail);
        }
    }

    /// 记录一次结构化超限；`breaches` 保留全部条目，`breach` 只保留首条文本。
    fn record_breach(&self, entry: LimitBreach, message: String) {
        self.breaches.lock().push(entry);
        self.set_breach(message);
    }

    /// 记录 run 级超限（无单次 attempt 归因，如 elapsed / agents）。
    pub(crate) fn record_run_breach(&self, breach: RunLimitBreach) {
        self.record_breach(
            LimitBreach {
                scope: breach.scope,
                limit: breach.limit,
                observed: breach.observed,
                attempt: None,
                detected_at: peri_time::now_utc_rfc3339(),
            },
            breach.message,
        );
    }

    /// 累计一次任务结果的 tool 调用计数；同一 task 的重复回填只计首次。
    ///
    /// 任务表允许对已终态任务重复 `settle` 以重试投递（design §5.1）：投递可重试，
    /// 计数不能重算，否则同一批 tool 调用被重复累计，未超限的 run 会误报 `maxToolCalls`。
    pub(crate) fn observe_tool_calls(
        &self,
        run_id: &str,
        task_id: &str,
        agent_id: u64,
        count: u64,
    ) -> Option<String> {
        let Some(maximum) = self.max_tool_calls else {
            // 无上限时不产生 breach，也无需为去重保留任务身份。
            return None;
        };
        if !self.counted_tasks.lock().insert(task_id.to_owned()) {
            return None;
        }
        let total = self
            .observed_tool_calls
            .fetch_add(count, Ordering::SeqCst)
            .saturating_add(count);
        if total > maximum {
            let detail = format!("workflow exceeded maxToolCalls ({maximum})");
            self.record_breach(
                LimitBreach {
                    scope: LimitScope::ToolCalls,
                    limit: Some(maximum),
                    observed: Some(total),
                    attempt: Some(LimitBreachAttempt {
                        run_id: run_id.to_string(),
                        task_id: Some(task_id.to_owned()),
                        agent_id,
                    }),
                    detected_at: peri_time::now_utc_rfc3339(),
                },
                detail.clone(),
            );
            return Some(detail);
        }
        None
    }

    /// 累计一次任务结果的 token 用量；同一 task 的重投只计首次（缓存命中不执行新调用）。
    pub(crate) fn observe_tokens(
        &self,
        run_id: &str,
        task_id: &str,
        agent_id: u64,
        count: u64,
    ) -> Option<String> {
        let Some(budget) = self.token_budget else {
            // 未配置预算时不产生 breach；用量仍要累计，供 run 状态展示。
            if self.counted_token_tasks.lock().insert(task_id.to_owned()) {
                self.observed_tokens.fetch_add(count, Ordering::SeqCst);
            }
            return None;
        };
        if !self.counted_token_tasks.lock().insert(task_id.to_owned()) {
            return None;
        }
        let total = self
            .observed_tokens
            .fetch_add(count, Ordering::SeqCst)
            .saturating_add(count);
        if total > budget {
            let detail = format!("workflow exceeded budgetTotal ({budget})");
            self.record_breach(
                LimitBreach {
                    scope: LimitScope::Tokens,
                    limit: Some(budget),
                    observed: Some(total),
                    attempt: Some(LimitBreachAttempt {
                        run_id: run_id.to_string(),
                        task_id: Some(task_id.to_owned()),
                        agent_id,
                    }),
                    detected_at: peri_time::now_utc_rfc3339(),
                },
                detail.clone(),
            );
            return Some(detail);
        }
        None
    }

    /// 记录一次迭代上限耗尽（上限与用量取执行器解析/计数结果，未知为 None）。
    pub(crate) fn observe_iteration_exhaustion(
        &self,
        run_id: &str,
        task_id: &str,
        agent_id: u64,
        limit: Option<u64>,
        observed: Option<u64>,
    ) -> Option<String> {
        if !self
            .counted_iteration_tasks
            .lock()
            .insert(task_id.to_owned())
        {
            return None;
        }
        let detail = match limit {
            Some(limit) => format!("workflow exceeded maxIterations ({limit})"),
            None => "workflow exceeded maxIterations".to_string(),
        };
        self.record_breach(
            LimitBreach {
                scope: LimitScope::Iterations,
                limit,
                observed,
                attempt: Some(LimitBreachAttempt {
                    run_id: run_id.to_string(),
                    task_id: Some(task_id.to_owned()),
                    agent_id,
                }),
                detected_at: peri_time::now_utc_rfc3339(),
            },
            detail.clone(),
        );
        Some(detail)
    }
}
