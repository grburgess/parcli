# parcli MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `top`-like ratatui dashboard that tracks parcels by driving the parcelsapp.com widget in headless Chrome, with add/remove of tracking numbers and periodic re-polling.

**Architecture:** A tokio event loop (`tui.rs`) multiplexes crossterm key events, a 250 ms render tick, and `PollResult`s from a single worker task (`poller.rs`) that owns the `Provider`. All state transitions live in pure functions on `App` (`app.rs`) that return `Effect`s, so they are unit-testable without a terminal or browser. `ParcelsAppProvider` opens the widget page, types the number, clicks track, and captures the `api/v2/parcels` response body via CDP network events.

**Tech Stack:** Rust stable 1.98 · tokio 1 · ratatui 0.30 · crossterm 0.29 · tui-input 0.15 · chromiumoxide 0.9 · serde/toml/serde_json · directories 6 · clap 4 · chrono 0.4 · anyhow · async-trait

**Spec:** `docs/superpowers/specs/2026-09-13-parcli-design.md`

## Global Constraints

- Rust stable ≥ 1.80 (machine has 1.98.1 after `rustup update stable`).
- Browser executable: `PARCLI_CHROME` env var if set; otherwise chromiumoxide's built-in detection (it finds `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome` on macOS and `google-chrome`/`chromium` on PATH). Missing browser → fatal startup error that names `PARCLI_CHROME`.
- Polling is strictly sequential: at most one widget page open at a time.
- Default poll interval 10 minutes (`--interval <minutes>`); per-parcel backoff `interval * 2^failures` capped at 60 min; `Delivered` parcels are not polled.
- Files: `<config_dir>/parcels.toml`, `<cache_dir>/state.json` via `directories::ProjectDirs::from("", "", "parcli")`; written atomically (temp file + rename).
- The terminal must always be restored (panic hook + explicit restore).
- `cargo test` green and `cargo clippy --all-targets -- -D warnings` clean at every commit.
- Commit messages end with:
  ```
  Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01Lhzg4G5VB2d8MMKCooXYwu
  ```

## Verified facts used by this plan (checked live 2026-09-13)

- `https://parcelsapp.com/widget` has `<input id="track-input" placeholder="Enter tracking number">` and `<button id="track-button">Track package</button>`.
- Clicking issues `POST https://parcelsapp.com/api/v2/parcels` (type `fetch`). The request body carries an obfuscated tracking id plus a browser fingerprint string, which is why we drive a real browser instead of using `reqwest`.
- Response body for `RB123456789CN` (saved as the fixture in Task 4):
  ```json
  {"states":[{"date":"2026-04-14T09:58:50Z","carrier":0,"status":"Pending shipping by the seller"}],"carriers":["China Post","Deutsche Post - DHL Paket"],"externalTracking":[{"url":"https://global.cainiao.com/detail.htm?mailNoList=RB123456789CN","slug":"china-post","method":"GET"}],"services":[{"slug":"china-post","name":"China Post","isFinished":true},{"slug":"deutsche-post","name":"Deutsche Post - DHL Paket"}],"detected":[0],"detectedCarrier":{"name":"China Post","slug":"china-post"},"carrier":0,"checkedCountry":"Germany","checkedCountryCode":"DE","status":"archive","final_status":"This is the final status. Carrier doesn't provide further tracking updates.","probablyDelayed":true,"attributes":[]}
  ```
  `states[].carrier` is an index into `carriers`. Top-level `status` values seen in Parcels App's public API docs: `pickup`, `transit`, `arrived`, `delivered`, `archive`; treat anything else as `Unknown` and refine from the latest state text.

## File structure

```
Cargo.toml
src/main.rs            clap Args, ProjectDirs, load store, build provider, spawn poller, run tui
src/app.rs             App, Mode, Effect, handle_key(), apply_poll_result(), rows()
src/store.rs           Parcel, ParcelList, ParcelState, StateCache, load/save, atomic_write
src/provider/mod.rs    Provider trait, Status, TrackEvent, Tracking, classify_status()
src/provider/parcelsapp.rs  parse_response(), ParcelsAppProvider (chromiumoxide)
src/poller.rs          PollCommand, PollResult, Scheduler (pure), run_poller()
src/tui.rs             terminal init/restore, event loop
src/ui.rs              draw(), header/table/detail/footer/input rendering
tests/fixtures/parcelsapp_archive.json
```

---

### Task 1: Project scaffold

**Files:**
- Create: `Cargo.toml`, `src/main.rs`, `.gitignore`

**Interfaces:**
- Produces: a compiling binary crate named `parcli` with all dependencies declared; later tasks only add modules.

- [ ] **Step 1: Create the crate**

Run from `/Users/jburgess/coding/projects/parcli`:
```bash
cargo init --name parcli --vcs none
```

- [ ] **Step 2: Write Cargo.toml**

```toml
[package]
name = "parcli"
version = "0.1.0"
edition = "2021"
rust-version = "1.80"
description = "top-like terminal dashboard for international parcel tracking"

[dependencies]
anyhow = "1"
async-trait = "0.1"
chromiumoxide = "0.9"
chrono = { version = "0.4", features = ["serde"] }
clap = { version = "4", features = ["derive"] }
crossterm = { version = "0.29", features = ["event-stream"] }
directories = "6"
futures = "0.3"
ratatui = "0.30"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "sync", "signal"] }
toml = "1"
tui-input = "0.15"

[dev-dependencies]
tempfile = "3"
tokio = { version = "1", features = ["test-util"] }
```

- [ ] **Step 3: Write src/main.rs placeholder and .gitignore**

`src/main.rs`:
```rust
fn main() {
    println!("parcli");
}
```

`.gitignore`:
```
/target
.playwright-mcp/
```

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: compiles (first build downloads crates; a minute or two).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs .gitignore
git commit -m "chore: scaffold parcli crate with dependencies"
```

---

### Task 2: Provider types and status classifier

**Files:**
- Create: `src/provider/mod.rs`
- Modify: `src/main.rs` (add `mod provider;`)

**Interfaces:**
- Produces:
  ```rust
  pub enum Status { Pending, InTransit, OutForDelivery, Delivered, Exception, Unknown }
  pub struct TrackEvent { pub time: Option<DateTime<Utc>>, pub description: String, pub location: Option<String> }
  pub struct Tracking { pub number: String, pub carrier: Option<String>, pub status: Status, pub events: Vec<TrackEvent>, pub fetched_at: DateTime<Utc> }
  #[async_trait] pub trait Provider: Send + Sync { async fn track(&self, number: &str) -> anyhow::Result<Tracking>; }
  pub fn classify_status(api_status: &str, latest_event: Option<&str>) -> Status
  impl Status { pub fn label(&self) -> &'static str }
  ```

- [ ] **Step 1: Write the failing tests**

`src/provider/mod.rs`:
```rust
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Pending,
    InTransit,
    OutForDelivery,
    Delivered,
    Exception,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackEvent {
    pub time: Option<DateTime<Utc>>,
    pub description: String,
    pub location: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tracking {
    pub number: String,
    pub carrier: Option<String>,
    pub status: Status,
    pub events: Vec<TrackEvent>,
    pub fetched_at: DateTime<Utc>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn track(&self, number: &str) -> anyhow::Result<Tracking>;
}

pub mod parcelsapp;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_api_status_values() {
        assert_eq!(classify_status("delivered", None), Status::Delivered);
        assert_eq!(classify_status("transit", None), Status::InTransit);
        assert_eq!(classify_status("arrived", None), Status::OutForDelivery);
        assert_eq!(classify_status("pickup", None), Status::InTransit);
        assert_eq!(classify_status("archive", None), Status::Unknown);
    }

    #[test]
    fn falls_back_to_latest_event_text() {
        assert_eq!(classify_status("archive", Some("Pending shipping by the seller")), Status::Pending);
        assert_eq!(classify_status("", Some("Out for delivery")), Status::OutForDelivery);
        assert_eq!(classify_status("", Some("Delivered to mailbox")), Status::Delivered);
        assert_eq!(classify_status("", Some("Held at customs")), Status::Exception);
        assert_eq!(classify_status("", Some("Return to sender")), Status::Exception);
        assert_eq!(classify_status("", Some("Departed facility")), Status::InTransit);
        assert_eq!(classify_status("", Some("something odd")), Status::Unknown);
        assert_eq!(classify_status("", None), Status::Unknown);
    }

    #[test]
    fn api_status_wins_over_event_text() {
        assert_eq!(classify_status("delivered", Some("Out for delivery")), Status::Delivered);
    }

    #[test]
    fn labels() {
        assert_eq!(Status::OutForDelivery.label(), "out for delivery");
        assert_eq!(Status::InTransit.label(), "in transit");
    }
}
```

Add to `src/main.rs` above `fn main`: `mod provider;` and create an empty `src/provider/parcelsapp.rs` (Task 4 fills it).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test provider`
Expected: compile error `cannot find function classify_status`.

- [ ] **Step 3: Implement**

Add to `src/provider/mod.rs` above `pub mod parcelsapp;`:
```rust
impl Status {
    pub fn label(&self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::InTransit => "in transit",
            Status::OutForDelivery => "out for delivery",
            Status::Delivered => "delivered",
            Status::Exception => "exception",
            Status::Unknown => "unknown",
        }
    }
}

/// Map parcelsapp's top-level `status` plus the newest event text onto `Status`.
/// The API value is authoritative when it is one of the documented values;
/// otherwise keywords in the latest event decide, defaulting to `Unknown`.
pub fn classify_status(api_status: &str, latest_event: Option<&str>) -> Status {
    match api_status {
        "delivered" => return Status::Delivered,
        "arrived" => return Status::OutForDelivery,
        "transit" | "pickup" => return Status::InTransit,
        _ => {}
    }
    let text = match latest_event {
        Some(t) => t.to_ascii_lowercase(),
        None => return Status::Unknown,
    };
    let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
    if has(&["delivered"]) {
        Status::Delivered
    } else if has(&["out for delivery", "with courier", "with delivery courier"]) {
        Status::OutForDelivery
    } else if has(&["customs", "return", "exception", "failed", "unsuccessful", "refused", "damaged", "lost"]) {
        Status::Exception
    } else if has(&["pending", "waiting", "label created", "shipment information received", "pre-advice"]) {
        Status::Pending
    } else if has(&["departed", "arrived", "in transit", "processed", "accepted", "posted", "received", "dispatched", "handed", "sorting", "left"]) {
        Status::InTransit
    } else {
        Status::Unknown
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test provider`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/provider
git commit -m "feat(provider): tracking types and status classifier"
```

---

### Task 3: Store — parcel list and state cache

**Files:**
- Create: `src/store.rs`
- Modify: `src/main.rs` (add `mod store;`)

**Interfaces:**
- Consumes: `provider::Tracking`.
- Produces:
  ```rust
  pub struct Parcel { pub number: String, pub label: Option<String>, pub added: DateTime<Utc> }
  pub struct ParcelList { pub parcels: Vec<Parcel> }
  impl ParcelList { pub fn load(path: &Path) -> Result<Self>; pub fn save(&self, path: &Path) -> Result<()>;
                    pub fn add(&mut self, number: &str, label: Option<&str>, now: DateTime<Utc>) -> bool;
                    pub fn remove(&mut self, number: &str) -> bool; pub fn contains(&self, number: &str) -> bool }
  pub struct ParcelState { pub tracking: Option<Tracking>, pub last_error: Option<String>, pub failures: u32, pub next_poll: DateTime<Utc> }
  pub struct StateCache { pub by_number: HashMap<String, ParcelState> }
  impl StateCache { pub fn load(path: &Path) -> Result<Self>; pub fn save(&self, path: &Path) -> Result<()> }
  pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()>
  pub struct Paths { pub parcels: PathBuf, pub state: PathBuf }
  impl Paths { pub fn discover() -> Result<Paths> }
  ```

- [ ] **Step 1: Write the failing tests**

`src/store.rs`:
```rust
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::provider::Tracking;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parcel {
    pub number: String,
    pub label: Option<String>,
    pub added: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParcelList {
    #[serde(default)]
    pub parcels: Vec<Parcel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParcelState {
    pub tracking: Option<Tracking>,
    pub last_error: Option<String>,
    pub failures: u32,
    pub next_poll: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateCache {
    #[serde(default)]
    pub by_number: HashMap<String, ParcelState>,
}

pub struct Paths {
    pub parcels: PathBuf,
    pub state: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Status;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap()
    }

    #[test]
    fn missing_files_load_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let list = ParcelList::load(&dir.path().join("parcels.toml")).unwrap();
        assert!(list.parcels.is_empty());
        let cache = StateCache::load(&dir.path().join("state.json")).unwrap();
        assert!(cache.by_number.is_empty());
    }

    #[test]
    fn parcel_list_round_trips_and_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("parcels.toml");
        let mut list = ParcelList::default();
        assert!(list.add("RB123456789CN", Some("camera"), now()));
        assert!(!list.add(" RB123456789CN ", None, now()), "dup is rejected");
        list.save(&path).unwrap();
        let loaded = ParcelList::load(&path).unwrap();
        assert_eq!(loaded, list);
        assert_eq!(loaded.parcels[0].label.as_deref(), Some("camera"));
        assert!(!dir.path().join("nested").read_dir().unwrap().any(|e| {
            e.unwrap().file_name().to_string_lossy().contains(".tmp")
        }));
    }

    #[test]
    fn add_trims_uppercases_and_rejects_duplicates_and_empty() {
        let mut list = ParcelList::default();
        assert!(list.add("  rb123456789cn ", None, now()));
        assert_eq!(list.parcels[0].number, "RB123456789CN");
        assert!(!list.add("RB123456789CN", None, now()));
        assert!(!list.add("   ", None, now()));
        assert_eq!(list.parcels.len(), 1);
        assert!(list.contains("RB123456789CN"));
    }

    #[test]
    fn remove_returns_whether_present() {
        let mut list = ParcelList::default();
        list.add("A1", None, now());
        assert!(list.remove("A1"));
        assert!(!list.remove("A1"));
        assert!(list.parcels.is_empty());
    }

    #[test]
    fn state_cache_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::default();
        cache.by_number.insert(
            "A1".into(),
            ParcelState {
                tracking: Some(Tracking {
                    number: "A1".into(),
                    carrier: Some("China Post".into()),
                    status: Status::InTransit,
                    events: vec![],
                    fetched_at: now(),
                }),
                last_error: None,
                failures: 0,
                next_poll: now(),
            },
        );
        cache.save(&path).unwrap();
        assert_eq!(StateCache::load(&path).unwrap(), cache);
    }

    #[test]
    fn corrupt_file_is_an_error_naming_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.toml");
        fs::write(&path, "this = [not valid").unwrap();
        let err = ParcelList::load(&path).unwrap_err().to_string();
        assert!(err.contains("parcels.toml"), "{err}");
    }
}
```

Add `mod store;` to `src/main.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test store`
Expected: compile errors for missing `load`/`save`/`add`/`remove`/`contains`.

- [ ] **Step 3: Implement**

Insert into `src/store.rs` before `#[cfg(test)]`:
```rust
/// Write `bytes` to `path` via a sibling temp file and rename, so a crash
/// mid-write never leaves a truncated file. Creates parent directories.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let file_name = path.file_name().context("path has no file name")?.to_string_lossy();
    let tmp = parent.join(format!(".{file_name}.tmp"));
    fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

fn read_if_exists(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn normalize(number: &str) -> String {
    number.trim().to_ascii_uppercase()
}

impl ParcelList {
    pub fn load(path: &Path) -> Result<Self> {
        match read_if_exists(path)? {
            None => Ok(Self::default()),
            Some(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serializing parcel list")?;
        atomic_write(path, text.as_bytes())
    }

    /// Returns false (and changes nothing) for an empty number or a duplicate.
    pub fn add(&mut self, number: &str, label: Option<&str>, now: DateTime<Utc>) -> bool {
        let number = normalize(number);
        if number.is_empty() || self.contains(&number) {
            return false;
        }
        let label = label.map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned);
        self.parcels.push(Parcel { number, label, added: now });
        true
    }

    pub fn remove(&mut self, number: &str) -> bool {
        let before = self.parcels.len();
        self.parcels.retain(|p| p.number != number);
        self.parcels.len() != before
    }

    pub fn contains(&self, number: &str) -> bool {
        self.parcels.iter().any(|p| p.number == number)
    }
}

impl StateCache {
    pub fn load(path: &Path) -> Result<Self> {
        match read_if_exists(path)? {
            None => Ok(Self::default()),
            Some(text) => serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self).context("serializing state cache")?;
        atomic_write(path, text.as_bytes())
    }
}

impl Paths {
    pub fn discover() -> Result<Paths> {
        let dirs = directories::ProjectDirs::from("", "", "parcli")
            .context("could not determine a home directory for config files")?;
        Ok(Paths {
            parcels: dirs.config_dir().join("parcels.toml"),
            state: dirs.cache_dir().join("state.json"),
        })
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test store`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```bash
git add src/store.rs src/main.rs
git commit -m "feat(store): TOML parcel list and JSON state cache with atomic writes"
```

---

### Task 4: parcelsapp response parser

**Files:**
- Create: `tests/fixtures/parcelsapp_archive.json`
- Modify: `src/provider/parcelsapp.rs`

**Interfaces:**
- Consumes: `Tracking`, `TrackEvent`, `classify_status` from Task 2.
- Produces: `pub fn parse_response(body: &str, number: &str, fetched_at: DateTime<Utc>) -> anyhow::Result<Tracking>`

- [ ] **Step 1: Save the fixture**

`tests/fixtures/parcelsapp_archive.json` — exactly the JSON from "Verified facts" above (single line is fine).

- [ ] **Step 2: Write the failing tests**

`src/provider/parcelsapp.rs`:
```rust
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{classify_status, TrackEvent, Tracking};

#[derive(Deserialize)]
struct ApiResponse {
    #[serde(default)]
    states: Vec<ApiState>,
    #[serde(default)]
    carriers: Vec<String>,
    #[serde(rename = "detectedCarrier")]
    detected_carrier: Option<ApiCarrier>,
    #[serde(default)]
    status: String,
    error: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct ApiState {
    date: Option<String>,
    carrier: Option<usize>,
    #[serde(default)]
    status: String,
    location: Option<String>,
}

#[derive(Deserialize)]
struct ApiCarrier {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Status;
    use chrono::TimeZone;

    const FIXTURE: &str = include_str!("../../tests/fixtures/parcelsapp_archive.json");

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap()
    }

    #[test]
    fn parses_archive_fixture() {
        let t = parse_response(FIXTURE, "RB123456789CN", now()).unwrap();
        assert_eq!(t.number, "RB123456789CN");
        assert_eq!(t.carrier.as_deref(), Some("China Post"));
        assert_eq!(t.status, Status::Pending);
        assert_eq!(t.events.len(), 1);
        assert_eq!(t.events[0].description, "Pending shipping by the seller");
        assert_eq!(t.events[0].time, Some(Utc.with_ymd_and_hms(2026, 4, 14, 9, 58, 50).unwrap()));
        assert_eq!(t.events[0].location, None);
        assert_eq!(t.fetched_at, now());
    }

    #[test]
    fn events_are_newest_first_and_carry_carrier_when_no_detected_carrier() {
        let body = r#"{"states":[
            {"date":"2026-09-01T08:00:00Z","carrier":1,"status":"Posted","location":"Shenzhen"},
            {"date":"2026-09-03T10:00:00Z","carrier":0,"status":"Delivered","location":"Berlin"}],
            "carriers":["DHL","China Post"],"status":"delivered"}"#;
        let t = parse_response(body, "X", now()).unwrap();
        assert_eq!(t.status, Status::Delivered);
        assert_eq!(t.events[0].description, "Delivered");
        assert_eq!(t.events[0].location.as_deref(), Some("Berlin"));
        assert_eq!(t.events[1].description, "Posted");
        assert_eq!(t.carrier.as_deref(), Some("DHL"), "carrier of the newest event");
    }

    #[test]
    fn empty_states_gives_unknown_status_and_no_events() {
        let t = parse_response(r#"{"states":[],"carriers":[]}"#, "X", now()).unwrap();
        assert_eq!(t.status, Status::Unknown);
        assert!(t.events.is_empty());
        assert_eq!(t.carrier, None);
    }

    #[test]
    fn api_error_field_is_an_error() {
        let err = parse_response(r#"{"error":"Too many requests"}"#, "X", now()).unwrap_err();
        assert!(err.to_string().contains("Too many requests"));
    }

    #[test]
    fn invalid_json_is_an_error() {
        assert!(parse_response("<html>", "X", now()).is_err());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test parcelsapp`
Expected: compile error `cannot find function parse_response`.

- [ ] **Step 4: Implement**

Insert into `src/provider/parcelsapp.rs` before `#[cfg(test)]`:
```rust
/// Parse the body of parcelsapp's `POST /api/v2/parcels` response.
pub fn parse_response(body: &str, number: &str, fetched_at: DateTime<Utc>) -> Result<Tracking> {
    let api: ApiResponse = serde_json::from_str(body).context("parcelsapp response is not JSON")?;
    if let Some(msg) = api.error.or(api.message).filter(|m| !m.is_empty()) {
        anyhow::bail!("parcelsapp error: {msg}");
    }

    let mut events: Vec<(Option<usize>, TrackEvent)> = api
        .states
        .iter()
        .map(|s| {
            let time = s
                .date
                .as_deref()
                .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                .map(|d| d.with_timezone(&Utc));
            (
                s.carrier,
                TrackEvent {
                    time,
                    description: s.status.trim().to_owned(),
                    location: s.location.clone().filter(|l| !l.trim().is_empty()),
                },
            )
        })
        .collect();
    // Newest first; events without a time sink to the bottom.
    events.sort_by(|a, b| b.1.time.cmp(&a.1.time));

    let carrier = api
        .detected_carrier
        .map(|c| c.name)
        .or_else(|| events.first().and_then(|(idx, _)| idx.and_then(|i| api.carriers.get(i).cloned())));

    let latest = events.first().map(|(_, e)| e.description.as_str());
    let status = classify_status(&api.status, latest);

    Ok(Tracking {
        number: number.to_owned(),
        carrier,
        status,
        events: events.into_iter().map(|(_, e)| e).collect(),
        fetched_at,
    })
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test parcelsapp`
Expected: 5 passed.

- [ ] **Step 6: Commit**

```bash
git add tests/fixtures/parcelsapp_archive.json src/provider/parcelsapp.rs
git commit -m "feat(provider): parse parcelsapp api/v2/parcels responses"
```

---

### Task 5: ParcelsAppProvider — headless Chrome driver

**Files:**
- Modify: `src/provider/parcelsapp.rs`

**Interfaces:**
- Consumes: `parse_response` (Task 4), `Provider` trait (Task 2).
- Produces:
  ```rust
  pub struct ParcelsAppProvider { .. }
  impl ParcelsAppProvider { pub async fn launch(timeout: Duration) -> anyhow::Result<Self>; pub async fn close(self) }
  #[async_trait] impl Provider for ParcelsAppProvider
  ```

- [ ] **Step 1: Write the ignored live test**

Append to the `tests` module in `src/provider/parcelsapp.rs`:
```rust
    /// Requires Chrome and network. Run with: cargo test live_ -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_tracks_archive_number() {
        let provider = ParcelsAppProvider::launch(std::time::Duration::from_secs(45)).await.unwrap();
        let t = provider.track("RB123456789CN").await.unwrap();
        provider.close().await;
        assert_eq!(t.number, "RB123456789CN");
        assert!(!t.events.is_empty(), "expected at least one event, got {t:?}");
    }
```

- [ ] **Step 2: Run to verify it fails to compile**

Run: `cargo test parcelsapp`
Expected: compile error `cannot find struct ParcelsAppProvider`.

- [ ] **Step 3: Implement the provider**

Add imports at the top of `src/provider/parcelsapp.rs`:
```rust
use std::time::Duration;

use async_trait::async_trait;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::{
    EventLoadingFinished, EventResponseReceived, GetResponseBodyParams,
};
use futures::StreamExt;
use tokio::task::JoinHandle;

use super::Provider;
```

Insert before `#[cfg(test)]`:
```rust
const WIDGET_URL: &str = "https://parcelsapp.com/widget";
const API_PATH: &str = "api/v2/parcels";
/// After the first API response, keep listening this long for a later one
/// (the widget sometimes re-queries once carriers respond).
const SETTLE: Duration = Duration::from_secs(3);

pub struct ParcelsAppProvider {
    browser: Browser,
    handler: JoinHandle<()>,
    timeout: Duration,
}

impl ParcelsAppProvider {
    /// Launch one headless browser. Honors `PARCLI_CHROME`; otherwise uses
    /// chromiumoxide's executable detection.
    pub async fn launch(timeout: Duration) -> Result<Self> {
        let mut builder = BrowserConfig::builder().request_timeout(timeout);
        if let Ok(path) = std::env::var("PARCLI_CHROME") {
            builder = builder.chrome_executable(path);
        }
        let config = builder.build().map_err(|e| {
            anyhow::anyhow!("{e}. Set PARCLI_CHROME to your Chrome/Chromium executable.")
        })?;
        let (browser, mut events) = Browser::launch(config).await.map_err(|e| {
            anyhow::anyhow!("launching Chrome failed: {e}. Set PARCLI_CHROME to your Chrome/Chromium executable.")
        })?;
        let handler = tokio::spawn(async move { while events.next().await.is_some() {} });
        Ok(Self { browser, handler, timeout })
    }

    pub async fn close(mut self) {
        let _ = self.browser.close().await;
        let _ = self.browser.wait().await;
        self.handler.abort();
    }

    async fn track_inner(&self, number: &str) -> Result<Tracking> {
        let page = self.browser.new_page(WIDGET_URL).await.context("opening widget page")?;
        let result = async {
            let mut responses = page.event_listener::<EventResponseReceived>().await?;
            let mut finished = page.event_listener::<EventLoadingFinished>().await?;
            page.wait_for_navigation().await.context("loading widget page")?;

            page.find_element("#track-input").await.context("#track-input not found")?
                .click().await?
                .type_str(number).await?;
            page.find_element("#track-button").await.context("#track-button not found")?
                .click().await?;

            let mut pending: Vec<String> = Vec::new();
            let mut body: Option<String> = None;
            let settle = tokio::time::sleep(Duration::from_secs(3600)); // reset once the first body arrives
            tokio::pin!(settle);
            loop {
                tokio::select! {
                    Some(ev) = responses.next() => {
                        if ev.response.url.contains(API_PATH) {
                            pending.push(ev.request_id.inner().clone());
                        }
                    }
                    Some(ev) = finished.next() => {
                        let id = ev.request_id.inner().clone();
                        if pending.contains(&id) {
                            let got = page.execute(GetResponseBodyParams::new(id)).await?;
                            body = Some(got.result.body.clone());
                            settle.as_mut().reset(tokio::time::Instant::now() + SETTLE);
                        }
                    }
                    () = &mut settle => break,
                }
            }
            let body = body.context("widget produced no api/v2/parcels response")?;
            parse_response(&body, number, Utc::now())
        };
        let outcome = tokio::time::timeout(self.timeout, result)
            .await
            .map_err(|_| anyhow::anyhow!("timed out after {:?} waiting for parcelsapp", self.timeout))
            .and_then(|r| r);
        let _ = page.close().await;
        outcome
    }
}

#[async_trait]
impl Provider for ParcelsAppProvider {
    async fn track(&self, number: &str) -> Result<Tracking> {
        self.track_inner(number).await
    }
}
```

Notes for the implementer:
- `ev.request_id.inner()` returns `&String` on chromiumoxide's `RequestId`; if it does not compile, use `ev.request_id.as_ref().to_string()` — check `chromiumoxide_cdp` `RequestId` in `~/.cargo/registry/src/*/chromiumoxide_cdp-0.9.1/src/cdp.rs`.
- `page.execute(..)` returns `CommandResponse<GetResponseBodyReturns>`; the body is `.result.body`.

- [ ] **Step 4: Compile and run the live test**

Run: `cargo test parcelsapp` (unit tests still pass, live test ignored), then
`cargo test live_ -- --ignored --nocapture`
Expected: PASS within ~20 s, printing nothing unusual. If it fails with a missing-Chrome error, set `PARCLI_CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"`.

- [ ] **Step 5: Clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: clean (add `#[allow(dead_code)]` only where a later task will consume the item, and remove it then).

- [ ] **Step 6: Commit**

```bash
git add src/provider/parcelsapp.rs
git commit -m "feat(provider): drive parcelsapp widget in headless Chrome via CDP"
```

---

### Task 6: Poller — scheduler and worker task

**Files:**
- Create: `src/poller.rs`
- Modify: `src/main.rs` (add `mod poller;`)

**Interfaces:**
- Consumes: `Provider`, `Tracking`, `Status`.
- Produces:
  ```rust
  pub enum PollCommand { Add(String), Remove(String), Refresh(String), RefreshAll, Shutdown }
  pub struct PollResult { pub number: String, pub result: Result<Tracking, String> }
  pub struct Scheduler { .. }
  impl Scheduler {
      pub fn new(interval: Duration, numbers: Vec<String>, now: DateTime<Utc>) -> Self;
      pub fn add(&mut self, number: &str, now: DateTime<Utc>);
      pub fn remove(&mut self, number: &str);
      pub fn refresh(&mut self, number: &str, now: DateTime<Utc>);
      pub fn refresh_all(&mut self, now: DateTime<Utc>);
      pub fn next_due(&self, now: DateTime<Utc>) -> Option<String>;
      pub fn record_success(&mut self, number: &str, status: Status, now: DateTime<Utc>);
      pub fn record_failure(&mut self, number: &str, now: DateTime<Utc>);
      pub fn next_poll(&self, number: &str) -> Option<DateTime<Utc>>;
  }
  pub async fn run_poller(provider: Arc<dyn Provider>, mut scheduler: Scheduler,
                          mut commands: mpsc::Receiver<PollCommand>, results: mpsc::Sender<PollResult>)
  ```

- [ ] **Step 1: Write the failing tests**

`src/poller.rs`:
```rust
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;

use crate::provider::{Provider, Status, Tracking};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollCommand {
    Add(String),
    Remove(String),
    Refresh(String),
    RefreshAll,
    Shutdown,
}

#[derive(Debug)]
pub struct PollResult {
    pub number: String,
    pub result: Result<Tracking, String>,
}

#[derive(Debug, Clone)]
struct Entry {
    next_poll: DateTime<Utc>,
    failures: u32,
    delivered: bool,
}

#[derive(Debug, Clone)]
pub struct Scheduler {
    interval: Duration,
    entries: BTreeMap<String, Entry>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::TrackEvent;
    use async_trait::async_trait;
    use chrono::TimeZone;
    use std::sync::Mutex;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap()
    }
    fn mins(m: i64) -> chrono::Duration {
        chrono::Duration::minutes(m)
    }

    #[test]
    fn new_parcels_are_due_immediately() {
        let s = Scheduler::new(Duration::from_secs(600), vec!["B".into(), "A".into()], t0());
        assert_eq!(s.next_due(t0()), Some("A".into()), "ties resolve alphabetically");
    }

    #[test]
    fn success_schedules_after_interval_and_picks_earliest() {
        let mut s = Scheduler::new(Duration::from_secs(600), vec!["A".into(), "B".into()], t0());
        s.record_success("A", Status::InTransit, t0());
        assert_eq!(s.next_due(t0()), Some("B".into()));
        s.record_success("B", Status::InTransit, t0() + mins(1));
        assert_eq!(s.next_due(t0() + mins(5)), None);
        assert_eq!(s.next_due(t0() + mins(10)), Some("A".into()));
        assert_eq!(s.next_poll("A"), Some(t0() + mins(10)));
    }

    #[test]
    fn delivered_parcels_are_never_due() {
        let mut s = Scheduler::new(Duration::from_secs(600), vec!["A".into()], t0());
        s.record_success("A", Status::Delivered, t0());
        assert_eq!(s.next_due(t0() + mins(600)), None);
        s.refresh("A", t0() + mins(600));
        assert_eq!(s.next_due(t0() + mins(600)), Some("A".into()), "explicit refresh still works");
    }

    #[test]
    fn failures_back_off_exponentially_capped_at_an_hour() {
        let mut s = Scheduler::new(Duration::from_secs(600), vec!["A".into()], t0());
        s.record_failure("A", t0());
        assert_eq!(s.next_poll("A"), Some(t0() + mins(20)));
        s.record_failure("A", t0());
        assert_eq!(s.next_poll("A"), Some(t0() + mins(40)));
        s.record_failure("A", t0());
        assert_eq!(s.next_poll("A"), Some(t0() + mins(60)));
        s.record_failure("A", t0());
        assert_eq!(s.next_poll("A"), Some(t0() + mins(60)));
        s.record_success("A", Status::InTransit, t0());
        assert_eq!(s.next_poll("A"), Some(t0() + mins(10)), "success resets backoff");
    }

    #[test]
    fn add_remove_refresh_all() {
        let mut s = Scheduler::new(Duration::from_secs(600), vec![], t0());
        assert_eq!(s.next_due(t0()), None);
        s.add("A", t0());
        s.record_success("A", Status::InTransit, t0());
        s.refresh_all(t0() + mins(1));
        assert_eq!(s.next_due(t0() + mins(1)), Some("A".into()));
        s.remove("A");
        assert_eq!(s.next_due(t0() + mins(1)), None);
        assert_eq!(s.next_poll("A"), None);
    }

    struct MockProvider {
        calls: Mutex<Vec<String>>,
        fail: bool,
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn track(&self, number: &str) -> anyhow::Result<Tracking> {
            self.calls.lock().unwrap().push(number.to_owned());
            if self.fail {
                anyhow::bail!("boom");
            }
            Ok(Tracking {
                number: number.to_owned(),
                carrier: None,
                status: Status::InTransit,
                events: vec![TrackEvent { time: None, description: "moving".into(), location: None }],
                fetched_at: Utc::now(),
            })
        }
    }

    #[tokio::test]
    async fn worker_polls_due_parcels_and_reports_results() {
        let provider = Arc::new(MockProvider { calls: Mutex::new(vec![]), fail: false });
        let scheduler = Scheduler::new(Duration::from_secs(600), vec!["A".into()], Utc::now());
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (res_tx, mut res_rx) = mpsc::channel(8);
        let worker = tokio::spawn(run_poller(provider.clone(), scheduler, cmd_rx, res_tx));

        let first = res_rx.recv().await.unwrap();
        assert_eq!(first.number, "A");
        assert!(first.result.is_ok());

        cmd_tx.send(PollCommand::Add("B".into())).await.unwrap();
        let second = res_rx.recv().await.unwrap();
        assert_eq!(second.number, "B");

        cmd_tx.send(PollCommand::Shutdown).await.unwrap();
        worker.await.unwrap();
        assert_eq!(*provider.calls.lock().unwrap(), vec!["A".to_string(), "B".to_string()]);
    }

    #[tokio::test]
    async fn worker_reports_errors_as_strings() {
        let provider = Arc::new(MockProvider { calls: Mutex::new(vec![]), fail: true });
        let scheduler = Scheduler::new(Duration::from_secs(600), vec!["A".into()], Utc::now());
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (res_tx, mut res_rx) = mpsc::channel(8);
        let worker = tokio::spawn(run_poller(provider, scheduler, cmd_rx, res_tx));
        let r = res_rx.recv().await.unwrap();
        assert_eq!(r.result.unwrap_err(), "boom");
        cmd_tx.send(PollCommand::Shutdown).await.unwrap();
        worker.await.unwrap();
    }
}
```

Add `mod poller;` to `src/main.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test poller`
Expected: compile errors for missing `Scheduler` methods and `run_poller`.

- [ ] **Step 3: Implement**

Insert into `src/poller.rs` before `#[cfg(test)]`:
```rust
const MAX_BACKOFF: Duration = Duration::from_secs(3600);

impl Scheduler {
    pub fn new(interval: Duration, numbers: Vec<String>, now: DateTime<Utc>) -> Self {
        let mut s = Self { interval, entries: BTreeMap::new() };
        for n in numbers {
            s.add(&n, now);
        }
        s
    }

    pub fn add(&mut self, number: &str, now: DateTime<Utc>) {
        self.entries.entry(number.to_owned()).or_insert(Entry { next_poll: now, failures: 0, delivered: false });
    }

    pub fn remove(&mut self, number: &str) {
        self.entries.remove(number);
    }

    pub fn refresh(&mut self, number: &str, now: DateTime<Utc>) {
        if let Some(e) = self.entries.get_mut(number) {
            e.next_poll = now;
            e.delivered = false;
        }
    }

    pub fn refresh_all(&mut self, now: DateTime<Utc>) {
        for e in self.entries.values_mut() {
            e.next_poll = now;
            e.delivered = false;
        }
    }

    /// The due parcel with the earliest `next_poll` (ties: insertion order is
    /// not tracked, so alphabetical), skipping delivered ones.
    pub fn next_due(&self, now: DateTime<Utc>) -> Option<String> {
        self.entries
            .iter()
            .filter(|(_, e)| !e.delivered && e.next_poll <= now)
            .min_by_key(|(_, e)| e.next_poll)
            .map(|(n, _)| n.clone())
    }

    pub fn record_success(&mut self, number: &str, status: Status, now: DateTime<Utc>) {
        if let Some(e) = self.entries.get_mut(number) {
            e.failures = 0;
            e.delivered = status == Status::Delivered;
            e.next_poll = now + chrono::Duration::from_std(self.interval).unwrap_or_else(|_| chrono::Duration::zero());
        }
    }

    pub fn record_failure(&mut self, number: &str, now: DateTime<Utc>) {
        if let Some(e) = self.entries.get_mut(number) {
            e.failures = e.failures.saturating_add(1);
            let factor = 2u32.saturating_pow(e.failures.min(16));
            let delay = self.interval.saturating_mul(factor).min(MAX_BACKOFF);
            e.next_poll = now + chrono::Duration::from_std(delay).unwrap_or_else(|_| chrono::Duration::zero());
        }
    }

    pub fn next_poll(&self, number: &str) -> Option<DateTime<Utc>> {
        self.entries.get(number).map(|e| e.next_poll)
    }
}

/// Worker: applies commands, polls one due parcel at a time, reports results.
pub async fn run_poller(
    provider: Arc<dyn Provider>,
    mut scheduler: Scheduler,
    mut commands: mpsc::Receiver<PollCommand>,
    results: mpsc::Sender<PollResult>,
) {
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            cmd = commands.recv() => match cmd {
                None | Some(PollCommand::Shutdown) => return,
                Some(PollCommand::Add(n)) => scheduler.add(&n, Utc::now()),
                Some(PollCommand::Remove(n)) => scheduler.remove(&n),
                Some(PollCommand::Refresh(n)) => scheduler.refresh(&n, Utc::now()),
                Some(PollCommand::RefreshAll) => scheduler.refresh_all(Utc::now()),
            },
            _ = tick.tick() => {
                let Some(number) = scheduler.next_due(Utc::now()) else { continue };
                let result = provider.track(&number).await.map_err(|e| e.to_string());
                match &result {
                    Ok(t) => scheduler.record_success(&number, t.status, Utc::now()),
                    Err(_) => scheduler.record_failure(&number, Utc::now()),
                }
                if results.send(PollResult { number, result }).await.is_err() {
                    return;
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test poller`
Expected: 7 passed.

- [ ] **Step 5: Commit**

```bash
git add src/poller.rs src/main.rs
git commit -m "feat(poller): scheduler with backoff and sequential polling worker"
```

---

### Task 7: App state machine

**Files:**
- Create: `src/app.rs`
- Modify: `src/main.rs` (add `mod app;`)

**Interfaces:**
- Consumes: `ParcelList`, `StateCache`, `ParcelState` (Task 3); `PollCommand`, `PollResult` (Task 6); `Tracking`, `Status`.
- Produces:
  ```rust
  pub enum Mode { Normal, Adding(tui_input::Input), ConfirmRemove, Detail { scroll: u16 } }
  pub enum Effect { SaveParcels, SaveState, Send(PollCommand), Quit }
  pub struct App { pub parcels: ParcelList, pub cache: StateCache, pub mode: Mode, pub selected: usize,
                   pub polling: Option<String>, pub last_error: Option<String>, pub interval: Duration }
  impl App {
      pub fn new(parcels: ParcelList, cache: StateCache, interval: Duration) -> Self;
      pub fn handle_key(&mut self, key: KeyEvent, now: DateTime<Utc>) -> Vec<Effect>;
      pub fn apply_poll_result(&mut self, r: PollResult, now: DateTime<Utc>) -> Vec<Effect>;
      pub fn selected_parcel(&self) -> Option<&Parcel>;
      pub fn state_for(&self, number: &str) -> Option<&ParcelState>;
  }
  pub fn split_number_and_label(input: &str) -> (String, Option<String>)
  ```
  `polling` is set by `tui.rs` when it forwards a `Refresh`, and cleared in `apply_poll_result`; `App` also sets it optimistically for the number it just asked to refresh/add.

- [ ] **Step 1: Write the failing tests**

`src/app.rs`:
```rust
use std::time::Duration;

use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::poller::{PollCommand, PollResult};
use crate::store::{Parcel, ParcelList, ParcelState, StateCache};

#[derive(Debug)]
pub enum Mode {
    Normal,
    Adding(Input),
    ConfirmRemove,
    Detail { scroll: u16 },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    SaveParcels,
    SaveState,
    Send(PollCommand),
    Quit,
}

pub struct App {
    pub parcels: ParcelList,
    pub cache: StateCache,
    pub mode: Mode,
    pub selected: usize,
    pub polling: Option<String>,
    pub last_error: Option<String>,
    pub interval: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Status, TrackEvent, Tracking};
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap()
    }
    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn code(k: KeyCode) -> KeyEvent {
        KeyEvent::new(k, KeyModifiers::NONE)
    }
    fn app_with(numbers: &[&str]) -> App {
        let mut list = ParcelList::default();
        for n in numbers {
            list.add(n, None, now());
        }
        App::new(list, StateCache::default(), Duration::from_secs(600))
    }
    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle_key(key(c), now());
        }
    }

    #[test]
    fn q_quits_in_normal_mode() {
        let mut app = app_with(&[]);
        assert_eq!(app.handle_key(key('q'), now()), vec![Effect::Quit]);
    }

    #[test]
    fn navigation_clamps() {
        let mut app = app_with(&["A", "B", "C"]);
        app.handle_key(key('k'), now());
        assert_eq!(app.selected, 0);
        app.handle_key(key('j'), now());
        app.handle_key(code(KeyCode::Down), now());
        app.handle_key(key('j'), now());
        assert_eq!(app.selected, 2);
        app.handle_key(code(KeyCode::Up), now());
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn add_flow_saves_and_polls() {
        let mut app = app_with(&[]);
        assert!(app.handle_key(key('a'), now()).is_empty());
        assert!(matches!(app.mode, Mode::Adding(_)));
        type_str(&mut app, "rb123456789cn camera body");
        let effects = app.handle_key(code(KeyCode::Enter), now());
        assert_eq!(effects, vec![Effect::SaveParcels, Effect::Send(PollCommand::Add("RB123456789CN".into()))]);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.parcels.parcels[0].number, "RB123456789CN");
        assert_eq!(app.parcels.parcels[0].label.as_deref(), Some("camera body"));
        assert_eq!(app.selected, 0);
        assert_eq!(app.polling.as_deref(), Some("RB123456789CN"));
    }

    #[test]
    fn add_rejects_empty_and_duplicate_without_effects() {
        let mut app = app_with(&["A"]);
        app.handle_key(key('a'), now());
        assert!(app.handle_key(code(KeyCode::Enter), now()).is_empty());
        assert!(matches!(app.mode, Mode::Normal));
        app.handle_key(key('a'), now());
        type_str(&mut app, "a");
        assert!(app.handle_key(code(KeyCode::Enter), now()).is_empty());
        assert_eq!(app.parcels.parcels.len(), 1);
        assert!(app.last_error.as_deref().unwrap().contains("already"));
    }

    #[test]
    fn esc_cancels_add() {
        let mut app = app_with(&[]);
        app.handle_key(key('a'), now());
        type_str(&mut app, "xyz");
        assert!(app.handle_key(code(KeyCode::Esc), now()).is_empty());
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.parcels.parcels.is_empty());
    }

    #[test]
    fn remove_flow_confirms_then_removes_and_fixes_selection() {
        let mut app = app_with(&["A", "B"]);
        app.handle_key(key('j'), now());
        app.handle_key(key('d'), now());
        assert!(matches!(app.mode, Mode::ConfirmRemove));
        assert!(app.handle_key(key('n'), now()).is_empty());
        assert_eq!(app.parcels.parcels.len(), 2);
        app.handle_key(key('d'), now());
        let effects = app.handle_key(key('y'), now());
        assert_eq!(effects, vec![Effect::SaveParcels, Effect::SaveState, Effect::Send(PollCommand::Remove("B".into()))]);
        assert_eq!(app.parcels.parcels.len(), 1);
        assert_eq!(app.selected, 0);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn d_on_empty_list_does_nothing() {
        let mut app = app_with(&[]);
        assert!(app.handle_key(key('d'), now()).is_empty());
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn refresh_keys() {
        let mut app = app_with(&["A", "B"]);
        assert_eq!(app.handle_key(key('r'), now()), vec![Effect::Send(PollCommand::Refresh("A".into()))]);
        assert_eq!(app.handle_key(key('R'), now()), vec![Effect::Send(PollCommand::RefreshAll)]);
    }

    #[test]
    fn detail_mode_scrolls_and_exits() {
        let mut app = app_with(&["A"]);
        app.handle_key(code(KeyCode::Enter), now());
        assert!(matches!(app.mode, Mode::Detail { scroll: 0 }));
        app.handle_key(key('j'), now());
        app.handle_key(key('j'), now());
        app.handle_key(key('k'), now());
        assert!(matches!(app.mode, Mode::Detail { scroll: 1 }));
        app.handle_key(key('k'), now());
        app.handle_key(key('k'), now());
        assert!(matches!(app.mode, Mode::Detail { scroll: 0 }), "scroll clamps at 0");
        assert_eq!(app.handle_key(key('r'), now()), vec![Effect::Send(PollCommand::Refresh("A".into()))]);
        app.handle_key(key('q'), now());
        assert!(matches!(app.mode, Mode::Normal));
        app.handle_key(code(KeyCode::Enter), now());
        app.handle_key(code(KeyCode::Esc), now());
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn poll_success_updates_cache_and_saves() {
        let mut app = app_with(&["A"]);
        app.polling = Some("A".into());
        let tracking = Tracking {
            number: "A".into(),
            carrier: Some("DHL".into()),
            status: Status::InTransit,
            events: vec![TrackEvent { time: Some(now()), description: "Posted".into(), location: None }],
            fetched_at: now(),
        };
        let effects = app.apply_poll_result(PollResult { number: "A".into(), result: Ok(tracking.clone()) }, now());
        assert_eq!(effects, vec![Effect::SaveState]);
        let st = app.state_for("A").unwrap();
        assert_eq!(st.tracking.as_ref(), Some(&tracking));
        assert_eq!(st.failures, 0);
        assert_eq!(st.last_error, None);
        assert_eq!(st.next_poll, now() + chrono::Duration::minutes(10));
        assert_eq!(app.polling, None);
    }

    #[test]
    fn poll_failure_keeps_old_tracking_and_records_error() {
        let mut app = app_with(&["A"]);
        let tracking = Tracking { number: "A".into(), carrier: None, status: Status::Pending, events: vec![], fetched_at: now() };
        app.apply_poll_result(PollResult { number: "A".into(), result: Ok(tracking.clone()) }, now());
        app.apply_poll_result(PollResult { number: "A".into(), result: Err("boom".into()) }, now());
        let st = app.state_for("A").unwrap();
        assert_eq!(st.tracking.as_ref(), Some(&tracking));
        assert_eq!(st.failures, 1);
        assert_eq!(st.last_error.as_deref(), Some("boom"));
        assert_eq!(st.next_poll, now() + chrono::Duration::minutes(20));
        assert_eq!(app.last_error.as_deref(), Some("A: boom"));
    }

    #[test]
    fn poll_result_for_removed_parcel_is_ignored() {
        let mut app = app_with(&[]);
        let effects = app.apply_poll_result(PollResult { number: "Z".into(), result: Err("x".into()) }, now());
        assert!(effects.is_empty());
        assert!(app.state_for("Z").is_none());
    }

    #[test]
    fn splits_number_and_label() {
        assert_eq!(split_number_and_label("  ab12  my thing "), ("AB12".into(), Some("my thing".into())));
        assert_eq!(split_number_and_label("ab12"), ("AB12".into(), None));
        assert_eq!(split_number_and_label("   "), ("".into(), None));
    }
}
```

Add `mod app;` to `src/main.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test app::`
Expected: compile errors for missing `App::new`, `handle_key`, etc.

- [ ] **Step 3: Implement**

Insert into `src/app.rs` before `#[cfg(test)]`:
```rust
const MAX_BACKOFF: chrono::Duration = chrono::Duration::hours(1);

/// "RB123 camera body" -> ("RB123", Some("camera body")). Number is upper-cased.
pub fn split_number_and_label(input: &str) -> (String, Option<String>) {
    let trimmed = input.trim();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let number = parts.next().unwrap_or("").to_ascii_uppercase();
    let label = parts.next().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned);
    (number, label)
}

impl App {
    pub fn new(parcels: ParcelList, cache: StateCache, interval: Duration) -> Self {
        Self { parcels, cache, mode: Mode::Normal, selected: 0, polling: None, last_error: None, interval }
    }

    pub fn selected_parcel(&self) -> Option<&Parcel> {
        self.parcels.parcels.get(self.selected)
    }

    pub fn state_for(&self, number: &str) -> Option<&ParcelState> {
        self.cache.by_number.get(number)
    }

    fn clamp_selection(&mut self) {
        let len = self.parcels.parcels.len();
        self.selected = if len == 0 { 0 } else { self.selected.min(len - 1) };
    }

    fn interval_chrono(&self) -> chrono::Duration {
        chrono::Duration::from_std(self.interval).unwrap_or_else(|_| chrono::Duration::zero())
    }

    pub fn handle_key(&mut self, key: KeyEvent, now: DateTime<Utc>) -> Vec<Effect> {
        match &mut self.mode {
            Mode::Normal => self.handle_normal(key, now),
            Mode::Adding(input) => match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::Normal;
                    vec![]
                }
                KeyCode::Enter => {
                    let (number, label) = split_number_and_label(input.value());
                    self.mode = Mode::Normal;
                    if number.is_empty() {
                        return vec![];
                    }
                    if self.parcels.contains(&number) {
                        self.last_error = Some(format!("{number} is already tracked"));
                        return vec![];
                    }
                    self.parcels.add(&number, label.as_deref(), now);
                    self.selected = self.parcels.parcels.len() - 1;
                    self.polling = Some(number.clone());
                    vec![Effect::SaveParcels, Effect::Send(PollCommand::Add(number))]
                }
                _ => {
                    input.handle_event(&crossterm::event::Event::Key(key));
                    vec![]
                }
            },
            Mode::ConfirmRemove => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.mode = Mode::Normal;
                        let Some(number) = self.selected_parcel().map(|p| p.number.clone()) else { return vec![] };
                        self.parcels.remove(&number);
                        self.cache.by_number.remove(&number);
                        self.clamp_selection();
                        vec![Effect::SaveParcels, Effect::SaveState, Effect::Send(PollCommand::Remove(number))]
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        self.mode = Mode::Normal;
                        vec![]
                    }
                    _ => vec![],
                }
            }
            Mode::Detail { scroll } => match key.code {
                KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => {
                    self.mode = Mode::Normal;
                    vec![]
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    *scroll = scroll.saturating_add(1);
                    vec![]
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    *scroll = scroll.saturating_sub(1);
                    vec![]
                }
                KeyCode::Char('r') => self.refresh_selected(),
                _ => vec![],
            },
        }
    }

    fn handle_normal(&mut self, key: KeyEvent, _now: DateTime<Utc>) -> Vec<Effect> {
        match key.code {
            KeyCode::Char('q') => vec![Effect::Quit],
            KeyCode::Char('j') | KeyCode::Down => {
                if self.selected + 1 < self.parcels.parcels.len() {
                    self.selected += 1;
                }
                vec![]
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                vec![]
            }
            KeyCode::Char('a') => {
                self.mode = Mode::Adding(Input::default());
                vec![]
            }
            KeyCode::Char('d') => {
                if self.selected_parcel().is_some() {
                    self.mode = Mode::ConfirmRemove;
                }
                vec![]
            }
            KeyCode::Char('r') => self.refresh_selected(),
            KeyCode::Char('R') => {
                if let Some(first) = self.parcels.parcels.first() {
                    self.polling = Some(first.number.clone());
                }
                vec![Effect::Send(PollCommand::RefreshAll)]
            }
            KeyCode::Enter => {
                if self.selected_parcel().is_some() {
                    self.mode = Mode::Detail { scroll: 0 };
                }
                vec![]
            }
            _ => vec![],
        }
    }

    fn refresh_selected(&mut self) -> Vec<Effect> {
        match self.selected_parcel().map(|p| p.number.clone()) {
            Some(number) => {
                self.polling = Some(number.clone());
                vec![Effect::Send(PollCommand::Refresh(number))]
            }
            None => vec![],
        }
    }

    pub fn apply_poll_result(&mut self, r: PollResult, now: DateTime<Utc>) -> Vec<Effect> {
        if self.polling.as_deref() == Some(r.number.as_str()) {
            self.polling = None;
        }
        if !self.parcels.contains(&r.number) {
            return vec![];
        }
        let interval = self.interval_chrono();
        let state = self.cache.by_number.entry(r.number.clone()).or_insert(ParcelState {
            tracking: None,
            last_error: None,
            failures: 0,
            next_poll: now,
        });
        match r.result {
            Ok(tracking) => {
                state.tracking = Some(tracking);
                state.last_error = None;
                state.failures = 0;
                state.next_poll = now + interval;
            }
            Err(msg) => {
                state.failures = state.failures.saturating_add(1);
                let factor = 2i32.saturating_pow(state.failures.min(16));
                state.next_poll = now + (interval * factor).min(MAX_BACKOFF);
                state.last_error = Some(msg.clone());
                self.last_error = Some(format!("{}: {msg}", r.number));
            }
        }
        vec![Effect::SaveState]
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test app::`
Expected: 13 passed.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs src/main.rs
git commit -m "feat(app): pure key/poll state machine with effects"
```

---

### Task 8: UI rendering

**Files:**
- Create: `src/ui.rs`
- Modify: `src/main.rs` (add `mod ui;`)

**Interfaces:**
- Consumes: `App`, `Mode`, `Status`, `ParcelState`.
- Produces: `pub fn draw(frame: &mut ratatui::Frame, app: &App, now: DateTime<Utc>, spinner_tick: usize)`; `pub fn status_color(s: Status) -> Color`; `pub fn humanize(d: chrono::Duration) -> String`.

- [ ] **Step 1: Write the failing tests**

`src/ui.rs`:
```rust
use chrono::{DateTime, Utc};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::app::{App, Mode};
use crate::provider::Status;

const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{TrackEvent, Tracking};
    use crate::store::{ParcelList, ParcelState, StateCache};
    use chrono::TimeZone;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::Duration;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap()
    }

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app, now(), 0)).unwrap();
        terminal.backend().to_string()
    }

    fn sample_app() -> App {
        let mut list = ParcelList::default();
        list.add("RB123456789CN", Some("camera"), now());
        list.add("1Z999AA10123456784", None, now());
        let mut cache = StateCache::default();
        cache.by_number.insert(
            "RB123456789CN".into(),
            ParcelState {
                tracking: Some(Tracking {
                    number: "RB123456789CN".into(),
                    carrier: Some("China Post".into()),
                    status: Status::InTransit,
                    events: vec![TrackEvent {
                        time: Some(now() - chrono::Duration::hours(5)),
                        description: "Departed facility".into(),
                        location: Some("Shenzhen".into()),
                    }],
                    fetched_at: now(),
                }),
                last_error: None,
                failures: 0,
                next_poll: now() + chrono::Duration::minutes(7),
            },
        );
        App::new(list, cache, Duration::from_secs(600))
    }

    #[test]
    fn table_shows_header_rows_and_values() {
        let app = sample_app();
        let out = render(&app, 120, 12);
        assert!(out.contains("LABEL"), "{out}");
        assert!(out.contains("camera"), "{out}");
        assert!(out.contains("RB123456789CN"), "{out}");
        assert!(out.contains("China Post"), "{out}");
        assert!(out.contains("in transit"), "{out}");
        assert!(out.contains("Departed facility"), "{out}");
        assert!(out.contains("5h"), "{out}");
        assert!(out.contains("7m"), "{out}");
        assert!(out.contains("1Z999AA10123456784"), "{out}");
        assert!(out.contains("2 parcels"), "{out}");
        assert!(out.contains("a add"), "{out}");
    }

    #[test]
    fn empty_list_shows_hint() {
        let app = App::new(ParcelList::default(), StateCache::default(), Duration::from_secs(600));
        let out = render(&app, 80, 10);
        assert!(out.contains("press a to add"), "{out}");
    }

    #[test]
    fn adding_mode_shows_prompt_and_input() {
        let mut app = sample_app();
        app.mode = Mode::Adding(tui_input::Input::new("RB9".into()));
        let out = render(&app, 80, 10);
        assert!(out.contains("add:"), "{out}");
        assert!(out.contains("RB9"), "{out}");
    }

    #[test]
    fn confirm_mode_names_the_parcel() {
        let mut app = sample_app();
        app.selected = 1;
        app.mode = Mode::ConfirmRemove;
        let out = render(&app, 80, 10);
        assert!(out.contains("remove 1Z999AA10123456784? (y/n)"), "{out}");
    }

    #[test]
    fn detail_mode_lists_events() {
        let mut app = sample_app();
        app.mode = Mode::Detail { scroll: 0 };
        let out = render(&app, 100, 12);
        assert!(out.contains("Shenzhen"), "{out}");
        assert!(out.contains("Departed facility"), "{out}");
        assert!(!out.contains("LABEL"), "{out}");
    }

    #[test]
    fn polling_row_shows_spinner_and_error_shows_in_header() {
        let mut app = sample_app();
        app.polling = Some("1Z999AA10123456784".into());
        app.last_error = Some("1Z999AA10123456784: timed out".into());
        let out = render(&app, 120, 12);
        assert!(out.contains("polling"), "{out}");
        assert!(out.contains("timed out"), "{out}");
    }

    #[test]
    fn humanize_durations() {
        assert_eq!(humanize(chrono::Duration::seconds(45)), "45s");
        assert_eq!(humanize(chrono::Duration::seconds(125)), "2m");
        assert_eq!(humanize(chrono::Duration::hours(5) + chrono::Duration::minutes(3)), "5h");
        assert_eq!(humanize(chrono::Duration::days(2)), "2d");
        assert_eq!(humanize(chrono::Duration::seconds(-5)), "now");
    }

    #[test]
    fn status_colors_are_distinct() {
        let all = [Status::Pending, Status::InTransit, Status::OutForDelivery, Status::Delivered, Status::Exception, Status::Unknown];
        let mut colors: Vec<Color> = all.iter().map(|s| status_color(*s)).collect();
        colors.sort_by_key(|c| format!("{c:?}"));
        colors.dedup();
        assert_eq!(colors.len(), 6);
    }
}
```

Add `mod ui;` to `src/main.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test ui::`
Expected: compile error `cannot find function draw`.

- [ ] **Step 3: Implement**

Insert into `src/ui.rs` before `#[cfg(test)]`:
```rust
pub fn status_color(s: Status) -> Color {
    match s {
        Status::Pending => Color::Gray,
        Status::InTransit => Color::Blue,
        Status::OutForDelivery => Color::Yellow,
        Status::Delivered => Color::Green,
        Status::Exception => Color::Red,
        Status::Unknown => Color::DarkGray,
    }
}

/// Coarse human duration: "45s", "2m", "5h", "2d"; negative or zero -> "now".
pub fn humanize(d: chrono::Duration) -> String {
    let secs = d.num_seconds();
    if secs <= 0 {
        "now".into()
    } else if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

pub fn draw(frame: &mut Frame, app: &App, now: DateTime<Utc>, spinner_tick: usize) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    draw_header(frame, header, app, now);
    match &app.mode {
        Mode::Detail { scroll } => draw_detail(frame, body, app, *scroll),
        _ => draw_table(frame, body, app, now, spinner_tick),
    }
    draw_footer(frame, footer, app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App, now: DateTime<Utc>) {
    let next = app
        .parcels
        .parcels
        .iter()
        .filter_map(|p| app.state_for(&p.number).map(|s| s.next_poll))
        .min()
        .map(|t| format!("next poll {}", humanize(t - now)))
        .unwrap_or_else(|| "next poll now".into());
    let mut spans = vec![
        Span::styled(" parcli ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw(format!(" {} parcels · {}", app.parcels.parcels.len(), next)),
    ];
    if let Some(err) = &app.last_error {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(err.clone(), Style::default().fg(Color::Red)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_table(frame: &mut Frame, area: Rect, app: &App, now: DateTime<Utc>, spinner_tick: usize) {
    if app.parcels.parcels.is_empty() {
        let hint = Paragraph::new("no parcels — press a to add a tracking number").dark_gray().centered();
        frame.render_widget(hint, area);
        return;
    }
    let header = Row::new(["LABEL", "NUMBER", "CARRIER", "STATUS", "LAST EVENT", "AGE", "NEXT"])
        .style(Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan));
    let rows = app.parcels.parcels.iter().map(|p| {
        let state = app.state_for(&p.number);
        let tracking = state.and_then(|s| s.tracking.as_ref());
        let status = tracking.map(|t| t.status).unwrap_or(Status::Unknown);
        let latest = tracking.and_then(|t| t.events.first());
        let next = if app.polling.as_deref() == Some(p.number.as_str()) {
            format!("{} polling", SPINNER[spinner_tick % SPINNER.len()])
        } else if status == Status::Delivered {
            "done".into()
        } else {
            state.map(|s| humanize(s.next_poll - now)).unwrap_or_else(|| "now".into())
        };
        let status_text = if tracking.is_none() && state.and_then(|s| s.last_error.as_ref()).is_some() {
            "error".to_string()
        } else if tracking.is_none() {
            "…".to_string()
        } else {
            status.label().to_string()
        };
        Row::new(vec![
            Cell::from(p.label.clone().unwrap_or_else(|_| chrono::Duration::zero())),
            Cell::from(p.number.clone()),
            Cell::from(tracking.and_then(|t| t.carrier.clone()).unwrap_or_else(|_| chrono::Duration::zero())),
            Cell::from(status_text).style(Style::default().fg(status_color(status)).add_modifier(Modifier::BOLD)),
            Cell::from(latest.map(|e| e.description.clone()).unwrap_or_else(|_| chrono::Duration::zero())),
            Cell::from(latest.and_then(|e| e.time).map(|t| humanize(now - t)).unwrap_or_else(|_| chrono::Duration::zero())),
            Cell::from(next),
        ])
    });
    let widths = [
        Constraint::Length(14),
        Constraint::Length(22),
        Constraint::Length(18),
        Constraint::Length(16),
        Constraint::Min(20),
        Constraint::Length(5),
        Constraint::Length(10),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .column_spacing(1);
    let mut state = TableState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &App, scroll: u16) {
    let Some(parcel) = app.selected_parcel() else { return };
    let tracking = app.state_for(&parcel.number).and_then(|s| s.tracking.as_ref());
    let title = match (&parcel.label, tracking.and_then(|t| t.carrier.as_deref())) {
        (Some(l), Some(c)) => format!(" {l} · {} · {c} ", parcel.number),
        (Some(l), None) => format!(" {l} · {} ", parcel.number),
        (None, Some(c)) => format!(" {} · {c} ", parcel.number),
        (None, None) => format!(" {} ", parcel.number),
    };
    let items: Vec<ListItem> = match tracking {
        None => vec![ListItem::new("no data yet")],
        Some(t) if t.events.is_empty() => vec![ListItem::new("no events reported")],
        Some(t) => t
            .events
            .iter()
            .skip(scroll as usize)
            .map(|e| {
                let when = e.time.map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|| "—".repeat(8));
                let loc = e.location.clone().unwrap_or_default();
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{when}  "), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{loc:<20} "), Style::default().fg(Color::Cyan)),
                    Span::raw(e.description.clone()),
                ]))
            })
            .collect(),
    };
    let status = tracking.map(|t| t.status).unwrap_or(Status::Unknown);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(Line::from(Span::styled(format!(" {} ", status.label()), Style::default().fg(status_color(status)))));
    frame.render_widget(List::new(items).block(block), area);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = match &app.mode {
        Mode::Normal => Line::from(" a add · d remove · r refresh · R refresh all · Enter detail · j/k move · q quit ").dark_gray(),
        Mode::Adding(input) => {
            let prompt = " add: ";
            let width = area.width.saturating_sub(prompt.len() as u16 + 1) as usize;
            let scroll = input.visual_scroll(width);
            let shown: String = input.value().chars().skip(scroll).collect();
            let cursor_x = area.x + prompt.len() as u16 + (input.visual_cursor().saturating_sub(scroll)) as u16;
            frame.set_cursor_position((cursor_x, area.y));
            Line::from(vec![Span::styled(prompt, Style::default().fg(Color::Yellow)), Span::raw(shown)])
        }
        Mode::ConfirmRemove => {
            let number = app.selected_parcel().map(|p| p.number.as_str()).unwrap_or("");
            Line::from(format!(" remove {number}? (y/n) ")).yellow()
        }
        Mode::Detail { .. } => Line::from(" j/k scroll · r refresh · q/Esc back ").dark_gray(),
    };
    frame.render_widget(Paragraph::new(line), area);
}
```

Notes: `TableState::with_selected`, `Layout::vertical(..).areas()`, `Paragraph::centered`, `Block::title_bottom`, and the `Stylize` shortcuts (`.dark_gray()`, `.yellow()`) exist in ratatui 0.30. If `Row::new([..])` with a `[&str; 7]` fails type inference, wrap each in `Cell::from`.

- [ ] **Step 4: Run tests**

Run: `cargo test ui::`
Expected: 8 passed.

- [ ] **Step 5: Commit**

```bash
git add src/ui.rs src/main.rs
git commit -m "feat(ui): colored table, detail view, input and confirm bars"
```

---

### Task 9: Event loop and main wiring

**Files:**
- Create: `src/tui.rs`
- Modify: `src/main.rs` (replace placeholder)

**Interfaces:**
- Consumes: everything above.
- Produces: `pub async fn run(app: App, paths: Paths, cmd_tx: mpsc::Sender<PollCommand>, results: mpsc::Receiver<PollResult>) -> Result<App>` and the `parcli` binary.

- [ ] **Step 1: Write src/tui.rs**

```rust
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::app::{App, Effect};
use crate::poller::{PollCommand, PollResult};
use crate::store::Paths;
use crate::ui;

/// Run the dashboard until the user quits. Restores the terminal on exit and on panic.
pub async fn run(
    mut app: App,
    paths: Paths,
    cmd_tx: mpsc::Sender<PollCommand>,
    mut results: mpsc::Receiver<PollResult>,
) -> Result<App> {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        default_hook(info);
    }));
    let mut terminal = ratatui::init();
    let outcome = event_loop(&mut terminal, &mut app, &paths, &cmd_tx, &mut results).await;
    ratatui::restore();
    outcome.map(|_| app)
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    paths: &Paths,
    cmd_tx: &mpsc::Sender<PollCommand>,
    results: &mut mpsc::Receiver<PollResult>,
) -> Result<()> {
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut spinner = 0usize;
    loop {
        terminal.draw(|f| ui::draw(f, app, Utc::now(), spinner))?;
        let effects = tokio::select! {
            _ = tick.tick() => { spinner = spinner.wrapping_add(1); vec![] }
            Some(r) = results.recv() => app.apply_poll_result(r, Utc::now()),
            Some(ev) = events.next() => match ev.context("reading terminal events")? {
                Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key, Utc::now()),
                _ => vec![],
            },
        };
        for effect in effects {
            match effect {
                Effect::SaveParcels => {
                    if let Err(e) = app.parcels.save(&paths.parcels) {
                        app.last_error = Some(format!("saving parcels: {e:#}"));
                    }
                }
                Effect::SaveState => {
                    if let Err(e) = app.cache.save(&paths.state) {
                        app.last_error = Some(format!("saving state: {e:#}"));
                    }
                }
                Effect::Send(cmd) => {
                    if cmd_tx.send(cmd).await.is_err() {
                        app.last_error = Some("poller stopped".into());
                    }
                }
                Effect::Quit => return Ok(()),
            }
        }
    }
}
```

- [ ] **Step 2: Write src/main.rs**

```rust
mod app;
mod poller;
mod provider;
mod store;
mod tui;
mod ui;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use tokio::sync::mpsc;

use crate::app::App;
use crate::poller::{run_poller, PollCommand, Scheduler};
use crate::provider::parcelsapp::ParcelsAppProvider;
use crate::store::{ParcelList, Paths, StateCache};

/// top-like terminal dashboard for international parcel tracking
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Minutes between polls of each parcel
    #[arg(long, default_value_t = 10)]
    interval: u64,
    /// Seconds to wait for parcelsapp before giving up on one poll
    #[arg(long, default_value_t = 45)]
    timeout: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let interval = Duration::from_secs(args.interval.max(1) * 60);

    let paths = Paths::discover()?;
    let parcels = ParcelList::load(&paths.parcels)?;
    let cache = StateCache::load(&paths.state)?;

    let provider = ParcelsAppProvider::launch(Duration::from_secs(args.timeout))
        .await
        .context("starting headless browser")?;
    let provider: Arc<ParcelsAppProvider> = Arc::new(provider);

    // Resume the cached schedule so a restart does not re-poll everything at once.
    let now = Utc::now();
    let mut scheduler = Scheduler::new(interval, vec![], now);
    for p in &parcels.parcels {
        scheduler.add(&p.number, now);
        if let Some(state) = cache.by_number.get(&p.number) {
            if let Some(t) = &state.tracking {
                scheduler.record_success(&p.number, t.status, state.next_poll - chrono::Duration::from_std(interval)?);
            }
        }
    }

    let (cmd_tx, cmd_rx) = mpsc::channel::<PollCommand>(32);
    let (res_tx, res_rx) = mpsc::channel(32);
    let worker = tokio::spawn(run_poller(provider.clone(), scheduler, cmd_rx, res_tx));

    let app = App::new(parcels, cache, interval);
    let outcome = tui::run(app, paths, cmd_tx.clone(), res_rx).await;

    let _ = cmd_tx.send(PollCommand::Shutdown).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), worker).await;
    if let Ok(provider) = Arc::try_unwrap(provider) {
        provider.close().await;
    }
    outcome.map(|_| ())
}
```

Note: `Arc<ParcelsAppProvider>` coerces to `Arc<dyn Provider>` at the `run_poller` call; if inference complains, write `provider.clone() as Arc<dyn crate::provider::Provider>`.

- [ ] **Step 3: Build, test, lint**

Run: `cargo build && cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all green. Remove any `#[allow(dead_code)]` added earlier.

- [ ] **Step 4: Manual smoke test**

Run: `cargo run` in a real terminal (not through a pipe). Checklist — each item must be observed, not inferred:
1. Empty table with the "press a to add" hint.
2. `a`, type `RB123456789CN camera`, Enter → row appears with spinner in NEXT; within ~30 s it shows carrier `China Post`, status `pending` (grey), last event `Pending shipping by the seller`, NEXT counting down from `10m`.
3. `Enter` → detail panel with the event; `q` → back.
4. `r` → spinner, then refreshed.
5. `d`, `n` → still there; `d`, `y` → gone.
6. Re-add, `q` to quit; `cargo run` again → row present immediately with cached data.
7. `cat ~/Library/Application\ Support/parcli/parcels.toml` and `~/Library/Caches/parcli/state.json` exist.
8. Terminal is restored (prompt visible, echo works) after quit.

Fix anything found before committing.

- [ ] **Step 5: Commit**

```bash
git add src/tui.rs src/main.rs
git commit -m "feat: wire event loop, poller and provider into the parcli binary"
```

---

### Task 10: README

**Files:**
- Create: `README.md`

- [ ] **Step 1: Write README.md**

```markdown
# parcli

A `top`-like terminal dashboard for international parcel tracking. It drives the
free [parcelsapp.com](https://parcelsapp.com) widget in headless Chrome, so no
API key is needed.

## Requirements

- Rust stable (1.80+)
- Google Chrome or Chromium. If it is not found automatically, set
  `PARCLI_CHROME=/path/to/chrome`.

## Usage

    cargo install --path .
    parcli                 # poll every 10 minutes
    parcli --interval 30   # poll every 30 minutes

| Key | Action |
|---|---|
| `a` | add a tracking number (optionally followed by a label: `RB123456789CN camera`) |
| `d` | remove the selected parcel (confirms with `y`/`n`) |
| `r` / `R` | refresh selected / all |
| `Enter` | show event history; `q`/`Esc` to go back |
| `j`/`k`, arrows | move |
| `q` | quit |

Delivered parcels stop being polled. Failed polls back off exponentially up to
an hour.

## Files

- `~/Library/Application Support/parcli/parcels.toml` (Linux: `~/.config/parcli/`) — your list
- `~/Library/Caches/parcli/state.json` (Linux: `~/.cache/parcli/`) — last known results

## Development

    cargo test                               # unit tests, no browser needed
    cargo test live_ -- --ignored --nocapture  # one real poll against parcelsapp

Design: `docs/superpowers/specs/2026-09-13-parcli-design.md`.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: README with usage and keys"
```

---

## Deferred from the spec

- **Browser crash relaunch** (spec "Error handling": poller attempts one relaunch). Not in this plan: a dead browser surfaces as per-parcel errors with backoff, and `R` retries. Add after the MVP is exercised, once the actual failure mode of a Chrome crash under chromiumoxide is observed.
