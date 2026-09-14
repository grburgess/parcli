# parcli Polish Implementation Plan (translation + visual pass)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Translate non-English tracking events to English, and make the table and detail views visually richer, on top of the shipped MVP (branch `feat/mvp`).

**Architecture:** A new `translate` module (trait `Translator` + `MyMemoryTranslator` over `reqwest`) is owned by the poller, which translates untranslated event descriptions after each successful poll using an in-memory cache seeded from the persisted state. `TrackEvent` gains `translated: Option<String>`; `Tracking` gains `attributes` and `tracking_url` parsed from the widget response. The UI reads only these fields — no App changes.

**Tech Stack:** as MVP + `reqwest 0.12` (rustls, json).

**Spec:** `docs/superpowers/specs/2026-09-13-parcli-design.md` plus the user's approved follow-up (this plan's header is the spec for the new behaviour).

## Global Constraints

- Everything from the MVP plan's Global Constraints (commit trailers, `cargo test` green, `cargo clippy --all-targets -- -D warnings` clean, no `.superpowers/` or `.playwright-mcp/` in commits).
- Backwards-compatible `state.json`: every new serde field has `#[serde(default)]`; an MVP-era cache must load.
- Translation is best-effort: any failure leaves `translated = None` and never fails a poll. It runs inside the poller task, sequentially, at most one HTTP request per untranslated distinct description.
- Translation provider: MyMemory `GET https://api.mymemory.translated.net/get?q=<text>&langpair=Autodetect|en` (free, no key, ~5,000 chars/day anonymous). Verified 2026-09-14: French → `{"responseData":{"translatedText":"Packages being prepared at the sender","match":0.85},"responseStatus":200,...}`; English input → `{"responseStatus":403,"responseDetails":"PLEASE SELECT TWO DISTINCT LANGUAGES",...}` (treat as "already English", cache as no-translation-needed); Chinese → 200 with English text. Send a `User-Agent: parcli/<version>` header.
- `--no-translate` flag disables translation entirely.
- Widget response fields used (verified live): `attributes: [{"l":"days_transit","n":"Days in transit","val":"2"}]`, `externalTracking: [{"url": "...", "slug": "...", "method": "GET"}]`.

## File structure

```
src/translate.rs        Translator trait, MyMemoryTranslator, parse_mymemory()
src/provider/mod.rs     TrackEvent.translated, Tracking.attributes/tracking_url, TrackEvent::display_text()
src/provider/parcelsapp.rs  parse attributes + externalTracking
src/poller.rs           run_poller gains Option<Arc<dyn Translator>> + translation cache
src/ui.rs               table + detail visual pass
src/main.rs             --no-translate, wire translator, seed cache
README.md               translation note + flag
```

---

### Task 1: Data model — translated text, attributes, tracking URL

**Files:** Modify `src/provider/mod.rs`, `src/provider/parcelsapp.rs`, `src/store.rs` (test only)

**Interfaces — Produces:**
```rust
pub struct TrackEvent { pub time, pub description, pub location, #[serde(default)] pub translated: Option<String> }
impl TrackEvent { pub fn display_text(&self) -> &str }   // translated if Some, else description
pub struct Tracking { ..., #[serde(default)] pub attributes: Vec<(String, String)>, #[serde(default)] pub tracking_url: Option<String> }
```

- [ ] **Step 1: Failing tests**

In `src/provider/mod.rs` tests add:
```rust
    #[test]
    fn display_text_prefers_translation() {
        let mut e = TrackEvent { time: None, description: "Colis".into(), location: None, translated: None };
        assert_eq!(e.display_text(), "Colis");
        e.translated = Some("Parcel".into());
        assert_eq!(e.display_text(), "Parcel");
    }
```
In `src/provider/parcelsapp.rs` tests, extend `parses_archive_fixture` with `assert!(t.attributes.is_empty()); assert_eq!(t.tracking_url.as_deref(), Some("https://global.cainiao.com/detail.htm?mailNoList=RB123456789CN"));` and add:
```rust
    #[test]
    fn parses_attributes_and_tracking_url() {
        let body = r#"{"states":[],"carriers":[],"attributes":[{"l":"days_transit","n":"Days in transit","val":"2"}],
            "externalTracking":[{"url":"https://example.test/x","slug":"x","method":"GET"}]}"#;
        let t = parse_response(body, "X", now()).unwrap();
        assert_eq!(t.attributes, vec![("Days in transit".to_string(), "2".to_string())]);
        assert_eq!(t.tracking_url.as_deref(), Some("https://example.test/x"));
        assert!(t.events.is_empty());
    }
```
In `src/store.rs` tests add a backwards-compat test:
```rust
    #[test]
    fn loads_mvp_era_state_without_new_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, r#"{"by_number":{"A":{"tracking":{"number":"A","carrier":null,"status":"InTransit",
            "events":[{"time":null,"description":"x","location":null}],"fetched_at":"2026-09-13T12:00:00Z"},
            "last_error":null,"failures":0,"next_poll":"2026-09-13T12:10:00Z"}}}"#).unwrap();
        let cache = StateCache::load(&path).unwrap();
        let t = cache.by_number["A"].tracking.as_ref().unwrap();
        assert_eq!(t.events[0].translated, None);
        assert!(t.attributes.is_empty());
        assert_eq!(t.tracking_url, None);
    }
```
Every existing constructor of `TrackEvent`/`Tracking` in tests (provider, poller, app, ui) needs the new fields — add `translated: None` and `attributes: vec![], tracking_url: None` (or use `..` with a helper). Prefer adding a test helper `Tracking::sample(number, status)` **only in `#[cfg(test)]`** in provider/mod.rs if it removes repetition.

- [ ] **Step 2: Run** `cargo test` — expect compile errors for missing fields.

- [ ] **Step 3: Implement**

`src/provider/mod.rs`:
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackEvent {
    pub time: Option<DateTime<Utc>>,
    pub description: String,
    pub location: Option<String>,
    /// English rendering of `description` when the source was not English.
    #[serde(default)]
    pub translated: Option<String>,
}

impl TrackEvent {
    pub fn display_text(&self) -> &str {
        self.translated.as_deref().unwrap_or(&self.description)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tracking {
    pub number: String,
    pub carrier: Option<String>,
    pub status: Status,
    pub events: Vec<TrackEvent>,
    pub fetched_at: DateTime<Utc>,
    /// Name/value pairs the site reports, e.g. ("Days in transit", "2").
    #[serde(default)]
    pub attributes: Vec<(String, String)>,
    /// Carrier's own tracking page, if the site provides one.
    #[serde(default)]
    pub tracking_url: Option<String>,
}
```
`src/provider/parcelsapp.rs`: add to `ApiResponse`
```rust
    #[serde(default)]
    attributes: Vec<ApiAttribute>,
    #[serde(rename = "externalTracking", default)]
    external_tracking: Vec<ApiExternal>,
```
with
```rust
#[derive(Deserialize)]
struct ApiAttribute { n: Option<String>, val: Option<String> }
#[derive(Deserialize)]
struct ApiExternal { url: Option<String> }
```
and in `parse_response`, before building `Tracking`:
```rust
    let attributes = api.attributes.iter()
        .filter_map(|a| Some((a.n.clone()?, a.val.clone()?)))
        .collect();
    let tracking_url = api.external_tracking.iter().find_map(|e| e.url.clone());
```
and set `translated: None` where `TrackEvent` is built, `attributes, tracking_url` in `Tracking`.

- [ ] **Step 4: Run** `cargo test` (all green) and clippy.
- [ ] **Step 5: Commit** `feat(provider): translated text, attributes and tracking URL on Tracking`

---

### Task 2: Translator module

**Files:** Create `src/translate.rs`; modify `Cargo.toml` (+reqwest), `src/main.rs` (`mod translate;`)

**Interfaces — Produces:**
```rust
#[async_trait] pub trait Translator: Send + Sync {
    /// Ok(Some(english)) when translated, Ok(None) when the text is already English.
    async fn translate(&self, text: &str) -> anyhow::Result<Option<String>>;
}
pub struct MyMemoryTranslator { client: reqwest::Client }
impl MyMemoryTranslator { pub fn new() -> anyhow::Result<Self> }
pub fn parse_mymemory(body: &str) -> anyhow::Result<Option<String>>
```

- [ ] **Step 1: Cargo.toml** add `reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json"] }`.

- [ ] **Step 2: Failing tests** (`src/translate.rs`):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_translation() {
        let body = r#"{"responseData":{"translatedText":"Packages being prepared at the sender","match":0.85},"quotaFinished":false,"responseDetails":"","responseStatus":200,"matches":[]}"#;
        assert_eq!(parse_mymemory(body).unwrap(), Some("Packages being prepared at the sender".into()));
    }

    #[test]
    fn same_language_means_no_translation_needed() {
        let body = r#"{"responseData":{"translatedText":"PLEASE SELECT TWO DISTINCT LANGUAGES"},"responseDetails":"PLEASE SELECT TWO DISTINCT LANGUAGES","responseStatus":403}"#;
        assert_eq!(parse_mymemory(body).unwrap(), None);
    }

    #[test]
    fn other_errors_are_errors() {
        let body = r#"{"responseData":{"translatedText":"MYMEMORY WARNING: YOU USED ALL AVAILABLE FREE TRANSLATIONS FOR TODAY"},"responseDetails":"MYMEMORY WARNING: YOU USED ALL AVAILABLE FREE TRANSLATIONS FOR TODAY","responseStatus":429}"#;
        let err = parse_mymemory(body).unwrap_err().to_string();
        assert!(err.contains("FREE TRANSLATIONS"), "{err}");
        assert!(parse_mymemory("<html>").is_err());
    }

    #[test]
    fn empty_translation_is_none() {
        let body = r#"{"responseData":{"translatedText":""},"responseStatus":200}"#;
        assert_eq!(parse_mymemory(body).unwrap(), None);
    }

    /// Network. cargo test live_translate -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_translate_french() {
        let t = MyMemoryTranslator::new().unwrap();
        let out = t.translate("Colis en cours de préparation chez l'expéditeur").await.unwrap();
        assert!(out.as_deref().unwrap_or("").to_lowercase().contains("sender"), "{out:?}");
        assert_eq!(t.translate("Pending shipping by the seller").await.unwrap(), None);
    }
}
```

- [ ] **Step 3: Implement**
```rust
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

const ENDPOINT: &str = "https://api.mymemory.translated.net/get";
const SAME_LANGUAGE: &str = "PLEASE SELECT TWO DISTINCT LANGUAGES";

#[async_trait]
pub trait Translator: Send + Sync {
    /// `Ok(Some(english))` when translated, `Ok(None)` when already English.
    async fn translate(&self, text: &str) -> Result<Option<String>>;
}

pub struct MyMemoryTranslator {
    client: reqwest::Client,
}

impl MyMemoryTranslator {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("parcli/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .context("building HTTP client")?;
        Ok(Self { client })
    }
}

#[async_trait]
impl Translator for MyMemoryTranslator {
    async fn translate(&self, text: &str) -> Result<Option<String>> {
        let body = self
            .client
            .get(ENDPOINT)
            .query(&[("q", text), ("langpair", "Autodetect|en")])
            .send()
            .await
            .context("translation request")?
            .text()
            .await
            .context("reading translation response")?;
        parse_mymemory(&body)
    }
}

#[derive(Deserialize)]
struct MyMemoryResponse {
    #[serde(rename = "responseData")]
    data: Option<MyMemoryData>,
    #[serde(rename = "responseStatus", default)]
    status: serde_json::Value,
    #[serde(rename = "responseDetails", default)]
    details: String,
}

#[derive(Deserialize)]
struct MyMemoryData {
    #[serde(rename = "translatedText", default)]
    translated_text: String,
}

/// Parse a MyMemory response. 403 "two distinct languages" means the text is
/// already English and maps to `Ok(None)`; other non-200 statuses are errors.
pub fn parse_mymemory(body: &str) -> Result<Option<String>> {
    let r: MyMemoryResponse = serde_json::from_str(body).context("translation response is not JSON")?;
    // responseStatus is sometimes a number, sometimes a string.
    let status = match &r.status {
        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
        serde_json::Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    };
    if r.details.contains(SAME_LANGUAGE) {
        return Ok(None);
    }
    if status != 200 {
        anyhow::bail!("translation failed ({status}): {}", r.details);
    }
    let text = r.data.map(|d| d.translated_text).unwrap_or_default();
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    Ok(Some(text.to_owned()))
}
```
Add `mod translate;` to main.rs (a temporary `#[allow(dead_code)]` **on the module declaration only** — `#[allow(dead_code)] mod translate;` — until Task 3 uses it).

- [ ] **Step 4: Run** `cargo test translate` (4 pass, 1 ignored), then the live test once, then `cargo test` + clippy.
- [ ] **Step 5: Commit** `feat(translate): MyMemory translator with same-language detection`

---

### Task 3: Poller translates events

**Files:** Modify `src/poller.rs`, `src/main.rs`

**Interfaces — Produces:**
```rust
pub async fn run_poller(provider: Arc<dyn Provider>, translator: Option<Arc<dyn Translator>>,
    translation_cache: HashMap<String, Option<String>>,   // description -> translated (None = already English)
    scheduler: Scheduler, commands: mpsc::Receiver<PollCommand>, results: mpsc::Sender<PollEvent>)
pub fn seed_translation_cache(cache: &StateCache) -> HashMap<String, Option<String>>
pub async fn translate_events(translator: &dyn Translator, cache: &mut HashMap<String, Option<String>>, events: &mut [TrackEvent])
```
`seed_translation_cache` collects every event with `translated: Some(_)` into the map. Events with `translated: None` in the cache are NOT seeded as "already English" (they may simply never have been translated), so they get one attempt.

- [ ] **Step 1: Failing tests** in `src/poller.rs`:
```rust
    struct FakeTranslator { calls: Mutex<Vec<String>>, fail: bool }
    #[async_trait]
    impl crate::translate::Translator for FakeTranslator {
        async fn translate(&self, text: &str) -> anyhow::Result<Option<String>> {
            self.calls.lock().unwrap().push(text.to_owned());
            if self.fail { anyhow::bail!("quota"); }
            Ok(if text.starts_with("EN:") { None } else { Some(format!("[en] {text}")) })
        }
    }

    fn ev(desc: &str) -> TrackEvent {
        TrackEvent { time: None, description: desc.into(), location: None, translated: None }
    }

    #[tokio::test]
    async fn translate_events_fills_and_caches() {
        let t = FakeTranslator { calls: Mutex::new(vec![]), fail: false };
        let mut cache = HashMap::new();
        let mut events = vec![ev("Colis"), ev("EN: Delivered"), ev("Colis")];
        translate_events(&t, &mut cache, &mut events).await;
        assert_eq!(events[0].translated.as_deref(), Some("[en] Colis"));
        assert_eq!(events[1].translated, None);
        assert_eq!(events[2].translated.as_deref(), Some("[en] Colis"));
        assert_eq!(t.calls.lock().unwrap().len(), 2, "duplicate description translated once");
        assert_eq!(cache.get("EN: Delivered"), Some(&None), "same-language result is cached too");
        // second pass hits the cache only
        let mut again = vec![ev("Colis")];
        translate_events(&t, &mut cache, &mut again).await;
        assert_eq!(t.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn translate_events_failure_leaves_original_and_is_not_cached() {
        let t = FakeTranslator { calls: Mutex::new(vec![]), fail: true };
        let mut cache = HashMap::new();
        let mut events = vec![ev("Colis")];
        translate_events(&t, &mut cache, &mut events).await;
        assert_eq!(events[0].translated, None);
        assert!(cache.is_empty());
    }

    #[test]
    fn seed_translation_cache_from_state() {
        use crate::store::{ParcelState, StateCache};
        let mut cache = StateCache::default();
        let mut tr = Tracking { number: "A".into(), carrier: None, status: Status::InTransit, events: vec![ev("Colis"), ev("Plain")], fetched_at: Utc::now(), attributes: vec![], tracking_url: None };
        tr.events[0].translated = Some("Parcel".into());
        cache.by_number.insert("A".into(), ParcelState { tracking: Some(tr), last_error: None, failures: 0, next_poll: Utc::now() });
        let seeded = seed_translation_cache(&cache);
        assert_eq!(seeded.get("Colis"), Some(&Some("Parcel".to_string())));
        assert!(!seeded.contains_key("Plain"));
    }
```
Update the two existing worker tests to pass `None, HashMap::new()` for the new params, and add:
```rust
    #[tokio::test]
    async fn worker_translates_before_reporting() {
        let provider = Arc::new(MockProvider { calls: Mutex::new(vec![]), fail: false }); // its events have description "moving"
        let translator: Arc<dyn crate::translate::Translator> = Arc::new(FakeTranslator { calls: Mutex::new(vec![]), fail: false });
        let scheduler = Scheduler::new(Duration::from_secs(600), vec!["A".into()], Utc::now());
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (res_tx, mut res_rx) = mpsc::channel(8);
        let worker = tokio::spawn(run_poller(provider, Some(translator), HashMap::new(), scheduler, cmd_rx, res_tx));
        expect_started(&mut res_rx, "A").await;
        let r = expect_finished(&mut res_rx, "A").await;
        assert_eq!(r.result.unwrap().events[0].translated.as_deref(), Some("[en] moving"));
        cmd_tx.send(PollCommand::Shutdown).await.unwrap();
        worker.await.unwrap();
    }
```
(`expect_started`/`expect_finished` helpers already exist in the tests module.)

- [ ] **Step 2: Run** `cargo test poller` → compile errors.

- [ ] **Step 3: Implement** in `src/poller.rs`:
```rust
use std::collections::HashMap;
use crate::provider::TrackEvent;
use crate::store::StateCache;
use crate::translate::Translator;

pub type TranslationCache = HashMap<String, Option<String>>;

/// Rebuild the description→translation map from previously persisted events.
pub fn seed_translation_cache(cache: &StateCache) -> TranslationCache {
    cache
        .by_number
        .values()
        .filter_map(|s| s.tracking.as_ref())
        .flat_map(|t| t.events.iter())
        .filter_map(|e| e.translated.clone().map(|tr| (e.description.clone(), Some(tr))))
        .collect()
}

/// Fill `translated` on events, consulting/updating the cache; failures are
/// logged into nothing and simply leave the original text.
pub async fn translate_events(translator: &dyn Translator, cache: &mut TranslationCache, events: &mut [TrackEvent]) {
    for e in events.iter_mut() {
        if e.translated.is_some() || e.description.trim().is_empty() {
            continue;
        }
        if let Some(cached) = cache.get(&e.description) {
            e.translated = cached.clone();
            continue;
        }
        match translator.translate(&e.description).await {
            Ok(result) => {
                cache.insert(e.description.clone(), result.clone());
                e.translated = result;
            }
            Err(_) => {} // best effort; retry on a later poll
        }
    }
}
```
`run_poller` gains `translator: Option<Arc<dyn Translator>>, mut translations: TranslationCache` after `provider`; after `let result = provider.track(..)`, before recording:
```rust
                let result = match (result, &translator) {
                    (Ok(mut t), Some(tr)) => {
                        translate_events(tr.as_ref(), &mut translations, &mut t.events).await;
                        Ok(t)
                    }
                    (r, _) => r,
                };
```
`src/main.rs`: add `#[arg(long)] no_translate: bool` ("Show carrier text as-is instead of translating to English"); build
```rust
    let translator: Option<Arc<dyn Translator>> = if args.no_translate { None } else { Some(Arc::new(MyMemoryTranslator::new()?)) };
    let translations = seed_translation_cache(&cache);
```
and pass both to `run_poller`. Remove the `#[allow(dead_code)]` from `mod translate;`.

- [ ] **Step 4: Run** `cargo test` + clippy.
- [ ] **Step 5: Commit** `feat(poller): translate event text to English via MyMemory, cached across polls`

---

### Task 4: Table visual pass

**Files:** Modify `src/ui.rs`

Requirements (all in `draw_table`/`draw_header`, tests via `TestBackend` substrings):
1. Table wrapped in a `Block` with rounded borders (`BorderType::Rounded`) and title ` parcels ` ; header row: bold, `Color::Black` on `Color::Cyan` background.
2. Column styles: LABEL cyan; NUMBER bold; CARRIER magenta; LAST EVENT uses `e.display_text()`; AGE colored by staleness — `< 24h` green, `< 72h` yellow, else red (`pub fn age_color(d: chrono::Duration) -> Color`).
3. STATUS rendered as a pill: text ` in transit ` with `fg(Color::Black).bg(status_color(status))` (pub fn `status_pill(status) -> Span<'static>`; reuse in detail).
4. NEXT column: `pub fn progress_bar(elapsed_frac: f64, width: usize) -> String` producing `▰▰▰▱▱`-style bar (`▰` filled, `▱` empty), followed by the countdown, e.g. `▰▰▱▱▱ 6m`. `elapsed_frac = 1 - remaining/interval`, clamped 0..=1; width 5. `done` for Delivered (green), spinner+`polling` while polling (yellow).
5. Zebra: odd rows get `Style::default().bg(Color::Indexed(235))` (a near-black grey); the selected row keeps `REVERSED` highlight. Zebra background is skipped when a row is the selected row.
6. Header line: keep content, add a right-aligned clock `HH:MM:SS` (UTC→local via `chrono::Local`) so the display visibly ticks. Implement by rendering two `Paragraph`s (left, right-aligned) in the same 1-line area using `Layout::horizontal([Min(0), Length(8)])`.

- [ ] **Step 1: Failing tests** (add to ui tests):
```rust
    #[test]
    fn progress_bar_fills_with_fraction() {
        assert_eq!(progress_bar(0.0, 5), "▱▱▱▱▱");
        assert_eq!(progress_bar(0.5, 4), "▰▰▱▱");
        assert_eq!(progress_bar(1.0, 3), "▰▰▰");
        assert_eq!(progress_bar(7.0, 2), "▰▰", "clamped");
        assert_eq!(progress_bar(-1.0, 2), "▱▱", "clamped");
    }

    #[test]
    fn age_colors_by_staleness() {
        assert_eq!(age_color(chrono::Duration::hours(3)), Color::Green);
        assert_eq!(age_color(chrono::Duration::hours(30)), Color::Yellow);
        assert_eq!(age_color(chrono::Duration::days(4)), Color::Red);
    }

    #[test]
    fn table_shows_progress_bar_and_translated_text() {
        let mut app = sample_app();
        // 7 minutes remaining of a 10-minute interval → 30% elapsed → 1 of 5 blocks
        let st = app.cache.by_number.get_mut("RB123456789CN").unwrap();
        st.tracking.as_mut().unwrap().events[0].translated = Some("Left the facility".into());
        let out = render(&app, 120, 12);
        assert!(out.contains("▰▱▱▱▱ 7m"), "{out}");
        assert!(out.contains("Left the facility"), "{out}");
        assert!(!out.contains("Departed facility"), "table shows translation only: {out}");
        assert!(out.contains("parcels"), "block title: {out}");
    }
```
Update `table_shows_header_rows_and_values`: the NEXT assertion becomes `out.contains("7m")` (still true) and the block title check.

- [ ] **Step 2: Run** `cargo test ui::` → compile errors for `progress_bar`/`age_color`.

- [ ] **Step 3: Implement** — replace `draw_table` and `draw_header` per requirements 1–6; add the three pub helpers:
```rust
pub fn age_color(age: chrono::Duration) -> Color {
    if age < chrono::Duration::hours(24) { Color::Green }
    else if age < chrono::Duration::hours(72) { Color::Yellow }
    else { Color::Red }
}

pub fn progress_bar(frac: f64, width: usize) -> String {
    let frac = if frac.is_nan() { 0.0 } else { frac.clamp(0.0, 1.0) };
    let filled = (frac * width as f64).round() as usize;
    let filled = filled.min(width);
    format!("{}{}", "▰".repeat(filled), "▱".repeat(width - filled))
}

pub fn status_pill(status: Status) -> Span<'static> {
    Span::styled(format!(" {} ", status.label()), Style::default().fg(Color::Black).bg(status_color(status)).add_modifier(Modifier::BOLD))
}
```
In the NEXT cell: `let remaining = s.next_poll - now; let frac = 1.0 - remaining.num_seconds() as f64 / app.interval.as_secs_f64(); format!("{} {}", progress_bar(frac, 5), humanize(remaining))`. Widths: NEXT column `Length(14)`, STATUS `Length(18)`. The rounded block costs 2 columns/rows; table area = `block.inner(area)`.

Note on the `▰▱` glyphs: they are 1 cell wide in unicode-width, so `TestBackend` output contains them verbatim.

- [ ] **Step 4: Run** `cargo test` + clippy.
- [ ] **Step 5: Commit** `feat(ui): bordered zebra table with status pills, staleness colors and poll progress bars`

---

### Task 5: Detail view visual pass

**Files:** Modify `src/ui.rs`

Requirements (`draw_detail`):
1. Layout: `Layout::vertical([Length(7), Min(3)])` inside a rounded outer `Block` titled ` <label or number> `.
2. Summary card (top, its own rounded block titled ` summary `): two columns of `key  value` lines rendered as a `Paragraph` with `Line`s: `Number`, `Label`, `Carrier` (magenta), `Status` (pill), then every `(name, value)` from `attributes` (e.g. `Days in transit  2`), `Last update` (`fetched_at` humanized as `Xm ago`), `Next poll` (countdown or `done`), `Tracking link` (`tracking_url`, dimmed) — keys dark gray, values default. Omit lines whose value is missing.
3. Timeline (bottom, block titled ` events (N) `): each event renders as up to two lines:
   - line 1: `●` (colored `status_color` for the newest event, dark gray otherwise) + date `YYYY-MM-DD HH:MM` (dark gray) + location (cyan) + `display_text()` (bold for the newest event);
   - line 2 (only when `translated` is Some and differs from `description`): `│` gutter + original `description` in dark gray italic.
   Older events are prefixed with `│` continuation glyphs so it reads as a vertical timeline. Scroll clamps as today; the clamp now applies to events, not lines.
4. `no data yet` / `no events reported` / `last error: …` (red) fallbacks when there is no tracking or when the last poll failed and no tracking exists.

- [ ] **Step 1: Failing tests**:
```rust
    #[test]
    fn detail_shows_summary_card_and_timeline_with_original_text() {
        let mut app = sample_app();
        {
            let st = app.cache.by_number.get_mut("RB123456789CN").unwrap();
            let t = st.tracking.as_mut().unwrap();
            t.events[0].translated = Some("Left the facility".into());
            t.attributes = vec![("Days in transit".into(), "2".into())];
            t.tracking_url = Some("https://example.test/track".into());
        }
        app.mode = Mode::Detail { scroll: 0 };
        let out = render(&app, 100, 20);
        for needle in ["summary", "Number", "RB123456789CN", "Carrier", "China Post", "Days in transit", "2",
                       "Tracking link", "https://example.test/track", "events (1)", "●", "Left the facility", "Departed facility", "Shenzhen"] {
            assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
        }
    }

    #[test]
    fn detail_without_data_shows_placeholder_and_error() {
        let mut app = sample_app();
        app.selected = 1; // 1Z999… has no cache entry
        app.mode = Mode::Detail { scroll: 0 };
        let out = render(&app, 100, 16);
        assert!(out.contains("no data yet"), "{out}");
        app.cache.by_number.insert("1Z999AA10123456784".into(), crate::store::ParcelState {
            tracking: None, last_error: Some("timed out".into()), failures: 1, next_poll: now() });
        let out = render(&app, 100, 16);
        assert!(out.contains("timed out"), "{out}");
    }
```
Keep the existing `detail_mode_lists_events` and `detail_scroll_past_end_still_shows_last_event` tests passing (they assert "Shenzhen"/"Departed facility" present and "LABEL" absent).

- [ ] **Step 2: Run** `cargo test ui::` → new tests fail.
- [ ] **Step 3: Implement** per requirements (Paragraph for the card, `List` of `ListItem::new(Vec<Line>)` for the timeline).
- [ ] **Step 4: Run** `cargo test` + clippy.
- [ ] **Step 5: Commit** `feat(ui): detail view with summary card and event timeline`

---

### Task 6: README + spec note

**Files:** Modify `README.md`, `docs/superpowers/specs/2026-09-13-parcli-design.md`

- [ ] README: add `--no-translate` to Usage; a "Translation" section: non-English carrier text is translated to English via MyMemory (free, anonymous, ~5,000 chars/day; results cached in `state.json`, so each event costs one request once); detail view shows the original beneath. Add the visual features to the description (status pills, staleness colors, poll progress bar, timeline).
- [ ] Spec: append a "Follow-up 2026-09-14" section summarising translation + visual pass, and correct the "Error handling" paragraph to what ships (per-parcel backoff, no browser relaunch; 90 s default timeout).
- [ ] Commit `docs: translation and visual pass`
