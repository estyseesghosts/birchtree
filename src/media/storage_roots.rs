//! Startup checks for the on-disk storage roots.
//!
//! The durable pool (local uploads, never evicted) and the cache pool
//! (remote bytes, evictable) must be physically separate trees: if one
//! contained the other, or if a symlink aliased them, cache eviction and the
//! cache reconciler could delete durable bytes. Comparing configured strings
//! is not enough, so every check here works on resolved paths.

use std::path::{Component, Path, PathBuf};

use crate::domain::{Error, Result};

/// Resolve `path` through symlinks even when its tail does not exist yet:
/// the longest existing ancestor is canonicalized and the remaining
/// components are appended unchanged.
pub fn resolve_lenient(path: &Path) -> Result<PathBuf> {
    let absolute: PathBuf = std::path::absolute(path).map_err(|err| {
        Error::Storage(format!("make {} absolute: {err}", path.display()))
    })?;
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut probe: PathBuf = absolute.clone();
    loop {
        match std::fs::canonicalize(&probe) {
            Ok(mut resolved) => {
                for part in tail.iter().rev() {
                    resolved.push(part);
                }
                return Ok(normalize_components(&resolved));
            }
            Err(_) => {
                let Some(name) = probe.file_name().map(std::ffi::OsStr::to_os_string) else {
                    return Ok(normalize_components(&absolute));
                };
                tail.push(name);
                if !probe.pop() {
                    return Ok(normalize_components(&absolute));
                }
            }
        }
    }
}

/// Drop `.` components and fold `..` lexically (the caller has already
/// resolved links, so the folding cannot cross one).
fn normalize_components(path: &Path) -> PathBuf {
    let mut out: PathBuf = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Case-fold for comparison on case-insensitive platforms.
fn comparable(path: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    } else {
        path.to_path_buf()
    }
}

/// Refuse two roots that are equal or nested, after resolving symlinks.
pub fn ensure_disjoint(first: &Path, second: &Path) -> Result<()> {
    let first_resolved: PathBuf = comparable(&resolve_lenient(first)?);
    let second_resolved: PathBuf = comparable(&resolve_lenient(second)?);
    if first_resolved == second_resolved {
        return Err(Error::InvalidInput(format!(
            "durable and cache blob roots resolve to the same directory ({})",
            first_resolved.display()
        )));
    }
    if first_resolved.starts_with(&second_resolved) || second_resolved.starts_with(&first_resolved)
    {
        return Err(Error::InvalidInput(format!(
            "blob roots overlap: {} and {}",
            first_resolved.display(),
            second_resolved.display()
        )));
    }
    Ok(())
}

/// Refuse a blob root that equals or contains the database file, and a root
/// that contains the whole data directory. (A root *inside* the data
/// directory is the default layout and is fine.)
pub fn ensure_root_excludes_database(root: &Path, database: &Path) -> Result<()> {
    let root_resolved: PathBuf = comparable(&resolve_lenient(root)?);
    let database_resolved: PathBuf = comparable(&resolve_lenient(database)?);
    if database_resolved.starts_with(&root_resolved) {
        return Err(Error::InvalidInput(format!(
            "blob root {} contains the database file {}",
            root_resolved.display(),
            database_resolved.display()
        )));
    }
    Ok(())
}

/// Run every startup path check for a durable/cache/database layout.
pub fn validate_layout(durable: &Path, cache: &Path, database: &Path) -> Result<()> {
    ensure_disjoint(durable, cache)?;
    ensure_root_excludes_database(durable, database)?;
    ensure_root_excludes_database(cache, database)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_and_equal_roots_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::create_dir_all(&a).unwrap();
        assert!(ensure_disjoint(&a, &a).is_err());
        assert!(ensure_disjoint(&a, &a.join("inner")).is_err());
        assert!(ensure_disjoint(&a.join("x").join(".."), &a).is_err());
        assert!(ensure_disjoint(&a, &dir.path().join("b")).is_ok());
    }

    #[test]
    fn database_must_not_live_inside_a_blob_root() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("birchtree.redb");
        assert!(ensure_root_excludes_database(dir.path(), &db).is_err());
        assert!(ensure_root_excludes_database(&dir.path().join("media"), &db).is_ok());
    }
}
