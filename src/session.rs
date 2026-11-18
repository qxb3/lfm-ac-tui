//! Session join helpers: LFM stores join links and passwords base64-encoded, and the
//! resulting URLs are `acmanager://…` links that need to be handed to the desktop.

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;

/// LFM encodes join links/passwords with `btoa`, so plain base64; be tolerant of
/// missing padding and the URL-safe alphabet.
pub fn decode_base64(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let decoders: [fn(&str) -> Result<Vec<u8>, base64::DecodeError>; 4] = [
        |value| STANDARD.decode(value),
        |value| STANDARD_NO_PAD.decode(value),
        |value| URL_SAFE.decode(value),
        |value| URL_SAFE_NO_PAD.decode(value),
    ];

    for decode in decoders {
        if let Ok(bytes) = decode(trimmed) {
            if let Ok(text) = String::from_utf8(bytes) {
                return Some(text);
            }
        }
    }

    None
}

/// Decode a join link if it looks base64, otherwise pass it through.
pub fn decode_link_or_raw(value: &str) -> String {
    decode_base64(value).unwrap_or_else(|| value.to_string())
}

/// Assetto Corsa's Steam app id, used to launch the game through Steam.
pub const AC_APP_ID: &str = "244210";

fn spawn_detached(program: &str, args: &[&str]) -> bool {
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Arguments that hand a join URL to the game's launcher through Steam.
pub fn join_argv(url: &str) -> [&str; 3] {
    ["-applaunch", AC_APP_ID, url]
}

/// Hand a URL to the desktop (websites, and join links on platforms that have a
/// protocol handler).
pub fn open_url(url: &str) -> bool {
    if cfg!(target_os = "windows") {
        spawn_detached("cmd", &["/C", "start", "", url])
    } else if cfg!(target_os = "macos") {
        spawn_detached("open", &[url])
    } else {
        spawn_detached("xdg-open", &[url])
    }
}

/// Open an `acmanager://` join link.
///
/// A desktop protocol handler cannot work on Linux: the link is only meaningful inside the
/// game's Wine/Proton prefix, and a handler would have to rebuild Steam's runtime
/// environment (a plain `proton run` hangs waiting on the Steam runtime, and running
/// Content Manager through protontricks starts it without a Steam session, where it cannot
/// launch the game). Asking Steam to start the game with the link as an argument avoids all
/// of that: Steam sets up the prefix, and Content Manager parses the URL from its command
/// line, exactly as it does on Windows.
pub fn open_join_link(url: &str) -> bool {
    if cfg!(target_os = "linux") && url.starts_with("acmanager://") {
        return spawn_detached("steam", &join_argv(url));
    }

    open_url(url)
}

#[cfg(unix)]
pub fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let candidates: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];

    for (program, args) in candidates {
        let Ok(mut child) = Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }

        if let Ok(status) = child.wait() {
            if status.success() {
                return true;
            }
        }
    }

    false
}

#[cfg(not(unix))]
pub fn copy_to_clipboard(_text: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_standard_base64() {
        assert_eq!(
            decode_base64("YWNtYW5hZ2VyOi8vcmFjZS9vbmxpbmUvam9pbg==").as_deref(),
            Some("acmanager://race/online/join")
        );
    }

    #[test]
    fn decodes_without_padding() {
        assert_eq!(decode_base64("YWNtYW5hZ2Vy").as_deref(), Some("acmanager"));
    }

    #[test]
    fn passes_through_non_base64() {
        assert_eq!(decode_link_or_raw("power"), "power");
    }

    #[test]
    fn join_links_are_handed_to_steam() {
        let url = "acmanager://race/online/join?ip=1.2.3.4&httpPort=9039";
        assert_eq!(join_argv(url), ["-applaunch", "244210", url]);
    }
}
