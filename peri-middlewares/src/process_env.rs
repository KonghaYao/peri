//! 进程级环境变量互斥：PTC 测试会临时改写 `HOME`，与 Bash / git 等子进程测试互斥。
//!
//! 使用文件排他锁（`fs2`），可在 async 测试全程持有（`tokio::sync::Mutex` 在 `.await` 时会释放）。

use std::{fs::OpenOptions, io, path::PathBuf, sync::OnceLock};

use fs2::FileExt;

pub struct EnvLockFile(std::fs::File);

impl Drop for EnvLockFile {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

static LOCK_PATH: OnceLock<PathBuf> = OnceLock::new();

fn lock_path() -> &'static PathBuf {
    LOCK_PATH.get_or_init(|| std::env::temp_dir().join("peri-middlewares-process-env.lock"))
}

/// 在改写 `HOME` 或启动依赖真实 `HOME` 的子进程前获取；guard 在 drop 时释放。
pub fn lock() -> Result<EnvLockFile, io::Error> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path())?;
    file.lock_exclusive()?;
    Ok(EnvLockFile(file))
}
