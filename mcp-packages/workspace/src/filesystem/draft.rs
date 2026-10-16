//! Write 失败草稿存储。
//!
//! 当写入失败且内容已到达文件系统层(tmp 写入失败、rename 失败、append 失败、超时)时,
//! 将已写出的文本暂存于进程级内存,错误消息携带 draft_id;LLM 通过 from_draft 参数恢复,
//! 避免重试时重新输出整个 content。路径校验/参数错误不落草稿。

use std::collections::HashMap;

/// 一次失败写入的完整内容。
pub(crate) struct DraftEntry {
    /// draft_id,形如 draft_{uuid v7}(与 tmp.{uuid} 命名同源,前缀 draft_)
    pub(crate) id: String,
    /// 保存时的目标路径(resolve_path / validate_path 的 canonical 输出;恢复时校验 file_path 一致)
    pub(crate) target: String,
    /// 已写出的文本:rename 失败为 tmp 实际文本,其余为 content 参数原文
    pub(crate) content: String,
    /// 原始调用的 append 标记:append 失败的草稿恢复时保持 append 语义,避免覆盖文件原内容
    pub(crate) append: bool,
}

/// 读取 exact id entry 的结果。
pub(crate) enum DraftAccessError<E> {
    Unknown,
    WrongTarget,
    Operation(E),
}

/// 进程级草稿存储:target → 最新草稿(同 target 覆盖,旧 draft_id 立即失效)。不设上限。
#[derive(Default)]
pub(crate) struct DraftStore {
    by_target: HashMap<String, DraftEntry>,
}

impl DraftStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 保存草稿并返回新 draft_id;同 target 已有草稿时覆盖(旧 id 失效)。
    pub(crate) fn save(&mut self, target: &str, content: String, append: bool) -> String {
        let id = format!("draft_{}", uuid::Uuid::now_v7());
        self.by_target.insert(
            target.to_string(),
            DraftEntry {
                id: id.clone(),
                target: target.to_string(),
                content,
                append,
            },
        );
        id
    }

    /// 在锁内读取 exact id，并仅在操作成功后删除该 exact id。
    pub(crate) fn with_exact_entry<T, E>(
        &mut self,
        draft_id: &str,
        target: &str,
        operation: impl FnOnce(&str, bool) -> Result<T, E>,
    ) -> Result<T, DraftAccessError<E>> {
        let Some((key, entry)) = self
            .by_target
            .iter()
            .find(|(_, entry)| entry.id == draft_id)
        else {
            return Err(DraftAccessError::Unknown);
        };
        if entry.target != target {
            return Err(DraftAccessError::WrongTarget);
        }
        let key = key.clone();
        let result =
            operation(&entry.content, entry.append).map_err(DraftAccessError::Operation)?;
        self.by_target.remove(&key);
        Ok(result)
    }
}

/// 英文草稿提示后缀(Write 工具)。行数 = lines().count(),字节数 = len()(UTF-8 字节)。
/// 前置空格拼接:`format!("Error ...: {e}{hint}")`;禁用时 hint 为空串。
/// 文案已核对:不含 PathSuggester ERROR_KEYWORDS("not found"/"no such file"/"does not exist"/
/// "not a directory"/"search path does not exist")。
pub(crate) fn draft_hint_en(id: &str, content: &str) -> String {
    format!(
        " A draft was saved: {id} ({} lines, {} bytes). Retry with from_draft={id}.",
        content.lines().count(),
        content.len()
    )
}
