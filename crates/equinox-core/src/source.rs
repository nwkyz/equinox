//! Modular wallpaper source interface.
//!
//! Adding a source = implement [`Source`] and register it in [`registry()`];
//! the GUI then generates its settings UI automatically.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use chrono::NaiveDate;
use serde_json::{Map, Value};

use crate::http::HttpClient;

/// A setting definition a source exposes to the GUI; the GUI generates
/// the corresponding control from it.
///
/// Labels and choice texts are wrapped with `gettext` at construction time,
/// so they are `String` (runtime) rather than `&'static str`.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingSpec {
    String {
        key: &'static str,
        label: String,
        default: &'static str,
    },
    Int {
        key: &'static str,
        label: String,
        default: i64,
        min: i64,
        max: i64,
    },
    Bool {
        key: &'static str,
        label: String,
        default: bool,
    },
    /// `choices` is a list of (value, display text).
    Choice {
        key: &'static str,
        label: String,
        choices: Vec<(&'static str, String)>,
        default: &'static str,
    },
}

impl SettingSpec {
    pub fn key(&self) -> &'static str {
        match self {
            SettingSpec::String { key, .. }
            | SettingSpec::Int { key, .. }
            | SettingSpec::Bool { key, .. }
            | SettingSpec::Choice { key, .. } => key,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            SettingSpec::String { label, .. }
            | SettingSpec::Int { label, .. }
            | SettingSpec::Bool { label, .. }
            | SettingSpec::Choice { label, .. } => label,
        }
    }
}

/// Setting values of a single source; unconfigured keys fall back to the
/// [`SettingSpec`] default.
#[derive(Debug, Clone, Default)]
pub struct SourceSettings(pub Map<String, Value>);

impl SourceSettings {
    pub fn get_str(&self, key: &str, default: &str) -> String {
        self.0
            .get(key)
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| default.to_owned())
    }

    pub fn get_i64(&self, key: &str, default: i64) -> i64 {
        self.0.get(key).and_then(|v| v.as_i64()).unwrap_or(default)
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.0.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
    }

    pub fn set(&mut self, key: &str, value: Value) {
        self.0.insert(key.to_owned(), value);
    }
}

/// Result of a successful fetch.
#[derive(Debug, Clone)]
pub struct FetchResult {
    /// Remote URL of the image.
    pub image_url: String,
    pub title: String,
    pub copyright: Option<String>,
    /// The image's **original** date (e.g. Bing `startdate` / NASA `date` /
    /// the POTD's own day). When set, the file is named after this date rather
    /// than the download time; empty when the source provides no date.
    pub date: Option<NaiveDate>,
    /// Extra metadata (e.g. NASA's summary, POTD's author/license), written
    /// to the sidecar description file.
    pub extra: HashMap<String, String>,
}

/// A wallpaper source. Implementations must be stateless and cheap; `date` is
/// the current day, for "daily" sources to use.
pub trait Source: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> String;
    fn settings(&self) -> Vec<SettingSpec>;

    /// Whether the source publishes ONE image per calendar day (Bing, NASA
    /// APOD, Wikimedia POTD). Daily sources may skip fetching a day that is
    /// already stored; rotating sources (Windows Spotlight changes several
    /// times a day) must always be fetched.
    fn daily(&self) -> bool {
        true
    }

    fn fetch<'a>(
        &'a self,
        settings: &'a SourceSettings,
        http: &'a HttpClient,
        date: NaiveDate,
    ) -> Pin<Box<dyn Future<Output = Result<FetchResult>> + 'a>>;
}

/// All built-in sources.
pub fn registry() -> Vec<&'static dyn Source> {
    use crate::sources::{
        BingSource, EarthViewSource, EuropeanaSource, NasaApodSource, PicsumSource,
        SpotlightSource, WikimediaPotdSource,
    };
    vec![
        &BingSource,
        &SpotlightSource,
        &NasaApodSource,
        &WikimediaPotdSource,
        &EarthViewSource,
        &PicsumSource,
        &EuropeanaSource,
    ]
}

/// Look up a source by id.
pub fn by_id(id: &str) -> Option<&'static dyn Source> {
    registry().into_iter().find(|s| s.id() == id)
}
