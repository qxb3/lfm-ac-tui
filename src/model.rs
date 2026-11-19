//! Shared domain types: the LFM season/content model and per-content status.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentKind {
    Track,
    Car,
}

impl ContentKind {
    pub fn label(self) -> &'static str {
        match self {
            ContentKind::Track => "track",
            ContentKind::Car => "car",
        }
    }
}

/// One downloadable LFM content item (a track or a car of a series).
#[derive(Clone, Debug)]
pub struct ContentItem {
    /// PitPath content id — `None` when the mod is not in the catalogue.
    pub content_id: Option<u32>,
    pub lfm_id: i64,
    pub name: String,
    pub kind: ContentKind,
    /// Several LFM entries can point at the same physical mod.
    pub minio_key: Option<String>,
    pub dlc: bool,
    pub in_db: bool,
    pub creator: String,
}

#[derive(Clone, Debug)]
pub struct Series {
    pub event_id: u32,
    pub name: String,
    pub style: String,
    pub active_week: i64,
    /// LFM series slug, used for the website link (`/series/<slug>`).
    pub slug: String,
    /// Base64-encoded `acmanager://` join links, straight from the season payload.
    pub practice_this_week: Vec<PracticeLink>,
    pub practice_next_week: Vec<PracticeLink>,
    pub content: Vec<ContentItem>,
}

impl Series {
    pub fn installed_count(&self, statuses: &HashMap<u32, Status>) -> usize {
        self.content
            .iter()
            .filter(|c| match c.content_id {
                Some(id) => {
                    statuses.get(&id).copied().unwrap_or(Status::Unknown) == Status::Installed
                }
                None => false,
            })
            .count()
    }

    pub fn syncable_count(&self) -> usize {
        self.content
            .iter()
            .filter(|c| c.in_db && !c.dlc && c.content_id.is_some())
            .count()
    }
}

#[derive(Clone, Debug, Default)]
pub struct PracticeLink {
    pub label: String,
    /// Base64 as returned by LFM; decoded when the user asks to join.
    pub encoded: String,
}

/// One upcoming LFM race of the selected series.
#[derive(Clone, Debug, Default)]
pub struct RaceInfo {
    pub race_id: u32,
    pub date: String,
    pub track: String,
    pub slots: u32,
    pub registered: u32,
    pub signed_up: bool,
    pub server_live: bool,
    pub server_name: String,
    pub practice_minutes: u32,
    pub quali_minutes: u32,
    pub race_minutes: u32,
    pub signup_opens: String,
    pub signup_closes: String,
}

/// A car the server accepts for one race, as offered in the sign-up popup.
#[derive(Clone, Debug, Default)]
pub struct CarOption {
    /// Server's `carModel` value used in the signup query.
    pub id: i64,
    pub name: String,
    pub class: String,
}

impl CarOption {
    /// `GT3 · Porsche 911 GT3 R` — the class is skipped when the API omits it.
    pub fn label(&self) -> String {
        if self.class.is_empty() {
            self.name.clone()
        } else {
            format!("{} · {}", self.class, self.name)
        }
    }
}

/// Resolved join data shown in the popup.
#[derive(Clone, Debug, Default)]
pub struct JoinInfo {
    pub title: String,
    pub url: Option<String>,
    pub password: Option<String>,
    pub note: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Unknown,
    Checking,
    Installed,
    /// Marker hash differs from the server hash.
    Outdated,
    /// Most expected files are on disk, but some are missing (usually a stale version).
    Partial,
    /// Structure/marker check failed.
    Missing,
    /// Not present in the PitPath catalogue — the user has to download it manually.
    NotInDb,
    /// Official DLC — never downloaded by the app.
    Dlc,
}

impl Status {
    pub fn tag(self) -> &'static str {
        match self {
            Status::Unknown => "?",
            Status::Checking => "..",
            Status::Installed => "ok",
            Status::Outdated => "upd",
            Status::Partial => "part",
            Status::Missing => "new",
            Status::NotInDb => "ext",
            Status::Dlc => "dlc",
        }
    }

    pub fn is_installed(self) -> bool {
        matches!(self, Status::Installed)
    }

    pub fn needs_download(self) -> bool {
        matches!(
            self,
            Status::Missing | Status::Outdated | Status::Partial | Status::Unknown
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProgressView {
    pub label: String,
    pub done: u64,
    pub total: u64,
    /// Bytes per second.
    pub bps: f64,
}

impl ProgressView {
    pub fn ratio(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f64 / self.total as f64).clamp(0.0, 1.0)
        }
    }
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}
