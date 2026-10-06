use peri_acp_types::session_resources::{
    work::*, SessionResourceError, SessionResourceResult, SessionResources,
};

pub(crate) async fn inspect(
    resources: &dyn SessionResources,
    session_id: &str,
    selector: WorkSelector,
) -> SessionResourceResult<WorkInspection> {
    let inspection = resources
        .inspect_work(&WorkQuery::new(session_id, selector))
        .await?;
    if inspection.session_id != session_id {
        return Err(SessionResourceError::conflict(
            "inspection session identity mismatch",
        ));
    }
    Ok(inspection)
}

#[cfg(test)]
pub(crate) async fn processing(
    resources: &dyn SessionResources,
    session_id: &str,
    processing_id: &str,
) -> SessionResourceResult<Processing> {
    let inspection = inspect(
        resources,
        session_id,
        WorkSelector::Processing {
            processing_id: processing_id.into(),
        },
    )
    .await?;
    let WorkPage::Processings(mut records) = inspection.page else {
        return Err(SessionResourceError::conflict(
            "processing lookup returned a different page",
        ));
    };
    records
        .pop()
        .filter(|record| record.processing_id == processing_id)
        .ok_or_else(|| SessionResourceError::conflict("processing identity unavailable"))
}

pub(crate) async fn effect(
    resources: &dyn SessionResources,
    session_id: &str,
    invocation_id: &str,
) -> SessionResourceResult<Effect> {
    let inspection = inspect(
        resources,
        session_id,
        WorkSelector::Effect {
            invocation_id: invocation_id.into(),
        },
    )
    .await?;
    let WorkPage::Effects(mut records) = inspection.page else {
        return Err(SessionResourceError::conflict(
            "effect lookup returned a different page",
        ));
    };
    records
        .pop()
        .filter(|record| record.invocation_id == invocation_id)
        .ok_or_else(|| SessionResourceError::conflict("effect identity unavailable"))
}

pub(crate) async fn admission(
    resources: &dyn SessionResources,
    admission: &WorkAdmission,
) -> SessionResourceResult<AdmissionRecord> {
    let inspection = inspect(
        resources,
        &admission.session_id,
        WorkSelector::Admission {
            admission_id: admission.admission_id.clone(),
        },
    )
    .await?;
    let WorkPage::Admissions(mut records) = inspection.page else {
        return Err(SessionResourceError::conflict(
            "admission lookup returned a different page",
        ));
    };
    records
        .pop()
        .filter(|record| record.admission == *admission)
        .ok_or_else(|| SessionResourceError::conflict("exact SDK admission unavailable"))
}

pub(crate) async fn descriptor(
    resources: &dyn SessionResources,
    session_id: &str,
    lifecycle: u64,
) -> SessionResourceResult<RecoveryDescriptor> {
    let inspection = inspect(
        resources,
        session_id,
        WorkSelector::RecoveryDescriptor { lifecycle },
    )
    .await?;
    let WorkPage::RecoveryDescriptors(mut records) = inspection.page else {
        return Err(SessionResourceError::conflict(
            "recovery lookup returned a different page",
        ));
    };
    records
        .pop()
        .filter(|record| record.recipient_lifecycle == lifecycle)
        .ok_or_else(|| SessionResourceError::conflict("recovery descriptor unavailable"))
}

pub(crate) async fn terminal(
    resources: &dyn SessionResources,
    admission: &WorkAdmission,
) -> SessionResourceResult<Option<TerminalObligation>> {
    let inspection = inspect(
        resources,
        &admission.session_id,
        WorkSelector::TerminalCommand {
            admission_id: admission.admission_id.clone(),
        },
    )
    .await?;
    let WorkPage::TerminalCommands(mut records) = inspection.page else {
        return Err(SessionResourceError::conflict(
            "terminal lookup returned a different page",
        ));
    };
    Ok(records.pop())
}

pub(crate) async fn delegation(
    resources: &dyn SessionResources,
    execution_admission: &WorkAdmission,
) -> SessionResourceResult<TaskBinding> {
    let reference = delegation_reference(resources, execution_admission).await?;
    let effect = effect(
        resources,
        &reference.parent_session_id,
        &reference.delegation_id,
    )
    .await?;
    let binding = effect
        .binding
        .ok_or_else(|| SessionResourceError::conflict("immutable task binding unavailable"))?;
    if binding.recipient_lifecycle != reference.parent_lifecycle {
        return Err(SessionResourceError::conflict(
            "delegation lifecycle mismatch",
        ));
    }
    Ok(binding)
}

async fn delegation_reference(
    resources: &dyn SessionResources,
    execution_admission: &WorkAdmission,
) -> SessionResourceResult<DelegationRef> {
    let session_id = execution_admission.session_id.as_str();
    let processing_id = execution_admission.work_id.as_str();
    let inspection = inspect(
        resources,
        session_id,
        WorkSelector::Processing {
            processing_id: processing_id.into(),
        },
    )
    .await?;
    let WorkPage::Processings(records) = inspection.page else {
        return Err(SessionResourceError::conflict(
            "delegation processing lookup returned a different page",
        ));
    };
    if !records.is_empty() {
        if records.len() != 1
            || records[0].processing_id != processing_id
            || records[0].recipient_lifecycle != execution_admission.lifecycle
        {
            return Err(SessionResourceError::conflict(
                "delegation processing identity mismatch",
            ));
        }
        return records[0]
            .delegation
            .clone()
            .ok_or_else(|| SessionResourceError::conflict("processing delegation unavailable"));
    }
    let registration = admission(resources, execution_admission).await?;
    if registration.leaving_evidence_id.is_some() {
        return Err(SessionResourceError::conflict(
            "preclaim delegation admission is inactive",
        ));
    }
    let delivery_ids = registration
        .initial_delivery_ids
        .as_ref()
        .filter(|delivery_ids| {
            !delivery_ids.is_empty() && delivery_ids.len() <= MAX_WORK_PAGE_SIZE as usize
        })
        .ok_or_else(|| {
            SessionResourceError::conflict(
                "preclaim delegation has no bounded frozen delivery membership",
            )
        })?;
    let mut reference = None;
    for delivery_id in delivery_ids {
        let inspection = inspect(
            resources,
            session_id,
            WorkSelector::Delivery {
                delivery_id: delivery_id.clone(),
            },
        )
        .await?;
        let WorkPage::Deliveries(records) = inspection.page else {
            return Err(SessionResourceError::conflict(
                "delegation delivery lookup returned a different page",
            ));
        };
        let delivery = records.first().ok_or_else(|| {
            SessionResourceError::conflict("delegation candidate delivery unavailable")
        })?;
        if records.len() != 1
            || delivery.delivery_id != *delivery_id
            || delivery.recipient_lifecycle != execution_admission.lifecycle
            || delivery.processing_id.is_some()
        {
            return Err(SessionResourceError::conflict(
                "delegation candidate delivery identity mismatch",
            ));
        }
        let current = delivery
            .delegation
            .as_ref()
            .ok_or_else(|| SessionResourceError::conflict("candidate delegation unavailable"))?;
        if reference.as_ref().is_some_and(|prior| prior != current) {
            return Err(SessionResourceError::conflict(
                "candidate delegation references differ",
            ));
        }
        reference = Some(current.clone());
    }
    reference.ok_or_else(|| SessionResourceError::conflict("candidate delegation unavailable"))
}
