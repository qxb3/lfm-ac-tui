//! `ac-content.tool` markers: the only record of which content version is installed.
//!
//! One copy is written into every root folder the content touches, e.g.
//! `<AC>/content/tracks/<mod>/ac-content.tool`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::ac::safe_join;
use crate::api::ContentMetadata;

pub const MARKER_NAME: &str = "ac-content.tool";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentToolFile {
    pub version: u32,
    pub content_id: u32,
    pub hash: String,
    pub downloaded_at: String,
    pub content_type: String,
    pub name: String,
    pub lfm_id: i64,
    pub file_count: usize,
    pub files: Vec<String>,
    pub directories: Vec<String>,
}

impl ContentToolFile {
    pub fn new(
        meta: &ContentMetadata,
        files: Vec<String>,
        directories: Vec<String>,
    ) -> Self {
        Self {
            version: 1,
            content_id: meta.id,
            hash: meta.hash.clone(),
            downloaded_at: iso8601_now(),
            content_type: meta.content_type.clone(),
            name: meta.name.clone(),
            lfm_id: meta.lfm_id.unwrap_or(0),
            file_count: files.len(),
            files,
            directories,
        }
    }
}

/// The four locations scanned for markers, mirroring the app.
pub fn search_dirs(ac: &Path) -> Vec<PathBuf> {
    [
        ac.join("content").join("cars"),
        ac.join("content").join("tracks"),
        ac.join("cars"),
        ac.join("tracks"),
    ]
    .into_iter()
    .filter(|d| d.is_dir())
    .collect()
}

pub fn read_marker(path: &Path) -> Option<ContentToolFile> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed = serde_json::from_str::<ContentToolFile>(&raw).ok()?;
    if parsed.version == 0 || parsed.content_id == 0 || parsed.hash.is_empty() {
        return None;
    }
    Some(parsed)
}

/// All markers belonging to `content_id`.
pub fn find_markers(ac: &Path, content_id: u32) -> Vec<(PathBuf, ContentToolFile)> {
    let mut found = Vec::new();

    for dir in search_dirs(ac) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let marker = entry.path().join(MARKER_NAME);
            if let Some(data) = read_marker(&marker) {
                if data.content_id == content_id {
                    found.push((marker, data));
                }
            }
        }
    }

    found
}

/// Every marker under the install, grouped by content id. One directory scan replaces a
/// separate scan per content item.
pub fn marker_index(ac: &Path) -> std::collections::HashMap<u32, Vec<ContentToolFile>> {
    let mut map: std::collections::HashMap<u32, Vec<ContentToolFile>> = Default::default();
    for dir in search_dirs(ac) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            if let Some(data) = read_marker(&entry.path().join(MARKER_NAME)) {
                map.entry(data.content_id).or_default().push(data);
            }
        }
    }
    map
}

pub fn remove_markers(ac: &Path, content_id: u32) -> Result<usize> {
    let markers = find_markers(ac, content_id);
    let mut removed = 0;
    for (path, _) in markers {
        std::fs::remove_file(&path)
            .with_context(|| format!("removing {}", path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

/// Derive `<root>` folders from a content structure, exactly like `getContentRootFolders`.
pub fn root_folders(files: &[String], directories: &[String]) -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();

    for dir in directories {
        let normalized = dir.replace('\\', "/");
        let parts: Vec<&str> = normalized.split('/').collect();
        if parts.len() >= 3 && parts[0] == "content" && matches!(parts[1], "cars" | "tracks") {
            push_unique(&mut roots, format!("{}/{}/{}", parts[0], parts[1], parts[2]));
        } else if parts.len() >= 2 && matches!(parts[0], "cars" | "tracks") {
            push_unique(&mut roots, format!("{}/{}", parts[0], parts[1]));
        }
    }

    if roots.is_empty() {
        for file in files {
            let normalized = file.replace('\\', "/");
            let parts: Vec<&str> = normalized.split('/').collect();
            if parts.len() >= 4 && parts[0] == "content" && matches!(parts[1], "cars" | "tracks") {
                push_unique(&mut roots, format!("{}/{}/{}", parts[0], parts[1], parts[2]));
            } else if parts.len() >= 3 && matches!(parts[0], "cars" | "tracks") {
                push_unique(&mut roots, format!("{}/{}", parts[0], parts[1]));
            }
        }
    }

    roots
}

fn push_unique(roots: &mut Vec<String>, value: String) {
    if !roots.contains(&value) {
        roots.push(value);
    }
}

/// Write a marker into every root folder. Returns the paths written.
pub fn write_markers(
    ac: &Path,
    meta: &ContentMetadata,
    files: &[String],
    directories: &[String],
) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let payload = ContentToolFile::new(meta, files.to_vec(), directories.to_vec());
    let json = serde_json::to_string_pretty(&payload)?;

    for root in root_folders(files, directories) {
        let Some(dir) = safe_join(ac, &root) else {
            continue;
        };
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        let marker = dir.join(MARKER_NAME);
        std::fs::write(&marker, &json)
            .with_context(|| format!("writing {}", marker.display()))?;
        written.push(marker);
    }

    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_roots_from_directories() {
        let dirs = vec!["content/tracks/spa".to_string(), "content/cars/gt3".to_string()];
        let roots = root_folders(&[], &dirs);
        assert_eq!(roots, ["content/tracks/spa", "content/cars/gt3"]);
    }

    #[test]
    fn falls_back_to_files_and_short_paths() {
        let files = vec![
            "content/tracks/spa/layout.kn5".to_string(),
            "cars/gt3/data.acd".to_string(),
        ];
        let roots = root_folders(&files, &[]);
        assert_eq!(roots, ["content/tracks/spa", "cars/gt3"]);
    }

    #[test]
    fn ignores_paths_outside_content() {
        let files = vec!["apps/python/RealPenalty/RealPenalty.py".to_string()];
        assert!(root_folders(&files, &[]).is_empty());
    }
}

/// RFC3339-ish UTC timestamp without pulling in `chrono`.
fn iso8601_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { y + 1 } else { y }, month, day)
}
