//! Google Earth View satellite/aerial landscapes.
//!
//! The catalogue is a community-mirrored JSON list of 2600+ locations
//! (region/country/map/image/attribution), served plain from GitHub. Each
//! fetch downloads the list once and picks one location at random — the
//! source is stateless (matching the `Source` contract), so there is no
//! "next" cursor to persist. Repeated picks of the same image are already
//! handled by the content-hash deduplication in storage.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use anyhow::{anyhow, Context, Result};
use chrono::NaiveDate;
use serde_json::Value;

use crate::http::HttpClient;
use crate::source::{FetchResult, SettingSpec, Source, SourceSettings};

/// Community mirror of the official Google Earth View archive
/// (alexpersian/earthview). A plain JSON array of location objects, ~870 KB
/// for ~2600 entries.
const CATALOG_URL: &str = "https://github.com/alexpersian/earthview/raw/master/earthview.json";

pub struct EarthViewSource;

impl Source for EarthViewSource {
    fn id(&self) -> &'static str {
        "earthview"
    }

    fn display_name(&self) -> String {
        // Kept in English by design (and thus not extracted for translation).
        "Google Earth".to_owned()
    }

    /// Zero-config: every fetch picks a random location from the catalogue.
    fn settings(&self) -> Vec<SettingSpec> {
        vec![]
    }

    /// A rotating library, not a "one new image per calendar day" feed —
    /// every scheduled update should fetch a new random pick (spotlight.rs
    /// behaves the same way).
    fn daily(&self) -> bool {
        false
    }

    fn fetch<'a>(
        &'a self,
        _settings: &'a SourceSettings,
        http: &'a HttpClient,
        _date: NaiveDate,
    ) -> Pin<Box<dyn Future<Output = Result<FetchResult>> + 'a>> {
        Box::pin(async move {
            let body = http.get_ok(CATALOG_URL).await?;
            let json: Value =
                serde_json::from_slice(&body).context("failed to parse Earth View catalogue")?;
            let entries = json
                .as_array()
                .ok_or_else(|| anyhow!("Earth View catalogue is not a list"))?;
            if entries.is_empty() {
                return Err(anyhow!("Earth View catalogue is empty"));
            }
            let idx = rand::random_range(0..entries.len());
            parse_entry(&entries[idx])
        })
    }
}

/// Build a [`FetchResult`] from one catalogue entry.
fn parse_entry(entry: &Value) -> Result<FetchResult> {
    let image_url = entry
        .get("image")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Earth View entry missing image field"))?
        .to_owned();
    let region = entry.get("region").and_then(|v| v.as_str()).unwrap_or("").to_owned();
    let country = entry.get("country").and_then(|v| v.as_str()).unwrap_or("").to_owned();
    let title = location_title(&region, &country);
    let copyright = entry.get("attribution").and_then(|v| v.as_str()).map(ToOwned::to_owned);
    let mut extra = HashMap::new();
    // The location doubles as the caption shown in the gallery/fullscreen
    // viewer (which reads `description`/`summary` from the sidecar).
    extra.insert("summary".to_owned(), title.clone());
    if let Some(map) = entry.get("map").and_then(|v| v.as_str()) {
        extra.insert("map".to_owned(), map.to_owned());
    }
    Ok(FetchResult {
        image_url,
        title,
        copyright,
        date: None,
        extra,
    })
}

/// Location title for an entry: "Region, Country" when a region is known,
/// otherwise just the country.
fn location_title(region: &str, country: &str) -> String {
    match (region, country) {
        ("", "") => "Google Earth".to_owned(),
        (r, c) if c.is_empty() => r.to_owned(),
        (r, c) if r.is_empty() => c.to_owned(),
        (r, c) => format!("{r}, {c}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{location_title, parse_entry};
    use serde_json::json;

    // Real entries captured 2026-09-08 from the catalogue.
    const FIXTURE: &str = r#"[
      {
        "id": "1003",
        "region": "",
        "country": "Australia",
        "map": "https://www.google.com/maps/@-10.040181,143.560709,11z/data=!3m1!1e3",
        "image": "https://www.gstatic.com/prettyearth/assets/full/1003.jpg",
        "attribution": "©2019 Cnes/Spot Image, Maxar Technologies, Landsat, U.S. Geological Survey"
      },
      {
        "id": "1004",
        "region": "Tamarugal",
        "country": "Chile",
        "map": "https://www.google.com/maps/@-19.140249,-68.683995,14z/data=!3m1!1e3",
        "image": "https://www.gstatic.com/prettyearth/assets/full/1004.jpg",
        "attribution": "©2019 CNES / Astrium, Cnes/Spot Image, Maxar Technologies"
      }
    ]"#;

    #[test]
    fn parses_real_entry_with_region() {
        let entries: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let r = parse_entry(&entries[1]).unwrap();
        assert_eq!(r.image_url, "https://www.gstatic.com/prettyearth/assets/full/1004.jpg");
        assert_eq!(r.title, "Tamarugal, Chile");
        assert_eq!(
            r.copyright.as_deref(),
            Some("©2019 CNES / Astrium, Cnes/Spot Image, Maxar Technologies")
        );
        assert_eq!(r.extra.get("summary").map(String::as_str), Some("Tamarugal, Chile"));
        assert!(r.extra.get("map").unwrap().starts_with("https://www.google.com/maps/"));
        assert!(r.date.is_none());
    }

    #[test]
    fn parses_entry_without_region() {
        let entries: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let r = parse_entry(&entries[0]).unwrap();
        assert_eq!(r.title, "Australia");
    }

    #[test]
    fn missing_image_is_error() {
        assert!(parse_entry(&json!({"id": "1", "country": "X"})).is_err());
    }

    #[test]
    fn location_title_rules() {
        assert_eq!(location_title("Tamarugal", "Chile"), "Tamarugal, Chile");
        assert_eq!(location_title("", "Chile"), "Chile");
        assert_eq!(location_title("Tamarugal", ""), "Tamarugal");
        assert_eq!(location_title("", ""), "Google Earth");
    }
}