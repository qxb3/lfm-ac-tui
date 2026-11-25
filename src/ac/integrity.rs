//! Install/update detection.
//!
//! The Electron app requires an `ac-content.tool` marker and treats content without one as
//! "needs reinstall". That fails for anything installed outside the app (Content Manager,
//! manual extraction, a Linux/Proton install), which is the common case here. So the
//! primary signal is the filesystem itself: how many of the files listed in the server's
//! `contentStructure` are present, matched case-insensitively.
//!
//! Markers are still read, but only to detect an explicit version change.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::ac::index::PathIndex;
use crate::ac::safe_join;
use crate::ac::tool_file::{self, ContentToolFile};
use crate::api::ContentMetadata;

/// Above this share of expected files, content counts as "installed but incomplete"
/// (a stale version) rather than missing.
pub const PARTIAL_THRESHOLD: f32 = 0.9;

#[derive(Debug, Default, Clone, Deserialize)]
pub struct Structure {
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub directories: Vec<String>,
}

/// The server sends `contentStructure` as a JSON string.
pub fn parse_structure(raw: &str) -> Structure {
    if raw.trim().is_empty() {
        return Structure::default();
    }
    serde_json::from_str::<Structure>(raw).unwrap_or_default()
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Integrity {
    pub is_installed: bool,
    pub is_up_to_date: bool,
    /// A marker exists and carries a different hash than the server's.
    pub needs_update: bool,
    pub present: usize,
    pub total: usize,
    pub roots_present: bool,
}

impl Integrity {
    pub fn healthy(&self) -> bool {
        self.is_installed && self.is_up_to_date
    }

    pub fn coverage(&self) -> f32 {
        if self.total == 0 {
            if self.roots_present {
                1.0
            } else {
                0.0
            }
        } else {
            self.present as f32 / self.total as f32
        }
    }

    /// Present, root folder exists, but some files are missing.
    pub fn is_partial(&self) -> bool {
        !self.is_installed
            && self.roots_present
            && self.present > 0
            && self.coverage() >= PARTIAL_THRESHOLD
    }
}

pub fn check_with_index(index: &PathIndex, ac: &Path, meta: &ContentMetadata) -> Integrity {
    let markers = tool_file::marker_index(ac);
    check_with_markers(index, ac, meta, &markers)
}

/// Like `check_with_index`, but reuses a marker map built once for a whole batch of checks.
pub fn check_with_markers(
    index: &PathIndex,
    ac: &Path,
    meta: &ContentMetadata,
    markers: &HashMap<u32, Vec<ContentToolFile>>,
) -> Integrity {
    let structure = parse_structure(&meta.content_structure);

    let mut total = 0usize;
    let mut present = 0usize;
    let declared_files = structure.files.len();

    for file in &structure.files {
        // Paths that would escape the install are never counted as satisfied.
        if safe_join(ac, file).is_none() {
            continue;
        }
        total += 1;
        if index.has_file(file) {
            present += 1;
        }
    }

    let roots = tool_file::root_folders(&structure.files, &structure.directories);
    let roots_present = roots.is_empty() || roots.iter().all(|root| index.has_dir(root));

    // If the structure declares files but every one of them was rejected as unsafe,
    // this is a malformed entry, never an installed one.
    let full = if total == 0 {
        roots_present && declared_files == 0
    } else {
        present == total
    };

    let own = markers.get(&meta.id).map(Vec::as_slice).unwrap_or(&[]);
    let hash_mismatch = own.iter().any(|m| m.hash != meta.hash);

    Integrity {
        is_installed: full,
        is_up_to_date: full && !hash_mismatch,
        needs_update: hash_mismatch,
        present,
        total,
        roots_present,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ContentMetadata;

    fn metadata(id: u32, files: &[&str], dirs: &[&str]) -> ContentMetadata {
        let structure = serde_json::json!({ "files": files, "directories": dirs }).to_string();
        ContentMetadata {
            id,
            content_structure: structure,
            hash: "abc".to_string(),
            ..Default::default()
        }
    }

    fn index_with(files: &[&str], dirs: &[&str]) -> PathIndex {
        // Build a real tree so the index behaves like production.
        let dir = tempfile::tempdir().unwrap();
        for d in dirs {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        for f in files {
            let path = dir.path().join(f);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"x").unwrap();
        }
        PathIndex::build(dir.path()).unwrap()
    }

    #[test]
    fn detects_case_insensitive_full_install() {
        let index = index_with(
            &["content/cars/TestCar/data.ACD"],
            &["content/cars/TestCar"],
        );
        let meta = metadata(
            1,
            &["content/cars/testcar/data.acd"],
            &["content/cars/testcar"],
        );
        let integrity = check_with_index(&index, Path::new("C:/AC"), &meta);
        assert!(integrity.is_installed, "casing must not matter");
        assert!(integrity.healthy());
    }

    #[test]
    fn reports_partial_when_most_files_are_present() {
        let index = index_with(
            &[
                "content/tracks/t/a.kn5",
                "content/tracks/t/b.kn5",
                "content/tracks/t/c.kn5",
                "content/tracks/t/d.kn5",
                "content/tracks/t/e.kn5",
                "content/tracks/t/f.kn5",
                "content/tracks/t/g.kn5",
                "content/tracks/t/h.kn5",
                "content/tracks/t/i.kn5",
                "content/tracks/t/j.kn5",
            ],
            &["content/tracks/t"],
        );
        let meta = metadata(
            2,
            &[
                "content/tracks/t/a.kn5",
                "content/tracks/t/b.kn5",
                "content/tracks/t/c.kn5",
                "content/tracks/t/d.kn5",
                "content/tracks/t/e.kn5",
                "content/tracks/t/f.kn5",
                "content/tracks/t/g.kn5",
                "content/tracks/t/h.kn5",
                "content/tracks/t/i.kn5",
                "content/tracks/t/j.kn5",
                "content/tracks/t/missing.kn5",
            ],
            &["content/tracks/t"],
        );
        let integrity = check_with_index(&index, Path::new("C:/AC"), &meta);
        assert!(!integrity.is_installed);
        assert!(integrity.is_partial());
        assert_eq!(integrity.present, 10);
        assert_eq!(integrity.total, 11);
    }

    #[test]
    fn missing_root_is_not_installed() {
        let index = index_with(&[], &[]);
        let meta = metadata(3, &["content/cars/nope/data.acd"], &["content/cars/nope"]);
        let integrity = check_with_index(&index, Path::new("C:/AC"), &meta);
        assert!(!integrity.is_installed);
        assert!(!integrity.is_partial());
        assert_eq!(integrity.present, 0);
    }

    #[test]
    fn rejects_traversal_entries() {
        let index = index_with(&[], &[]);
        let meta = metadata(4, &["../../etc/passwd"], &[]);
        let integrity = check_with_index(&index, Path::new("C:/AC"), &meta);
        assert_eq!(integrity.total, 0);
        assert!(!integrity.is_installed);
    }
}
