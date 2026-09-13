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
