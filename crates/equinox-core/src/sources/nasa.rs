//! NASA Astronomy Picture of the Day (APOD): https://api.nasa.gov/planetary/apod

use std::future::Future;
use std::pin::Pin;

use anyhow::{anyhow, Context, Result};
use gettextrs::gettext;
use chrono::NaiveDate;
use serde_json::Value;

use crate::http::HttpClient;
use crate::source::{FetchResult, SettingSpec, Source, SourceSettings};

const API_URL: &str = "https://api.nasa.gov/planetary/apod";

pub struct NasaApodSource;

impl Source for NasaApodSource {
    fn id(&self) -> &'static str {
        "nasa"
    }

    fn display_name(&self) -> String {
        gettext("NASA")
    }

    fn settings(&self) -> Vec<SettingSpec> {
        vec![
            SettingSpec::String {
                key: "api_key",
                label: gettext("API key (optional)"),
                default: "DEMO_KEY",
            },
            SettingSpec::Bool {
                key: "hd",
                label: gettext("Use high-definition image (hdurl)"),
                default: true,
            },
        ]
    }

    fn fetch<'a>(
        &'a self,
        settings: &'a SourceSettings,
        http: &'a HttpClient,
        _date: NaiveDate,
    ) -> Pin<Box<dyn Future<Output = Result<FetchResult>> + 'a>> {
        Box::pin(async move {
            let api_key = settings.get_str("api_key", "DEMO_KEY");
            let hd = settings.get_bool("hd", true);
            let url = format!("{API_URL}?api_key={api_key}&hd={hd}");
            let body = http.get_ok(&url).await?;
            let json: Value = serde_json::from_slice(&body).context("failed to parse NASA response")?;
            parse_apod(&json, hd)
        })
    }
}

/// Parse an APOD response. Returns an error when the day's entry is a video
/// (`media_type != image`).
fn parse_apod(json: &Value, hd: bool) -> Result<FetchResult> {
    let media_type = json
        .get("media_type")
        .and_then(|v| v.as_str())
        .unwrap_or("image");
    if media_type != "image" {
        return Err(anyhow!(format!(
            "{}: {media_type}",
            gettext("Today's APOD entry is a video, skipped")
        )));
    }
    let image_url = if hd {
        json.get("hdurl")
            .and_then(|v| v.as_str())
            .or_else(|| json.get("url").and_then(|v| v.as_str()))
    } else {
        json.get("url").and_then(|v| v.as_str())
    }
    .ok_or_else(|| anyhow!("APOD response missing image URL"))?
    .to_owned();

    let title = json
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| gettext("NASA"));
    let copyright = json.get("copyright").and_then(|v| v.as_str()).map(ToOwned::to_owned);
    let mut extra = std::collections::HashMap::new();
    if let Some(explanation) = json.get("explanation").and_then(|v| v.as_str()) {
        extra.insert("summary".to_owned(), explanation.to_owned());
    }
    // `date` is the image's own date (e.g. "2026-08-03"); the file name
    // should use it rather than the download time.
    let date = json
        .get("date")
        .and_then(|v| v.as_str())
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
    if let Some(d) = &date {
        extra.insert("date".to_owned(), d.format("%Y-%m-%d").to_string());
    }
    Ok(FetchResult {
        image_url,
        title,
        copyright,
        date,
        extra,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_apod;
    use serde_json::json;

    // Verified response captured 2026-08-04 (excerpt)
    const FIXTURE: &str = r#"{"copyright":"Tom Burnett","date":"2026-08-03","explanation":"Vaporizing meteors and the Lacerta nebula.","hdurl":"https://apod.nasa.gov/apod/image/2608/MeteorPhotobombsLacerta_1024.jpg","media_type":"image","service_version":"v1","title":"Vaporizing Meteor Photobombs the Lacerta Nebula","url":"https://apod.nasa.gov/apod/image/2608/MeteorPhotobombsLacerta_1080.jpg"}"#;

    #[test]
    fn parses_real_fixture() {
        let json: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let r = parse_apod(&json, true).unwrap();
        assert!(r.image_url.contains("_1024.jpg"));
        assert_eq!(r.title, "Vaporizing Meteor Photobombs the Lacerta Nebula");
        assert_eq!(r.copyright.as_deref(), Some("Tom Burnett"));
        assert!(r.extra.contains_key("summary"));
    }

    #[test]
    fn non_hd_uses_regular_url() {
        let json: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let r = parse_apod(&json, false).unwrap();
        assert!(r.image_url.contains("_1080.jpg"));
    }

    #[test]
    fn video_is_skipped() {
        let json = json!({"media_type": "video", "url": "https://x/y.mp4", "title": "v"});
        assert!(parse_apod(&json, true).is_err());
    }

    #[test]
    fn missing_url_is_error() {
        assert!(parse_apod(&json!({"media_type": "image", "title": "t"}), true).is_err());
    }
}
