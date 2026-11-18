//! A case-insensitive snapshot of the AC install.
//!
//! The Electron app trusts `ac-content.tool` markers, which it fails to write for content
//! installed outside it (and which are simply absent on a Linux/Proton setup that was
//! populated by Content Manager). Instead, walk the install once and answer path queries
//! from memory.
//!
//! Matching is case-insensitive because mod archives are built on Windows and the
//! `contentStructure` casing does not always match what extraction produced on a
//! case-sensitive filesystem.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

#[derive(Debug, Default)]
pub struct PathIndex {
    files: HashSet<String>,
    dirs: HashSet<String>,
}

impl PathIndex {
    pub fn build(root: &Path) -> Result<Self> {
        let mut index = Self::default();
        let mut stack = vec![root.to_path_buf()];
        let mut visited: HashSet<PathBuf> = HashSet::new();

        while let Some(dir) = stack.pop() {
            let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
            if !visited.insert(canonical) {
                continue;
            }

            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(_) => continue,
            };

            for entry in entries.flatten() {
                let path = entry.path();
                let relative = match path.strip_prefix(root) {
                    Ok(relative) => normalize(&relative.to_string_lossy()),
                    Err(_) => continue,
                };
                if relative.is_empty() {
                    continue;
                }

                let file_type = match entry.file_type() {
                    Ok(file_type) => file_type,
                    Err(_) => continue,
                };

                if file_type.is_dir() {
                    index.dirs.insert(relative);
                    stack.push(path);
                } else if file_type.is_file() {
                    index.files.insert(relative);
                } else if file_type.is_symlink() {
                    // Proton installs occasionally symlink content directories.
                    match std::fs::metadata(&path) {
                        Ok(metadata) if metadata.is_dir() => {
                            index.dirs.insert(relative);
                            stack.push(path);
                        }
                        Ok(metadata) if metadata.is_file() => {
                            index.files.insert(relative);
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(index)
    }

    pub fn has_file(&self, relative: &str) -> bool {
        self.files.contains(&normalize(relative))
    }

    pub fn has_dir(&self, relative: &str) -> bool {
        self.dirs.contains(&normalize(relative))
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }
}

/// Lowercase, forward slashes, no leading `./` — the form used in both the index and the
/// server-provided content structure.
pub fn normalize(path: &str) -> String {
    let mut value = path.replace('\\', "/");
    while let Some(stripped) = value.strip_prefix("./") {
        value = stripped.to_string();
    }
    let trimmed = value.trim_end_matches('/');
    trimmed.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_windows_and_relative_paths() {
        assert_eq!(
            normalize("Content\\Tracks\\Ks_Silverstone\\3_LFM.KN5"),
            "content/tracks/ks_silverstone/3_lfm.kn5"
        );
        assert_eq!(normalize("./content/cars/x/"), "content/cars/x");
    }

    #[test]
    fn indexes_only_what_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("content/tracks/Test")).unwrap();
        std::fs::write(dir.path().join("content/tracks/Test/a.KN5"), b"x").unwrap();

        let index = PathIndex::build(dir.path()).unwrap();
        assert!(index.has_file("content/tracks/test/a.kn5"));
        assert!(index.has_dir("CONTENT/TRACKS/TEST"));
        assert!(!index.has_file("content/tracks/test/b.kn5"));
    }
}
