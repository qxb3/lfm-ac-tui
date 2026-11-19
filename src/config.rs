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

/// Assetto Corsa's Steam AppID.
const AC_APP_ID: &str = "244210";

/// Folder names AC has historically been installed under, used when its app manifest
/// cannot be read.
const AC_DIR_NAMES: [&str; 5] = [
    "assettocorsa",
    "Assetto Corsa",
    "assettocorsa_",
    "AssettoCorsa",
    "assettocorsa-1",
];

/// Best-effort auto-detection: every Steam root for the platform, then every library in
/// `libraryfolders.vdf`, looking for the entry named by AC's app manifest.
pub fn detect_ac_path() -> Option<PathBuf> {
    for root in steam_roots() {
        for library in steam_libraries(&root) {
            if let Some(found) = ac_in_library(&library) {
                return Some(found);
            }
        }
    }
    None
}

/// Locate Assetto Corsa inside one Steam library. `appmanifest_244210.acf` records the
/// real install folder name, which avoids guessing and works on case-sensitive filesystems.
fn ac_in_library(library: &Path) -> Option<PathBuf> {
    let steamapps = library.join("steamapps");
    let manifest = steamapps.join(format!("appmanifest_{AC_APP_ID}.acf"));

    if let Some(dirname) = std::fs::read_to_string(manifest)
        .ok()
        .and_then(|text| vdf_value(&text, "installdir"))
    {
        let candidate = steamapps.join("common").join(dirname);
        if is_likely_ac_path(&candidate) {
            return Some(candidate);
        }
    }

    AC_DIR_NAMES
        .iter()
        .map(|dirname| steamapps.join("common").join(dirname))
        .find(|candidate| is_likely_ac_path(candidate))
}

fn steam_roots() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    #[cfg(windows)]
    {
        if let Some(p) = steam_root_from_registry() {
            candidates.push(p);
        }

        for var in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Ok(base) = std::env::var(var) {
                candidates.push(PathBuf::from(base).join("Steam"));
            }
        }
    }

    #[cfg(target_os = "linux")]
    candidates.extend(linux_steam_roots());

    #[cfg(target_os = "macos")]
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("Library/Application Support/Steam"));
    }

    dedupe_existing(candidates)
}

/// Keep the roots that exist, collapsing duplicates that reach the same install through
/// symlinks (`~/.steam/steam` -> `~/.local/share/Steam`, for example).
fn dedupe_existing(candidates: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut roots = Vec::new();

    for candidate in candidates {
        if !candidate.is_dir() {
            continue;
        }
        let key = candidate.canonicalize().unwrap_or_else(|_| candidate.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        roots.push(candidate);
    }

    roots
}

/// Steam locations used by the common Linux packaging routes: distro packages (Debian,
/// Arch, Fedora, ...), the official Flatpak, and the Snap. `STEAM_*` environment
/// variables are honoured when set, since some launchers relocate the root.
#[cfg(target_os = "linux")]
fn linux_steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    for var in ["STEAM_ROOT", "STEAM_DIR"] {
        if let Some(value) = std::env::var_os(var).filter(|v| !v.is_empty()) {
            roots.push(PathBuf::from(value));
        }
    }

    // `XDG_DATA_HOME` (defaults to ~/.local/share) holds the native install.
    if let Some(data) = dirs::data_dir() {
        roots.push(data.join("Steam"));
    }

    if let Some(home) = dirs::home_dir() {
        // Native packages; `~/.steam/*` is Steam's own symlink farm.
        roots.push(home.join(".local/share/Steam"));
        roots.push(home.join(".steam/steam"));
        roots.push(home.join(".steam/root"));
        roots.push(home.join(".steam/debian-installation"));

        // Flatpak (`com.valvesoftware.Steam`), both the current and older data layouts.
        let flatpak = home.join(".var/app/com.valvesoftware.Steam");
        roots.push(flatpak.join("data/Steam"));
        roots.push(flatpak.join(".local/share/Steam"));
        roots.push(flatpak.join(".steam/steam"));
        roots.push(flatpak.join(".steam/root"));

        // Snap (`snap install steam`).
        let snap = home.join("snap/steam/common");
        roots.push(snap.join(".local/share/Steam"));
        roots.push(snap.join(".steam/steam"));
        roots.push(snap.join(".steam/root"));
    }

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

/// Read the first `"key"  "value"` pair with the given key out of Steam's VDF/ACF text.
/// Returns `None` when the key is absent.
fn vdf_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (found, rest) = split_quoted(line.trim())?;
        if !found.eq_ignore_ascii_case(key) {
            return None;
        }
        let (value, _) = split_quoted(rest.trim_start())?;
        if value.is_empty() {
            None
        } else {
            // VDF escapes backslashes; Linux paths carry none and pass through unchanged.
            Some(value.replace("\\\\", "\\"))
        }
    })
}

fn split_quoted(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix('"')?;
    let (value, rest) = rest.split_once('"')?;
    Some((value, rest))
}

/// Parse `"path"  "D:\\SteamLibrary"` / `"path"  "/mnt/games/SteamLibrary"` entries out
/// of a Steam `libraryfolders.vdf`.
fn steam_libraries(steam_root: &Path) -> Vec<PathBuf> {
    let mut libraries = vec![steam_root.to_path_buf()];
    let vdf = steam_root.join("steamapps").join("libraryfolders.vdf");
    let Ok(text) = std::fs::read_to_string(&vdf) else {
        return libraries;
    };

    // Newer clients also keep a copy under `config/`; the `steamapps` one is canonical.
    libraries.extend(
        text.lines()
            .filter_map(|line| vdf_value(line.trim(), "path"))
            .map(PathBuf::from),
    );

    dedupe_existing(libraries)
}

/// Documents folder holding AC's config (`gameplay.ini`, `python.ini`).
pub fn ac_documents_dir() -> Option<PathBuf> {
    dirs::document_dir().map(|d| d.join("Assetto Corsa"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_install(root: &Path, dirname: &str) -> PathBuf {
        let ac = root.join("steamapps").join("common").join(dirname);
        std::fs::create_dir_all(ac.join("content")).unwrap();
        std::fs::create_dir_all(ac.join("system")).unwrap();
        ac
    }

    #[test]
    fn reads_install_dir_from_app_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let expected = fake_install(dir.path(), "assettocorsa");
        std::fs::create_dir_all(dir.path().join("steamapps")).unwrap();
        std::fs::write(
            dir.path().join("steamapps/appmanifest_244210.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\t\"244210\"\n\t\"installdir\"\t\t\"assettocorsa\"\n}\n",
        )
        .unwrap();

        assert_eq!(ac_in_library(dir.path()), Some(expected));
    }

    #[test]
    fn falls_back_to_known_folder_names() {
        let dir = tempfile::tempdir().unwrap();
        let expected = fake_install(dir.path(), "Assetto Corsa");
        assert_eq!(ac_in_library(dir.path()), Some(expected));
    }

    #[test]
    fn ignores_library_without_ac() {
        let dir = tempfile::tempdir().unwrap();
        fake_install(dir.path(), "some-other-game");
        assert_eq!(ac_in_library(dir.path()), None);
    }

    #[test]
    fn parses_linux_library_paths() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("games/SteamLibrary");
        std::fs::create_dir_all(library.join("steamapps")).unwrap();
        std::fs::create_dir_all(dir.path().join("steamapps")).unwrap();
        std::fs::write(
            dir.path().join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
                dir.path().display(),
                library.display()
            ),
        )
        .unwrap();

        let libraries = steam_libraries(dir.path());
        assert!(libraries.contains(&library));
        // The root is listed twice (implicitly and in the VDF) but only kept once.
        assert_eq!(libraries.len(), 2);
    }

    #[test]
    fn parses_windows_escaped_library_paths() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("steam");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::create_dir_all(dir.path().join("steamapps")).unwrap();
        std::fs::write(
            dir.path().join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
                library.display()
            ),
        )
        .unwrap();

        assert!(steam_libraries(dir.path()).contains(&library));
    }

    #[test]
    fn vdf_value_is_case_insensitive_and_unescapes() {
        let text = "\"AppState\"\n{\n\t\"InstallDir\"\t\t\"Assetto\\\\Corsa\"\n}\n";
        assert_eq!(vdf_value(text, "installdir").as_deref(), Some("Assetto\\Corsa"));
        assert_eq!(vdf_value(text, "missing"), None);
    }
}
