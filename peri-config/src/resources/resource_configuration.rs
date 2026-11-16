use super::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResourceConfiguration {
    pub disable_bundled_skills: bool,
}

#[must_use]
pub fn resolve(global: &Value) -> ResourceConfiguration {
    let disable_bundled_skills = global
        .get("config")
        .and_then(|config| config.get("disableBundledSkills"))
        .or_else(|| global.get("disableBundledSkills"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    ResourceConfiguration {
        disable_bundled_skills,
    }
}
