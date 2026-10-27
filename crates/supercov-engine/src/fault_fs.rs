//! `std::fs` with every call a fault site, `fs`: a module that reads
//! `use crate::fault_fs as fs;` lets a test fail its Nth file operation
//! (`fs@N`, see [`crate::fault`]) and so reach each path that handles one
//! failing. Types and the calls not listed here are `std::fs`'s own.

use std::{io, path::Path};

pub use std::fs::*;

/// `call`, unless the test fails it; at the place the wrapper was called
/// from, for `fs#N`.
#[track_caller]
fn checked<T>(call: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    crate::fault::io_here("fs", io::ErrorKind::Other, call)
}

#[track_caller]
pub fn canonicalize<P: AsRef<Path>>(path: P) -> io::Result<std::path::PathBuf> {
    checked(|| std::fs::canonicalize(path))
}

#[track_caller]
pub fn copy<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<u64> {
    checked(|| std::fs::copy(from, to))
}

#[track_caller]
pub fn create_dir<P: AsRef<Path>>(path: P) -> io::Result<()> {
    checked(|| std::fs::create_dir(path))
}

#[track_caller]
pub fn create_dir_all<P: AsRef<Path>>(path: P) -> io::Result<()> {
    checked(|| std::fs::create_dir_all(path))
}

#[track_caller]
pub fn hard_link<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    checked(|| std::fs::hard_link(original, link))
}

#[track_caller]
pub fn metadata<P: AsRef<Path>>(path: P) -> io::Result<Metadata> {
    checked(|| std::fs::metadata(path))
}

#[track_caller]
pub fn read<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
    checked(|| std::fs::read(path))
}

/// A directory's entries, the first of which a test can fail as reading
/// it does: once per listing, so that a sweep grows by a call, not by the
/// size of the directory.
pub struct ReadDir {
    entries: std::fs::ReadDir,
    first: Option<crate::fault::Place>,
}

impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.entries.next()?;
        Some(match self.first.take() {
            Some(place) => crate::fault::io_at("fs", io::ErrorKind::Other, place, || entry),
            None => entry,
        })
    }
}

#[track_caller]
pub fn read_dir<P: AsRef<Path>>(path: P) -> io::Result<ReadDir> {
    let place = std::panic::Location::caller();
    checked(|| std::fs::read_dir(path)).map(|entries| ReadDir {
        entries,
        first: Some(place),
    })
}

#[track_caller]
pub fn read_link<P: AsRef<Path>>(path: P) -> io::Result<std::path::PathBuf> {
    checked(|| std::fs::read_link(path))
}

#[track_caller]
pub fn read_to_string<P: AsRef<Path>>(path: P) -> io::Result<String> {
    checked(|| std::fs::read_to_string(path))
}

#[track_caller]
pub fn remove_dir<P: AsRef<Path>>(path: P) -> io::Result<()> {
    checked(|| std::fs::remove_dir(path))
}

#[track_caller]
pub fn remove_dir_all<P: AsRef<Path>>(path: P) -> io::Result<()> {
    checked(|| std::fs::remove_dir_all(path))
}

#[track_caller]
pub fn remove_file<P: AsRef<Path>>(path: P) -> io::Result<()> {
    checked(|| std::fs::remove_file(path))
}

#[track_caller]
pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<()> {
    checked(|| std::fs::rename(from, to))
}

#[track_caller]
pub fn set_permissions<P: AsRef<Path>>(path: P, permissions: Permissions) -> io::Result<()> {
    checked(|| std::fs::set_permissions(path, permissions))
}

#[track_caller]
pub fn symlink_metadata<P: AsRef<Path>>(path: P) -> io::Result<Metadata> {
    checked(|| std::fs::symlink_metadata(path))
}

#[track_caller]
pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(path: P, contents: C) -> io::Result<()> {
    checked(|| std::fs::write(path, contents))
}

/// `std::os::unix::fs::symlink`.
#[cfg(unix)]
#[track_caller]
pub fn symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    checked(|| std::os::unix::fs::symlink(original, link))
}

/// `std::os::windows::fs::symlink_file`.
#[cfg(windows)]
#[track_caller]
pub fn symlink_file<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    checked(|| std::os::windows::fs::symlink_file(original, link))
}

/// `std::os::windows::fs::symlink_dir`.
#[cfg(windows)]
#[track_caller]
pub fn symlink_dir<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    checked(|| std::os::windows::fs::symlink_dir(original, link))
}

/// `File::open`.
#[track_caller]
pub fn open<P: AsRef<Path>>(path: P) -> io::Result<File> {
    checked(|| File::open(path))
}

/// `File::create`.
#[track_caller]
pub fn create<P: AsRef<Path>>(path: P) -> io::Result<File> {
    checked(|| File::create(path))
}

/// `options.open(path)`.
#[track_caller]
pub fn open_with<P: AsRef<Path>>(options: &OpenOptions, path: P) -> io::Result<File> {
    checked(|| options.open(path))
}
