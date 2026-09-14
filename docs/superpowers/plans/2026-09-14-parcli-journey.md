# parcli Journey Implementation Plan (palette, journey animation, map)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Richer color, an animated origin→home journey strip in the detail view, and a terminal world map with origin / waypoints / current / home pins, fed by Nominatim geocoding of event locations and a configured home address.

**Architecture:** New `geo` module (Geocoder trait + Nominatim impl + cache in `state.json`) driven by the poller after translation; geocode results flow to the App via a new `PollEvent::Geocoded`. New `journey` module derives ordered stops from a `Tracking` and renders the animated strip. `ui` gains a `theme` palette and a `Canvas`-based map pane (toggle `m`). Home address is stored in `parcels.toml` and set with `--home`.

**Tech Stack:** as before (ratatui 0.30 `canvas` with the built-in world `Map`, reqwest 0.12 rustls). No new crates.

**Spec:** the approved design in chat (2026-09-14) — recorded verbatim in the "Design" section below; base spec `docs/superpowers/specs/2026-09-13-parcli-design.md`.

## Design (approved)

1. **Palette** — `ui::theme` constants: `ACCENT` cyan, `CARRIER` magenta, `WARN` amber (`Color::Indexed(214)`), `DIM` dark gray, `ZEBRA` `Indexed(235)`, `ERROR_BG` `Indexed(52)`. Applied: table title + header band in ACCENT; NUMBER white bold; carrier color from a stable hash of the carrier name over a fixed 8-color list; footer key letters highlighted (`a` add → the `a` in ACCENT bold); timeline dates colored by recency (`age_color`); last error red on ERROR_BG; block borders tinted with the selected parcel's `status_color`.
2. **Journey strip** (detail, above the timeline, 3 lines): stops = distinct event locations oldest→newest, then home. `Shenzhen ●━━━● Roissy ━━━◉┄┄┄┄○ Berlin`. Solid `━` travelled, `◉` current, dashed `┄` remaining; a pulse `▸` moves along the dashed segment with the 250 ms tick; Delivered: all solid green with `✔` at home; Exception: current node red. Pure fn of (stops, status, tick, width).
3. **Map** (detail, right pane when width ≥ 100 and `show_map`): `Canvas` + `Map{HighResolution}` in DIM, Braille marker, bounds = bbox of pins padded 15% (min span 10° lon / 6° lat), fallback world view; lines between consecutive geocoded stops in `status_color`; pins printed as `●` origin, `•` waypoints, `◉` current (alternates `◉`/`○` on tick), `⌂` home. Empty → world outline + "no locations yet". Toggle `m` (Normal or Detail), default on.
4. **Geocoding** — Nominatim `GET https://nominatim.openstreetmap.org/search?q=<place>&format=jsonv2&limit=1[&featureType=settlement]`, header `User-Agent: parcli/<ver> (github.com/jburgess/parcli)`, ≥1.1 s between requests, 15 s timeout. Events use `featureType=settlement` (verified: "Shenzhen"→22.54,114.05; "Roissy"→48.79,2.65; "Web Services"→`[]`). Home uses no featureType (street address). Results (incl. misses as `None`) cached in `StateCache.geo: HashMap<String, Option<(f64,f64)>>`; key = trimmed location string; home key = the address string.
5. **Home** — `ParcelList.home: Option<String>` (serde default). `parcli --home "Musterstraße 1, 10115 Berlin"` saves it and continues into the TUI. Header shows `⌂ set`/`⌂ unset`; footer hint when unset: `--home to set your address`.

## Global Constraints

- All MVP/polish Global Constraints (trailers, `cargo test` green, `cargo clippy --all-targets -- -D warnings` clean, `#[serde(default)]` on new persisted fields, no `.superpowers/` or `.playwright-mcp/` in commits).
- Geocoding is best-effort and sequential inside the poller, ≤ 1 request per distinct location, throttled ≥ 1.1 s; a failure is not cached (retry next poll); an empty result IS cached as `None`. Circuit-break on the first `Err` per poll (same pattern as translation).
- Only place-name strings and the home address are ever sent to Nominatim — never tracking numbers, carriers or event text.
- Rendering functions stay pure; animation state is the existing `spinner_tick: usize`.
- Terminal ≥ 80×24 must not panic at any size; the map pane only appears at width ≥ 100.

## File structure

```
src/geo.rs           Geocoder trait, NominatimGeocoder, parse_nominatim, GeoCache type
src/journey.rs       Stop, journey_stops(), render_strip()
src/ui/mod.rs        (move ui.rs here) draw(), header/table/footer, theme application
src/ui/theme.rs      palette constants, carrier_color(), key_hint()
src/ui/detail.rs     draw_detail (card, strip, timeline, map pane)
src/ui/map.rs        map_bounds(), draw_map()
src/poller.rs        geocode step, PollEvent::Geocoded
src/app.rs           show_map, `m` key, Geocoded handling
src/store.rs         ParcelList.home, StateCache.geo
src/main.rs          --home
```

---

### Task 1: geo module + persistence

**Files:** Create `src/geo.rs`; modify `src/store.rs`, `src/main.rs` (`mod geo;` with `#[allow(dead_code)]` on the mod line until Task 2).

**Interfaces — Produces:**
```rust
pub type Coord = (f64, f64);                       // (lat, lon)
pub type GeoCache = HashMap<String, Option<Coord>>;
#[async_trait] pub trait Geocoder: Send + Sync {
    /// `Ok(None)` = no match (cacheable). Err = transient failure.
    async fn geocode(&self, query: &str, settlement_only: bool) -> anyhow::Result<Option<Coord>>;
}
pub struct NominatimGeocoder { .. }   // pub fn new() -> Result<Self>; enforces ≥1.1 s between calls with a tokio::sync::Mutex<Option<Instant>>
pub fn parse_nominatim(body: &str) -> anyhow::Result<Option<Coord>>
pub fn normalize_place(s: &str) -> String   // trim, collapse whitespace
// store.rs
pub struct ParcelList { pub parcels, #[serde(default)] pub home: Option<String> }
pub struct StateCache { pub by_number, #[serde(default)] pub geo: GeoCache }
```

- [ ] **Tests (write first)** — `src/geo.rs`:
```rust
    #[test] fn parses_first_result() {
        let body = r#"[{"place_id":1,"lat":"22.5445741","lon":"114.0545429","display_name":"深圳市","addresstype":"city"}]"#;
        assert_eq!(parse_nominatim(body).unwrap(), Some((22.5445741, 114.0545429)));
    }
    #[test] fn empty_array_is_none() { assert_eq!(parse_nominatim("[]").unwrap(), None); }
    #[test] fn bad_json_or_bad_numbers_are_errors() {
        assert!(parse_nominatim("<html>").is_err());
        assert!(parse_nominatim(r#"[{"lat":"x","lon":"1"}]"#).is_err());
    }
    #[test] fn normalize_place_collapses_whitespace() {
        assert_eq!(normalize_place("  Roissy   CDG \n"), "Roissy CDG");
    }
    /// Network: cargo test live_geocode -- --ignored --nocapture
    #[tokio::test] #[ignore] async fn live_geocode_shenzhen_and_junk() {
        let g = NominatimGeocoder::new().unwrap();
        let c = g.geocode("Shenzhen", true).await.unwrap().unwrap();
        assert!((c.0 - 22.5).abs() < 1.0 && (c.1 - 114.0).abs() < 1.0, "{c:?}");
        assert_eq!(g.geocode("Web Services", true).await.unwrap(), None);
    }
```
`src/store.rs` tests: `home` round-trips through TOML and defaults to None on old files; `geo` round-trips through JSON (`{"Shenzhen": [22.5,114.0], "Web Services": null}`) and defaults to empty on MVP-era files (extend `loads_mvp_era_state_without_new_fields`).

- [ ] **Implement** — `parse_nominatim`: `Vec<serde_json::Value>`-free typed struct `{ lat: String, lon: String }`, parse f64 with context. `NominatimGeocoder::geocode`: lock the mutex, sleep until 1.1 s since the last call, GET with `.query(&[("q", q), ("format","jsonv2"), ("limit","1")])` plus `("featureType","settlement")` when `settlement_only`, `.text()` → `parse_nominatim`. Client: user-agent `concat!("parcli/", env!("CARGO_PKG_VERSION"), " (github.com/jburgess/parcli)")`, timeout 15 s.
- [ ] `cargo test` (+ live geocode test once), clippy, commit `feat(geo): Nominatim geocoder and geo cache/home persistence`.

---

### Task 2: poller geocodes; App receives coordinates; `--home`

**Files:** Modify `src/poller.rs`, `src/app.rs`, `src/main.rs`, `src/tui.rs` (no change expected), `src/store.rs` (none).

**Interfaces — Produces:**
```rust
pub enum PollEvent { Started(String), Finished(PollResult), Geocoded { key: String, coord: Option<Coord> } }
pub async fn run_poller(provider, translator, translations, geocoder: Option<Arc<dyn Geocoder>>, geo: GeoCache, home: Option<String>, scheduler, commands, results)
pub async fn geocode_locations(geocoder: &dyn Geocoder, geo: &mut GeoCache, events: &[TrackEvent], out: &mpsc::Sender<PollEvent>)  // distinct normalized locations; skips cached; break on first Err; sends Geocoded for each new result
// App
pub show_map: bool (default true); key `m` toggles in Normal and Detail; apply_poll_event(Geocoded{..}) inserts into cache.geo and returns [SaveState]
pub fn home_coord(&self) -> Option<Coord>  // cache.geo.get(home)
```
Home geocoding: at poller start, if `home` is Some and not in `geo`, geocode it with `settlement_only = false` and send `Geocoded`.
`main.rs`: `#[arg(long, value_name = "ADDRESS")] home: Option<String>` — when given, `parcels.home = Some(addr)`, save immediately, print nothing, continue.

- [ ] **Tests:** poller — `geocode_locations_dedups_skips_cached_and_sends_events` (FakeGeocoder: "Shenzhen"→Some, "Nowhere"→None, "Boom"→Err; events with locations ["Shenzhen","shenzhen ","Nowhere","Boom","Paris"] → calls = Shenzhen, Nowhere, Boom(err→break); sent Geocoded for Shenzhen and Nowhere only; cache has both; "Paris" untouched); `worker_geocodes_home_once_at_start`; app — `m_toggles_show_map_in_normal_and_detail`, `geocoded_event_updates_cache_and_saves`; main — none (manual `--help`).
- [ ] Implement; `cargo test`, clippy; remove the `#[allow(dead_code)]` on `mod geo;`; commit `feat(poller): geocode event locations and home via Nominatim`.

---

### Task 3: theme and color pass

**Files:** Move `src/ui.rs` → `src/ui/mod.rs` (git mv), create `src/ui/theme.rs`; modify `src/ui/mod.rs`.

**Interfaces — Produces:** `pub mod theme { pub const ACCENT, CARRIER, WARN, DIM, ZEBRA, ERROR_BG: Color; pub fn carrier_color(name: &str) -> Color; pub fn key_hint(key: &str, label: &str) -> Vec<Span<'static>> }`.
Requirements: `carrier_color` = stable hash (FNV-1a over bytes) mod 8 into `[Magenta, Blue, Green, Cyan, Yellow, LightMagenta, LightBlue, LightGreen]`; same name → same color (test), "DHL" ≠ "China Post" (test; if they collide, add a salt until they differ and pin it in the test). Footer hints rebuilt with `key_hint` (key in ACCENT bold, label DIM). Header: error styled red on ERROR_BG; `⌂ set`/`⌂ unset` indicator (needs `app.parcels.home`). Table: CARRIER cell uses `carrier_color`; block border `.border_style(status_color(selected status))`. Timeline dates use `age_color(now - time)`.
- [ ] Tests: `carrier_color_is_stable_and_distinct_for_samples`, `footer_shows_key_hints` (contains "add", "remove"), header contains "⌂". Existing tests keep passing (paths change to `ui::` submodule — keep `pub use` so `crate::ui::draw` still works).
- [ ] Commit `feat(ui): theme palette, per-carrier colors, key hints, status-tinted borders`.

---

### Task 4: journey strip

**Files:** Create `src/journey.rs`; create `src/ui/detail.rs` (move `draw_detail` there from mod.rs); modify `src/ui/mod.rs`, `src/main.rs` (`mod journey;`).

**Interfaces — Produces:**
```rust
pub struct Stop { pub name: String, pub coord: Option<Coord>, pub kind: StopKind }   // StopKind::{Origin, Waypoint, Current, Home}
pub fn journey_stops(tracking: Option<&Tracking>, home: Option<&str>, geo: &GeoCache) -> Vec<Stop>
   // distinct event locations oldest→newest (case-insensitive on normalize_place), last one = Current unless Delivered (then Home is Current-and-done); home appended if Some
pub fn render_strip(stops: &[Stop], status: Status, tick: usize, width: u16) -> Vec<Line<'static>>  // 3 lines: names row, track row, sub-labels row ("origin", "now", "home")
```
Track row rules: between consecutive stops, `━` (status_color) up to and including the Current stop; after Current, `┄` in DIM with one `▸` at position `tick % seg_len` (WARN); Delivered → all `━` green and Home glyph `✔`; Exception → Current glyph `◉` red. Names truncated to fit `width / stops.len()`. Zero stops → single line "no locations yet"; one stop (home only) → `○ Berlin` with hint "waiting for first scan".
- [ ] Tests: `stops_dedup_and_order` (events newest-first in `Tracking` → oldest-first stops, "Shenzhen"/"shenzhen" dedup, home appended as Home), `strip_marks_current_and_dashes_remaining` (contains `◉`, `┄`, `▸`), `strip_delivered_is_solid_with_check` (contains `✔`, no `┄`), `strip_pulse_moves_with_tick` (render tick 0 vs tick 1 differ), `strip_handles_narrow_width` (width 20, no panic, ≤ 20 cells per line), ui: detail render contains `┄` and the home name when home set.
- [ ] Commit `feat(journey): animated origin→home strip in detail view`.

---

### Task 5: map pane

**Files:** Create `src/ui/map.rs`; modify `src/ui/detail.rs`, `src/app.rs` (none beyond Task 2), `src/ui/mod.rs`.

**Interfaces — Produces:** `pub fn map_bounds(coords: &[Coord]) -> ([f64;2] /*x lon*/, [f64;2] /*y lat*/)`; `pub fn draw_map(frame, area, stops: &[Stop], status: Status, tick: usize)`.
Requirements: bounds = bbox padded 15% each side, min span 10° lon × 6° lat, clamped to [-180,180]/[-90,90]; no coords → world `[-180,180]×[-60,85]`. Canvas: `Marker::Braille`, `Map { resolution: MapResolution::High, color: theme::DIM }`, `Line` between consecutive geocoded stops in `status_color`, `ctx.print(lon, lat, Span)` pins: `●` Origin (ACCENT), `•` Waypoint (DIM), `◉`/`○` Current alternating by `tick % 2` (status_color), `⌂` Home (green). Block titled ` map ` (border tinted like the others). Detail layout: if `app.show_map && area.width >= 100` → `Layout::horizontal([Percentage(58), Percentage(42)])`, map on the right; else no map.
- [ ] Tests: `map_bounds_pads_and_enforces_min_span` (single coord → 10°×6° box centered; two far coords → padded bbox; empty → world), `map_bounds_clamps_at_poles`; ui: detail at 120×30 with geo cache for Shenzhen+home contains "map" and `⌂`; at 90×30 contains no "map" title; with `show_map=false` no "map".
- [ ] Commit `feat(ui): world map pane with origin, waypoints, current position and home`.

---

### Task 6: docs

README: `--home` usage + privacy note (address and place names go to OpenStreetMap Nominatim, 1 req/s, cached); `m` key; screenshot-free description of strip + map. Spec: append "Follow-up 2026-09-14 (2) — palette, journey strip, map". Commit `docs: journey strip, map and --home`.
