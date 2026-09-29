//! Model-facing failure projection: typed, allowlisted diagnostics only.

use crate::failure::ToolFailure;

pub fn failure_text(tool: &str, error: &(dyn std::error::Error + 'static)) -> String {
    let recovery = if let Some(error) = error.downcast_ref::<ToolFailure>() {
        error.recovery.as_str()
    } else if let Some(error) = error.downcast_ref::<std::io::Error>() {
        match error.kind() {
            std::io::ErrorKind::NotFound => "File not found. Verify the requested path.",
            std::io::ErrorKind::PermissionDenied => {
                "Permission denied. Check access permissions for the requested path."
            }
            std::io::ErrorKind::AlreadyExists => {
                "Target already exists. Read it before choosing a different target."
            }
            std::io::ErrorKind::InvalidData => {
                "Invalid file data. Check the file encoding and format."
            }
            _ => "File operation failed. Check access permissions and available disk space.",
        }
    } else {
        "the failure detail is withheld by policy (no paths, environment values, or credentials are exposed). Verify the input and retry."
    };
    format!("tool `{tool}` failed to execute; {recovery}")
}
