use crate::error::{AgmError, Result};
use crate::resolver::PackageType;
use crate::types::{DependencySpec, PackageManifest};
use std::path::Path;

/// Filter a list of (name, glob) items according to pick/omit patterns.
/// Matching is done against both the item name and its glob path.
pub fn filter_items(
    items: &[(String, String)],
    spec: &DependencySpec,
) -> Result<Vec<(String, String)>> {
    let (pick_patterns, omit_patterns) = match spec {
        DependencySpec::Simple(_) => return Ok(items.to_vec()),
        DependencySpec::Detailed { pick, omit, .. } => (pick, omit),
    };

    let pick_compiled = compile_patterns(pick_patterns)?;
    let omit_compiled = compile_patterns(omit_patterns)?;

    let mut result = Vec::new();
    for (name, glob) in items {
        let matched_pick = pick_compiled.is_empty()
            || pick_compiled
                .iter()
                .any(|p| p.matches(name) || p.matches(glob));
        let matched_omit = omit_compiled
            .iter()
            .any(|p| p.matches(name) || p.matches(glob));

        if matched_pick && !matched_omit {
            result.push((name.clone(), glob.clone()));
        }
    }

    Ok(result)
}

fn compile_patterns(patterns: &[String]) -> Result<Vec<glob::Pattern>> {
    patterns
        .iter()
        .map(|p| {
            glob::Pattern::new(p).map_err(|e| AgmError::InvalidGlobPattern {
                pattern: p.clone(),
                reason: e.to_string(),
            })
        })
        .collect()
}

/// Auto-detection results: (skills, agents, mcp), each element is a (name, glob) pair, glob is relative path in store
pub type DetectedTypes = (
    Vec<(String, String)>,
    Vec<(String, String)>,
    Vec<(String, String)>,
);

/// Resolve the scan-root directories from an optional base glob pattern.
///
/// `base` is relative to `repo_root` and may contain glob wildcards (e.g.
/// `plugins/*`, `plugins/kit-core`). When omitted, the repo root itself is used.
fn resolve_scan_roots(repo_root: &Path, base: Option<&str>) -> Result<Vec<std::path::PathBuf>> {
    let Some(pattern) = base else {
        return Ok(vec![repo_root.to_path_buf()]);
    };

    let full_pattern = repo_root.join(pattern);
    // glob patterns must use '/' as the separator even on Windows.
    let pattern_str = full_pattern.to_string_lossy().replace('\\', "/");
    let entries = glob::glob(&pattern_str).map_err(|e| AgmError::InvalidGlobPattern {
        pattern: pattern.into(),
        reason: e.to_string(),
    })?;

    let roots: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|p| p.is_dir())
        .collect();

    Ok(roots)
}

/// Auto-detect skills, agents, and mcp in a repo (when no agm.package.json)
/// Returns (name, glob) pairs, glob is relative path in repo_root.
///
/// `base` is an optional directory or glob pattern relative to repo_root where discovery
/// should start. It only affects auto-discovery; explicit agm.package.json exports ignore it.
pub fn auto_detect_types(repo_root: &Path, base: Option<&str>) -> Result<DetectedTypes> {
    let scan_roots = resolve_scan_roots(repo_root, base)?;

    let mut skills: Vec<(String, String)> = Vec::new();
    let mut agents: Vec<(String, String)> = Vec::new();

    for scan_root in &scan_roots {
        // Detect .{tool}/skills/**/SKILL.md (supports nested categories via recursion)
        for tool_prefix in &[".claude", ""] {
            let skills_dir = if tool_prefix.is_empty() {
                scan_root.join("skills")
            } else {
                scan_root.join(tool_prefix).join("skills")
            };
            skills.extend(find_skills_recursive(&skills_dir, repo_root));

            // Detect .{tool}/agents/*.md
            let agents_dir = if tool_prefix.is_empty() {
                scan_root.join("agents")
            } else {
                scan_root.join(tool_prefix).join("agents")
            };
            if let Ok(entries) = std::fs::read_dir(&agents_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && path.extension().is_some_and(|e| e == "md") {
                        let name = path.file_stem().unwrap().to_string_lossy().to_string();
                        let prefix_components: Vec<_> = path
                            .strip_prefix(repo_root)
                            .unwrap_or(&path)
                            .parent()
                            .unwrap_or(Path::new(""))
                            .iter()
                            .map(|c| c.to_string_lossy().to_string())
                            .collect();
                        let glob = if prefix_components.is_empty() {
                            format!("agents/{}.md", name)
                        } else {
                            format!("{}/agents/{}.md", prefix_components.join("/"), name)
                        };
                        tracing::info!("auto-detected agent: {} ({})", name, glob);
                        agents.push((name, glob));
                    }
                }
            }
        }
    }

    Ok((skills, agents, Vec::new()))
}

/// Detect the items exported by a package in the store for a given package type.
/// Returns (name, glob) pairs, where glob is relative to the store path.
///
/// `base` is an optional directory relative to the package root where auto-discovery
/// should start. It is ignored when an explicit `agm.package.json` is present.
pub fn detect_package_items(
    store_path: &Path,
    typ: PackageType,
    package_name: &str,
    base: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let pkg_manifest_path = store_path.join("agm.package.json");
    if pkg_manifest_path.exists() {
        let pkg = PackageManifest::load(&pkg_manifest_path)?;
        match typ {
            PackageType::Skills => Ok(pkg
                .skills
                .into_iter()
                .map(|g| (extract_skill_name(&g), g))
                .collect()),
            PackageType::Agents => Ok(pkg
                .agents
                .into_iter()
                .map(|g| (extract_skill_name(&g), g))
                .collect()),
            PackageType::Mcp => Ok(pkg
                .mcp
                .into_iter()
                .map(|g| (extract_skill_name(&g), g))
                .collect()),
        }
    } else {
        let (detected_skills, detected_agents, _) = auto_detect_types(store_path, base)?;
        let detected = match typ {
            PackageType::Skills => detected_skills,
            PackageType::Agents => detected_agents,
            PackageType::Mcp => Vec::new(),
        };
        if detected.is_empty() {
            Ok(vec![(package_name.into(), ".".into())])
        } else {
            Ok(detected)
        }
    }
}

/// Recursively find directories containing SKILL.md, supporting nested categories
/// (e.g., skills/engineering/grill-me/SKILL.md)
/// Returns (skill_name, path relative to repo_root)
fn find_skills_recursive(base_dir: &Path, repo_root: &Path) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let entries = match std::fs::read_dir(base_dir) {
        Ok(e) => e,
        Err(_) => return result,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if skill_md.exists() {
            let name = path
                .file_name()
                .expect("path must have a filename")
                .to_string_lossy()
                .to_string();
            let rel = skill_md.strip_prefix(repo_root).unwrap_or(&skill_md);
            let glob = rel.to_string_lossy().replace('\\', "/");
            tracing::info!("auto-detected skill: {} ({})", name, glob);
            result.push((name, glob));
        } else {
            // Recurse into subdirectories (e.g., skills/engineering/, skills/productivity/)
            result.extend(find_skills_recursive(&path, repo_root));
        }
    }
    result
}

/// Extract skill/agent name from a glob path (e.g., ".claude/skills/interview/SKILL.md" → "interview")
pub fn extract_skill_name(glob: &str) -> String {
    let parts: Vec<&str> = glob.split('/').collect();
    // Find the part after "skills" or "agents"
    for (i, part) in parts.iter().enumerate() {
        if (*part == "skills" || *part == "agents" || *part == "mcp") && i + 1 < parts.len() {
            return parts[i + 1].to_string();
        }
    }
    // fallback: use the last meaningful directory name
    parts
        .iter()
        .rev()
        .find(|p| !p.ends_with(".md") && **p != "SKILL.md")
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown".into())
}
