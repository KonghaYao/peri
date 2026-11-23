//! Bundled SEP-2640 skills served through `skills/list`, `skills/get` and
//! `resources/read`.
//!
//! The manifest and the resource bytes come from the same `include_bytes!`
//! constant, so the digest, the size and the bytes a client reads cannot drift.

use rmcp::model::{
    Resource, ResourceContents, Skill, SkillFrontmatter, SkillResource, SkillResources,
};

use crate::error::BrowserError;

/// URI root shared by every bundled skill.
pub const SKILL_ROOT: &str = "skill://peri-browser";
/// MIME type used for `SKILL.md`.
pub const SKILL_MIME: &str = "text/markdown";

struct Definition {
    name: &'static str,
    bytes: &'static [u8],
}

const DEFINITIONS: &[Definition] = &[
    Definition {
        name: "browser-automation",
        bytes: include_bytes!("../skills/browser-automation/SKILL.md"),
    },
    Definition {
        name: "debugging",
        bytes: include_bytes!("../skills/debugging/SKILL.md"),
    },
];

/// Parsed frontmatter of one bundled file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frontmatter {
    pub name: String,
    pub description: String,
}

/// Parses the leading `---` frontmatter block of a `SKILL.md`.
#[must_use]
pub fn parse_frontmatter(text: &str) -> Option<Frontmatter> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let block = &rest[..end];
    let mut name = None;
    let mut description = None;
    for line in block.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key.trim() {
            "name" => name = Some(value.to_owned()),
            "description" => description = Some(value.to_owned()),
            _ => {}
        }
    }
    Some(Frontmatter {
        name: name?,
        description: description?,
    })
}

/// The two skills shipped with this binary.
///
/// # Errors
///
/// Returns [`BrowserError::Internal`] if a bundled file loses its frontmatter;
/// the `skills_test` keeps that from reaching a user.
pub fn catalog() -> Result<Vec<Skill>, BrowserError> {
    DEFINITIONS
        .iter()
        .map(|definition| {
            let text = std::str::from_utf8(definition.bytes)
                .map_err(|error| BrowserError::Internal(format!("skill is not UTF-8: {error}")))?;
            let frontmatter = parse_frontmatter(text).ok_or_else(|| {
                BrowserError::Internal(format!(
                    "skill {} has no name/description frontmatter",
                    definition.name
                ))
            })?;
            if frontmatter.name != definition.name {
                return Err(BrowserError::Internal(format!(
                    "skill {} declares the name {}",
                    definition.name, frontmatter.name
                )));
            }
            let uri = skill_uri(definition.name);
            let files = vec![SkillResource::from_bytes(&uri, definition.bytes)];
            Ok(Skill::new(
                uri,
                SkillFrontmatter::new(frontmatter.name, frontmatter.description),
                SkillResources::Files(files),
            ))
        })
        .collect()
}

/// One skill by its `SKILL.md` URI.
///
/// # Errors
///
/// Returns [`BrowserError::ElementNotFound`] for an unknown URI.
pub fn skill(uri: &str) -> Result<Skill, BrowserError> {
    catalog()?
        .into_iter()
        .find(|skill| skill.uri == uri)
        .ok_or_else(|| BrowserError::ElementNotFound(uri.to_owned()))
}

/// Resources advertised by `resources/list`: every bundled `SKILL.md`.
///
/// # Errors
///
/// Propagates a frontmatter failure from [`catalog`].
pub fn resources() -> Result<Vec<Resource>, BrowserError> {
    catalog()?
        .into_iter()
        .map(|skill| {
            let SkillResources::Files(files) = &skill.resources else {
                return Err(BrowserError::Internal(
                    "bundled skills always have fixed resources".to_owned(),
                ));
            };
            let file = files
                .first()
                .ok_or_else(|| BrowserError::Internal("skill has no SKILL.md".to_owned()))?;
            Ok(
                Resource::new(file.uri.clone(), skill.frontmatter.name.clone())
                    .with_title(skill.frontmatter.description.clone())
                    .with_mime_type(SKILL_MIME)
                    .with_size(file.size),
            )
        })
        .collect()
}

/// Exact bytes served by `resources/read`.
///
/// # Errors
///
/// Returns [`BrowserError::ElementNotFound`] for a URI that is not a bundled
/// skill file.
pub fn read(uri: &str) -> Result<RawSkill, BrowserError> {
    let definition = DEFINITIONS
        .iter()
        .find(|definition| skill_uri(definition.name) == uri)
        .ok_or_else(|| BrowserError::ElementNotFound(uri.to_owned()))?;
    Ok(RawSkill {
        uri: uri.to_owned(),
        bytes: definition.bytes,
    })
}

/// Raw skill bytes and the URI they were requested under.
#[derive(Debug)]
pub struct RawSkill {
    pub uri: String,
    pub bytes: &'static [u8],
}

impl RawSkill {
    /// MCP text content for this file.
    ///
    /// # Errors
    ///
    /// Fails when the bundled bytes are not UTF-8.
    pub fn content(&self) -> Result<ResourceContents, BrowserError> {
        let text = std::str::from_utf8(self.bytes)
            .map_err(|error| BrowserError::Internal(format!("skill is not UTF-8: {error}")))?;
        Ok(ResourceContents::text(text, &self.uri).with_mime_type(SKILL_MIME))
    }
}

#[must_use]
pub fn skill_uri(name: &str) -> String {
    format!("{SKILL_ROOT}/{name}/SKILL.md")
}

/// Lowercase `sha256:` digest, used by tests and diagnostics.
#[must_use]
pub fn digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    use sha2::{Digest, Sha256};

    let hash = Sha256::digest(bytes);
    let mut digest = String::with_capacity(7 + 64);
    digest.push_str("sha256:");
    for byte in hash {
        write!(digest, "{byte:02x}").expect("writing to a String cannot fail");
    }
    digest
}

#[cfg(test)]
#[path = "skills_test.rs"]
mod tests;
