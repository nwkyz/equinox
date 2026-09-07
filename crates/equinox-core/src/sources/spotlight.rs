//! Windows Spotlight (lock screen / desktop wallpaper) via Microsoft's Iris
//! service: https://fd.api.iris.microsoft.com/v4/api/selection
//!
//! The API is undocumented; this follows the request shape captured from the
//! OS. It requires the Windows shell user agent, so this source builds its
//! own `HttpClient` instead of using the shared one.

use std::future::Future;
use std::pin::Pin;

use anyhow::{anyhow, Context, Result};
use chrono::NaiveDate;
use gettextrs::gettext;
use serde_json::Value;

use crate::http::HttpClient;
use crate::source::{FetchResult, SettingSpec, Source, SourceSettings};

const API_URL: &str = "https://fd.api.iris.microsoft.com/v4/api/selection";
/// Spotlight wallpaper placement id.
const PLACEMENT: &str = "88000820";
/// The Iris service answers with 400s to other user agents.
const SHELL_UA: &str = "WindowsShellClient/10.0.22631.1";
/// Regions without Spotlight coverage answer HTTP 200 with an errors payload;
/// requests are retried against this country instead.
const FALLBACK_COUNTRY: &str = "US";

pub struct SpotlightSource;

impl Source for SpotlightSource {
    fn id(&self) -> &'static str {
        "spotlight"
    }

    // Spotlight rotates several times per day — never skip "already stored".
    fn daily(&self) -> bool {
        false
    }

    fn display_name(&self) -> String {
        gettext("Windows Spotlight")
    }

    fn settings(&self) -> Vec<SettingSpec> {
        // "auto" labels show the resolved system region/locale, e.g.
        // "Auto (CN)" / "Auto (zh-CN)". Region-less Chinese variants and
        // Tibetan resolve to mainland China, not the US (see locale_fallback).
        let sys = crate::i18n::system_locale();
        let (fb_locale, fb_country) = crate::i18n::locale_fallback();
        let sys_region = match sys.split('-').nth(1) {
            Some(r) if !r.is_empty() => r.to_owned(),
            _ => fb_country.to_owned(),
        };
        let sys_locale = if sys.contains('-') {
            sys.clone()
        } else {
            fb_locale.to_owned()
        };
        vec![
            SettingSpec::Choice {
                key: "country",
                label: gettext("Country"),
                choices: vec![
                    ("auto", format!("{} ({})", gettext("Auto"), sys_region)),
                    ("CN", "CN".into()),
                    ("US", "US".into()),
                    ("JP", "JP".into()),
                    ("GB", "GB".into()),
                    ("DE", "DE".into()),
                    ("FR", "FR".into()),
                    ("AU", "AU".into()),
                    ("CA", "CA".into()),
                ],
                default: "auto",
            },
            SettingSpec::Choice {
                key: "locale",
                label: gettext("Locale"),
                choices: vec![
                    ("auto", format!("{} ({})", gettext("Auto"), sys_locale)),
                    ("zh-CN", "中文 (简体)".into()),
                    ("en-US", "English (US)".into()),
                    ("en-GB", "English (UK)".into()),
                    ("ja-JP", "日本語".into()),
                    ("de-DE", "Deutsch".into()),
                    ("fr-FR", "Français".into()),
                ],
                default: "auto",
            },
        ]
    }

    fn fetch<'a>(
        &'a self,
        settings: &'a SourceSettings,
        _http: &'a HttpClient,
        date: NaiveDate,
    ) -> Pin<Box<dyn Future<Output = Result<FetchResult>> + 'a>> {
        Box::pin(async move {
            let sys = crate::i18n::system_locale();
            let (fb_locale, fb_country) = crate::i18n::locale_fallback();
            // "auto"/empty follows the system language/region; region-less
            // Chinese variants and Tibetan resolve to mainland China.
            let locale = match settings.get_str("locale", "auto").as_str() {
                "" | "auto" => {
                    if sys.contains('-') {
                        sys.clone()
                    } else {
                        fb_locale.to_owned()
                    }
                }
                v => v.to_owned(),
            };
            let country = match settings.get_str("country", "auto").as_str() {
                "" | "auto" => match sys.split('-').nth(1) {
                    Some(r) if !r.is_empty() => r.to_ascii_uppercase(),
                    _ => fb_country.to_owned(),
                },
                v => v.to_owned(),
            };
            let url = format!(
                "{API_URL}?placement={PLACEMENT}&bcnt=1&country={country}&fmt=json&locale={locale}"
            );
            // The Iris service requires the Windows shell user agent.
            let shell = HttpClient::new(SHELL_UA);
            let body = shell.get_ok(&url).await?;
            let mut json: Value =
                serde_json::from_slice(&body).context("failed to parse Spotlight response")?;
            // Regions without coverage (e.g. RU) answer 200 with an errors
            // payload and no items — retry against the fallback country.
            if !has_items(&json) && country != FALLBACK_COUNTRY {
                let url = format!(
                    "{API_URL}?placement={PLACEMENT}&bcnt=1&country={FALLBACK_COUNTRY}&fmt=json&locale=en-US"
                );
                let body = shell.get_ok(&url).await?;
                json = serde_json::from_slice(&body)
                    .context("failed to parse Spotlight response")?;
            }
            // The API exposes no date field (checked: JSON, image URL, EXIF,
            // HTTP Last-Modified is the CDN publish date). Spotlight rotates
            // daily and item 0 is the current day's image, so the requested
            // day is used as the image date, like Wikimedia POTD.
            let mut result = parse_spotlight(&json)?;
            result.date = Some(date);
            Ok(result)
        })
    }
}

/// Whether the response carries at least one Spotlight item. Unsupported
/// regions get `{"batchrsp":{"errors":[{"code":2040,"msg":"... No eligible
/// content."}]}}` with HTTP 200.
fn has_items(json: &Value) -> bool {
    json.get("batchrsp")
        .and_then(|b| b.get("items"))
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty())
}

/// Parse a Spotlight (Iris) response. `batchrsp.items[0].item` is a
/// JSON-encoded string that must be decoded a second time; the image URLs,
/// title, copyright and description live under its `ad` object.
///
/// Field names verified against the live API 2026-08-05: the landscape
/// variant is `landscapeImage.asset`, the portrait one `portraitImage.asset`
/// (earlier third-party notes described `image_fullscreen_001_*` — outdated).
fn parse_spotlight(json: &Value) -> Result<FetchResult> {
    let item = json
        .get("batchrsp")
        .and_then(|b| b.get("items"))
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|i| i.get("item"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            let api_err = json
                .get("batchrsp")
                .and_then(|b| b.get("errors"))
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|e| e.get("msg"))
                .and_then(|m| m.as_str())
                .unwrap_or("no items in response");
            anyhow!("Spotlight response missing items ({api_err})")
        })?;
    let inner: Value = serde_json::from_str(item).context("failed to parse Spotlight item")?;
    let ad = inner
        .get("ad")
        .ok_or_else(|| anyhow!("Spotlight response missing ad data"))?;

    // Landscape is the desktop variant; fall back to portrait when absent,
    // and to the video preview frame for video items.
    let image_url = ad
        .get("landscapeImage")
        .and_then(|v| v.get("asset"))
        .or_else(|| ad.get("portraitImage").and_then(|v| v.get("asset")))
        .or_else(|| ad.get("video").and_then(|v| v.get("PreviewImagePath")))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Spotlight response missing image URL"))?
        .to_owned();

    let title = ad
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| gettext("Spotlight"));
    let copyright = ad.get("copyright").and_then(|v| v.as_str()).map(ToOwned::to_owned);

    let mut extra = std::collections::HashMap::new();
    if let Some(desc) = ad.get("description").and_then(|v| v.as_str()) {
        extra.insert("description".to_owned(), desc.to_owned());
    }
    // Stable identity for re-fetch dedup: the same creative can come back
    // with a different CDN URL / re-encoded bytes on later updates.
    let dedup_id = ad
        .get("entityId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    if let Some(id) = dedup_id {
        extra.insert("dedup-id".to_owned(), id);
    }

    Ok(FetchResult {
        image_url,
        title,
        copyright,
        date: None,
        extra,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_spotlight;
    use serde_json::json;

    // Shape captured from the live API 2026-08-05 (double-encoded item,
    // structure kept, text anglicized)
    const FIXTURE: &str = r#"{"batchrsp":{"ver":"1.0","items":[{"item":"{\"f\":\"raf\",\"v\":\"1.0\",\"rdr\":[{\"c\":\"CDMLite\",\"u\":\"DesktopSpotlightSurface\"}],\"ad\":{\"landscapeImage\":{\"asset\":\"https://res.public.onecdn.static.microsoft/creativeservice/example_3840x2160.jpg\"},\"portraitImage\":{\"asset\":\"https://res.public.onecdn.static.microsoft/creativeservice/example_1080x1920.jpg\"},\"iconLabel\":\"Learn more about this image\",\"iconHoverText\":\"Aerial view of Terceira\\r\\n© Marco Bottigelli / Moment / Getty Images\",\"title\":\"Aerial view of Terceira\",\"description\":\"This panorama shows terraced farmland nestled between small volcanic cones.\",\"copyright\":\"© Marco Bottigelli / Moment / Getty Images\"}}"}]}}"#;

    #[test]
    fn parses_real_fixture() {
        let json: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let r = parse_spotlight(&json).unwrap();
        assert_eq!(r.title, "Aerial view of Terceira");
        assert_eq!(r.copyright.as_deref(), Some("© Marco Bottigelli / Moment / Getty Images"));
        assert!(r.image_url.contains("3840x2160.jpg"));
        assert!(r.extra.get("description").unwrap().contains("terraced farmland"));
    }

    #[test]
    fn falls_back_to_portrait_url() {
        let json = json!({"batchrsp":{"items":[{"item":"{\"ad\":{\"title\":\"t\",\"portraitImage\":{\"asset\":\"https://example.com/p.jpg\"}}}"}]}});
        let r = parse_spotlight(&json).unwrap();
        assert_eq!(r.image_url, "https://example.com/p.jpg");
        assert_eq!(r.copyright, None);
    }

    #[test]
    fn video_item_uses_preview_frame() {
        let json = json!({"batchrsp":{"items":[{"item":"{\"ad\":{\"title\":\"t\",\"isVideo\":\"true\",\"video\":{\"PreviewImagePath\":\"https://example.com/preview.jpg\"}}}"}]}});
        let r = parse_spotlight(&json).unwrap();
        assert_eq!(r.image_url, "https://example.com/preview.jpg");
    }

    #[test]
    fn missing_items_is_error() {
        assert!(parse_spotlight(&json!({})).is_err());
        assert!(parse_spotlight(&json!({"batchrsp": {"items": []}})).is_err());
    }

    #[test]
    fn unsupported_region_reports_api_error() {
        // Shape returned for RU etc.: HTTP 200, errors payload, no items.
        let json = json!({"batchrsp":{"errors":[{"code":2040,"msg":"Demand source returns error (Name: GN_ps, Error: No eligible content.)."}]}});
        let err = parse_spotlight(&json).unwrap_err().to_string();
        assert!(err.contains("missing items"));
        assert!(err.contains("No eligible content"));
    }
}
