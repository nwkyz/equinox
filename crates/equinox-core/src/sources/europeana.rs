//! Europeana: Europe's digital cultural heritage (https://www.europeana.eu).
//!
//! Uses the Europeana Search API v2 with `reusability=open` + `media=true`, so
//! only records that carry openly-licensed, downloadable media are returned.
//! The API's `sort=random` yields a rotating pick, so this is a non-daily
//! source (like Spotlight / Earth View).
//!
//! NOTE: `edmIsShownBy` is *usually* a direct image URL but not always — some
//! providers return an HTML landing/protection page. Storage now rejects
//! non-image responses (see `Storage::store`), and the daemon's scheduled
//! retries draw a fresh random item, so a bad pick self-heals.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use anyhow::{anyhow, Context, Result};
use chrono::NaiveDate;
use gettextrs::gettext;
use serde_json::Value;

use crate::http::HttpClient;
use crate::source::{FetchResult, SettingSpec, Source, SourceSettings};

const API_URL: &str = "https://api.europeana.eu/record/v2/search.json";
/// Europeana's public demo key (rate-limited). Users can register a free
/// personal key and override it in the source settings.
const DEFAULT_API_KEY: &str = "api2demo";
/// Default art query: artworks, excluding documents/photographs. Empty user
/// keyword falls back to this.
const DEFAULT_QUERY: &str =
    "(painting OR watercolor OR canvas OR artwork) NOT photograph NOT manuscript NOT print NOT book";
/// Rows requested per search; only the first usable item is used.
const ROWS: i64 = 30;
/// Longest description kept in the sidecar (characters).
const DESC_MAX: usize = 2000;

pub struct EuropeanaSource;

impl Source for EuropeanaSource {
    fn id(&self) -> &'static str {
        "europeana"
    }

    fn display_name(&self) -> String {
        gettext("Europeana")
    }

    fn settings(&self) -> Vec<SettingSpec> {
        vec![
            SettingSpec::String {
                key: "api_key",
                label: gettext("API key (optional)"),
                default: DEFAULT_API_KEY,
            },
            SettingSpec::String {
                key: "query",
                label: gettext("Search keyword"),
                default: "",
            },
        ]
    }

    /// A rotating library (random-sorted search), not one image per calendar
    /// day: every scheduled update should fetch a new random artwork.
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
            let api_key = settings.get_str("api_key", DEFAULT_API_KEY);
            let query = settings.get_str("query", "");
            let url = build_url(&api_key, &query);
            let body = http.get_ok(&url).await?;
            let json: Value =
                serde_json::from_slice(&body).context("failed to parse Europeana response")?;
            ensure_success(&json)?;
            parse_europeana(&json)
        })
    }
}

/// Trimmed value, or `None` for empty input.
fn non_empty(s: &str) -> Option<&str> {
    let t = s.trim();
    (!t.is_empty()).then_some(t)
}

/// Build the search API URL. Empty `api_key`/`query` fall back to the demo key
/// and the default art query.
fn build_url(api_key: &str, query: &str) -> String {
    let key = non_empty(api_key).unwrap_or(DEFAULT_API_KEY);
    let q = non_empty(query).unwrap_or(DEFAULT_QUERY);
    format!(
        "{API_URL}?wskey={key}&query={query}&sort=random&rows={ROWS}\
         &profile=minimal&reusability=open&media=true&completeness={complete}\
         &collection=art&type=IMAGE&img_color=true&img_size=extra_large&img_ratio=landscape",
        key = percent_encode(key),
        query = percent_encode(q),
        complete = percent_encode("[1 TO 10]"),
    )
}

/// Minimal percent-encoding for query-string values: keep RFC 3986
/// unreserved characters, encode everything else. `urlencoding` is not a
/// dependency, so this small, testable helper avoids adding one.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Surface an API-level failure (`success:false`) as a readable error.
fn ensure_success(json: &Value) -> Result<()> {
    if json.get("success").and_then(|v| v.as_bool()) == Some(false) {
        let msg = json
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(anyhow!(format!("Europeana API error: {msg}")));
    }
    Ok(())
}

/// All string values of an array field (missing/null → empty).
fn list_strings(item: &Value, key: &str) -> Vec<String> {
    item.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// First non-empty string of an array field.
fn first_string(item: &Value, key: &str) -> Option<String> {
    list_strings(item, key).into_iter().find(|s| !s.is_empty())
}

/// Last non-empty string of an array field (Europeana `title` is a list whose
/// most specific entry is last).
fn last_string(item: &Value, key: &str) -> Option<String> {
    list_strings(item, key)
        .into_iter()
        .filter(|s| !s.is_empty())
        .next_back()
}

/// Pick the first item that carries a usable image URL and turn it into a
/// [`FetchResult`].
fn parse_europeana(json: &Value) -> Result<FetchResult> {
    let items = json
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("no items in Europeana response"))?;
    let item = items
        .iter()
        .find(|it| first_string(it, "edmIsShownBy").is_some())
        .ok_or_else(|| anyhow!("no image in Europeana response"))?;

    let image_url = first_string(item, "edmIsShownBy").expect("filtered for an image URL");
    let guid = item.get("guid").and_then(|v| v.as_str()).unwrap_or("");
    let artwork_url = first_string(item, "edmIsShownAt").unwrap_or_else(|| guid.to_owned());

    let title = last_string(item, "title").unwrap_or_else(|| gettext("Europeana"));

    // Author: the first creator that is not itself a URL; the data provider is
    // the fallback (matches the reference implementation).
    let creators = list_strings(item, "dcCreator");
    let author = creators
        .iter()
        .find(|c| !c.contains("http"))
        .cloned()
        .or_else(|| list_strings(item, "dataProvider").into_iter().next());

    let mut extra = HashMap::new();
    if let Some(desc) = first_string(item, "dcDescription") {
        let desc = desc.trim();
        if !desc.is_empty() {
            extra.insert("description".to_owned(), desc.chars().take(DESC_MAX).collect());
        }
    }
    if let Some(author) = &author {
        extra.insert("author".to_owned(), author.clone());
    }
    if !artwork_url.is_empty() {
        extra.insert("source".to_owned(), artwork_url);
    }
    if let Some(license) = first_string(item, "rights") {
        extra.insert("license".to_owned(), license);
    }
    // Stable identity for re-fetch dedup (the same record can come back with a
    // different CDN URL / re-encoded bytes).
    if let Some(id) = item
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(non_empty)
    {
        extra.insert("dedup-id".to_owned(), id.to_owned());
    }

    Ok(FetchResult {
        image_url,
        title,
        copyright: author,
        date: None,
        extra,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real response captured 2026-09-19 from the live API (text anglicized,
    /// structure kept): item 0 has no creator and falls back to the data
    /// provider; item 1 uses its creator.
    const FIXTURE: &str = r#"{
      "apikey": "api2demo",
      "success": true,
      "totalResults": 122621,
      "items": [
        {
          "id": "/990/item_EXAMPLE0001",
          "guid": "https://www.europeana.eu/item/990/item_EXAMPLE0001",
          "title": ["Plant Study"],
          "dcCreator": null,
          "dcDescription": null,
          "dataProvider": ["Art Collection of the University"],
          "edmIsShownBy": ["https://example.org/images/plant-study.jpg"],
          "edmIsShownAt": ["https://example.org/records/1"],
          "type": "IMAGE",
          "rights": ["http://creativecommons.org/licenses/by-sa/4.0/"]
        },
        {
          "id": "/2064116/item_EXAMPLE0002",
          "guid": "https://www.europeana.eu/item/2064116/item_EXAMPLE0002",
          "title": ["The Old Castle"],
          "dcCreator": ["Jane Painter (Attributed to)"],
          "dcDescription": ["A landscape with a ruined castle."],
          "dataProvider": ["National Museum"],
          "edmIsShownBy": ["https://example.org/images/old-castle.jpg"],
          "edmIsShownAt": ["https://example.org/records/2"],
          "type": "IMAGE",
          "rights": ["http://creativecommons.org/publicdomain/mark/1.0/"]
        }
      ]
    }"#;

    #[test]
    fn percent_encode_keeps_unreserved() {
        assert_eq!(percent_encode("abc-._~123"), "abc-._~123");
        assert_eq!(percent_encode("a b(c)[d]"), "a%20b%28c%29%5Bd%5D");
        assert_eq!(percent_encode("爱"), "%E7%88%B1");
    }

    #[test]
    fn build_url_uses_demo_defaults() {
        let url = build_url("", "");
        assert!(url.starts_with(API_URL));
        assert!(url.contains("wskey=api2demo"), "url: {url}");
        assert!(url.contains("sort=random"));
        assert!(url.contains("reusability=open"));
        assert!(url.contains("media=true"));
        assert!(url.contains("type=IMAGE"));
        // Default query is percent-encoded, not raw
        assert!(url.contains("query=%28painting%20OR%20watercolor"), "url: {url}");
        assert!(!url.contains(' '), "no raw spaces allowed: {url}");
    }

    #[test]
    fn build_url_honors_overrides() {
        let url = build_url("my-key", "sea forest");
        assert!(url.contains("wskey=my-key"));
        assert!(url.contains("query=sea%20forest"));
        assert!(!url.contains("painting"), "custom keyword replaces the default query");
    }

    #[test]
    fn parses_real_fixture_provider_as_author() {
        let json: Value = serde_json::from_str(FIXTURE).unwrap();
        let r = parse_europeana(&json).unwrap();
        assert_eq!(r.image_url, "https://example.org/images/plant-study.jpg");
        assert_eq!(r.title, "Plant Study");
        // No dcCreator → dataProvider becomes the credit
        assert_eq!(r.copyright.as_deref(), Some("Art Collection of the University"));
        assert_eq!(r.extra.get("source").unwrap(), "https://example.org/records/1");
        assert_eq!(r.extra.get("dedup-id").unwrap(), "/990/item_EXAMPLE0001");
        assert_eq!(
            r.extra.get("license").unwrap(),
            "http://creativecommons.org/licenses/by-sa/4.0/"
        );
    }

    #[test]
    fn prefers_creator_over_provider() {
        let json: Value = serde_json::from_str(FIXTURE).unwrap();
        // Second item: filter only that one
        let mut json = json;
        json["items"] = serde_json::json!([json["items"][1].clone()]);
        let r = parse_europeana(&json).unwrap();
        assert_eq!(r.copyright.as_deref(), Some("Jane Painter (Attributed to)"));
        assert_eq!(r.title, "The Old Castle");
        assert_eq!(
            r.extra.get("description").unwrap(),
            "A landscape with a ruined castle."
        );
    }

    #[test]
    fn creator_that_is_a_url_is_skipped() {
        let json = serde_json::json!({
            "items": [{
                "id": "/x/1",
                "title": ["T"],
                "dcCreator": ["https://example.org/artist-page", "Real Artist"],
                "dataProvider": ["Museum"],
                "edmIsShownBy": ["https://example.org/a.jpg"]
            }]
        });
        let r = parse_europeana(&json).unwrap();
        assert_eq!(r.copyright.as_deref(), Some("Real Artist"));
    }

    #[test]
    fn skips_item_without_image() {
        let json = serde_json::json!({
            "items": [
                { "id": "/x/no-img", "title": ["No image"], "edmIsShownBy": [] },
                { "id": "/x/ok", "title": ["Has image"],
                  "edmIsShownBy": ["https://example.org/ok.jpg"],
                  "dataProvider": ["Museum"] }
            ]
        });
        let r = parse_europeana(&json).unwrap();
        assert_eq!(r.image_url, "https://example.org/ok.jpg");
        assert_eq!(r.extra.get("dedup-id").unwrap(), "/x/ok");
    }

    #[test]
    fn missing_items_is_error() {
        assert!(parse_europeana(&serde_json::json!({})).is_err());
        assert!(parse_europeana(&serde_json::json!({ "items": [] })).is_err());
        assert!(parse_europeana(&serde_json::json!({ "items": [{ "title": ["x"] }] })).is_err());
    }

    #[test]
    fn api_error_is_reported() {
        let json = serde_json::json!({
            "success": false,
            "error": "API key is invalid",
            "message": "Please register for an API key",
        });
        let err = ensure_success(&json).unwrap_err().to_string();
        // The human-readable `message` is surfaced (not the machine `error`).
        assert!(err.contains("register for an API key"), "err: {err}");
        assert!(ensure_success(&serde_json::json!({ "success": true })).is_ok());
    }

    // ------------------------------------------------------------------
    // Live tests (network). Run explicitly:
    //   cargo test -p equinox-core --lib -- --ignored --nocapture europeana
    // ------------------------------------------------------------------

    use crate::http::USER_AGENT;

    /// Fetch + actually download one Europeana image, retrying a few times
    /// because a random pick can occasionally point at an HTML wrapper.
    #[test]
    #[ignore = "hits the live Europeana API; run with --ignored"]
    fn live_fetch_and_download() {
        let ctx = glib::MainContext::default();
        ctx.block_on(async {
            let http = HttpClient::new(USER_AGENT);
            let src = EuropeanaSource;
            let settings = SourceSettings::default();
            let date = chrono::Local::now().date_naive();

            let mut last = String::new();
            for attempt in 1..=5 {
                let r = src.fetch(&settings, &http, date).await.expect("fetch");
                assert!(r.image_url.starts_with("http"), "image_url: {}", r.image_url);
                assert!(!r.title.is_empty(), "title must not be empty");
                let resp = http.get(&r.image_url).await.expect("image download");
                let ct = resp.content_type.clone().unwrap_or_default();
                if resp.status == soup::Status::Ok && ct.starts_with("image/") && !resp.body.is_empty() {
                    println!(
                        "europeana LIVE ok on attempt {attempt}: title={:?} ct={ct} bytes={} extra={:?}",
                        r.title,
                        resp.body.len(),
                        r.extra
                    );
                    return;
                }
                last = format!("status={:?} ct={ct} bytes={}", resp.status, resp.body.len());
                println!("attempt {attempt}: not an image ({last}), retrying");
            }
            panic!("no downloadable image in 5 attempts; last: {last}");
        });
    }

    /// Measure how often the randomly picked `edmIsShownBy` is a real image
    /// (the rest are HTML landing/protection pages). Informs the storage
    /// content-type guard decision.
    #[test]
    #[ignore = "hits the live Europeana API; run with --ignored"]
    fn live_image_hit_rate() {
        let ctx = glib::MainContext::default();
        ctx.block_on(async {
            let http = HttpClient::new(USER_AGENT);
            let src = EuropeanaSource;
            let settings = SourceSettings::default();
            let date = chrono::Local::now().date_naive();

            let mut images = 0;
            let mut total = 0;
            for _ in 0..8 {
                let r = match src.fetch(&settings, &http, date).await {
                    Ok(r) => r,
                    Err(e) => {
                        println!("fetch failed: {e:#}");
                        continue;
                    }
                };
                total += 1;
                match http.get(&r.image_url).await {
                    Ok(resp) => {
                        let ct = resp.content_type.clone().unwrap_or_default();
                        let ok = resp.status == soup::Status::Ok && ct.starts_with("image/");
                        if ok {
                            images += 1;
                        }
                        println!(
                            "{}  ct={ct}  {}",
                            if ok { "IMAGE" } else { "BAD  " },
                            &r.image_url[..r.image_url.len().min(80)]
                        );
                    }
                    Err(e) => println!("BAD    download error: {e:#}"),
                }
            }
            println!("hit rate: {images}/{total}");
            assert!(total > 0, "no fetch succeeded at all");
        });
    }
}
