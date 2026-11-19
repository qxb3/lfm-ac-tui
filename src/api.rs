//! HTTP client for the two backends used by the LFM app.
//!
//! LFM API       -> Bearer token, season + event content lists.
//! PitPath API   -> `X-API-Key: Bearer <token>` + `X-App-Version`, content catalogue,
//!                  metadata, download tickets.

use std::sync::{Arc, RwLock};

use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

pub const LFM_BASE: &str = "https://api2.lowfuelmotorsport.com/api/";
pub const PIT_BASE: &str = "https://api.livery.lowfuelmotorsport.com/api/v1/";

/// The PitPath API gates clients on a minimum version (`version/hub`); advertise the
/// version this implementation was built against.
pub const APP_VERSION: &str = "1.1.13";

/// LFM's sim id for Assetto Corsa (the season payload groups AC under `"3"`).
const AC_SIM_ID: u32 = 3;

#[derive(Clone)]
pub struct ApiClient {
    http: reqwest::Client,
    /// Shared so a browser login can replace it while the client is in use.
    token: Arc<RwLock<String>>,
}

impl ApiClient {
    pub fn new(token: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("lfm-ac/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            http,
            token: Arc::new(RwLock::new(token)),
        })
    }

    pub fn token(&self) -> String {
        self.token
            .read()
            .map(|token| token.clone())
            .unwrap_or_default()
    }

    pub fn set_token(&self, token: impl Into<String>) {
        if let Ok(mut guard) = self.token.write() {
            *guard = token.into();
        }
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    fn lfm(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{LFM_BASE}{path}"))
            .bearer_auth(self.token())
    }

    fn pit(&self, path: &str) -> reqwest::RequestBuilder {
        self.pit_req(reqwest::Method::GET, path)
    }

    fn pit_req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{PIT_BASE}{path}"))
            .header("X-API-Key", format!("Bearer {}", self.token()))
            .header("X-App-Version", APP_VERSION)
    }

    /// Season index. Only sim `3` (Assetto Corsa) is relevant.
    pub async fn season(&self) -> Result<SeasonResponse> {
        send_json(self.lfm("v2/seasons/getMinifiedSeasonBySim")).await
    }

    pub async fn event_content(&self, event_id: u32) -> Result<EventContent> {
        send_json(self.lfm(&format!("v2/seasons/getContent/{event_id}"))).await
    }

    /// Full PitPath catalogue (`id`, `lfmId`, `contentType`, `minioKey`, `dlc`, `hash`).
    pub async fn catalogue(&self) -> Result<Vec<CatalogueEntry>> {
        send_json(self.pit("content/all")).await
    }

    pub async fn metadata(&self, id: u32) -> Result<ContentMetadata> {
        send_json(self.pit(&format!("content/{id}"))).await
    }

    /// Short-lived ticket carrying a presigned URL for the archive itself.
    pub async fn ticket(&self, id: u32) -> Result<DownloadTicket> {
        send_json(self.pit(&format!("content/{id}/download"))).await
    }

    /// Upcoming races of one series/event.
    pub async fn upcoming_sessions(&self, event_id: u32) -> Result<Vec<RaceDto>> {
        let sessions: UpcomingSessions = send_json(self.lfm(&format!(
            "v2/seasons/getUpcomingSessions/{event_id}"
        )))
        .await?;
        Ok(sessions.data)
    }

    /// Whether the race servers are live (and whether we're signed up).
    pub async fn check_data(&self, race_ids: &[u32]) -> Result<Vec<CheckDataEntry>> {
        let request = self
            .http
            .post(format!("{LFM_BASE}races/checkData"))
            .bearer_auth(self.token())
            .json(&serde_json::json!({ "races": race_ids }));
        send_json(request).await
    }

    /// Join link + password for a race. 404 until the server is live / signed up.
    pub async fn join_data(&self, race_id: u32) -> Result<JoinDataDto> {
        send_json(self.lfm(&format!("race/{race_id}/getJoinData"))).await
    }

    /// The signed-in user (`GET user`). The signup payload needs the name and id.
    pub async fn user(&self) -> Result<UserDto> {
        send_json(self.lfm("user")).await
    }

    /// Cars this race accepts: `v2/seasons/getAvailableCars/{event}?race_id={race}`.
    pub async fn available_cars(&self, event_id: u32, race_id: u32) -> Result<Vec<AvailableCar>> {
        let value: Value = send_json(self.lfm(&format!(
            "v2/seasons/getAvailableCars/{event_id}?race_id={race_id}"
        )))
        .await?;
        // Newer builds wrap the list in `{ "data": [...] }`; older ones return an array.
        let list = value.get("data").cloned().unwrap_or(value);
        serde_json::from_value(list).context("decoding the available cars list")
    }

    /// Pre-qualification laps of the signed-in driver for one event.
    pub async fn prequalification(&self, event_id: u32) -> Result<PreQualiStatus> {
        let value: Value = send_json(self.pit(&format!("hotlaps/{event_id}/me"))).await?;
        let rows = value.get("data").cloned().unwrap_or(value);
        let rows = rows
            .as_array()
            .context("unexpected pre-qualification payload")?;

        let mut eligible_classes: Vec<String> = Vec::new();
        for row in rows {
            if !has_lap_time(row) {
                continue;
            }
            let Some(class) = prequali_class(row) else {
                continue;
            };
            if !eligible_classes
                .iter()
                .any(|known| known.eq_ignore_ascii_case(&class))
            {
                eligible_classes.push(class);
            }
        }

        Ok(PreQualiStatus { eligible_classes })
    }

    /// Register the signed-in driver for a solo race.
    pub async fn signup(
        &self,
        race_id: u32,
        event_id: u32,
        car_id: i64,
        user: &UserDto,
    ) -> Result<()> {
        let body = serde_json::json!([{
            "vorname": user.vorname,
            "nachname": user.nachname,
            "userid": user.id,
            "driver_no": 0,
        }]);

        let res = self
            .pit_req(reqwest::Method::POST, &format!("signup/{race_id}/solo"))
            .query(&[
                ("carModel", car_id.to_string()),
                ("liveryId", "0".to_string()),
                ("teamId", "0".to_string()),
                ("teamName", String::new()),
                ("simId", AC_SIM_ID.to_string()),
                ("eventId", event_id.to_string()),
            ])
            .json(&body)
            .send()
            .await
            .context("signup request failed")?;

        check_ok(res).await
    }

    /// Withdraw the signed-in driver from a solo race.
    pub async fn withdraw(&self, race_id: u32, event_id: u32) -> Result<()> {
        let res = self
            .pit_req(reqwest::Method::DELETE, &format!("signup/{race_id}/solo"))
            .query(&[("eventId", event_id.to_string())])
            .send()
            .await
            .context("withdraw request failed")?;

        check_ok(res).await
    }

    /// Plain-text minimum client version.
    pub async fn required_version(&self) -> Result<String> {
        let res = self.pit("version/hub").send().await?;
        let status = res.status();
        let text = res.text().await?;
        if !status.is_success() {
            bail!("version/hub returned {status}");
        }
        Ok(text.trim().to_string())
    }
}

async fn send_json<T: DeserializeOwned>(req: reqwest::RequestBuilder) -> Result<T> {
    let res = req.send().await.context("request failed")?;
    let status = res.status();
    let text = res.text().await.context("reading response body")?;

    if !status.is_success() {
        let hint = match status.as_u16() {
            401 | 403 => " (token rejected — check --api-key / the LFM App login)",
            _ => "",
        };
        bail!("HTTP {status}{hint}: {}", truncate(&text, 240));
    }

    serde_json::from_str(&text).with_context(|| {
        format!(
            "unexpected response (HTTP {status}, {} bytes): {}",
            text.len(),
            truncate(&text, 240)
        )
    })
}

fn has_lap_time(row: &Value) -> bool {
    for key in ["best_lap", "totalTime", "lap_time"] {
        match row.get(key) {
            Some(Value::Number(n)) => {
                if n.as_f64().is_some_and(|v| v > 0.0) {
                    return true;
                }
            }
            Some(Value::String(s)) => {
                let s = s.trim();
                if !s.is_empty() && s != "-" && s != "0" {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn prequali_class(row: &Value) -> Option<String> {
    for key in ["class_name", "class", "car_class", "carClass", "classname"] {
        if let Some(value) = row.get(key).and_then(Value::as_str) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Signup/withdraw return either an empty body or a JSON error (`status`, `title`).
async fn check_ok(res: reqwest::Response) -> Result<()> {
    let status = res.status();
    if status.is_success() {
        return Ok(());
    }

    let text = res.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|value| {
            let title = value.get("title").and_then(Value::as_str);
            let state = value.get("status").and_then(Value::as_str);
            match (title, state) {
                (Some(title), Some(state)) => Some(format!("{title} ({state})")),
                (Some(title), None) => Some(title.to_string()),
                (None, Some(state)) => Some(state.to_string()),
                (None, None) => None,
            }
        })
        .unwrap_or_else(|| truncate(&text, 240));

    let hint = match status.as_u16() {
        401 | 403 => " (token rejected — check --api-key / the LFM App login)",
        _ => "",
    };
    bail!("HTTP {status}{hint}: {detail}")
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..s.floor_char_boundary(max)])
    }
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct SeasonResponse {
    #[serde(default)]
    pub series: std::collections::HashMap<String, SimBucket>,
    #[serde(default)]
    pub special_events: std::collections::HashMap<String, SimBucket>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SimBucket {
    #[serde(default)]
    pub series: Vec<SeriesRef>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SeriesRef {
    #[serde(default)]
    pub event_id: u32,
    #[serde(default)]
    pub series_name: String,
    #[serde(default)]
    pub event_style: String,
    #[serde(default)]
    pub active_week: i64,
    #[serde(default)]
    pub slug: Option<String>,
    /// Minimum safety rating to sign up. `1` (or absent) means no requirement.
    #[serde(default, deserialize_with = "flex_optional_f64")]
    pub min_sr: Option<f64>,
    #[serde(default)]
    pub min_license: Option<String>,
    /// The series requires a pre-qualification lap before signing up.
    #[serde(default, deserialize_with = "flex_bool")]
    pub prequali: bool,
    #[serde(default)]
    pub practice_server: Vec<PracticeServerDto>,
    #[serde(default)]
    pub practice_server_next_week: Vec<PracticeServerDto>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PracticeServerDto {
    /// Base64-encoded `acmanager://` join link.
    #[serde(default)]
    pub link: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct EventContent {
    #[serde(default)]
    pub tracks: Vec<RawEntity>,
    #[serde(default)]
    pub cars: Vec<RawEntity>,
}

/// The LFM payload uses `track_*` for tracks and `car_*` for cars in the same shape.
#[derive(Debug, Default, Deserialize)]
pub struct RawEntity {
    #[serde(default)]
    pub track_id: Option<i64>,
    #[serde(default)]
    pub car_id: Option<i64>,
    #[serde(default)]
    pub track_name: Option<String>,
    #[serde(default)]
    pub car_name: Option<String>,
    #[serde(default)]
    pub modder_name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CatalogueEntry {
    pub id: u32,
    #[serde(rename = "lfmId", default)]
    pub lfm_id: Option<i64>,
    #[serde(rename = "contentType", default)]
    pub content_type: String,
    #[serde(rename = "minioKey", default)]
    pub minio_key: Option<String>,
    #[serde(rename = "name", default)]
    pub name: String,
    #[serde(default, deserialize_with = "flex_bool")]
    pub dlc: bool,
}

fn flex_optional_f64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    let raw = Option::<Value>::deserialize(d)?;
    Ok(match raw {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().replace(',', ".").parse().ok(),
        _ => None,
    })
}

fn flex_bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let raw = Option::<Value>::deserialize(d)?;
    Ok(match raw {
        Some(Value::Bool(b)) => b,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(Value::String(s)) => s == "true" || s == "1",
        _ => false,
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContentMetadata {
    pub id: u32,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "contentType", default)]
    pub content_type: String,
    #[serde(rename = "lfmId", default)]
    pub lfm_id: Option<i64>,
    #[serde(default)]
    pub hash: String,
    /// JSON *string*: `{"files": [...], "directories": [...]}`.
    #[serde(rename = "contentStructure", default)]
    pub content_structure: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpcomingSessions {
    #[serde(default)]
    pub data: Vec<RaceDto>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RaceDto {
    #[serde(default)]
    pub race_id: u32,
    #[serde(default)]
    pub race_date: String,
    #[serde(default)]
    pub slots: u32,
    #[serde(default)]
    pub registered: u32,
    #[serde(rename = "signedUp", default)]
    pub signed_up: bool,
    // LFM sends this as 0/1, not a JSON boolean.
    #[serde(rename = "serverLive", default, deserialize_with = "flex_bool")]
    pub server_live: bool,
    #[serde(default)]
    pub server_name: Option<String>,
    #[serde(default)]
    pub sessions_length: Option<SessionsLengthDto>,
    #[serde(rename = "signupStart", default)]
    pub signup_start: String,
    #[serde(rename = "signupCloses", default)]
    pub signup_closes: String,
    #[serde(default)]
    pub track: Option<TrackDto>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionsLengthDto {
    #[serde(default)]
    pub p: u32,
    #[serde(default)]
    pub q: u32,
    #[serde(default)]
    pub r: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TrackDto {
    #[serde(default)]
    pub track_name: Option<String>,
}

/// `POST races/checkData` entry.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CheckDataEntry {
    #[serde(default)]
    pub race_id: u32,
    #[serde(rename = "serverLive", default, deserialize_with = "flex_bool")]
    pub server_live: bool,
    #[serde(rename = "signedUp", default)]
    pub signed_up: bool,
}

/// `race/{id}/getJoinData` — `link` and `pw` are base64.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct JoinDataDto {
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub pw: Option<String>,
    #[serde(default)]
    pub servername: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserDto {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub vorname: String,
    #[serde(default)]
    pub nachname: String,
    #[serde(default)]
    pub safety_rating: Option<Value>,
    #[serde(rename = "rating_by_sim", default)]
    pub rating_by_sim: Vec<RatingBySimDto>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RatingBySimDto {
    #[serde(default)]
    pub sim_id: i64,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub rating: Option<Value>,
}

/// What `hotlaps/{event}/me` says about the signed-in driver.
#[derive(Debug, Clone, Default)]
pub struct PreQualiStatus {
    /// Car classes the driver has set a pre-qualification lap in.
    pub eligible_classes: Vec<String>,
}

/// One car offered by `getAvailableCars` for a race.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AvailableCar {
    #[serde(default, alias = "id")]
    pub car_id: i64,
    #[serde(default, alias = "name")]
    pub car_name: String,
    #[serde(default, alias = "class_name")]
    pub class: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DownloadTicket {
    #[serde(rename = "downloadUrl")]
    pub download_url: String,
    #[serde(rename = "fileSize", default)]
    pub file_size: Option<Value>,
}

impl DownloadTicket {
    pub fn size(&self) -> u64 {
        match &self.file_size {
            Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
            Some(Value::String(s)) => s.parse().unwrap_or(0),
            _ => 0,
        }
    }
}
