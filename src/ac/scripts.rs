//! The two extra AC components the app forces on every sync:
//!   * RealPenalty (content id 112) — python app
//!   * LFM livery script (content id 114) — lua app
//!
//! plus the `gameplay.ini` / `python.ini` toggles.

use std::path::Path;

use anyhow::{Context, Result};

use crate::config::ac_documents_dir;

pub const REAL_PENALTY_CONTENT_ID: u32 = 112;
pub const LIVERY_SCRIPT_CONTENT_ID: u32 = 114;

pub const REAL_PENALTY_MARKER: &str = "apps/python/RealPenalty/RealPenalty.py";
pub const LIVERY_SCRIPT_MARKER: &str = "apps/lua/offlineLfmLivery.lua";

/// The config files live in `Documents/Assetto Corsa`, not in the game folder.
fn gameplay_ini() -> Option<std::path::PathBuf> {
    ac_documents_dir().map(|d| d.join("cfg").join("gameplay.ini"))
}

fn python_ini() -> Option<std::path::PathBuf> {
    ac_documents_dir().map(|d| d.join("cfg").join("python.ini"))
}

pub fn real_penalty_installed(ac: &Path) -> bool {
    ac.join(REAL_PENALTY_MARKER).is_file()
}

pub fn livery_script_installed(ac: &Path) -> bool {
    ac.join(LIVERY_SCRIPT_MARKER).is_file()
}

pub fn set_python_enabled(enabled: bool) -> Result<bool> {
    let Some(path) = gameplay_ini() else {
        return Ok(false);
    };
    set_ini_value(&path, "PYTHON", "ENABLE_PYTHON", if enabled { "1" } else { "0" })?;
    Ok(true)
}

pub fn set_real_penalty_enabled(enabled: bool) -> Result<bool> {
    let Some(path) = python_ini() else {
        return Ok(false);
    };
    set_ini_value(&path, "REALPENALTY", "ACTIVE", if enabled { "1" } else { "0" })?;
    Ok(true)
}

/// Targeted INI edit. Unlike re-serialising the whole file (what the Electron app does),
/// this only touches the one key so user formatting and comments survive.
pub fn set_ini_value(path: &Path, section: &str, key: &str, value: &str) -> Result<()> {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let newline = if raw.contains("\r\n") { "\r\n" } else { "\n" };

    let mut lines: Vec<String> = raw.split(newline).map(str::to_string).collect();
    // `split` leaves a trailing empty element for files ending with a newline.
    let trailing_newline = lines.last().map(|l| l.is_empty()).unwrap_or(false);
    if trailing_newline {
        lines.pop();
    }

    let header = format!("[{section}]");
    let mut in_section = false;
    let mut section_found = false;
    let mut key_written = false;
    let mut insert_at: Option<usize> = None;

    for (idx, line) in lines.iter_mut().enumerate() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_section && !key_written {
                insert_at = Some(idx);
            }
            in_section = trimmed.eq_ignore_ascii_case(&header);
            if in_section {
                section_found = true;
            }
            continue;
        }

        if in_section && !key_written {
            if let Some((lhs, _)) = trimmed.split_once('=') {
                if lhs.trim().eq_ignore_ascii_case(key) {
                    *line = format!("{key}={value}");
                    key_written = true;
                }
            }
        }
    }

    if !key_written {
        let new_line = format!("{key}={value}");
        match insert_at {
            Some(pos) => lines.insert(pos, new_line),
            None if section_found => lines.push(new_line),
            None => {
                if !lines.is_empty() && !lines.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
                    lines.push(String::new());
                }
                lines.push(header);
                lines.push(new_line);
            }
        }
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let mut out = lines.join(newline);
    if trailing_newline || !raw.is_empty() {
        out.push_str(newline);
    }
    std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
