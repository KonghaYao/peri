//! 连接级幂等账本（设计 §6.5、§7.5）。
//!
//! 账本只保存轻量记录与**已成功**响应的字节；不保存后端快照或页面对象。所有转换都在
//! 调用方持有的短锁内完成，不跨后端 await。高水位常驻：键一旦被接纳，淘汰后重试只能得到
//! `operation_expired`，不会被当成新操作重新装配。

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::ServiceFailure;

/// 记录条数上限。
pub(super) const LEDGER_MAX_ENTRIES: usize = 256;
/// 成功响应字节上限（8 MiB）。
pub(super) const LEDGER_MAX_BYTES: usize = 8 * 1024 * 1024;
/// 记录 TTL。
pub(super) const LEDGER_TTL: Duration = Duration::from_secs(30 * 60);
/// `InProgress` 超过该时长视为可重试，避免取消后永久阻塞。
pub(super) const IN_PROGRESS_RETRY_AFTER: Duration = Duration::from_secs(120);

/// 可注入的账本预算（测试用小上限复现淘汰）。
#[derive(Clone, Copy, Debug)]
pub(super) struct LedgerLimits {
    pub(super) max_entries: usize,
    pub(super) max_bytes: usize,
    pub(super) ttl: Duration,
    pub(super) in_progress_retry: Duration,
}

impl Default for LedgerLimits {
    fn default() -> Self {
        Self {
            max_entries: LEDGER_MAX_ENTRIES,
            max_bytes: LEDGER_MAX_BYTES,
            ttl: LEDGER_TTL,
            in_progress_retry: IN_PROGRESS_RETRY_AFTER,
        }
    }
}

/// 幂等准入结果。
#[derive(Debug)]
pub(super) enum Admission {
    /// 已有成功记录且仍有效：原样重放响应字节。
    Replay(Value),
    /// 执行本次操作；`attempt` 防止超时后的旧任务覆盖新尝试。
    Execute { attempt: u64 },
}

/// 记录被淘汰、关闭或过期时使用的领域原因。
///
/// initial 的成功重试依赖 delivery 连续性，因此使用 `delivery_expired`；普通只读操作
/// 仍使用 `operation_expired`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExpiryReason {
    OperationExpired,
    DeliveryExpired,
}

impl ExpiryReason {
    fn failure(self) -> ServiceFailure {
        match self {
            Self::OperationExpired => ServiceFailure::operation_expired(),
            Self::DeliveryExpired => ServiceFailure::delivery_expired(),
        }
    }
}

#[derive(Clone)]
enum EntryState {
    InProgress {
        attempt: u64,
        started_at: Duration,
    },
    Retryable {
        recorded_at: Duration,
    },
    Succeeded {
        response: Value,
        response_bytes: usize,
        window_id: Option<String>,
        requires_window: bool,
        completed_at: Duration,
    },
}

impl EntryState {
    fn timestamp(&self) -> Duration {
        match self {
            Self::InProgress { started_at, .. } => *started_at,
            Self::Retryable { recorded_at } => *recorded_at,
            Self::Succeeded { completed_at, .. } => *completed_at,
        }
    }
}

struct Entry {
    fingerprint: [u8; 32],
    state: EntryState,
}

/// 连接级幂等账本；`high_water` 跨淘汰常驻。
pub(super) struct Ledger {
    limits: LedgerLimits,
    high_water: u64,
    entries: HashMap<u64, Entry>,
    total_bytes: usize,
    next_attempt: u64,
}

impl Ledger {
    pub(super) fn new(limits: LedgerLimits) -> Self {
        Self {
            limits,
            high_water: 0,
            entries: HashMap::new(),
            total_bytes: 0,
            next_attempt: 0,
        }
    }

    /// 清理超过 TTL 的记录；高水位保留。
    pub(super) fn prune(&mut self, now: Duration) {
        let stale: Vec<u64> = self
            .entries
            .iter()
            .filter(|(_, entry)| now.saturating_sub(entry.state.timestamp()) >= self.limits.ttl)
            .map(|(key, _)| *key)
            .collect();
        for key in stale {
            self.remove_entry(key);
        }
    }

    /// 校验 requestKey 并登记本次尝试。
    ///
    /// `window_live` 只用于判断成功记录引用的窗口是否仍有效；关闭窗口的 close 记录
    /// 不要求窗口存活。
    pub(super) fn begin(
        &mut self,
        key: u64,
        fingerprint: [u8; 32],
        now: Duration,
        expiry: ExpiryReason,
        window_live: impl FnOnce(&str) -> bool,
    ) -> Result<Admission, ServiceFailure> {
        self.prune(now);
        if let Some(entry) = self.entries.get(&key) {
            if entry.fingerprint != fingerprint {
                return Err(ServiceFailure::request_conflict());
            }
            match entry.state.clone() {
                EntryState::InProgress { started_at, .. } => {
                    if now.saturating_sub(started_at) >= self.limits.in_progress_retry {
                        let attempt = self.next_attempt();
                        self.insert_in_progress(key, fingerprint, attempt, now);
                        return Ok(Admission::Execute { attempt });
                    }
                    Err(ServiceFailure::operation_in_progress())
                }
                EntryState::Retryable { .. } => {
                    let attempt = self.next_attempt();
                    self.insert_in_progress(key, fingerprint, attempt, now);
                    Ok(Admission::Execute { attempt })
                }
                EntryState::Succeeded {
                    response,
                    window_id,
                    requires_window,
                    ..
                } => {
                    if requires_window && window_id.as_deref().is_some_and(|id| !window_live(id)) {
                        self.remove_entry(key);
                        return Err(expiry.failure());
                    }
                    Ok(Admission::Replay(response))
                }
            }
        } else if key <= self.high_water {
            Err(expiry.failure())
        } else {
            self.high_water = key;
            self.ensure_room_for_new()?;
            let attempt = self.next_attempt();
            self.insert_in_progress(key, fingerprint, attempt, now);
            Ok(Admission::Execute { attempt })
        }
    }

    /// 本次尝试是否仍是该 requestKey 的唯一在途尝试。
    ///
    /// 提交阶段必须在写成功记录前复核：超时旧任务、被新尝试接管的旧任务都不得
    /// 发布结果，也不得覆盖账本。
    pub(super) fn is_current(&self, key: u64, attempt: u64, fingerprint: [u8; 32]) -> bool {
        matches!(
            self.entries.get(&key),
            Some(Entry {
                fingerprint: recorded,
                state: EntryState::InProgress {
                    attempt: recorded_attempt,
                    ..
                },
            }) if *recorded == fingerprint && *recorded_attempt == attempt
        )
    }

    /// 成功响应入账；只接受当前尝试，避免超时旧任务覆盖新尝试。
    #[allow(clippy::too_many_arguments)] // 账本字段逐项入账，分组结构会隐藏调用点事实。
    pub(super) fn finish_success(
        &mut self,
        key: u64,
        attempt: u64,
        fingerprint: [u8; 32],
        response: Value,
        window_id: Option<String>,
        requires_window: bool,
        now: Duration,
    ) {
        let applies = matches!(
            self.entries.get(&key),
            Some(Entry {
                fingerprint: recorded,
                state: EntryState::InProgress {
                    attempt: recorded_attempt,
                    ..
                },
            }) if *recorded == fingerprint && *recorded_attempt == attempt
        );
        if !applies {
            return;
        }
        let response_bytes = serde_json::to_vec(&response)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX);
        self.remove_entry(key);
        self.entries.insert(
            key,
            Entry {
                fingerprint,
                state: EntryState::Succeeded {
                    response,
                    response_bytes,
                    window_id,
                    requires_window,
                    completed_at: now,
                },
            },
        );
        self.total_bytes = self.total_bytes.saturating_add(response_bytes);
        self.enforce_byte_budget();
    }

    /// 失败（含后端权限/不存在/快照失效）标记为可重试；不写成功记录。
    pub(super) fn finish_failure(
        &mut self,
        key: u64,
        attempt: u64,
        fingerprint: [u8; 32],
        now: Duration,
    ) {
        let applies = matches!(
            self.entries.get(&key),
            Some(Entry {
                fingerprint: recorded,
                state: EntryState::InProgress {
                    attempt: recorded_attempt,
                    ..
                },
            }) if *recorded == fingerprint && *recorded_attempt == attempt
        );
        if applies {
            self.entries.insert(
                key,
                Entry {
                    fingerprint,
                    state: EntryState::Retryable { recorded_at: now },
                },
            );
        }
    }

    fn next_attempt(&mut self) -> u64 {
        self.next_attempt = self.next_attempt.wrapping_add(1);
        self.next_attempt
    }

    /// 显式作废一条记录；高水位保留，后续同键重试只能得到过期结果。
    pub(super) fn expire(&mut self, key: u64) {
        self.remove_entry(key);
    }

    fn insert_in_progress(&mut self, key: u64, fingerprint: [u8; 32], attempt: u64, now: Duration) {
        self.remove_entry(key);
        self.entries.insert(
            key,
            Entry {
                fingerprint,
                state: EntryState::InProgress {
                    attempt,
                    started_at: now,
                },
            },
        );
    }

    fn remove_entry(&mut self, key: u64) -> Option<Entry> {
        let entry = self.entries.remove(&key)?;
        if let EntryState::Succeeded { response_bytes, .. } = entry.state {
            self.total_bytes = self.total_bytes.saturating_sub(response_bytes);
        }
        Some(entry)
    }

    /// 新操作登记前腾出条数空间：先淘汰最旧的成功记录，再淘汰失败记录。
    fn ensure_room_for_new(&mut self) -> Result<(), ServiceFailure> {
        while self.entries.len() >= self.limits.max_entries {
            if !self.evict_oldest_succeeded() && !self.evict_oldest_retryable() {
                return Err(ServiceFailure::capacity_exceeded());
            }
        }
        Ok(())
    }

    fn enforce_byte_budget(&mut self) {
        while self.total_bytes > self.limits.max_bytes {
            if !self.evict_oldest_succeeded() {
                break;
            }
        }
    }

    fn evict_oldest_succeeded(&mut self) -> bool {
        let candidate = self
            .entries
            .iter()
            .filter_map(|(key, entry)| match &entry.state {
                EntryState::Succeeded { completed_at, .. } => Some((*completed_at, *key)),
                _ => None,
            })
            .min();
        if let Some((_, key)) = candidate {
            self.remove_entry(key);
            true
        } else {
            false
        }
    }

    fn evict_oldest_retryable(&mut self) -> bool {
        let candidate = self
            .entries
            .iter()
            .filter_map(|(key, entry)| match &entry.state {
                EntryState::Retryable { recorded_at } => Some((*recorded_at, *key)),
                _ => None,
            })
            .min();
        if let Some((_, key)) = candidate {
            self.remove_entry(key);
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    pub(super) fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

/// 对 params 递归排序键后计算 SHA-256；`requestKey` 不参与指纹。
pub(super) fn fingerprint(params: &Value) -> [u8; 32] {
    let mut value = params.clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("requestKey");
    }
    let canonical = canonicalize(&value);
    let encoded = serde_json::to_vec(&canonical).expect("serde_json::Value is serializable");
    Sha256::digest(encoded).into()
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort();
            let mut sorted = Map::new();
            for key in keys {
                sorted.insert(key.clone(), canonicalize(&object[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}
