//! `std::fs` with every call a fault site, `fs`: a module that reads
//! `use crate::fault_fs as fs;` lets a test fail its Nth file operation
//! (`fs@N`, see [`crate::fault`]) and so reach each path that handles one
//! failing. Types and the calls not listed here are `std::fs`'s own.

use std::{io, path::Path};

pub use std::fs::*;

fn checked<T>(call: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    crate::fault::io("fs", io::ErrorKind::Other, call)
}

pub fn canonicalize<P: AsRef<Path>>(path: P) -> io::Result<std::path::PathBuf> {
    checked(|| std::fs::canonicalize(path))
}

pub fn copy<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<u64> {
    checked(|| std::fs::copy(from, to))
}

pub fn create_dir<P: AsRef<Path>>(path: P) -> io::Result<()> {
    checked(|| std::fs::create_dir(path))
}

pub fn create_dir_all<P: AsRef<Path>>(path: P) -> io::Result<()> {
    checked(|| std::fs::create_dir_all(path))
}

pub fn hard_link<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    checked(|| std::fs::hard_link(original, link))
}

pub fn metadata<P: AsRef<Path>>(path: P) -> io::Result<Metadata> {
    checked(|| std::fs::metadata(path))
}

pub fn read<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
    checked(|| std::fs::read(path))
}

pub fn read_dir<P: AsRef<Path>>(path: P) -> io::Result<ReadDir> {
    checked(|| std::fs::read_dir(path))
}

pub fn read_link<P: AsRef<Path>>(path: P) -> io::Result<std::path::PathBuf> {
    checked(|| std::fs::read_link(path))
}

pub fn read_to_string<P: AsRef<Path>>(path: P) -> io::Result<String> {
    checked(|| std::fs::read_to_string(path))
}

pub fn remove_dir<P: AsRef<Path>>(path: P) -> io::Result<()> {
    checked(|| std::fs::remove_dir(path))
}

pub fn remove_dir_all<P: AsRef<Path>>(path: P) -> io::Result<()> {
    checked(|| std::fs::remove_dir_all(path))
}

pub fn remove_file<P: AsRef<Path>>(path: P) -> io::Result<()> {
    checked(|| std::fs::remove_file(path))
}

pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<()> {
    checked(|| std::fs::rename(from, to))
}

pub fn set_permissions<P: AsRef<Path>>(path: P, permissions: Permissions) -> io::Result<()> {
    checked(|| std::fs::set_permissions(path, permissions))
}

pub fn symlink_metadata<P: AsRef<Path>>(path: P) -> io::Result<Metadata> {
    checked(|| std::fs::symlink_metadata(path))
}

pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(path: P, contents: C) -> io::Result<()> {
    checked(|| std::fs::write(path, contents))
}

/// `std::os::unix::fs::symlink`.
#[cfg(unix)]
pub fn symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    checked(|| std::os::unix::fs::symlink(original, link))
}

/// `std::os::windows::fs::symlink_file`.
#[cfg(windows)]
pub fn symlink_file<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    checked(|| std::os::windows::fs::symlink_file(original, link))
}

/// `std::os::windows::fs::symlink_dir`.
#[cfg(windows)]
pub fn symlink_dir<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    checked(|| std::os::windows::fs::symlink_dir(original, link))
}
