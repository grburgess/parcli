# parcli — design spec

Date: 2026-09-13
Status: approved in chat, pending spec review

## Purpose

A colorful, `top`-like terminal dashboard that tracks international parcels by
driving the free parcelsapp.com widget headlessly, lets the user add and remove
tracking numbers, and re-polls each parcel on an interval. Single Rust binary.

Research backing the choices: `docs/research-2026-09-13.md`. 17TRACK's API was
the first choice but requires a business account, so the parcelsapp headless
route is the MVP provider.

## Non-goals (MVP)

- Multiple providers running at once (the trait exists; only one impl ships).
- Notifications, webhooks, sound.
- Reverse-engineering the parcelsapp XHR for a browserless provider (future spike).
- Windows support (untested; macOS/Linux only).

## Runtime requirements

- Rust stable ≥ 1.80 (`rustup update stable`; the machine currently has 1.65).
- A Chromium-based browser. Discovery order: `PARCLI_CHROME` env var, then
  `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`, then
  `google-chrome`, `chromium`, `chromium-browser` on `PATH`. Missing browser is a
  fatal startup error that names the env var.

## Crates

| Purpose | Crate |
|---|---|
| async runtime | `tokio` (rt-multi-thread, macros, time, sync) |
| TUI | `ratatui`, `crossterm` (event-stream feature) |
| headless browser | `chromiumoxide` (tokio-runtime feature), `futures` |
| serialization | `serde`, `serde_json`, `toml` |
| paths | `directories` |
| CLI args | `clap` (derive) |
| errors | `anyhow`, `thiserror` |
| time | `chrono` |
| text input | `tui-input` |

## Module layout

```
src/
  main.rs        clap args, browser discovery, wiring, runs tui::run
  app.rs         App state + pure event handlers (Mode enum, selection, table rows)
  store.rs       ParcelList TOML load/save; StateCache JSON load/save
  provider/
    mod.rs       Provider trait, Tracking/TrackEvent/Status types, status classifier
    parcelsapp.rs  ParcelsAppProvider (chromiumoxide) + response parser
  poller.rs      worker task: schedule, sequential polling, backoff, mpsc out
  tui.rs         terminal setup/teardown, tokio::select! event loop
  ui.rs          ratatui rendering (header, table, detail panel, input, confirm)
tests/
  fixtures/parcelsapp_response.json
```

## Data model

```rust
struct Parcel { number: String, label: Option<String>, added: DateTime<Utc> }
struct ParcelList { parcels: Vec<Parcel> }                // TOML

enum Status { Pending, InTransit, OutForDelivery, Delivered, Exception, Unknown }

struct TrackEvent { time: Option<DateTime<Utc>>, description: String, location: Option<String> }
struct Tracking { number: String, carrier: Option<String>, status: Status,
                  events: Vec<TrackEvent>, fetched_at: DateTime<Utc> }

struct ParcelState { tracking: Option<Tracking>, last_error: Option<String>,
                     failures: u32, next_poll: DateTime<Utc> }
struct StateCache { by_number: HashMap<String, ParcelState> }   // JSON
```

Files (via `directories::ProjectDirs::from("", "", "parcli")`):
- config: `<config_dir>/parcels.toml`
- cache:  `<cache_dir>/state.json`

Both are written atomically (write temp file, rename). A missing file is an
empty list/cache; a corrupt file is a fatal error with the path in the message.

## Provider

```rust
#[async_trait]
trait Provider { async fn track(&self, number: &str) -> anyhow::Result<Tracking>; }
```

`ParcelsAppProvider`:
1. Launch one headless browser at startup (`chromiumoxide::Browser::launch`,
   `BrowserConfig` with the discovered executable, headless).
2. `track(number)`: open a new page at `https://parcelsapp.com/widget`, subscribe
   to `Network.responseReceived` events, set `#track-input` value, click
   `#track-button`, await the first response whose URL contains `api/v2/parcels`,
   fetch its body with `Network.getResponseBody`, close the page, parse.
   Timeout 45 s → error.
3. Parsing lives in a pure `fn parse_response(json: &str, number: &str) ->
   Result<Tracking>`; the exact field names are taken from a captured fixture
   during implementation (first task of the provider work is capturing it).
   Status classification maps parcelsapp's status string/keywords onto `Status`
   with `Unknown` as fallback.

Polling is strictly sequential through one worker, so at most one page is open
at a time.

## Poller

- Owns the `Provider`, receives `PollCommand::{Refresh(number), RefreshAll,
  Remove(number), Add(number)}` over mpsc, sends `PollResult { number, result }`
  back.
- Loop: every second, find the parcel with the earliest `next_poll <= now` that
  is not `Delivered`; poll it; on success `next_poll = now + interval`,
  `failures = 0`; on error `failures += 1`, `next_poll = now + min(interval *
  2^failures, 1 h)`. Newly added parcels are polled immediately.
- `interval` from `--interval <minutes>` (default 10).

## App state and keys

`Mode::{Normal, Adding { input: Input }, ConfirmRemove, Detail { scroll: u16 }}`

| Key | Normal | Adding | ConfirmRemove | Detail |
|---|---|---|---|---|
| `q` | quit | — | — | back |
| `j/k`, `↑/↓` | move selection | — | — | scroll |
| `a` | enter Adding | — | — | — |
| `d` | enter ConfirmRemove (if any) | — | — | — |
| `r` / `R` | refresh selected / all | — | — | refresh this |
| `Enter` | Detail | commit (non-empty, trimmed, dedup) | — | — |
| `Esc` | — | cancel | cancel | back |
| `y` / `n` | — | — | remove / cancel | — |
| typing | — | edits input (`tui-input`) | — | — |

Adding accepts an optional label after a space: `RB123456789CN camera`.

All handlers are pure functions on `App` returning `Vec<Effect>` (`SaveParcels`, `SendPoll(cmd)`, `Quit`) so they can be unit-tested without a terminal.

## UI

- **Header** (1 line): `parcli · 4 parcels · next poll in 3m12s · last error: …`
- **Table**: columns `LABEL NUMBER CARRIER STATUS LAST EVENT AGE NEXT`.
  `AGE` = time since last event; `NEXT` = countdown or `polling…` with spinner,
  `done` for Delivered. Status color: Pending grey, InTransit blue,
  OutForDelivery yellow, Delivered green, Exception red, Unknown dark grey.
  Selected row reversed.
- **Detail**: full-width panel replacing the table: parcel header + scrollable
  list of events (time, location, description), newest first.
- **Adding**: one-line input at the bottom with cursor; **ConfirmRemove**:
  bottom line `remove <number>? (y/n)`.
- **Footer**: key hints for the current mode.
- Render on every event and on a 250 ms tick (for spinner/countdowns).

## Error handling

- Provider errors are per-parcel; they go into `ParcelState.last_error` and the
  header, and trigger backoff. The loop never exits on provider errors.
- Browser crash: not auto-relaunched in the MVP; a dead browser surfaces as per-parcel errors with exponential backoff, and `R` retries. Default per-poll timeout is 90 s because parcelsapp performs live carrier lookups (5–60 s observed).
- Terminal is always restored (panic hook + Drop guard).

## Testing

- `store`: round-trip TOML/JSON, missing file → empty, atomic write leaves no
  temp file.
- `app`: key → mode transitions and effects; add dedup/trim/label parsing;
  remove adjusts selection; detail scroll bounds.
- `poller`: scheduling picks earliest due, skips Delivered, backoff curve —
  driven with a `MockProvider` and `tokio::time::pause`.
- `provider::parcelsapp::parse_response`: against `tests/fixtures/…json`;
  status classification table.
- Live test `#[ignore]`: launches Chrome and tracks a known number.
- `ui`: `TestBackend` snapshot of one rendered table with mixed statuses.

## Done criteria

- `cargo test` green; `cargo clippy -- -D warnings` clean.
- `parcli` launches, shows an empty table; `a` + a real tracking number results
  in a populated, colored row within a minute; `Enter` shows events; `d y`
  removes it; restart shows the list and cached state instantly.

## Follow-up 2026-09-14 — translation and visual pass

- MyMemory translation in the poller with per-description cache persisted via `TrackEvent.translated` in `state.json`.
- `--no-translate` flag disables translation for raw carrier text display.
- `Tracking.attributes` and `tracking_url` parsed from the widget response and rendered in the detail summary card.
- Table: rounded block, Black-on-Cyan header, status pill, staleness-colored AGE (green → yellow → red), `▰▱` poll progress bar in NEXT column, zebra rows, header clock.
- Detail: summary card with aligned keys + `●`/`│` event timeline with original text beneath translations.
- parcelsapp keys on the `HeadlessChrome` UA, so the provider overrides the UA.
