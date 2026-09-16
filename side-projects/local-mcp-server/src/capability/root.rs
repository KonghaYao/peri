//! 以目录 fd 为锚的授权根：`openat` 逐组件解析，杜绝 canonicalize→open 的 TOCTOU 窗口。
//!
//! ## 为什么不是 `canonicalize` + `open`
//!
//! 源实现（`peri-middlewares/src/tools/filesystem/mod.rs:27`、`transaction.rs:96`）先
//! `canonicalize` 再按字符串打开：两次系统调用之间存在窗口，本机上并发进程（模型自己的
//! Bash 就能做到）可以在窗口内把目录换成指向根外的符号链接，让后续 `open`/`create_dir_all`
//! 逃出授权根。
//!
//! 本模块改为一律相对**已验证的目录 fd** 打开：
//!
//! - 每个中间段都用 `openat(cur, seg, O_RDONLY|O_NOFOLLOW|O_DIRECTORY)` 打开，`O_NOFOLLOW`
//!   让符号链接在这一步就失败（`ELOOP`），拿到 fd 后再 `fstat` 确认是目录；
//! - 末段解析为「父目录 fd + 名字」，后续操作 `openat(parent, name, …|O_NOFOLLOW)`，
//!   解析与使用之间不再有可被替换的路径字符串；
//! - `..` **不交给内核**（内核的 `..` 会越过挂载点）：弹出 fd 栈，空栈再弹即越界；
//! - 根内符号链接会被解析（对齐源 `resolve_path` 的 canonicalize 行为），但目标为绝对
//!   路径或经 `..` 越出根即拒绝（`RESOLVE_BENEATH` 语义）；符号链接预算 40 次（与内核
//!   `ELOOP` 一致），防环。
//!
//! 遍历（Glob / `folder_operations=deep_scan`）同样走 fd：每个子目录都用相对父 fd 的
//! `openat` 进入，条目类型取 `fstatat(..., AT_SYMLINK_NOFOLLOW)`（与源实现
//! `DirEntry::metadata()`／`walkdir(follow_links=false)` 的 lstat 语义一致）。

use std::collections::VecDeque;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::path::{join_components, RequestedPath};
use super::{invalid_input, outside_root, symlink_escape, AccessError, CapabilityError};

/// 符号链接解析预算：与内核 `ELOOP` 的 40 次一致，防环。
const MAX_SYMLINK_HOPS: usize = 40;

/// `std::fs::read_to_string` 在非 UTF-8 输入上的错误文案（保持逐字一致）。
const INVALID_UTF8_MESSAGE: &str = "stream did not contain valid UTF-8";

/// 缺失中间目录的处理策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parents {
    /// 中间目录必须已存在（Read / Edit / Glob / folder 的检查语义）。
    MustExist,
    /// 中间目录缺失即创建（Write 与落盘产物的提交语义，等价源实现 `create_dir_all`）。
    CreateMissing,
}

/// 授权根目录句柄。
#[derive(Debug)]
pub struct RootDir {
    dir: OwnedFd,
    base: PathBuf,
    /// 授权根的**真实路径**（`canonicalize` 结果；失败时退回 `base`）。
    ///
    /// 只用于判定「绝对符号链接目标是否落在根内」：展示路径与错误载荷仍用 `base`，
    /// 因此调用方传入的路径形态不会被悄悄改写。
    real_base: PathBuf,
}

impl RootDir {
    /// 打开授权根（必须存在且是目录；失败即 fail closed）。
    pub fn open(base: impl Into<PathBuf>) -> Result<Self, AccessError> {
        let base = base.into();
        let raw = path_to_cstring(&base)?;
        let fd = unsafe {
            libc::open(
                raw.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(AccessError::io(
                "open workspace root",
                base.to_string_lossy(),
                io::Error::last_os_error(),
            ));
        }
        let real_base = std::fs::canonicalize(&base).unwrap_or_else(|_| base.clone());
        Ok(Self {
            dir: unsafe { OwnedFd::from_raw_fd(fd) },
            base,
            real_base,
        })
    }

    /// 授权根的真实路径（`canonicalize`；仅用于边界判定，见字段文档）。
    pub fn real_base(&self) -> &Path {
        &self.real_base
    }

    /// 根的宿主绝对路径（非敏感，可用于错误载荷的 `root` 字段；该字段从不外发）。
    pub fn base(&self) -> &Path {
        &self.base
    }

    fn root_fd(&self) -> RawFd {
        self.dir.as_raw_fd()
    }

    /// 把调用方的路径字符串解析为根内**宿主绝对路径**（宿主路径单表示）。
    ///
    /// 两种输入形态（与源实现「相对 cwd」的调用习惯一致）：
    /// - 绝对路径：必须**逐组件**以授权根为前缀（禁止字符串前缀），越界即
    ///   [`CapabilityError::OutsideRoot`]；
    /// - 相对路径：一律相对授权根（`.`、空组件即根本身；`..` 越出根即拒绝）。
    ///
    /// **有意比源实现更严**（登记差异 D-3）：绝对路径按**组件前缀**判定，因此「指向根的
    /// 符号链接别名」形态（例如根以 `/real/ws` 打开、调用方写 `/alias/ws/x`）会被拒绝——
    /// 源实现先 `canonicalize` 再按字符串前缀比较，会接受这种别名。这里不放宽：根的符号
    /// 链接在启动期（`main.rs`）已解析，运行期只认这一条真实路径。
    ///
    /// 这里只做**词法**归一（含 `..` 消解），不做 FS 访问：真实路径（符号链接、竞态）
    /// 一律由 [`RootDir::resolve`] / [`RootDir::walk`] 在已持有的目录 fd 上判定，
    /// 因此调用方拿到本函数的产物后仍要经 fd 解析才能操作文件系统。
    pub fn resolve_host_path(&self, requested: &str) -> Result<PathBuf, CapabilityError> {
        let parsed = RequestedPath::parse(requested, &self.base)?;
        if Path::new(requested).is_absolute() {
            let base_components = super::path::components_of(&self.base);
            if !super::path::starts_with_components(parsed.components(), &base_components) {
                return Err(outside_root(requested, &self.base));
            }
            Ok(join_under(
                &self.base,
                &parsed.components()[base_components.len()..],
            ))
        } else {
            Ok(join_under(&self.base, parsed.components()))
        }
    }

    /// 把**绝对**宿主路径解析为根内相对请求路径（校验授权根前缀）。
    ///
    /// 这是唯一允许把线上路径字符串送进 capability 判定的入口：先做词法归一
    /// （含 `..` 消解与越界拒绝），再逐组件确认它确实以授权根为前缀——绝对路径的越界
    /// 只有在这一步才能判定（`RequestedPath::parse` 不知道根在哪里）。
    pub fn request_path(&self, absolute: &str) -> Result<RequestedPath, AccessError> {
        let parsed = RequestedPath::parse(absolute, &self.base).map_err(AccessError::rejected)?;
        let base_components = super::path::components_of(&self.base);
        if !super::path::starts_with_components(parsed.components(), &base_components) {
            return Err(AccessError::rejected(outside_root(absolute, &self.base)));
        }
        Ok(RequestedPath::from_components(
            &parsed.components()[base_components.len()..],
            &self.base,
        ))
    }

    /// 把**绝对**符号链接目标表达为「授权根内的相对组件」；无法证明在根内返回 `None`。
    ///
    /// 两条证据路径（都要求逐组件落在授权根的**真实路径** [`RootDir::real_base`] 之下）：
    /// 1. 目标字符串本身（词法归一后）就在根下——覆盖 `-> /real/root/sub/file`；
    /// 2. 目标字符串的 `realpath` 在根下——覆盖经由符号链接祖先写出的绝对路径
    ///    （例如根以 `/private/var/...` 打开、链接写成 `/var/...`）。
    ///
    /// 判定只用于「是否继续解析」，绝不用于后续操作：返回的相对组件仍会由本函数的调用方
    /// 逐组件 `openat(O_NOFOLLOW)` 重新解析，因此检查与使用之间没有可被替换的路径字符串。
    fn symlink_target_under_root(&self, target: &Path) -> Option<Vec<String>> {
        let base = super::path::components_of(&self.real_base);
        let lexical = super::path::components_of(target);
        if super::path::starts_with_components(&lexical, &base) {
            // 绝对目标的 `..` 已在 `components_of` 里消解；消解到 `/` 之上会得到比根更短的
            // 组件序列，前缀比较自然失败。
            return Some(lexical[base.len()..].to_vec());
        }
        let resolved = std::fs::canonicalize(target).ok()?;
        let resolved = super::path::components_of(&resolved);
        if super::path::starts_with_components(&resolved, &base) {
            return Some(resolved[base.len()..].to_vec());
        }
        None
    }

    /// 解析请求路径为「父目录 fd + 末段名字」；末段**允许不存在**（创建路径）。
    pub fn resolve(&self, requested: &RequestedPath) -> Result<DirectChild, AccessError> {
        self.resolve_with(requested, Parents::MustExist)
    }

    /// 解析请求路径，并按 `parents` 策略决定是否创建缺失的中间目录。
    pub fn resolve_with(
        &self,
        requested: &RequestedPath,
        parents: Parents,
    ) -> Result<DirectChild, AccessError> {
        if requested.is_root() {
            return Err(AccessError::rejected(invalid_input(
                "Path refers to the workspace root, which is not a file.",
            )));
        }
        let mut session = ResolveSession::new(self)?;
        let mut pending: VecDeque<String> = requested.components().iter().cloned().collect();
        let mut hops = 0usize;
        let components = requested.components();

        loop {
            let Some(component) = pending.pop_front() else {
                return session.finish(components);
            };
            if component == "." {
                continue;
            }
            if component == ".." {
                if !session.pop() {
                    return Err(AccessError::rejected(outside_root(
                        requested.raw(),
                        &self.base,
                    )));
                }
                continue;
            }
            match open_dir_at(session.current_fd(), &component) {
                Ok(fd) => session.push(fd, component),
                Err(error) => match error.raw_os_error() {
                    Some(libc::ENOENT) if pending.is_empty() => {
                        return session.finish_with_new(component, components);
                    }
                    Some(libc::ENOENT) => {
                        if parents == Parents::CreateMissing {
                            // 源实现由 create_dir_all 在提交阶段建父目录；这里在解析阶段
                            // 就地建，保证后续每一步都相对已持有的 fd（无 TOCTOU 窗口）。
                            let current = session.current_fd();
                            mkdir_at(current, &component).map_err(|error| {
                                AccessError::io("create directory", requested.raw(), error)
                            })?;
                            let fd = open_dir_at(current, &component).map_err(|error| {
                                AccessError::io("create directory", requested.raw(), error)
                            })?;
                            session.push(fd, component);
                            continue;
                        }
                        return Err(AccessError::not_found(requested.raw()));
                    }
                    _ => {
                        // 可能是符号链接（O_NOFOLLOW|O_DIRECTORY 下为 ELOOP），也可能是
                        // 普通文件/特殊文件（ENOTDIR）。先读链接，再决定。
                        let name = to_cstring(&component)?;
                        match read_link_at(session.current_fd(), &name) {
                            Ok(target) => {
                                hops += 1;
                                if hops > MAX_SYMLINK_HOPS {
                                    return Err(AccessError::rejected(symlink_escape(
                                        requested.raw(),
                                        &session.preview_path(&target),
                                    )));
                                }
                                // 绝对目标：只有能证明它落在授权根的**真实路径**之下才解析
                                // （与源实现 `canonicalize` 跟随绝对链接的行为一致）；其余一律
                                // 拒绝。判定是词法 + realpath 复核，操作仍然只走 fd。
                                if target.is_absolute() {
                                    let Some(suffix) = self.symlink_target_under_root(&target)
                                    else {
                                        return Err(AccessError::rejected(symlink_escape(
                                            requested.raw(),
                                            &session.preview_path(&target),
                                        )));
                                    };
                                    session.reset_to_root();
                                    for component in suffix.into_iter().rev() {
                                        pending.push_front(component);
                                    }
                                    continue;
                                }
                                splice_link_target(&mut pending, &target, requested.raw())?;
                            }
                            Err(link_error) if link_error.raw_os_error() == Some(libc::EINVAL) => {
                                // 不是符号链接：普通文件或特殊文件。
                                if pending.is_empty() {
                                    return session.finish_with_new(component, components);
                                }
                                return Err(AccessError::io(
                                    "open directory",
                                    requested.raw(),
                                    error,
                                ));
                            }
                            Err(link_error) => {
                                return Err(AccessError::io("open", requested.raw(), link_error));
                            }
                        }
                    }
                },
            }
        }
    }

    /// 解析并要求末段存在，返回已打开的条目（符号链接在解析阶段跟随）。
    pub fn open_entry(&self, requested: &RequestedPath) -> Result<OpenedEntry, AccessError> {
        let child = self.resolve(requested)?;
        let file = child.open_read()?;
        Ok(OpenedEntry { child, file })
    }

    /// 解析一个已存在的目录（遍历起点、Glob 的 `path`、folder 的 list/deep_scan）。
    pub fn open_dir(&self, requested: &RequestedPath) -> Result<DirHandle, AccessError> {
        if requested.is_root() {
            // 根分支也必须给出**独立的 open file description**：`root_dup()` 只是 `dup(2)`，
            // 与根 fd 共享目录偏移（一次 readdir 到 EOF 后偏移停末尾），后续枚举会静默为空。
            // `openat(root_fd, ".")` 每次都新开描述符、偏移为 0，语义与非根分支一致。
            let fd = open_dir_at(self.root_fd(), ".")
                .map_err(|error| AccessError::io("open directory", requested.raw(), error))?;
            return Ok(DirHandle::new(fd, self.base.clone()));
        }
        let child = self.resolve(requested)?;
        child.open_dir()
    }

    /// 请求路径是否存在（授权根本身恒存在）。
    pub fn exists(&self, requested: &RequestedPath) -> Result<bool, AccessError> {
        if requested.is_root() {
            return Ok(true);
        }
        self.resolve(requested)?.exists()
    }

    /// 请求路径是否是目录（符号链接已跟随）。
    pub fn entry_is_dir(&self, requested: &RequestedPath) -> Result<bool, AccessError> {
        if requested.is_root() {
            return Ok(true);
        }
        Ok(self.resolve(requested)?.lstat()?.is_dir())
    }

    /// 递归创建目录（源实现 `create_dir_all` 的语义，但每一步都用 `openat` 锚定）。
    pub fn create_dir_all(&self, requested: &RequestedPath) -> Result<(), AccessError> {
        let mut session = ResolveSession::new(self)?;
        for component in requested.components() {
            let current = session.current_fd();
            match open_dir_at(current, component) {
                Ok(fd) => session.push(fd, component.clone()),
                Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                    mkdir_at(current, component).map_err(|error| {
                        AccessError::io("create directory", requested.raw(), error)
                    })?;
                    let fd = open_dir_at(current, component).map_err(|error| {
                        AccessError::io("create directory", requested.raw(), error)
                    })?;
                    session.push(fd, component.clone());
                }
                Err(error) => {
                    // 已存在但不是目录（或符号链接）→ 与源实现一样让 create_dir_all 报错。
                    return Err(AccessError::io("create directory", requested.raw(), error));
                }
            }
        }
        Ok(())
    }

    /// 单层创建目录（`recursive=false`）；目标是导出根本身时按 `EEXIST` 失败（源语义）。
    pub fn create_dir(&self, requested: &RequestedPath) -> Result<(), AccessError> {
        if requested.is_root() {
            return Err(AccessError::io(
                "create directory",
                requested.raw(),
                io::Error::from_raw_os_error(libc::EEXIST),
            ));
        }
        self.resolve(requested)?.create_dir(requested.raw())
    }

    /// 深度优先遍历（readdir 顺序，不跟随符号链接）。
    ///
    /// - `depth` 以 `start` 为 0（与 `walkdir::WalkDir::new(start)` 一致）；
    /// - `rel` 始终相对**授权根**，调用方据此复现 `strip_prefix(base)` 的显示路径；
    /// - 元数据取 `lstat`；`lstat` 失败的条目跳过（对齐源实现跳过遍历错误的行为）。
    pub fn walk(
        &self,
        start: &[String],
        max_depth: Option<usize>,
        visit: &mut dyn FnMut(WalkEntry<'_>) -> WalkControl,
    ) -> Result<(), AccessError> {
        let start_requested = RequestedPath::from_components(start, &self.base);
        match self.open_dir(&start_requested) {
            Ok(start_dir) => walk_into(start_dir, join_components(start), 0, max_depth, visit),
            Err(error) => {
                // 与 `walkdir` 对文件根的行为对齐：产出该条目本身，不下钻。
                let child = self.resolve(&start_requested)?;
                let metadata = child.lstat()?;
                let rel = join_components(start);
                let name = child.name().to_string();
                let _ = visit(WalkEntry {
                    rel: &rel,
                    depth: 0,
                    name: &name,
                    metadata: &metadata,
                });
                let _ = error;
                Ok(())
            }
        }
    }

    /// 在根内新建文件并写入（父目录按需创建）；返回**宿主**绝对路径。
    pub fn write_new_file(
        &self,
        requested: &RequestedPath,
        bytes: &[u8],
    ) -> Result<PathBuf, AccessError> {
        let child = self.resolve_with(requested, Parents::CreateMissing)?;
        child.create_new(bytes)?;
        Ok(child.display_path())
    }

    /// 就地改写根内**已存在**文件的文本内容；返回是否真的改写过（GAP-015/R4-02）。
    ///
    /// 用途：路径列举类工具（Glob/Grep/`folder_operations`）的截断产物内容由绝对路径构成，
    /// 而交付文本与 `persisted_path` 用宿主表示——两者不一致时「use Read tool」的指引会给出
    /// 宿主上不存在的路径。`rewrite` 由调用方提供（与交付文本同一次前缀改写）。
    ///
    /// 安全性质（与同文件其他写路径同源）：
    /// - 目标必须已存在且在授权根内：逐组件 `openat(O_NOFOLLOW|O_DIRECTORY)` 解析，
    ///   符号链接祖先/越界目标一律 [`AccessError::Rejected`]；
    /// - 写**同目录**临时文件（`O_CREAT|O_EXCL|O_NOFOLLOW`）后用 `renameat` 原子替换：
    ///   产物不会出现"写了一半"的中间态，且 `renameat` **不跟随**末段符号链接
    ///   （替换的是链接本身）；
    /// - 任何一步失败都**不动**原文件：调用方据错误降级为"内容保持原样"。
    /// - 内容不是合法 UTF-8 时不做改写（不是文本产物）。
    pub fn replace_entry_contents(
        &self,
        requested: &RequestedPath,
        rewrite: impl FnOnce(&str) -> String,
    ) -> Result<bool, AccessError> {
        let target = self.resolve_with(requested, Parents::MustExist)?;
        let mut file = target.open_read()?;
        let mut bytes: Vec<u8> = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes)
            .map_err(|error| AccessError::io("read", requested.raw(), error))?;
        let Ok(content) = String::from_utf8(bytes) else {
            return Ok(false);
        };
        let rewritten = rewrite(&content);
        if rewritten == content {
            return Ok(false);
        }
        // 临时文件与目标同目录，`renameat` 因此是同目录原子替换。
        let parent = join_components(&target.parent_rel());
        let temp_name = format!(".rewrite-{}.tmp", uuid::Uuid::new_v4().simple());
        let temp_relative = if parent.as_os_str().is_empty() {
            temp_name.clone()
        } else {
            format!("{}/{temp_name}", parent.to_string_lossy())
        };
        let temp_requested =
            RequestedPath::parse(&temp_relative, &self.base).map_err(AccessError::rejected)?;
        let temp = self.resolve_with(&temp_requested, Parents::CreateMissing)?;
        temp.create_new(rewritten.as_bytes())?;
        match target.rename_from(&temp_name) {
            Ok(()) => Ok(true),
            Err(error) => {
                // 替换失败：清理临时文件，原文件逐字不变。
                let _ = temp.unlink();
                Err(error)
            }
        }
    }

    fn root_dup(&self) -> Result<OwnedFd, AccessError> {
        dup_fd(self.root_fd())
    }
}

/// 已打开的条目：fd 已就位，后续读取不再经过路径。
#[derive(Debug)]
pub struct OpenedEntry {
    child: DirectChild,
    file: File,
}

impl OpenedEntry {
    /// 目标元数据（符号链接已跟随，等价源实现 `fs::metadata`）。
    pub fn metadata(&self) -> io::Result<std::fs::Metadata> {
        self.file.metadata()
    }

    /// 相对授权根的路径（锁键与显示用）。
    pub fn rel_path(&self) -> PathBuf {
        self.child.rel_path()
    }

    /// 源实现 `fs::read_to_string` 的等价读取（非 UTF-8 报错文案逐字一致）。
    pub fn read_to_string(&mut self) -> io::Result<String> {
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut self.file, &mut bytes)?;
        String::from_utf8(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, INVALID_UTF8_MESSAGE))
    }

    /// 读取原始字节（append 需要保留非 UTF-8 原字节）。
    pub fn read_bytes(&mut self) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut self.file, &mut bytes)?;
        Ok(bytes)
    }

    /// 同目录兄弟目标（tmp 文件与原子 rename）。
    pub fn sibling(&self, name: &str) -> Result<DirectChild, AccessError> {
        self.child.sibling(name)
    }
}

/// 已解析的目录句柄（fd 锚定；路径字符串只用于展示）。
#[derive(Debug)]
pub struct DirHandle {
    dir: OwnedFd,
    path: PathBuf,
}

impl DirHandle {
    fn new(dir: OwnedFd, path: PathBuf) -> Self {
        Self { dir, path }
    }

    /// 展示用绝对路径（宿主根 + 根内相对路径）。
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn fd(&self) -> RawFd {
        self.dir.as_raw_fd()
    }

    /// 列出目录条目（readdir 顺序，`lstat` 元数据）。
    pub fn entries(&self) -> Result<Vec<RawDirEntry>, AccessError> {
        read_dir_entries(self.fd(), &self.path)
    }
}

/// 一次 `lstat` 结果：名字 + 元数据（不跟随符号链接）。
#[derive(Debug, Clone)]
pub struct RawDirEntry {
    /// 条目名（不含路径）。
    pub name: String,
    /// `lstat` 元数据。
    pub metadata: EntryMeta,
}

/// `lstat` 元数据（不依赖 `std::fs::Metadata` 的构造限制，字段按需暴露）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryMeta {
    mode: libc::mode_t,
    size: u64,
    modified: Option<SystemTime>,
}

impl EntryMeta {
    /// 是否目录（符号链接为 `false`，与 `DirEntry::metadata()` 一致）。
    pub fn is_dir(&self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFDIR
    }

    /// 是否普通文件（符号链接为 `false`）。
    pub fn is_file(&self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFREG
    }

    /// 是否符号链接。
    pub fn is_symlink(&self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFLNK
    }

    /// 字节大小（源实现 `metadata.len()`）。
    pub fn len(&self) -> u64 {
        self.size
    }

    /// 是否空文件。
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// 权限位（含 setuid/setgid/sticky）。
    pub fn mode(&self) -> u32 {
        (self.mode & 0o7777) as u32
    }

    /// 修改时间；负时间戳（挂载元数据异常）为 `None`，调用方渲染 `unknown`。
    pub fn modified(&self) -> Option<SystemTime> {
        self.modified
    }

    fn from_stat(stat: &libc::stat) -> Self {
        Self {
            mode: stat.st_mode,
            size: stat.st_size.max(0) as u64,
            modified: system_time(stat.st_mtime, stat.st_mtime_nsec),
        }
    }
}

/// 「父目录 fd + 末段名字」：写/删/改名都基于它，不再重新解析路径字符串。
#[derive(Debug)]
pub struct DirectChild {
    parent: OwnedFd,
    name: String,
    rel: Vec<String>,
    base: PathBuf,
}

impl DirectChild {
    /// 相对授权根的路径。
    pub fn rel_path(&self) -> PathBuf {
        join_components(&self.rel)
    }

    /// 宿主绝对路径（展示用）。
    pub fn display_path(&self) -> PathBuf {
        self.directory().join(&self.name)
    }

    /// 末段名字。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 所在目录的宿主绝对路径。
    pub fn directory(&self) -> PathBuf {
        let mut path = self.base.clone();
        for component in self.parent_rel() {
            path.push(component);
        }
        path
    }

    /// 父目录相对根的组件。
    pub fn parent_rel(&self) -> Vec<String> {
        let mut rel = self.rel.clone();
        rel.pop();
        rel
    }

    fn parent_fd(&self) -> RawFd {
        self.parent.as_raw_fd()
    }

    /// 打开为可读文件（`O_RDONLY|O_NOFOLLOW|O_NONBLOCK`）。
    ///
    /// `O_NONBLOCK` 防止在命名管道上阻塞（探测阶段不想等生产者）；正常文件不受影响。
    /// 解析阶段已跟随符号链接，因此此处 `ELOOP` 只可能来自「解析后被换成符号链接」的竞态。
    pub fn open_read(&self) -> Result<File, AccessError> {
        let name = to_cstring(&self.name)?;
        let fd = unsafe {
            libc::openat(
                self.parent_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(self.error("open", io::Error::last_os_error()));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// 打开为目录句柄。
    pub fn open_dir(&self) -> Result<DirHandle, AccessError> {
        let name = to_cstring(&self.name)?;
        let fd = openat_fd(
            self.parent_fd(),
            &name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
        .map_err(|error| self.error("open directory", error))?;
        Ok(DirHandle::new(fd, self.display_path()))
    }

    /// `lstat` 元数据（不跟随符号链接）。
    pub fn lstat(&self) -> Result<EntryMeta, AccessError> {
        let name = to_cstring(&self.name)?;
        lstat_at(self.parent_fd(), &name).map_err(|error| self.error("inspect", error))
    }

    /// 末段是否存在（不跟随符号链接）。
    ///
    /// 「不存在」在能力层是 [`AccessError::NotFound`]：工具需要的「存在性回答」在这里
    /// 转成 `Ok(false)`，而不是让调用方处理错误。
    pub fn exists(&self) -> Result<bool, AccessError> {
        match self.lstat() {
            Ok(_) => Ok(true),
            Err(AccessError::NotFound { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// `O_CREAT|O_EXCL` 新建并写入（tmp 文件路径）。
    pub fn create_new(&self, bytes: &[u8]) -> Result<(), AccessError> {
        let name = to_cstring(&self.name)?;
        let fd = unsafe {
            libc::openat(
                self.parent_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(self.error("create", io::Error::last_os_error()));
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        io::Write::write_all(&mut file, bytes).map_err(|error| self.error("write", error))
    }

    /// 设置权限位（源实现把已存在目标的权限位复制到 tmp 再 rename）。
    pub fn set_mode(&self, mode: u32) -> Result<(), AccessError> {
        let name = to_cstring(&self.name)?;
        let rc = unsafe {
            libc::fchmodat(
                self.parent_fd(),
                name.as_ptr(),
                (mode & 0o7777) as libc::mode_t,
                0,
            )
        };
        if rc != 0 {
            return Err(self.error("chmod", io::Error::last_os_error()));
        }
        Ok(())
    }

    /// 删除末段（tmp 清理）。
    pub fn unlink(&self) -> Result<(), AccessError> {
        let name = to_cstring(&self.name)?;
        let rc = unsafe { libc::unlinkat(self.parent_fd(), name.as_ptr(), 0) };
        if rc != 0 {
            return Err(self.error("remove", io::Error::last_os_error()));
        }
        Ok(())
    }

    /// 用同目录的 `src_name` 原子替换末段（`renameat`，tmp → 目标）。
    pub fn rename_from(&self, src_name: &str) -> Result<(), AccessError> {
        let from = to_cstring(src_name)?;
        let to = to_cstring(&self.name)?;
        let rc = unsafe {
            libc::renameat(
                self.parent_fd(),
                from.as_ptr(),
                self.parent_fd(),
                to.as_ptr(),
            )
        };
        if rc != 0 {
            return Err(self.error("rename", io::Error::last_os_error()));
        }
        Ok(())
    }

    /// 创建目录（`folder_operations=create` 的非递归分支）。
    pub fn create_dir(&self, requested: &str) -> Result<(), AccessError> {
        let name = to_cstring(&self.name)?;
        let rc = unsafe { libc::mkdirat(self.parent_fd(), name.as_ptr(), 0o755) };
        if rc != 0 {
            return Err(AccessError::io(
                "create directory",
                requested,
                io::Error::last_os_error(),
            ));
        }
        Ok(())
    }

    /// 同目录兄弟目标。
    pub fn sibling(&self, name: &str) -> Result<DirectChild, AccessError> {
        let mut rel = self.parent_rel();
        rel.push(name.to_string());
        Ok(DirectChild {
            parent: dup_fd(self.parent_fd())?,
            name: name.to_string(),
            rel,
            base: self.base.clone(),
        })
    }

    fn error(&self, action: &'static str, error: io::Error) -> AccessError {
        match error.raw_os_error() {
            Some(libc::ELOOP) | Some(libc::EMLINK) => AccessError::rejected(symlink_escape(
                &self.rel_path().to_string_lossy(),
                &self.display_path(),
            )),
            Some(libc::ENOENT) => {
                AccessError::not_found(self.rel_path().to_string_lossy().to_string())
            }
            _ => AccessError::io(action, self.rel_path().to_string_lossy(), error),
        }
    }
}

/// 遍历回调对每个条目的处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkControl {
    /// 目录条目照常下钻。
    Continue,
    /// 目录条目不下钻（文件条目无影响）。
    SkipDescend,
    /// 提前结束整个遍历。
    Stop,
}

/// 遍历条目：相对授权根的路径、深度、名字与 `lstat` 元数据。
#[derive(Debug)]
pub struct WalkEntry<'a> {
    /// 相对授权根的路径。
    pub rel: &'a Path,
    /// 相对遍历起点的深度（起点 = 0）。
    pub depth: usize,
    /// 条目名。
    pub name: &'a str,
    /// `lstat` 元数据（不跟随符号链接）。
    pub metadata: &'a EntryMeta,
}

impl WalkEntry<'_> {
    /// 是否目录（符号链接为 `false`）。
    pub fn is_dir(&self) -> bool {
        self.metadata.is_dir()
    }

    /// 是否普通文件（符号链接为 `false`）。
    pub fn is_file(&self) -> bool {
        self.metadata.is_file()
    }
}

fn walk_into(
    dir: DirHandle,
    rel: PathBuf,
    depth: usize,
    max_depth: Option<usize>,
    visit: &mut dyn FnMut(WalkEntry<'_>) -> WalkControl,
) -> Result<(), AccessError> {
    for entry in dir.entries()? {
        let entry_rel = rel.join(&entry.name);
        let control = visit(WalkEntry {
            rel: &entry_rel,
            depth,
            name: &entry.name,
            metadata: &entry.metadata,
        });
        if control == WalkControl::Stop {
            return Ok(());
        }
        if control == WalkControl::Continue
            && entry.metadata.is_dir()
            && max_depth.is_none_or(|limit| depth < limit)
        {
            let name = to_cstring(&entry.name)?;
            let Ok(fd) = openat_fd(
                dir.fd(),
                &name,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            ) else {
                // 条目在遍历期间消失/被替换：跳过该子树（对齐源实现跳过遍历错误）。
                continue;
            };
            let child = DirHandle::new(fd, dir.path().join(&entry.name));
            walk_into(child, entry_rel, depth + 1, max_depth, visit)?;
        }
    }
    Ok(())
}

/// 解析会话：维护 fd 栈（`..` 弹栈而非交给内核）。
struct ResolveSession<'a> {
    root: &'a RootDir,
    dirs: Vec<OwnedFd>,
    names: Vec<String>,
}

impl<'a> ResolveSession<'a> {
    fn new(root: &'a RootDir) -> Result<Self, AccessError> {
        Ok(Self {
            root,
            dirs: vec![root.root_dup()?],
            names: Vec::new(),
        })
    }

    fn current_fd(&self) -> RawFd {
        self.dirs.last().expect("fd 栈永不为空").as_raw_fd()
    }

    fn push(&mut self, fd: OwnedFd, name: String) {
        self.dirs.push(fd);
        self.names.push(name);
    }

    /// 弹出一层；`false` 表示已在根、再弹即越界。
    fn pop(&mut self) -> bool {
        if self.dirs.len() <= 1 {
            return false;
        }
        self.dirs.pop();
        self.names.pop();
        true
    }

    /// 回到授权根（绝对符号链接目标以根为起点重新解析）。
    fn reset_to_root(&mut self) {
        self.dirs.truncate(1);
        self.names.clear();
    }

    /// 解析结束：末段是最后一个成功进入的目录。
    fn finish(&self, components: &[String]) -> Result<DirectChild, AccessError> {
        let Some(name) = self.names.last().cloned() else {
            return Err(AccessError::rejected(invalid_input(
                "Path refers to the workspace root, which is not a file.",
            )));
        };
        let index = self.dirs.len().saturating_sub(2);
        Ok(DirectChild {
            parent: dup_fd(self.dirs[index].as_raw_fd())?,
            name,
            rel: components.to_vec(),
            base: self.root.base.clone(),
        })
    }

    /// 解析结束：末段尚未创建（`ENOENT`）或不是目录。
    fn finish_with_new(
        &self,
        name: String,
        components: &[String],
    ) -> Result<DirectChild, AccessError> {
        Ok(DirectChild {
            parent: dup_fd(self.current_fd())?,
            name,
            rel: components.to_vec(),
            base: self.root.base.clone(),
        })
    }

    /// 仅在错误构造中使用：把符号链接目标拼成展示路径（不回显给调用方）。
    fn preview_path(&self, target: &Path) -> PathBuf {
        let mut path = self.root.base.clone();
        for name in &self.names {
            path.push(name);
        }
        path.push(target);
        path
    }
}

fn splice_link_target(
    pending: &mut VecDeque<String>,
    target: &Path,
    requested: &str,
) -> Result<(), AccessError> {
    if target.is_absolute() {
        // 绝对目标无法证明仍在根内（例如链接指向宿主上的任意绝对路径）：保守拒绝，
        // 绝不把绝对路径或裸 `..` 交给内核。
        return Err(AccessError::rejected(symlink_escape(requested, target)));
    }
    let mut spec: Vec<String> = Vec::new();
    for segment in target.to_string_lossy().split('/') {
        match segment {
            "" | "." => continue,
            other => spec.push(other.to_string()),
        }
    }
    for component in spec.into_iter().rev() {
        pending.push_front(component);
    }
    Ok(())
}

fn read_link_at(dir_fd: RawFd, name: &CStr) -> io::Result<PathBuf> {
    let mut buffer = vec![0u8; libc::PATH_MAX as usize + 1];
    let len = unsafe {
        libc::readlinkat(
            dir_fd,
            name.as_ptr(),
            buffer.as_mut_ptr() as *mut libc::c_char,
            buffer.len(),
        )
    };
    if len < 0 {
        return Err(io::Error::last_os_error());
    }
    let len = len as usize;
    if len >= buffer.len() {
        return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
    }
    buffer.truncate(len);
    Ok(PathBuf::from(std::ffi::OsString::from_vec(buffer)))
}

fn open_dir_at(dir_fd: RawFd, name: &str) -> io::Result<OwnedFd> {
    let cname = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    openat_fd(
        dir_fd,
        &cname,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
    )
}

fn openat_fd(dir_fd: RawFd, name: &CStr, flags: libc::c_int) -> io::Result<OwnedFd> {
    let fd = unsafe { libc::openat(dir_fd, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn mkdir_at(dir_fd: RawFd, name: &str) -> io::Result<()> {
    let cname = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let rc = unsafe { libc::mkdirat(dir_fd, cname.as_ptr(), 0o755) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// 列出一个目录 fd 的条目（readdir 顺序 + `lstat` 元数据）。
///
/// 每次调用都必须读自**独立的 open file description**：`dup(2)` 与来源 fd 共享目录偏移，
/// 同一个句柄上第二次 readdir 会从 EOF 开始，表现为「第二次枚举静默返回空」。因此这里用
/// 相对 `dir_fd` 的 `openat(".")` 新开描述符，而不是 dup 来源 fd；任何调用方都不需要
/// 假设「上一次枚举已把偏移复位」。
fn read_dir_entries(dir_fd: RawFd, display: &Path) -> Result<Vec<RawDirEntry>, AccessError> {
    let holder = match open_dir_at(dir_fd, ".") {
        Ok(fresh) => fresh,
        Err(_) => {
            // 目录已被删除/不可重开（例如遍历期间被 Bash 删掉）：退回旧的 dup 语义并显式
            // 复位偏移，保持「空列表」这一既有行为，而不是让枚举顺序重新变得不确定。
            let dup = dup_fd(dir_fd)?;
            if unsafe { libc::lseek(dup.as_raw_fd(), 0, libc::SEEK_SET) } < 0 {
                return Err(AccessError::io(
                    "list",
                    display.to_string_lossy(),
                    io::Error::last_os_error(),
                ));
            }
            dup
        }
    };
    let dir = unsafe { libc::fdopendir(holder.as_raw_fd()) };
    if dir.is_null() {
        return Err(AccessError::io(
            "list",
            display.to_string_lossy(),
            io::Error::last_os_error(),
        ));
    }
    // fdopendir 接管 fd 所有权：用 into_raw_fd 阻止 OwnedFd 二次关闭。
    let _ = holder.into_raw_fd();
    let mut entries: Vec<RawDirEntry> = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            break;
        }
        let name_bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name_bytes == b"." || name_bytes == b".." {
            continue;
        }
        let Ok(name) = std::str::from_utf8(name_bytes) else {
            // 非 UTF-8 名字：源实现用 to_string_lossy 展示；此处跳过并保持列举不失败。
            continue;
        };
        let Ok(cname) = CString::new(name) else {
            continue;
        };
        match lstat_at(dir_fd, &cname) {
            Ok(metadata) => entries.push(RawDirEntry {
                name: name.to_string(),
                metadata,
            }),
            Err(_) => continue,
        }
    }
    unsafe { libc::closedir(dir) };
    Ok(entries)
}

/// `stat` 的秒/纳秒字段 → `SystemTime`（负秒或越界纳秒视为不可用）。
///
/// 形参类型写 `libc::c_long` 而不是 `libc::time_t`：在 musl 目标上 libc 把 `time_t` 标注为
/// deprecated（`libc` 0.2.80 起，见 rust-lang/libc#1848），会让本产品的构建产生一条
/// `-D warnings` 下的告警；而 `stat.st_mtime` 在支持的所有 64 位平台上就是 `c_long`
/// （macOS、linux-gnu、linux-musl 的 `time_t` 都定义为 `c_long`），因此两者是同一个类型，
/// 这里只是避开那个别名。
fn system_time(secs: libc::c_long, nanos: libc::c_long) -> Option<SystemTime> {
    if secs < 0 {
        return None;
    }
    let secs = secs as u64;
    let nanos = if (0..1_000_000_000).contains(&nanos) {
        nanos as u32
    } else {
        0
    };
    Some(UNIX_EPOCH + Duration::new(secs, nanos))
}

fn lstat_at(dir_fd: RawFd, name: &CStr) -> io::Result<EntryMeta> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstatat(dir_fd, name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(EntryMeta::from_stat(&stat))
}

/// 把根内相对组件拼到授权根上（产物是**宿主绝对路径**，只用于展示与错误载荷）。
fn join_under(base: &Path, relative: &[String]) -> PathBuf {
    let mut path = base.to_path_buf();
    for component in relative {
        path.push(component);
    }
    path
}

/// `Path` → `CString`（拒绝内嵌 NUL）。
fn path_to_cstring(path: &Path) -> Result<CString, AccessError> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| AccessError::rejected(invalid_input("Path must not contain a NUL byte.")))
}

fn to_cstring(name: &str) -> Result<CString, AccessError> {
    CString::new(name)
        .map_err(|_| AccessError::rejected(invalid_input("Path must not contain a NUL byte.")))
}

fn dup_fd(fd: RawFd) -> Result<OwnedFd, AccessError> {
    let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if dup < 0 {
        return Err(AccessError::io(
            "duplicate directory handle",
            "<workspace root>",
            io::Error::last_os_error(),
        ));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(dup) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实授权根。`tempfile::tempdir()` 在 macOS 上返回 `/var/...`（`/private/var/...` 的
    /// 符号链接视图），生产路径由 `main` 规范化后交给 capability 根，这里与生产一致。
    fn rooted() -> (tempfile::TempDir, RootDir) {
        let dir = tempfile::tempdir().expect("临时目录");
        let base = dir.path().canonicalize().expect("规范化临时目录");
        let root = RootDir::open(&base).expect("打开授权根");
        (dir, root)
    }

    fn base_of(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().canonicalize().expect("规范化临时目录")
    }

    #[test]
    fn test_resolve_host_path_accepts_relative_and_in_root_absolute() {
        let (dir, root) = rooted();
        let base = base_of(&dir);
        // 相对路径相对授权根。
        assert_eq!(
            root.resolve_host_path("a/b.txt").unwrap(),
            base.join("a/b.txt")
        );
        assert_eq!(root.resolve_host_path(".").unwrap(), base);
        assert_eq!(root.resolve_host_path("a/../b").unwrap(), base.join("b"));
        // 绝对路径必须逐组件位于根内；根本身合法（由工具语义决定是否可读）。
        assert_eq!(
            root.resolve_host_path(&base.join("a/b.txt").to_string_lossy())
                .unwrap(),
            base.join("a/b.txt")
        );
        assert_eq!(
            root.resolve_host_path(&base.to_string_lossy()).unwrap(),
            base
        );
    }

    #[test]
    fn test_resolve_host_path_rejects_escapes_and_echoes_only_the_request() {
        let (dir, root) = rooted();
        let base = base_of(&dir);
        for requested in [
            "../outside.txt".to_string(),
            "a/../../outside.txt".to_string(),
            "/etc/passwd".to_string(),
        ] {
            let error = root
                .resolve_host_path(&requested)
                .expect_err("必须拒绝根外路径");
            assert!(
                matches!(error, CapabilityError::OutsideRoot { .. }),
                "{requested}: {error:?}"
            );
            // 错误文本只回显调用方原始路径，不含授权根的真实路径。
            assert!(error.public_message().contains(&requested));
            assert!(!error.public_message().contains(&*base.to_string_lossy()));
        }
    }

    #[test]
    fn test_resolve_host_path_rejects_prefix_collision_sibling() {
        let (dir, root) = rooted();
        let base = base_of(&dir);
        let sibling = format!("{}-evil/loot.txt", base.display());
        let error = root
            .resolve_host_path(&sibling)
            .expect_err("兄弟目录不是根内路径");
        assert!(matches!(error, CapabilityError::OutsideRoot { .. }));
        // 回显的是调用方原始字符串（它本身以根路径开头），但授权根不作为 `root` 外泄：
        // 判据是错误载荷的 root 字段与"仅词法拒绝、未触盘"一致。
        assert!(error.public_message().ends_with("-evil/loot.txt"));
    }

    #[test]
    fn test_resolve_host_path_rejects_nul_and_blank() {
        let (_dir, root) = rooted();
        assert!(matches!(
            root.resolve_host_path("a\0b"),
            Err(CapabilityError::InvalidInput { .. })
        ));
        assert!(matches!(
            root.resolve_host_path("   "),
            Err(CapabilityError::InvalidInput { .. })
        ));
    }
}
