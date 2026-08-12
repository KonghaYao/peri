//! Write / SandboxWrite 失败草稿存储。
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

    /// 只读查看草稿(不消费)。用于恢复前先校验 target 一致性,避免误消费导致无法用原路径重试。
    pub(crate) fn peek(&self, draft_id: &str) -> Option<&DraftEntry> {
        self.by_target.values().find(|e| e.id == draft_id)
    }

    /// 按 draft_id 消费性取出草稿(调用方须先 peek 确认)。线性扫描:草稿实际规模 = 会话内失败次数,
    /// 远小于 HashMap 扫描成本;无上限决策下可接受。
    pub(crate) fn take(&mut self, draft_id: &str) -> Option<DraftEntry> {
        let key = self
            .by_target
            .iter()
            .find(|(_, e)| e.id == draft_id)
            .map(|(k, _)| k.clone())?;
        self.by_target.remove(&key)
    }

    /// 成功写入后清理同 target 草稿(幂等)。
    pub(crate) fn remove_by_target(&mut self, target: &str) {
        self.by_target.remove(target);
    }
}

/// 草稿开关:PERI_WRITE_DRAFT=0 或 false(不区分大小写)关闭,其余默认开启。构造时读取一次。
pub(crate) fn draft_enabled() -> bool {
    match std::env::var("PERI_WRITE_DRAFT") {
        Ok(v) => !(v == "0" || v.eq_ignore_ascii_case("false")),
        Err(_) => true,
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

/// 中文草稿提示后缀(SandboxWrite 工具)。同上,规避关键词。
/// 格式与英文版对齐:`{id} ({n} 行,{m} 字节)`——id 与统计信息间保留空格,
/// 便于按空白切分提取 draft_id。
pub(crate) fn draft_hint_zh(id: &str, content: &str) -> String {
    format!(
        " 内容草稿已保存: {id} ({} 行,{} 字节)。可改用 from_draft={id} 重试。",
        content.lines().count(),
        content.len()
    )
}
