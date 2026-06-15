use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use git2::{DiffOptions, Oid, Repository};

use super::commit::{CommitDetail, DiffStats, FileChange, FileStatus, TopoNode};

pub struct GitRepo {
    repo: Repository,
}

impl GitRepo {
    pub fn open(path: &Path) -> Result<Self> {
        let repo =
            Repository::discover(path).with_context(|| format!("不是 git 仓库: {:?}", path))?;
        Ok(Self { repo })
    }

    /// 一次性扫描拓扑骨架（包含 stash 节点）
    #[allow(dead_code)]
    pub fn scan_topology(&self) -> Result<Vec<TopoNode>> {
        self.scan_topology_with_extra(&[])
    }

    /// 扫描拓扑骨架，额外注入指定的 oid（如所有 stash commit）
    pub fn scan_topology_with_extra(&self, extra_oids: &[Oid]) -> Result<Vec<TopoNode>> {
        let mut revwalk = self.repo.revwalk()?;
        for branch in self.repo.branches(Some(git2::BranchType::Local))? {
            let branch = branch?.0;
            if let Some(name) = branch.name()?.map(|s| s.to_string()) {
                let ref_name = format!("refs/heads/{}", name);
                if let Ok(oid) = self.repo.refname_to_id(&ref_name) {
                    revwalk.push(oid)?;
                }
            }
        }
        // 加入 stash 引用
        if let Ok(oid) = self.repo.refname_to_id("refs/stash") {
            let _ = revwalk.push(oid);
        }
        // 加入额外 oid（所有 stash commit）
        for oid in extra_oids {
            let _ = revwalk.push(*oid);
        }
        let mut nodes = Vec::new();
        for oid in revwalk {
            let oid = oid?;
            let commit = self.repo.find_commit(oid)?;
            let parent_oids: Vec<Oid> = commit.parent_ids().collect();
            let time = commit.time().seconds();
            let message_short = commit
                .message()
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            nodes.push(TopoNode {
                oid,
                parent_oids,
                time,
                message_short,
            });
        }
        Ok(nodes)
    }

    /// 按 commit hash 查找完整 commit 详情
    pub fn commit_detail(&self, oid: Oid) -> Result<CommitDetail> {
        let commit = self.repo.find_commit(oid)?;
        let short_hash = format!("{:.7}", oid);
        let author = commit.author();
        let message = commit.message().unwrap_or("").to_string();
        let parent_oids: Vec<Oid> = commit.parent_ids().collect();
        let branches = self.branches_for_oid(oid)?;
        let tags = self.tags_for_oid(oid)?;
        Ok(CommitDetail {
            oid,
            short_hash,
            author_name: author.name().unwrap_or("").to_string(),
            author_email: author.email().unwrap_or("").to_string(),
            time: commit.time().seconds(),
            message,
            parent_oids,
            branches,
            tags,
            stats: None,
        })
    }

    /// 加载 commit diff 统计
    pub fn load_diff_stats(&self, oid: Oid) -> Result<DiffStats> {
        let commit = self.repo.find_commit(oid)?;
        let tree = commit.tree()?;
        let parent = commit.parent(0);
        let parent_tree = match &parent {
            Ok(p) => Some(p.tree()?),
            Err(_) => None,
        };

        let mut opts = DiffOptions::new();
        let diff =
            self.repo
                .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;

        let stats = diff.stats()?;
        let mut files = Vec::new();
        for delta in diff.deltas() {
            let new_file = delta.new_file();
            let old_file = delta.old_file();
            files.push(FileChange {
                path: new_file
                    .path()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                old_path: if delta.status() == git2::Delta::Renamed {
                    old_file.path().map(|p| p.to_string_lossy().to_string())
                } else {
                    None
                },
                status: FileStatus::from_delta(delta.status()),
                insertions: 0,
                deletions: 0,
            });
        }

        Ok(DiffStats {
            files,
            insertions: stats.insertions(),
            deletions: stats.deletions(),
            files_changed: stats.files_changed(),
        })
    }

    pub fn head_oid(&self) -> Result<Oid> {
        let head = self.repo.head()?;
        let target = head.target().context("HEAD 无目标")?;
        Ok(target)
    }

    /// 获取当前分支名，如果处于 detached HEAD 则返回 None
    pub fn head_branch(&self) -> Option<String> {
        let head = self.repo.head().ok()?;
        if head.is_branch() {
            head.shorthand().ok().map(|s| s.to_string())
        } else {
            None
        }
    }

    /// 当前分支是否有 upstream 跟踪
    pub fn has_upstream(&self) -> bool {
        if let Some(branch_name) = self.head_branch() {
            if let Ok(branch) = self.repo.find_branch(&branch_name, git2::BranchType::Local) {
                branch.upstream().is_ok()
            } else {
                false
            }
        } else {
            false
        }
    }

    /// 获取当前分支的 upstream 名称（如 "origin/main"）
    #[allow(dead_code)]
    pub fn upstream_name(&self) -> Option<String> {
        let branch_name = self.head_branch()?;
        let branch = self
            .repo
            .find_branch(&branch_name, git2::BranchType::Local)
            .ok()?;
        let upstream = branch.upstream().ok()?;
        upstream.name().ok()?.map(|s| s.to_string())
    }

    /// 获取远程仓库的默认分支名（通过 origin/HEAD 符号引用）
    #[allow(dead_code)]
    pub fn remote_head_branch(&self) -> Option<String> {
        let remote_head = self.repo.find_reference("refs/remotes/origin/HEAD").ok()?;
        let target = remote_head.symbolic_target().ok().flatten()?;
        // target 格式: "refs/remotes/origin/main"
        target.rsplit('/').next().map(|s| s.to_string())
    }
    /// 返回 (ahead, behind)，如果没有 upstream 返回 None
    pub fn ahead_behind(&self) -> Option<(usize, usize)> {
        let branch_name = self.head_branch()?;
        let branch = self
            .repo
            .find_branch(&branch_name, git2::BranchType::Local)
            .ok()?;
        let upstream = branch.upstream().ok()?;
        let local_oid = branch.get().target()?;
        let upstream_oid = upstream.get().target()?;
        self.repo.graph_ahead_behind(local_oid, upstream_oid).ok()
    }

    pub fn branch_map(&self) -> Result<HashMap<Oid, Vec<String>>> {
        let mut map: HashMap<Oid, Vec<String>> = HashMap::new();
        for branch in self.repo.branches(Some(git2::BranchType::Local))? {
            let branch = branch?.0;
            if let (Some(name), Some(target)) =
                (branch.name()?.map(|s| s.to_string()), branch.get().target())
            {
                map.entry(target).or_default().push(name);
            }
        }
        // 排序保证确定性，避免 HashMap/迭代器顺序不稳定导致颜色跳动
        for names in map.values_mut() {
            names.sort();
        }
        Ok(map)
    }

    /// 远程分支 → Oid 映射（过滤 origin/HEAD）
    pub fn remote_branch_map(&self) -> Result<HashMap<Oid, Vec<String>>> {
        let mut map: HashMap<Oid, Vec<String>> = HashMap::new();
        for branch in self.repo.branches(Some(git2::BranchType::Remote))? {
            let branch = branch?.0;
            if let (Some(full_name), Some(target)) =
                (branch.name()?.map(|s| s.to_string()), branch.get().target())
            {
                if full_name.ends_with("/HEAD") {
                    continue;
                }
                map.entry(target).or_default().push(full_name);
            }
        }
        for names in map.values_mut() {
            names.sort();
        }
        Ok(map)
    }

    pub fn tag_map(&self) -> Result<HashMap<Oid, Vec<String>>> {
        let mut map: HashMap<Oid, Vec<String>> = HashMap::new();
        for name in (&self.repo.tag_names(None)?)
            .into_iter()
            .filter_map(Result::ok)
            .flatten()
        {
            let name = name.to_string();
            if let Ok(ref_name) = self.repo.refname_to_id(&format!("refs/tags/{}", name)) {
                if let Ok(commit) = self.repo.find_commit(ref_name) {
                    map.entry(commit.id()).or_default().push(name);
                } else if let Ok(tag_obj) = self.repo.find_tag(ref_name) {
                    let target_oid = tag_obj.target_id();
                    map.entry(target_oid).or_default().push(name);
                }
            }
        }
        for names in map.values_mut() {
            names.sort();
        }
        Ok(map)
    }

    pub fn branch_names(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for branch in self.repo.branches(Some(git2::BranchType::Local))? {
            let branch = branch?.0;
            if let Some(name) = branch.name()?.map(|s| s.to_string()) {
                names.push(name);
            }
        }
        Ok(names)
    }

    pub fn tag_names_list(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for name in self
            .repo
            .tag_names(None)?
            .iter()
            .filter_map(Result::ok)
            .flatten()
        {
            names.push(name.to_string());
        }
        Ok(names)
    }

    fn branches_for_oid(&self, oid: Oid) -> Result<Vec<String>> {
        let map = self.branch_map()?;
        Ok(map.get(&oid).cloned().unwrap_or_default())
    }

    fn tags_for_oid(&self, oid: Oid) -> Result<Vec<String>> {
        let map = self.tag_map()?;
        Ok(map.get(&oid).cloned().unwrap_or_default())
    }

    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    pub fn repo_mut(&mut self) -> &mut Repository {
        &mut self.repo
    }

    /// 列出 workdir 下所有文件（跳过 .git / target / node_modules 等），供文件搜索使用
    pub fn list_all_files(&self) -> Result<Vec<String>> {
        let workdir = self
            .repo
            .workdir()
            .context("bare 仓库无 workdir")?
            .to_path_buf();
        let skip_dirs = [".git", "target", "node_modules"];
        let mut files = Vec::new();
        // 递归遍历 workdir，跳过 skip_dirs，收集相对路径
        fn walk_rel(
            base: &std::path::Path,
            dir: &std::path::Path,
            skip: &[&str],
            out: &mut Vec<String>,
        ) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if let Ok(ft) = entry.file_type() {
                        if ft.is_dir() {
                            if skip.contains(&name_str.as_ref()) {
                                continue;
                            }
                            walk_rel(base, &entry.path(), skip, out);
                        } else if ft.is_file() {
                            if let Ok(rel) = entry.path().strip_prefix(base) {
                                out.push(rel.to_string_lossy().to_string());
                            }
                        }
                    }
                }
            }
        }
        walk_rel(&workdir, &workdir, &skip_dirs, &mut files);
        // 按路径深度排序，浅层文件（源码）排在前面
        files.sort_by_key(|p| p.matches('/').count());
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn setup_test_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(path)
            .output()
            .unwrap();
        std::fs::write(path.join("a.txt"), "hello").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(path)
            .output()
            .unwrap();
        dir
    }

    #[test]
    fn test_open_repo() {
        let dir = setup_test_repo();
        let repo = GitRepo::open(dir.path()).unwrap();
        let head = repo.head_oid().unwrap();
        assert!(!head.is_zero());
    }

    #[test]
    fn test_scan_topology() {
        let dir = setup_test_repo();
        let repo = GitRepo::open(dir.path()).unwrap();
        let topo = repo.scan_topology().unwrap();
        assert_eq!(topo.len(), 1);
        assert!(topo[0].parent_oids.is_empty());
    }

    #[test]
    fn test_commit_detail() {
        let dir = setup_test_repo();
        let repo = GitRepo::open(dir.path()).unwrap();
        let head = repo.head_oid().unwrap();
        let detail = repo.commit_detail(head).unwrap();
        assert_eq!(detail.short_hash.len(), 7);
        assert_eq!(detail.message.trim(), "initial");
        assert!(!detail.branches.is_empty());
    }
}
