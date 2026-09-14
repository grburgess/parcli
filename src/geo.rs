use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

pub type Coord = (f64, f64);
pub type GeoCache = HashMap<String, Option<Coord>>;

const ENDPOINT: &str = "https://nominatim.openstreetmap.org/search";
const MIN_INTERVAL: Duration = Duration::from_millis(1100);

#[async_trait]
pub trait Geocoder: Send + Sync {
    /// `Ok(None)` = no match (cacheable). Err = transient failure.
    async fn geocode(&self, query: &str, settlement_only: bool) -> Result<Option<Coord>>;
}

pub struct NominatimGeocoder {
    client: reqwest::Client,
    last_request: Mutex<Option<Instant>>,
}

impl NominatimGeocoder {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("parcli/", env!("CARGO_PKG_VERSION"), " (github.com/jburgess/parcli)"))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .context("building HTTP client")?;
        Ok(Self { client, last_request: Mutex::new(None) })
    }
}

#[async_trait]
impl Geocoder for NominatimGeocoder {
    async fn geocode(&self, query: &str, settlement_only: bool) -> Result<Option<Coord>> {
        let mut last_request = self.last_request.lock().await;
        if let Some(last) = *last_request {
            let elapsed = last.elapsed();
            if elapsed < MIN_INTERVAL {
                tokio::time::sleep(MIN_INTERVAL - elapsed).await;
            }
        }

        let mut query_params = vec![("q", query), ("format", "jsonv2"), ("limit", "1")];
        if settlement_only {
            query_params.push(("featureType", "settlement"));
        }

        // Stamped before sending, not after, so a request that times out still
        // counts toward the spacing between requests.
        *last_request = Some(Instant::now());
        drop(last_request);

        let body = self
            .client
            .get(ENDPOINT)
            .query(&query_params)
            .send()
            .await
            .context("geocoding request")?
            .text()
            .await
            .context("reading geocoding response")?;

        parse_nominatim(&body)
    }
}

#[derive(Deserialize)]
struct NominatimResult {
    lat: String,
    lon: String,
}

/// Parse a Nominatim `/search` response: a JSON array of results with `lat`/`lon`
/// as strings. An empty array is `Ok(None)`; malformed JSON or non-numeric
/// coordinates are errors.
pub fn parse_nominatim(body: &str) -> Result<Option<Coord>> {
    let results: Vec<NominatimResult> = serde_json::from_str(body).context("geocoding response is not JSON")?;
    let Some(first) = results.into_iter().next() else {
        return Ok(None);
    };
    let lat: f64 = first.lat.parse().with_context(|| format!("invalid latitude {:?}", first.lat))?;
    let lon: f64 = first.lon.parse().with_context(|| format!("invalid longitude {:?}", first.lon))?;
    Ok(Some((lat, lon)))
}

/// Trim and collapse internal whitespace to single spaces.
pub fn normalize_place(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_first_result() {
        let body = r#"[{"place_id":1,"lat":"22.5445741","lon":"114.0545429","display_name":"深圳市","addresstype":"city"}]"#;
        assert_eq!(parse_nominatim(body).unwrap(), Some((22.5445741, 114.0545429)));
    }

    #[test]
    fn empty_array_is_none() {
        assert_eq!(parse_nominatim("[]").unwrap(), None);
    }

    #[test]
    fn bad_json_or_bad_numbers_are_errors() {
        assert!(parse_nominatim("<html>").is_err());
        assert!(parse_nominatim(r#"[{"lat":"x","lon":"1"}]"#).is_err());
    }

    #[test]
    fn normalize_place_collapses_whitespace() {
        assert_eq!(normalize_place("  Roissy   CDG \n"), "Roissy CDG");
    }

    /// Network: cargo test live_geocode -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_geocode_shenzhen_and_junk() {
        let g = NominatimGeocoder::new().unwrap();
        let c = g.geocode("Shenzhen", true).await.unwrap().unwrap();
        assert!((c.0 - 22.5).abs() < 1.0 && (c.1 - 114.0).abs() < 1.0, "{c:?}");
        assert_eq!(g.geocode("Web Services", true).await.unwrap(), None);
    }
}
