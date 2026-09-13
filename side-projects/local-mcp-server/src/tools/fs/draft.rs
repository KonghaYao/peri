//! Write 失败草稿存储（`peri-middlewares/src/tools/filesystem/draft.rs` 的等价实现）。
//!
//! 语义与源实现逐条对齐：
//! - 只有**写盘阶段**失败（tmp 创建/写入、权限复制、rename）才落草稿；参数错误、越界拒绝、
//!   哨兵拒绝都不落；
//! - 同 target 覆盖：新草稿让旧 `draft_id` 立即失效；
//! - `with_exact_entry`：id 命中且 target 一致才消费，且**只在操作成功后**删除；
//! - 开关 `PERI_WRITE_DRAFT=0|false`（大小写不敏感）关闭；构造时读取一次。
//!
//! 差异（记录在 handoff）：草稿只存在本进程内存中，进程退出即丢失——与源实现
//! 「进程级内存」的语义一致。

use std::collections::HashMap;

/// 一次失败写入的完整内容。
#[derive(Debug, Clone)]
pub struct DraftEntry {
    /// `draft_{uuid v7}`（与源实现同源命名，前缀 `draft_`）。
    pub id: String,
    /// 保存时的目标键（授权根内归一路径；恢复时校验一致）。
    pub target: String,
    /// 已写出的文本。
    pub content: String,
    /// 原始调用的 append 标记：恢复时保持 append 语义，避免覆盖文件原有内容。
    pub append: bool,
}

/// 读取 exact id 的结果。
#[derive(Debug)]
pub enum DraftAccessError<E> {
    /// id 不存在（或已被覆盖失效）。
    Unknown,
    /// id 存在但属于另一个 target。
    WrongTarget,
    /// 操作本身失败。
    Operation(E),
}

/// 进程级草稿存储：target → 最新草稿。不设上限（与源实现一致）。
#[derive(Debug, Default)]
pub struct DraftStore {
    by_target: HashMap<String, DraftEntry>,
}

impl DraftStore {
    /// 空存储。
    pub fn new() -> Self {
        Self::default()
    }

    /// 保存草稿并返回新 id；同 target 已有草稿时覆盖。
    pub fn save(&mut self, target: &str, content: String, append: bool) -> String {
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
    pub fn with_exact_entry<T, E>(
        &mut self,
        draft_id: &str,
        target: &str,
        operation: impl FnOnce(&str, bool) -> Result<T, E>,
    ) -> Result<T, DraftAccessError<E>> {
        let Some(key) = self
            .by_target
            .iter()
            .find(|(_, entry)| entry.id == draft_id)
            .map(|(key, _)| key.clone())
        else {
            return Err(DraftAccessError::Unknown);
        };
        let entry = &self.by_target[&key];
        if entry.target != target {
            return Err(DraftAccessError::WrongTarget);
        }
        let result =
            operation(&entry.content, entry.append).map_err(DraftAccessError::Operation)?;
        self.by_target.remove(&key);
        Ok(result)
    }

    /// 成功写入后清理同 target 草稿（幂等）。
    pub fn remove_by_target(&mut self, target: &str) {
        self.by_target.remove(target);
    }
}

/// 草稿开关：`PERI_WRITE_DRAFT=0` 或 `false`（不区分大小写）关闭，其余默认开启。
///
/// 变量名沿用源实现，便于从 Peri 迁移的使用者保持行为；单进程本机形态下（D-003）
/// 本进程直接继承启动者的环境，**没有**容器期的 env clear + allowlist 机制
/// （那份要求随容器后端作废，见 `decisions/decision-003-pivot-to-local-mcp-server.md`）。
pub fn draft_enabled() -> bool {
    match std::env::var("PERI_WRITE_DRAFT") {
        Ok(value) => draft_enabled_for(&value),
        Err(_) => true,
    }
}

/// 纯函数形态的开关判定（便于单测，不需要改进程环境）。
pub fn draft_enabled_for(value: &str) -> bool {
    !(value == "0" || value.eq_ignore_ascii_case("false"))
}

/// 英文草稿提示后缀（行数 = `lines().count()`，字节数 = UTF-8 字节数）。
pub fn draft_hint_en(id: &str, content: &str) -> String {
    format!(
        " A draft was saved: {id} ({} lines, {} bytes). Retry with from_draft={id}.",
        content.lines().count(),
        content.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_save_overwrites_same_target_and_invalidates_old_id() {
        let mut store = DraftStore::new();
        let first = store.save("root/a.txt", "one".into(), false);
        let second = store.save("root/a.txt", "two".into(), true);
        assert_ne!(first, second);
        assert!(matches!(
            store.with_exact_entry::<(), ()>(&first, "root/a.txt", |_, _| Ok(())),
            Err(DraftAccessError::Unknown)
        ));
        let mut seen = None;
        store
            .with_exact_entry::<(), ()>(&second, "root/a.txt", |content, append| {
                seen = Some((content.to_string(), append));
                Ok(())
            })
            .unwrap();
        assert_eq!(seen, Some(("two".to_string(), true)));
        // 消费成功后条目被删除
        assert!(matches!(
            store.with_exact_entry::<(), ()>(&second, "root/a.txt", |_, _| Ok(())),
            Err(DraftAccessError::Unknown)
        ));
    }

    #[test]
    fn test_wrong_target_is_rejected_and_entry_survives() {
        let mut store = DraftStore::new();
        let id = store.save("root/a.txt", "x".into(), false);
        assert!(matches!(
            store.with_exact_entry::<(), ()>(&id, "root/b.txt", |_, _| Ok(())),
            Err(DraftAccessError::WrongTarget)
        ));
        assert!(store
            .with_exact_entry::<(), ()>(&id, "root/a.txt", |_, _| Ok(()))
            .is_ok());
    }

    #[test]
    fn test_failed_operation_keeps_draft() {
        let mut store = DraftStore::new();
        let id = store.save("root/a.txt", "x".into(), false);
        assert!(matches!(
            store.with_exact_entry::<(), &str>(&id, "root/a.txt", |_, _| Err("boom")),
            Err(DraftAccessError::Operation("boom"))
        ));
        assert!(store
            .with_exact_entry::<(), ()>(&id, "root/a.txt", |_, _| Ok(()))
            .is_ok());
    }

    #[test]
    fn test_draft_switch_matches_source_rules() {
        assert!(!draft_enabled_for("0"));
        assert!(!draft_enabled_for("false"));
        assert!(!draft_enabled_for("FALSE"));
        assert!(draft_enabled_for("1"));
        assert!(draft_enabled_for("true"));
        assert!(draft_enabled_for(""));
    }

    #[test]
    fn test_hint_shape_matches_source() {
        let hint = draft_hint_en("draft_abc", "a\nb\n");
        assert_eq!(
            hint,
            " A draft was saved: draft_abc (2 lines, 4 bytes). Retry with from_draft=draft_abc."
        );
    }
}
