use std::collections::HashMap;

use anyhow::Result;
use git2::Oid;

/// Stash 信息
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct StashInfo {
    pub index: usize,
    pub oid: Oid,
    /// stash 基于的 commit（stash 的第一个 parent）
    pub base_commit: Oid,
    pub message: String,
}

impl super::repo::GitRepo {
    /// 获取所有 stash（git2 的 stash_foreach 需要 &mut self）
    pub fn stash_list(&mut self) -> Result<Vec<StashInfo>> {
        // 第一步：收集 stash index/oid/message（closure 内不能调用 repo 的其他方法）
        let mut raw: Vec<(usize, Oid, String)> = Vec::new();
        self.repo_mut().stash_foreach(|index, message, oid| {
            raw.push((index, *oid, message.to_string()));
            true
        })?;

        // 第二步：查找每个 stash 的 base_commit
        let mut stashes = Vec::new();
        for (index, oid, message) in raw {
            let base_commit = self
                .repo()
                .find_commit(oid)
                .ok()
                .and_then(|c| c.parent(0).ok())
                .map(|p| p.id())
                .unwrap_or(Oid::ZERO_SHA1);
            stashes.push(StashInfo {
                index,
                oid,
                base_commit,
                message,
            });
        }
        Ok(stashes)
    }

    /// 按 base_commit oid 索引 stash
    pub fn stash_by_commit(&mut self) -> Result<HashMap<Oid, Vec<StashInfo>>> {
        let list = self.stash_list()?;
        let mut map: HashMap<Oid, Vec<StashInfo>> = HashMap::new();
        for stash in list {
            map.entry(stash.base_commit).or_default().push(stash);
        }
        Ok(map)
    }

    pub fn stash_pop(&mut self, index: usize) -> Result<()> {
        self.repo_mut().stash_pop(index, None)?;
        Ok(())
    }

    pub fn stash_drop(&mut self, index: usize) -> Result<()> {
        self.repo_mut().stash_drop(index)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn stash_apply(&mut self, index: usize) -> Result<()> {
        self.repo_mut().stash_apply(index, None)?;
        Ok(())
    }
}
