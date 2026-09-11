//! 主题来源与继承装配；继承使用主题名，根加载保留路径兼容性。

use std::collections::HashMap;
use std::path::PathBuf;

use super::ThemeLoadError;
use super::decode::decode_flat;
use super::resolve::{MAX_DEPTH, flatten_json_obj};
use crate::theme::ThemeDefinition;

pub(super) struct ThemeSources {
    user_dir: Option<PathBuf>,
}

impl ThemeSources {
    pub(super) fn from_environment() -> Self {
        Self::new(std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".peri/themes")))
    }

    pub(super) fn new(user_dir: Option<PathBuf>) -> Self {
        Self { user_dir }
    }

    pub(super) fn load(&self, name: &str) -> Result<ThemeDefinition, ThemeLoadError> {
        decode_flat(self.load_flat(name, &mut Vec::new())?)
    }

    fn load_flat(
        &self,
        name: &str,
        active: &mut Vec<String>,
    ) -> Result<HashMap<String, String>, ThemeLoadError> {
        if active.iter().any(|parent| parent == name) || active.len() > MAX_DEPTH {
            return Err(ThemeLoadError::CircularRef(format!(
                "theme inheritance: {} → {name}",
                active.join(" → ")
            )));
        }
        let json = self.read(name)?;
        active.push(name.to_string());
        let result = self.merge_document(&json, active);
        active.pop();
        result
    }

    fn merge_document(
        &self,
        json: &str,
        active: &mut Vec<String>,
    ) -> Result<HashMap<String, String>, ThemeLoadError> {
        let raw: serde_json::Value = serde_json::from_str(json)
            .map_err(|error| ThemeLoadError::ParseError(error.to_string()))?;
        let object = raw
            .as_object()
            .ok_or_else(|| ThemeLoadError::ParseError("theme must be a JSON object".to_string()))?;
        let mut merged = match object.get("extends") {
            None => HashMap::new(),
            Some(serde_json::Value::String(parent)) => {
                validate_name(parent)?;
                self.load_flat(parent, active)?
            }
            Some(_) => {
                return Err(ThemeLoadError::ParseError(
                    "extends must be a theme name string".to_string(),
                ));
            }
        };
        let mut child = HashMap::new();
        flatten_json_obj("", &raw, &mut child);
        child.remove("extends");
        // 显式默认子主题名称，避免继承父主题的身份；mode 则可以继承。
        child
            .entry("name".to_string())
            .or_insert_with(|| "unnamed".to_string());
        merged.extend(child);
        Ok(merged)
    }

    fn read(&self, name: &str) -> Result<String, ThemeLoadError> {
        if let Some(directory) = &self.user_dir {
            let path = directory.join(format!("{name}.json"));
            if path.exists() {
                return std::fs::read_to_string(path)
                    .map_err(|error| ThemeLoadError::ParseError(error.to_string()));
            }
        }
        match name {
            "peri-dark" | "dark" => Ok(include_str!("../../themes/dark.json").to_string()),
            "peri-light" | "light" => Ok(include_str!("../../themes/light.json").to_string()),
            _ => Err(ThemeLoadError::ThemeNotFound(name.to_string())),
        }
    }

    pub(super) fn list(&self) -> Vec<String> {
        let mut names = vec!["peri-dark".to_string(), "peri-light".to_string()];
        if let Some(directory) = &self.user_dir
            && let Ok(entries) = std::fs::read_dir(directory)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file()
                    && path
                        .extension()
                        .is_some_and(|extension| extension == "json")
                    && let Some(name) = path.file_stem()
                {
                    names.push(name.to_string_lossy().to_string());
                }
            }
        }
        names.sort();
        names.dedup();
        names
    }

    #[cfg(test)]
    pub(super) fn parse_document(&self, json: &str) -> Result<ThemeDefinition, ThemeLoadError> {
        decode_flat(self.merge_document(json, &mut Vec::new())?)
    }
}

fn validate_name(name: &str) -> Result<(), ThemeLoadError> {
    if name.trim().is_empty() || matches!(name, "." | "..") || name.contains(['/', '\\', ':']) {
        return Err(ThemeLoadError::ParseError(format!(
            "expected a theme name, not a path: {name}"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "source_test.rs"]
mod tests;
