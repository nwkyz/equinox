//! Lorem Picsum: random photography from Unsplash (https://picsum.photos).
//!
//! The `/v2/list` catalogue is paginated (30–100 per page); each fetch picks a
//! random page and a random entry, then requests that specific image at the
//! configured size, optionally grayscale/blurred. The entry's metadata (author,
//! original dimensions, the Unsplash source page) is carried into the sidecar.
//!
//! Like the other rotating libraries, this source is stateless and always
//! fetches on a scheduled tick (`daily() == false`); repeated picks of an
//! already-stored image are dropped by the content-hash dedup.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use anyhow::{anyhow, Context, Result};
use chrono::NaiveDate;
use gettextrs::gettext;
use serde_json::Value;

use crate::http::HttpClient;
use crate::source::{FetchResult, SettingSpec, Source, SourceSettings};

/// Catalogue endpoint (metadata for every image: id/author/width/height/url).
const LIST_BASE: &str = "https://picsum.photos/v2/list";
/// The catalogue currently holds ~1000 images; the page range covers it.
const MAX_PAGE: i64 = 10;
const PAGE_SIZE: i64 = 100;
/// Fallback size when neither a manual value nor the auto-detected screen
/// size is available (headless first fetch before the GUI ever ran).
const FALLBACK_W: i64 = 1920;
const FALLBACK_H: i64 = 1080;

pub struct PicsumSource;

impl Source for PicsumSource {
    fn id(&self) -> &'static str {
        "picsum"
    }

    fn display_name(&self) -> String {
        // Kept in English by design (and thus not extracted for translation).
        "Lorem Picsum".to_owned()
    }

    fn settings(&self) -> Vec<SettingSpec> {
        vec![
            SettingSpec::Int {
                key: "width",
                label: gettext("Width (0 = auto)"),
                default: 0,
                min: 0,
                max: 7680,
            },
            SettingSpec::Int {
                key: "height",
                label: gettext("Height (0 = auto)"),
                default: 0,
                min: 0,
                max: 4320,
            },
            SettingSpec::Bool {
                key: "grayscale",
                label: gettext("Grayscale"),
                default: false,
            },
            SettingSpec::Bool {
                key: "blur",
                label: gettext("Blur"),
                default: false,
            },
        ]
    }

    /// A rotating library, not a "one new image per calendar day" feed.
    fn daily(&self) -> bool {
        false
    }

    fn fetch<'a>(
        &'a self,
        settings: &'a SourceSettings,
        http: &'a HttpClient,
        _date: NaiveDate,
    ) -> Pin<Box<dyn Future<Output = Result<FetchResult>> + 'a>> {
        Box::pin(async move {
            let (w, h) = resolve_size(settings);
            let effects = effects_query(
                settings.get_bool("grayscale", false),
                settings.get_bool("blur", false),
            );

            // Pick a random page; fall back to page 1 if it is beyond the last.
            let page = rand::random_range(1..=MAX_PAGE);
            let url = format!("{LIST_BASE}?page={page}&limit={PAGE_SIZE}");
            let body = http.get_ok(&url).await?;
            let json: Value =
                serde_json::from_slice(&body).context("failed to parse Lorem Picsum list")?;
            let entries = json
                .as_array()
                .ok_or_else(|| anyhow!("Lorem Picsum list is not an array"))?;
            if entries.is_empty() {
                return Err(anyhow!("Lorem Picsum list is empty on page {page}"));
            }
            let idx = rand::random_range(0..entries.len());
            parse_entry(&entries[idx], w, h, &effects)
        })
    }
}

/// Resolve the requested image size: the manual width/height win; 0 ("auto")
/// falls back to a sane default (the GUI normally fills the screen size in).
fn resolve_size(settings: &SourceSettings) -> (i64, i64) {
    let w = settings.get_i64("width", 0);
    let h = settings.get_i64("height", 0);
    let w = if w > 0 { w } else { FALLBACK_W };
    let h = if h > 0 { h } else { FALLBACK_H };
    (w, h)
}

/// Build the URL query suffix from the effect toggles ("" when none).
fn effects_query(grayscale: bool, blur: bool) -> String {
    let mut q: Vec<&str> = Vec::new();
    if grayscale {
        q.push("grayscale");
    }
    if blur {
        q.push("blur");
    }
    if q.is_empty() {
        String::new()
    } else {
        format!("?{}", q.join("&"))
    }
}

/// Build a [`FetchResult`] from one catalogue entry: the image's **id** is the
/// title, the **author** becomes the caption/description, and the Unsplash
/// source page is carried into the sidecar metadata.
fn parse_entry(entry: &Value, w: i64, h: i64, effects: &str) -> Result<FetchResult> {
    let id = entry
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Lorem Picsum entry missing id"))?
        .to_owned();
    let image_url = format!("https://picsum.photos/id/{id}/{w}/{h}{effects}");
    let mut extra = HashMap::new();
    // The author is shown as the caption in the gallery/fullscreen viewer.
    if let Some(author) = entry.get("author").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        extra.insert("summary".to_owned(), author.to_owned());
    }
    if let Some(url) = entry.get("url").and_then(|v| v.as_str()) {
        extra.insert("source".to_owned(), url.to_owned());
    }
    Ok(FetchResult {
        image_url,
        title: id,
        copyright: None,
        date: None,
        extra,
    })
}

#[cfg(test)]
mod tests {
    use super::{effects_query, parse_entry, resolve_size};
    use serde_json::json;
    use crate::source::SourceSettings;

    // Real entry captured 2026-09-08 from /v2/list.
    const FIXTURE: &str = r#"[{"id":"1005","author":"Matthew Wiebe","width":4000,"height":6000,"url":"https://unsplash.com/photos/N7XodRrbzS0","download_url":"https://picsum.photos/id/1005/4000/6000"}]"#;

    #[test]
    fn parses_real_entry() {
        let entries: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let r = parse_entry(&entries[0], 1920, 1080, "").unwrap();
        assert_eq!(r.image_url, "https://picsum.photos/id/1005/1920/1080");
        assert_eq!(r.title, "1005");
        assert_eq!(r.extra.get("summary").map(String::as_str), Some("Matthew Wiebe"));
        assert_eq!(
            r.extra.get("source").map(String::as_str),
            Some("https://unsplash.com/photos/N7XodRrbzS0")
        );
        assert!(r.date.is_none());
    }

    #[test]
    fn effects_query_combines() {
        assert_eq!(effects_query(false, false), "");
        assert_eq!(effects_query(true, false), "?grayscale");
        assert_eq!(effects_query(false, true), "?blur");
        assert_eq!(effects_query(true, true), "?grayscale&blur");
    }

    #[test]
    fn missing_id_is_error() {
        assert!(parse_entry(&json!({"author": "x"}), 1, 1, "").is_err());
    }

    #[test]
    fn auto_size_falls_back() {
        assert_eq!(resolve_size(&SourceSettings::default()), (1920, 1080));
        let mut s = SourceSettings::default();
        s.set("width", json!(2560));
        s.set("height", json!(1440));
        assert_eq!(resolve_size(&s), (2560, 1440));
    }
}