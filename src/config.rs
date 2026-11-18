//! Reads the LFM App's own settings so an existing login can be reused, and locates the
//! Assetto Corsa install.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Values inherited from the installed Electron app.
#[derive(Debug, Default, Clone)]
pub struct AppConfig {
    pub api_key: Option<String>,
    pub ac_path: Option<PathBuf>,
    pub selected_content_ids: Vec<u32>,
    /// Where the settings were read from, for display in the TUI.
    pub source: Option<PathBuf>,
}

/// `electron-store` uses the app name, so `%APPDATA%\lfm-hub\config.json` on Windows and
/// `~/.config/lfm-hub/config.json` natively. The Flatpak keeps its config under
/// `~/.var/app/<app-id>/config/lfm-hub/config.json`.
pub fn lfm_config_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();

    if let Some(dir) = dirs::config_dir() {
        for name in APP_DIR_NAMES {
            out.push(dir.join(name).join("config.json"));
        }
    }

    if let Some(home) = dirs::home_dir() {
        // Known Flatpak id first, then any other Flatpak that ships the app.
        out.push(
            home.join(".var/app/com.lowfuelmotorsport.LFMApp/config/lfm-hub/config.json"),
        );

        if let Ok(entries) = std::fs::read_dir(home.join(".var/app")) {
            let mut flatpaks: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            flatpaks.sort();
            for app in flatpaks {
                for name in APP_DIR_NAMES {
                    out.push(app.join("config").join(name).join("config.json"));
                }
            }
        }
    }

    out
}

const APP_DIR_NAMES: [&str; 3] = ["lfm-hub", "LFM App", "lfm-app"];

pub fn load_app_config() -> AppConfig {
    let mut cfg = AppConfig::default();

    for path in lfm_config_paths() {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };

        cfg.source = Some(path.clone());

        if let Some(key) = json.get("apiKey").and_then(Value::as_str) {
            let key = key.trim();
            if !key.is_empty() {
                cfg.api_key = Some(key.to_string());
            }
        }

        if let Some(ac) = json.get("acSteamPath").and_then(Value::as_str) {
            let ac = ac.trim();
            if !ac.is_empty() {
                cfg.ac_path = Some(PathBuf::from(ac));
            }
        }

        if let Some(ids) = json
            .get("settings")
            .and_then(|s| s.get("acSelectedContentIds"))
            .and_then(Value::as_array)
        {
            cfg.selected_content_ids = ids
                .iter()
                .filter_map(|v| v.as_u64())
                .map(|v| v as u32)
                .collect();
        }

        break;
    }

    cfg
}

/// Mirrors `isLikelyAcPath()` from the app.
pub fn is_likely_ac_path(path: &Path) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    let has_exe = ["acs.exe", "AssettoCorsa.exe", "assettocorsa.exe"]
        .iter()
        .any(|exe| path.join(exe).is_file());
    has_exe || (path.join("content").is_dir() && path.join("system").is_dir())
}

/// Best-effort auto-detection: Steam root from the registry, then every library in
/// `libraryfolders.vdf` (Assetto Corsa's Steam AppID is 244210).
pub fn detect_ac_path() -> Option<PathBuf> {
    for root in steam_roots() {
        for library in steam_libraries(&root) {
            for dirname in ["assettocorsa", "Assetto Corsa", "assettocorsa_"] {
                let candidate = library.join("steamapps").join("common").join(dirname);
                if is_likely_ac_path(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Some(p) = steam_root_from_registry() {
        roots.push(p);
    }

    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Ok(base) = std::env::var(var) {
            let p = PathBuf::from(base).join("Steam");
            if p.is_dir() {
                roots.push(p);
            }
        }
    }

    roots.dedup();
    roots
}

#[cfg(windows)]
fn steam_root_from_registry() -> Option<PathBuf> {
    use std::process::Command;

    let out = Command::new("reg")
        .args(["query", r"HKCU\Software\Valve\Steam", "/v", "SteamPath"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("SteamPath"))?;
    let path = line.split("REG_SZ").nth(1)?.trim();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

#[cfg(not(windows))]
fn steam_root_from_registry() -> Option<PathBuf> {
    None
}

/// Parse `"path"  "D:\\SteamLibrary"` entries out of a Steam `libraryfolders.vdf`.
fn steam_libraries(steam_root: &Path) -> Vec<PathBuf> {
    let mut libraries = vec![steam_root.to_path_buf()];
    let vdf = steam_root.join("steamapps").join("libraryfolders.vdf");
    let Ok(text) = std::fs::read_to_string(&vdf) else {
        return libraries;
    };

    for line in text.lines() {
        let trimmed = line.trim();
        if !(trimmed.starts_with("\"path\"") || trimmed.starts_with("\"Path\"")) {
            continue;
        }
        if let Some(rest) = trimmed.split_once("\"path\"").map(|(_, r)| r) {
            if let Some(value) = rest.trim().strip_prefix('"').and_then(|r| r.split('"').next()) {
                if !value.is_empty() {
                    libraries.push(PathBuf::from(value.replace("\\\\", "\\")));
                }
            }
        }
    }

    libraries
}

/// Documents folder holding AC's config (`gameplay.ini`, `python.ini`).
pub fn ac_documents_dir() -> Option<PathBuf> {
    dirs::document_dir().map(|d| d.join("Assetto Corsa"))
}
