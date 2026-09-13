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

    /// Test-only accessor; production code never inspects a scheduled time directly.
    #[cfg(test)]
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
