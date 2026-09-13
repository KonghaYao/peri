//! 输出落盘与任务日志文件（服务私有目录，位于工作区根内）。
//!
//! 两条独立路径，都不写工作区之外、不写只读 rootfs：
//!
//! 1. **工具截断输出落盘**（[`OutputPersist`]）：Grep/Bash 等工具在输出被截断时
//!    把完整内容写入私有目录，并返回源格式提示文本
//!    `\n\n[Full output saved to {path} — use Read tool to view complete content]`。
//!    源实现的文件名为 `peri-tool-output-{uuid}.txt`；独立 server 使用
//!    [`TOOL_OUTPUT_PREFIX`]，避免冒用 Peri 命名（已登记为后端差异）。
//! 2. **任务日志**（[`LogStore`]）：Bash 后台/提升任务的 stdout/stderr 追加日志，
//!    文件按**高熵不透明句柄**命名，broker 侧 `TaskHandle.stdout_log` 指向它，
//!    普通 `Read`（同一授权根视图）可直接读取。
//!
//! ## capability 校验（F2）
//!
//! 产物路径**不能**只靠"它本来在工作区里"这一假设：本机同 uid 的其它进程可以把
//! `.local-mcp/logs` 换成指向 `/tmp` 的符号链接，让随后所有落盘（日志与
//! `local-tool-output-*.txt`）静默写到授权根之外，而工具仍然报告原路径
//! （WP-009-r3 的 F2）。因此生产路径必须走 capability 层：
//!
//! - [`DirOutputPersist::rooted`] / [`LogStore::rooted`] 以 [`RootDir`] 为唯一解析入口，
//!   逐组件 `openat(O_NOFOLLOW|O_DIRECTORY)`；符号链接祖先、越界目标一律
//!   [`AccessError::Rejected`]；
//! - 建文件用 `O_CREAT|O_EXCL|O_NOFOLLOW`（绝不跟随被替换的末段）；
//! - 失败即 fail closed：**不写任何文件**，并给出明确错误文本。
//!
//! 句柄是读取日志的唯一键（`TaskLogRead` 必须同时给出 task id 与句柄），因此
//! 猜到 task id 也无法读取他人日志；句柄格式非法直接拒绝，避免路径穿越。

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::capability::{AccessError, Parents, RequestedPath, RootDir};

/// 工具截断输出落盘文件名前缀。
pub const TOOL_OUTPUT_PREFIX: &str = "local-tool-output-";

/// 日志句柄的十六进制字符数（两个 v4 UUID = 256 位熵，禁止截断）。
pub const LOG_HANDLE_HEX_LEN: usize = 64;

/// 日志流。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    /// 标准输出。
    Stdout,
    /// 标准错误。
    Stderr,
}

impl LogStream {
    /// 文件名后缀（`stdout`/`stderr`）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// 截断输出落盘 seam。
///
/// 生产实现是 [`DirOutputPersist`]（服务私有目录）；WP-007 在其 `output`
/// 模块具备等价能力后可以替换实现，但**不得**改变返回文本格式。
pub trait OutputPersist: Send + Sync {
    /// 写入完整内容，返回提示文本与落盘路径。
    ///
    /// 文案：`[Full output saved to {path} — use Read tool to view complete content]`；
    /// 写失败时降级为 `[Failed to save full output to {path}: {error}]`。
    fn persist(&self, full_content: &str) -> PersistOutcome;

    /// 写入前台超时前的部分输出，返回提示文本与落盘路径。
    ///
    /// 文案：`[Partial output saved to {path} — use Read tool to view captured output so far]`
    /// （源 `terminal.rs::persist_partial_output`，含 `partial output` 关键词）。
    fn persist_partial(&self, content: &str) -> PersistOutcome;
}

/// 落盘结果：提示文本 + 真实路径（失败时为 `None`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistOutcome {
    /// 追加到截断信息后的提示文本（含前导换行）。
    pub hint: String,
    /// 落盘路径；写失败时为 `None`。
    pub path: Option<PathBuf>,
}

/// capability 校验过的产物目录：`<授权根>/<subdir>`。
///
/// 这是 F2 的修复面——所有服务自产文件（截断落盘、任务日志）都必须经它解析，
/// 而不是直接用 `std::fs` 打开一个由字符串拼出来的路径。解析规则完全由
/// [`RootDir`] 决定：逐组件 `openat(O_NOFOLLOW|O_DIRECTORY)`，符号链接祖先与越界
/// 目标一律拒绝；建父目录也相对已持有的 fd（`mkdirat`），不存在二次解析窗口。
#[derive(Clone, Debug)]
pub struct RootedDir {
    root: Arc<RootDir>,
    subdir: String,
}

impl RootedDir {
    /// 以授权根与**相对根**的子目录构造（子目录不存在时按需创建）。
    pub fn new(root: Arc<RootDir>, subdir: impl Into<String>) -> Self {
        Self {
            root,
            subdir: subdir.into(),
        }
    }

    /// 展示用绝对目录（工作区根内的绝对路径；`Read` 的指引与返回文本都用它）。
    pub fn display_dir(&self) -> PathBuf {
        self.root.base().join(&self.subdir)
    }

    /// 相对根的子目录。
    pub fn subdir(&self) -> &str {
        &self.subdir
    }

    /// 解析 `<subdir>/<name>`（父目录按需创建）。
    fn child(&self, name: &str) -> Result<crate::capability::DirectChild, AccessError> {
        let relative = format!("{}/{name}", self.subdir);
        let requested = RequestedPath::parse(&relative, self.root.base()).map_err(|error| {
            AccessError::rejected(crate::error::CapabilityError::InvalidInput {
                message: error.public_message(),
            })
        })?;
        self.root.resolve_with(&requested, Parents::CreateMissing)
    }

    /// 在目录内新建文件（`O_CREAT|O_EXCL|O_NOFOLLOW`），返回工作区根内的绝对路径。
    ///
    /// 父目录按需创建；任何一步走不出 capability 判定即拒绝，**不写文件**。
    pub fn create_file(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, AccessError> {
        let child = self.child(name)?;
        child.create_new(bytes)?;
        Ok(child.display_path())
    }

    /// 打开目录内已存在的文件（只读；末段 `O_NOFOLLOW`）。
    pub fn open_read(&self, name: &str) -> Result<File, AccessError> {
        self.child(name)?.open_read()
    }

    /// 删除目录内的文件（末段 `unlinkat`）。
    pub fn remove_file(&self, name: &str) -> Result<(), AccessError> {
        match self.child(name)?.unlink() {
            Ok(()) => Ok(()),
            Err(error) if error.is_not_found() => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// 文件是否存在（不跟随符号链接）。
    pub fn exists(&self, name: &str) -> bool {
        match self.child(name) {
            Ok(child) => child.exists().unwrap_or(false),
            Err(_) => false,
        }
    }

    /// 以 `O_APPEND|O_NOFOLLOW` 打开刚创建的文件（追加写；不做 `O_CREAT`）。
    ///
    /// 走路径是因为 capability 层不暴露"打开并返回 fd"的写接口；安全性来自两点：
    /// 创建用 `O_EXCL|O_NOFOLLOW`（拒绝被替换的末段），追加不带 `O_CREAT`
    /// （目标不存在即失败，不会新建越界文件）。残留的祖先目录替换竞态只可能让追加
    /// 落在根内另一个已存在文件上（攻击者本就能写的位置），不构成越界写入。
    fn open_append_no_follow(&self, name: &str) -> io::Result<File> {
        let path = self.display_dir().join(name);
        let mut options = std::fs::OpenOptions::new();
        options.append(true).create(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        options
            .open(&path)
            .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
    }
}

/// 把 capability 失败投影成 `io::Error`（文本已脱敏，只含调用方已知的路径）。
fn capability_io(action: &'static str, requested: &str, error: AccessError) -> io::Error {
    let kind = if error.is_not_found() {
        io::ErrorKind::NotFound
    } else if error.is_rejected() {
        io::ErrorKind::PermissionDenied
    } else {
        io::ErrorKind::Other
    };
    io::Error::new(kind, format!("{action} {requested}: {}", error.message()))
}

/// 写入指定目录的 [`OutputPersist`]。
pub struct DirOutputPersist {
    dir: PathBuf,
    /// capability 校验目标；`Some` 时所有写入必须经授权根解析（生产路径）。
    rooted: Option<RootedDir>,
}

impl DirOutputPersist {
    /// 使用给定目录（不存在时按需创建）。
    ///
    /// **不经过 capability 校验**：仅用于测试夹具与显式给出绝对目录的宿主侧调用点；
    /// 服务内的产物路径必须用 [`DirOutputPersist::rooted`]。
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            rooted: None,
        }
    }

    /// 以授权根 + 相对子目录构造（F2：服务私有产物路径的唯一生产构造方式）。
    pub fn rooted(root: Arc<RootDir>, subdir: impl Into<String>) -> Self {
        let rooted = RootedDir::new(root, subdir);
        Self {
            dir: rooted.display_dir(),
            rooted: Some(rooted),
        }
    }

    /// 目录路径。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 是否走 capability 校验。
    pub fn is_capability_checked(&self) -> bool {
        self.rooted.is_some()
    }

    /// 单次落盘的公共实现（唯一产生提示文本的地方）。
    fn write(&self, content: &str, kind: PersistKind) -> PersistOutcome {
        let name = format!("{TOOL_OUTPUT_PREFIX}{}.txt", uuid::Uuid::new_v4());
        let path = self.dir.join(&name);
        let outcome = match &self.rooted {
            Some(rooted) => rooted
                .create_file(&name, content.as_bytes())
                .map_err(|error| capability_io("write", &path.to_string_lossy(), error)),
            None => {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(&path, content).map(|()| path.clone())
            }
        };
        match outcome {
            Ok(path) => PersistOutcome {
                hint: kind.success_hint(&path),
                path: Some(path),
            },
            Err(error) => PersistOutcome {
                hint: kind.failure_hint(&path, &error),
                path: None,
            },
        }
    }
}

/// 落盘语义（完整输出 / 前台超时的部分输出）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersistKind {
    /// 完整输出。
    Full,
    /// 前台超时前的部分输出。
    Partial,
}

impl PersistKind {
    fn success_hint(self, path: &Path) -> String {
        match self {
            Self::Full => format!(
                "\n\n[Full output saved to {} — use Read tool to view complete content]",
                path.display()
            ),
            Self::Partial => format!(
                "\n\n[Partial output saved to {} — use Read tool to view captured output so far]",
                path.display()
            ),
        }
    }

    fn failure_hint(self, path: &Path, error: &io::Error) -> String {
        match self {
            Self::Full => format!(
                "\n\n[Failed to save full output to {}: {error}]",
                path.display()
            ),
            Self::Partial => format!(
                "\n\n[Failed to save partial output to {}: {error}]",
                path.display()
            ),
        }
    }
}

impl OutputPersist for DirOutputPersist {
    fn persist(&self, full_content: &str) -> PersistOutcome {
        self.write(full_content, PersistKind::Full)
    }

    fn persist_partial(&self, content: &str) -> PersistOutcome {
        self.write(content, PersistKind::Partial)
    }
}

/// 生成高熵不透明日志句柄：两个 v4 UUID 的简单形式拼接（256 位）。
pub fn mint_log_handle() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// 句柄是否合法（小写十六进制、长度精确）；非法即拒绝，防止路径穿越。
pub fn is_valid_log_handle(handle: &str) -> bool {
    handle.len() == LOG_HANDLE_HEX_LEN
        && handle
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// 任务日志文件集合（服务私有；任务表只持有句柄与展示路径）。
///
/// 生产实现由 [`LogStore::rooted`] 构造：每个文件操作都经 capability 层解析，
/// 因此把日志目录替换成符号链接既不能让日志写到根外，也读不到根外的文件。
#[derive(Clone)]
pub struct LogStore {
    dir: PathBuf,
    /// capability 校验目标；`Some` 时所有文件操作必须经授权根解析。
    rooted: Option<RootedDir>,
}

impl LogStore {
    /// 使用给定目录（不存在时按需创建）。**不经过** capability 校验（测试夹具用）。
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            rooted: None,
        }
    }

    /// 以授权根 + 相对子目录构造（服务日志目录的唯一生产构造方式）。
    pub fn rooted(root: Arc<RootDir>, subdir: impl Into<String>) -> Self {
        let rooted = RootedDir::new(root, subdir);
        Self {
            dir: rooted.display_dir(),
            rooted: Some(rooted),
        }
    }

    /// 日志目录。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 是否走 capability 校验。
    pub fn is_capability_checked(&self) -> bool {
        self.rooted.is_some()
    }

    /// 日志文件名：`{handle}.{stdout|stderr}.log`（句柄非法即拒绝）。
    fn file_name(&self, handle: &str, stream: LogStream) -> io::Result<String> {
        if !is_valid_log_handle(handle) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid log handle",
            ));
        }
        Ok(format!("{handle}.{}.log", stream.as_str()))
    }

    /// 日志文件路径：`{dir}/{handle}.{stdout|stderr}.log`。
    pub fn path(&self, handle: &str, stream: LogStream) -> io::Result<PathBuf> {
        Ok(self.dir.join(self.file_name(handle, stream)?))
    }

    /// 创建任务日志文件并返回（工作区根内的绝对路径，追加写句柄）。
    ///
    /// - capability 模式：先解析（符号链接祖先/越界目标在此拒绝），再用
    ///   `O_CREAT|O_EXCL|O_NOFOLLOW` 建文件，最后以 `O_APPEND|O_NOFOLLOW` 持句柄；
    ///   任一步失败即返回错误，调用方必须**降级为不写日志**（绝不落到别处）。
    /// - 非 capability 模式：等价于 `create_dir_all` + `append(true).create(true)`。
    pub fn create(&self, handle: &str, stream: LogStream) -> io::Result<(PathBuf, File)> {
        let name = self.file_name(handle, stream)?;
        let path = self.dir.join(&name);
        match &self.rooted {
            Some(rooted) => {
                rooted
                    .create_file(&name, b"")
                    .map_err(|error| capability_io("create log", &path.to_string_lossy(), error))?;
                let file = rooted.open_append_no_follow(&name)?;
                Ok((path, file))
            }
            None => {
                std::fs::create_dir_all(&self.dir)?;
                Ok((path.clone(), self.open_append(handle, stream)?))
            }
        }
    }

    /// 创建/追加打开日志文件（同任务同名流共用，进程运行期间持续追加）。
    pub fn open_append(&self, handle: &str, stream: LogStream) -> io::Result<File> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path(handle, stream)?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    }

    /// 读取日志尾部：最多 `max_bytes`，按 UTF-8 边界回退对齐。
    pub fn read_tail(
        &self,
        handle: &str,
        stream: LogStream,
        max_bytes: usize,
    ) -> io::Result<LogChunk> {
        let name = self.file_name(handle, stream)?;
        let bytes = self.read_file(&name)?;
        let total = bytes.len();
        let start = total.saturating_sub(max_bytes);
        // 尾部窗口起点必须落在字符边界上。
        let mut start = start;
        while start < total && (bytes[start] & 0b1100_0000) == 0b1000_0000 {
            start += 1;
        }
        let content = String::from_utf8_lossy(&bytes[start..]).to_string();
        Ok(LogChunk {
            content,
            total_bytes: total as u64,
            truncated: start > 0,
        })
    }

    /// 按模式读取日志文件内容（capability / 直读路径的唯一汇合点）。
    fn read_file(&self, name: &str) -> io::Result<Vec<u8>> {
        match &self.rooted {
            Some(rooted) => {
                let mut file = rooted
                    .open_read(name)
                    .map_err(|error| capability_io("read log", name, error))?;
                let mut bytes = Vec::new();
                io::Read::read_to_end(&mut file, &mut bytes)?;
                Ok(bytes)
            }
            None => std::fs::read(self.dir.join(name)),
        }
    }

    /// 删除某任务的全部日志文件（TTL 回收/任务遗忘）。
    pub fn remove(&self, handle: &str) -> io::Result<()> {
        for stream in [LogStream::Stdout, LogStream::Stderr] {
            let name = self.file_name(handle, stream)?;
            match &self.rooted {
                Some(rooted) => rooted.remove_file(&name).map_err(|error| {
                    capability_io("remove log", &self.dir.join(&name).to_string_lossy(), error)
                })?,
                None => match std::fs::remove_file(self.dir.join(name)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                },
            }
        }
        Ok(())
    }

    /// 日志是否已存在（读取前存在性判断）。
    pub fn exists(&self, handle: &str, stream: LogStream) -> bool {
        let Ok(name) = self.file_name(handle, stream) else {
            return false;
        };
        match &self.rooted {
            Some(rooted) => rooted.exists(&name),
            None => self.dir.join(name).exists(),
        }
    }
}

/// 日志读取结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogChunk {
    /// 已读取内容（UTF-8 安全）。
    pub content: String,
    /// 该流日志总字节数。
    pub total_bytes: u64,
    /// 是否因上限只返回了尾部。
    pub truncated: bool,
}
