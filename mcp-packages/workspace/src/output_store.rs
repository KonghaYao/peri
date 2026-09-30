use std::{collections::HashMap, io::Write, path::PathBuf};

use parking_lot::Mutex;
use peri_acp_types::workspace_output::{StoreOutputRequest, StoredOutput};
use rmcp::{model::CustomResult, ErrorData as McpError};
use uuid::Uuid;

pub(crate) struct OutputStore {
    authority: Uuid,
    directory: PathBuf,
    artifacts: Mutex<HashMap<String, PathBuf>>,
}

impl OutputStore {
    pub(crate) fn new() -> Self {
        let authority = Uuid::new_v4();
        Self {
            authority,
            directory: std::env::temp_dir().join(format!("peri-mcp-output-{authority}")),
            artifacts: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn store(&self, request: StoreOutputRequest) -> Result<CustomResult, McpError> {
        self.write(request)
            .map(CustomResult::new)
            .map_err(|_| McpError::internal_error("workspace output persistence failed", None))
    }

    fn write(&self, request: StoreOutputRequest) -> Result<serde_json::Value, std::io::Error> {
        let mut artifacts = self.artifacts.lock();
        if artifacts.is_empty() && !self.directory.exists() {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&self.directory)?;
        }
        let artifact = Uuid::new_v4();
        let file_path = self.directory.join(format!("{artifact}.txt"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&file_path)?;
        if let Err(error) = file
            .write_all(request.content.as_bytes())
            .and_then(|_| file.sync_all())
        {
            drop(file);
            let _ = std::fs::remove_file(&file_path);
            return Err(error);
        }
        let uri = format!("peri-output://{}/{artifact}", self.authority);
        let output = StoredOutput {
            uri: uri.clone(),
            path: file_path.to_string_lossy().into_owned(),
            byte_length: request.content.len() as u64,
        };
        let value = serde_json::to_value(output)?;
        artifacts.insert(uri, file_path);
        Ok(value)
    }

    pub(crate) fn read(&self, uri: &str) -> Result<String, McpError> {
        let file_path =
            self.artifacts.lock().get(uri).cloned().ok_or_else(|| {
                McpError::invalid_params("unknown workspace output resource", None)
            })?;
        std::fs::read_to_string(file_path)
            .map_err(|_| McpError::internal_error("workspace output read failed", None))
    }
}

#[cfg(test)]
#[path = "output_store_test.rs"]
mod tests;
