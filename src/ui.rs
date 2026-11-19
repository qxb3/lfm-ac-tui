//! Ratatui rendering.

use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Confirm, Focus, SessionRow, SignupPopup, View};
use crate::model::{human_bytes, ContentItem, JoinInfo, RaceInfo, Status};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(3),
        Constraint::Length(8),
        Constraint::Length(3),
    ])
    .split(area);

    draw_header(frame, app, rows[0]);
    match app.view {
        View::Mods => draw_body(frame, app, rows[1]),
        View::Sessions => draw_sessions_body(frame, app, rows[1]),
    }
    draw_transfer(frame, app, rows[2]);
    draw_log(frame, app, rows[3]);
    draw_footer(frame, app, rows[4]);

    if app.show_help {
        draw_help(frame, area);
    }
    if let Some(info) = app.join_popup.clone() {
        draw_join_popup(frame, area, &info);
    }
    if let Some(popup) = app.signup_popup.clone() {
        draw_signup_popup(frame, area, &popup);
    }
    if let Some(Confirm::Purge(ids)) = app.confirm.clone() {
        draw_confirm(frame, area, ids.len());
    }
}

fn dark_block(title: &str, focused: bool) -> Block<'_> {
    Block::bordered()
        .title(format!(" {title} "))
        .border_style(Style::default().fg(if focused { Color::Cyan } else { Color::DarkGray }))
}

fn push_span(spans: &mut Vec<Span<'static>>, x: &mut u16, span: Span<'static>) {
    *x += span.content.chars().count() as u16;
    spans.push(span);
}

fn draw_header(frame: &mut Frame, app: &mut App, area: Rect) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    // Track the column so the tabs can be hit-tested by the mouse.
    let mut x = area.x + 1;

    push_span(
        &mut spans,
        &mut x,
        Span::styled(
            "LFM",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
    );
    push_span(
        &mut spans,
        &mut x,
        Span::styled(" · Assetto Corsa", Style::default().fg(Color::Gray)),
    );
    push_span(&mut spans, &mut x, Span::raw("   "));

    for (view, label) in [(View::Mods, "1 Mods"), (View::Sessions, "2 Sessions")] {
        let selected = app.view == view;
        let text = format!(" {label} ");
        let width = text.chars().count() as u16;
        let start = x;

        push_span(
            &mut spans,
            &mut x,
            Span::styled(
                text,
                if selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
        );

        let rect = Rect::new(start, area.y + 1, width, 1);
        match view {
            View::Mods => app.regions.tab_mods = rect,
            View::Sessions => app.regions.tab_sessions = rect,
        }

        push_span(&mut spans, &mut x, Span::raw(" "));
    }

    spans.push(Span::raw("  "));
    match &app.ac_path {
        Some(path) => spans.push(Span::styled(
            format!("AC: {}", path.display()),
            Style::default().fg(Color::Green),
        )),
        None => spans.push(Span::styled(
            "AC: not found (--ac-path)",
            Style::default().fg(Color::Red),
        )),
    }

    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        if app.token_present { "token: ok" } else { "token: missing" },
        Style::default().fg(if app.token_present { Color::Green } else { Color::Red }),
    ));

    let (installed, tracked, outdated) = app.stats();
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        format!("{installed}/{tracked} installed"),
        Style::default().fg(Color::Gray),
    ));
    if outdated > 0 {
        spans.push(Span::styled(
            format!("  {outdated} outdated"),
            Style::default().fg(Color::Yellow),
        ));
    }
    if app.busy {
        spans.push(Span::styled(
            format!("  {} working", app.busy_spinner()),
            Style::default().fg(Color::Yellow),
        ));
    }

    let block = Block::bordered().border_style(Style::default().fg(Color::DarkGray));
    frame.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
}

fn draw_body(frame: &mut Frame, app: &mut App, area: Rect) {
    let columns = Layout::horizontal([Constraint::Percentage(33), Constraint::Percentage(67)])
        .split(area);
    draw_series(frame, app, columns[0]);
    draw_content(frame, app, columns[1]);
}

fn draw_sessions_body(frame: &mut Frame, app: &mut App, area: Rect) {
    let columns = Layout::horizontal([Constraint::Percentage(33), Constraint::Percentage(67)])
        .split(area);
    draw_series(frame, app, columns[0]);
    draw_sessions(frame, app, columns[1]);
}

fn draw_series(frame: &mut Frame, app: &mut App, area: Rect) {
    app.regions.series = area.inner(Margin::new(1, 1));

    let items: Vec<ListItem> = app
        .series
        .iter()
        .map(|series| {
            let installed = series.installed_count(&app.statuses);
            let total = series.syncable_count();
            let counters = Span::styled(
                format!("{installed:>2}/{total:<2} "),
                Style::default().fg(if total > 0 && installed == total {
                    Color::Green
                } else {
                    Color::Gray
                }),
            );
            let name = if series.name.is_empty() {
                format!("event {}", series.event_id)
            } else {
                series.name.clone()
            };
            ListItem::new(Line::from(vec![
                counters,
                Span::styled(name, Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!("  w{}", series.active_week),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(dark_block("Series", app.focus == Focus::Series))
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(24, 40, 48))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, area, &mut app.series_state);
}

fn draw_sessions(frame: &mut Frame, app: &mut App, area: Rect) {
    app.regions.sessions = area.inner(Margin::new(1, 1));

    let title = app
        .current_series()
        .map(|s| {
            let base = if s.name.is_empty() {
                format!("event {}", s.event_id)
            } else {
                s.name.clone()
            };
            if s.style.is_empty() {
                base
            } else {
                format!("{base} · {}", s.style)
            }
        })
        .unwrap_or_else(|| "Sessions".to_string());

    let rows = app.session_rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            SessionRow::Header(text) => ListItem::new(Line::from(Span::styled(
                format!(" {text}"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))),
            SessionRow::Note(text) => ListItem::new(Line::from(Span::styled(
                format!("   {text}"),
                Style::default().fg(Color::DarkGray),
            ))),
            SessionRow::Practice { label, .. } => ListItem::new(Line::from(vec![
                Span::styled("  join  ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    label.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "   acmanager:// link",
                    Style::default().fg(Color::DarkGray),
                ),
            ])),
            SessionRow::Race(index) => match app.races.get(*index) {
                Some(race) => ListItem::new(race_line(race)),
                None => ListItem::new(Line::from("")),
            },
        })
        .collect();

    let list = List::new(items)
        .block(dark_block(&title, app.focus == Focus::Content))
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(24, 40, 48))
                .add_modifier(Modifier::BOLD),
        );

    frame.render_stateful_widget(list, area, &mut app.sessions_state);
}

fn race_line(race: &RaceInfo) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!("  {:<12}", short_date(&race.date)),
            Style::default().fg(Color::Gray),
        ),
        Span::styled(
            format!("{:<32}", clip(&race.track, 32)),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:>3}/{:<3}", race.registered, race.slots),
            Style::default().fg(Color::Gray),
        ),
    ];

    if race.signed_up {
        spans.push(Span::styled(
            " you",
            Style::default().fg(Color::Green),
        ));
    }
    if race.server_live {
        spans.push(Span::styled(
            " LIVE",
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ));
        if !race.server_name.is_empty() {
            spans.push(Span::styled(
                format!(" {}", clip(&race.server_name, 18)),
                Style::default().fg(Color::Gray),
            ));
        }
    }
    if race.race_minutes > 0 {
        spans.push(Span::styled(
            format!(
                "  p{} q{} r{}",
                race.practice_minutes, race.quali_minutes, race.race_minutes
            ),
            Style::default().fg(Color::DarkGray),
        ));
    }

    Line::from(spans)
}

/// "2026-11-20 18:00:00" -> "11-20 18:00"
fn short_date(date: &str) -> String {
    let chars: Vec<char> = date.chars().collect();
    if chars.len() >= 16 {
        chars[5..16].iter().collect()
    } else {
        date.to_string()
    }
}

fn clip(value: &str, max: usize) -> String {
    let mut out: String = value.chars().take(max).collect();
    if value.chars().count() > max {
        out.pop();
        out.push('…');
    }
    out
}

fn status_color(status: Status) -> Color {
    match status {
        Status::Installed => Color::Green,
        Status::Outdated => Color::Yellow,
        Status::Partial => Color::Rgb(210, 150, 60),
        Status::Missing => Color::Cyan,
        Status::Checking => Color::Blue,
        Status::NotInDb => Color::Magenta,
        Status::Dlc => Color::DarkGray,
        Status::Unknown => Color::DarkGray,
    }
}

fn content_line(
    item: &ContentItem,
    status: Status,
    selected: bool,
    coverage: Option<(usize, usize)>,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!("{:<3} ", status.tag()),
            Style::default().fg(status_color(status)),
        ),
        Span::styled(
            if selected { "✔ " } else { "  " },
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            if item.name.is_empty() {
                format!("lfm #{}", item.lfm_id)
            } else {
                item.name.clone()
            },
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {}", item.kind.label()),
            Style::default().fg(Color::DarkGray),
        ),
    ];

    if !item.creator.is_empty() {
        spans.push(Span::styled(
            format!("  by {}", item.creator),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if item.dlc {
        spans.push(Span::styled("  DLC", Style::default().fg(Color::DarkGray)));
    }
    if !item.in_db {
        spans.push(Span::styled(
            "  not in catalogue",
            Style::default().fg(Color::Magenta),
        ));
    }
    if let Some((present, total)) = coverage {
        // `0/N` on absent content is noise; the percentage is only useful when something
        // is actually there.
        if total > 0 && present > 0 && present < total {
            spans.push(Span::styled(
                format!("  {present}/{total}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    if let Some(id) = item.content_id {
        spans.push(Span::styled(
            format!("  #{id}"),
            Style::default().fg(Color::Rgb(70, 70, 70)),
        ));
    }

    Line::from(spans)
}

fn draw_content(frame: &mut Frame, app: &mut App, area: Rect) {
    app.regions.content = area.inner(Margin::new(1, 1));

    let title = app
        .current_series()
        .map(|s| {
            let base = if s.name.is_empty() {
                format!("event {}", s.event_id)
            } else {
                s.name.clone()
            };
            if s.style.is_empty() {
                base
            } else {
                format!("{base} · {}", s.style)
            }
        })
        .unwrap_or_else(|| "Content".to_string());

    let content: Vec<ContentItem> = app.current_content().to_vec();

    if content.is_empty() {
        let paragraph = Paragraph::new(vec![
            Line::from(Span::styled(
                "No series loaded.",
                Style::default().fg(Color::Gray),
            )),
            Line::from(Span::styled(
                "Press r to fetch the current LFM season.",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .block(dark_block(&title, app.focus == Focus::Content));
        frame.render_widget(paragraph, area);
        return;
    }

    let items: Vec<ListItem> = content
        .iter()
        .map(|item| {
            let status = app.status_for(item);
            let selected = item
                .content_id
                .map(|id| app.selected.contains(&id))
                .unwrap_or(false);
            let coverage = item
                .content_id
                .and_then(|id| app.coverage.get(&id).copied());
            ListItem::new(content_line(item, status, selected, coverage))
        })
        .collect();

    let list = List::new(items)
        .block(dark_block(&title, app.focus == Focus::Content))
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(24, 40, 48))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, area, &mut app.content_state);
}

fn draw_transfer(frame: &mut Frame, app: &App, area: Rect) {
    let block = dark_block("Transfer", false);

    match &app.progress {
        Some(p) => {
            let label = format!(
                "{}  {} / {}  {:.0}%  {}/s",
                p.label,
                human_bytes(p.done),
                human_bytes(p.total),
                p.ratio() * 100.0,
                human_bytes(p.bps.max(0.0) as u64),
            );
            let gauge = Gauge::default()
                .block(block)
                .gauge_style(Style::default().fg(Color::Cyan).bg(Color::Rgb(20, 20, 20)))
                .ratio(p.ratio())
                .label(label);
            frame.render_widget(gauge, area);
        }
        None => {
            let text = if app.signup_popup.as_ref().is_some_and(|p| p.submitting) {
                "sending sign-up…".to_string()
            } else if app.join_loading {
                "resolving join data…".to_string()
            } else if app.busy {
                format!("{} working…", app.busy_spinner())
            } else {
                "idle".to_string()
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    text,
                    Style::default().fg(Color::DarkGray),
                )))
                .block(block),
                area,
            );
        }
    }
}

fn draw_log(frame: &mut Frame, app: &App, area: Rect) {
    let height = area.height.saturating_sub(2) as usize;
    let skip = app.logs.len().saturating_sub(height);
    let lines: Vec<Line> = app
        .logs
        .iter()
        .skip(skip)
        .map(|msg| Line::from(Span::styled(msg.clone(), Style::default().fg(Color::Gray))))
        .collect();

    frame.render_widget(
        Paragraph::new(lines)
            .block(dark_block("Log", false))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let hint = match (app.view, app.focus) {
        (View::Mods, Focus::Series) => {
            "Left: choose a championship.  Right: every car and track its servers require."
        }
        (View::Mods, Focus::Content) => {
            "space marks an item · s installs the marked ones (or everything missing here) · v re-checks the disk"
        }
        (View::Sessions, _) => {
            "enter opens a practice link or race details · R signs up for the highlighted race"
        }
    };

    let keys = match app.view {
        View::Mods => {
            "j/k move · Tab pane · space select · a all · s sync · v verify · d purge · r refresh · L login · ? help · q quit"
        }
        View::Sessions => {
            "j/k move · Tab pane · enter open · R sign up · w next week · o series page · r reload · L login · ? help · q quit"
        }
    };

    let lines = vec![
        Line::from(Span::styled(hint, Style::default().fg(Color::Cyan))),
        Line::from(Span::styled(keys, Style::default().fg(Color::Gray))),
        Line::from(Span::styled(
            "mouse: click a row to select · double-click to mark/open · wheel scrolls · ? explains everything",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_join_popup(frame: &mut Frame, area: Rect, info: &JoinInfo) {
    let popup = centered(area, 68, 38);

    let mut lines = vec![
        Line::from(Span::styled(
            info.title.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    match &info.url {
        Some(url) => {
            lines.push(Line::from(Span::styled(
                "Join link",
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(Span::raw(url.clone())));
            if !info.note.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    info.note.clone(),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        None => {
            lines.push(Line::from(Span::styled(
                info.note.clone(),
                Style::default().fg(Color::Yellow),
            )));
        }
    }

    if let Some(password) = &info.password {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Password  ", Style::default().fg(Color::DarkGray)),
            Span::styled(password.clone(), Style::default().fg(Color::Green)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "o join via Steam · c copy link · y copy password · Esc close",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(dark_block("Session", true))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn draw_signup_popup(frame: &mut Frame, area: Rect, popup: &SignupPopup) {
    let rect = centered(area, 74, 60);

    let mut lines = vec![
        Line::from(Span::styled(
            popup.label.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!(
                "{}/{} slots · signup closes {}",
                popup.registered, popup.slots, popup.signup_closes
            ),
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
    ];

    if popup.signed_up {
        lines.push(Line::from(Span::styled(
            "✔ You are registered for this race.",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "u withdraw · Esc close",
            Style::default().fg(Color::DarkGray),
        )));
    } else if popup.loading {
        lines.push(Line::from(Span::styled(
            "Loading the cars available for this race…",
            Style::default().fg(Color::Yellow),
        )));
    } else if let Some(error) = &popup.error {
        lines.extend([
            Line::from(Span::styled(error.clone(), Style::default().fg(Color::Red))),
            Line::from(""),
            Line::from(Span::styled(
                "r retry · Esc close",
                Style::default().fg(Color::DarkGray),
            )),
        ]);
    } else if popup.cars.is_empty() {
        lines.push(Line::from(Span::styled(
            "No cars were offered for this race.",
            Style::default().fg(Color::Yellow),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Esc close",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "Car",
            Style::default().fg(Color::DarkGray),
        )));

        // Show a window around the selection so long car lists stay inside the dialog.
        const VISIBLE: usize = 14;
        let start = popup
            .selected
            .saturating_sub(VISIBLE / 2)
            .min(popup.cars.len().saturating_sub(VISIBLE));
        let end = (start + VISIBLE).min(popup.cars.len());

        for index in start..end {
            let car = &popup.cars[index];
            let selected = index == popup.selected;
            lines.push(Line::from(vec![
                Span::styled(
                    if selected { "▶ " } else { "  " },
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    clip(&car.label(), 62),
                    if selected {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
            ]));
        }

        lines.push(Line::from(""));
        if popup.submitting {
            lines.push(Line::from(Span::styled(
                "Sending the sign-up to LFM…",
                Style::default().fg(Color::Yellow),
            )));
        }
        lines.push(Line::from(Span::styled(
            "j/k choose a car · enter register · r reload · Esc cancel",
            Style::default().fg(Color::DarkGray),
        )));
    }

    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .block(dark_block("Race sign-up", true))
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let popup = centered(area, 92, 94);

    let head = |text: &'static str| {
        Line::from(Span::styled(
            text,
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))
    };
    let dim = |text: &'static str| Line::from(Span::styled(text, Style::default().fg(Color::DarkGray)));

    let lines = vec![
        Line::from(Span::styled(
            "LFM · Assetto Corsa — mod sync",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        dim("Reads the LFM season, checks the AC install, downloads what the servers require. Official DLC is never downloaded."),
        Line::from(""),
        head("MODS TAB  (1)"),
        dim("  Left pane — series.  Right pane — the cars and tracks its servers require."),
        Line::from("  space   mark / unmark the highlighted item"),
        Line::from("  a / c   mark everything installable in the series / clear all marks"),
        Line::from("  s       install marked items; nothing marked = everything missing in this series"),
        Line::from("  v       re-check every catalogue item against the disk (no downloads)"),
        Line::from("  d       delete installed content that is not marked (asks first)"),
        Line::from("  x       install RealPenalty + the LFM livery script and enable them in AC"),
        Line::from("  r       reload the season from LFM"),
        Line::from(""),
        head("STATUS TAGS"),
        Line::from("  ok      every file in the mod's file list is on disk"),
        Line::from("  part    most files are there (row shows found/expected); syncing fills the rest"),
        Line::from("  upd     the files are there but the version changed"),
        Line::from("  new     not installed"),
        Line::from("  ext     not in the LFM catalogue — download it from the modder"),
        Line::from("  dlc     official Kunos content, never downloaded"),
        Line::from(""),
        head("SESSIONS TAB  (2)"),
        dim("  Practice servers and upcoming races for the selected series."),
        Line::from("  enter   open the practice join link or the race details"),
        Line::from("  o       in the popup: start Assetto Corsa through Steam with the link"),
        Line::from("  c / y   copy the join link / the race password"),
        Line::from("  R       sign up for the highlighted race (pick a car first); u withdraws"),
        Line::from("  w       include next week's practice servers"),
        Line::from("  o       otherwise: open the series page on lowfuelmotorsport.com"),
        Line::from(""),
        head("LOGIN"),
        Line::from("  L       log in through the browser (Steam); the key is saved for next time"),
        Line::from(""),
        head("MOUSE"),
        Line::from("  click a tab to switch · click a row to select · double-click to mark or open"),
        Line::from("  wheel scrolls the pane under the pointer · any click closes this help"),
        Line::from(""),
        dim("  Esc / q closes this guide · ? reopens it · j/k and Tab work everywhere"),
    ];

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(dark_block("Help", true))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn draw_confirm(frame: &mut Frame, area: Rect, count: usize) {
    let popup = centered(area, 56, 24);
    let lines = vec![
        Line::from(Span::styled(
            format!("Purge {count} content item(s)?"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("Only files listed in the content structure are deleted."),
        Line::from(""),
        Line::from(vec![
            Span::styled("y", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw(" confirm    "),
            Span::styled("n / Esc", Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD)),
            Span::raw(" cancel"),
        ]),
    ];

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(dark_block("Confirm", true))
            .wrap(Wrap { trim: true }),
        popup,
    );
}

fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);

    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}
