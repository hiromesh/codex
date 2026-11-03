use crate::FileMetadata;
use std::io;
use std::path::Path;

#[cfg(unix)]
mod unix;

#[cfg(unix)]
use unix as imp;

pub(crate) fn open_file(path: &Path) -> io::Result<std::fs::File> {
    imp::open_file_sync(path)
}

pub(crate) async fn write_file(path: &Path, contents: Vec<u8>) -> io::Result<()> {
    imp::write_file(path.to_path_buf(), contents).await
}

#[cfg(unix)]
pub(crate) async fn metadata(path: &Path) -> io::Result<FileMetadata> {
    imp::metadata(path.to_path_buf()).await
}

pub(crate) async fn create_directory(path: &Path, recursive: bool) -> io::Result<()> {
    imp::create_directory(path.to_path_buf(), recursive).await
}

pub(crate) async fn remove(path: &Path, recursive: bool, force: bool) -> io::Result<()> {
    imp::remove(path.to_path_buf(), recursive, force).await
}
