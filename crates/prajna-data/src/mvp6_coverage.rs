//! Shared serialization for updates to the local MVP-6 coverage report.

use std::{fs, fs::File, io, path::Path};

use fs2::FileExt;

/// Acquire the lock shared by every command that updates `mvp6-coverage.json`.
///
/// Callers must hold the returned file until their report read/merge/write is
/// complete. The lock waits for other writers instead of dropping an update.
pub fn lock_mvp6_coverage_report(root: &Path) -> io::Result<File> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("mvp6-coverage.lock"))?;
    file.lock_exclusive()?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_report_lock_serializes_writers() {
        let root = tempfile::tempdir().unwrap();
        let first = lock_mvp6_coverage_report(root.path()).unwrap();
        let second = fs::OpenOptions::new()
            .write(true)
            .open(root.path().join("mvp6-coverage.lock"))
            .unwrap();

        assert_eq!(
            second.try_lock_exclusive().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(first);
        second.try_lock_exclusive().unwrap();
    }
}
