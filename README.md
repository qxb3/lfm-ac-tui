# lfm-ac

Ratatui TUI for Assetto Corsa on Low Fuel Motorsport: sync the mods a season requires, and
navigate its practice servers and upcoming races. It reimplements the AC side of the LFM App —
see [`../LFM-AC-SYNC-ANALYSIS.md`](../LFM-AC-SYNC-ANALYSIS.md) for the protocol it speaks.

## Requirements

- Rust 1.80+ (developed on 1.95)
- Assetto Corsa installed through Steam
- An LFM API token — a normal LFM App login is enough, the token is read from
  `%APPDATA%\lfm-hub\config.json`

## Run

```bash
cargo run --release
cargo run --release -- --ac-path "C:\SteamLibrary\steamapps\common\assettocorsa"
LFM_API_KEY=<token> cargo run --release

# reinstall specific content ids without the TUI (useful after a bad install)
./target/release/lfm-ac --install 56,98,272

# log in through the browser and save the API key (no LFM App needed)
./target/release/lfm-ac --login
```

Resolution order: `--api-key` → `$LFM_API_KEY` → this tool's saved login
(`~/.config/lfm-ac/config.json`) → the LFM App's `config.json`.
AC folder: `--ac-path` → `$LFM_AC_PATH` → `acSteamPath` from the LFM App → Steam detection
(registry + `libraryfolders.vdf`).

## Login

The LFM App is not required. With no token, press `L` in the TUI (or run `--login`):

1. a **temporary** `liverytool://` handler is registered for this binary only;
2. the browser opens LFM's Steam login — your browser may ask to open an external app, allow it;
3. the redirect `liverytool://auth?token=…` runs `lfm-ac --auth-callback`, which hands the key
   to the waiting process and exits;
4. the key is written to `~/.config/lfm-ac/config.json` (mode `0600`), the temporary handler is
   removed and the previous one restored, and the season loads without restarting.

If a login is interrupted (Ctrl+C), the next run removes the leftover handler automatically.
Nothing is installed permanently. A `return_to` pointed at a loopback `http://127.0.0.1` port
was tried first and does not work: LFM bounces between Steam and its own login page instead of
redirecting, so the desktop app's own deep link is used.

## Keys

| Key | Action |
|---|---|
| `1` / `2` | Mods tab / Sessions tab |
| `j`/`k`, arrows | move in the focused pane |
| `Tab`, `h`/`l` | switch series ↔ content |
| `g`/`G`, Home/End | jump |
| `space` | toggle selection |
| `a` / `c` | select all syncable in series / clear selection |
| `s` | sync selection (or everything missing in the current series) |
| `v` | verify every catalogue item against disk |
| `d` | purge installed content outside the selection (asks for confirmation) |
| `x` | install RealPenalty (112) + LFM livery script (114) and toggle the AC ini files |
| `L` | log in through the browser (Steam) and save the key |
| `?` | full guide: what every tab, key and status tag does |
| `q` | quit |

Mouse works throughout: click the tabs to switch, click a row to select, double-click to mark
content (Mods) or open a practice link / race (Sessions), and scroll with the wheel over the pane
you want to move. Clicking anywhere closes the guide.

The footer always shows, for the pane you are in, what the screen is for, the keys that matter,
and the mouse equivalent, so nothing has to be memorised.

Sessions tab:

| Key | Action |
|---|---|
| `enter` | join the selected practice server / resolve a race's join link |
| `s` | sign up for the highlighted race (pick a car) |
| `u` | sign out of the highlighted race |
| `w` | show next week's practice servers as well |
| `o` | open the series page on lowfuelmotorsport.com |
| `j`/`k` | move through practice servers and races |

In the join popup: `o` opens the link, `c` copies it, `y` copies the server password, `Esc`
closes.

In the sign-up popup: `j`/`k` choose a car, `enter` registers, `r` reloads the car list,
`u` signs out when you are already registered, and `Esc` cancels. Errors from LFM (sign-ups
closed, full grid, missing safety rating) stay in the dialog so the reason is visible.

Sign-up is gated locally before any request is made. The series payload's `min_sr` is
checked against your profile, `prequali` against `hotlaps/{event_id}/me`, and per race the
grid limit (`registered >= slots`), the `signupStart`/`signupCloses` window and the race
start time. `min_license` is shown as a requirement but not enforced locally, because its
ordering is not documented. A race that fails any check is marked with a red `✖` and its
first reason in the Sessions list, and the dialog lists every reason and refuses to submit.
Series with `min_sr <= 1` have no SR floor.

Your Assetto Corsa ELO (`rating_by_sim` entry for sim 3, hidden for rookie licences like the
desktop app) and safety rating (`safety_rating`) are shown in the top-right of the header,
refreshed with the season data.

The race list refreshes itself every minute while the Sessions tab is open, and the season
(plus installed-content check) every 15 minutes on the Mods tab, so there is no refresh key.
Refreshes are skipped while a dialog is open or a download/check is running.

Status tags: `ok` installed, `part` nearly complete (shows `found/expected`), `upd` version
changed, `new` missing, `ext` not in the catalogue (download it from the modder), `dlc` official
content, `..` being checked.

## Sessions

The Sessions tab lists, for the selected series:

- **practice servers** — the `practice_server` / `practice_server_next_week` entries of the
  season payload, whose links are base64-encoded `acmanager://race/online/join?…` URLs;
- **upcoming races** — `v2/seasons/getUpcomingSessions/{event_id}`: date, track, sign-ups vs
  slots, session lengths (p/q/r), whether you are signed up, and whether the server is live.

`enter` on a race calls `races/checkData` and then `race/{id}/getJoinData`, which LFM only
answers once the server is live and you are signed up; otherwise the popup explains why.

`o` in the join popup does not use `xdg-open`: on Linux the scheme only means something inside
the game's prefix, so a host handler cannot work. Instead the link is handed to Steam
(`steam -applaunch 244210 "acmanager://…"`), which starts Assetto Corsa in its normal Proton
environment with the URL on the command line; Content Manager — the game's launcher here —
picks it up and opens the join dialog.

`s` registers directly: the car list comes from
`v2/seasons/getAvailableCars/{event_id}?race_id=…`, and the sign-up is
`POST signup/{race_id}/solo` on the PitPath API with `carModel`, `eventId` and `simId=3`.
`u` signs out with `DELETE signup/{race_id}/solo?eventId=…`. Both mirror the LFM App's own
`RaceCard`, with `liveryId=0` and no team (AC series here are solo).

## Behaviour notes

- **Install detection is filesystem-based.** One walk of the AC install builds a
  case-insensitive path index, then each item is scored by how many files from its server
  `contentStructure` are present. This is deliberate: the LFM App treats content without an
  `ac-content.tool` marker as "needs reinstall", and on a Linux/Proton install populated by
  Content Manager those markers do not exist, so everything looks missing. Markers are still
  read, but only to spot an explicit version change.
  - `ok` — every expected file found (casing does not matter);
  - `part` — at least 90 % found and the mod's root folder exists, shown as `found/expected`;
  - `new` — below that, or the root folder is absent.
  Both `part` and `new` are re-downloaded by `s`.
- Downloads stream to a temp file, are size-checked, then extracted through a temporary
  directory and copied in. ZIP entries are sanitised (the Electron app does not).
- **The archive layout is respected.** A single wrapper folder is unwrapped, but never past an
  AC root folder: `Mod/content/cars/x/…` becomes `content/cars/x/…`, while an archive that
  already starts at `content/` is left alone. Unwrapping blindly turns
  `content/tracks/x/models.ini` into `./models.ini` and spreads a mod across the game root —
  which is exactly why Content Manager then reports damaged tracks and Kunos cars.
- After extracting, the manifest paths are checked; a mod whose files did not land there fails
  the install and gets **no** marker, instead of being recorded as installed.
- `minioKey` dedupe: entries sharing a physical mod are downloaded once.
- Purge deletes only the files listed in `contentStructure` and prunes emptied folders.
- The PitPath license gate (`license.pitpath.net`) that the Electron app enforces at startup is
  intentionally not implemented; this is a client helper, not an auth mechanism.

## Known limitations

- Windows is the only target with AC auto-detection; elsewhere pass `--ac-path`.
- A full check walks the install once (~47 k files here, about a second) and makes one
  metadata request per catalogue item (~200), so the first scan takes a few seconds.
- Race times are shown exactly as LFM returns them (`YYYY-MM-DD HH:MM:SS`, series time zone),
  so no countdown is computed.
- No resumable downloads, no bandwidth throttling (the app throttles to 75 % of a measured
  speed test).
- `git`-style semver comparison of `version/hub` is not enforced, only reported.
- Nothing is written back to the LFM App's `config.json`; selection lives in memory for the
  session.
