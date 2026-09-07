//! Built-in wallpaper sources. Adding a source = a new file implementing
//! [`Source`] + one registration line in `source.rs::registry()`.

mod bing;
mod nasa;
mod spotlight;
mod wikimedia;

pub use bing::BingSource;
pub use nasa::NasaApodSource;
pub use spotlight::SpotlightSource;
pub use wikimedia::WikimediaPotdSource;
