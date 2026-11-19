# lfm-ac-tui

A terminal client for [Low Fuel Motorsport](https://lowfuelmotorsport.com) on Assetto Corsa.
It keeps the mods a season requires in sync, shows the practice servers and upcoming races,
handles race sign-up/sign-out, and can join a live server by handing the join link to Steam.

It reimplements the Assetto Corsa side of the LFM desktop app in Rust + Ratatui.

## Features

- **Mod sync** — reads the current LFM season, checks the AC install against each item's
  server-side file manifest, and downloads only what is missing or outdated.
- **Multi-library Steam detection** — finds Assetto Corsa on Windows, and on Linux through
  native Steam packages, Flatpak or Snap, including secondary Steam libraries.
- **Sessions** — practice servers for this/next week, upcoming races, session lengths, live
  state, and the join link/password popup.
- **Race sign-up** — pick an accepted car and register from the TUI, or sign out again.
- **Eligibility checks** — series minimum SR, pre-qualification lap, grid capacity and the
  sign-up window are checked before anything is sent; blocked races are marked and explained.
- **Profile readout** — your AC ELO and safety rating in the top-right corner.
- **Automatic refresh** — no refresh key; race lists and season data update on their own.
- **Headless mode** — `--install` for scripted re-installs, `--login` for browser login only.
- **No LFM App needed** — an existing LFM App token/config is reused when present, otherwise
  the built-in Steam browser login stores its own key.

## Requirements

- Rust 1.80 or newer to build from source (developed against 1.95).
- Assetto Corsa installed through Steam. On Linux this normally means Steam Play/Proton.
- An LFM account. Either an LFM App installation whose token can be reused, or the built-in
  browser login.

## Install

```bash
git clone <repository-url>
cd lfm-ac-tui
cargo build --release
```

The binary is `target/release/lfm-ac`. Copy it anywhere on `PATH` if you like.

## Quick start

```bash
# start the TUI; reuses the LFM App token and auto-detects Assetto Corsa
cargo run --release

# explicit paths/token
cargo run --release -- \
  --ac-path "$HOME/.local/share/Steam/steamapps/common/assettocorsa" \
  --api-key "<token>"

# browser login only, then exit (stores the key for next time)
./target/release/lfm-ac --login

# reinstall specific content ids without the TUI (useful after a bad install)
./target/release/lfm-ac --install 56,98,272
```

### Command line

| Flag | Description |
|---|---|
| `--api-key <TOKEN>` | API token. Also reads `LFM_API_KEY`. |
| `--ac-path <DIR>` | Assetto Corsa install folder. Also reads `LFM_AC_PATH`. |
| `--install <IDS>` | Comma-separated content ids to install, print the log and exit. |
| `--login` | Open the browser login, save the key and exit. |
| `--help`, `--version` | Usage and build version. |

Token resolution order: `--api-key` → `$LFM_API_KEY` → this tool's saved login →
the LFM App's `config.json`.

Install folder resolution order: `--ac-path` → `$LFM_AC_PATH` → `acSteamPath` from the LFM
App config → automatic Steam detection.

## Authentication

The LFM App is not required. With no token, press `L` in the TUI or run `--login`:

1. A temporary `liverytool://` handler is registered for this binary only.
2. The browser opens LFM's Steam login. Your browser may ask to open an external app — allow
   it.
3. The redirect (`liverytool://auth?token=…`) invokes `lfm-ac --auth-callback`, which hands the
   key back to the waiting process. If the scheme handler cannot be registered, a short-lived
   loopback HTTP listener is used instead.
4. The key is written to the per-user config directory with mode `0600` on Unix, the temporary
   handler is removed and any previous handler restored, and the season reloads without a
   restart.

An interrupted login (Ctrl+C) leaves its handler behind; the next run removes it
automatically. Nothing is installed permanently.

## Assetto Corsa detection

Steam roots and `libraryfolders.vdf` are scanned in this order, and the folder is identified
through `appmanifest_244210.acf` (falling back to known folder names):

- **Windows** — `HKCU\Software\Valve\Steam\SteamPath`, then `%ProgramFiles(x86)%\Steam` and
  `%ProgramFiles%\Steam`, plus every library listed in the VDF.
- **Linux** — `$XDG_DATA_HOME/Steam`, `~/.local/share/Steam`, `~/.steam/steam`,
  `~/.steam/root`, `~/.steam/debian-installation`, the Flatpak data layouts under
  `~/.var/app/com.valvesoftware.Steam/`, and the Snap layout under
  `~/snap/steam/common/`. `STEAM_ROOT`/`STEAM_DIR` are honoured when set. Secondary libraries
  from `libraryfolders.vdf` are included.
- **macOS** — `~/Library/Application Support/Steam`. Assetto Corsa itself is Windows-only, so
  this is mainly useful for completeness.

Pass `--ac-path` if detection picks the wrong install or the game lives outside Steam.

## The interface

### Header

Left to right: tabs, the detected AC install, token state, installed/tracked counters, and —
on the right — `ELO 1500 · SR 85`. ELO is hidden for rookie licences, matching the desktop app.
The counters refer to catalogue items of the loaded season; official DLC is excluded.

### Tabs

- **Mods (1)** — the season's series on the left, the cars and tracks they require on the
  right. Selection marks drive `s`; status tags show disk state.
- **Sessions (2)** — practice servers and upcoming races for the selected series, with
  sign-up state and eligibility.

The footer always explains the current pane and its keys. `?` opens the full guide.

### Automatic refresh

- The race list reloads every 60 seconds while the Sessions tab is open. Background loads are
  quiet and keep the highlighted race.
- The season and the installed-content check refresh every 15 minutes on the Mods tab.
- Refreshes are skipped while a dialog is open, a download/check is running, or no token is
  configured. There is deliberately no refresh key.

## Keys

### Global

| Key | Action |
|---|---|
| `1` / `2` | Mods tab / Sessions tab |
| `j`/`k`, arrows | move in the focused pane |
| `Tab`, `h`/`l` | switch series ↔ content |
| `g`/`G`, Home/End | jump to first/last |
| `PgUp`/`PgDn` | move by a page |
| `L` | log in through the browser (Steam) |
| `?` / `F1` | full help |
| `q`, `Esc` | quit (or close the open dialog) |

### Mods tab

| Key | Action |
|---|---|
| `space` | mark / unmark the highlighted item |
| `a` / `c` | mark everything installable in the series / clear all marks |
| `s` | sync marked items; with nothing marked, everything missing in this series |
| `v` | re-check every catalogue item against the disk (no downloads) |
| `d` | purge installed content outside the selection (asks first) |
| `x` | install RealPenalty (112) + the LFM livery script (114) and enable them in AC |

### Sessions tab

| Key | Action |
|---|---|
| `enter` / `space` | open a practice link, or resolve a race's join link |
| `s` | sign up for the highlighted race (choose a car) |
| `u` | sign out of the highlighted race |
| `w` | include next week's practice servers |
| `o` | open the series page on lowfuelmotorsport.com |

Join popup: `o` launches the join link through Steam, `c` copies the link, `y` copies the
password, `Esc` closes.

Sign-up popup: `j`/`k` choose a car, `enter` registers, `r` reloads the car list, `u` signs
out when already registered, `Esc` cancels. Unmet requirements are listed in the dialog and
`enter` refuses to submit while any remain.

### Mouse

Click the tabs to switch, click a row to select, double-click to mark content (Mods) or open a
practice link/race (Sessions), and scroll the pane under the pointer. Any click closes the
help overlay. The purge confirmation only changes state from the keyboard, so a stray click
can never approve a deletion.

## Race sign-up and eligibility

Sign-up is checked locally before a request is made:

| Check | Source |
|---|---|
| Series minimum SR | `min_sr` in the season payload; values `<= 1` mean no floor |
| Pre-qualification lap | `prequali` flag + `hotlaps/{event_id}/me` |
| Grid capacity | `registered >= slots` |
| Sign-up window | `signupStart` / `signupCloses` from the race list |
| Race start | the race date has passed |

`min_license` is displayed as a requirement but not enforced locally, because its ordering is
not publicly defined. The server remains the final authority; its error (`status`/`title`) is
shown in the dialog.

A race that fails a check is marked with a red `✖` and the first reason in the Sessions list.
The sign-up dialog lists every reason. Pre-qualification is re-checked when the dialog opens,
so a newly set lap counts and a transient failure does not stick permanently.

The actual calls mirror the desktop app's `RaceCard`:

- `GET v2/seasons/getAvailableCars/{event_id}?race_id=…` — accepted cars;
- `POST signup/{race_id}/solo?carModel=…&eventId=…&simId=3` on the PitPath API, with the
  driver payload from `GET user`;
- `DELETE signup/{race_id}/solo?eventId=…` — sign out.

AC series here are solo, so `teamId=0` and `liveryId=0` are sent.

## Sessions and joining

For the selected series the Sessions tab shows:

- **practice servers** — the `practice_server` / `practice_server_next_week` entries of the
  season payload, whose links are base64-encoded `acmanager://race/online/join?…` URLs;
- **upcoming races** — `v2/seasons/getUpcomingSessions/{event_id}`: date, track, sign-ups vs
  slots, session lengths (practice/quali/race), whether you are signed up, and whether the
  server is live.

`enter` on a race calls `races/checkData` and then `race/{id}/getJoinData`. LFM only answers
that once the server is live and you are signed up; otherwise the popup explains why.

`o` in the join popup does not use `xdg-open`. On Linux the `acmanager://` scheme only means
something inside the game's prefix, so a host handler cannot work. Instead the link is handed
to Steam:

```bash
steam -applaunch 244210 "acmanager://…"
```

Steam sets up the Proton environment and starts Assetto Corsa with the URL on the command
line; Content Manager picks it up and opens the join dialog.

## How mod sync works

- **Install detection is filesystem-based.** One walk of the AC install builds a
  case-insensitive path index; each item is then scored by how many files from its server
  `contentStructure` are present. This intentionally differs from the Electron app, which
  treats content without an `ac-content.tool` marker as “needs reinstall” — common for
  Content Manager installs and Linux/Proton setups. Markers are still read, but only to spot
  an explicit version change.
  - `ok` — every expected file found (casing does not matter);
  - `part` — at least 90% found and the mod's root folder exists, shown as `found/expected`;
  - `new` — below that, or the root folder is absent;
  - `upd` — files are present but the marker hash differs;
  - `ext` — not in the PitPath catalogue (download manually);
  - `dlc` — official content, never downloaded.
- Downloads stream to a temporary file, are size-checked, then extracted through a temporary
  directory and copied in. ZIP entries are sanitised.
- **Archive layout is respected.** A single wrapper folder is unwrapped, but never past an AC
  root folder: `Mod/content/cars/x/…` becomes `content/cars/x/…`, while an archive already
  starting at `content/` is left alone. Blind unwrapping would scatter a mod across the game
  root and produce damaged content.
- After extraction the manifest paths are re-checked. If files did not land where expected,
  the install fails and no marker is written instead of recording a false success.
- Entries sharing a `minioKey` are downloaded once.
- Purge deletes only the files listed in `contentStructure` and prunes emptied folders.
- The PitPath license gate the Electron app enforces at startup is intentionally not
  implemented; this is a client helper, not an auth mechanism.

## Files and configuration

| Path | Purpose |
|---|---|
| `%APPDATA%\lfm-ac\config.json` (Windows) / `$XDG_CONFIG_HOME/lfm-ac/config.json` (Linux, usually `~/.config/lfm-ac/config.json`) | This tool's saved API key; `0600` on Unix |
| `%APPDATA%\lfm-hub\config.json` / `~/.config/lfm-hub/config.json` | LFM App settings, reused for `apiKey`, `acSteamPath` and the selected content ids |
| `~/.var/app/<app-id>/config/lfm-hub/config.json` | Flatpak LFM App settings |

The app also honours `$LFM_API_KEY`, `$LFM_AC_PATH`, `$STEAM_ROOT` and `$STEAM_DIR`.

## Troubleshooting

- **“AC: not found”** — pass `--ac-path` pointing at the folder containing `content/` and
  `system/`. On Linux this is usually
  `~/.local/share/Steam/steamapps/common/assettocorsa` (or the Flatpak/Snap equivalent, or a
  secondary library). `--ac-path` is printed in the header when detected.
- **“token: missing”** — press `L`, pass `--api-key`, or set `LFM_API_KEY`. The LFM App token
  is only reused if its `config.json` is readable.
- **“ELO — · SR —”** — the profile request failed or the token was rejected; the log shows the
  reason. The rest of the app still works.
- **Browser login does not return** — allow the browser's “open external app” prompt, and make
  sure nothing else claims `liverytool://`. An interrupted login is cleaned up on the next
  run.
- **A race is marked `✖`** — the dialog lists exactly which check failed. Requirements may also
  fail server-side for reasons this client cannot see (e.g. a ban or a class-specific
  pre-qualification rule).
- **“server expects client version …”** — informational only; `version/hub` is reported, not
  enforced.

## Known limitations

- No resumable downloads or bandwidth throttling (the desktop app throttles to 75% of a
  measured speed test).
- The 15-minute season refresh re-walks the AC install, which can take a few seconds on a large
  library or a slow disk.
- Race times are displayed exactly as LFM returns them (`YYYY-MM-DD HH:MM:SS`). The local
  eligibility window treats zoneless timestamps as local time, as the desktop app does;
  timezone differences can shift the boundary by an hour or two.
- `min_license` is shown but not enforced locally.
- Pre-qualification is checked as “has a lap for this event”, not per car class; the server
  still enforces class eligibility.
- Selection lives in memory for the session; nothing is written back to the LFM App config.
- One Assetto Corsa install is used at a time.
- Sign-up/sign-out endpoints mirror the desktop app and may change if LFM changes its API.

## Development

```bash
cargo build          # debug build
cargo test           # unit tests (sync, integrity, config, auth, eligibility, UI helpers)
cargo build --release
```

Source layout:

| Path | Role |
|---|---|
| `src/main.rs` | CLI, terminal loop, headless modes |
| `src/app.rs` | TUI state, key/mouse handling, eligibility |
| `src/ui.rs` | Ratatui rendering |
| `src/worker.rs` | Async network/disk worker and events |
| `src/api.rs` | LFM + PitPath HTTP client and DTOs |
| `src/auth.rs` | Browser login and token storage |
| `src/config.rs` | LFM App config, AC/Steam detection |
| `src/session.rs` | Join links, Steam launch, clipboard |
| `src/ac/` | Path index, integrity checks, extraction, purge, markers, scripts |

## License

MIT — see [LICENSE](LICENSE).
