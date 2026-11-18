//! Browser login: the LFM App authenticates through Steam and receives the API key as a
//! `token` query parameter on a callback URL. The app receives it on the `liverytool://`
//! scheme, which a terminal cannot handle, so this implementation points the same flow at a
//! loopback HTTP listener and reads the token from the redirect.
//!
//! The token is stored in `~/.config/lfm-ac/config.json` (mode 0600 on Unix), so the TUI can
//! be used on a machine where the Electron app was never installed.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

const AUTH_ENDPOINT: &str = "https://api2.lowfuelmotorsport.com/auth/steam";
const SCHEME: &str = "liverytool";
const HANDLER_ID: &str = "lfm-ac-auth.desktop";

/// The URL the Electron app uses, and the only return target LFM definitely accepts.
pub const SCHEME_CALLBACK: &str = "liverytool://auth";

/// Where the token is cached for this tool.
pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("lfm-ac").join("config.json"))
}

pub fn load_token() -> Option<String> {
    let path = config_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let token = json.get("apiKey")?.as_str()?.trim().to_string();
    (!token.is_empty()).then_some(token)
}

pub fn save_token(token: &str) -> Result<PathBuf> {
    let path = config_path().context("no config directory")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let body = serde_json::json!({ "apiKey": token }).to_string();
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(path)
}

/// The LFM login page, told to come back to our loopback listener with the token.
pub fn auth_url(port: u16) -> String {
    let back = format!("http://127.0.0.1:{port}/auth");
    format!("{AUTH_ENDPOINT}?return_to={}", encode_component(&back))
}

/// The LFM login page with the callback scheme the desktop app uses.
pub fn scheme_auth_url() -> String {
    format!(
        "{AUTH_ENDPOINT}?return_to={}",
        encode_component(SCHEME_CALLBACK)
    )
}

/// Read `token=` out of a deep link such as `liverytool://auth?token=…`.
pub fn token_from_url(url: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if key == "token" && !value.is_empty() {
            return Some(decode_component(value));
        }
    }
    None
}

fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn decode_component(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// Pull `token=` out of an HTTP request line (`GET /auth?token=… HTTP/1.1`).
pub fn token_from_request(request: &str) -> Option<String> {
    let target = request.lines().next()?.split_whitespace().nth(1)?;
    let query = target.split_once('?')?.1;

    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if key == "token" && !value.is_empty() {
            return Some(decode_component(value));
        }
    }

    None
}

/// A loopback listener that waits for the login redirect.
pub struct Callback {
    listener: TcpListener,
    port: u16,
}

impl Callback {
    pub fn start() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).context("opening a loopback port")?;
        let port = listener.local_addr().context("reading the bound port")?.port();
        listener
            .set_nonblocking(true)
            .context("making the listener non-blocking")?;
        Ok(Self { listener, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Block until the browser lands on our URL, or the timeout expires.
    pub fn wait(self, timeout: Duration) -> Result<String> {
        let started = Instant::now();

        loop {
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    let request = read_request(&mut stream);
                    if let Some(token) = token_from_request(&request) {
                        respond(&mut stream, "Logged in — you can close this tab.");
                        return Ok(token);
                    }
                    // Browsers also fetch /favicon.ico and friends.
                    respond(&mut stream, "Waiting for the LFM login to finish…");
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error).context("accepting the login redirect"),
            }

            if started.elapsed() > timeout {
                bail!("timed out waiting for the browser login");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

fn runtime_token_path() -> PathBuf {
    runtime_dir().join("lfm-ac-auth-token")
}

/// The default handler that was replaced, so an interrupted login can restore it later.
fn runtime_previous_path() -> PathBuf {
    runtime_dir().join("lfm-ac-auth-previous")
}

fn mime() -> String {
    format!("x-scheme-handler/{SCHEME}")
}

/// Undo a handler registration left behind by a login that was killed mid-wait.
pub fn cleanup_stale_handler() {
    let Some(apps_dir) = dirs::data_dir().map(|dir| dir.join("applications")) else {
        return;
    };
    let desktop = apps_dir.join(HANDLER_ID);
    if !desktop.exists() {
        return;
    }

    let previous = std::fs::read_to_string(runtime_previous_path())
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && value != HANDLER_ID);

    match previous {
        Some(previous) => {
            let _ = std::process::Command::new("xdg-mime")
                .args(["default", &previous, &mime()])
                .status();
        }
        None => remove_default_entry(&mime()),
    }

    let _ = std::fs::remove_file(&desktop);
    let _ = std::fs::remove_file(runtime_previous_path());
    let _ = std::process::Command::new("update-desktop-database")
        .arg(&apps_dir)
        .status();
}

fn shell_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', ""))
}

/// Registers this binary as the `liverytool://` handler for the duration of one login, so the
/// browser's redirect is delivered to a small file the waiting process reads.
///
/// The deep link is the flow the desktop app uses, and the one LFM reliably redirects to; a
/// loopback `http://127.0.0.1` target makes the login bounce between Steam and LFM instead.
pub struct SchemeHandler {
    desktop: PathBuf,
    apps_dir: PathBuf,
    token_file: PathBuf,
    previous_default: Option<String>,
}

impl SchemeHandler {
    pub fn install() -> Result<Self> {
        let apps_dir = dirs::data_dir()
            .context("no data directory")?
            .join("applications");
        std::fs::create_dir_all(&apps_dir)?;

        let exe = std::env::current_exe().context("locating this executable")?;
        let token_file = runtime_token_path();
        let _ = std::fs::remove_file(&token_file);

        let desktop = apps_dir.join(HANDLER_ID);
        let exec = format!(
            "{} --auth-callback %u --token-file {}",
            shell_quote(&exe.display().to_string()),
            shell_quote(&token_file.display().to_string())
        );
        std::fs::write(
            &desktop,
            format!(
                "[Desktop Entry]\nType=Application\nName=LFM AC login callback\nExec={exec}\nMimeType=x-scheme-handler/{SCHEME};\nNoDisplay=true\nTerminal=false\n"
            ),
        )?;

        let _ = std::process::Command::new("update-desktop-database")
            .arg(&apps_dir)
            .status();

        let mime = format!("x-scheme-handler/{SCHEME}");
        let previous_default = std::process::Command::new("xdg-mime")
            .args(["query", "default", &mime])
            .output()
            .ok()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|value| !value.is_empty() && value != HANDLER_ID);

        let status = std::process::Command::new("xdg-mime")
            .args(["default", HANDLER_ID, &mime])
            .status()
            .context("running xdg-mime")?;
        if !status.success() {
            bail!("could not register the {SCHEME}:// handler");
        }

        let _ = std::fs::write(
            runtime_previous_path(),
            previous_default.clone().unwrap_or_default(),
        );

        Ok(Self {
            desktop,
            apps_dir,
            token_file,
            previous_default,
        })
    }

    pub fn wait(&self, timeout: Duration) -> Result<String> {
        let started = Instant::now();

        loop {
            if let Ok(raw) = std::fs::read_to_string(&self.token_file) {
                let token = raw.trim().to_string();
                if !token.is_empty() {
                    let _ = std::fs::remove_file(&self.token_file);
                    return Ok(token);
                }
            }

            if started.elapsed() > timeout {
                bail!("timed out waiting for the browser login");
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

impl Drop for SchemeHandler {
    fn drop(&mut self) {
        let mime = format!("x-scheme-handler/{SCHEME}");

        match &self.previous_default {
            Some(previous) => {
                let _ = std::process::Command::new("xdg-mime")
                    .args(["default", previous, &mime])
                    .status();
            }
            // Our own entry is the only one we can be sure about, so fall back to any other
            // installed handler (the Flatpak app, typically) before leaving the scheme
            // unassociated.
            None => match find_existing_handler() {
                Some(handler) => {
                    let _ = std::process::Command::new("xdg-mime")
                        .args(["default", &handler, &mime])
                        .status();
                }
                None => remove_default_entry(&mime),
            },
        }

        let _ = std::fs::remove_file(&self.desktop);
        let _ = std::fs::remove_file(runtime_previous_path());
        let _ = std::process::Command::new("update-desktop-database")
            .arg(&self.apps_dir)
            .status();
        let _ = std::fs::remove_file(&self.token_file);
    }
}

/// Look for another desktop file that claims the `liverytool://` scheme.
fn find_existing_handler() -> Option<String> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(data) = dirs::data_dir() {
        dirs.push(data.join("applications"));
        dirs.push(data.join("flatpak/exports/share/applications"));
    }
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    dirs.push(PathBuf::from("/usr/share/applications"));
    dirs.push(PathBuf::from("/usr/local/share/applications"));

    let needle = format!("x-scheme-handler/{SCHEME}");

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().and_then(|name| name.to_str()) == Some(HANDLER_ID) {
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            if body.contains(&needle) {
                if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                    return Some(name.to_string());
                }
            }
        }
    }

    None
}

/// Drop a scheme association from `mimeapps.list` (used when there was no previous handler).
fn remove_default_entry(mime: &str) {
    let Some(path) = dirs::config_dir().map(|dir| dir.join("mimeapps.list")) else {
        return;
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };

    let needle = format!("{mime}=");
    let filtered: Vec<&str> = raw
        .lines()
        .filter(|line| !line.trim_start().starts_with(&needle))
        .collect();

    let mut body = filtered.join("\n");
    if !body.ends_with('\n') {
        body.push('\n');
    }
    let _ = std::fs::write(path, body);
}

/// How the token comes back: the scheme handler normally, loopback if registration fails.
pub enum LoginWatcher {
    Scheme(SchemeHandler),
    Loopback(Callback),
}

impl LoginWatcher {
    pub fn wait(self, timeout: Duration) -> Result<String> {
        match self {
            LoginWatcher::Scheme(handler) => handler.wait(timeout),
            LoginWatcher::Loopback(callback) => callback.wait(timeout),
        }
    }
}

/// Prepare a login: returns the URL to open plus the watcher for the token.
pub fn start_login() -> Result<(String, LoginWatcher)> {
    match SchemeHandler::install() {
        Ok(handler) => Ok((scheme_auth_url(), LoginWatcher::Scheme(handler))),
        Err(_) => {
            let callback = Callback::start()?;
            let url = auth_url(callback.port());
            Ok((url, LoginWatcher::Loopback(callback)))
        }
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buffer = [0u8; 4096];
    match stream.read(&mut buffer) {
        Ok(n) if n > 0 => String::from_utf8_lossy(&buffer[..n]).into_owned(),
        _ => String::new(),
    }
}

fn respond(stream: &mut TcpStream, message: &str) {
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>LFM login</title>\
         <body style=\"font:16px system-ui;margin:4rem auto;max-width:32rem\">\
         <h2>LFM · Assetto Corsa</h2><p>{message}</p></body>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_token_from_a_redirect() {
        let request = "GET /auth?token=eyJ0eXAiOiJKV1Qi.abc-123 HTTP/1.1\r\nHost: 127.0.0.1\r\n";
        assert_eq!(
            token_from_request(request).as_deref(),
            Some("eyJ0eXAiOiJKV1Qi.abc-123")
        );
    }

    #[test]
    fn decodes_percent_escapes() {
        let request = "GET /auth?foo=1&token=a%2Bb%3Dc HTTP/1.1\r\n";
        assert_eq!(token_from_request(request).as_deref(), Some("a+b=c"));
    }

    #[test]
    fn ignores_requests_without_a_token() {
        assert_eq!(token_from_request("GET /favicon.ico HTTP/1.1\r\n"), None);
        assert_eq!(token_from_request("GET /auth?token= HTTP/1.1\r\n"), None);
        assert_eq!(token_from_request(""), None);
    }

    #[test]
    fn reads_the_token_from_a_deep_link() {
        assert_eq!(
            token_from_url("liverytool://auth?token=abc.def-123").as_deref(),
            Some("abc.def-123")
        );
        assert_eq!(token_from_url("liverytool://auth"), None);
        assert_eq!(token_from_url("liverytool://auth?token="), None);
    }

    #[test]
    fn the_scheme_url_matches_the_app_callback() {
        let url = scheme_auth_url();
        assert!(url.ends_with("return_to=liverytool%3A%2F%2Fauth"));
    }

    #[test]
    fn points_the_login_back_at_the_listener() {
        let url = auth_url(48231);
        assert!(url.starts_with("https://api2.lowfuelmotorsport.com/auth/steam?return_to="));
        assert!(url.contains("127.0.0.1%3A48231%2Fauth"));
    }
}
