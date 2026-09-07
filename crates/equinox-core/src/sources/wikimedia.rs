//! Wikimedia Commons Picture of the Day (POTD): https://commons.wikimedia.org

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use anyhow::{anyhow, Context, Result};
use gettextrs::gettext;
use chrono::NaiveDate;
use serde_json::Value;

use crate::http::HttpClient;
use crate::source::{FetchResult, SettingSpec, Source, SourceSettings};

const API_URL: &str = "https://commons.wikimedia.org/w/api.php";

pub struct WikimediaPotdSource;

impl Source for WikimediaPotdSource {
    fn id(&self) -> &'static str {
        "wikimedia"
    }

    fn display_name(&self) -> String {
        gettext("Wikimedia")
    }

    fn settings(&self) -> Vec<SettingSpec> {
        vec![SettingSpec::Int {
            key: "width",
            label: gettext("Thumbnail width"),
            default: 1920,
            min: 640,
            max: 3840,
        }]
    }

    fn fetch<'a>(
        &'a self,
        settings: &'a SourceSettings,
        http: &'a HttpClient,
        date: NaiveDate,
    ) -> Pin<Box<dyn Future<Output = Result<FetchResult>> + 'a>> {
        Box::pin(async move {
            let width = settings.get_i64("width", 1920).clamp(640, 3840);
            let title = format!("Template:Potd/{date}");
            let url = format!(
                "{API_URL}?action=query&format=json&generator=images&titles={title}\
                 &prop=imageinfo&iiprop=url|extmetadata&iiurlwidth={width}"
            );
            let body = http.get_ok(&url).await?;
            let json: Value =
                serde_json::from_slice(&body).context("failed to parse Wikimedia response")?;
            // POTD is inherently date-based: the requested day is the image's date.
            let mut result = parse_potd(&json)?;
            result.date = Some(date);
            Ok(result)
        })
    }
}

/// Parse a POTD response, taking the first file with imageinfo.
fn parse_potd(json: &Value) -> Result<FetchResult> {
    let page = json
        .get("query")
        .and_then(|q| q.get("pages"))
        .and_then(|p| p.as_object())
        .and_then(|pages| pages.values().find(|v| v.get("imageinfo").is_some()))
        .ok_or_else(|| anyhow!("no image information in POTD response"))?;
    let info = page
        .get("imageinfo")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| anyhow!("POTD response missing imageinfo"))?;

    let image_url = info
        .get("thumburl")
        .or_else(|| info.get("url"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("POTD response missing image URL"))?
        .to_owned();
    // All extmetadata values are raw HTML (authors wrapped in <a>/<bdi>,
    // descriptions with inline links) — strip the markup before storing.
    let title = clean_meta(info, "ImageDescription")
        .filter(|s| !s.is_empty())
        .or_else(|| clean_meta(info, "ObjectName").filter(|s| !s.is_empty()))
        .unwrap_or_else(|| {
            page.get("title")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| gettext("Wikimedia"))
        });
    let copyright = clean_meta(info, "Artist").filter(|s| !s.is_empty());

    let mut extra = HashMap::new();
    for (key, label) in [
        ("ImageDescription", "description"),
        ("LicenseShortName", "license"),
        ("Artist", "author"),
    ] {
        if let Some(v) = clean_meta(info, key).filter(|s| !s.is_empty()) {
            extra.insert(label.to_owned(), v);
        }
    }
    Ok(FetchResult {
        image_url,
        title,
        copyright,
        date: None, // filled in by fetch() from the requested day
        extra,
    })
}

/// HTML-stripped extmetadata value for `key` (None when missing/empty).
fn clean_meta(info: &Value, key: &str) -> Option<String> {
    ext_meta(info, key).map(strip_html).filter(|s| !s.is_empty())
}

/// Get the `value` string field of a key under `extmetadata`.
fn ext_meta<'a>(info: &'a Value, key: &str) -> Option<&'a str> {
    info.get("extmetadata")
        .and_then(|m| m.get(key))
        .and_then(|v| v.get("value"))
        .and_then(|v| v.as_str())
}

/// Strip HTML tags and decode entities from an extmetadata value. The API
/// wraps authors in `<a>`/`<bdi>` and descriptions often contain inline
/// links; storing the raw markup would show `<a href=…>` in the GUI title /
/// caption. Only simple tag removal + entity decoding is needed here.
fn strip_html(raw: &str) -> String {
    // Remove tags (including their attributes).
    let mut text = String::with_capacity(raw.len());
    let mut in_tag = false;
    for c in raw.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    // Decode common HTML entities.
    let text = text
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    // Collapse whitespace runs left behind by removed tags.
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::parse_potd;

    use serde_json::json;

    // Verified response captured 2026-08-04 (key parts excerpted)
    fn fixture() -> serde_json::Value {
        json!({
            "query": {
                "pages": {
                    "151974955": {
                        "pageid": 151974955,
                        "ns": 6,
                        "title": "File:Rom (IT), Brucke -- 2024 -- 0732.jpg",
                        "imagerepository": "local",
                        "imageinfo": [{
                            "thumburl": "https://upload.wikimedia.org/wikipedia/commons/thumb/2/27/x/1920px-x.jpg",
                            "thumbwidth": 1920,
                            "url": "https://upload.wikimedia.org/wikipedia/commons/2/27/x.jpg",
                            "extmetadata": {
                                "ObjectName": { "value": "Rom (IT), Brucke -- 2024 -- 0732" },
                                "ImageDescription": { "value": "Ponte Vittorio Emanuele II (bridge) in Rome at the blue hour." },
                                "Artist": { "value": "Anil Öztas" },
                                "LicenseShortName": { "value": "CC BY 4.0" },
                                "Credit": { "value": "Own work, anil-oeztas.de" }
                            }
                        }]
                    }
                }
            }
        })
    }

    #[test]
    fn parses_real_fixture() {
        let json = fixture();
        let r = parse_potd(&json).unwrap();
        assert!(r.image_url.contains("1920px-"));
        assert_eq!(r.copyright.as_deref(), Some("Anil Öztas"));
        assert_eq!(r.extra.get("license").map(String::as_str), Some("CC BY 4.0"));
        assert!(r.title.contains("Ponte Vittorio"));
    }

    #[test]
    fn missing_imageinfo_is_error() {
        let json = serde_json::json!({"query": {"pages": {}}});
        assert!(parse_potd(&json).is_err());
    }

    #[test]
    fn strips_html_from_extmetadata() {
        use super::strip_html;
        // Realistic extmetadata values (2026-09 POTD): inline links, <bdi>,
        // <span> and entities.
        let desc = "Café and restaurant “Marienhöhe”, <a href=\"https://en.wikipedia.org/wiki/Norderney\" class=\"extiw\" title=\"en:Norderney\">Norderney</a>, Germany";
        assert_eq!(
            strip_html(desc),
            "Café and restaurant “Marienhöhe”, Norderney, Germany"
        );
        let artist = "<bdi><a href=\"https://www.wikidata.org/wiki/Q34788025\" class=\"extiw\" title=\"d:Q34788025\"><span title=\"German photographer and mathematician\">Dietmar Rabich</span></a></bdi>";
        assert_eq!(strip_html(artist), "Dietmar Rabich");
        assert_eq!(strip_html("Tom &amp; Jerry"), "Tom & Jerry");
    }

    #[test]
    fn clean_meta_strips_markup() {
        use serde_json::json;
        let info = json!({
            "extmetadata": {
                "Artist": {
                    "value": "<bdi><a href=\"//x\">Anil Öztas</a></bdi>"
                },
                "LicenseShortName": { "value": "CC BY 4.0" },
                "Missing": { "value": "" }
            }
        });
        assert_eq!(
            super::clean_meta(&info, "Artist").as_deref(),
            Some("Anil Öztas")
        );
        assert_eq!(super::clean_meta(&info, "LicenseShortName").as_deref(), Some("CC BY 4.0"));
        assert_eq!(super::clean_meta(&info, "Missing"), None);
    }
}
