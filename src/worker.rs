//! Background task: all network and disk work happens here, the TUI only receives
//! [`Event`]s.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use futures_util::{stream, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::ac;
use crate::api::{
    ApiClient, CatalogueEntry, ContentMetadata, EventContent, PracticeServerDto, RaceDto, RawEntity,
    SeriesRef,
};
use crate::model::{
    human_bytes, ContentItem, ContentKind, JoinInfo, PracticeLink, ProgressView, RaceInfo, Series,
    Status,
};
use crate::session;

#[derive(Debug)]
pub enum Command {
    Refresh,
    Sync(Vec<u32>),
    Verify(Vec<u32>),
    Purge(Vec<u32>),
    Scripts,
    /// Load the upcoming races of one series (event id).
    Sessions(u32),
    /// Resolve the join link/password for a race.
    JoinRace { race_id: u32, label: String },
    /// Log in through the browser and store the API key.
    Login,
}

#[derive(Debug)]
pub enum Event {
    Busy(bool),
    Log(String),
    Season(Vec<Series>),
    Status(u32, Status),
    /// Full check result, including how many expected files were found.
    Checked { id: u32, status: Status, present: usize, total: usize },
    Progress(Option<ProgressView>),
    Sessions { event_id: u32, races: Vec<RaceInfo> },
    JoinInfo(JoinInfo),
    LoggedIn,
}

pub struct Worker {
    api: ApiClient,
    ac_path: Option<PathBuf>,
    tx: mpsc::UnboundedSender<Event>,
}

impl Worker {
    pub fn new(
        api: ApiClient,
        ac_path: Option<PathBuf>,
        tx: mpsc::UnboundedSender<Event>,
    ) -> Self {
        Self { api, ac_path, tx }
    }

    pub async fn run(self, mut rx: mpsc::UnboundedReceiver<Command>) {
        // Mod work is exclusive (one download/purge/scan at a time), but the command
        // loop must keep receiving so session lookups can run in parallel with a long
        // integrity scan.
        let exclusive = std::sync::Arc::new(tokio::sync::Mutex::new(()));

        while let Some(command) = rx.recv().await {
            match command {
                Command::Sessions(event_id) => {
                    spawn_sessions(self.api.clone(), self.tx.clone(), event_id);
                }
                Command::JoinRace { race_id, label } => {
                    spawn_join(self.api.clone(), self.tx.clone(), race_id, label);
                }
                command => {
                    let api = self.api.clone();
                    let ac_path = self.ac_path.clone();
                    let tx = self.tx.clone();
                    let exclusive = exclusive.clone();

                    tokio::spawn(async move {
                        let _guard = exclusive.lock().await;
                        let worker = Worker { api, ac_path, tx };

                        worker.send(Event::Busy(true));
                        if let Err(error) = worker.dispatch(command).await {
                            worker.log(format!("error: {error:#}"));
                        }
                        worker.send(Event::Progress(None));
                        worker.send(Event::Busy(false));
                    });
                }
            }
        }
    }

    async fn dispatch(&self, command: Command) -> Result<()> {
        match command {
            Command::Refresh => self.refresh().await,
            Command::Sync(ids) => self.sync(ids).await,
            Command::Login => self.login().await,
            Command::Verify(ids) => self.verify(ids).await,
            Command::Purge(ids) => self.purge(ids).await,
            Command::Scripts => self.scripts().await,
            Command::Sessions(_) | Command::JoinRace { .. } => unreachable!(),
        }
    }

    // ------------------------------------------------------------- plumbing

    fn send(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    fn log(&self, msg: impl Into<String>) {
        self.send(Event::Log(msg.into()));
    }

    fn progress(&self, label: &str, done: u64, total: u64, bps: f64) {
        self.send(Event::Progress(Some(ProgressView {
            label: label.to_string(),
            done,
            total,
            bps,
        })));
    }

    fn require_ac(&self) -> Result<PathBuf> {
        self.ac_path.clone().context(
            "no Assetto Corsa installation found — pass --ac-path or set acSteamPath in the LFM App",
        )
    }

    // ---------------------------------------------------------------- tasks

    async fn refresh(&self) -> Result<()> {
        if let Ok(required) = self.api.required_version().await {
            if required != crate::api::APP_VERSION {
                self.log(format!(
                    "note: server expects client version {required}; this build reports {}",
                    crate::api::APP_VERSION
                ));
            }
        }

        self.log("Fetching season and content catalogue…");
        let (catalogue, season) = tokio::try_join!(self.api.catalogue(), self.api.season())?;
        self.log(format!("Catalogue: {} entries", catalogue.len()));

        let mut refs: Vec<SeriesRef> = Vec::new();
        if let Some(bucket) = season.series.get("3") {
            refs.extend(bucket.series.iter().cloned());
        }
        if let Some(bucket) = season.special_events.get("3") {
            refs.extend(bucket.series.iter().cloned());
        }

        if refs.is_empty() {
            bail!("LFM returned no Assetto Corsa series");
        }

        let mut series = Vec::new();
        for reference in &refs {
            match self.api.event_content(reference.event_id).await {
                Ok(content) => series.push(build_series(reference, content, &catalogue)),
                Err(error) => self.log(format!(
                    "{}: {error:#}",
                    if reference.series_name.is_empty() {
                        format!("event {}", reference.event_id)
                    } else {
                        reference.series_name.clone()
                    }
                )),
            }
        }

        let entries: usize = series.iter().map(|s| s.content.len()).sum();
        self.log(format!("Loaded {} series ({entries} entries)", series.len()));

        self.send(Event::Season(series.clone()));
        self.check_ids(&collect_ids(&series)).await;
        Ok(())
    }

    async fn verify(&self, ids: Vec<u32>) -> Result<()> {
        if ids.is_empty() {
            self.log("Nothing to verify");
            return Ok(());
        }
        self.check_ids(&ids).await;
        Ok(())
    }

    async fn check_ids(&self, ids: &[u32]) {
        let ac = match &self.ac_path {
            Some(path) => path.clone(),
            None => {
                self.log("No AC install — cannot verify content");
                return;
            }
        };

        let Some(index) = self.build_index(&ac).await else {
            return;
        };

        for &id in ids {
            self.send(Event::Status(id, Status::Checking));
        }

        let mut checks = stream::iter(ids.to_vec())
            .map(|id| {
                let api = self.api.clone();
                let ac = ac.clone();
                let index = index.clone();
                async move {
                    let result = match api.metadata(id).await {
                        Ok(meta) => {
                            let meta_for_disk = meta.clone();
                            let ac_for_disk = ac.clone();
                            let index_for_disk = index.clone();
                            match tokio::task::spawn_blocking(move || {
                                ac::integrity::check_with_index(
                                    &index_for_disk,
                                    &ac_for_disk,
                                    &meta_for_disk,
                                )
                            })
                            .await
                            {
                                Ok(integrity) => Ok((
                                    status_from(&integrity),
                                    integrity.present,
                                    integrity.total,
                                )),
                                Err(error) => Err(anyhow::anyhow!("worker panicked: {error}")),
                            }
                        }
                        Err(error) => Err(error),
                    };
                    (id, result)
                }
            })
            .buffer_unordered(10);

        while let Some((id, result)) = checks.next().await {
            match result {
                Ok((status, present, total)) => {
                    self.send(Event::Checked { id, status, present, total });
                }
                Err(error) => {
                    self.log(format!("content {id}: {error:#}"));
                    self.send(Event::Status(id, Status::Missing));
                }
            }
        }

        self.log("Integrity check finished");
    }

    async fn sync(&self, ids: Vec<u32>) -> Result<()> {
        if ids.is_empty() {
            self.log("Nothing to sync");
            return Ok(());
        }
        let ac = self.require_ac()?;
        let index = self
            .build_index(&ac)
            .await
            .context("indexing the AC install")?;
        let total = ids.len();

        for (position, id) in ids.into_iter().enumerate() {
            self.log(format!("[{}/{total}] content {id}", position + 1));
            if let Err(error) = self.install_one(id, &ac, &index).await {
                self.log(format!("content {id} failed: {error:#}"));
                self.send(Event::Status(id, Status::Missing));
            }
        }

        Ok(())
    }

    /// Browser login: the token arrives on the `liverytool://` deep link, see [`crate::auth`].
    async fn login(&self) -> Result<()> {
        let (url, watcher) = crate::auth::start_login()?;

        self.log("Opening the LFM login page in your browser…");
        self.log(format!("If nothing opens, visit: {url}"));
        if !crate::session::open_url(&url) {
            self.log("No browser could be started — open that URL manually");
        }
        self.log("Your browser may ask to open an external app — allow it.");
        self.log("Waiting for the login to finish…");

        let token = match tokio::task::spawn_blocking(move || {
            watcher.wait(std::time::Duration::from_secs(300))
        })
        .await
        .context("login task")?
        {
            Ok(token) => token,
            Err(error) => {
                self.log(format!("Login did not complete: {error}"));
                self.log(
                    "If the browser showed an error after signing in, pass the key with --api-key instead",
                );
                return Err(error);
            }
        };

        let path = crate::auth::save_token(&token)?;
        self.api.set_token(token);
        self.log(format!("Logged in — API key saved to {}", path.display()));
        self.send(Event::LoggedIn);

        self.refresh().await
    }

    async fn build_index(&self, ac: &Path) -> Option<Arc<ac::index::PathIndex>> {
        let path = ac.to_path_buf();
        match tokio::task::spawn_blocking(move || ac::index::PathIndex::build(&path)).await {
            Ok(Ok(index)) => {
                self.log(format!(
                    "Indexed {} files under the AC install",
                    index.file_count()
                ));
                Some(Arc::new(index))
            }
            Ok(Err(error)) => {
                self.log(format!("Could not index the AC install: {error:#}"));
                None
            }
            Err(error) => {
                self.log(format!("Indexing panicked: {error}"));
                None
            }
        }
    }

    async fn install_one(
        &self,
        id: u32,
        ac: &Path,
        index: &ac::index::PathIndex,
    ) -> Result<()> {
        let meta = self.api.metadata(id).await?;
        let label = display_name(&meta);

        let existing = ac::integrity::check_with_index(index, ac, &meta);

        if existing.healthy() {
            self.log(format!("{label} is already installed and up to date"));
            self.send(Event::Status(id, Status::Installed));
            return Ok(());
        }

        if existing.needs_update {
            let removed = ac::tool_file::remove_markers(ac, id)?;
            self.log(format!(
                "{label}: version changed, replacing ({removed} marker(s) dropped)"
            ));
        }

        let ticket = self.api.ticket(id).await?;
        let expected = ticket.size();
        self.log(format!(
            "Downloading {label} ({})",
            if expected > 0 {
                human_bytes(expected)
            } else {
                "size unknown".to_string()
            }
        ));

        let temp = tempfile::Builder::new()
            .prefix("lfm-ac-")
            .suffix(".archive")
            .tempfile()
            .context("creating temporary download file")?;

        let received = self
            .download(&ticket.download_url, temp.path(), &label)
            .await?;
        self.send(Event::Progress(None));

        if expected > 0 && received != expected {
            self.log(format!(
                "warning: expected {} but received {}",
                human_bytes(expected),
                human_bytes(received)
            ));
        }

        let kind = ac::extract::sniff(temp.path())?;
        self.log(format!(
            "Extracting {} archive ({})",
            kind.label(),
            human_bytes(received)
        ));

        let archive = temp.path().to_path_buf();
        let target = ac.to_path_buf();
        let written = tokio::task::spawn_blocking(move || {
            ac::extract::extract(&archive, kind, &target)
        })
        .await
        .context("extraction task")??;

        let structure = ac::integrity::parse_structure(&meta.content_structure);

        // Extraction used to scatter a mod across the game root and still record a
        // marker, leaving a "successful" install with no files in it. Confirm the files
        // are at their manifest paths before anything is recorded.
        if structure
            .files
            .iter()
            .any(|rel| !ac.join(rel).is_file())
        {
            let ac_for_check = ac.to_path_buf();
            let meta_for_check = meta.clone();
            let check = tokio::task::spawn_blocking(move || {
                let index = ac::index::PathIndex::build(&ac_for_check)?;
                Ok::<_, anyhow::Error>(ac::integrity::check_with_index(
                    &index,
                    &ac_for_check,
                    &meta_for_check,
                ))
            })
            .await
            .context("post-install check task")??;

            if !check.healthy() {
                bail!(
                    "{label}: files did not land under the expected paths ({} of {} found) — \
                     the archive layout is not what the manifest describes",
                    check.present,
                    check.total
                );
            }
        }

        let markers = ac::tool_file::write_markers(
            ac,
            &meta,
            &structure.files,
            &structure.directories,
        )?;

        self.log(format!(
            "Installed {label} — {written} files, {} marker(s)",
            markers.len()
        ));
        self.send(Event::Status(id, Status::Installed));
        Ok(())
    }

    async fn download(&self, url: &str, destination: &Path, label: &str) -> Result<u64> {
        let response = self
            .api
            .http()
            .get(url)
            .send()
            .await
            .context("requesting the download URL")?;

        if !response.status().is_success() {
            bail!("download URL returned HTTP {}", response.status());
        }

        let total = response.content_length().unwrap_or(0);
        let mut body = response.bytes_stream();
        let mut file = tokio::fs::File::create(destination)
            .await
            .with_context(|| format!("creating {}", destination.display()))?;

        let start = Instant::now();
        let mut last_update = Instant::now();
        let mut done = 0u64;

        while let Some(chunk) = body.next().await {
            let chunk = chunk.context("reading the download stream")?;
            file.write_all(&chunk).await?;
            done += chunk.len() as u64;

            if last_update.elapsed() >= Duration::from_millis(150) {
                let bps = done as f64 / start.elapsed().as_secs_f64().max(0.001);
                self.progress(label, done, total, bps);
                last_update = Instant::now();
            }
        }

        file.flush().await?;
        let bps = done as f64 / start.elapsed().as_secs_f64().max(0.001);
        self.progress(label, done, total.max(done), bps);
        Ok(done)
    }

    async fn purge(&self, ids: Vec<u32>) -> Result<()> {
        let ac = self.require_ac()?;

        for id in ids {
            let meta = self.api.metadata(id).await?;
            let label = display_name(&meta);

            let target = ac.clone();
            let meta_for_disk = meta.clone();
            let report = tokio::task::spawn_blocking(move || {
                ac::purge::purge(&target, &meta_for_disk)
            })
            .await
            .context("purge task")??;

            self.log(format!(
                "Purged {label} — {} files, {} marker(s), {} freed",
                report.files_removed,
                report.markers_removed,
                human_bytes(report.bytes_freed)
            ));
            self.send(Event::Status(id, Status::Missing));
        }

        Ok(())
    }

    async fn scripts(&self) -> Result<()> {
        let ac = self.require_ac()?;
        let index = self
            .build_index(&ac)
            .await
            .context("indexing the AC install")?;

        match ac::scripts::set_python_enabled(true) {
            Ok(true) => self.log("Python enabled in gameplay.ini"),
            Ok(false) => self.log("Documents/Assetto Corsa not found — skipped gameplay.ini"),
            Err(error) => self.log(format!("gameplay.ini: {error:#}")),
        }
        match ac::scripts::set_real_penalty_enabled(true) {
            Ok(true) => self.log("RealPenalty enabled in python.ini"),
            Ok(false) => self.log("Documents/Assetto Corsa not found — skipped python.ini"),
            Err(error) => self.log(format!("python.ini: {error:#}")),
        }

        if ac::scripts::real_penalty_installed(&ac) {
            self.log("RealPenalty is already installed");
        } else {
            self.log("Installing RealPenalty (content 112)…");
            self.install_one(ac::scripts::REAL_PENALTY_CONTENT_ID, &ac, &index)
                .await?;
        }

        if ac::scripts::livery_script_installed(&ac) {
            self.log("LFM livery script is already installed");
        } else {
            self.log("Installing LFM livery script (content 114)…");
            self.install_one(ac::scripts::LIVERY_SCRIPT_CONTENT_ID, &ac, &index)
                .await?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// session lookups (spawned so they never block a mod sync)
// ---------------------------------------------------------------------------

fn spawn_sessions(api: ApiClient, tx: mpsc::UnboundedSender<Event>, event_id: u32) {
    tokio::spawn(async move {
        if let Err(error) = fetch_sessions(api, tx.clone(), event_id).await {
            let _ = tx.send(Event::Log(format!("sessions: {error:#}")));
        }
    });
}

async fn fetch_sessions(
    api: ApiClient,
    tx: mpsc::UnboundedSender<Event>,
    event_id: u32,
) -> Result<()> {
    let _ = tx.send(Event::Log(format!("Loading sessions for event {event_id}…")));
    let races = api.upcoming_sessions(event_id).await?;
    let races: Vec<RaceInfo> = races.iter().map(map_race).collect();
    let _ = tx.send(Event::Sessions { event_id, races });
    Ok(())
}

fn spawn_join(
    api: ApiClient,
    tx: mpsc::UnboundedSender<Event>,
    race_id: u32,
    label: String,
) {
    tokio::spawn(async move {
        fetch_join(api, tx, race_id, label).await;
    });
}

async fn fetch_join(
    api: ApiClient,
    tx: mpsc::UnboundedSender<Event>,
    race_id: u32,
    label: String,
) {
    // Informational only: the server can hand out join data before `checkData` flips.
    if let Ok(entries) = api.check_data(&[race_id]).await {
        if let Some(entry) = entries.iter().find(|entry| entry.race_id == race_id) {
            if !entry.server_live {
                let _ = tx.send(Event::Log(format!("race {race_id}: server not live yet")));
            }
            if !entry.signed_up {
                let _ = tx.send(Event::Log(format!(
                    "race {race_id}: you are not signed up"
                )));
            }
        }
    }

    match api.join_data(race_id).await {
        Ok(data) => {
            let url = data
                .link
                .as_deref()
                .or(data.url.as_deref())
                .map(session::decode_link_or_raw);
            let password = data.pw.as_deref().map(session::decode_link_or_raw);

            let note = if url.is_none() {
                "The API returned no join link yet.".to_string()
            } else {
                String::new()
            };

            let _ = tx.send(Event::JoinInfo(JoinInfo {
                title: data.servername.unwrap_or(label),
                url,
                password,
                note,
            }));
        }
        Err(error) => {
            let _ = tx.send(Event::Log(format!("race {race_id}: {error:#}")));
            let _ = tx.send(Event::JoinInfo(JoinInfo {
                title: label,
                url: None,
                password: None,
                note: format!(
                    "No join data: the server is not live yet, or you are not signed up. ({error})"
                ),
            }));
        }
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn collect_ids(series: &[Series]) -> Vec<u32> {
    let mut ids: Vec<u32> = series
        .iter()
        .flat_map(|s| s.content.iter())
        .filter(|c| c.in_db && !c.dlc)
        .filter_map(|c| c.content_id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn status_from(integrity: &ac::integrity::Integrity) -> Status {
    if integrity.healthy() {
        Status::Installed
    } else if integrity.needs_update {
        Status::Outdated
    } else if integrity.is_partial() {
        Status::Partial
    } else {
        Status::Missing
    }
}

fn display_name(meta: &ContentMetadata) -> String {
    if meta.name.is_empty() {
        format!("content {}", meta.id)
    } else {
        meta.name.clone()
    }
}

fn build_series(
    reference: &SeriesRef,
    content: EventContent,
    catalogue: &[CatalogueEntry],
) -> Series {
    let mut items = Vec::with_capacity(content.tracks.len() + content.cars.len());
    for raw in &content.tracks {
        items.push(build_item(raw, ContentKind::Track, catalogue));
    }
    for raw in &content.cars {
        items.push(build_item(raw, ContentKind::Car, catalogue));
    }

    Series {
        event_id: reference.event_id,
        name: reference.series_name.clone(),
        style: reference.event_style.clone(),
        active_week: reference.active_week,
        slug: reference.slug.clone().unwrap_or_default(),
        practice_this_week: map_practice(&reference.practice_server),
        practice_next_week: map_practice(&reference.practice_server_next_week),
        content: items,
    }
}

fn map_practice(servers: &[PracticeServerDto]) -> Vec<PracticeLink> {
    servers
        .iter()
        .enumerate()
        .filter_map(|(index, server)| {
            let encoded = server.link.clone()?;
            if encoded.trim().is_empty() {
                return None;
            }
            Some(PracticeLink {
                label: format!("Practice {}", index + 1),
                encoded,
            })
        })
        .collect()
}

fn map_race(dto: &RaceDto) -> RaceInfo {
    RaceInfo {
        race_id: dto.race_id,
        date: dto.race_date.clone(),
        track: dto
            .track
            .as_ref()
            .and_then(|t| t.track_name.clone())
            .unwrap_or_default(),
        slots: dto.slots,
        registered: dto.registered,
        signed_up: dto.signed_up,
        server_live: dto.server_live,
        server_name: dto.server_name.clone().unwrap_or_default(),
        practice_minutes: dto.sessions_length.as_ref().map(|s| s.p).unwrap_or(0),
        quali_minutes: dto.sessions_length.as_ref().map(|s| s.q).unwrap_or(0),
        race_minutes: dto.sessions_length.as_ref().map(|s| s.r).unwrap_or(0),
        signup_opens: dto.signup_start.clone(),
        signup_closes: dto.signup_closes.clone(),
    }
}

fn build_item(raw: &RawEntity, kind: ContentKind, catalogue: &[CatalogueEntry]) -> ContentItem {
    let (lfm_id, lfm_name) = match kind {
        ContentKind::Track => (
            raw.track_id.unwrap_or_default(),
            raw.track_name.clone().unwrap_or_default(),
        ),
        ContentKind::Car => (
            raw.car_id.unwrap_or_default(),
            raw.car_name.clone().unwrap_or_default(),
        ),
    };

    let wanted = match kind {
        ContentKind::Track => "TRACK",
        ContentKind::Car => "CAR",
    };

    let entry = catalogue.iter().find(|entry| {
        entry.lfm_id == Some(lfm_id) && entry.content_type.eq_ignore_ascii_case(wanted)
    });

    ContentItem {
        content_id: entry.map(|e| e.id),
        lfm_id,
        name: if lfm_name.is_empty() {
            entry.map(|e| e.name.clone()).unwrap_or_default()
        } else {
            lfm_name
        },
        kind,
        minio_key: entry.and_then(|e| e.minio_key.clone()),
        dlc: entry.map(|e| e.dlc).unwrap_or(false),
        in_db: entry.is_some(),
        creator: raw.modder_name.clone().unwrap_or_default(),
    }
}
