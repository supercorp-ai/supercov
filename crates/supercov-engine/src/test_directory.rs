//! A directory a test made, removed when the test is done with it.
//!
//! Tests removed their directory as their last step, so one that failed,
//! returned early, or never had that step left it in the system's temporary
//! directory: a release check left hundreds. Dropped as the test ends, passed
//! or panicked, this removes it. `KEEP=1` leaves it to look at.

use std::ops::Deref;
use std::path::{Path, PathBuf};

pub(crate) struct TestDirectory(PathBuf);

impl TestDirectory {
    /// Takes over a directory the test has just created.
    pub(crate) fn adopt(path: PathBuf) -> Self {
        Self(path)
    }
}

impl Deref for TestDirectory {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TestDirectory {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if std::env::var_os("KEEP").is_none() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
