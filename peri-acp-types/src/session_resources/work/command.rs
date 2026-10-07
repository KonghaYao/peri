use std::ops::Deref;
use std::sync::Arc;

use super::*;

#[cfg(test)]
std::thread_local! {
    static PREPARE_COUNTS: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

#[derive(Clone, Debug)]
pub struct PreparedWorkCommand {
    inner: Arc<PreparedWorkCommandInner>,
}

#[derive(Debug)]
struct PreparedWorkCommandInner {
    command: WorkCommand,
    encoded: Arc<str>,
    digest: String,
}

impl PreparedWorkCommand {
    pub fn try_new(command: WorkCommand) -> SessionResourceResult<Self> {
        let started = tracing::enabled!(
            target: "peri_acp_types::work_prepare",
            tracing::Level::DEBUG
        )
        .then(std::time::Instant::now);
        command.validate_identity()?;
        #[cfg(test)]
        PREPARE_COUNTS.with(|counts| {
            let (encodes, hashes) = counts.get();
            counts.set((encodes + 1, hashes));
        });
        let encoded =
            serde_json::to_string(&command).map_err(|_| invalid("invalid work command"))?;
        #[cfg(test)]
        PREPARE_COUNTS.with(|counts| {
            let (encodes, hashes) = counts.get();
            counts.set((encodes, hashes + 1));
        });
        let digest = format!("{:x}", Sha256::digest(encoded.as_bytes()));
        let prepared = Self {
            inner: Arc::new(PreparedWorkCommandInner {
                command,
                encoded: encoded.into(),
                digest,
            }),
        };
        if let Some(started) = started {
            tracing::debug!(
                target: "peri_acp_types::work_prepare",
                session_id = %prepared.session_id,
                mutation_id = %prepared.mutation_id,
                command_bytes = prepared.encoded().len(),
                command_encode_count = 1_u64,
                command_hash_count = 1_u64,
                elapsed_wall_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                "work command prepared"
            );
        }
        Ok(prepared)
    }

    pub fn command(&self) -> &WorkCommand {
        &self.inner.command
    }

    pub fn encoded(&self) -> &Arc<str> {
        &self.inner.encoded
    }

    pub fn digest(&self) -> &str {
        &self.inner.digest
    }

    pub fn into_command(self) -> WorkCommand {
        match Arc::try_unwrap(self.inner) {
            Ok(inner) => inner.command,
            Err(inner) => inner.command.clone(),
        }
    }
}

impl Deref for PreparedWorkCommand {
    type Target = WorkCommand;

    fn deref(&self) -> &Self::Target {
        self.command()
    }
}

impl PartialEq for PreparedWorkCommand {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner) || self.command() == other.command()
    }
}

impl Eq for PreparedWorkCommand {}

#[cfg(test)]
#[path = "command_test.rs"]
mod tests;
