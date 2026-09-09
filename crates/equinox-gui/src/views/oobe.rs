//! First-run welcome (OOBE): a two-step wizard.
//!
//! 1. **Sources** — per-source enable toggle + update interval; "Continue"
//!    persists the choices, starts the background and **kicks off** an
//!    immediate update of every enabled source. The update is non-blocking:
//!    downloads run in the daemon's background queue while the wizard moves
//!    on, so the UI never hangs on "this source has no images yet".
//! 2. **Background** — hosting mode (Standalone / systemd, terse labels) and
//!    login autostart.
//!
//! Shown once on a brand-new install (no config file yet); upgraded installs
//! already have a config and never see it.

use std::rc::Rc;

use libadwaita as Adw;
use libadwaita::prelude::*;

use gettextrs::gettext;

use equinox_core::config::{
    Config, DEFAULT_UPDATE_INTERVAL, MAX_UPDATE_INTERVAL, MIN_UPDATE_INTERVAL,
};
use equinox_core::source::registry;

use crate::daemon_client::DaemonClient;

/// One source's page-1 controls: the enable switch sits on the OUTER row bar
/// (as an `Adw::ExpanderRow` suffix); only the interval spin is expanded.
struct SourceCtl {
    id: &'static str,
    toggle: gtk4::Switch,
    spin: Adw::SpinRow,
}

/// How long "Continue" waits for the daemon to come online before giving up
/// and moving on anyway (bounded so a slow/absent daemon can never stall the
/// wizard). 20 × 300 ms = 6 s, then the page 2 appears regardless.
const DAEMON_WAIT_TICKS: u32 = 20;
const DAEMON_WAIT_TICK_MS: u64 = 300;

/// Show the OOBE wizard on a brand-new install (no config file yet). Returns
/// whether it was shown.
pub fn maybe_show(config: &Config, client: &Rc<DaemonClient>) -> bool {
    if !config.is_first_run() {
        return false;
    }
    let win = Adw::Window::builder()
        .title("Equinox")
        .modal(true)
        .default_width(560)
        .default_height(680)
        .resizable(false)
        .build();

    let stack = gtk4::Stack::new();
    stack.set_transition_type(gtk4::StackTransitionType::SlideLeftRight);

    // ----------------------------------------------------------------
    // Page 1: sources
    // ----------------------------------------------------------------
    let page1 = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
    page1.set_margin_top(48);
    page1.set_margin_bottom(24);
    page1.set_margin_start(32);
    page1.set_margin_end(32);

    let title1 = gtk4::Label::new(Some(&gettext("Choose the sources")));
    title1.add_css_class("title-1");
    title1.set_xalign(0.0);
    page1.append(&title1);

    let sub1 = gtk4::Label::new(Some(&gettext(
        "Enable the sources to keep up to date and set how often each one updates. Enabled sources are updated right away when you continue.",
    )));
    sub1.add_css_class("dim-label");
    sub1.set_wrap(true);
    sub1.set_xalign(0.0);
    page1.append(&sub1);

    let group1 = Adw::PreferencesGroup::new();
    group1.set_title(&gettext("Sources"));
    let mut ctls: Vec<SourceCtl> = Vec::new();
    for source in registry() {
        let row = Adw::ExpanderRow::new();
        row.set_title(&source.display_name());

        // Enable switch on the row bar itself (suffix); only the interval is
        // tucked into the expanded content.
        let toggle = gtk4::Switch::new();
        toggle.set_active(true); // brand-new install: every source defaults ON
        toggle.set_valign(gtk4::Align::Center);
        row.add_suffix(&toggle);

        // Interval default: 1 h; Windows Spotlight (rotates several times a
        // day) defaults to 10 h.
        let default_interval = if source.id() == "spotlight" {
            10 * 3600
        } else {
            DEFAULT_UPDATE_INTERVAL
        };
        let adj = gtk4::Adjustment::new(
            default_interval as f64,
            MIN_UPDATE_INTERVAL as f64,
            MAX_UPDATE_INTERVAL as f64,
            60.0,
            10.0,
            0.0,
        );
        let spin = Adw::SpinRow::new(Some(&adj), 1.0, 0);
        spin.set_title(&gettext("Refresh interval (seconds)"));
        row.add_row(&spin);

        group1.add(&row);
        ctls.push(SourceCtl { id: source.id(), toggle, spin });
    }
    let scroller1 = gtk4::ScrolledWindow::new();
    scroller1.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroller1.set_vexpand(true);
    scroller1.set_child(Some(&group1));
    page1.append(&scroller1);

    // Continue button + a spinner shown while the daemon is being started.
    let continue_btn = gtk4::Button::with_label(&gettext("Continue"));
    continue_btn.add_css_class("suggested-action");
    continue_btn.set_halign(gtk4::Align::End);
    let continue_spinner = gtk4::Spinner::new();
    continue_spinner.set_spinning(true);
    continue_spinner.set_visible(false);
    let continue_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    continue_box.set_halign(gtk4::Align::End);
    continue_box.append(&continue_spinner);
    continue_box.append(&continue_btn);
    page1.append(&continue_box);

    // ----------------------------------------------------------------
    // Page 2: background service
    // ----------------------------------------------------------------
    let page2 = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
    page2.set_margin_top(48);
    page2.set_margin_bottom(24);
    page2.set_margin_start(32);
    page2.set_margin_end(32);

    let title2 = gtk4::Label::new(Some(&gettext("Background service")));
    title2.add_css_class("title-1");
    title2.set_xalign(0.0);
    page2.append(&title2);

    let group2 = Adw::PreferencesGroup::new();
    // Hosting mode: Standalone by default, systemd only when a user instance
    // is reachable. Terse labels only ("独立 / systemd") — no explanations.
    let systemd_ok = equinox_core::autostart::systemd_available();
    let mut modes: Vec<(&str, String)> = vec![("supervisor", gettext("Standalone"))];
    if systemd_ok {
        modes.push(("systemd", "systemd".to_owned()));
    }
    let mode_labels: Vec<&str> = modes.iter().map(|(_, l)| l.as_str()).collect();
    let mode_row = Adw::ComboRow::new();
    mode_row.set_title(&gettext("Run via"));
    mode_row.set_model(Some(&gtk4::StringList::new(&mode_labels)));
    mode_row.set_selected(0);
    group2.add(&mode_row);

    let autostart_row = Adw::SwitchRow::new();
    autostart_row.set_title(&gettext("Start at login"));
    autostart_row.set_active(true); // OOBE default: login autostart ON
    group2.add(&autostart_row);
    page2.append(&group2);

    // Spacer, then Back / Finish at the bottom.
    let spacer2 = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    spacer2.set_vexpand(true);
    page2.append(&spacer2);

    let back_btn = gtk4::Button::with_label(&gettext("Back"));
    let finish_btn = gtk4::Button::with_label(&gettext("Finish"));
    finish_btn.add_css_class("suggested-action");
    let nav = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    nav.set_halign(gtk4::Align::End);
    nav.append(&back_btn);
    nav.append(&finish_btn);
    page2.append(&nav);

    stack.add_named(&page1, Some("page1"));
    stack.add_named(&page2, Some("page2"));
    stack.set_visible_child_name("page1");
    win.set_content(Some(&stack));

    // Back from page 2 → page 1.
    back_btn.connect_clicked({
        let s = stack.clone();
        move |_| s.set_visible_child_name("page1")
    });

    // "Continue": persist source choices, start the background, kick off an
    // update for every enabled source, then move to page 2. The update is
    // NON-BLOCKING: the daemon downloads in its background queue while the
    // wizard proceeds, so the UI never sits stuck on "no images".
    {
        let cfg = config.clone();
        let s = stack.clone();
        let c = Rc::clone(client);
        let btn = continue_btn.clone();
        let spinner = continue_spinner.clone();
        continue_btn.connect_clicked(move |_| {
            for ctl in &ctls {
                cfg.set_update_enabled(ctl.id, ctl.toggle.is_active());
                cfg.set_update_interval(ctl.id, ctl.spin.value() as i64);
            }
            let enabled: Vec<String> = ctls
                .iter()
                .filter(|c| c.toggle.is_active())
                .map(|c| c.id.to_owned())
                .collect();
            btn.set_sensitive(false);
            spinner.set_visible(true);
            let c2 = Rc::clone(&c);
            let s2 = s.clone();
            let b2 = btn.clone();
            let sp2 = spinner.clone();
            glib::spawn_future_local(async move {
                // Start the background (supervisor by default at this point;
                // page 2 may switch hosting to systemd later).
                crate::app::spawn_background_if_needed();
                // Wait for the daemon's D-Bus name — bounded, so a slow or
                // absent daemon can never stall the wizard.
                let mut waited = 0u32;
                while !c2.running() && waited < DAEMON_WAIT_TICKS {
                    glib::timeout_future(std::time::Duration::from_millis(
                        DAEMON_WAIT_TICK_MS,
                    ))
                    .await;
                    waited += 1;
                }
                if c2.running() {
                    for id in &enabled {
                        if let Err(e) = c2.fetch_now(id).await {
                            log::warn!("OOBE initial update of {id} failed: {e:#}");
                        }
                    }
                }
                b2.set_sensitive(true);
                sp2.set_visible(false);
                s2.set_visible_child_name("page2");
            });
        });
    }

    // "Finish": persist background choices, register login-startup, pick the
    // first enabled source as the wallpaper rule, then close.
    {
        let cfg = config.clone();
        let c = Rc::clone(client);
        let w = win.clone();
        finish_btn.connect_clicked(move |_| {
            let mode = modes[mode_row.selected() as usize].0;
            cfg.set_run_mode(mode);
            cfg.set_autostart(autostart_row.is_active());
            cfg.set_setup_done(true);
            // Brand-new install: fill the Lorem Picsum size from the screen
            // right away (the app.rs call is skipped on this first run).
            crate::app::auto_fill_picsum_size(&cfg);
            if mode == "systemd" {
                let _ = equinox_core::autostart::install_systemd_service();
            } else if autostart_row.is_active() {
                let _ = equinox_core::autostart::set_autostart(true);
            }
            // The background was already started on "Continue" (supervisor);
            // in systemd mode the install above switched hosting, so re-run to
            // make the chosen arrangement the live one.
            crate::app::spawn_background_if_needed();
            // Start in the "None" wallpaper source: merely opening the app
            // must NOT replace the user's wallpaper. Updates download into
            // the library; the wallpaper stays untouched until the user
            // actively picks a source on the Wallpaper page.
            cfg.set_wallpaper_source("");
            let c2 = Rc::clone(&c);
            glib::spawn_future_local(async move {
                if let Err(e) = c2.set_wallpaper_source("", "latest").await {
                    log::warn!("OOBE wallpaper-source init failed: {e:#}");
                }
            });
            w.close();
        });
    }

    win.present();
    true
}