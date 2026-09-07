//! Equinox core library: wallpaper source interface, config, storage, applier, history.
//!
//! All async work is based on the GLib main loop ([`glib::spawn_future_local`] +
//! libsoup3), not tokio, so it can be reused by both the GUI (gtk main loop)
//! and the daemon (glib main loop).

pub mod applier;
pub mod autostart;
pub mod config;
pub mod dbus;
pub mod history;
pub mod http;
pub mod i18n;
pub mod logging;
pub mod source;
pub mod sources;
pub mod storage;

pub use applier::Applier;
pub use config::Config;
pub use history::{History, HistoryEntry};
pub use http::HttpClient;
pub use i18n::init as init_i18n;
pub use source::{FetchResult, SettingSpec, Source, SourceSettings};
pub use storage::{ImageMeta, StoredImage, Storage};
