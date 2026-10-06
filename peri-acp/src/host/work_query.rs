use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::session_resources::work::{
    AdmissionRecord, OwnedWorkCommand, Processing, RecoveryDescriptor, TerminalObligation,
    WorkAvailability, WorkInspection, WorkPage, WorkQuery, WorkSelector,
};

use crate::transport::types::AcpError;

pub(super) async fn inspect(
    resources: &dyn SessionResources,
    session_id: &str,
    selector: WorkSelector,
) -> Result<WorkInspection, AcpError> {
    let limit = if matches!(selector, WorkSelector::Availability) {
        64
    } else {
        1
    };
    let inspection = resources
        .inspect_work(&WorkQuery {
            session_id: session_id.into(),
            selector,
            limit,
            cursor: None,
        })
        .await
        .map_err(super::workspace::resource_error)?;
    if inspection.session_id != session_id {
        return Err(wrong_page());
    }
    Ok(inspection)
}

pub(super) fn admission<'record>(
    page: &'record WorkInspection,
    identity: &str,
) -> Result<Option<&'record AdmissionRecord>, AcpError> {
    let WorkPage::Admissions(records) = &page.page else {
        return Err(wrong_page());
    };
    Ok(records
        .iter()
        .find(|record| record.admission.admission_id == identity))
}

pub(super) fn descriptor(
    page: &WorkInspection,
    lifecycle: u64,
) -> Result<Option<&RecoveryDescriptor>, AcpError> {
    let WorkPage::RecoveryDescriptors(records) = &page.page else {
        return Err(wrong_page());
    };
    Ok(records
        .iter()
        .find(|record| record.recipient_lifecycle == lifecycle))
}

pub(super) fn processing<'record>(
    page: &'record WorkInspection,
    identity: &str,
) -> Result<Option<&'record Processing>, AcpError> {
    let WorkPage::Processings(records) = &page.page else {
        return Err(wrong_page());
    };
    Ok(records
        .iter()
        .find(|record| record.processing_id == identity))
}

pub(super) fn terminal<'record>(
    page: &'record WorkInspection,
    identity: &str,
) -> Result<Option<&'record TerminalObligation>, AcpError> {
    let WorkPage::TerminalCommands(records) = &page.page else {
        return Err(wrong_page());
    };
    Ok(records
        .iter()
        .find(|record| record.admission_id == identity))
}

pub(super) fn availability(page: &WorkInspection) -> Result<&WorkAvailability, AcpError> {
    let WorkPage::Availability(availability) = &page.page else {
        return Err(wrong_page());
    };
    Ok(availability)
}

pub(super) async fn command(
    resources: &dyn SessionResources,
    session_id: &str,
    mutation_id: &str,
) -> Result<Option<OwnedWorkCommand>, AcpError> {
    let page = inspect(
        resources,
        session_id,
        WorkSelector::Command {
            mutation_id: mutation_id.into(),
        },
    )
    .await?;
    let WorkPage::Commands(mut records) = page.page else {
        return Err(wrong_page());
    };
    if records.len() > 1
        || records.first().is_some_and(|record| {
            record.command.session_id != session_id || record.command.mutation_id != mutation_id
        })
    {
        return Err(wrong_page());
    }
    Ok(records.pop())
}

pub(super) fn wrong_page() -> AcpError {
    AcpError::new(-32603, "work inspection returned a conflicting typed page")
}

#[cfg(test)]
pub(super) async fn test_payload(
    resources: &dyn SessionResources,
    session_id: &str,
    payload: &peri_acp_types::store::PersistedPayload,
) -> peri_acp_types::session_resources::work::WorkPayload {
    use peri_acp_types::messages::BaseMessage;
    use peri_acp_types::session_resources::work::{EvidenceWrite, WorkPayload};
    use peri_acp_types::store::{PersistedPayload, serialize_persisted_payload};
    let role = match payload {
        PersistedPayload::Message(BaseMessage::Human { .. }) => "user",
        PersistedPayload::Message(BaseMessage::Ai { .. }) => "assistant",
        PersistedPayload::Message(BaseMessage::Tool { .. }) => "tool",
        PersistedPayload::Message(BaseMessage::System { .. }) => "system",
        PersistedPayload::SystemReminder { .. } => "system_reminder",
    };
    let tool_call_id = match payload {
        PersistedPayload::Message(BaseMessage::Tool { tool_call_id, .. }) => {
            Some(tool_call_id.clone())
        }
        _ => None,
    };
    let content = resources
        .prepare_evidence(&EvidenceWrite {
            session_id: session_id.into(),
            storage_scope: session_id.into(),
            payload_id: payload.id().as_uuid().to_string(),
            encoding: 1,
            bytes: serialize_persisted_payload(payload).unwrap().into_bytes(),
        })
        .await
        .unwrap();
    WorkPayload {
        message_id: payload.id(),
        role: role.into(),
        content,
        tool_call_id,
    }
}

#[cfg(test)]
pub(super) async fn test_processing(
    resources: &dyn SessionResources,
    session_id: &str,
    processing_id: &str,
) -> Processing {
    let inspection = inspect(
        resources,
        session_id,
        WorkSelector::Processing {
            processing_id: processing_id.into(),
        },
    )
    .await
    .unwrap();
    processing(&inspection, processing_id)
        .unwrap()
        .unwrap()
        .clone()
}

#[cfg(test)]
pub(super) async fn test_admission(
    resources: &dyn SessionResources,
    session_id: &str,
    admission_id: &str,
) -> AdmissionRecord {
    let inspection = inspect(
        resources,
        session_id,
        WorkSelector::Admission {
            admission_id: admission_id.into(),
        },
    )
    .await
    .unwrap();
    admission(&inspection, admission_id)
        .unwrap()
        .unwrap()
        .clone()
}

#[cfg(test)]
pub(super) async fn test_terminal(
    resources: &dyn SessionResources,
    session_id: &str,
    admission_id: &str,
) -> TerminalObligation {
    let inspection = inspect(
        resources,
        session_id,
        WorkSelector::TerminalCommand {
            admission_id: admission_id.into(),
        },
    )
    .await
    .unwrap();
    terminal(&inspection, admission_id)
        .unwrap()
        .unwrap()
        .clone()
}

#[cfg(test)]
pub(super) async fn test_descriptor(
    resources: &dyn SessionResources,
    session_id: &str,
    lifecycle: u64,
) -> Option<RecoveryDescriptor> {
    let inspection = inspect(
        resources,
        session_id,
        WorkSelector::RecoveryDescriptor { lifecycle },
    )
    .await
    .unwrap();
    descriptor(&inspection, lifecycle).unwrap().cloned()
}

#[cfg(test)]
pub(super) async fn test_delivery(
    resources: &dyn SessionResources,
    session_id: &str,
    delivery_id: &str,
) -> peri_acp_types::session_resources::work::Delivery {
    let inspection = inspect(
        resources,
        session_id,
        WorkSelector::Delivery {
            delivery_id: delivery_id.into(),
        },
    )
    .await
    .unwrap();
    let WorkPage::Deliveries(mut deliveries) = inspection.page else {
        panic!("delivery page expected")
    };
    assert_eq!(deliveries.len(), 1);
    let delivery = deliveries.pop().unwrap();
    assert_eq!(delivery.delivery_id, delivery_id);
    delivery
}
