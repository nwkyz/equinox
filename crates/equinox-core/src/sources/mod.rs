//! Built-in wallpaper sources. Adding a source = a new file implementing
//! [`Source`] + one registration line in `source.rs::registry()`.

mod bing;
mod earthview;
mod europeana;
mod nasa;
mod picsum;
mod spotlight;
mod wikimedia;

pub use bing::BingSource;
pub use earthview::EarthViewSource;
pub use europeana::EuropeanaSource;
pub use nasa::NasaApodSource;
pub use picsum::PicsumSource;
pub use spotlight::SpotlightSource;
pub use wikimedia::WikimediaPotdSource;
