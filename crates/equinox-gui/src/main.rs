//! Equinox GUI (GTK4 + Libadwaita), drives the daemon over D-Bus.

mod app;
mod daemon_client;
mod settings_ui;
mod views;

use gio::prelude::*;
use libadwaita::Application;

fn main() -> glib::ExitCode {
    // Language override from Preferences ("auto" follows the system): must
    // happen BEFORE gettext init, which reads $LANGUAGE via setlocale.
    if let Ok(config) = equinox_core::Config::new() {
        let lang = config.language();
        if lang != "auto" && !lang.is_empty() {
            std::env::set_var("LANGUAGE", &lang);
        }
    }
    equinox_core::init_i18n();
    equinox_core::logging::init();
    let app = Application::builder()
        .application_id("github.nwkyz.Equinox")
        .build();
    app.connect_activate(app::activate);
    app.run()
}
