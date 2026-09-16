use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;

use crate::geo::{normalize_place, Coord, GeoCache, Geocoder};
use crate::provider::{Provider, Status, TrackEvent, Tracking};
use crate::store::StateCache;
use crate::translate::Translator;

pub type TranslationCache = HashMap<String, Option<String>>;

/// Rebuild the description→translation map from previously persisted events.
/// `translate_events` stores `translated = Some(description)` even for
/// already-English text, so this also seeds the cache for descriptions that
/// needed no translation — restarts never re-look those up either.
pub fn seed_translation_cache(cache: &StateCache) -> TranslationCache {
    cache
        .by_number
        .values()
        .filter_map(|s| s.tracking.as_ref())
        .flat_map(|t| t.events.iter())
        .filter_map(|e| e.translated.clone().map(|tr| (e.description.clone(), Some(tr))))
        .collect()
}

/// Fill `translated` on events, consulting/updating the cache. `Ok(None)`
/// (already English) is persisted as `Some(description)` so a restart never
/// re-requests translation for text already known not to need it. On the
/// first `Err` the loop stops for this poll — remaining events are left
/// untranslated and retried on the next poll — bounding the worst case to one
/// translator timeout per poll.
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
                let result = result.or_else(|| Some(e.description.clone()));
                cache.insert(e.description.clone(), result.clone());
                e.translated = result;
            }
            Err(_) => break,
        }
    }
}

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

/// Sent from the poller worker to the UI: `Started` right before a poll begins
/// (so the UI can show the spinner even for scheduled, non-user-initiated
/// polls), then `Finished` once it completes.
#[derive(Debug)]
pub enum PollEvent {
    Started(String),
    Finished(PollResult),
    Geocoded { key: String, coord: Option<Coord> },
}

/// Geocode each event's location, at most once per distinct normalized
/// location. `Ok(None)` (no match) is cached like a hit. On the first `Err`
/// the loop stops for this poll — remaining locations are left ungeocoded
/// and retried on the next poll — mirroring `translate_events`'s circuit
/// breaker. Already-cached and empty locations are skipped without a call.
pub async fn geocode_locations(geocoder: &dyn Geocoder, geo: &mut GeoCache, events: &[TrackEvent], out: &mpsc::Sender<PollEvent>) {
    for e in events {
        let Some(loc) = e.location.as_deref() else { continue };
        let display = normalize_place(loc);
        let key = display.to_lowercase();
        if key.is_empty() || geo.contains_key(&key) {
            continue;
        }
        match geocode_place(geocoder, &display).await {
            Ok(coord) => {
                geo.insert(key.clone(), coord);
                if out.send(PollEvent::Geocoded { key, coord }).await.is_err() {
                    return;
                }
            }
            Err(_) => break,
        }
    }
}

/// Try the full location string, then its place-name parts, until one geocodes.
async fn geocode_place(geocoder: &dyn Geocoder, display: &str) -> anyhow::Result<Option<Coord>> {
    for candidate in crate::geo::place_candidates(display) {
        if let Some(coord) = geocoder.geocode(&candidate, true).await? {
            return Ok(Some(coord));
        }
    }
    Ok(None)
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

    /// The site has no record of this number: retry at the backoff cap rather
    /// than climbing through the whole backoff ladder.
    pub fn record_not_found(&mut self, number: &str, now: DateTime<Utc>) {
        if let Some(e) = self.entries.get_mut(number) {
            e.failures = e.failures.saturating_add(1);
            e.next_poll = now + chrono::Duration::from_std(MAX_BACKOFF).unwrap_or_else(|_| chrono::Duration::zero());
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

    /// Test-only accessor; production code never inspects a scheduled time directly.
    #[cfg(test)]
    pub fn next_poll(&self, number: &str) -> Option<DateTime<Utc>> {
        self.entries.get(number).map(|e| e.next_poll)
    }
}

/// Geocode `home` once at startup, if not already cached, and report the
/// result via `results`. A miss (`Ok(None)`) is *not* cached — an address
/// Nominatim can't currently resolve shouldn't permanently pin the home key
/// to `None`, so it's retried on the next startup instead of getting stuck
/// showing `⌂ unresolved`; a transient `Err` is likewise not cached, and
/// neither sends an event. Returns `false` if the result channel is closed
/// (caller should stop).
async fn geocode_home(geocoder: &dyn Geocoder, geo: &mut GeoCache, home: &str, results: &mpsc::Sender<PollEvent>) -> bool {
    if home.is_empty() || geo.contains_key(home) {
        return true;
    }
    if let Ok(coord) = geocoder.geocode(home, false).await {
        if coord.is_some() {
            geo.insert(home.to_owned(), coord);
        }
        return results.send(PollEvent::Geocoded { key: home.to_owned(), coord }).await.is_ok();
    }
    true
}

/// Worker: applies commands, polls one due parcel at a time, reports results.
#[allow(clippy::too_many_arguments)]
pub async fn run_poller(
    provider: Arc<dyn Provider>,
    translator: Option<Arc<dyn Translator>>,
    mut translations: TranslationCache,
    geocoder: Option<Arc<dyn Geocoder>>,
    mut geo: GeoCache,
    home: Option<String>,
    mut scheduler: Scheduler,
    mut commands: mpsc::Receiver<PollCommand>,
    results: mpsc::Sender<PollEvent>,
) {
    if let (Some(g), Some(home)) = (&geocoder, &home) {
        if !geocode_home(g.as_ref(), &mut geo, home, &results).await {
            return;
        }
    }
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
                if results.send(PollEvent::Started(number.clone())).await.is_err() {
                    return;
                }
                let result = provider.track(&number).await.map_err(|e| e.to_string());
                let result = match (result, &translator) {
                    (Ok(mut t), Some(tr)) => {
                        translate_events(tr.as_ref(), &mut translations, &mut t.events).await;
                        Ok(t)
                    }
                    (r, _) => r,
                };
                match &result {
                    Ok(t) => scheduler.record_success(&number, t.status, Utc::now()),
                    Err(msg) if msg == crate::provider::NOT_FOUND_MSG => scheduler.record_not_found(&number, Utc::now()),
                    Err(_) => scheduler.record_failure(&number, Utc::now()),
                }
                // Events to geocode are cloned out before `result` moves into
                // `PollResult`: `Finished` must reach the UI before any `Geocoded`
                // for this poll, so the UI can show the parcel's new state first.
                let geocode_events: Option<Vec<TrackEvent>> =
                    match (&result, &geocoder) {
                        (Ok(t), Some(_)) => Some(t.events.clone()),
                        _ => None,
                    };
                if results.send(PollEvent::Finished(PollResult { number, result })).await.is_err() {
                    return;
                }
                if let (Some(events), Some(g)) = (geocode_events, &geocoder) {
                    geocode_locations(g.as_ref(), &mut geo, &events, &results).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::TrackEvent;
    use crate::translate::Translator;
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
    fn not_found_goes_straight_to_the_backoff_cap() {
        let mut s = Scheduler::new(Duration::from_secs(600), vec!["A".into()], t0());
        s.record_not_found("A", t0());
        assert_eq!(s.next_poll("A"), Some(t0() + mins(60)));
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
                events: vec![TrackEvent { time: None, description: "moving".into(), location: None, translated: None }],
                fetched_at: Utc::now(),
                attributes: vec![],
                tracking_url: None,
            })
        }
    }

    async fn expect_started(rx: &mut mpsc::Receiver<PollEvent>, number: &str) {
        match rx.recv().await.unwrap() {
            PollEvent::Started(n) => assert_eq!(n, number),
            other => panic!("expected Started({number}), got {other:?}"),
        }
    }

    async fn expect_finished(rx: &mut mpsc::Receiver<PollEvent>, number: &str) -> PollResult {
        match rx.recv().await.unwrap() {
            PollEvent::Finished(r) => {
                assert_eq!(r.number, number);
                r
            }
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn worker_polls_due_parcels_and_reports_results() {
        let provider = Arc::new(MockProvider { calls: Mutex::new(vec![]), fail: false });
        let scheduler = Scheduler::new(Duration::from_secs(600), vec!["A".into()], Utc::now());
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (res_tx, mut res_rx) = mpsc::channel(8);
        let worker = tokio::spawn(run_poller(provider.clone(), None, HashMap::new(), None, HashMap::new(), None, scheduler, cmd_rx, res_tx));

        expect_started(&mut res_rx, "A").await;
        let first = expect_finished(&mut res_rx, "A").await;
        assert!(first.result.is_ok());

        cmd_tx.send(PollCommand::Add("B".into())).await.unwrap();
        expect_started(&mut res_rx, "B").await;
        expect_finished(&mut res_rx, "B").await;

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
        let worker = tokio::spawn(run_poller(provider, None, HashMap::new(), None, HashMap::new(), None, scheduler, cmd_rx, res_tx));
        expect_started(&mut res_rx, "A").await;
        let r = expect_finished(&mut res_rx, "A").await;
        assert_eq!(r.result.unwrap_err(), "boom");
        cmd_tx.send(PollCommand::Shutdown).await.unwrap();
        worker.await.unwrap();
    }

    struct FakeTranslator {
        calls: Mutex<Vec<String>>,
        fail: bool,
    }

    #[async_trait]
    impl Translator for FakeTranslator {
        async fn translate(&self, text: &str) -> anyhow::Result<Option<String>> {
            self.calls.lock().unwrap().push(text.to_owned());
            if self.fail {
                anyhow::bail!("quota");
            }
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
        assert_eq!(events[1].translated.as_deref(), Some("EN: Delivered"), "already-English text is persisted, not left None");
        assert_eq!(events[2].translated.as_deref(), Some("[en] Colis"));
        assert_eq!(t.calls.lock().unwrap().len(), 2, "duplicate description translated once");
        assert_eq!(
            cache.get("EN: Delivered"),
            Some(&Some("EN: Delivered".to_string())),
            "same-language result is cached as itself too"
        );
        // second pass hits the cache only
        let mut again = vec![ev("Colis")];
        translate_events(&t, &mut cache, &mut again).await;
        assert_eq!(t.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn translate_events_failure_leaves_original_and_is_not_cached() {
        let t = FakeTranslator { calls: Mutex::new(vec![]), fail: true };
        let mut cache = HashMap::new();
        let mut events = vec![ev("Colis"), ev("Autre")];
        translate_events(&t, &mut cache, &mut events).await;
        assert_eq!(events[0].translated, None);
        assert_eq!(events[1].translated, None);
        assert!(cache.is_empty());
        assert_eq!(t.calls.lock().unwrap().len(), 1, "circuit breaker: stop after the first failure");
    }

    #[test]
    fn seed_translation_cache_from_state() {
        use crate::store::{ParcelState, StateCache};
        let mut cache = StateCache::default();
        let mut tr = Tracking {
            number: "A".into(),
            carrier: None,
            status: Status::InTransit,
            events: vec![ev("Colis"), ev("Plain")],
            fetched_at: Utc::now(),
            attributes: vec![],
            tracking_url: None,
        };
        tr.events[0].translated = Some("Parcel".into());
        cache.by_number.insert("A".into(), ParcelState { tracking: Some(tr), last_error: None, failures: 0, next_poll: Utc::now() });
        let seeded = seed_translation_cache(&cache);
        assert_eq!(seeded.get("Colis"), Some(&Some("Parcel".to_string())));
        assert!(!seeded.contains_key("Plain"));
    }

    #[tokio::test]
    async fn worker_translates_before_reporting() {
        let provider = Arc::new(MockProvider { calls: Mutex::new(vec![]), fail: false }); // its events have description "moving"
        let translator: Arc<dyn Translator> = Arc::new(FakeTranslator { calls: Mutex::new(vec![]), fail: false });
        let scheduler = Scheduler::new(Duration::from_secs(600), vec!["A".into()], Utc::now());
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (res_tx, mut res_rx) = mpsc::channel(8);
        let worker = tokio::spawn(run_poller(provider, Some(translator), HashMap::new(), None, HashMap::new(), None, scheduler, cmd_rx, res_tx));
        expect_started(&mut res_rx, "A").await;
        let r = expect_finished(&mut res_rx, "A").await;
        assert_eq!(r.result.unwrap().events[0].translated.as_deref(), Some("[en] moving"));
        cmd_tx.send(PollCommand::Shutdown).await.unwrap();
        worker.await.unwrap();
    }

    struct FakeGeocoder {
        calls: Mutex<Vec<(String, bool)>>,
        responses: HashMap<String, Result<Option<Coord>, ()>>,
    }

    #[async_trait]
    impl Geocoder for FakeGeocoder {
        async fn geocode(&self, query: &str, settlement_only: bool) -> anyhow::Result<Option<Coord>> {
            self.calls.lock().unwrap().push((query.to_owned(), settlement_only));
            match self.responses.get(query) {
                Some(Ok(coord)) => Ok(*coord),
                Some(Err(())) => anyhow::bail!("boom"),
                None => Ok(None),
            }
        }
    }

    fn loc(location: &str) -> TrackEvent {
        TrackEvent { time: None, description: "x".into(), location: Some(location.into()), translated: None }
    }

    #[tokio::test]
    async fn geocode_locations_falls_back_to_place_parts() {
        let mut responses = HashMap::new();
        responses.insert("SECLIN".to_string(), Ok(Some((50.5, 3.0))));
        let geocoder = FakeGeocoder { calls: Mutex::new(vec![]), responses };
        let mut geo: GeoCache = HashMap::new();
        let events = vec![loc("SECLIN - CHRONOPOST"), loc("Web Services")];
        let (tx, mut rx) = mpsc::channel(8);

        geocode_locations(&geocoder, &mut geo, &events, &tx).await;
        drop(tx);

        let calls: Vec<String> = geocoder.calls.lock().unwrap().iter().map(|(q, _)| q.clone()).collect();
        assert_eq!(calls, vec!["SECLIN - CHRONOPOST", "SECLIN", "Web Services"], "full string first, then parts; stops at the first hit");
        assert_eq!(geo.get("seclin - chronopost"), Some(&Some((50.5, 3.0))), "cached under the ORIGINAL key");
        assert_eq!(geo.get("web services"), Some(&None));
        let mut n = 0;
        while rx.recv().await.is_some() {
            n += 1;
        }
        assert_eq!(n, 2);
    }

    #[tokio::test]
    async fn geocode_locations_dedups_skips_cached_and_sends_events() {
        let mut responses = HashMap::new();
        responses.insert("Shenzhen".to_string(), Ok(Some((22.5, 114.0))));
        responses.insert("Nowhere".to_string(), Ok(None));
        responses.insert("Boom".to_string(), Err(()));
        let geocoder = FakeGeocoder { calls: Mutex::new(vec![]), responses };
        let mut geo: GeoCache = HashMap::new();
        // "shenzhen " (different case, trailing whitespace) must dedup with "Shenzhen"
        // via the lowercased cache key, yielding a single request and a single key.
        let events = vec![loc("Shenzhen"), loc("shenzhen "), loc("Nowhere"), loc("Boom"), loc("Paris")];
        let (tx, mut rx) = mpsc::channel(8);

        geocode_locations(&geocoder, &mut geo, &events, &tx).await;
        drop(tx);

        assert_eq!(
            *geocoder.calls.lock().unwrap(),
            vec![("Shenzhen".to_string(), true), ("Nowhere".to_string(), true), ("Boom".to_string(), true)],
            "dedups case/whitespace variants (query uses the first-seen display form); stops after the Boom error; Paris never reached"
        );

        let mut sent = vec![];
        while let Some(e) = rx.recv().await {
            match e {
                PollEvent::Geocoded { key, coord } => sent.push((key, coord)),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(sent, vec![("shenzhen".to_string(), Some((22.5, 114.0))), ("nowhere".to_string(), None)]);

        assert_eq!(geo.get("shenzhen"), Some(&Some((22.5, 114.0))));
        assert_eq!(geo.get("nowhere"), Some(&None), "Ok(None) is cached too");
        assert!(!geo.contains_key("paris"), "never reached: loop broke on Boom's error");
        assert!(!geo.contains_key("boom"), "Err is not cached");
    }

    #[tokio::test]
    async fn worker_geocodes_home_once_at_start() {
        let provider = Arc::new(MockProvider { calls: Mutex::new(vec![]), fail: false });
        let mut responses = HashMap::new();
        responses.insert("123 Main St".to_string(), Ok(Some((1.0, 2.0))));
        let geocoder = Arc::new(FakeGeocoder { calls: Mutex::new(vec![]), responses });
        let dyn_geocoder: Arc<dyn Geocoder> = geocoder.clone();
        let scheduler = Scheduler::new(Duration::from_secs(600), vec!["A".into()], Utc::now());
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (res_tx, mut res_rx) = mpsc::channel(8);
        let worker = tokio::spawn(run_poller(
            provider,
            None,
            HashMap::new(),
            Some(dyn_geocoder),
            HashMap::new(),
            Some("123 Main St".into()),
            scheduler,
            cmd_rx,
            res_tx,
        ));

        match res_rx.recv().await.unwrap() {
            PollEvent::Geocoded { key, coord } => {
                assert_eq!(key, "123 Main St");
                assert_eq!(coord, Some((1.0, 2.0)));
            }
            other => panic!("expected Geocoded, got {other:?}"),
        }
        expect_started(&mut res_rx, "A").await;
        expect_finished(&mut res_rx, "A").await;

        cmd_tx.send(PollCommand::Add("B".into())).await.unwrap();
        expect_started(&mut res_rx, "B").await;
        expect_finished(&mut res_rx, "B").await;

        cmd_tx.send(PollCommand::Shutdown).await.unwrap();
        worker.await.unwrap();

        assert_eq!(
            *geocoder.calls.lock().unwrap(),
            vec![("123 Main St".to_string(), false)],
            "home is geocoded exactly once at start, with settlement_only=false, regardless of how many parcels poll afterward"
        );
    }

    #[tokio::test]
    async fn home_miss_is_not_cached() {
        // No matching entry in `responses` -> FakeGeocoder returns Ok(None), i.e. a miss.
        let geocoder = FakeGeocoder { calls: Mutex::new(vec![]), responses: HashMap::new() };
        let mut geo: GeoCache = HashMap::new();
        let (tx, mut rx) = mpsc::channel(8);

        let ok = geocode_home(&geocoder, &mut geo, "Nowhere Ave", &tx).await;
        assert!(ok);
        drop(tx);

        match rx.recv().await.unwrap() {
            PollEvent::Geocoded { key, coord } => {
                assert_eq!(key, "Nowhere Ave");
                assert_eq!(coord, None);
            }
            other => panic!("expected Geocoded, got {other:?}"),
        }
        assert!(rx.recv().await.is_none(), "exactly one event for the miss");
        assert!(!geo.contains_key("Nowhere Ave"), "a home geocode miss must not be cached, so it's retried on the next startup");
    }

    struct LocatedProvider;

    #[async_trait]
    impl Provider for LocatedProvider {
        async fn track(&self, number: &str) -> anyhow::Result<Tracking> {
            Ok(Tracking {
                number: number.to_owned(),
                carrier: None,
                status: Status::InTransit,
                events: vec![TrackEvent { time: None, description: "moving".into(), location: Some("Shenzhen".into()), translated: None }],
                fetched_at: Utc::now(),
                attributes: vec![],
                tracking_url: None,
            })
        }
    }

    #[tokio::test]
    async fn worker_sends_finished_before_geocoding_event_locations() {
        let provider = Arc::new(LocatedProvider);
        let mut responses = HashMap::new();
        responses.insert("Shenzhen".to_string(), Ok(Some((22.5, 114.0))));
        let geocoder: Arc<dyn Geocoder> = Arc::new(FakeGeocoder { calls: Mutex::new(vec![]), responses });
        let scheduler = Scheduler::new(Duration::from_secs(600), vec!["A".into()], Utc::now());
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (res_tx, mut res_rx) = mpsc::channel(8);
        let worker =
            tokio::spawn(run_poller(provider, None, HashMap::new(), Some(geocoder), HashMap::new(), None, scheduler, cmd_rx, res_tx));

        expect_started(&mut res_rx, "A").await;
        // expect_finished panics on anything but Finished, so this alone proves
        // Finished arrives before any Geocoded event for this poll's locations.
        let finished = expect_finished(&mut res_rx, "A").await;
        assert!(finished.result.is_ok());

        match res_rx.recv().await.unwrap() {
            PollEvent::Geocoded { key, coord } => {
                assert_eq!(key, "shenzhen");
                assert_eq!(coord, Some((22.5, 114.0)));
            }
            other => panic!("expected Geocoded after Finished, got {other:?}"),
        }

        cmd_tx.send(PollCommand::Shutdown).await.unwrap();
        worker.await.unwrap();
    }
}
