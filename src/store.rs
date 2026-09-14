use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::geo::GeoCache;
use crate::provider::Tracking;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parcel {
    pub number: String,
    pub label: Option<String>,
    pub added: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParcelList {
    #[serde(default)]
    pub parcels: Vec<Parcel>,
    #[serde(default)]
    pub home: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParcelState {
    pub tracking: Option<Tracking>,
    pub last_error: Option<String>,
    pub failures: u32,
    pub next_poll: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StateCache {
    #[serde(default)]
    pub by_number: HashMap<String, ParcelState>,
    #[serde(default)]
    pub geo: GeoCache,
}

pub struct Paths {
    pub parcels: PathBuf,
    pub state: PathBuf,
}

/// Write `bytes` to `path` via a sibling temp file and rename, so a crash
/// mid-write never leaves a truncated file. Creates parent directories.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let file_name = path.file_name().context("path has no file name")?.to_string_lossy();
    let tmp = parent.join(format!(".{file_name}.tmp"));
    fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

fn read_if_exists(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn normalize(number: &str) -> String {
    number.trim().to_ascii_uppercase()
}

impl ParcelList {
    pub fn load(path: &Path) -> Result<Self> {
        match read_if_exists(path)? {
            None => Ok(Self::default()),
            Some(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serializing parcel list")?;
        atomic_write(path, text.as_bytes())
    }

    /// Returns false (and changes nothing) for an empty number or a duplicate.
    pub fn add(&mut self, number: &str, label: Option<&str>, now: DateTime<Utc>) -> bool {
        let number = normalize(number);
        if number.is_empty() || self.contains(&number) {
            return false;
        }
        let label = label.map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned);
        self.parcels.push(Parcel { number, label, added: now });
        true
    }

    pub fn remove(&mut self, number: &str) -> bool {
        let before = self.parcels.len();
        self.parcels.retain(|p| p.number != number);
        self.parcels.len() != before
    }

    pub fn contains(&self, number: &str) -> bool {
        self.parcels.iter().any(|p| p.number == number)
    }
}

impl StateCache {
    pub fn load(path: &Path) -> Result<Self> {
        match read_if_exists(path)? {
            None => Ok(Self::default()),
            Some(text) => serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self).context("serializing state cache")?;
        atomic_write(path, text.as_bytes())
    }
}

impl Paths {
    pub fn discover() -> Result<Paths> {
        let dirs = directories::ProjectDirs::from("", "", "parcli")
            .context("could not determine a home directory for config files")?;
        Ok(Paths {
            parcels: dirs.config_dir().join("parcels.toml"),
            state: dirs.cache_dir().join("state.json"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Status;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap()
    }

    #[test]
    fn missing_files_load_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let list = ParcelList::load(&dir.path().join("parcels.toml")).unwrap();
        assert!(list.parcels.is_empty());
        let cache = StateCache::load(&dir.path().join("state.json")).unwrap();
        assert!(cache.by_number.is_empty());
    }

    #[test]
    fn parcel_list_round_trips_and_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("parcels.toml");
        let mut list = ParcelList::default();
        assert!(list.add("RB123456789CN", Some("camera"), now()));
        assert!(!list.add(" RB123456789CN ", None, now()), "dup is rejected");
        list.save(&path).unwrap();
        let loaded = ParcelList::load(&path).unwrap();
        assert_eq!(loaded, list);
        assert_eq!(loaded.parcels[0].label.as_deref(), Some("camera"));
        assert!(!dir.path().join("nested").read_dir().unwrap().any(|e| {
            e.unwrap().file_name().to_string_lossy().contains(".tmp")
        }));
    }

    #[test]
    fn add_trims_uppercases_and_rejects_duplicates_and_empty() {
        let mut list = ParcelList::default();
        assert!(list.add("  rb123456789cn ", None, now()));
        assert_eq!(list.parcels[0].number, "RB123456789CN");
        assert!(!list.add("RB123456789CN", None, now()));
        assert!(!list.add("   ", None, now()));
        assert_eq!(list.parcels.len(), 1);
        assert!(list.contains("RB123456789CN"));
    }

    #[test]
    fn remove_returns_whether_present() {
        let mut list = ParcelList::default();
        list.add("A1", None, now());
        assert!(list.remove("A1"));
        assert!(!list.remove("A1"));
        assert!(list.parcels.is_empty());
    }

    #[test]
    fn state_cache_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::default();
        cache.by_number.insert(
            "A1".into(),
            ParcelState {
                tracking: Some(Tracking {
                    number: "A1".into(),
                    carrier: Some("China Post".into()),
                    status: Status::InTransit,
                    events: vec![],
                    fetched_at: now(),
                    attributes: vec![],
                    tracking_url: None,
                }),
                last_error: None,
                failures: 0,
                next_poll: now(),
            },
        );
        cache.save(&path).unwrap();
        assert_eq!(StateCache::load(&path).unwrap(), cache);
    }

    #[test]
    fn corrupt_file_is_an_error_naming_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.toml");
        fs::write(&path, "this = [not valid").unwrap();
        let err = ParcelList::load(&path).unwrap_err().to_string();
        assert!(err.contains("parcels.toml"), "{err}");
    }

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
        assert!(cache.geo.is_empty());
    }

    #[test]
    fn home_round_trips_through_toml_and_defaults_to_none_on_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.toml");
        let list = ParcelList { home: Some("Roissy CDG".into()), ..Default::default() };
        list.save(&path).unwrap();
        assert_eq!(ParcelList::load(&path).unwrap(), list);

        // MVP-era file predating the `home` field.
        fs::write(&path, "parcels = []\n").unwrap();
        let loaded = ParcelList::load(&path).unwrap();
        assert_eq!(loaded.home, None);
    }

    #[test]
    fn geo_round_trips_through_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::default();
        cache.geo.insert("Shenzhen".into(), Some((22.5, 114.0)));
        cache.geo.insert("Web Services".into(), None);
        cache.save(&path).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["geo"]["Shenzhen"], serde_json::json!([22.5, 114.0]));
        assert_eq!(value["geo"]["Web Services"], serde_json::Value::Null);

        assert_eq!(StateCache::load(&path).unwrap(), cache);
    }
}
