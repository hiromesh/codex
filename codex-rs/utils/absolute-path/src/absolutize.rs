// Adapted from path-absolutize 3.1.1:
// Copyright (c) 2018 magiclen.org (Ron Li)
// Licensed under the MIT License.
//
// Keep this implementation local so explicit-base normalization can be
// infallible for `AbsolutePathBuf::resolve_path_against_base` and
// `AbsolutePathBuf::join`; only current-working-directory lookup remains
// fallible.

use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

pub(super) fn absolutize(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(normalize_path(path));
    }

    Ok(absolutize_from(path, &std::env::current_dir()?))
}

pub(super) fn absolutize_from(path: &Path, base_path: &Path) -> PathBuf {
    normalize_path(&path_with_base(path, base_path))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}

fn path_with_base(path: &Path, base_path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_path.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[cfg(unix)]
    #[test]
    fn absolute_path_without_dots_is_unchanged() {
        assert_eq!(
            absolutize_from(Path::new("/path/to/123/456"), Path::new("/base")),
            PathBuf::from("/path/to/123/456")
        );
    }

    #[cfg(unix)]
    #[test]
    fn absolute_path_dots_are_removed() {
        assert_eq!(
            absolutize_from(Path::new("/path/to/./123/../456"), Path::new("/base")),
            PathBuf::from("/path/to/456")
        );
    }

    #[cfg(unix)]
    #[test]
    fn relative_path_without_dot_uses_base() {
        assert_eq!(
            absolutize_from(Path::new("path/to/123/456"), Path::new("/base")),
            PathBuf::from("/base/path/to/123/456")
        );
    }

    #[cfg(unix)]
    #[test]
    fn relative_path_with_current_dir_uses_base() {
        assert_eq!(
            absolutize_from(Path::new("./path/to/123/456"), Path::new("/base")),
            PathBuf::from("/base/path/to/123/456")
        );
    }

    #[cfg(unix)]
    #[test]
    fn relative_path_with_parent_dir_uses_base_parent() {
        assert_eq!(
            absolutize_from(Path::new("../path/to/123/456"), Path::new("/base/cwd")),
            PathBuf::from("/base/path/to/123/456")
        );
    }

    #[cfg(unix)]
    #[test]
    fn parent_dir_above_root_stays_at_root() {
        assert_eq!(
            absolutize_from(Path::new("../../path/to/123/456"), Path::new("/")),
            PathBuf::from("/path/to/123/456")
        );
    }

    #[cfg(unix)]
    #[test]
    fn empty_path_uses_base() {
        assert_eq!(
            absolutize_from(Path::new(""), Path::new("/base/cwd")),
            PathBuf::from("/base/cwd")
        );
    }

}
