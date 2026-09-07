//! Bing daily wallpaper: https://www.bing.com/HPImageArchive.aspx

use std::future::Future;
use std::pin::Pin;

use anyhow::{anyhow, Context, Result};
use gettextrs::gettext;
use chrono::NaiveDate;
use serde_json::Value;

use crate::http::HttpClient;
use crate::source::{FetchResult, SettingSpec, Source, SourceSettings};

const API_BASE: &str = "https://www.bing.com/HPImageArchive.aspx";
const IMAGE_BASE: &str = "https://www.bing.com";

pub struct BingSource;

impl Source for BingSource {
    fn id(&self) -> &'static str {
        "bing"
    }

    fn display_name(&self) -> String {
        gettext("Bing")
    }

    fn settings(&self) -> Vec<SettingSpec> {
        vec![
            SettingSpec::Choice {
                key: "mkt",
                label: gettext("Region"),
                choices: vec![
                    ("auto", auto_label(&resolve_mkt("auto"))),
                    ("zh-CN", "中文 (中国)".into()),
                    ("zh-HK", "中文 (香港)".into()),
                    ("en-US", "English (US)".into()),
                    ("en-GB", "English (UK)".into()),
                    ("ja-JP", "日本語".into()),
                    ("ko-KR", "한국어".into()),
                    ("de-DE", "Deutsch".into()),
                    ("fr-FR", "Français".into()),
                    ("es-ES", "Español".into()),
                    ("it-IT", "Italia".into()),
                    ("ru-RU", "Русский".into()),
                ],
                default: "auto",
            },
            SettingSpec::Choice {
                key: "resolution",
                label: gettext("Resolution"),
                choices: vec![
                    ("UHD", "UHD (2160p)".into()),
                    ("1920x1080", "FHD (1080p)".into()),
                    ("1366x768", "HD (768p)".into()),
                ],
                default: "UHD",
            },
            SettingSpec::Int {
                key: "index",
                label: gettext("Include history"),
                default: 0,
                min: 0,
                max: 7,
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
            let mkt = resolve_mkt(&settings.get_str("mkt", "auto"));
            let index = settings.get_i64("index", 0).clamp(0, 7);
            let resolution = settings.get_str("resolution", "UHD");
            let url = format!("{API_BASE}?format=js&idx={index}&n=1&mkt={mkt}");
            let body = http.get_ok(&url).await?;
            let json: Value = serde_json::from_slice(&body).context("failed to parse Bing response")?;
            parse_bing(&json, &resolution)
        })
    }
}

/// Label for an "auto" choice, showing which region it resolves to,
/// e.g. "Auto (zh-CN)".
fn auto_label(resolved: &str) -> String {
    format!("{} ({})", gettext("Auto"), resolved)
}

/// Resolve the market setting; "auto"/empty follows the system locale.
/// Region-less Chinese variants (zh/yue/lzh) and Tibetan resolve to the
/// mainland China market; other region-less languages fall back to en-US.
fn resolve_mkt(v: &str) -> String {
    match v {
        "" | "auto" => {
            let loc = crate::i18n::system_locale();
            if loc.contains('-') {
                loc
            } else {
                crate::i18n::locale_fallback().0.to_owned()
            }
        }
        other => other.to_owned(),
    }
}

/// Parse a Bing HPImageArchive response. With `resolution` "UHD" the
/// full-resolution original is requested.
fn parse_bing(json: &Value, resolution: &str) -> Result<FetchResult> {
    let img = json
        .get("images")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| anyhow!("no images in Bing response"))?;
    let image_url = if resolution == "UHD" {
        // urlbase carries no resolution marker; appending _UHD yields the
        // official full-resolution original.
        let base = img
            .get("urlbase")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Bing response missing urlbase field"))?;
        format!("{IMAGE_BASE}{base}_UHD.jpg")
    } else {
        let rel = img
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Bing response missing url field"))?;
        format!("{IMAGE_BASE}{rel}")
    };
    let title = img
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| gettext("Bing"));
    let copyright = img
        .get("copyright")
        .and_then(|v| v.as_str())
        .map(normalize_copyright);
    // startdate is the image's own date (e.g. "20260803"); the file name
    // should use it rather than the download time.
    let date = img
        .get("startdate")
        .and_then(|v| v.as_str())
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y%m%d").ok());
    Ok(FetchResult {
        image_url,
        title,
        copyright,
        date,
        extra: Default::default(),
    })
}

/// `copyright` looks like "Caption (© photographer/agency)". Drop the
/// parentheses and put a newline before the parenthesized part, so the UI can
/// show the description on one line and the credit on the next. Returned
/// unchanged when there are no parentheses.
fn normalize_copyright(raw: &str) -> String {
    let Some(open) = raw.find('(') else {
        return raw.to_owned();
    };
    // Find the closing paren on the same line (source data is single-line).
    let Some(close) = raw[open..].find(')') else {
        return raw.to_owned();
    };
    let inner = raw[open + 1..open + close].trim();
    let head = raw[..open].trim_end();
    let tail = raw[open + close + 1..].trim_start();
    if tail.is_empty() {
        format!("{head}\n{inner}")
    } else {
        format!("{head}\n{inner} {tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_copyright, parse_bing};
    use serde_json::json;

    // Verified response captured 2026-08-04 (structure kept, text anglicized)
    const FIXTURE: &str = r#"{"images":[{"startdate":"20260803","fullstartdate":"202608031600","enddate":"20260804","url":"/th?id=OHR.AdorableOwlet_EN-US6929234033_1920x1080.jpg&rf=LaDigue_1920x1080.jpg&pid=hp","urlbase":"/th?id=OHR.AdorableOwlet_EN-US6929234033","copyright":"Burrowing owl chicks, Cape Coral, Florida, USA (© mlorenzphotography/Getty Images)","copyrightlink":"https://www.bing.com/search?q=burrowing+owl","title":"Hoot, what a hoot!","quiz":"/search?q=Bing+homepage+quiz","wp":true,"hsh":"e64f1c18480f78138cf56da1049ce2be","drk":1,"top":1,"bot":1,"hs":[]}],"tooltips":{"loading":"Loading...","previous":"Previous image","next":"Next image","walle":"This image cannot be downloaded as wallpaper.","walls":"Download today's beauty. Hover to download!"}}"#;

    #[test]
    fn parses_real_fixture() {
        let json: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let r = parse_bing(&json, "UHD").unwrap();
        assert_eq!(r.title, "Hoot, what a hoot!");
        assert_eq!(
            r.copyright.unwrap(),
            "Burrowing owl chicks, Cape Coral, Florida, USA\n© mlorenzphotography/Getty Images"
        );
        assert_eq!(
            r.image_url,
            "https://www.bing.com/th?id=OHR.AdorableOwlet_EN-US6929234033_UHD.jpg"
        );
    }

    #[test]
    fn keeps_requested_resolution() {
        let json: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let r = parse_bing(&json, "1920x1080").unwrap();
        assert!(r.image_url.contains("_1920x1080.jpg"));
    }

    #[test]
    fn missing_images_is_error() {
        assert!(parse_bing(&json!({}), "UHD").is_err());
    }

    #[test]
    fn normalize_copyright_splits_parenthesis() {
        assert_eq!(normalize_copyright("Caption (© Someone)"), "Caption\n© Someone");
        assert_eq!(normalize_copyright("Caption(© Someone)tail"), "Caption\n© Someone tail");
        // No parentheses (or unclosed) — returned unchanged
        assert_eq!(normalize_copyright("No parentheses caption"), "No parentheses caption");
        assert_eq!(normalize_copyright("Only opening paren (x"), "Only opening paren (x");
    }
}
