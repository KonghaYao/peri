//! 会话 frozen 数据的版本化持久化格式。
//!
//! wire blob 由 [`ThreadStore`](peri_acp_types::store::ThreadStore) 专用接口
//! 持久化，不进入 `ThreadMeta` / thread list。未知未来版本 fail closed，禁止
//! 旧二进制以当前格式覆盖。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use peri_agent::session::exec::executor::FrozenSessionData;
use serde::{Deserialize, Serialize};

const FROZEN_SNAPSHOT_VERSION: u64 = 1;

#[derive(Debug, thiserror::Error)]
pub(crate) enum FrozenSnapshotError {
    #[error("invalid frozen snapshot: {0}")]
    Invalid(#[from] serde_json::Error),
    #[error("unsupported frozen snapshot version: {0}")]
    UnsupportedVersion(u64),
}

#[derive(Serialize, Deserialize)]
struct FrozenSnapshotEnvelope {
    version: u64,
    data: FrozenSnapshotV1,
}

#[derive(Serialize, Deserialize)]
struct FrozenSnapshotV1 {
    system_prompt: String,
    claude_md: String,
    claude_local_md: Option<String>,
    skill_summary: String,
    date: String,
    language: Option<String>,
    meta_harness: MetaHarnessSnapshotV1,
}

#[derive(Serialize, Deserialize)]
struct MetaHarnessSnapshotV1 {
    section_overrides: BTreeMap<String, String>,
    disabled_middlewares: BTreeSet<String>,
    built_in_subagents_enabled: bool,
}

pub(crate) fn encode_frozen_snapshot(
    frozen: &FrozenSessionData,
) -> Result<String, FrozenSnapshotError> {
    let meta = frozen.meta_harness();
    let envelope = FrozenSnapshotEnvelope {
        version: FROZEN_SNAPSHOT_VERSION,
        data: FrozenSnapshotV1 {
            system_prompt: frozen.system_prompt().to_string(),
            claude_md: frozen.claude_md().unwrap_or_default().to_string(),
            claude_local_md: frozen.claude_local_md().map(str::to_string),
            skill_summary: frozen.skill_summary().unwrap_or_default().to_string(),
            date: frozen.date().to_string(),
            language: frozen.language().map(str::to_string),
            meta_harness: MetaHarnessSnapshotV1 {
                section_overrides: meta
                    .section_overrides
                    .iter()
                    .map(|(key, value)| (key.clone(), value.to_string()))
                    .collect(),
                disabled_middlewares: meta.disabled_middlewares.iter().cloned().collect(),
                built_in_subagents_enabled: meta.built_in_subagents_enabled,
            },
        },
    };
    serde_json::to_string(&envelope).map_err(FrozenSnapshotError::Invalid)
}

pub(crate) fn decode_frozen_snapshot(raw: &str) -> Result<FrozenSessionData, FrozenSnapshotError> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| invalid_snapshot("missing unsigned version"))?;
    if version != FROZEN_SNAPSHOT_VERSION {
        return Err(FrozenSnapshotError::UnsupportedVersion(version));
    }
    let envelope: FrozenSnapshotEnvelope = serde_json::from_value(value)?;
    let data = envelope.data;
    let meta_harness = peri_acp_types::meta_harness::MetaHarnessState {
        section_overrides: data
            .meta_harness
            .section_overrides
            .into_iter()
            .map(|(key, value)| (key, Arc::<str>::from(value)))
            .collect::<HashMap<_, _>>(),
        disabled_middlewares: data
            .meta_harness
            .disabled_middlewares
            .into_iter()
            .collect::<HashSet<_>>(),
        built_in_subagents_enabled: data.meta_harness.built_in_subagents_enabled,
    };
    let frozen = peri_agent::session::FrozenContext {
        system_prompt: Arc::from(data.system_prompt),
        claude_md: Arc::from(data.claude_md),
        skill_summary: Arc::from(data.skill_summary),
        date: Arc::from(data.date),
        language: data.language.map(Arc::from),
        meta_harness,
    };
    Ok(FrozenSessionData::from_frozen_parts(
        frozen,
        data.claude_local_md.map(Arc::from),
    ))
}

fn invalid_snapshot(message: &str) -> FrozenSnapshotError {
    FrozenSnapshotError::Invalid(<serde_json::Error as serde::de::Error>::custom(message))
}

#[cfg(test)]
#[path = "frozen_snapshot_test.rs"]
mod tests;
