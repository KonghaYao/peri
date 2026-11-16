use super::{io, FileExt, OnceLock, OpenOptions, PathBuf};

#[derive(Debug)]
pub struct EnvLockFile(pub(super) std::fs::File);

impl Drop for EnvLockFile {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub(super) static LOCK_PATH: OnceLock<PathBuf> = OnceLock::new();

pub(super) fn lock_path() -> &'static PathBuf {
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
    FileExt::lock(&file)?;
    Ok(EnvLockFile(file))
}
