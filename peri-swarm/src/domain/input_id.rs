use uuid::{uuid, Uuid};

use super::LogicalId;

// UUIDv5(NAMESPACE_DNS, "peri-swarm.input-idempotency-key.v1")。
// 此值是持久化契约：变更会让已受理的键映射成新输入。
const IDEMPOTENCY_NAMESPACE: Uuid = uuid!("76663edb-72b5-58df-9b4c-b1e3c8f8d418");

pub(crate) fn input_id_for_key(logical_id: &LogicalId, key: &str) -> String {
    let mut name = Vec::with_capacity(logical_id.as_str().len() + 1 + key.len());
    name.extend_from_slice(logical_id.as_str().as_bytes());
    name.push(0);
    name.extend_from_slice(key.as_bytes());
    Uuid::new_v5(&IDEMPOTENCY_NAMESPACE, &name).to_string()
}

#[cfg(test)]
#[path = "input_id_test.rs"]
mod tests;
