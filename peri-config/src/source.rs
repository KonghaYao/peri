use std::{collections::BTreeMap, io, path::Path};

use crate::{ConfigurationInputs, ConfigurationScope};

pub fn read_environment(names: &[&str]) -> io::Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for name in names {
        if let Some(value) = crate::io::read_environment(name)? {
            values.insert((*name).to_owned(), value);
        }
    }
    Ok(values)
}

pub trait ConfigurationSource: Send + Sync {
    fn collect(&self, scope: &ConfigurationScope) -> io::Result<ConfigurationInputs>;

    fn write_if_unchanged(
        &self,
        path: &Path,
        expected: Option<&str>,
        content: &str,
    ) -> io::Result<bool>;
}

#[derive(Debug, Default)]
pub struct McpConfigurationSource;

impl ConfigurationSource for McpConfigurationSource {
    fn collect(&self, scope: &ConfigurationScope) -> io::Result<ConfigurationInputs> {
        let workspace_path = crate::assembly::workspace_settings_path(&scope.cwd);
        let workspace = if crate::io::same_file(&workspace_path, &scope.global_settings)? {
            None
        } else {
            Some(workspace_path)
        };
        collect_with_layout(
            scope,
            workspace.as_deref(),
            Some(&crate::assembly::project_mcp_path(&scope.cwd)),
        )
    }

    fn write_if_unchanged(
        &self,
        path: &Path,
        expected: Option<&str>,
        content: &str,
    ) -> io::Result<bool> {
        crate::io::write_text_if_unchanged(path, &expected.map(str::to_owned), content)
    }
}

pub(crate) fn collect_with_layout(
    scope: &ConfigurationScope,
    workspace: Option<&Path>,
    project: Option<&Path>,
) -> io::Result<ConfigurationInputs> {
    collect_with_layout_and_global(scope, workspace, project, None)
}

pub(crate) fn collect_with_layout_and_global(
    scope: &ConfigurationScope,
    workspace: Option<&Path>,
    project: Option<&Path>,
    injected_global: Option<&str>,
) -> io::Result<ConfigurationInputs> {
    let global = match injected_global {
        Some(content) => Some(content.to_owned()),
        None => read_optional(&scope.global_settings)?,
    };
    let workspace = workspace.map(read_optional).transpose()?.flatten();
    let project = project.map(read_optional).transpose()?.flatten();
    let keys = crate::assembly::environment_keys();
    Ok(ConfigurationInputs {
        global,
        workspace,
        project,
        environment: read_environment(&keys)?,
    })
}

pub(crate) fn read_optional(path: &Path) -> io::Result<Option<String>> {
    match crate::io::read_text(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
