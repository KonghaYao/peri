use peri_acp_types::session_resources::work::*;
use peri_acp_types::store::{
    deserialize_persisted_payload, serialize_persisted_payload, PersistedPayload,
};

use super::work_pipeline::WorkSession;

impl WorkSession {
    pub(crate) async fn inspect_head(&self) -> anyhow::Result<WorkInspection> {
        Ok(self
            .ledger
            .inspect(&WorkQuery::new(
                &self.admission.session_id,
                WorkSelector::Head,
            ))
            .await?)
    }

    pub(crate) async fn processing(&self, processing_id: &str) -> anyhow::Result<Processing> {
        let inspection = self
            .ledger
            .inspect(&WorkQuery::new(
                &self.admission.session_id,
                WorkSelector::Processing {
                    processing_id: processing_id.into(),
                },
            ))
            .await?;
        let WorkPage::Processings(mut records) = inspection.page else {
            return Err(anyhow::anyhow!(
                "processing query returned a different page"
            ));
        };
        let record = records
            .pop()
            .filter(|record| record.processing_id == processing_id)
            .ok_or_else(|| anyhow::anyhow!("durable processing is missing"))?;
        if !records.is_empty() || record.recipient_lifecycle != self.admission.lifecycle {
            return Err(anyhow::anyhow!(
                "processing identity or lifecycle differs from SDK admission"
            ));
        }
        Ok(record)
    }

    pub(crate) async fn effects(&self, processing: &Processing) -> anyhow::Result<Vec<Effect>> {
        let mut query = WorkQuery::new(
            &self.admission.session_id,
            WorkSelector::Effects {
                processing_id: processing.processing_id.clone(),
                phase_sequence: Some(processing.phase_sequence),
            },
        );
        let mut effects = Vec::new();
        loop {
            let inspection = self.ledger.inspect(&query).await?;
            let WorkPage::Effects(page) = inspection.page else {
                return Err(anyhow::anyhow!("effect query returned a different page"));
            };
            effects.extend(page);
            let Some(cursor) = inspection.next_cursor else {
                break;
            };
            if query.cursor.as_ref() == Some(&cursor) {
                return Err(anyhow::anyhow!("effect pagination failed to advance"));
            }
            query.cursor = Some(cursor);
        }
        Ok(effects)
    }

    pub(crate) async fn deliveries(&self, processing_id: &str) -> anyhow::Result<Vec<Delivery>> {
        let mut query = WorkQuery::new(
            &self.admission.session_id,
            WorkSelector::ProcessingDeliveries {
                processing_id: processing_id.into(),
            },
        );
        let mut deliveries = Vec::new();
        loop {
            let inspection = self.ledger.inspect(&query).await?;
            let WorkPage::Deliveries(page) = inspection.page else {
                return Err(anyhow::anyhow!("delivery query returned a different page"));
            };
            deliveries.extend(page);
            let Some(cursor) = inspection.next_cursor else {
                break;
            };
            if query.cursor.as_ref() == Some(&cursor) {
                return Err(anyhow::anyhow!("delivery pagination failed to advance"));
            }
            query.cursor = Some(cursor);
        }
        Ok(deliveries)
    }

    pub(crate) async fn evidence(&self, reference: &PayloadRef) -> anyhow::Result<Vec<u8>> {
        let evidence = self
            .ledger
            .read_evidence(&EvidenceQuery {
                session_id: self.admission.session_id.clone(),
                reference: reference.clone(),
            })
            .await?;
        evidence.validate()?;
        if evidence.reference != *reference {
            return Err(anyhow::anyhow!(
                "evidence identity differs from requested reference"
            ));
        }
        Ok(evidence.bytes)
    }

    pub(crate) async fn payload(&self, payload: &WorkPayload) -> anyhow::Result<PersistedPayload> {
        payload.validate()?;
        let bytes = self.evidence(&payload.content).await?;
        let serialized = std::str::from_utf8(&bytes)?;
        let persisted = deserialize_persisted_payload(serialized)?;
        if persisted.id() != payload.message_id {
            return Err(anyhow::anyhow!(
                "message evidence identity differs from canonical envelope"
            ));
        }
        Ok(persisted)
    }

    pub(crate) async fn prepare_payload(
        &self,
        payload: &PersistedPayload,
    ) -> anyhow::Result<WorkPayload> {
        prepare_payload(
            self.ledger.resources().as_ref(),
            &self.admission.session_id,
            payload,
        )
        .await
    }
}

pub(crate) async fn prepare_payload(
    resources: &dyn peri_acp_types::session_resources::SessionResources,
    session_id: &str,
    payload: &PersistedPayload,
) -> anyhow::Result<WorkPayload> {
    let serialized = serialize_persisted_payload(payload)?;
    let content = prepare_evidence(resources, session_id, serialized.into_bytes()).await?;
    let role = match payload {
        PersistedPayload::Message(peri_acp_types::messages::BaseMessage::Human { .. }) => {
            "user".into()
        }
        PersistedPayload::Message(peri_acp_types::messages::BaseMessage::Ai { .. }) => {
            "assistant".into()
        }
        PersistedPayload::Message(peri_acp_types::messages::BaseMessage::Tool { .. }) => {
            "tool".into()
        }
        PersistedPayload::Message(peri_acp_types::messages::BaseMessage::System { .. }) => {
            "system".into()
        }
        PersistedPayload::SystemReminder { .. } => "system_reminder".into(),
    };
    let tool_call_id = match payload {
        PersistedPayload::Message(peri_acp_types::messages::BaseMessage::Tool {
            tool_call_id,
            ..
        }) => Some(tool_call_id.clone()),
        _ => None,
    };
    Ok(WorkPayload {
        message_id: payload.id(),
        role,
        content,
        tool_call_id,
    })
}

pub(crate) async fn prepare_evidence(
    resources: &dyn peri_acp_types::session_resources::SessionResources,
    session_id: &str,
    bytes: Vec<u8>,
) -> anyhow::Result<PayloadRef> {
    use sha2::{Digest, Sha256};
    let payload_id = format!("{:x}", Sha256::digest(&bytes));
    Ok(resources
        .prepare_evidence(&EvidenceWrite {
            session_id: session_id.into(),
            storage_scope: session_id.into(),
            payload_id,
            encoding: 1,
            bytes,
        })
        .await?)
}
