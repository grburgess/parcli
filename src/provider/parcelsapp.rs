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
