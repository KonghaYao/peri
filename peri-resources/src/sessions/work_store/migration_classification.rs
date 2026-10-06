use peri_acp_types::session_resources::{work::WorkResolution, SessionResourceResult};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

pub(crate) fn legacy_command_unresolved(
    session_id: &str,
    mutation_id: &str,
    command_json: &str,
    digest: &str,
    receipt_digest: Option<&str>,
    receipt_json: Option<&str>,
) -> SessionResourceResult<bool> {
    if receipt_digest != Some(digest)
        || format!("{:x}", Sha256::digest(command_json.as_bytes())) != digest
    {
        return Ok(true);
    }
    let Ok(command) = serde_json::from_str::<Value>(command_json) else {
        return Ok(true);
    };
    if command.get("sessionId").and_then(Value::as_str) != Some(session_id)
        || command.get("mutationId").and_then(Value::as_str) != Some(mutation_id)
        || command
            .get("recipientLifecycle")
            .and_then(Value::as_u64)
            .is_none_or(|lifecycle| lifecycle == 0)
    {
        return Ok(true);
    }
    let Some(receipt_json) = receipt_json else {
        return Ok(true);
    };
    let Ok(resolution) = serde_json::from_str::<WorkResolution>(receipt_json) else {
        return Ok(true);
    };
    Ok(match resolution {
        WorkResolution::Applied { receipt } => {
            receipt.session_id != session_id || receipt.mutation_id != mutation_id
        }
        WorkResolution::NotApplied => false,
        WorkResolution::Unknown => true,
    })
}

fn records<'source>(source: &'source Value, name: &str) -> Option<&'source Map<String, Value>> {
    source.get(name)?.as_object()
}

pub(crate) fn classify_legacy(json: &str) -> SessionResourceResult<bool> {
    let Ok(source) = serde_json::from_str::<Value>(json) else {
        return Ok(true);
    };
    let Some(object) = source.as_object() else {
        return Ok(true);
    };
    let keys = [
        "revision",
        "nextAdmissionSequence",
        "limits",
        "deliveries",
        "obligations",
        "batches",
        "works",
        "budgets",
        "invocations",
        "taskBindings",
        "legacyUnknown",
        "admissions",
        "resourceOwners",
        "childResumeMetadata",
        "terminalObligations",
        "terminalAcknowledgements",
        "workDelegations",
        "stagedUserInputs",
        "userInputPublications",
    ];
    if object.keys().any(|key| !keys.contains(&key.as_str()))
        || source.get("revision").and_then(Value::as_u64).is_none()
        || source
            .get("nextAdmissionSequence")
            .and_then(Value::as_u64)
            .is_none()
    {
        return Ok(true);
    }
    let Some(unknown) = records(&source, "legacyUnknown") else {
        return Ok(true);
    };
    if !unknown.is_empty() {
        return Ok(true);
    }
    let Some(works) = records(&source, "works") else {
        return Ok(true);
    };
    let Some(batches) = records(&source, "batches") else {
        return Ok(true);
    };
    let Some(budgets) = records(&source, "budgets") else {
        return Ok(true);
    };
    for (identity, record) in works {
        if record.get("workId").and_then(Value::as_str) != Some(identity.as_str())
            || !matches!(
                record.get("stage").and_then(Value::as_str),
                Some("settled" | "abandoned")
            )
            || record
                .get("batchId")
                .and_then(Value::as_str)
                .is_none_or(|identity| !batches.contains_key(identity))
            || record
                .get("budgetId")
                .and_then(Value::as_str)
                .is_none_or(|identity| !budgets.contains_key(identity))
        {
            return Ok(true);
        }
    }
    let Some(obligations) = records(&source, "obligations") else {
        return Ok(true);
    };
    if obligations.values().any(|record| {
        !matches!(
            record.get("status").and_then(Value::as_str),
            Some("satisfied" | "suppressed" | "abandoned")
        )
    }) {
        return Ok(true);
    }
    let Some(invocations) = records(&source, "invocations") else {
        return Ok(true);
    };
    if invocations.values().any(|record| {
        record.get("status").and_then(Value::as_str) != Some("settled")
            || record.get("outcome").is_none_or(Value::is_null)
    }) {
        return Ok(true);
    }
    let Some(deliveries) = records(&source, "deliveries") else {
        return Ok(true);
    };
    for record in deliveries.values() {
        let terminal_disposition = matches!(
            record.get("disposition").and_then(Value::as_str),
            Some("withdrawn" | "suppressed" | "abandoned" | "satisfied")
        );
        let terminal_batch = record
            .get("batchId")
            .and_then(Value::as_str)
            .is_some_and(|batch| {
                works
                    .values()
                    .any(|work| work.get("batchId").and_then(Value::as_str) == Some(batch))
            });
        if !terminal_disposition && !terminal_batch {
            return Ok(true);
        }
    }
    if let Some(drafts) = records(&source, "stagedUserInputs") {
        if drafts.values().any(|record| {
            !matches!(
                record.get("status").and_then(Value::as_str),
                Some("published" | "withdrawn")
            )
        }) {
            return Ok(true);
        }
    }
    let Some(admissions) = records(&source, "admissions") else {
        return Ok(true);
    };
    if admissions
        .values()
        .any(|record| record.get("settledReceipt").is_none_or(Value::is_null))
    {
        return Ok(true);
    }
    if let Some(terminal) = records(&source, "terminalObligations") {
        let acknowledgements = records(&source, "terminalAcknowledgements");
        if !terminal.is_empty() || acknowledgements.is_some_and(|records| !records.is_empty()) {
            return Ok(true);
        }
    }
    Ok(false)
}
