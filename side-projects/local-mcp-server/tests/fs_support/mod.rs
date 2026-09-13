//! FS 工具测试的公共夹具与调用入口（integration tests 通过 `#[path]` 引入）。
//!
//! 设计约束：
//! - 所有文件操作都发生在 `tempfile::TempDir` 里，绝不触碰真实用户文件；
//! - 授权根 = `<temp>/root`，越界目标 = `<temp>/outside`（同层兄弟目录，便于构造
//!   `../` 逃逸与前缀碰撞）；
//! - `FsRuntime` 是进程内执行器，这些测试就是在临时工作区根上真跑五工具的形态
//!   （单进程本机执行，无容器/无 worker）。

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use local_mcp_server::tools::fs::{FsCall, FsRuntime};
use local_mcp_server::wire::{RequestContext, ToolRequest, ToolResponse};
use serde_json::{json, Value};
use tempfile::TempDir;

/// 物化后的测试树。
pub struct Tree {
    /// 授权根（`<temp>/root`）。
    pub root: PathBuf,
    /// 授权根之外的兄弟目录（`<temp>/outside`），内含 `secret.txt`。
    pub outside: PathBuf,
    /// 保持临时目录存活。
    pub temp: TempDir,
}

impl Tree {
    /// 授权根的字符串形式（直接作为能力边界传入）。
    pub fn root_str(&self) -> String {
        self.root.to_string_lossy().to_string()
    }

    /// 根内路径的绝对形式。
    pub fn path(&self, relative: &str) -> String {
        self.root.join(relative).to_string_lossy().to_string()
    }

    /// 根外路径的绝对形式。
    pub fn outside_path(&self, relative: &str) -> String {
        self.outside.join(relative).to_string_lossy().to_string()
    }

    /// 授权根内的进程内执行器。
    pub fn runtime(&self) -> FsRuntime {
        FsRuntime::new(self.root.clone()).expect("授权根必须可打开")
    }

    /// 根内读取（测试自身用，绕过工具）。
    pub fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.root.join(relative)).expect("读取夹具文件")
    }
}

/// 按夹具描述物化 `tests/fixtures/fs/trees/<name>.json`。
pub fn tree(name: &str) -> Tree {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fs/trees")
        .join(format!("{name}.json"));
    let spec: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("读取树夹具"))
        .expect("树夹具必须是合法 JSON");

    let temp = TempDir::new().expect("创建临时目录");
    let root = temp.path().join("root");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&root).expect("创建授权根");
    std::fs::create_dir_all(&outside).expect("创建越界目录");
    std::fs::write(outside.join("secret.txt"), "OUTSIDE-SECRET-CONTENT\n")
        .expect("写入越界哨兵文件");

    for dir in spec["dirs"].as_array().cloned().unwrap_or_default() {
        std::fs::create_dir_all(root.join(dir.as_str().expect("目录名是字符串")))
            .expect("创建夹具目录");
    }
    if let Some(files) = spec["files"].as_object() {
        for (relative, content) in files {
            let target = root.join(relative);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).expect("创建文件父目录");
            }
            std::fs::write(&target, content.as_str().expect("文件内容是字符串"))
                .expect("写入夹具文件");
        }
    }
    if let Some(links) = spec["symlinks"].as_object() {
        for (relative, target) in links {
            let resolved = target
                .as_str()
                .expect("链接目标是字符串")
                .replace("$OUTSIDE", &outside.to_string_lossy());
            symlink(&resolved, &root.join(relative));
        }
    }

    Tree {
        root,
        outside,
        temp,
    }
}

/// 创建符号链接（Unix）。
pub fn symlink(target: &str, link: &Path) {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).expect("创建链接父目录");
    }
    let _ = std::fs::remove_file(link);
    std::os::unix::fs::symlink(target, link).expect("创建符号链接");
}

/// 在指定授权根上构造执行器（供前缀碰撞等需要自定义根的用例）。
pub fn runtime_at(root: &Path) -> FsRuntime {
    FsRuntime::new(root.to_path_buf()).expect("授权根必须可打开")
}

/// 调用一个 FS 工具（成功路径；协议级失败会 panic）。
pub fn call(runtime: &FsRuntime, tool: &str, path: Option<&str>, arguments: Value) -> ToolResponse {
    let call = FsCall {
        tool: tool.to_string(),
        arguments,
        path: path.map(str::to_string),
    };
    runtime
        .call(&call)
        .unwrap_or_else(|error| panic!("{tool} 调用应当返回工具结果，实际协议错误: {error:?}"))
}

/// 调用一个 FS 工具并断言它是业务错误，返回错误文本。
pub fn call_error(runtime: &FsRuntime, tool: &str, path: Option<&str>, arguments: Value) -> String {
    let response = call(runtime, tool, path, arguments);
    assert!(
        response.is_error,
        "{tool} 期望业务错误，实际成功: {}",
        response.text
    );
    response.text
}

/// 结构化字段读取（`extra` 展平在顶层）。
pub fn field(response: &ToolResponse, key: &str) -> Option<Value> {
    response.structured.get(key).cloned()
}

/// 结构化字段（字符串）。
pub fn field_str(response: &ToolResponse, key: &str) -> String {
    field(response, key)
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| panic!("结构化字段 {key} 缺失或不是字符串: {}", response.structured))
}

/// 常用参数构造：`{"file_path": ...}`。
pub fn path_args(path: &str) -> Value {
    json!({ "file_path": path })
}

/// 常用参数构造：`{"folder_path": ...}`。
pub fn folder_args(operation: &str, path: &str) -> Value {
    json!({ "operation": operation, "folder_path": path })
}

/// 收集目录下所有文件名（用于断言无副作用、无半成品）。
pub fn list_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// 递归收集 `key = value` 形态的摘要（用于目视与断言）。
pub fn snapshot(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    collect(dir, dir, &mut out);
    out
}

fn collect(base: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let relative = path
            .strip_prefix(base)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        let metadata = std::fs::symlink_metadata(&path).expect("元数据");
        if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(&path).expect("读链接");
            out.insert(relative, format!("symlink -> {}", target.display()));
        } else if metadata.is_dir() {
            out.insert(relative.clone(), "dir".to_string());
            collect(base, &path, out);
        } else {
            let content = std::fs::read_to_string(&path).unwrap_or_else(|_| "<binary>".to_string());
            out.insert(relative, content);
        }
    }
}

/// 构造一个大小为 `bytes` 的稀疏文件（用于 32 MiB 上限用例，避免写入真实数据）。
pub fn sparse_file(path: &Path, bytes: u64) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("创建父目录");
    }
    let file = std::fs::File::create(path).expect("创建稀疏文件");
    file.set_len(bytes).expect("设置文件长度");
}

// ───────────────────────────── 调用入口辅助 ─────────────────────────────

/// 组装一次工具调用请求（`name` 必须是 `&'static str`，与冻结契约一致）。
///
/// 执行入口是进程内执行内核（[`local_mcp_server::runtime::InProcessExecutor`]）：
/// 测试用真实的临时工作区根驱动它，不再有"脚本化 worker 替身"这一类对象
/// （容器期的 RPC 通道已随 D-003 删除）。
pub fn tool_request(tool: &'static str, arguments: Value) -> ToolRequest {
    ToolRequest {
        name: tool,
        arguments,
        context: RequestContext::new("req-1", "principal-a", "instance-1"),
    }
}
