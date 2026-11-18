//! Everything that touches the Assetto Corsa install on disk.

pub mod extract;
pub mod index;
pub mod integrity;
pub mod purge;
pub mod scripts;
pub mod tool_file;

use std::path::{Path, PathBuf};

/// Join a path from the server-provided content structure, refusing anything that would
/// escape the AC installation.
pub fn safe_join(root: &Path, relative: &str) -> Option<PathBuf> {
    let rel = relative.replace('\\', "/");
    if rel.is_empty() {
        return None;
    }
    let mut out = root.to_path_buf();
    for part in rel.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains(':') {
            return None;
        }
        out.push(part);
    }
    if out.starts_with(root) {
        Some(out)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_relative_paths() {
        let root = Path::new("C:/AC");
        let joined = safe_join(root, "content/tracks/foo/data.acd").unwrap();
        assert!(joined.ends_with("data.acd"));
        assert!(joined.starts_with(root));
    }

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        let root = Path::new("C:/AC");
        assert!(safe_join(root, "../outside.txt").is_none());
        assert!(safe_join(root, "content/../../outside.txt").is_none());
        assert!(safe_join(root, "C:/Windows/system32/evil.dll").is_none());
        assert!(safe_join(root, "").is_none());
    }
}
