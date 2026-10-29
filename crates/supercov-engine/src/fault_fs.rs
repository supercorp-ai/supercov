//! `std::fs` with every call a fault site, `fs`: a module that reads
//! `use crate::fault_fs as fs;` lets a test fail its Nth file operation
//! (`fs@N`, see [`crate::fault`]) and so reach each path that handles one
//! failing. Types and the calls not listed here are `std::fs`'s own.

use std::{io, path::Path};

pub use std::fs::*;

/// `call`, unless the test fails it; at the place the wrapper was called
/// from, for `fs#N`.
#[track_caller]
fn checked<T>(operation: &'static str, call: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    crate::fault::io_here("fs", operation, io::ErrorKind::Other, call)
}

#[track_caller]
pub fn canonicalize<P: AsRef<Path>>(path: P) -> io::Result<std::path::PathBuf> {
    checked("canonicalize", || std::fs::canonicalize(path))
}

#[track_caller]
pub fn copy<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<u64> {
    checked("copy", || std::fs::copy(from, to))
}

#[track_caller]
pub fn create_dir<P: AsRef<Path>>(path: P) -> io::Result<()> {
    checked("create_dir", || std::fs::create_dir(path))
}

/// A directory only its owner can enter, made so in one call: there is no
/// moment when another user could open it.
#[cfg(unix)]
#[track_caller]
pub fn create_private_dir<P: AsRef<Path>>(path: P) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    checked("create_dir", || {
        std::fs::DirBuilder::new().mode(0o700).create(path)
    })
}

#[track_caller]
pub fn create_dir_all<P: AsRef<Path>>(path: P) -> io::Result<()> {
    checked("create_dir_all", || std::fs::create_dir_all(path))
}

#[track_caller]
pub fn hard_link<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    checked("hard_link", || std::fs::hard_link(original, link))
}

#[track_caller]
pub fn metadata<P: AsRef<Path>>(path: P) -> io::Result<Metadata> {
    checked("metadata", || std::fs::metadata(path))
}

#[track_caller]
pub fn read<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
    checked("read", || std::fs::read(path))
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

/// An entry's type, as [`DirEntry::file_type`] reads it, unless the test
/// fails it.
#[track_caller]
pub fn file_type(entry: &DirEntry) -> io::Result<FileType> {
    checked("file_type", || entry.file_type())
}

#[track_caller]
pub fn read_dir<P: AsRef<Path>>(path: P) -> io::Result<ReadDir> {
    // The listing's first entry is a place of its own, apart from the call
    // that opened it.
    let place = (std::panic::Location::caller(), "read_dir entry");
    checked("read_dir", || std::fs::read_dir(path)).map(|entries| ReadDir {
        entries,
        first: Some(place),
    })
}

#[track_caller]
pub fn read_link<P: AsRef<Path>>(path: P) -> io::Result<std::path::PathBuf> {
    checked("read_link", || std::fs::read_link(path))
}

#[track_caller]
pub fn read_to_string<P: AsRef<Path>>(path: P) -> io::Result<String> {
    checked("read_to_string", || std::fs::read_to_string(path))
}

#[track_caller]
pub fn remove_dir<P: AsRef<Path>>(path: P) -> io::Result<()> {
    checked("remove_dir", || std::fs::remove_dir(path))
}

#[track_caller]
pub fn remove_dir_all<P: AsRef<Path>>(path: P) -> io::Result<()> {
    checked("remove_dir_all", || std::fs::remove_dir_all(path))
}

#[track_caller]
pub fn remove_file<P: AsRef<Path>>(path: P) -> io::Result<()> {
    checked("remove_file", || std::fs::remove_file(path))
}

#[track_caller]
pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<()> {
    checked("rename", || std::fs::rename(from, to))
}

#[track_caller]
pub fn set_permissions<P: AsRef<Path>>(path: P, permissions: Permissions) -> io::Result<()> {
    checked("set_permissions", || {
        std::fs::set_permissions(path, permissions)
    })
}

#[track_caller]
pub fn symlink_metadata<P: AsRef<Path>>(path: P) -> io::Result<Metadata> {
    checked("symlink_metadata", || std::fs::symlink_metadata(path))
}

#[track_caller]
pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(path: P, contents: C) -> io::Result<()> {
    checked("write", || std::fs::write(path, contents))
}

/// `std::os::unix::fs::symlink`.
#[cfg(unix)]
#[track_caller]
pub fn symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    checked("symlink", || std::os::unix::fs::symlink(original, link))
}

/// `std::os::windows::fs::symlink_file`.
#[cfg(windows)]
#[track_caller]
pub fn symlink_file<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    checked("symlink_file", || {
        std::os::windows::fs::symlink_file(original, link)
    })
}

/// `std::os::windows::fs::symlink_dir`.
#[cfg(windows)]
#[track_caller]
pub fn symlink_dir<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    checked("symlink_dir", || {
        std::os::windows::fs::symlink_dir(original, link)
    })
}

/// `File::open`.
#[track_caller]
pub fn open<P: AsRef<Path>>(path: P) -> io::Result<File> {
    checked("open", || File::open(path))
}

/// `options.open(path)`.
#[track_caller]
pub fn open_with<P: AsRef<Path>>(options: &OpenOptions, path: P) -> io::Result<File> {
    checked("open_with", || options.open(path))
}

/// `file.sync_all()`.
#[track_caller]
pub fn sync_all(file: &File) -> io::Result<()> {
    checked("sync_all", || file.sync_all())
}

/// `file.metadata()`, of a file already open.
#[track_caller]
pub fn file_metadata(file: &File) -> io::Result<Metadata> {
    checked("file_metadata", || file.metadata())
}

/// `file.write_all(bytes)`, to a file already open.
#[track_caller]
pub fn write_all(file: &mut File, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    checked("write_all", || file.write_all(bytes))
}

/// `file.seek(position)`.
#[track_caller]
pub fn seek(file: &mut File, position: io::SeekFrom) -> io::Result<u64> {
    use std::io::Seek;
    checked("seek", || file.seek(position))
}

/// `file.read(buffer)`, from a file already open (or a reader over one).
#[track_caller]
pub fn read_from(file: &mut impl io::Read, buffer: &mut [u8]) -> io::Result<usize> {
    checked("read_from", || file.read(buffer))
}

/// `file.read_exact(buffer)`.
#[track_caller]
pub fn read_exact_from(file: &mut impl io::Read, buffer: &mut [u8]) -> io::Result<()> {
    checked("read_exact_from", || file.read_exact(buffer))
}

/// `file.read_to_end(bytes)`.
#[track_caller]
pub fn read_to_end_from(file: &mut impl io::Read, bytes: &mut Vec<u8>) -> io::Result<usize> {
    checked("read_to_end_from", || file.read_to_end(bytes))
}

/// `io::copy(from, to)`, between files already open.
#[track_caller]
pub fn copy_between(from: &mut File, to: &mut File) -> io::Result<u64> {
    checked("copy_between", || io::copy(from, to))
}
