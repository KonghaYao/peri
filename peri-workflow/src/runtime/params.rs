//! 启动参数解析与校验（自 `tool.rs::WorkflowTool::invoke` 迁入）。
//!
//! 单一权威：MCP `Workflow` 工具与旧 `WorkflowTool` 都经 [`parse_start_input`]，
//! 不复制校验规则。`strictPreflight` 恒拒绝（workflow primitives 与图不可静态验证，
//! 见工具 schema 的诚实口径）。

use serde_json::Value;

use crate::runner::WorkflowInput;

/// JSON 安全整数上界（schema `maximum` 与解析校验共用同一常量）。
pub(crate) const MAX_SAFE_BUDGET_TOTAL: u64 = 9_007_199_254_740_991;
pub(crate) const MAX_SAFE_INTEGER: u64 = MAX_SAFE_BUDGET_TOTAL;
pub(crate) const MAX_CONCURRENCY_CAP: u64 = 16;

/// `Workflow` 工具的脚本用法说明（schema `script` 字段描述；MCP 工具与旧工具共用）。
pub const WORKFLOW_SCRIPT_DESCRIPTION: &str = r#"The script is the body of an async function (`new AsyncFunction`), not an ESM module; use JavaScript (TypeScript syntax is not transpiled). It must contain exactly one plain-literal `export const meta = { name, description }`; the engine removes that metadata before executing the body. The body receives these top-level injected primitives directly: `agent()`, `parallel()`, `pipeline()`, `phase()`, `log()`, and `workflow()`, plus `args` and `budget`. Do not use static or dynamic `import`, `export default`, or any other `export`; do not use old `workflow.agent(...)`, `workflow.parallel(...)`, `workflow.pipeline(...)`, `workflow.phase(...)`, or `workflow.log(...)` calls. Return the workflow result with a top-level `return`; without it the body returns undefined. Missing-return validation is advisory and does not prove that a result will be returned. Minimal read-only example:

```javascript
export const meta = {
  name: 'read-only-demo',
  description: 'Inspect files without changing the repository',
}

const result = await agent('Inspect the requested files and summarize findings.')
return result
```

Invoke this example with the tool parameter `writeIntent: { "kind": "read_only" }` when you want a read-only Git postcondition. This is not a permission boundary or filesystem sandbox.

Either `script` or `scriptPath` must be provided."#;

/// `Workflow` 工具描述（模型面；MCP 工具与旧工具共用同一份文案）。
pub const START_TOOL_DESCRIPTION: &str =
    "Launch a workflow with multiple agents working in parallel or pipeline. \
     The workflow runs asynchronously — this tool returns immediately with a run_id. \
     When the workflow completes, you'll receive a notification with the result summary. \
     Use the workflow when you need to orchestrate multiple agents for complex tasks.";

/// `Workflow` 启动参数 schema（模型面契约，单一权威）。
pub fn start_tool_parameters() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "script": {
                "type": "string",
                "description": WORKFLOW_SCRIPT_DESCRIPTION
            },
            "args": {
                "type": "object",
                "description": "Optional arguments passed to the workflow script."
            },
            "maxConcurrency": {
                "type": "number",
                "description": "Maximum concurrent agents (default 3).",
                "default": 3
            },
            "budgetTotal": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_SAFE_INTEGER,
                "description": "Maximum total token budget for this workflow. Omit for no explicit budget."
            },
            "maxAgents": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_SAFE_INTEGER,
                "description": "Fail-safe host limit for live agent attempts. Resume cache hits are not charged."
            },
            "maxToolCalls": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_SAFE_INTEGER,
                "description": "Fail-safe host limit for tool calls reported by completed live agents."
            },
            "maxElapsedMs": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_SAFE_INTEGER,
                "description": "Fail-safe host wall-clock limit in milliseconds."
            },
            "resumeFromRunId": {
                "type": "string",
                "description": "If provided, resume the workflow from the given run ID. \
                The journal from the previous run will be loaded for cache-hit."
            },
            "name": {
                "type": "string",
                "description": "Optional workflow name (for display). \
                If omitted, extracted from script's meta.name."
            },
            "strictPreflight": {
                "type": "boolean",
                "default": false,
                "description": "Reject unless workflow primitives and graph can be statically validated. The current engine cannot provide that proof."
            },
            "writeIntent": {
                "description": "Declarative repository postcondition, not a permission boundary or filesystem sandbox. Pass {kind: 'read_only'} for a workflow that should leave a Git repository unchanged: when the active cwd is in a Git repository, the host compares canonical HEAD, index, worktree, and untracked status after execution. This check does not block script or agent capabilities and does not observe ignored files or writes outside the repository; a non-Git cwd has no baseline and cannot produce a deliverable result. For {kind: 'write'}, repo_root and cwd must equal the active canonical repository root and workflow cwd, and path_allowlist must be non-empty repository-relative paths without parent traversal. Postcondition checks allow changes only under that list; head_may_change and commit_required control the current HEAD checks. Omit only for legacy runs; omitted intent can never produce a deliverable result.",
                "oneOf": [
                    {"type": "object", "properties": {"kind": {"const": "read_only"}}, "required": ["kind"], "additionalProperties": false},
                    {
                        "type": "object",
                        "properties": {
                            "kind": {"const": "write"},
                            "repo_root": {"type": "string"},
                            "cwd": {"type": "string"},
                            "path_allowlist": {"type": "array", "items": {"type": "string"}},
                            "head_may_change": {"type": "boolean", "default": false},
                            "commit_required": {"type": "boolean"}
                        },
                        "required": ["kind", "repo_root", "cwd", "path_allowlist"],
                        "additionalProperties": false
                    }
                ]
            },
            "scriptPath": {
                "type": "string",
                "description": "Path to a workflow script file (alternative to inline script). The file is canonicalized and must remain inside the active workflow cwd; its contents follow the same async-function-body grammar as `script`."
            }
        },
        "required": []
    })
}

/// 解析工具入参为 `WorkflowInput`（含 scriptPath 读取、参数校验、preflight）。
pub(crate) fn parse_start_input(input: &Value, cwd: &str) -> Result<WorkflowInput, String> {
    if input["strictPreflight"].as_bool().unwrap_or(false) {
        return Err(
            "strict preflight is unavailable: workflow primitives and graph cannot be statically validated".into(),
        );
    }

    // scriptPath 优先于 inline script（GAP-09 命名 Workflow 支持）
    // 路径安全：限定在 cwd 内，拒绝越权读取；读取经已校验 handle（见
    // `read_script_file`：不按路径名二次打开，消除 check-then-read TOCTOU）。
    let script_owned: String = if let Some(sp) = input["scriptPath"].as_str() {
        let cwd_path = std::path::PathBuf::from(cwd);
        let cwd_canonical = cwd_path
            .canonicalize()
            .map_err(|e| format!("cwd not accessible: {}", e))?;
        let script_path = resolve_script_path(sp, &cwd_canonical)
            .map_err(|e| format!("Invalid scriptPath '{}': {}", sp, e))?;
        read_script_file(&script_path, sp, &cwd_canonical)
            .map_err(|e| format!("Failed to read scriptPath '{}': {}", sp, e))?
    } else {
        input["script"]
            .as_str()
            .ok_or("missing 'script' or 'scriptPath' field")?
            .to_string()
    };
    let script = script_owned.as_str();

    let max_concurrency =
        parse_bounded_integer(input, "maxConcurrency", Some(3), MAX_CONCURRENCY_CAP)?
            .expect("maxConcurrency has a default") as u32;
    let budget_total = parse_budget_total(input)?;
    let limits = crate::protocol::WorkflowLimits {
        max_agents: parse_bounded_integer(input, "maxAgents", None, MAX_SAFE_INTEGER)?,
        max_tool_calls: parse_bounded_integer(input, "maxToolCalls", None, MAX_SAFE_INTEGER)?,
        max_elapsed_ms: parse_bounded_integer(input, "maxElapsedMs", None, MAX_SAFE_INTEGER)?,
    };

    let args = input.get("args").cloned();

    // 解析 resumeFromRunId（GAP-04）— 必须通过安全校验
    let resume_from = if let Some(s) = input["resumeFromRunId"].as_str() {
        if !is_safe_run_id(s) {
            return Err(format!(
                "Invalid resumeFromRunId '{}': must be a valid UUID without path traversal characters",
                s
            ));
        }
        Some(s.to_string())
    } else {
        None
    };

    let write_intent = input
        .get("writeIntent")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("Invalid writeIntent: {error}"))?;

    let git_baseline = crate::journal::GitBaseline::capture_for_intent(
        std::path::Path::new(cwd),
        write_intent.as_ref(),
    )
    .map_err(|error| format!("Workflow preflight failed: {error}"))?;

    // name 参数优先于脚本 heuristic
    let workflow_name = input["name"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| extract_workflow_name(script));

    Ok(WorkflowInput {
        script: script.to_string(),
        args,
        max_concurrency,
        budget_total,
        limits,
        workflow_name,
        resume_from,
        write_intent,
        git_baseline,
    })
}

fn parse_budget_total(input: &Value) -> Result<Option<u64>, String> {
    parse_bounded_integer(input, "budgetTotal", None, MAX_SAFE_INTEGER)
}

fn parse_bounded_integer(
    input: &Value,
    field: &str,
    default: Option<u64>,
    maximum: u64,
) -> Result<Option<u64>, String> {
    let Some(value) = input.get(field) else {
        return Ok(default);
    };
    let Some(value) = value.as_u64() else {
        return Err(format!(
            "'{field}' must be an integer between 1 and {maximum}"
        ));
    };
    if !(1..=maximum).contains(&value) {
        return Err(format!(
            "'{field}' must be an integer between 1 and {maximum}"
        ));
    }
    Ok(Some(value))
}

/// 从脚本中提取 workflow 名称（简单 heuristic：查找 `name:` 后的第一个引号字符串）
fn extract_workflow_name(script: &str) -> String {
    // 尝试匹配 name: '...' 或 name: "..."
    if let Some(pos) = script.find("name:") {
        let after = &script[pos + 5..];
        let trimmed = after.trim_start();
        if trimmed.starts_with('\'') || trimmed.starts_with('"') {
            let quote = trimmed.chars().next().unwrap();
            let start = 1;
            if let Some(end) = trimmed[1..].find(quote) {
                return trimmed[start..start + end].to_string();
            }
        }
    }
    "unnamed".to_string()
}

/// 将用户提供的 scriptPath 解析为安全路径。
///
/// 1. 转为以 cwd 为基准的绝对路径
/// 2. 规范化（解析 `..` 和符号链接）
/// 3. 验证路径在 cwd 子树内，拒绝越权访问
fn resolve_script_path(
    raw: &str,
    cwd_canonical: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let path = std::path::PathBuf::from(raw);
    let abs = if path.is_absolute() {
        path
    } else {
        cwd_canonical.join(&path)
    };
    let canonical = abs
        .canonicalize()
        .map_err(|e| format!("path not found: {e}"))?;
    if !canonical.starts_with(cwd_canonical) {
        return Err(format!("path '{}' is outside the working directory", raw));
    }
    Ok(canonical)
}

/// 从已校验的路径读取脚本内容（TOCTOU 收敛：读取只经这一个 handle）。
///
/// 先 `open` 拿句柄，再复核「路径名重新解析后仍指向同一文件」：
/// - 路径名在 `resolve_script_path` 之后被替换为指向 cwd 外的 symlink ⇒ 复核失败；
/// - 复核通过后即使路径名再次被换走，读的仍是已校验 inode（句柄不随路径名变化）。
///
/// 非 Unix 平台没有 `dev/ino` 可用：退化为「复核时仍在 cwd 子树内」，并在读后再
/// 次复核路径归属（仍是 best-effort，文档记录该限制，不假装强保证）。
fn read_script_file(
    script_path: &std::path::Path,
    raw: &str,
    cwd_canonical: &std::path::Path,
) -> Result<String, String> {
    use std::io::Read as _;

    let mut file = std::fs::File::open(script_path).map_err(|e| e.to_string())?;
    let opened = file.metadata().map_err(|e| e.to_string())?;
    let recheck = script_path.canonicalize().map_err(|e| e.to_string())?;
    if !recheck.starts_with(cwd_canonical) {
        return Err(format!("path '{}' is outside the working directory", raw));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let current = std::fs::metadata(&recheck).map_err(|e| e.to_string())?;
        if (current.dev(), current.ino()) != (opened.dev(), opened.ino()) {
            return Err(
                "path changed during validation (refusing to read a different file)".to_string(),
            );
        }
    }
    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|e| e.to_string())?;
    #[cfg(not(unix))]
    if !script_path
        .canonicalize()
        .map(|path| path.starts_with(cwd_canonical))
        .unwrap_or(false)
    {
        return Err(
            "path changed during validation (refusing to read a different file)".to_string(),
        );
    }
    Ok(content)
}

/// 验证 run_id 安全性：合法的 UUID 且不含路径遍历字符。
pub(crate) fn is_safe_run_id(s: &str) -> bool {
    // 禁止路径遍历字符
    if s.contains("..") || s.contains('/') || s.contains('\\') {
        return false;
    }
    // 必须为合法 UUID
    uuid::Uuid::parse_str(s).is_ok()
}

#[cfg(test)]
#[path = "params_test.rs"]
mod tests;
