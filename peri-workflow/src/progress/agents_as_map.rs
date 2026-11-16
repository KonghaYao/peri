use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use peri_acp_types::workflow::AgentProgress;

/// 将 IndexMap 序列化为 JSON 数组，保持与 Vec<AgentProgress> 相同的输出格式。
pub fn serialize<S>(map: &IndexMap<u64, AgentProgress>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let values: Vec<&AgentProgress> = map.values().collect();
    values.serialize(serializer)
}

/// 将 JSON 数组反序列化为 IndexMap，以 agent_id 为 key。
pub fn deserialize<'de, D>(deserializer: D) -> Result<IndexMap<u64, AgentProgress>, D::Error>
where
    D: Deserializer<'de>,
{
    let vec: Vec<AgentProgress> = Vec::deserialize(deserializer)?;
    let map: IndexMap<u64, AgentProgress> = vec.into_iter().map(|a| (a.agent_id, a)).collect();
    Ok(map)
}
