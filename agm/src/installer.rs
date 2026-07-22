use crate::adapter::*;
use crate::config::AgmConfig;
use crate::error::{AgmError, Result};
use crate::filter::{auto_detect_types, detect_package_items, extract_skill_name, filter_items};
use crate::fs_util::remove_symlink_or_dir;
use crate::git;
use crate::registry::RegistryClient;
use crate::resolver::*;
use crate::store::*;
use crate::types::DependencySpec;
use crate::types::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::runtime::Runtime;

/// Installation context
pub struct InstallContext {
    pub config: AgmConfig,
    pub store: Store,
    pub manifest: ProjectManifest,
    pub lock: Option<LockFile>,
    pub target: String,
    pub project_root: PathBuf,
}

impl InstallContext {
    pub fn new(
        config: AgmConfig,
        manifest: ProjectManifest,
        target: &str,
        project_root: PathBuf,
    ) -> Result<Self> {
        let store = Store::new(config.store_path.clone());
        store.ensure_root()?;

        // Ensure agm temp directory exists (same filesystem as store, for atomic rename)
        let tmp_dir = crate::config::agm_dir().join("tmp");
        std::fs::create_dir_all(&tmp_dir)?;

        let lock_path = project_root.join("agm.lock.json");
        let lock = if lock_path.exists() {
            Some(LockFile::load(&lock_path)?)
        } else {
            None
        };

        Ok(Self {
            config,
            store,
            manifest,
            lock,
            target: target.to_string(),
            project_root,
        })
    }

    /// Create temp directory under ~/.agm/tmp/, ensuring same filesystem as store
    fn temp_dir(&self) -> Result<tempfile::TempDir> {
        let tmp_root = crate::config::agm_dir().join("tmp");
        std::fs::create_dir_all(&tmp_root)?;
        Ok(tempfile::TempDir::new_in(&tmp_root)?)
    }

    /// Install a package directly from git URL (similar to npm install <url>)
    pub fn install_from_git(&mut self, repo_url: &str) -> Result<()> {
        let adapter = get_adapter(&self.target)
            .ok_or_else(|| AgmError::Other(format!("unknown target: {}", self.target)))?;

        // Parse URL → package name
        let (owner, repo) = git::parse_github_url(repo_url)
            .ok_or_else(|| AgmError::Other(format!("unsupported git URL: {}", repo_url)))?;
        let pkg_name = format!("@git/{}/{}", owner, repo);

        // Use ls-remote to get HEAD hash, check if store already has it
        let head_commit = git::resolve_head(repo_url)?;
        let store_path = self.store.git_package_path(repo_url, &head_commit);

        // Resolve optional discovery base from the manifest dependency spec.
        let base = self
            .manifest
            .skills
            .get(&pkg_name)
            .or_else(|| self.manifest.agents.get(&pkg_name))
            .or_else(|| self.manifest.mcp.get(&pkg_name))
            .and_then(|spec| spec.base());

        let (skills, agents, mcp, store_path, actual_commit, resolution) = if store_path.exists() {
            println!("  (already in store, skipping clone)");
            let pkg_manifest_path = store_path.join("agm.package.json");
            let (skills, agents, mcp) = if pkg_manifest_path.exists() {
                let pkg = PackageManifest::load(&pkg_manifest_path)?;
                let skills: Vec<_> = pkg
                    .skills
                    .into_iter()
                    .map(|g| {
                        let n = extract_skill_name(&g);
                        (n, g)
                    })
                    .collect();
                let agents: Vec<_> = pkg
                    .agents
                    .into_iter()
                    .map(|g| {
                        let n = extract_skill_name(&g);
                        (n, g)
                    })
                    .collect();
                let mcp: Vec<_> = pkg
                    .mcp
                    .into_iter()
                    .map(|g| {
                        let n = extract_skill_name(&g);
                        (n, g)
                    })
                    .collect();
                (skills, agents, mcp)
            } else {
                let (skills, agents, _) = auto_detect_types(&store_path, base)?;
                (skills, agents, Vec::new())
            };
            let resolution = Resolution::Git {
                repo: repo_url.to_string(),
                commit: head_commit.clone(),
            };
            (
                skills,
                agents,
                mcp,
                store_path,
                head_commit.clone(),
                resolution,
            )
        } else {
            let temp_dir = self.temp_dir()?;
            let cloned_commit = git::clone_head(repo_url, temp_dir.path())?;
            tracing::info!("cloned {} at commit {}", repo_url, &cloned_commit[..12]);

            if cloned_commit != head_commit {
                tracing::warn!(
                    "HEAD changed during clone (expected {}, got {})",
                    &head_commit[..12],
                    &cloned_commit[..12]
                );
            }

            let pkg_manifest_path = temp_dir.path().join("agm.package.json");
            let (skills, agents, mcp) = if pkg_manifest_path.exists() {
                let pkg = PackageManifest::load(&pkg_manifest_path)?;
                let skills: Vec<_> = pkg
                    .skills
                    .into_iter()
                    .map(|g| {
                        let n = extract_skill_name(&g);
                        (n, g)
                    })
                    .collect();
                let agents: Vec<_> = pkg
                    .agents
                    .into_iter()
                    .map(|g| {
                        let n = extract_skill_name(&g);
                        (n, g)
                    })
                    .collect();
                let mcp: Vec<_> = pkg
                    .mcp
                    .into_iter()
                    .map(|g| {
                        let n = extract_skill_name(&g);
                        (n, g)
                    })
                    .collect();
                (skills, agents, mcp)
            } else {
                let (skills, agents, _) = auto_detect_types(temp_dir.path(), base)?;
                (skills, agents, Vec::new())
            };

            let resolution = Resolution::Git {
                repo: repo_url.to_string(),
                commit: cloned_commit.clone(),
            };
            let store_path = install_to_store(
                &self.store,
                temp_dir.path(),
                &resolution,
                &pkg_name,
                &cloned_commit,
            )?;
            let _ = temp_dir.close();
            (skills, agents, mcp, store_path, cloned_commit, resolution)
        };

        let final_spec = self
            .manifest
            .skills
            .get(&pkg_name)
            .or_else(|| self.manifest.agents.get(&pkg_name))
            .or_else(|| self.manifest.mcp.get(&pkg_name))
            .cloned()
            .unwrap_or_else(|| DependencySpec::Simple(actual_commit.clone()));

        let skills = filter_items(&skills, &final_spec)?;
        let agents = filter_items(&agents, &final_spec)?;
        let mcp = filter_items(&mcp, &final_spec)?;

        if skills.is_empty()
            && agents.is_empty()
            && mcp.is_empty()
            && !matches!(final_spec, DependencySpec::Simple(_))
        {
            println!(
                "  (no skills/agents/mcp matched pick/omit filters for {})",
                pkg_name
            );
        }

        let mut installed = Vec::new();

        // Remove stale symlinks from a previous install of this package
        let skills_target = adapter.map_dir(PackageType::Skills, &self.project_root);
        remove_package_symlinks(&skills_target, &store_path)?;
        let agents_target = adapter.map_dir(PackageType::Agents, &self.project_root);
        remove_package_symlinks(&agents_target, &store_path)?;
        let mcp_target = adapter.map_dir(PackageType::Mcp, &self.project_root);
        remove_package_symlinks(&mcp_target, &store_path)?;

        // Create symlinks for skills
        for (skill_name, skill_glob) in &skills {
            let target_dir = adapter.map_dir(PackageType::Skills, &self.project_root);
            let link_name = symlink_name(skill_name, &[]);
            let store_skill_path = store_path.join(skill_glob);
            if store_skill_path.exists() {
                let parent = store_skill_path.parent().unwrap_or(&store_skill_path);
                adapter
                    .install(parent, &target_dir, &link_name)
                    .map_err(|e| AgmError::Other(format!("symlink skill {}: {}", skill_name, e)))?;
                println!(
                    "  ✓ skill: {} → .{}/skills/{}",
                    skill_name, self.target, link_name
                );
            }
        }

        // Create symlinks for agents
        for (agent_name, agent_glob) in &agents {
            let target_dir = adapter.map_dir(PackageType::Agents, &self.project_root);
            let link_name = symlink_name(agent_name, &[]);
            let store_agent_path = store_path.join(agent_glob);
            if store_agent_path.exists() {
                adapter
                    .install(&store_agent_path, &target_dir, &link_name)
                    .map_err(|e| AgmError::Other(format!("symlink agent {}: {}", agent_name, e)))?;
                println!(
                    "  ✓ agent: {} → .{}/agents/{}",
                    agent_name, self.target, link_name
                );
            }
        }

        // Create symlinks for MCP items
        for (mcp_name, mcp_glob) in &mcp {
            let target_dir = adapter.map_dir(PackageType::Mcp, &self.project_root);
            let link_name = symlink_name(mcp_name, &[]);
            let store_mcp_path = store_path.join(mcp_glob);
            if store_mcp_path.exists() {
                adapter
                    .install(&store_mcp_path, &target_dir, &link_name)
                    .map_err(|e| AgmError::Other(format!("symlink mcp {}: {}", mcp_name, e)))?;
                println!("  ✓ mcp: {} → .{}/mcp/{}", mcp_name, self.target, link_name);
            }
        }

        if !skills.is_empty() {
            self.manifest
                .skills
                .insert(pkg_name.clone(), final_spec.clone());
        }
        if !agents.is_empty() {
            self.manifest
                .agents
                .insert(pkg_name.clone(), final_spec.clone());
        }
        if !mcp.is_empty() {
            self.manifest
                .mcp
                .insert(pkg_name.clone(), final_spec.clone());
        }
        if !skills.is_empty() || !agents.is_empty() || !mcp.is_empty() {
            installed.push((pkg_name.clone(), actual_commit.clone(), resolution.clone()));
        }

        if skills.is_empty() && agents.is_empty() && mcp.is_empty() {
            println!("No skills, agents, or mcp found in the repo. If the repo has an agm.package.json, it should declare exports.");
        }

        if !installed.is_empty() {
            // Save agm.json
            let manifest_path = self.project_root.join("agm.json");
            self.manifest
                .save(&manifest_path)
                .map_err(|e| AgmError::Other(format!("save agm.json: {}", e)))?;

            // Update lock file
            self.update_lock(&installed)
                .map_err(|e| AgmError::Other(format!("update lock: {}", e)))?;
        }

        adapter
            .post_install()
            .map_err(|e| AgmError::Other(format!("post_install: {}", e)))?;
        Ok(())
    }
}

/// Remove existing symlinks in `target_dir` that point into `store_path`.
///
/// Uses canonicalized paths when possible so the comparison works on Windows
/// even when symlinks are resolved to short/long path variants. Directory
/// symlinks on Windows are removed with `remove_dir` to avoid following the
/// link and deleting the target contents.
pub(crate) fn remove_package_symlinks(target_dir: &Path, store_path: &Path) -> Result<()> {
    if !target_dir.exists() {
        return Ok(());
    }

    let canonical_store = std::fs::canonicalize(store_path).ok();

    for entry in std::fs::read_dir(target_dir)? {
        let entry = entry?;
        let path = entry.path();
        if let Ok(target) = std::fs::read_link(&path) {
            let belongs = match canonical_store {
                Some(ref store) => std::fs::canonicalize(&target)
                    .map(|t| t.starts_with(store))
                    .unwrap_or(false),
                None => target.starts_with(store_path),
            };

            if belongs {
                remove_symlink_or_dir(&path)?;
            }
        }
    }
    Ok(())
}

fn typ_label(typ: PackageType) -> &'static str {
    match typ {
        PackageType::Skills => "skill",
        PackageType::Agents => "agent",
        PackageType::Mcp => "mcp",
    }
}

fn typ_subdir(typ: PackageType) -> &'static str {
    match typ {
        PackageType::Skills => "skills",
        PackageType::Agents => "agents",
        PackageType::Mcp => "mcp",
    }
}

impl InstallContext {
    pub fn install_all(&mut self) -> Result<()> {
        let adapter = get_adapter(&self.target)
            .ok_or_else(|| AgmError::Other(format!("unknown target: {}", self.target)))?;

        let deps = collect_dependencies(&self.manifest);

        if deps.is_empty() {
            tracing::info!("no dependencies to install");
            return Ok(());
        }

        let registry_url = self
            .manifest
            .registry
            .as_deref()
            .unwrap_or(&self.config.default_registry);
        let registry_client = RegistryClient::new(registry_url, self.config.registry_token.clone());

        let types = [PackageType::Skills, PackageType::Agents, PackageType::Mcp];
        let mut installed_packages: Vec<(String, String, Resolution)> = Vec::new();
        let rt = Runtime::new()?;

        for typ in &types {
            let deps_of_type: Vec<_> = deps.iter().filter(|(_, _, t)| t == typ).collect();

            for (name, spec, _) in &deps_of_type {
                let lock_version: String;
                let resolution;

                if is_git_dep(name) {
                    validate_commit_hash(spec.version())?;
                    lock_version = spec.version().to_string();

                    let pkg_key = format!("{}@{}", name, lock_version);
                    if let Some(lock) = &self.lock {
                        if lock.packages.contains_key(&pkg_key)
                            && matches!(spec, DependencySpec::Simple(_))
                        {
                            continue;
                        }
                    }

                    let temp_dir = self.temp_dir()?;
                    let repo_url =
                        format!("https://github.com/{}", name.trim_start_matches("@git/"));
                    git::clone_at_commit(&repo_url, spec.version(), temp_dir.path())?;

                    resolution = Resolution::Git {
                        repo: repo_url,
                        commit: spec.version().to_string(),
                    };

                    install_to_store(
                        &self.store,
                        temp_dir.path(),
                        &resolution,
                        name,
                        spec.version(),
                    )?;
                    let _ = temp_dir.close();
                } else {
                    let resolved_version = rt.block_on(resolve_registry_version(
                        &registry_client,
                        name,
                        spec.version(),
                    ))?;
                    lock_version = resolved_version.clone();

                    let pkg_key = format!("{}@{}", name, lock_version);
                    if let Some(lock) = &self.lock {
                        if lock.packages.contains_key(&pkg_key)
                            && matches!(spec, DependencySpec::Simple(_))
                        {
                            continue;
                        }
                    }

                    let version_meta =
                        rt.block_on(registry_client.get_version(name, &resolved_version))?;

                    let temp_dir = self.temp_dir()?;
                    let tarball_path = temp_dir.path().join("pkg.tar.gz");
                    rt.block_on(registry_client.download_tarball(
                        name,
                        &version_meta.tarball,
                        &tarball_path,
                    ))?;

                    let extract_dir = temp_dir.path().join("extracted");
                    std::fs::create_dir(&extract_dir)?;
                    extract_tarball(&tarball_path, &extract_dir)?;

                    resolution = Resolution::Registry {
                        integrity: version_meta.integrity.clone(),
                    };

                    install_to_store(
                        &self.store,
                        &extract_dir,
                        &resolution,
                        name,
                        &resolved_version,
                    )?;
                }

                let store_path = match &resolution {
                    Resolution::Git { repo, commit, .. } => {
                        self.store.git_package_path(repo, commit)
                    }
                    Resolution::Registry { .. } => {
                        self.store.registry_package_path(name, &lock_version)
                    }
                };

                // Detect and filter items
                let (items, target_subdir): (Vec<(String, String)>, _) = match *typ {
                    PackageType::Skills => {
                        let detected = detect_package_items(
                            &store_path,
                            PackageType::Skills,
                            name,
                            spec.base(),
                        )?;
                        (
                            filter_items(&detected, spec)?,
                            adapter.map_dir(*typ, &self.project_root),
                        )
                    }
                    PackageType::Agents => {
                        let detected = detect_package_items(
                            &store_path,
                            PackageType::Agents,
                            name,
                            spec.base(),
                        )?;
                        (
                            filter_items(&detected, spec)?,
                            adapter.map_dir(*typ, &self.project_root),
                        )
                    }
                    PackageType::Mcp => {
                        let detected =
                            detect_package_items(&store_path, PackageType::Mcp, name, spec.base())?;
                        (
                            filter_items(&detected, spec)?,
                            adapter.map_dir(*typ, &self.project_root),
                        )
                    }
                };

                // Remove stale symlinks from a previous install of this package
                remove_package_symlinks(&target_subdir, &store_path)?;

                if items.is_empty() && !matches!(spec, DependencySpec::Simple(_)) {
                    println!("  (no items matched pick/omit filters for {})", name);
                }

                for (item_name, item_glob) in &items {
                    let link_name = symlink_name(item_name, &[]);
                    let store_item_path = if item_glob == "." {
                        store_path.to_path_buf()
                    } else {
                        store_path.join(item_glob)
                    };
                    let install_source: &Path = if item_glob == "." {
                        &store_item_path
                    } else {
                        match *typ {
                            PackageType::Skills => {
                                store_item_path.parent().unwrap_or(&store_item_path)
                            }
                            PackageType::Agents | PackageType::Mcp => &store_item_path,
                        }
                    };
                    if store_item_path.exists() {
                        adapter.install(install_source, &target_subdir, &link_name)?;
                        println!(
                            "  ✓ {}: {} → .{}/{}/{}",
                            typ_label(*typ),
                            item_name,
                            self.target,
                            typ_subdir(*typ),
                            link_name
                        );
                    }
                }

                installed_packages.push((name.clone(), lock_version, resolution));
            }
        }

        adapter.post_install()?;
        self.update_lock(&installed_packages)
    }

    fn update_lock(&self, installed: &[(String, String, Resolution)]) -> Result<()> {
        let mut lock = self.lock.clone().unwrap_or_else(|| LockFile {
            lockfile_version: 1,
            importers: BTreeMap::new(),
            packages: BTreeMap::new(),
        });

        let importer = lock
            .importers
            .entry(".".into())
            .or_insert_with(|| LockImporter {
                skills: BTreeMap::new(),
                agents: BTreeMap::new(),
                mcp: BTreeMap::new(),
            });

        for (name, version, _) in installed {
            let dep = LockDependency {
                version: version.clone(),
            };
            if self.manifest.skills.contains_key(name) {
                importer.skills.insert(name.clone(), dep);
            } else if self.manifest.agents.contains_key(name) {
                importer.agents.insert(name.clone(), dep);
            } else if self.manifest.mcp.contains_key(name) {
                importer.mcp.insert(name.clone(), dep);
            }
        }

        for (name, version, resolution) in installed {
            let pkg_key = format!("{}@{}", name, version);
            lock.packages
                .entry(pkg_key)
                .or_insert_with(|| LockedPackage {
                    resolution: resolution.clone(),
                    targets: vec![self.target.clone()],
                });
        }

        let lock_path = self.project_root.join("agm.lock.json");
        lock.save(&lock_path)?;
        Ok(())
    }
}

/// Extract .tar.gz tarball
fn extract_tarball(tarball_path: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(tarball_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(dest)?;
    Ok(())
}

#[cfg(test)]
#[path = "installer_test.rs"]
mod tests;
