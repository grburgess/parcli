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
