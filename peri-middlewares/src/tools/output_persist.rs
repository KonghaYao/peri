use std::{env, fs, path::PathBuf};

/// 当输出被截断时，将完整内容写入临时文件。
/// 返回追加到截断信息后的提示字符串。
/// 文件路径：`{temp_dir}/peri-tool-output-{uuid}.txt`
pub fn persist_truncated_output(full_content: &str) -> String {
    let id = uuid::Uuid::new_v4();
    let dir = env::temp_dir();
    let file_name = format!("peri-tool-output-{id}.txt");
    let file_path: PathBuf = dir.join(&file_name);

    match fs::write(&file_path, full_content) {
        Ok(_) => format!(
            "\n\n[Full output saved to {} — use Read tool to view complete content]",
            file_path.display()
        ),
        Err(e) => format!(
            "\n\n[Failed to save full output to {}: {e}]",
            file_path.display()
        ),
    }
}

#[cfg(test)]
#[path = "output_persist_test.rs"]
mod tests;
