//! Sources page: per-source settings (scheduled update toggle, refresh
//! interval, source parameters, update now, image count).

use std::rc::Rc;

use gio::prelude::*;
use libadwaita as Adw;
use libadwaita::prelude::*;

use gettextrs::gettext;

use equinox_core::config::Config;
use equinox_core::source::{registry, SettingSpec, Source};
use equinox_core::storage::Storage;

use crate::daemon_client::DaemonClient;

pub fn build(
    config: &Config,
    client: &Rc<DaemonClient>,
    toast: &Adw::ToastOverlay,
) -> gtk4::Widget {
    let page = Adw::PreferencesPage::new();
    let group = Adw::PreferencesGroup::new();
    group.set_title(&gettext("Wallpaper sources"));
    page.add(&group);

    for source in registry() {
        group.add(&build_source_row(source, config, client, toast));
    }
    // The page scrolls as a whole (content stays reachable in short windows)
    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroll.set_child(Some(&page));
    scroll.upcast()
}

fn build_source_row(
    source: &'static dyn Source,
    config: &Config,
    client: &Rc<DaemonClient>,
    toast: &Adw::ToastOverlay,
) -> Adw::ExpanderRow {
    let row = Adw::ExpanderRow::new();
    row.set_title(&source.display_name());

    // Downloaded image count (reads the local directory directly)
    let count_label = gtk4::Label::new(None);
    count_label.add_css_class("dim-label");
    row.add_suffix(&count_label);
    let refresh_count = {
        let count_label = count_label.clone();
        let id = source.id().to_owned();
        move || {
            let n = Storage::list(&id).len();
            count_label.set_text(&gettext_fmt_count(n));
        }
    };
    refresh_count();

    // Scheduled update toggle
    let update_enabled = Adw::SwitchRow::new();
    update_enabled.set_title(&gettext("Enable scheduled updates"));
    update_enabled.set_active(config.update_enabled(source.id()));
    let cfg = config.clone();
    let sid = source.id().to_owned();
    update_enabled.connect_active_notify(move |sw| {
        cfg.set_update_enabled(&sid, sw.is_active());
    });
    row.add_row(&update_enabled);

    // Refresh interval (per source, seconds)
    let time_row = crate::settings_ui::build_rows(
        config,
        source.id(),
        &[SettingSpec::Int {
            key: "update-interval",
            label: gettext("Refresh interval (seconds)"),
            default: equinox_core::config::DEFAULT_UPDATE_INTERVAL,
            min: equinox_core::config::MIN_UPDATE_INTERVAL,
            max: equinox_core::config::MAX_UPDATE_INTERVAL,
        }],
    )
    .into_iter()
    .next()
    .expect("one control");
    row.add_row(&time_row);

    // Image count cap
    let cap_row = crate::settings_ui::build_rows(
        config,
        source.id(),
        &[SettingSpec::Int {
            key: "max-images",
            label: gettext("Max images (0 = unlimited)"),
            default: equinox_core::config::DEFAULT_MAX_IMAGES,
            min: 0,
            max: 2000,
        }],
    )
    .into_iter()
    .next()
    .expect("one control");
    row.add_row(&cap_row);

    // Source API parameters
    for widget in crate::settings_ui::build_rows(config, source.id(), &source.settings()) {
        row.add_row(&widget);
    }

    // Status line (updating / failure)
    let status_label = gtk4::Label::new(Some(""));
    status_label.add_css_class("dim-label");
    let status_row = Adw::ActionRow::new();
    status_row.set_title(&gettext("Status"));
    status_row.add_suffix(&status_label);

    // Update now
    let update_row = Adw::ActionRow::new();
    update_row.set_title(&gettext("Update now"));
    update_row.set_subtitle(&gettext(
        "Fetch one image immediately and store it in the image directory",
    ));
    let update_btn = gtk4::Button::with_label(&gettext("Update"));
    update_btn.add_css_class("suggested-action");
    // ActionRow stretches suffixes vertically; center the button so it does
    // not fill the whole row height.
    update_btn.set_valign(gtk4::Align::Center);
    let c = Rc::clone(client);
    let sid2 = source.id().to_owned();
    let t = toast.clone();
    let st = status_label.clone();
    let cnt = refresh_count.clone();
    update_btn.connect_clicked(move |_| {
        st.set_text(&gettext("Updating…"));
        let c2 = c.clone();
        let sid3 = sid2.clone();
        let t2 = t.clone();
        let st2 = st.clone();
        let cnt2 = cnt.clone();
        glib::spawn_future_local(async move {
            match c2.fetch_now(&sid3).await {
                Ok(()) => {
                    st2.set_text("");
                    cnt2();
                }
                Err(e) => {
                    let msg = format!("{}: {e}", gettext("Update failed"));
                    st2.set_text(&msg);
                    t2.add_toast(crate::views::toast(&msg));
                }
            }
        });
    });
    update_row.add_suffix(&update_btn);
    row.add_row(&update_row);
    row.add_row(&status_row);

    // ImageAdded signal → refresh count and status
    let cnt_sig = refresh_count.clone();
    let st_sig = status_label.clone();
    let sid_sig = source.id().to_owned();
    client.set_on_image_added(move |added_source, _path| {
        if added_source == sid_sig {
            cnt_sig();
            st_sig.set_text("");
        }
    });

    row
}

/// Image count, e.g. "12 images".
fn gettext_fmt_count(n: usize) -> String {
    format!("{n} {}", gettext("images"))
}
