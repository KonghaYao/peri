use peri_agent::tools::BaseTool;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::draft::{draft_hint_en, DraftStore};
use super::resolve_path;

const WRITE_FILE_DESCRIPTION: &str = include_str!("descriptions/write.md");

/// Write tool - 与 TypeScript write_tool 对齐
pub struct WriteFileTool {
    pub cwd: String,
    /// 失败草稿存储(进程级内存);None = PERI_WRITE_DRAFT=0 关闭
    drafts: Option<Arc<Mutex<DraftStore>>>,
}

impl WriteFileTool {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self::with_draft(cwd, super::draft::draft_enabled())
    }

    /// 测试注入构造:enabled=false 时完全禁用草稿(不创建 store)
    pub(crate) fn with_draft(cwd: impl Into<String>, enabled: bool) -> Self {
        Self {
            cwd: cwd.into(),
            drafts: enabled.then(|| Arc::new(Mutex::new(DraftStore::new()))),
        }
    }

    /// 保存失败内容为草稿;禁用时返回 None
    fn save_draft(&self, target: &str, content: &str, append: bool) -> Option<String> {
        self.drafts.as_ref().map(|store| {
            store
                .lock()
                .unwrap()
                .save(target, content.to_string(), append)
        })
    }

    /// 恢复前 peek + take:先校验 target 一致,不匹配则不消费草稿(LLM 可用原路径重试)
    fn restore_draft(&self, draft_id: &str, target: &str) -> Result<(String, bool), String> {
        let Some(store) = self.drafts.as_ref() else {
            // 禁用 == 不存在,统一优雅降级
            return Err(format!(
                "Draft '{draft_id}' is unknown or no longer available. Please retry by providing the 'content' parameter directly."
            ));
        };
        let verdict = {
            let guard = store.lock().unwrap();
            match guard.peek(draft_id) {
                None => None,                                 // 未知/失效
                Some(e) if e.target != target => Some(false), // 路径不符
                Some(_) => Some(true),                        // 匹配
            }
        };
        match verdict {
            None => Err(format!(
                "Draft '{draft_id}' is unknown or no longer available. Please retry by providing the 'content' parameter directly."
            )),
            Some(false) => Err(format!(
                "Draft '{draft_id}' belongs to a different file_path. Provide the original file_path, or retry with the 'content' parameter directly."
            )),
            Some(true) => {
                // peek 与 take 之间存在竞态窗口(同 draft_id 被并发恢复),优雅降级为 unknown
                let Some(entry) = store.lock().unwrap().take(draft_id) else {
                    return Err(format!(
                        "Draft '{draft_id}' is unknown or no longer available. Please retry by providing the 'content' parameter directly."
                    ));
                };
                Ok((entry.content, entry.append))
            }
        }
    }

    /// 成功写入后清理同 target 草稿(幂等;禁用时 no-op)
    fn remove_draft(&self, target: &str) {
        if let Some(store) = &self.drafts {
            store.lock().unwrap().remove_by_target(target);
        }
    }
}

#[async_trait::async_trait]
impl BaseTool for WriteFileTool {
    fn name(&self) -> &str {
        "Write"
    }

    fn is_direct(&self) -> bool {
        true
    }

    /// 同类工具分组（design v2 §2.5.1）：filesystem 工具统一归组。
    fn namespace(&self) -> Option<&str> {
        Some("filesystem")
    }

    /// 提示词层声明模板（design v2 §2.5.3）：对应 05 段落 "Write or edit a file"
    /// 条目语义（选择指引 + 纪律约束），不逐字重复（守护测试断言）。
    /// title 不覆盖——走 `tool_description` 默认推导路径。
    fn prompt_declaration(&self) -> Option<String> {
        Some(
            "Write a file → `{{name}}` (full contents). Use `{{name}}` for writing files, not `echo >`/`sed`/`awk`."
                .to_string(),
        )
    }

    fn description(&self) -> &str {
        WRITE_FILE_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to write (must be absolute, not relative)"
                },
                "content": {
                    "type": "string",
                    "description": "The full content to write to the file. Either 'content' or 'from_draft' must be provided."
                },
                "from_draft": {
                    "type": "string",
                    "description": "A draft id returned in a previous Write error message. Recover the failed write without resending content. Mutually exclusive with 'content'; reuse the original file_path."
                },
                "append": {
                    "type": "boolean",
                    "description": "If true, append content to the end of the file instead of overwriting. Use this for writing large files in chunks: first call Write without append to create the file with the initial content, then call Write with append=true to add more content. This avoids sending the entire file content in a single tool call, saving context window space.",
                    "default": false
                }
            },
            "required": ["file_path"]
        })
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or("The 'file_path' parameter is required for the Write tool.")?;

        // resolve 提前到 timeout 闭包外(infallible,行为不变),供草稿/恢复使用
        let resolved = resolve_path(&self.cwd, file_path);
        let resolved_str = resolved.to_string_lossy().to_string();

        // 宽容解析（与 Agent 工具 resume_thread_id 同构的容错）：LLM 常同时携带
        // 互斥参数，或用 "" / "__omit__" 等占位符表达「省略」——一律按语义处理而
        // 不报错：content 优先（完整自足，直接写入成功，草稿随后清理）；from_draft
        // 仅在 content 未提供时使用；两者皆无才报缺参数。原「互斥报错」会让文件
        // 无法落盘、模型被迫重输出一遍 content，白白浪费已生成内容。
        let content = input["content"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && *s != "__omit__");
        let from_draft = input["from_draft"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && *s != "__omit__");
        let (content, append) = if let Some(c) = content {
            (c.to_string(), input["append"].as_bool().unwrap_or(false))
        } else if let Some(id) = from_draft {
            self.restore_draft(id, &resolved_str)?
        } else {
            return Err(
                "Either 'content' or 'from_draft' must be provided for the Write tool.".into(),
            );
        };

        let result = tokio::time::timeout(Duration::from_secs(120), async {
            let line_count = content.lines().count();

            if let Some(parent) = resolved.parent() {
                if !parent.exists() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        // 文件系统层失败(与 tmp 写入失败同级):保存草稿,
                        // 错误消息携带 from_draft 恢复提示,避免重试时重新输出 content
                        let hint = self
                            .save_draft(&resolved_str, &content, append)
                            .map(|id| draft_hint_en(&id, &content))
                            .unwrap_or_default();
                        return Err(format!("Error creating parent directory: {e}{hint}").into());
                    }
                }
            }

            if append {
                use std::io::Write;
                let mut file = match std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&resolved)
                {
                    Ok(f) => f,
                    Err(e) => {
                        // append 分支仅在 append=true 时进入,草稿保留 append 语义
                        let hint = self
                            .save_draft(&resolved_str, &content, true)
                            .map(|id| draft_hint_en(&id, &content))
                            .unwrap_or_default();
                        return Err(format!("Error opening file for append: {e}{hint}").into());
                    }
                };
                if let Err(e) = file.write_all(content.as_bytes()) {
                    let hint = self
                        .save_draft(&resolved_str, &content, true)
                        .map(|id| draft_hint_en(&id, &content))
                        .unwrap_or_default();
                    return Err(format!("Error appending to file: {e}{hint}").into());
                }
                drop(file); // 确保句柄关闭后再读取文件

                let total_lines = std::fs::read_to_string(&resolved)
                    .map(|s| s.lines().count())
                    .unwrap_or(line_count);

                let rel = resolved
                    .strip_prefix(&self.cwd)
                    .unwrap_or(&resolved)
                    .display()
                    .to_string();
                let lines_label = if line_count == 1 { "line" } else { "lines" };
                self.remove_draft(&resolved_str);
                Ok::<String, Box<dyn std::error::Error + Send + Sync>>(format!(
                    "Appended {} {} to {} (file total: {} lines)",
                    line_count, lines_label, rel, total_lines
                ))
            } else {
                // 原子写入：先写临时文件再 rename，防止崩溃时丢失数据
                // 使用随机后缀避免并发写入冲突
                let tmp_ext = format!("tmp.{}", uuid::Uuid::now_v7());
                let tmp_path = resolved.with_extension(tmp_ext);
                if let Err(e) = std::fs::write(&tmp_path, &content) {
                    let hint = self
                        .save_draft(&resolved_str, &content, false)
                        .map(|id| draft_hint_en(&id, &content))
                        .unwrap_or_default();
                    return Err(format!("Error writing file: {e}{hint}").into());
                }
                // 恢复原文件的 Unix 权限位（含可执行位），防止原子写入后 +x 丢失
                if let Ok(metadata) = std::fs::metadata(&resolved) {
                    #[cfg(unix)]
                    {
                        let _ = std::fs::set_permissions(&tmp_path, metadata.permissions());
                    }
                    #[cfg(not(unix))]
                    let _ = &metadata; // Windows 上 #[cfg(unix)] 排除后 metadata 未使用
                }
                match std::fs::rename(&tmp_path, &resolved) {
                    Ok(_) => {
                        let rel = resolved
                            .strip_prefix(&self.cwd)
                            .unwrap_or(&resolved)
                            .display()
                            .to_string();
                        let lines_label = if line_count == 1 { "line" } else { "lines" };
                        self.remove_draft(&resolved_str);
                        Ok(format!("Wrote {} {} {}", line_count, lines_label, rel))
                    }
                    Err(e) => {
                        // 读 tmp 实际文本 → 存草稿 → 删 tmp(顺序固定,先读后删)
                        let draft_content = std::fs::read_to_string(&tmp_path)
                            .unwrap_or_else(|_| content.to_string());
                        let hint = self
                            .save_draft(&resolved_str, &draft_content, false)
                            .map(|id| draft_hint_en(&id, &draft_content))
                            .unwrap_or_default();
                        let _ = std::fs::remove_file(&tmp_path);
                        Err(format!("Error renaming temp file: {e}{hint}").into())
                    }
                }
            }
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(_elapsed) => {
                // 草稿 = content 原文,append 标记随原始调用;新 id 自洽(恢复写入超时存的是恢复出的内容)
                let hint = self
                    .save_draft(&resolved_str, &content, append)
                    .map(|id| draft_hint_en(&id, &content))
                    .unwrap_or_default();
                Err(format!(
                    "Write operation timed out (exceeded 2 minutes).{hint}\
                  \nFor large files, use the append=true parameter to write in chunks:\
                  \n1. First call Write without append to create the file with the initial content\
                  \n2. Then call Write with append=true to append the remaining content\
                  \nThis avoids timeouts caused by writing too much content in a single call."
                )
                .into())
            }
        }
    }
}

#[cfg(test)]
#[path = "write_test.rs"]
mod tests;
