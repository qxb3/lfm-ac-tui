//! Removing installed content: delete exactly the files from `contentStructure`,
//! prune the directories that become empty, then drop the markers.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::ac::integrity::parse_structure;
use crate::ac::{safe_join, tool_file};
use crate::api::ContentMetadata;

#[derive(Debug, Default, Clone, Copy)]
pub struct PurgeReport {
    pub files_removed: usize,
    pub bytes_freed: u64,
    pub markers_removed: usize,
    pub skipped: usize,
}

pub fn purge(ac: &Path, meta: &ContentMetadata) -> Result<PurgeReport> {
    let structure = parse_structure(&meta.content_structure);
    let mut report = PurgeReport {
        markers_removed: tool_file::remove_markers(ac, meta.id)?,
        ..Default::default()
    };

    let mut parents: Vec<PathBuf> = Vec::new();

    for file in &structure.files {
        let Some(path) = safe_join(ac, file) else {
            report.skipped += 1;
            continue;
        };
        if !path.is_file() {
            report.skipped += 1;
            continue;
        }

        report.bytes_freed += std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        std::fs::remove_file(&path)?;
        report.files_removed += 1;

        if let Some(parent) = path.parent() {
            parents.push(parent.to_path_buf());
        }
    }

    // Deepest first so nested empty dirs collapse in one pass.
    parents.sort();
    parents.dedup();
    for parent in parents.into_iter().rev() {
        prune_empty_dirs(&parent, ac);
    }

    Ok(report)
}

/// Remove `dir` and its parents while they are inside `ac` and empty.
fn prune_empty_dirs(dir: &Path, ac: &Path) {
    let mut current = dir;
    while current.starts_with(ac) && current != ac {
        let is_empty = std::fs::read_dir(current)
            .map(|mut it| it.next().is_none())
            .unwrap_or(false);
        if !is_empty {
            break;
        }
        if std::fs::remove_dir(current).is_err() {
            break;
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
}
