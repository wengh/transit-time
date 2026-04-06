use anyhow::Result;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// Acquire an exclusive flock on `{cache_path}.lock`, run `f`, then release.
/// Blocks if another process is downloading the same file.
/// `f` must re-check whether `path` already exists before downloading,
/// since a concurrent process may have populated it while we waited.
pub fn with_download_lock<F>(cache_path: &Path, f: F) -> Result<PathBuf>
where
    F: FnOnce(&Path) -> Result<PathBuf>,
{
    let lock_path = cache_path.with_extension("lock");
    let lock_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    let ret = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
    anyhow::ensure!(
        ret == 0,
        "flock failed: {}",
        std::io::Error::last_os_error()
    );
    let result = f(cache_path);
    drop(lock_file);
    result
}
