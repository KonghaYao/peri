pub(crate) mod draft;
pub mod edit;
pub mod folder;
pub mod glob;
pub(crate) mod glob_patterns;
pub mod grep;
pub(crate) mod grep_args;
pub(crate) mod search_sink;
pub(crate) use search_sink as grep_format;
pub(crate) mod path_hints;
pub mod read;
pub(crate) mod transaction;
pub mod write;

use std::path::{Component, Path, PathBuf};

pub use edit::EditFileTool;
pub use folder::FolderOperationsTool;
pub use glob::GlobFilesTool;
pub use grep::GrepTool;
pub use read::ReadFileTool;
pub use write::WriteFileTool;

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[expect(
    clippy::module_inception,
    reason = "The style guide keeps the facade declarative while the implementation leaf groups related filesystem operations."
)]
mod filesystem;
pub use filesystem::parse_json_input;
pub use filesystem::resolve_path;
pub(crate) use filesystem::should_skip_dir;
pub(crate) use filesystem::SearchCancellation;
