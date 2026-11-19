//! TUI state and input handling.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use tokio::sync::mpsc::UnboundedSender;

use crate::model::{
    CarOption, ContentItem, JoinInfo, PracticeLink, ProgressView, RaceInfo, Series, Status,
};
use crate::session;
use crate::worker::{Command, Event};

/// Race lists are cheap to fetch, so keep them current while the tab is open.
const SESSIONS_REFRESH: Duration = Duration::from_secs(60);
/// The season payload also triggers a full integrity scan, so refresh it rarely.
const SEASON_REFRESH: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Series,
    Content,
}

/// Top-level tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Required mods for the season (download/verify/purge).
    Mods,
    /// Practice servers and upcoming races.
    Sessions,
}

#[derive(Debug, Clone)]
pub enum Confirm {
    Purge(Vec<u32>),
}

/// Screen rectangles of the last frame, so the mouse can tell what was clicked.
#[derive(Debug, Default, Clone, Copy)]
pub struct Regions {
    pub tab_mods: Rect,
    pub tab_sessions: Rect,
    pub series: Rect,
    pub content: Rect,
    pub sessions: Rect,
}

/// Which list a mouse event landed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Series,
    Content,
    Sessions,
}

/// Sign-up dialog: pick a car for an upcoming race, or withdraw from one already booked.
#[derive(Clone, Debug, Default)]
pub struct SignupPopup {
    pub race_id: u32,
    pub event_id: u32,
    pub label: String,
    pub slots: u32,
    pub registered: u32,
    pub signup_closes: String,
    pub signed_up: bool,
    pub loading: bool,
    pub submitting: bool,
    pub cars: Vec<CarOption>,
    pub selected: usize,
    pub error: Option<String>,
}

/// A row of the sessions pane. Headers and notes are not selectable.
#[derive(Debug, Clone)]
pub enum SessionRow {
    Header(&'static str),
    Practice { label: String, encoded: String },
    Race(usize),
    Note(String),
}

impl SessionRow {
    pub fn selectable(&self) -> bool {
        matches!(
            self,
            SessionRow::Practice { .. } | SessionRow::Race(_)
        )
    }
}

pub struct App {
    pub series: Vec<Series>,
    pub series_state: ListState,
    pub content_state: ListState,
    pub focus: Focus,
    pub view: View,
    pub selected: HashSet<u32>,
    pub statuses: HashMap<u32, Status>,
    /// Expected-files found vs expected, per content id (from the filesystem check).
    pub coverage: HashMap<u32, (usize, usize)>,
    pub logs: VecDeque<String>,
    pub progress: Option<ProgressView>,
    pub busy: bool,
    pub ac_path: Option<PathBuf>,
    pub token_present: bool,
    pub show_help: bool,
    pub confirm: Option<Confirm>,
    /// A browser login is in flight.
    pub login_pending: bool,
    pub should_quit: bool,
    pub spinner: usize,
    pub tx: UnboundedSender<Command>,

    // sessions
    pub races: Vec<RaceInfo>,
    pub races_event: Option<u32>,
    pub races_loading: bool,
    pub races_error: Option<String>,
    pub sessions_state: ListState,
    pub show_next_week: bool,
    pub join_popup: Option<JoinInfo>,
    pub join_loading: bool,
    pub signup_popup: Option<SignupPopup>,
    /// Last race-list request, for the automatic refresh.
    last_sessions_refresh: Option<Instant>,
    /// Last season refresh, for the automatic refresh.
    last_season_refresh: Instant,

    /// Filled in by the renderer on every frame.
    pub regions: Regions,
    /// Last click, for double-click detection.
    last_click: Option<(Instant, Pane, usize)>,
}

impl App {
    pub fn new(
        tx: UnboundedSender<Command>,
        ac_path: Option<PathBuf>,
        token_present: bool,
        selected: HashSet<u32>,
    ) -> Self {
        Self {
            series: Vec::new(),
            series_state: ListState::default(),
            content_state: ListState::default(),
            focus: Focus::Series,
            view: View::Mods,
            selected,
            statuses: HashMap::new(),
            coverage: HashMap::new(),
            logs: VecDeque::new(),
            progress: None,
            busy: false,
            ac_path,
            token_present,
            show_help: false,
            confirm: None,
            login_pending: false,
            should_quit: false,
            spinner: 0,
            tx,
            races: Vec::new(),
            races_event: None,
            races_loading: false,
            races_error: None,
            sessions_state: ListState::default(),
            show_next_week: false,
            join_popup: None,
            join_loading: false,
            signup_popup: None,
            last_sessions_refresh: None,
            last_season_refresh: Instant::now(),
            regions: Regions::default(),
            last_click: None,
        }
    }

    // ----------------------------------------------------------------- state

    pub fn log(&mut self, msg: impl Into<String>) {
        self.logs.push_back(msg.into());
        while self.logs.len() > 1000 {
            self.logs.pop_front();
        }
    }

    pub fn current_series(&self) -> Option<&Series> {
        let idx = self.series_state.selected()?;
        self.series.get(idx)
    }

    pub fn current_content(&self) -> &[ContentItem] {
        self.current_series()
            .map(|s| s.content.as_slice())
            .unwrap_or(&[])
    }

    pub fn status_for(&self, item: &ContentItem) -> Status {
        if !item.in_db {
            return Status::NotInDb;
        }
        if item.dlc {
            return Status::Dlc;
        }
        match item.content_id {
            Some(id) => self.statuses.get(&id).copied().unwrap_or(Status::Unknown),
            None => Status::NotInDb,
        }
    }

    pub fn selected_content(&self) -> Option<&ContentItem> {
        let idx = self.content_state.selected()?;
        self.current_content().get(idx)
    }

    pub fn busy_spinner(&self) -> &'static str {
        const FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
        FRAMES[self.spinner % FRAMES.len()]
    }

    pub fn tick(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
        self.auto_refresh();
    }

    /// Keep the visible data current without a refresh key. Nothing runs while a modal is
    /// open, the app is busy, or no token is present.
    fn auto_refresh(&mut self) {
        if self.busy
            || !self.token_present
            || self.show_help
            || self.confirm.is_some()
            || self.join_popup.is_some()
            || self.signup_popup.is_some()
        {
            return;
        }

        match self.view {
            View::Sessions => {
                let due = self
                    .last_sessions_refresh
                    .is_some_and(|at| at.elapsed() >= SESSIONS_REFRESH);
                if due && !self.races_loading {
                    if let Some(event_id) = self.current_series().map(|s| s.event_id) {
                        self.last_sessions_refresh = Some(Instant::now());
                        let _ = self.tx.send(Command::Sessions {
                            event_id,
                            quiet: true,
                        });
                    }
                }
            }
            View::Mods => {
                if self.last_season_refresh.elapsed() >= SEASON_REFRESH {
                    self.last_season_refresh = Instant::now();
                    self.races_event = None;
                    let _ = self.tx.send(Command::Refresh);
                }
            }
        }
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        let mut installed = 0;
        let mut tracked = 0;
        let mut outdated = 0;
        for series in &self.series {
            for item in &series.content {
                if !item.in_db || item.dlc || item.content_id.is_none() {
                    continue;
                }
                tracked += 1;
                match self.status_for(item) {
                    Status::Installed => installed += 1,
                    Status::Outdated => outdated += 1,
                    _ => {}
                }
            }
        }
        (installed, tracked, outdated)
    }

    // -------------------------------------------------------------- sessions

    pub fn session_rows(&self) -> Vec<SessionRow> {
        let Some(series) = self.current_series() else {
            return Vec::new();
        };

        let mut rows = Vec::new();
        let mut groups: Vec<(&'static str, &[PracticeLink])> = Vec::new();

        if !series.practice_this_week.is_empty() || (self.show_next_week && !series.practice_next_week.is_empty()) {
            groups.push(("Practice · this week", series.practice_this_week.as_slice()));
        }
        if self.show_next_week && !series.practice_next_week.is_empty() {
            groups.push(("Practice · next week", series.practice_next_week.as_slice()));
        }

        for (title, links) in groups {
            if links.is_empty() {
                continue;
            }
            rows.push(SessionRow::Header(title));
            for link in links {
                rows.push(SessionRow::Practice {
                    label: link.label.clone(),
                    encoded: link.encoded.clone(),
                });
            }
        }

        rows.push(SessionRow::Header("Upcoming races"));
        if self.races_loading {
            rows.push(SessionRow::Note("loading…".to_string()));
        } else if let Some(error) = &self.races_error {
            rows.push(SessionRow::Note(error.clone()));
        } else if self.races.is_empty() {
            rows.push(SessionRow::Note("no upcoming races".to_string()));
        } else {
            for index in 0..self.races.len() {
                rows.push(SessionRow::Race(index));
            }
        }

        rows
    }

    pub fn selected_session(&self) -> Option<SessionRow> {
        let rows = self.session_rows();
        let index = self.sessions_state.selected()?;
        rows.get(index).cloned()
    }

    fn select_first_session(&mut self) {
        let rows = self.session_rows();
        match rows.iter().position(|row| row.selectable()) {
            Some(index) => self.sessions_state.select(Some(index)),
            None => self.sessions_state.select(None),
        }
    }

    fn move_session(&mut self, delta: isize) {
        let rows = self.session_rows();
        let selectable: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.selectable())
            .map(|(index, _)| index)
            .collect();

        if selectable.is_empty() {
            return;
        }

        let current = self.sessions_state.selected().unwrap_or(selectable[0]);
        let position = selectable
            .iter()
            .position(|&index| index == current)
            .unwrap_or(0);
        let next = (position as isize + delta).clamp(0, selectable.len() as isize - 1) as usize;
        self.sessions_state.select(Some(selectable[next]));
    }

    /// Load the race list for the selected series if it is not already loaded.
    pub fn ensure_sessions(&mut self) {
        let Some(event_id) = self.current_series().map(|s| s.event_id) else {
            return;
        };
        if self.races_loading && self.races_event == Some(event_id) {
            return;
        }
        if self.races_event == Some(event_id)
            && self.races_error.is_none()
            && !self.races.is_empty()
        {
            return;
        }

        self.races.clear();
        self.races_error = None;
        self.races_loading = true;
        self.races_event = Some(event_id);
        self.sessions_state.select(None);
        self.last_sessions_refresh = Some(Instant::now());
        let _ = self.tx.send(Command::Sessions {
            event_id,
            quiet: false,
        });
    }

    fn reload_sessions(&mut self) {
        self.races_event = None;
        self.ensure_sessions();
    }

    fn activate_session(&mut self) {
        let Some(row) = self.selected_session() else {
            return;
        };

        match row {
            SessionRow::Practice { label, encoded } => match session::decode_base64(&encoded) {
                Some(url) => {
                    self.join_popup = Some(JoinInfo {
                        title: label,
                        url: Some(url),
                        password: None,
                        note: "o starts Assetto Corsa through Steam with this link."
                            .to_string(),
                    });
                }
                None => self.log("Could not decode the practice join link"),
            },
            SessionRow::Race(index) => {
                let Some(race) = self.races.get(index).cloned() else {
                    return;
                };
                if self.busy {
                    self.log("Busy — wait for the running task");
                    return;
                }
                let race_id = race.race_id;
                let label = if race.track.is_empty() {
                    format!("Race {race_id}")
                } else {
                    format!("{} — {}", race.track, race.date)
                };
                self.join_loading = true;
                self.log(format!("Resolving join data for {label}…"));
                self.log(format!(
                    "signup {} → {} · {}/{} slots{}",
                    race.signup_opens,
                    race.signup_closes,
                    race.registered,
                    race.slots,
                    if race.signed_up { " · signed up" } else { "" }
                ));
                let _ = self.tx.send(Command::JoinRace { race_id, label });
            }
            _ => {}
        }
    }

    // -------------------------------------------------------------- signup

    /// Open the sign-up dialog for the highlighted race, loading its car list.
    /// Sign out of the highlighted race directly, without opening the picker.
    fn sign_out(&mut self) {
        let Some(SessionRow::Race(index)) = self.selected_session() else {
            self.log("Select an upcoming race first");
            return;
        };
        let Some(race) = self.races.get(index).cloned() else {
            return;
        };
        if !race.signed_up {
            self.log("You are not signed up for this race");
            return;
        }
        let Some(event_id) = self.current_series().map(|s| s.event_id) else {
            return;
        };
        let label = if race.track.is_empty() {
            format!("race {}", race.race_id)
        } else {
            format!("{} — {}", race.track, race.date)
        };

        self.log(format!("Signing out of {label}…"));
        let _ = self.tx.send(Command::Withdraw {
            race_id: race.race_id,
            event_id,
            label,
        });
    }

    fn open_signup(&mut self) {
        let Some(SessionRow::Race(index)) = self.selected_session() else {
            self.log("Select an upcoming race first");
            return;
        };
        let Some(race) = self.races.get(index).cloned() else {
            return;
        };
        let Some(event_id) = self.current_series().map(|s| s.event_id) else {
            return;
        };

        let label = if race.track.is_empty() {
            format!("race {}", race.race_id)
        } else {
            format!("{} — {}", race.track, race.date)
        };

        self.signup_popup = Some(SignupPopup {
            race_id: race.race_id,
            event_id,
            label: label.clone(),
            slots: race.slots,
            registered: race.registered,
            signup_closes: race.signup_closes.clone(),
            signed_up: race.signed_up,
            loading: !race.signed_up,
            ..SignupPopup::default()
        });

        if race.signed_up {
            self.log(format!("{label}: you are already registered — u withdraws"));
        } else {
            self.log(format!("Loading the cars available for {label}…"));
            let _ = self.tx.send(Command::RaceCars {
                race_id: race.race_id,
                event_id,
            });
        }
    }

    fn handle_signup_popup_key(&mut self, key: KeyEvent) {
        // Take the popup so the handlers can borrow `self` freely; putting it back keeps it
        // open, leaving it dropped closes the dialog.
        let Some(mut popup) = self.signup_popup.take() else {
            return;
        };

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return,

            KeyCode::Down | KeyCode::Char('j') => {
                if !popup.cars.is_empty() {
                    popup.selected = (popup.selected + 1).min(popup.cars.len() - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                popup.selected = popup.selected.saturating_sub(1);
            }
            KeyCode::Home | KeyCode::Char('g') => popup.selected = 0,
            KeyCode::End | KeyCode::Char('G') => {
                popup.selected = popup.cars.len().saturating_sub(1);
            }

            KeyCode::Enter => {
                if popup.submitting || popup.loading {
                    // A request is already in flight.
                } else if popup.signed_up {
                    self.log("You are already registered — press u to withdraw");
                } else if let Some(car) = popup.cars.get(popup.selected) {
                    popup.submitting = true;
                    popup.error = None;
                    self.log(format!(
                        "Registering for {} with {}…",
                        popup.label, car.name
                    ));
                    let _ = self.tx.send(Command::SignUp {
                        race_id: popup.race_id,
                        event_id: popup.event_id,
                        car_id: car.id,
                        label: popup.label.clone(),
                    });
                } else {
                    self.log("No car selected — no cars were offered for this race");
                }
            }

            KeyCode::Char('u') | KeyCode::Char('U') => {
                if popup.signed_up && !popup.submitting {
                    popup.submitting = true;
                    popup.error = None;
                    self.log(format!("Withdrawing from {}…", popup.label));
                    let _ = self.tx.send(Command::Withdraw {
                        race_id: popup.race_id,
                        event_id: popup.event_id,
                        label: popup.label.clone(),
                    });
                } else if !popup.signed_up {
                    self.log("You are not registered for this race");
                }
            }

            KeyCode::Char('r') if !popup.signed_up && !popup.submitting => {
                popup.loading = true;
                popup.error = None;
                popup.cars.clear();
                popup.selected = 0;
                self.log(format!("Reloading cars for {}…", popup.label));
                let _ = self.tx.send(Command::RaceCars {
                    race_id: popup.race_id,
                    event_id: popup.event_id,
                });
            }

            _ => {}
        }

        self.signup_popup = Some(popup);
    }

    fn open_series_page(&mut self) {
        let Some(slug) = self.current_series().map(|s| s.slug.clone()) else {
            return;
        };
        if slug.is_empty() {
            self.log("This series has no website slug");
            return;
        }
        let url = format!("https://lowfuelmotorsport.com/series/{slug}");
        if session::open_url(&url) {
            self.log(format!("Opening {url}"));
        } else {
            self.log(format!("Could not open a browser. URL: {url}"));
        }
    }

    // --------------------------------------------------------------- actions

    pub fn sync_targets(&self) -> Vec<u32> {
        let known: HashSet<u32> = self
            .series
            .iter()
            .flat_map(|s| s.content.iter())
            .filter(|c| c.in_db && !c.dlc)
            .filter_map(|c| c.content_id)
            .collect();

        let mut ids: Vec<u32> = self
            .selected
            .iter()
            .copied()
            .filter(|id| known.contains(id))
            .collect();

        if ids.is_empty() {
            if let Some(series) = self.current_series() {
                ids = series
                    .content
                    .iter()
                    .filter(|c| c.in_db && !c.dlc)
                    .filter(|c| self.status_for(c).needs_download())
                    .filter_map(|c| c.content_id)
                    .collect();
            }
        }

        ids.sort_unstable();
        ids.dedup();
        self.dedupe_minio(ids)
    }

    /// Several LFM entries can resolve to the same physical mod; download it once.
    fn dedupe_minio(&self, ids: Vec<u32>) -> Vec<u32> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut out: Vec<u32> = Vec::new();

        for series in &self.series {
            for item in &series.content {
                let Some(id) = item.content_id else {
                    continue;
                };
                if !ids.contains(&id) || out.contains(&id) {
                    continue;
                }
                if let Some(key) = item.minio_key.as_deref() {
                    if !seen.insert(key) {
                        continue;
                    }
                }
                out.push(id);
            }
        }

        for id in ids {
            if !out.contains(&id) {
                out.push(id);
            }
        }

        out.sort_unstable();
        out
    }

    pub fn purge_targets(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self
            .series
            .iter()
            .flat_map(|s| s.content.iter())
            .filter_map(|c| c.content_id)
            .filter(|id| !self.selected.contains(id))
            .filter(|id| {
                self.statuses
                    .get(id)
                    .copied()
                    .unwrap_or(Status::Unknown)
                    .is_installed()
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    fn all_tracked_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self
            .series
            .iter()
            .flat_map(|s| s.content.iter())
            .filter(|c| c.in_db && !c.dlc)
            .filter_map(|c| c.content_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    fn toggle_selection(&mut self) {
        let Some(item) = self.selected_content().cloned() else {
            return;
        };
        let Some(id) = item.content_id else {
            self.log(format!("{} is not in the content catalogue", item.name));
            return;
        };
        if item.dlc {
            self.log(format!("{} is official DLC — nothing to sync", item.name));
            return;
        }
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
    }

    fn select_series_all(&mut self) {
        let Some(series) = self.current_series() else {
            return;
        };
        let ids: Vec<u32> = series
            .content
            .iter()
            .filter(|c| c.in_db && !c.dlc)
            .filter_map(|c| c.content_id)
            .collect();
        let count = ids.len();
        self.selected.extend(ids);
        self.log(format!("Selected {count} item(s) in this series"));
    }

    fn start_login(&mut self) {
        if self.login_pending {
            self.log("Login already waiting for the browser…");
            return;
        }
        self.login_pending = true;
        self.log("Opening your browser to log in to LFM…");
        let _ = self.tx.send(Command::Login);
    }

    fn start_sync(&mut self) {
        if self.busy {
            self.log("Busy — wait for the running task");
            return;
        }
        if self.ac_path.is_none() {
            self.log("No Assetto Corsa install found — pass --ac-path");
            return;
        }
        let ids = self.sync_targets();
        if ids.is_empty() {
            self.log("Nothing to sync (everything selected is installed)");
            return;
        }
        self.log(format!("Syncing {} content item(s)…", ids.len()));
        let _ = self.tx.send(Command::Sync(ids));
    }

    fn start_verify(&mut self) {
        if self.busy {
            self.log("Busy — wait for the running task");
            return;
        }
        if self.ac_path.is_none() {
            self.log("No Assetto Corsa install found — pass --ac-path");
            return;
        }
        let ids = self.all_tracked_ids();
        if ids.is_empty() {
            self.log("Nothing to verify — press r to load the season first");
            return;
        }
        self.log(format!("Verifying {} content item(s)…", ids.len()));
        let _ = self.tx.send(Command::Verify(ids));
    }

    fn ask_purge(&mut self) {
        if self.busy {
            self.log("Busy — wait for the running task");
            return;
        }
        if self.ac_path.is_none() {
            self.log("No Assetto Corsa install found — pass --ac-path");
            return;
        }
        let ids = self.purge_targets();
        if ids.is_empty() {
            self.log("Nothing to purge (no installed content outside the selection)");
            return;
        }
        self.confirm = Some(Confirm::Purge(ids));
    }

    fn run_scripts(&mut self) {
        if self.busy {
            self.log("Busy — wait for the running task");
            return;
        }
        if self.ac_path.is_none() {
            self.log("No Assetto Corsa install found — pass --ac-path");
            return;
        }
        let _ = self.tx.send(Command::Scripts);
    }

    // --------------------------------------------------------------- events

    pub fn on_event(&mut self, event: Event) {
        match event {
            Event::RaceCars {
                race_id,
                cars,
                error,
            } => {
                if let Some(popup) = self
                    .signup_popup
                    .as_mut()
                    .filter(|popup| popup.race_id == race_id)
                {
                    popup.loading = false;
                    popup.cars = cars;
                    popup.selected = 0;
                    popup.error = error;
                }
            }
            Event::SignupResult {
                race_id,
                signed_up,
                ok,
                message,
            } => {
                if !ok {
                    if let Some(popup) = self
                        .signup_popup
                        .as_mut()
                        .filter(|popup| popup.race_id == race_id)
                    {
                        popup.submitting = false;
                        popup.error = Some(message);
                    }
                    return;
                }

                if let Some(race) = self.races.iter_mut().find(|race| race.race_id == race_id) {
                    race.signed_up = signed_up;
                }
                self.signup_popup = None;
                self.reload_sessions();
            }
            Event::Log(msg) => self.log(msg),
            Event::Season(series) => {
                self.series = series;
                if self.series.is_empty() {
                    self.series_state.select(None);
                } else if self
                    .series_state
                    .selected()
                    .map(|i| i >= self.series.len())
                    .unwrap_or(true)
                {
                    self.series_state.select(Some(0));
                }
                self.content_state
                    .select(if self.current_content().is_empty() { None } else { Some(0) });

                if self.view == View::Sessions {
                    self.select_first_session();
                    self.ensure_sessions();
                }
            }
            Event::Status(id, status) => {
                self.statuses.insert(id, status);
                if status != Status::Checking {
                    self.coverage.remove(&id);
                }
            }
            Event::Checked {
                id,
                status,
                present,
                total,
            } => {
                self.statuses.insert(id, status);
                self.coverage.insert(id, (present, total));
            }
            Event::Progress(progress) => self.progress = progress,
            Event::Busy(busy) => {
                self.busy = busy;
                if busy {
                    self.log("Working…");
                } else {
                    self.login_pending = false;
                }
            }
            Event::LoggedIn => {
                self.token_present = true;
                self.login_pending = false;
            }
            Event::Sessions { event_id, races } => {
                if self.races_event == Some(event_id) {
                    // The list is also replaced by the one-minute background refresh, so keep
                    // the highlighted race instead of jumping back to the first row.
                    let previous = match self.selected_session() {
                        Some(SessionRow::Race(index)) => {
                            self.races.get(index).map(|race| race.race_id)
                        }
                        _ => None,
                    };

                    self.races = races;
                    self.races_loading = false;
                    self.races_error = None;
                    self.last_sessions_refresh = Some(Instant::now());

                    let rows = self.session_rows();
                    let same_race = previous.and_then(|race_id| {
                        rows.iter().position(|row| {
                            matches!(row, SessionRow::Race(index)
                                if self.races.get(*index)
                                    .is_some_and(|race| race.race_id == race_id))
                        })
                    });

                    match same_race {
                        Some(index) => self.sessions_state.select(Some(index)),
                        None => self.select_first_session(),
                    }
                }
            }
            Event::JoinInfo(info) => {
                self.join_loading = false;
                self.join_popup = Some(info);
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if self.signup_popup.is_some() {
            self.handle_signup_popup_key(key);
            return;
        }

        if let Some(popup) = self.join_popup.clone() {
            self.handle_join_popup_key(key, popup);
            return;
        }

        if let Some(confirm) = self.confirm.clone() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let Confirm::Purge(ids) = confirm;
                    self.log(format!("Purging {} content item(s)…", ids.len()));
                    let _ = self.tx.send(Command::Purge(ids));
                    self.confirm = None;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirm = None;
                    self.log("Purge cancelled");
                }
                _ => {}
            }
            return;
        }

        if self.show_help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
                    self.show_help = false
                }
                _ => {}
            }
            return;
        }

        if matches!(key.code, KeyCode::Char('1')) {
            self.view = View::Mods;
            self.focus = Focus::Series;
            return;
        }
        if matches!(key.code, KeyCode::Char('2')) {
            self.view = View::Sessions;
            self.focus = Focus::Series;
            self.select_first_session();
            self.ensure_sessions();
            return;
        }

        match self.view {
            View::Mods => self.on_key_mods(key),
            View::Sessions => self.on_key_sessions(key),
        }
    }

    fn handle_join_popup_key(&mut self, key: KeyEvent, popup: JoinInfo) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.join_popup = None,
            KeyCode::Char('o') => match &popup.url {
                Some(url) => {
                    if session::open_join_link(url) {
                        self.log("Asking Steam to start Assetto Corsa with the join link");
                    } else {
                        self.log("Could not reach Steam — press c to copy the link");
                    }
                }
                None => self.log("This race has no join link yet"),
            },
            KeyCode::Char('c') => match &popup.url {
                Some(url) => {
                    if session::copy_to_clipboard(url) {
                        self.log("Join link copied to the clipboard");
                    } else {
                        self.log(format!("Clipboard unavailable — link: {url}"));
                    }
                }
                None => self.log("Nothing to copy"),
            },
            KeyCode::Char('y') => match &popup.password {
                Some(password) => {
                    if session::copy_to_clipboard(password) {
                        self.log("Server password copied to the clipboard");
                    } else {
                        self.log(format!("Clipboard unavailable — password: {password}"));
                    }
                }
                None => self.log("This race has no separate password"),
            },
            _ => {}
        }
    }

    // ------------------------------------------------------------------ mouse

    pub fn on_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                    -3
                } else {
                    3
                };
                match self.pane_at(mouse.column, mouse.row) {
                    Some(Pane::Series) => {
                        self.focus = Focus::Series;
                        self.move_selection(delta);
                    }
                    Some(Pane::Content) => {
                        self.focus = Focus::Content;
                        self.move_selection(delta);
                    }
                    Some(Pane::Sessions) => {
                        self.focus = Focus::Content;
                        self.move_session(delta);
                    }
                    None => {}
                }
            }
            MouseEventKind::Down(MouseButton::Left) => self.on_click(mouse.column, mouse.row),
            _ => {}
        }
    }

    fn on_click(&mut self, col: u16, row: u16) {
        // Any click dismisses the help overlay first, like a modal dialog.
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.signup_popup.is_some() {
            self.signup_popup = None;
            return;
        }
        if self.join_popup.is_some() {
            self.join_popup = None;
            return;
        }
        // The purge confirmation only changes state from the keyboard, so a stray
        // click can never approve deleting content.
        if self.confirm.is_some() {
            return;
        }

        if contains(self.regions.tab_mods, col, row) {
            self.view = View::Mods;
            self.focus = Focus::Series;
            return;
        }
        if contains(self.regions.tab_sessions, col, row) {
            self.view = View::Sessions;
            self.focus = Focus::Series;
            self.select_first_session();
            self.ensure_sessions();
            return;
        }

        let Some(pane) = self.pane_at(col, row) else {
            return;
        };

        let (rect, offset, len) = match pane {
            Pane::Series => (self.regions.series, self.series_state.offset(), self.series.len()),
            Pane::Content => (
                self.regions.content,
                self.content_state.offset(),
                self.current_content().len(),
            ),
            Pane::Sessions => (
                self.regions.sessions,
                self.sessions_state.offset(),
                self.session_rows().len(),
            ),
        };

        let Some(index) = row_index(rect, offset, row, len) else {
            return;
        };

        let double_click = self.last_click.is_some_and(|(at, last_pane, last_index)| {
            last_pane == pane && last_index == index && at.elapsed() < Duration::from_millis(450)
        });
        self.last_click = Some((Instant::now(), pane, index));

        match pane {
            Pane::Series => {
                self.focus = Focus::Series;
                self.select_series_index(index);
            }
            Pane::Content => {
                self.focus = Focus::Content;
                self.content_state.select(Some(index));
                if double_click {
                    self.toggle_selection();
                }
            }
            Pane::Sessions => {
                self.focus = Focus::Content;
                self.sessions_state.select(Some(index));
                if double_click {
                    self.activate_session();
                }
            }
        }
    }

    fn pane_at(&self, col: u16, row: u16) -> Option<Pane> {
        if contains(self.regions.series, col, row) {
            return Some(Pane::Series);
        }
        if self.view == View::Mods && contains(self.regions.content, col, row) {
            return Some(Pane::Content);
        }
        if self.view == View::Sessions && contains(self.regions.sessions, col, row) {
            return Some(Pane::Sessions);
        }
        None
    }

    /// Select a series by index, resetting the dependent panes.
    fn select_series_index(&mut self, index: usize) {
        if index >= self.series.len() {
            return;
        }
        self.series_state.select(Some(index));

        let content_len = self.current_content().len();
        self.content_state
            .select(if content_len == 0 { None } else { Some(0) });

        if self.view == View::Sessions {
            self.ensure_sessions();
            self.select_first_session();
        }
    }

    fn on_key_mods(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('?') | KeyCode::F(1) => self.show_help = true,

            KeyCode::Tab => self.focus = toggle_focus(self.focus),
            KeyCode::Char('h') | KeyCode::Left => self.focus = Focus::Series,
            KeyCode::Char('l') | KeyCode::Right => self.focus = Focus::Content,

            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::Home | KeyCode::Char('g') => self.jump(0),
            KeyCode::End | KeyCode::Char('G') => self.jump(usize::MAX),

            KeyCode::Char(' ') => self.toggle_selection(),
            KeyCode::Char('a') => self.select_series_all(),
            KeyCode::Char('c') => {
                self.selected.clear();
                self.log("Selection cleared");
            }

            KeyCode::Char('s') => self.start_sync(),
            KeyCode::Char('v') => self.start_verify(),
            KeyCode::Char('d') => self.ask_purge(),
            KeyCode::Char('x') => self.run_scripts(),
            KeyCode::Char('L') => self.start_login(),

            _ => {}
        }
    }

    fn on_key_sessions(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('?') | KeyCode::F(1) => self.show_help = true,

            KeyCode::Tab => self.focus = toggle_focus(self.focus),
            KeyCode::Char('h') | KeyCode::Left => self.focus = Focus::Series,
            KeyCode::Char('l') | KeyCode::Right => self.focus = Focus::Content,

            KeyCode::Down | KeyCode::Char('j') => {
                if self.focus == Focus::Series {
                    self.move_selection(1)
                } else {
                    self.move_session(1)
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.focus == Focus::Series {
                    self.move_selection(-1)
                } else {
                    self.move_session(-1)
                }
            }
            KeyCode::PageDown => self.move_session(5),
            KeyCode::PageUp => self.move_session(-5),
            KeyCode::Home | KeyCode::Char('g') => self.select_first_session(),
            KeyCode::End | KeyCode::Char('G') => self.move_session(isize::MAX / 2),

            KeyCode::Enter | KeyCode::Char(' ') => self.activate_session(),
            KeyCode::Char('w') => {
                self.show_next_week = !self.show_next_week;
                self.select_first_session();
            }
            KeyCode::Char('s') => self.open_signup(),
            KeyCode::Char('u') => self.sign_out(),
            KeyCode::Char('o') => self.open_series_page(),
            KeyCode::Char('L') => self.start_login(),

            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.focus {
            Focus::Series => {
                let len = self.series.len();
                if len == 0 {
                    return;
                }
                let current = self.series_state.selected().unwrap_or(0) as isize;
                let next = (current + delta).clamp(0, len as isize - 1) as usize;
                self.select_series_index(next);
            }
            Focus::Content => {
                let len = self.current_content().len();
                if len == 0 {
                    return;
                }
                let current = self.content_state.selected().unwrap_or(0) as isize;
                let next = (current + delta).clamp(0, len as isize - 1) as usize;
                self.content_state.select(Some(next));
            }
        }
    }

    fn jump(&mut self, index: usize) {
        match self.focus {
            Focus::Series => {
                if self.series.is_empty() {
                    return;
                }
                self.select_series_index(index.min(self.series.len() - 1));
            }
            Focus::Content => {
                let len = self.current_content().len();
                if len == 0 {
                    return;
                }
                let idx = index.min(len - 1);
                self.content_state.select(Some(idx));
            }
        }
    }
}

fn toggle_focus(focus: Focus) -> Focus {
    match focus {
        Focus::Series => Focus::Content,
        Focus::Content => Focus::Series,
    }
}

fn contains(rect: Rect, col: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && col >= rect.x
        && col < rect.x + rect.width
        && row >= rect.y
        && row < rect.y + rect.height
}

/// Map a screen row inside a list to an item index, taking the rendered scroll
/// offset into account (the widget may have scrolled to keep the selection visible).
fn row_index(rect: Rect, offset: usize, row: u16, len: usize) -> Option<usize> {
    if rect.height == 0 || row < rect.y || row >= rect.y + rect.height {
        return None;
    }

    let index = offset + (row - rect.y) as usize;
    (index < len).then_some(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signup_app() -> (App, tokio::sync::mpsc::UnboundedReceiver<Command>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(tx, None, true, HashSet::new());
        app.signup_popup = Some(SignupPopup {
            race_id: 7,
            event_id: 42,
            label: "Monza — 2026-11-20".to_string(),
            slots: 40,
            registered: 12,
            cars: vec![
                CarOption { id: 11, name: "GT3 Car".to_string(), class: "GT3".to_string() },
                CarOption { id: 12, name: "Cup Car".to_string(), class: "CUP".to_string() },
            ],
            ..SignupPopup::default()
        });
        (app, rx)
    }

    #[test]
    fn enter_sends_the_signup_for_the_highlighted_car() {
        let (mut app, mut rx) = signup_app();

        app.handle_signup_popup_key(KeyEvent::from(KeyCode::Down));
        app.handle_signup_popup_key(KeyEvent::from(KeyCode::Enter));

        match rx.try_recv() {
            Ok(Command::SignUp {
                race_id,
                event_id,
                car_id,
                ..
            }) => {
                assert_eq!((race_id, event_id, car_id), (7, 42, 12));
            }
            other => panic!("expected a signup command, got {other:?}"),
        }
        assert!(app.signup_popup.as_ref().unwrap().submitting);
    }

    #[test]
    fn withdraw_only_when_registered() {
        let (mut app, mut rx) = signup_app();

        app.handle_signup_popup_key(KeyEvent::from(KeyCode::Char('u')));
        assert!(rx.try_recv().is_err());

        app.signup_popup.as_mut().unwrap().signed_up = true;
        app.handle_signup_popup_key(KeyEvent::from(KeyCode::Char('u')));
        assert!(matches!(
            rx.try_recv(),
            Ok(Command::Withdraw { race_id: 7, .. })
        ));
    }

    #[test]
    fn failed_signup_keeps_the_popup_open_with_the_error() {
        let (mut app, _rx) = signup_app();

        app.on_event(Event::SignupResult {
            race_id: 7,
            signed_up: true,
            ok: false,
            message: "signup closed".to_string(),
        });

        let popup = app.signup_popup.expect("popup stays open");
        assert_eq!(popup.error.as_deref(), Some("signup closed"));
        assert!(!popup.submitting);
    }

    fn sessions_app(signed_up: bool) -> (App, tokio::sync::mpsc::UnboundedReceiver<Command>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(tx, None, true, HashSet::new());
        app.series = vec![Series {
            event_id: 42,
            name: "GT3".to_string(),
            style: String::new(),
            active_week: 1,
            slug: String::new(),
            practice_this_week: Vec::new(),
            practice_next_week: Vec::new(),
            content: Vec::new(),
        }];
        app.series_state.select(Some(0));
        app.races = vec![RaceInfo {
            race_id: 9,
            track: "Monza".to_string(),
            date: "2026-11-20 18:00:00".to_string(),
            signed_up,
            ..RaceInfo::default()
        }];
        // Session rows: header, then the race.
        app.sessions_state.select(Some(1));
        (app, rx)
    }

    #[test]
    fn signs_out_of_the_highlighted_race() {
        let (mut app, mut rx) = sessions_app(true);
        app.sign_out();

        assert!(matches!(
            rx.try_recv(),
            Ok(Command::Withdraw {
                race_id: 9,
                event_id: 42,
                ..
            })
        ));
    }

    #[test]
    fn sign_out_does_nothing_when_not_registered() {
        let (mut app, mut rx) = sessions_app(false);
        app.sign_out();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn escape_closes_the_signup_popup() {
        let (mut app, _rx) = signup_app();
        app.handle_signup_popup_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.signup_popup.is_none());
    }

    #[test]
    fn maps_click_rows_to_indices() {
        let rect = Rect::new(10, 5, 20, 6);
        assert_eq!(row_index(rect, 0, 5, 10), Some(0));
        assert_eq!(row_index(rect, 0, 10, 10), Some(5));
        assert_eq!(row_index(rect, 4, 7, 10), Some(6));
        assert_eq!(row_index(rect, 0, 11, 10), None);
        assert_eq!(row_index(rect, 0, 4, 10), None);
        assert_eq!(row_index(rect, 0, 5, 0), None);
    }

    #[test]
    fn ignores_clicks_outside_a_region() {
        let rect = Rect::new(10, 5, 20, 6);
        assert!(contains(rect, 10, 5));
        assert!(contains(rect, 29, 10));
        assert!(!contains(rect, 30, 10));
        assert!(!contains(rect, 11, 11));
        assert!(!contains(Rect::default(), 0, 0));
    }
}
