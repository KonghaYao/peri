use super::*;
use peri_acp_types::session_resources::{
    control::decide_control, ControlCommand, ControlReceipt, ControlResolution, ControlState,
};

#[derive(Default)]
pub(super) struct TestControl {
    state: ControlState,
    receipts: HashMap<String, (String, ControlReceipt)>,
}

impl MockSessionResources {
    pub(super) fn read_control(&self, id: &ThreadId) -> ControlState {
        self.controls
            .lock()
            .unwrap()
            .entry(id.clone())
            .or_default()
            .state
            .clone()
    }

    pub(super) fn write_control(
        &self,
        command: &ControlCommand,
    ) -> SessionResourceResult<ControlReceipt> {
        self.ensure_writable()?;
        let digest = command.digest()?;
        let mut controls = self.controls.lock().unwrap();
        let control = controls.entry(command.session_id.clone()).or_default();
        if let Some((stored, receipt)) = control.receipts.get(&command.command_id) {
            if stored != &digest {
                return Err(SessionResourceError::conflict(
                    "control command identity reused",
                ));
            }
            return Ok(receipt.clone());
        }
        let receipt = decide_control(command, &control.state);
        control.state = receipt.state.clone();
        control
            .receipts
            .insert(command.command_id.clone(), (digest, receipt.clone()));
        Ok(receipt)
    }

    pub(super) fn resolve_control(
        &self,
        command: &ControlCommand,
    ) -> SessionResourceResult<ControlResolution> {
        let digest = command.digest()?;
        let controls = self.controls.lock().unwrap();
        let Some(control) = controls.get(&command.session_id) else {
            return Ok(ControlResolution::NotApplied);
        };
        let Some((stored, receipt)) = control.receipts.get(&command.command_id) else {
            return Ok(ControlResolution::NotApplied);
        };
        if stored != &digest {
            return Err(SessionResourceError::conflict(
                "control command identity reused",
            ));
        }
        Ok(ControlResolution::Applied {
            receipt: receipt.clone(),
        })
    }
}
