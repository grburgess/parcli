use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::{
    EventLoadingFinished, EventResponseReceived, GetResponseBodyParams, SetUserAgentOverrideParams,
};
use futures::StreamExt;
use serde::Deserialize;
use tokio::task::JoinHandle;

use super::{classify_status, Provider, TrackEvent, Tracking};

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
    events.sort_by_key(|a| std::cmp::Reverse(a.1.time));

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

const WIDGET_URL: &str = "https://parcelsapp.com/widget";
const API_PATH: &str = "api/v2/parcels";
/// After the first API response, keep listening this long for a later one
/// (the widget sometimes re-queries once carriers respond).
const SETTLE: Duration = Duration::from_secs(3);

pub struct ParcelsAppProvider {
    browser: Browser,
    handler: JoinHandle<()>,
    timeout: Duration,
    user_agent: String,
}

impl ParcelsAppProvider {
    /// Launch one headless browser. Honors `PARCLI_CHROME`; otherwise uses
    /// chromiumoxide's executable detection.
    pub async fn launch(timeout: Duration) -> Result<Self> {
        let mut builder = BrowserConfig::builder()
            .request_timeout(timeout)
            .new_headless_mode()
            .hide()
            .window_size(1280, 900);
        if let Ok(path) = std::env::var("PARCLI_CHROME") {
            builder = builder.chrome_executable(path);
        }
        let config = builder.build().map_err(|e| {
            anyhow::anyhow!("{e}. Set PARCLI_CHROME to your Chrome/Chromium executable.")
        })?;
        let (mut browser, mut events) = Browser::launch(config).await.map_err(|e| {
            anyhow::anyhow!("launching Chrome failed: {e}. Set PARCLI_CHROME to your Chrome/Chromium executable.")
        })?;
        let handler = tokio::spawn(async move { while events.next().await.is_some() {} });
        // parcelsapp.com rejects the default HeadlessChrome UA with NO_DATA; masquerade as
        // regular Chrome (headless already hides navigator.webdriver via `.hide()`).
        let user_agent = match browser.user_agent().await {
            Ok(ua) => ua.replace("HeadlessChrome", "Chrome"),
            Err(e) => {
                let _ = browser.close().await;
                let _ = browser.wait().await;
                handler.abort();
                return Err(e).context("querying browser user agent");
            }
        };
        Ok(Self { browser, handler, timeout, user_agent })
    }

    pub async fn close(mut self) {
        let _ = self.browser.close().await;
        let _ = self.browser.wait().await;
        self.handler.abort();
    }

    async fn track_inner(&self, number: &str) -> Result<Tracking> {
        let page = self.browser.new_page("about:blank").await.context("opening page")?;
        let result = async {
            page.execute(SetUserAgentOverrideParams::new(self.user_agent.clone())).await?;

            let mut responses = page.event_listener::<EventResponseReceived>().await?;
            let mut finished = page.event_listener::<EventLoadingFinished>().await?;

            page.goto(WIDGET_URL).await.context("opening widget page")?;
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

    /// Requires Chrome and network. Run with: cargo test live_ -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_tracks_archive_number() {
        let provider = ParcelsAppProvider::launch(std::time::Duration::from_secs(90)).await.unwrap();
        let t = provider.track("RB123456789CN").await.unwrap();
        provider.close().await;
        assert_eq!(t.number, "RB123456789CN");
        assert!(!t.events.is_empty(), "expected at least one event, got {t:?}");
    }
}
