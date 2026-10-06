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
    session_id: &str,
    processing_id: &str,
) -> SessionResourceResult<TaskBinding> {
    let processing = processing(resources, session_id, processing_id).await?;
    let reference = processing
        .delegation
        .ok_or_else(|| SessionResourceError::conflict("processing delegation unavailable"))?;
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
