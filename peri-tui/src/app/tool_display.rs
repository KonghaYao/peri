/// 将绝对路径剥离 cwd 前缀，返回相对路径；失败则取末段文件名
fn strip_cwd(path: &str, cwd: Option<&str>) -> String {
    if let Some(cwd) = cwd {
        let base = if cwd.ends_with('/') {
            cwd.to_string()
        } else {
            format!("{}/", cwd)
        };
        if let Some(rel) = path.strip_prefix(&base) {
            return rel.to_string();
        }
    }
    // fallback：取最后一段文件名
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// 返回简短 display name，控制在 3-6 字符以保持 UI 对齐
pub fn format_tool_name(tool: &str) -> String {
    match tool {
        "Bash" => "Shell",
        "Read" => "Read",
        "Write" => "Write",
        "Edit" => "Edit",
        "Glob" => "Glob",
        "Grep" => "Grep",
        "folder_operations" => "Folder",
        "TodoWrite" => "Todo",
        "AskUserQuestion" => "Ask",
        "Agent" => "Agent",
        "LSP" => "LSP",
        "artifact" => "ArtUp",
        other => return to_pascal(other),
    }
    .to_string()
}

/// 返回参数摘要（含路径缩短逻辑）
pub fn format_tool_args(
    tool: &str,
    input: &serde_json::Value,
    cwd: Option<&str>,
) -> Option<String> {
    match tool {
        "Bash" => input["command"].as_str().map(|s| truncate(s, 400)),
        "Read" | "Write" | "Edit" => input["file_path"].as_str().map(|p| strip_cwd(p, cwd)),
        "Glob" => input["pattern"]
            .as_str()
            .map(|p| truncate(&strip_cwd(p, cwd), 200)),
        "Grep" => input["pattern"].as_str().map(|s| truncate(s, 200)),
        "folder_operations" => {
            let op = input["operation"].as_str().unwrap_or("?");
            let path = input["folder_path"].as_str().unwrap_or("?");
            Some(format!("{} {}", op, strip_cwd(path, cwd)))
        }
        "WebSearch" => input["query"].as_str().map(|s| truncate(s, 60)),
        "WebFetch" => input["url"].as_str().map(|s| truncate(s, 60)),
        "ExecuteExtraTool" => input["tool_name"].as_str().map(|s| truncate(s, 40)),
        "SearchExtraTools" => input["query"].as_str().map(|s| truncate(s, 40)),
        "artifact" => input["file_path"].as_str().map(|p| strip_cwd(p, cwd)),
        "AgentResult" => input["task_id"].as_str().map(|t| truncate(t, 12)),
        "LSP" => input["operation"].as_str().map(|s| truncate(s, 40)),
        _ => None,
    }
}

pub fn to_pascal(s: &str) -> String {
    s.split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect()
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// 判断工具结果是否应默认展开（不折叠）。
///
/// `AgentResult`：后台 agent 的最终结果，用户需要看到任务产出。
/// `ExecuteExtraTool`：deferred 工具的统一包装（如 artifact/WebFetch），
/// 其结果对用户有直接价值（例如上传后的 URL），折叠会完全吞掉关键信息。
/// 错误结果一律不在此展开（错误走 `error_summary_lines` 始终可见）。
pub fn should_auto_expand_tool(tool_name: &str, is_error: bool) -> bool {
    if is_error {
        return false;
    }
    matches!(tool_name, "AgentResult" | "ExecuteExtraTool")
}
