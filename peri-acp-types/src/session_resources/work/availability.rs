use super::*;
use crate::session::MessageRequirement;

/// 仅供内部通知查询使用，不属于 ACP wire 协议。
#[derive(Clone, Debug)]
pub struct WorkAvailability {
    pub control: ControlState,
    pub state: WorkAvailabilityState,
}

impl WorkAvailability {
    pub fn is_available(&self, lifecycle: u64, observer_floor: Option<u64>) -> bool {
        if self.control.lifecycle != lifecycle {
            return false;
        }
        match observer_floor {
            None => self.state.has_pending_work_for(lifecycle),
            Some(floor) => self.state.claimable_deliveries(lifecycle).iter().any(|id| {
                let delivery = &self.state.deliveries[id];
                delivery.admission_sequence >= floor
                    && delivery.requirement == MessageRequirement::Required
            }),
        }
    }
}

/// 存储投影只携带判定事实，不携带请求、响应、消息正文或 mutation 载荷。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkAvailabilityState {
    pub revision: u64,
    pub max_batch_size: u64,
    pub deliveries: BTreeMap<String, AvailabilityDelivery>,
    pub obligations: BTreeMap<String, ObligationStatus>,
    pub works: BTreeMap<String, AvailabilityWork>,
    pub batches: BTreeMap<String, AvailabilityBatch>,
    pub admissions: BTreeMap<String, AvailabilityAdmission>,
    pub legacy_unknown: Vec<String>,
    pub terminal_obligations: Vec<String>,
    pub terminal_acknowledgements: std::collections::BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AvailabilityDelivery {
    pub recipient_lifecycle: u64,
    pub admission_sequence: u64,
    pub requirement: MessageRequirement,
    pub batch_id: Option<String>,
    pub disposition: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AvailabilityWork {
    pub work_id: String,
    pub batch_id: String,
    pub stage: WorkStage,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AvailabilityBatch {
    pub batch_id: String,
    pub recipient_lifecycle: u64,
    pub processing_delivery_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AvailabilityAdmission {
    pub session_id: String,
    pub work_id: String,
    pub lifecycle: u64,
    pub execution: ControlAttempt,
}

impl From<&WorkState> for WorkAvailabilityState {
    fn from(state: &WorkState) -> Self {
        Self {
            revision: state.revision,
            max_batch_size: state.limits.max_batch_size,
            deliveries: state
                .deliveries
                .iter()
                .map(|(id, record)| {
                    (
                        id.clone(),
                        AvailabilityDelivery {
                            recipient_lifecycle: record.recipient_lifecycle,
                            admission_sequence: record.admission_sequence,
                            requirement: record.publication.policy.requirement,
                            batch_id: record.batch_id.clone(),
                            disposition: record.disposition.clone(),
                        },
                    )
                })
                .collect(),
            obligations: state
                .obligations
                .iter()
                .map(|(id, record)| (id.clone(), record.status))
                .collect(),
            works: state
                .works
                .iter()
                .map(|(id, record)| {
                    (
                        id.clone(),
                        AvailabilityWork {
                            work_id: record.work_id.clone(),
                            batch_id: record.batch_id.clone(),
                            stage: record.stage,
                        },
                    )
                })
                .collect(),
            batches: state
                .batches
                .iter()
                .map(|(id, record)| {
                    (
                        id.clone(),
                        AvailabilityBatch {
                            batch_id: record.batch_id.clone(),
                            recipient_lifecycle: record.recipient_lifecycle,
                            processing_delivery_ids: record.processing_delivery_ids.clone(),
                        },
                    )
                })
                .collect(),
            admissions: state
                .admissions
                .iter()
                .map(|(id, record)| {
                    (
                        id.clone(),
                        AvailabilityAdmission {
                            session_id: record.admission.session_id.clone(),
                            work_id: record.admission.work_id.clone(),
                            lifecycle: record.admission.lifecycle,
                            execution: record.admission.execution.clone(),
                        },
                    )
                })
                .collect(),
            legacy_unknown: state.legacy_unknown.keys().cloned().collect(),
            terminal_obligations: state.terminal_obligations.keys().cloned().collect(),
            terminal_acknowledgements: state.terminal_acknowledgements.keys().cloned().collect(),
        }
    }
}

impl WorkAvailabilityState {
    fn work_lifecycle(&self, work_id: &str) -> Option<u64> {
        self.works
            .get(work_id)
            .and_then(|work| self.batches.get(&work.batch_id))
            .map(|batch| batch.recipient_lifecycle)
    }

    pub(super) fn has_unknown_live_work_lifecycle(&self) -> bool {
        self.works.values().any(|work| {
            !matches!(work.stage, WorkStage::Settled | WorkStage::Abandoned)
                && self.work_lifecycle(&work.work_id).is_none()
        })
    }

    pub(super) fn has_pending_terminal_obligations_for(&self, lifecycle: u64) -> bool {
        self.terminal_obligations.iter().any(|admission_id| {
            !self.terminal_acknowledgements.contains(admission_id)
                && self.admissions.get(admission_id).is_none_or(|admission| {
                    admission.lifecycle == lifecycle
                        && !self.admission_processing_superseded(admission)
                })
        })
    }

    pub(super) fn admission_batch_key(&self, work_id: &str, lifecycle: u64) -> Option<&str> {
        admission_batch_key(
            self.works.get(work_id).map(|work| work.batch_id.as_str()),
            work_id,
            lifecycle,
            self.batches.iter().map(|(id, batch)| {
                (
                    id.as_str(),
                    batch.recipient_lifecycle,
                    batch.processing_delivery_ids.as_slice(),
                )
            }),
        )
    }

    fn admission_processing_superseded(&self, admission: &AvailabilityAdmission) -> bool {
        let mut abandoned = false;
        for record in self.admissions.values().filter(|record| {
            record.lifecycle == admission.lifecycle
                && record.session_id == admission.session_id
                && record.execution == admission.execution
        }) {
            let Some(key) = self.admission_batch_key(&record.work_id, record.lifecycle) else {
                return false;
            };
            let batch = &self.batches[key];
            let mut found_work = false;
            for work in self
                .works
                .values()
                .filter(|work| work.batch_id == batch.batch_id)
            {
                found_work = true;
                match work.stage {
                    WorkStage::Abandoned => abandoned = true,
                    WorkStage::Settled => {}
                    _ => return false,
                }
            }
            if !found_work {
                return false;
            }
        }
        abandoned
    }

    pub(super) fn has_pending_work_for(&self, lifecycle: u64) -> bool {
        !self.legacy_unknown.is_empty()
            || self.has_unknown_live_work_lifecycle()
            || self.has_pending_terminal_obligations_for(lifecycle)
            || self.obligations.iter().any(|(delivery_id, status)| {
                matches!(
                    status,
                    ObligationStatus::Pending
                        | ObligationStatus::InProgress
                        | ObligationStatus::Blocked
                ) && self
                    .deliveries
                    .get(delivery_id)
                    .is_none_or(|delivery| delivery.recipient_lifecycle == lifecycle)
            })
            || self.works.values().any(|work| {
                !matches!(work.stage, WorkStage::Settled | WorkStage::Abandoned)
                    && self.work_lifecycle(&work.work_id) == Some(lifecycle)
            })
    }

    pub(super) fn claimable_deliveries(&self, lifecycle: u64) -> Vec<String> {
        let mut pending: Vec<_> = self
            .deliveries
            .iter()
            .filter(|(delivery_id, record)| {
                record.recipient_lifecycle == lifecycle
                    && record.batch_id.is_none()
                    && record.disposition.is_none()
                    && self.obligations.get(*delivery_id).is_none_or(|status| {
                        matches!(
                            status,
                            ObligationStatus::Pending | ObligationStatus::Blocked
                        )
                    })
            })
            .collect();
        pending.sort_by_key(|(_, record)| record.admission_sequence);
        let limit = self.max_batch_size as usize;
        let mut selected: Vec<_> = pending.iter().take(limit).copied().collect();
        if limit > 0
            && !selected
                .iter()
                .any(|(_, record)| record.requirement == MessageRequirement::Required)
        {
            if let Some(required) = pending
                .iter()
                .find(|(_, record)| record.requirement == MessageRequirement::Required)
            {
                if selected.len() == limit {
                    selected.pop();
                }
                selected.push(*required);
                selected.sort_by_key(|(_, record)| record.admission_sequence);
            }
        }
        selected.into_iter().map(|(id, _)| id.clone()).collect()
    }
}

// 两种状态表示共享关联规则；完整状态的准入查询不必为返回一个 batch 克隆全部事实。
pub(super) fn admission_batch_key<'a>(
    batch_id: Option<&str>,
    work_id: &str,
    lifecycle: u64,
    batches: impl Iterator<Item = (&'a str, u64, &'a [String])>,
) -> Option<&'a str> {
    let mut matches = batches.filter(|(id, recipient, processing)| {
        *recipient == lifecycle
            && match batch_id {
                Some(batch_id) => *id == batch_id,
                None => processing.iter().any(|id| id == work_id),
            }
    });
    let (key, _, _) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(key)
}
