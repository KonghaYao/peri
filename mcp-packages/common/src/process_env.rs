//! 进程级环境变量互斥：临时改写 `HOME` 的测试与 Bash / git 等子进程测试互斥。
//!
//! 使用文件排他锁（`fs4`），可在 async 测试全程持有（`tokio::sync::Mutex` 在 `.await` 时会释放）。

use std::fs::OpenOptions;
use std::io;
use std::path::PathBuf;
use std::sync::OnceLock;

use fs4::FileExt;

#[path = "process_env/env_lock_file.rs"]
mod env_lock_file;
pub use env_lock_file::lock;
pub use env_lock_file::EnvLockFile;
