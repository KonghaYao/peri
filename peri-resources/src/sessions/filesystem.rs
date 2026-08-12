use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use tokio::{fs, io::AsyncWriteExt};

use peri_acp_types::{
    messages::BaseMessage,
    store::{CompactionLifecycle, ThreadStore},
    thread::{AgentStatus, ThreadId, ThreadMeta},
};

/// 基于文件系统的 ThreadStore 实现
///
/// 目录结构：
/// ```text
/// <base_dir>/
///   index.json                 # 所有 thread 的摘要索引
///   <thread_id>/
///     meta.json                # 单个 thread 的完整元数据
///     messages.jsonl           # 消息流，每行一条 JSON
/// ```
pub struct FilesystemThreadStore {
    base_dir: PathBuf,
}

impl FilesystemThreadStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// 使用默认路径 `~/.peri/threads/` 创建
    pub fn default_path() -> Result<Self> {
        let dir = dirs_next::home_dir()
            .context("无法获取 home 目录")?
            .join(".peri")
            .join("threads");
        Ok(Self::new(dir))
    }

    fn thread_dir(&self, id: &ThreadId) -> PathBuf {
        self.base_dir.join(id)
    }

    fn meta_path(&self, id: &ThreadId) -> PathBuf {
        self.thread_dir(id).join("meta.json")
    }

    fn messages_path(&self, id: &ThreadId) -> PathBuf {
        self.thread_dir(id).join("messages.jsonl")
    }

    fn index_path(&self) -> PathBuf {
        self.base_dir.join("index.json")
    }

    /// 读取全局 index，不存在时返回空列表
    async fn read_index(&self) -> Result<Vec<ThreadMeta>> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(vec![]);
        }
        let raw = fs::read_to_string(&path).await?;
        let metas: Vec<ThreadMeta> = serde_json::from_str(&raw)?;
        Ok(metas)
    }

    /// 将 metas 写入 index.json
    async fn write_index(&self, metas: &[ThreadMeta]) -> Result<()> {
        fs::create_dir_all(&self.base_dir).await?;
        let json = serde_json::to_string_pretty(metas)?;
        fs::write(self.index_path(), json).await?;
        Ok(())
    }

    /// 在 index 中更新或插入一条摘要
    async fn upsert_index(&self, meta: &ThreadMeta) -> Result<()> {
        let mut metas = self.read_index().await?;
        if let Some(pos) = metas.iter().position(|m| m.id == meta.id) {
            metas[pos] = meta.clone();
        } else {
            metas.push(meta.clone());
        }
        // 按 updated_at 降序排列
        metas.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
        self.write_index(&metas).await
    }
}

#[async_trait]
impl ThreadStore for FilesystemThreadStore {
    async fn create_thread(&self, meta: ThreadMeta) -> Result<ThreadId> {
        let id = meta.id.clone();
        fs::create_dir_all(self.thread_dir(&id)).await?;
        let json = serde_json::to_string_pretty(&meta)?;
        fs::write(self.meta_path(&id), json).await?;
        // 创建空的 messages.jsonl
        fs::write(self.messages_path(&id), b"").await?;
        self.upsert_index(&meta).await?;
        Ok(id)
    }

    async fn append_messages(&self, id: &ThreadId, msgs: &[BaseMessage]) -> Result<()> {
        if msgs.is_empty() {
            return Ok(());
        }
        let path = self.messages_path(id);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .with_context(|| format!("打开 messages.jsonl 失败: {}", path.display()))?;

        for msg in msgs {
            let mut line = serde_json::to_string(msg)?;
            line.push('\n');
            file.write_all(line.as_bytes()).await?;
        }
        file.flush().await?;

        // 更新 meta 的 message_count 和 updated_at
        let mut meta = self.load_meta(id).await?;
        meta.message_count += msgs.len();
        meta.updated_at = Utc::now();
        // 如果还没有标题，用第一条 Human 消息的前 50 字符作为标题
        if meta.title.is_none() {
            if let Some(title) = extract_title(msgs) {
                meta.title = Some(title);
            }
        }
        self.update_meta(id, meta).await
    }

    async fn load_messages(&self, id: &ThreadId) -> Result<Vec<BaseMessage>> {
        let path = self.messages_path(id);
        if !path.exists() {
            return Ok(vec![]);
        }
        let raw = fs::read_to_string(&path).await?;
        let mut msgs = Vec::new();
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let msg: BaseMessage =
                serde_json::from_str(line).with_context(|| format!("反序列化消息失败: {line}"))?;
            msgs.push(msg);
        }
        Ok(msgs)
    }

    async fn load_meta(&self, id: &ThreadId) -> Result<ThreadMeta> {
        let path = self.meta_path(id);
        let raw = fs::read_to_string(&path)
            .await
            .with_context(|| format!("读取 meta.json 失败: {}", path.display()))?;
        let meta: ThreadMeta = serde_json::from_str(&raw)?;
        Ok(meta)
    }

    async fn update_meta(&self, id: &ThreadId, meta: ThreadMeta) -> Result<()> {
        let json = serde_json::to_string_pretty(&meta)?;
        fs::write(self.meta_path(id), json).await?;
        self.upsert_index(&meta).await
    }

    async fn list_threads(&self) -> Result<Vec<ThreadMeta>> {
        let mut metas = self.read_index().await?;
        // 排除 hidden 的子 agent
        metas.retain(|m| !m.hidden);
        metas.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
        // 计算 content_size（从 messages.jsonl 文件大小）
        for meta in &mut metas {
            let msg_path = self.messages_path(&meta.id);
            if msg_path.exists() {
                if let Ok(file_meta) = tokio::fs::metadata(&msg_path).await {
                    meta.content_size = file_meta.len();
                }
            }
        }
        Ok(metas)
    }

    async fn delete_thread(&self, id: &ThreadId) -> Result<()> {
        // 级联删除整个线程树：hidden 子 agent 线程沿 parent_thread_id 挂链，
        // 若不递归删除会留下永远无法通过 UI/协议访问的孤儿数据。
        let mut to_delete = vec![id.clone()];
        let mut idx = 0;
        while idx < to_delete.len() {
            let children = self.list_child_threads(&to_delete[idx]).await?;
            to_delete.extend(children.into_iter().map(|m| m.id));
            idx += 1;
        }
        let mut metas = self.read_index().await?;
        for tid in &to_delete {
            let dir = self.thread_dir(tid);
            if dir.exists() {
                fs::remove_dir_all(&dir).await?;
            }
        }
        metas.retain(|m| !to_delete.contains(&m.id));
        self.write_index(&metas).await
    }

    async fn load_context(&self, thread_id: &ThreadId) -> Result<Vec<BaseMessage>> {
        // 文件系统实现暂不支持祖先链，直接加载自身消息
        self.load_messages(thread_id).await
    }

    async fn list_child_threads(&self, parent_id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        let metas = self.read_index().await?;
        Ok(metas
            .into_iter()
            .filter(|m| m.parent_thread_id.as_deref() == Some(parent_id.as_str()))
            .collect())
    }

    async fn list_session_threads(&self, root_id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        // 文件系统实现：简单过滤 parent_chain 包含 root_id 的 thread
        let metas = self.read_index().await?;
        let mut result = vec![];
        for m in &metas {
            if m.id == *root_id {
                result.push(m.clone());
            }
        }
        // 简单 BFS 查找子孙
        let mut to_check: Vec<String> = result.iter().map(|m| m.id.clone()).collect();
        while let Some(pid) = to_check.pop() {
            for m in &metas {
                if m.parent_thread_id.as_deref() == Some(pid.as_str()) {
                    to_check.push(m.id.clone());
                    result.push(m.clone());
                }
            }
        }
        Ok(result)
    }

    async fn update_thread_status(&self, id: &ThreadId, status: &str) -> Result<()> {
        // 关键约束：参数字符串必须经 FromStr 解析，非法值直接返回错误，不静默 fallback
        let status = AgentStatus::from_str(status)
            .with_context(|| format!("非法 agent_status 值: {status:?}"))?;
        let mut meta = self.load_meta(id).await?;
        meta.agent_status = status;
        meta.updated_at = Utc::now();
        self.update_meta(id, meta).await
    }

    async fn invalidate_context_cache(&self, thread_id: &ThreadId) -> Result<()> {
        let mut meta = self.load_meta(thread_id).await?;
        meta.cached_context = None;
        self.update_meta(thread_id, meta).await
    }

    async fn delete_messages(
        &self,
        _thread_id: &ThreadId,
        _message_ids: &[peri_acp_types::messages::MessageId],
    ) -> Result<()> {
        Ok(())
    }

    async fn update_message_flags(
        &self,
        _message_id: &peri_acp_types::messages::MessageId,
        _flags: &peri_acp_types::store::MessageFlags,
    ) -> Result<()> {
        // FilesystemThreadStore 仅用于测试，未持久化 truncated/excluded/projection 标记
        // （JSONL 行为纯 BaseMessage，无 flags envelope）。
        // 生产路径走 SqliteThreadStore（独立 truncated/excluded/projection 列）。
        // 此处保留 no-op 以满足 ThreadStore 契约；测试若需断言标记落库请改用
        // in-memory SqliteThreadStore。
        Ok(())
    }

    async fn commit_compaction_lifecycle(
        &self,
        thread_id: &ThreadId,
        lifecycle: &CompactionLifecycle,
    ) -> Result<()> {
        let _ = (thread_id, lifecycle);
        anyhow::bail!(
            "Filesystem store does not support compaction lifecycle. Please use SqliteThreadStore."
        )
    }

    async fn delete_messages_since(
        &self,
        thread_id: &ThreadId,
        message_id: &peri_acp_types::messages::MessageId,
    ) -> Result<()> {
        // 重写 messages.jsonl：保留 message_id 所在位置（含）之前所有行。
        let path = self.messages_path(thread_id);
        if !path.exists() {
            return Ok(());
        }
        let raw = fs::read_to_string(&path).await?;
        let mut kept: Vec<String> = Vec::new();
        let mut found = false;
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            kept.push(line.to_string());
            if let Ok(msg) = serde_json::from_str::<BaseMessage>(trimmed) {
                if msg.id() == *message_id {
                    found = true;
                    break;
                }
            }
        }
        if !found {
            // 目标 message_id 不存在，按 ThreadStore trait 默认契约不执行任何操作
            return Ok(());
        }
        let mut file = tokio::fs::File::create(&path).await?;
        for line in &kept {
            file.write_all(line.as_bytes()).await?;
            file.write_all(b"\n").await?;
        }
        file.flush().await?;
        // 同步 meta 的 message_count
        let mut meta = self.load_meta(thread_id).await?;
        meta.message_count = kept.len();
        meta.updated_at = Utc::now();
        self.update_meta(thread_id, meta).await
    }
}

/// 从消息列表中提取标题（取第一条 Human 消息的前 50 字符）
fn extract_title(msgs: &[BaseMessage]) -> Option<String> {
    use peri_acp_types::messages::{ContentBlock, MessageContent};

    for msg in msgs {
        if let BaseMessage::Human { content, .. } = msg {
            let text = match content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::Text { text } = b {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                MessageContent::Raw(_) => continue,
            };
            let title: String = text.chars().take(50).collect();
            if !title.is_empty() {
                return Some(title);
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "filesystem_test.rs"]
mod tests;
