//! Archive sniffing and extraction into the AC install root.
//!
//! Unlike the Electron app (which unzips into RAM and copies twice), the archive is
//! extracted to a temporary directory on disk and then copied in.
//!
//! Both archive kinds go through the same copy step, because whether the archive wraps
//! everything in one folder is a property of the archive, not of its format. The wrapper
//! is unwrapped at most until an AC root folder appears: `Mod/content/cars/x/...` becomes
//! `content/cars/x/...`, while `content/cars/x/...` is left alone. Unwrapping blindly
//! (as `copy_tree` used to) turns `content/tracks/x/models.ini` into `./models.ini` and
//! scatters a mod across the game root.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    SevenZ,
}

impl ArchiveKind {
    pub fn label(self) -> &'static str {
        match self {
            ArchiveKind::Zip => "zip",
            ArchiveKind::SevenZ => "7z",
        }
    }
}

/// Top-level folders of an Assetto Corsa install. An archive that already starts with one
/// of these is packaged from the game root and must not be unwrapped further.
const AC_ROOTS: [&str; 8] = [
    "content",
    "apps",
    "cfg",
    "extension",
    "system",
    "server",
    "sdk",
    "launcher",
];

pub fn sniff(path: &Path) -> Result<ArchiveKind> {
    let mut head = [0u8; 8];
    let mut f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let n = f.read(&mut head)?;

    if n >= 2 && head[0] == 0x50 && head[1] == 0x4b {
        return Ok(ArchiveKind::Zip);
    }
    if n >= 6 && head[..6] == [0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c] {
        return Ok(ArchiveKind::SevenZ);
    }

    bail!("unsupported archive format (magic {:02x?})", &head[..n.min(8)])
}

pub fn extract(path: &Path, kind: ArchiveKind, ac_root: &Path) -> Result<usize> {
    let tmp = tempfile::Builder::new()
        .prefix("lfm-ac-extract-")
        .tempdir()
        .context("creating temp dir for extraction")?;

    match kind {
        ArchiveKind::Zip => extract_zip(path, tmp.path())?,
        ArchiveKind::SevenZ => {
            sevenz_rust2::decompress_file(path, tmp.path()).context("extracting 7z archive")?
        }
    }

    copy_tree(tmp.path(), ac_root)
}

fn extract_zip(path: &Path, dst: &Path) -> Result<()> {
    let file = File::open(path)?;
    let mut archive = zip::ZipArchive::new(file).context("reading zip archive")?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;

        // `enclosed_name` rejects absolute paths and `..` segments.
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out = dst.join(rel);

        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }

        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut dest =
            File::create(&out).with_context(|| format!("creating {}", out.display()))?;
        std::io::copy(&mut entry, &mut dest)?;
    }

    Ok(())
}

/// Copy the extracted tree into the install, unwrapping a single wrapper folder on the way.
fn copy_tree(src: &Path, dst: &Path) -> Result<usize> {
    let mut base = src.to_path_buf();

    loop {
        let entries = sorted_entries(&base)?;
        if entries.len() != 1 || !entries[0].is_dir() {
            break;
        }

        let name = entries[0]
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        if AC_ROOTS.contains(&name.as_str()) {
            break;
        }
        base = entries[0].clone();
    }

    copy_entries(&base, dst)
}

fn sorted_entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?.flatten().map(|e| e.path()).collect();
    entries.sort();
    Ok(entries)
}

fn copy_entries(src: &Path, dst: &Path) -> Result<usize> {
    std::fs::create_dir_all(dst)?;
    let mut written = 0usize;

    for entry in sorted_entries(src)? {
        let Some(name) = entry.file_name() else {
            continue;
        };
        let target = dst.join(name);

        if entry.is_dir() {
            written += copy_entries(&entry, &target)?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&entry, &target)
                .with_context(|| format!("copying to {}", target.display()))?;
            written += 1;
        }
    }

    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, rel: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    #[test]
    fn game_root_archive_keeps_content_layout() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        touch(src.path(), "content/tracks/vhe_interlagos/models.ini");
        touch(
            src.path(),
            "content/tracks/vhe_interlagos/data/surfaces.ini",
        );

        let written = copy_tree(src.path(), dst.path()).unwrap();

        assert_eq!(written, 2);
        assert!(dst
            .path()
            .join("content/tracks/vhe_interlagos/models.ini")
            .is_file());
        assert!(!dst.path().join("models.ini").exists());
    }

    #[test]
    fn single_wrapper_folder_is_unwrapped_once() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        touch(src.path(), "ACF Supra/content/cars/lfm_supra/data.acd");
        touch(src.path(), "ACF Supra/content/cars/lfm_supra/sfx/g.bank");

        copy_tree(src.path(), dst.path()).unwrap();

        assert!(dst
            .path()
            .join("content/cars/lfm_supra/data.acd")
            .is_file());
        assert!(dst.path().join("content/cars/lfm_supra/sfx/g.bank").is_file());
        assert!(!dst.path().join("data.acd").exists());
    }

    #[test]
    fn car_only_archive_is_not_dumped_in_the_root() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        touch(src.path(), "content/cars/lfm_x/ui/ui_car.json");
        touch(src.path(), "content/cars/lfm_x/data.acd");

        copy_tree(src.path(), dst.path()).unwrap();

        assert!(dst.path().join("content/cars/lfm_x/data.acd").is_file());
        assert!(!dst.path().join("data.acd").exists());
        assert!(!dst.path().join("ui").exists());
    }
}
