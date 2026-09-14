use std::time::Duration;

use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::poller::{PollCommand, PollEvent, PollResult};
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
    pub show_map: bool,
}

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
        Self { parcels, cache, mode: Mode::Normal, selected: 0, polling: None, last_error: None, interval, show_map: true }
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
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return vec![Effect::Quit];
        }
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
                        if self.polling.as_deref() == Some(number.as_str()) {
                            self.polling = None;
                        }
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
                KeyCode::Char('m') => {
                    self.show_map = !self.show_map;
                    vec![]
                }
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
            KeyCode::Char('R') => vec![Effect::Send(PollCommand::RefreshAll)],
            KeyCode::Char('m') => {
                self.show_map = !self.show_map;
                vec![]
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
            Some(number) => vec![Effect::Send(PollCommand::Refresh(number))],
            None => vec![],
        }
    }

    /// `Started` is the single source of truth for the spinner: it fires for
    /// every poll, scheduled or user-initiated. `Finished` applies the result.
    pub fn apply_poll_event(&mut self, event: PollEvent, now: DateTime<Utc>) -> Vec<Effect> {
        match event {
            PollEvent::Started(number) => {
                self.polling = Some(number);
                vec![]
            }
            PollEvent::Finished(r) => self.apply_poll_result(r, now),
            PollEvent::Geocoded { key, coord } => {
                self.cache.geo.insert(key, coord);
                vec![Effect::SaveState]
            }
        }
    }

    fn apply_poll_result(&mut self, r: PollResult, now: DateTime<Utc>) -> Vec<Effect> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Status, TrackEvent, Tracking};
    use chrono::TimeZone;
    use crossterm::event::KeyModifiers;

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
    fn ctrl_c_quits_in_every_mode() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let mut app = app_with(&["A"]);
        assert_eq!(app.handle_key(ctrl_c, now()), vec![Effect::Quit]);

        app.handle_key(key('a'), now());
        assert!(matches!(app.mode, Mode::Adding(_)));
        assert_eq!(app.handle_key(ctrl_c, now()), vec![Effect::Quit]);

        app.mode = Mode::Normal;
        app.handle_key(key('d'), now());
        assert!(matches!(app.mode, Mode::ConfirmRemove));
        assert_eq!(app.handle_key(ctrl_c, now()), vec![Effect::Quit]);

        app.mode = Mode::Detail { scroll: 0 };
        assert_eq!(app.handle_key(ctrl_c, now()), vec![Effect::Quit]);
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
        assert!(app.polling.is_none());
        app.apply_poll_event(PollEvent::Started("RB123456789CN".into()), now());
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
    fn remove_clears_polling_for_that_parcel() {
        let mut app = app_with(&["A"]);
        app.polling = Some("A".into());
        app.handle_key(key('d'), now());
        app.handle_key(key('y'), now());
        assert!(app.polling.is_none());

        let mut app = app_with(&["A", "B"]);
        app.polling = Some("A".into());
        app.handle_key(key('j'), now());
        app.handle_key(key('d'), now());
        app.handle_key(key('y'), now());
        assert_eq!(app.polling.as_deref(), Some("A"));
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
            events: vec![TrackEvent { time: Some(now()), description: "Posted".into(), location: None, translated: None }],
            fetched_at: now(),
            attributes: vec![],
            tracking_url: None,
        };
        let effects = app.apply_poll_event(PollEvent::Finished(PollResult { number: "A".into(), result: Ok(tracking.clone()) }), now());
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
        let tracking = Tracking { number: "A".into(), carrier: None, status: Status::Pending, events: vec![], fetched_at: now(), attributes: vec![], tracking_url: None };
        app.apply_poll_event(PollEvent::Finished(PollResult { number: "A".into(), result: Ok(tracking.clone()) }), now());
        app.apply_poll_event(PollEvent::Finished(PollResult { number: "A".into(), result: Err("boom".into()) }), now());
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
        let effects = app.apply_poll_event(PollEvent::Finished(PollResult { number: "Z".into(), result: Err("x".into()) }), now());
        assert!(effects.is_empty());
        assert!(app.state_for("Z").is_none());
    }

    #[test]
    fn started_event_sets_polling_and_finished_clears_it() {
        let mut app = app_with(&["A"]);
        assert!(app.polling.is_none());
        let effects = app.apply_poll_event(PollEvent::Started("A".into()), now());
        assert!(effects.is_empty());
        assert_eq!(app.polling.as_deref(), Some("A"));
        let tracking = Tracking { number: "A".into(), carrier: None, status: Status::Pending, events: vec![], fetched_at: now(), attributes: vec![], tracking_url: None };
        app.apply_poll_event(PollEvent::Finished(PollResult { number: "A".into(), result: Ok(tracking) }), now());
        assert!(app.polling.is_none());
    }

    #[test]
    fn m_toggles_show_map_in_normal_and_detail() {
        let mut app = app_with(&["A"]);
        assert!(app.show_map, "defaults to shown");

        assert!(app.handle_key(key('m'), now()).is_empty());
        assert!(!app.show_map);
        assert!(app.handle_key(key('m'), now()).is_empty());
        assert!(app.show_map);

        app.handle_key(code(KeyCode::Enter), now());
        assert!(matches!(app.mode, Mode::Detail { .. }));
        assert!(app.handle_key(key('m'), now()).is_empty());
        assert!(!app.show_map);
        assert!(matches!(app.mode, Mode::Detail { .. }), "toggling the map does not leave Detail mode");

        app.mode = Mode::Normal;
        app.show_map = true;
        app.handle_key(key('a'), now());
        assert!(matches!(app.mode, Mode::Adding(_)));
        assert!(app.handle_key(key('m'), now()).is_empty());
        assert!(app.show_map, "m is text input while adding, not a map toggle");
        match &app.mode {
            Mode::Adding(input) => assert_eq!(input.value(), "m"),
            other => panic!("expected Adding, got {other:?}"),
        }
    }

    #[test]
    fn geocoded_event_updates_cache_and_saves() {
        let mut app = app_with(&["A"]);
        assert!(app.cache.geo.is_empty());

        let effects = app.apply_poll_event(PollEvent::Geocoded { key: "Shenzhen".into(), coord: Some((22.5, 114.0)) }, now());
        assert_eq!(effects, vec![Effect::SaveState]);
        assert_eq!(app.cache.geo.get("Shenzhen"), Some(&Some((22.5, 114.0))));

        let effects = app.apply_poll_event(PollEvent::Geocoded { key: "Nowhere".into(), coord: None }, now());
        assert_eq!(effects, vec![Effect::SaveState]);
        assert_eq!(app.cache.geo.get("Nowhere"), Some(&None), "a miss is cached too");
    }

    #[test]
    fn splits_number_and_label() {
        assert_eq!(split_number_and_label("  ab12  my thing "), ("AB12".into(), Some("my thing".into())));
        assert_eq!(split_number_and_label("ab12"), ("AB12".into(), None));
        assert_eq!(split_number_and_label("   "), ("".into(), None));
    }
}
