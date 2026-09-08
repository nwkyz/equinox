//! Window assembly: HeaderBar with ViewSwitcher navigation (four pages) and
//! a primary hamburger menu (live task list + About), plus daemon status
//! banner.

use std::rc::Rc;

use libadwaita as Adw;
use libadwaita::prelude::*;

use gettextrs::gettext;

use crate::daemon_client::{DaemonClient, TaskRow};
use crate::views;

pub fn activate(app: &libadwaita::Application) {
    let config =
        equinox_core::Config::new().expect("cannot read config file ~/.config/equinox/config.json");
    views::ensure_ui_css();
    // Heal a pre-existing XDG autostart entry whose Exec was written as a bare
    // `equinox-supervisor` (older versions) — such entries silently fail at
    // login on GNOME/KDE, where autostart runs outside the session PATH.
    equinox_core::autostart::refresh_autostart();
    // Auto-fill the Lorem Picsum size from the primary monitor, but only while
    // the user has not chosen a manual size (width/height still 0 = "auto").
    auto_fill_picsum_size(&config);
    let window = libadwaita::ApplicationWindow::new(app);
    window.set_title(Some("Equinox"));
    window.set_default_size(1000, 680);

    let toast = libadwaita::ToastOverlay::new();
    let toolbar = libadwaita::ToolbarView::new();

    let header = libadwaita::HeaderBar::new();
    toolbar.add_top_bar(&header);

    // Daemon status banner
    let banner = libadwaita::Banner::new(&gettext(
        "The daemon is not running; scheduled updates will be unavailable",
    ));
    banner.set_button_label(Some(&gettext("Start")));
    banner.set_revealed(false);
    banner.add_css_class("error");
    toolbar.add_top_bar(&banner);

    let client = DaemonClient::new();

    // Pages (About lives in the hamburger menu, per GNOME HIG)
    let wallpaper = views::wallpaper::build(&client, &config, &toast);
    let sources = views::sources::build(&config, &client, &toast);
    let gallery = views::gallery::build(&client, &toast);
    let history = views::history::build(&client, &toast);

    // Page stack + ViewSwitcher in the HeaderBar (icon + text; narrow windows
    // shrink to icons only). ViewSwitcher requires symbolic icons, otherwise
    // they don't follow the theme / don't show.
    let stack = Adw::ViewStack::new();
    stack.add_titled_with_icon(
        &wallpaper,
        Some("wallpaper"),
        &gettext("Wallpaper"),
        "video-display-symbolic",
    );
    stack.add_titled_with_icon(
        &sources,
        Some("sources"),
        &gettext("Sources"),
        "preferences-system-symbolic",
    );
    stack.add_titled_with_icon(
        &gallery,
        Some("gallery"),
        &gettext("Gallery"),
        "image-x-generic-symbolic",
    );
    stack.add_titled_with_icon(
        &history,
        Some("history"),
        &gettext("History"),
        "document-open-recent-symbolic",
    );

    let switcher = Adw::ViewSwitcher::new();
    switcher.set_policy(Adw::ViewSwitcherPolicy::Wide);
    switcher.set_stack(Some(&stack));
    header.set_title_widget(Some(&switcher));

    // Primary menu: task list + Preferences + About
    build_primary_menu(&header, &client, &config);

    toolbar.set_content(Some(&stack));
    toast.set_child(Some(&toolbar));
    window.set_content(Some(&toast));

    // Banner follows the daemon online state
    let banner_state = banner.clone();
    client.set_on_running_changed(move |running| banner_state.set_revealed(!running));
    banner.set_revealed(!client.running());

    banner.connect_button_clicked(move |_| start_daemon());

    // Dev aid: EQ_DEBUG_PAGE=gallery|history|sources|wallpaper opens that page
    // directly (used for screenshot-based debugging); "timeline" opens the
    // update-timeline window on top.
    if let Ok(page) = std::env::var("EQ_DEBUG_PAGE") {
        if page == "timeline" {
            let w = views::timeline::build_window(&client);
            w.present();
        } else {
            stack.set_visible_child_name(&page);
        }
    }

    // First run: the OOBE welcome wizard shows once and lets the user pick
    // sources + update interval (then updates them right away), followed by
    // the background hosting mode, before starting the background.
    views::oobe::maybe_show(&config, &client);

    window.present();
}

/// Hamburger menu in the header bar's right side: the daemon's live task
/// queue (running/queued) plus Preferences and About entries.
fn build_primary_menu(
    header: &Adw::HeaderBar,
    client: &Rc<DaemonClient>,
    config: &equinox_core::Config,
) {
    let menu_btn = gtk4::MenuButton::new();
    // Button face is a Stack — hamburger icon when idle, spinner while any
    // daemon task is running/queued (swapped by the refresh closure below).
    // A Stack (rather than toggling icon-name) keeps the button at a constant
    // size; the hidden spinner costs nothing while idle (GTK only animates
    // mapped widgets).
    let menu_face = gtk4::Stack::new();
    let menu_idle_icon = gtk4::Image::from_icon_name("open-menu-symbolic");
    let menu_spinner = gtk4::Spinner::new();
    menu_spinner.set_spinning(true);
    menu_face.add_named(&menu_idle_icon, Some("idle"));
    menu_face.add_named(&menu_spinner, Some("busy"));
    menu_face.set_visible_child_name("idle");
    menu_btn.set_child(Some(&menu_face));
    // A custom child (the Stack) suppresses the automatic `.image-button`
    // class that makes header-bar icon buttons flat — without it the button
    // renders with a persistent raised background.
    menu_btn.add_css_class("flat");
    menu_btn.set_tooltip_text(Some(&gettext("Main menu")));

    let popover = gtk4::Popover::new();
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 10);
    box_.set_margin_top(12);
    box_.set_margin_bottom(8);
    box_.set_margin_start(12);
    box_.set_margin_end(12);
    box_.set_width_request(300);

    let tasks_title = gtk4::Label::new(Some(&gettext("Tasks")));
    tasks_title.add_css_class("title-4");
    tasks_title.add_css_class("eq-menu-title");
    tasks_title.set_halign(gtk4::Align::Start);
    box_.append(&tasks_title);

    let task_list = gtk4::ListBox::new();
    task_list.add_css_class("boxed-list");
    task_list.set_selection_mode(gtk4::SelectionMode::None);
    box_.append(&task_list);

    let sep = gtk4::Separator::new(gtk4::Orientation::Horizontal);
    box_.append(&sep);

    // Update timeline: persistent log of every update/apply run
    let timeline_btn = gtk4::Button::new();
    let timeline_content = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let timeline_icon = gtk4::Image::from_icon_name("preferences-system-time-symbolic");
    let timeline_label = gtk4::Label::new(Some(&gettext("Timeline")));
    timeline_content.append(&timeline_icon);
    timeline_content.append(&timeline_label);
    timeline_btn.set_child(Some(&timeline_content));
    timeline_btn.add_css_class("flat");
    timeline_btn.set_halign(gtk4::Align::Fill);
    {
        let client = Rc::clone(client);
        let popover = popover.clone();
        timeline_btn.connect_clicked(move |_| {
            // GTK4 popovers don't auto-close on inner clicks — pop down so
            // the new window/dialog isn't left under a floating menu.
            popover.popdown();
            crate::views::timeline::build_window(&client).present();
        });
    }
    box_.append(&timeline_btn);

    // Preferences: display language (applied after restarting the GUI)
    let prefs_btn = gtk4::Button::new();
    let prefs_content = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let prefs_icon = gtk4::Image::from_icon_name("emblem-system-symbolic");
    let prefs_label = gtk4::Label::new(Some(&gettext("Preferences")));
    prefs_content.append(&prefs_icon);
    prefs_content.append(&prefs_label);
    prefs_btn.set_child(Some(&prefs_content));
    prefs_btn.add_css_class("flat");
    prefs_btn.set_halign(gtk4::Align::Fill);
    {
        let cfg = config.clone();
        let client_prefs = Rc::clone(client);
        let popover = popover.clone();
        prefs_btn.connect_clicked(move |btn| {
            popover.popdown();
            open_preferences(btn, &client_prefs, &cfg);
        });
    }
    box_.append(&prefs_btn);

    let about_btn = gtk4::Button::new();
    let about_content = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let about_icon = gtk4::Image::from_icon_name("help-about-symbolic");
    let about_label = gtk4::Label::new(Some(&gettext("About")));
    about_content.append(&about_icon);
    about_content.append(&about_label);
    about_btn.set_child(Some(&about_content));
    about_btn.add_css_class("flat");
    about_btn.set_halign(gtk4::Align::Fill);
    {
        let popover = popover.clone();
        about_btn.connect_clicked(move |btn| {
            popover.popdown();
            // Adw::AboutDialog replaces the deprecated AboutWindow (libadwaita 1.6+)
            let dialog = Adw::AboutDialog::builder()
                .application_name("Equinox")
                .application_icon("equinox")
                .version(env!("CARGO_PKG_VERSION"))
                .comments(gettext("Multi-source daily wallpaper manager").as_str())
                .developer_name("nwkyz")
                .license(
                    format!(
                        "{} {}",
                        gettext("Released under the"),
                        "GNU GPL v3 or later"
                    )
                    .as_str(),
                )
                .build();
            let root = btn.root();
            dialog.present(root.as_ref());
        });
    }
    box_.append(&about_btn);

    popover.set_child(Some(&box_));
    menu_btn.set_popover(Some(&popover));
    header.pack_end(&menu_btn);

    // Refresh the menu button face + task rows. The face (hamburger vs
    // spinner) follows every TasksChanged even while the popover is closed;
    // the row list itself is only rebuilt while the popover is visible.
    let refresh = {
        let client = Rc::clone(client);
        let list = task_list.clone();
        let face = menu_face.clone();
        let btn = menu_btn.clone();
        move || {
            let client = Rc::clone(&client);
            let list = list.clone();
            let face = face.clone();
            let btn = btn.clone();
            glib::spawn_future_local(async move {
                let rows: Vec<TaskRow> = client.tasks().await.unwrap_or_default();
                face.set_visible_child_name(if rows.is_empty() { "idle" } else { "busy" });
                if !btn.is_active() {
                    return; // popover closed: rows are rebuilt on next open
                }
                while let Some(c) = list.first_child() {
                    list.remove(&c);
                }
                if rows.is_empty() {
                    let empty = gtk4::Label::new(Some(&gettext("No running tasks")));
                    empty.add_css_class("dim-label");
                    empty.set_halign(gtk4::Align::Center);
                    empty.set_margin_top(10);
                    empty.set_margin_bottom(10);
                    let row = adw_row(&empty);
                    list.append(&row);
                    return;
                }
                for (id, label, running, done, total) in rows {
                    let label = if running && total > 0 {
                        format!("{label} ({done}/{total})")
                    } else {
                        label
                    };
                    let row_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
                    row_box.set_margin_top(6);
                    row_box.set_margin_bottom(6);
                    row_box.set_margin_start(10);
                    row_box.set_margin_end(10);
                    if running {
                        let spinner = gtk4::Spinner::new();
                        spinner.set_spinning(true);
                        row_box.append(&spinner);
                    } else {
                        let icon = gtk4::Image::from_icon_name("process-working-symbolic");
                        icon.add_css_class("dim-label");
                        row_box.append(&icon);
                    }
                    let lbl = gtk4::Label::new(Some(&label));
                    lbl.set_halign(gtk4::Align::Start);
                    lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                    if running {
                        lbl.add_css_class("heading");
                    } else {
                        lbl.add_css_class("dim-label");
                    }
                    row_box.append(&lbl);
                    // Push a spacer, then a SMALL ✓-off ✕ button at the far
                    // right that must not stretch the row (the row height
                    // stays driven by the spinner/label).
                    let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
                    spacer.set_hexpand(true);
                    row_box.append(&spacer);
                    let cancel_btn = gtk4::Button::from_icon_name("window-close-symbolic");
                    cancel_btn.add_css_class("flat");
                    cancel_btn.set_valign(gtk4::Align::Center);
                    cancel_btn.set_size_request(18, 18);
                    cancel_btn.set_can_focus(false);
                    cancel_btn.set_tooltip_text(Some(&gettext("Cancel")));
                    {
                        let c_cancel = Rc::clone(&client);
                        let tid = id;
                        cancel_btn.connect_clicked(move |_| {
                            let c2 = Rc::clone(&c_cancel);
                            glib::spawn_future_local(async move {
                                if let Err(e) = c2.cancel_task(tid).await {
                                    log::warn!("cancel task {tid} failed: {e:#}");
                                }
                            });
                        });
                    }
                    row_box.append(&cancel_btn);
                    let row = adw_row(&row_box);
                    list.append(&row);
                }
            });
        }
    };
    // Face + row list follow every queue change (a few cheap D-Bus roundtrips
    // per task: enqueue/start/progress-steps/finish).
    {
        let refresh_sig = refresh.clone();
        client.set_on_tasks_changed(move || refresh_sig());
    }
    menu_btn.connect_active_notify({
        let refresh_open = refresh.clone();
        move |btn| {
            if btn.is_active() {
                refresh_open();
            }
        }
    });
    // Keep the face honest across the daemon lifecycle: a fresh GUI start with
    // tasks already in flight shows the spinner, and a vanished daemon resets
    // to idle (its queue is gone — no TasksChanged will announce that).
    {
        let refresh_run = refresh.clone();
        client.set_on_running_changed(move |_| refresh_run());
    }
    refresh(); // initial face, in case the daemon is already running tasks
}

/// Wrap a widget in a ListBoxRow.
fn adw_row(child: &impl AsRef<gtk4::Widget>) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.set_child(Some(child.as_ref()));
    row
}

/// Preferences dialog: UI language + background hosting controls.
///
/// The language row applies after a GUI restart (gettext is initialized once
/// at startup). The background group lets the user pick how the daemon is
/// hosted (standalone supervisor vs. systemd user service), whether it starts
/// at login, and — outside flatpak — install/uninstall the systemd service.
/// In standalone mode a live daemon-status row (running/stopped + start/stop)
/// polls only while this dialog is open.
fn open_preferences(
    btn: &gtk4::Button,
    client: &Rc<DaemonClient>,
    config: &equinox_core::Config,
) {
    let cfg = config.clone();
    let client = Rc::clone(client);

    let page = Adw::PreferencesPage::new();

    // ---- UI group: display language ----
    let ui_group = Adw::PreferencesGroup::new();
    ui_group.set_title(&gettext("Interface"));
    page.add(&ui_group);

    // (value, display) — values are gettext locale names as in po/*.po.
    let languages: Vec<(&str, String)> = vec![
        ("auto", gettext("Auto (follow system)")),
        ("en", "English".to_owned()),
        ("zh_CN", "中文 (普通话)".to_owned()),
        ("yue", "中文 (粤语)".to_owned()),
        ("lzh", "中文 (文言)".to_owned()),
        ("ja", "日本語".to_owned()),
        ("ru", "Русский".to_owned()),
        ("bo", "བོད་ཡིག".to_owned()),
    ];
    let labels: Vec<&str> = languages.iter().map(|(_, l)| l.as_str()).collect();
    let current = config.language();
    let selected = languages
        .iter()
        .position(|(v, _)| *v == current)
        .unwrap_or(0) as u32;

    let lang_row = Adw::ComboRow::new();
    lang_row.set_title(&gettext("Language"));
    lang_row.set_model(Some(&gtk4::StringList::new(&labels)));
    lang_row.set_selected(selected);
    ui_group.add(&lang_row);

    // ---- Background group: hosting mode / autostart / systemd ----
    let bg_group = Adw::PreferencesGroup::new();
    bg_group.set_title(&gettext("Background service"));
    page.add(&bg_group);

    let flatpak = equinox_core::autostart::in_flatpak();
    let systemd_ok = equinox_core::autostart::systemd_available();

    // Hosting mode: standalone supervisor (default) or systemd user service.
    // The systemd option only exists when a systemd user instance is actually
    // reachable (never inside flatpak, where the sandbox has no host systemd).
    let mut modes: Vec<(&str, String)> = vec![("supervisor", gettext("Standalone"))];
    if systemd_ok {
        modes.push(("systemd", "systemd".to_owned()));
    }
    let mode_labels: Vec<&str> = modes.iter().map(|(_, l)| l.as_str()).collect();
    let mode_row = Adw::ComboRow::new();
    mode_row.set_title(&gettext("Run background via"));
    mode_row.set_model(Some(&gtk4::StringList::new(&mode_labels)));
    // Initial selection follows reality: when the systemd service is already
    // installed/running (e.g. the user set it up manually or upgraded from a
    // systemd install), show "systemd" even if the config lagged behind.
    let cur_mode: String = if equinox_core::autostart::systemd_enabled()
        || equinox_core::autostart::systemd_running()
    {
        "systemd".to_owned()
    } else {
        config.run_mode()
    };
    let cur_mode_idx = modes
        .iter()
        .position(|(v, _)| *v == cur_mode.as_str())
        .unwrap_or(0) as u32;
    mode_row.set_selected(cur_mode_idx);
    bg_group.add(&mode_row);

    // Live daemon status + start/stop, shown only in standalone mode (systemd
    // mode has its own run control on the "systemd service" row).
    let daemon_status_row = Adw::ActionRow::new();
    daemon_status_row.set_title(&gettext("Daemon status"));
    let daemon_state = gtk4::Label::new(None);
    daemon_state.add_css_class("dim-label");
    let daemon_btn = gtk4::Button::with_label(&gettext("Start"));
    daemon_btn.add_css_class("suggested-action");
    daemon_btn.set_valign(gtk4::Align::Center);
    daemon_status_row.add_suffix(&daemon_state);
    daemon_status_row.add_suffix(&daemon_btn);
    bg_group.add(&daemon_status_row);

    if flatpak {
        // Explain why the systemd option is missing inside flatpak.
        let hint = Adw::ActionRow::new();
        hint.set_title(&gettext("systemd is unavailable in flatpak"));
        hint.set_subtitle(&gettext(
            "Sandboxed apps cannot reach the host systemd user session, so the background runs as a supervised process inside the sandbox.",
        ));
        bg_group.add(&hint);
    }

    // "Start at login" means different things per hosting mode:
    // - Standalone: the XDG autostart entry (config.autostart flag)
    // - systemd:     systemctl --user enable/disable of the unit
    // The row follows the currently selected mode and is never greyed out.
    let autostart_row = Adw::SwitchRow::new();
    autostart_row.set_title(&gettext("Start at login"));
    bg_group.add(&autostart_row);

    // systemd service row: deploy state (install/uninstall) + run state
    // (start/stop). Hidden entirely inside flatpak — there is no host
    // systemd user instance to talk to.
    let systemd_action = Adw::ActionRow::new();
    systemd_action.set_title(&gettext("systemd service"));
    let sysd_state = gtk4::Label::new(None);
    sysd_state.add_css_class("dim-label");
    let sysd_toggle_btn = gtk4::Button::new(); // Start / Stop
    sysd_toggle_btn.set_valign(gtk4::Align::Center);
    let sysd_btn = gtk4::Button::with_label(&gettext("Install")); // Install / Uninstall
    sysd_btn.add_css_class("suggested-action");
    sysd_btn.set_valign(gtk4::Align::Center);
    systemd_action.add_suffix(&sysd_state);
    systemd_action.add_suffix(&sysd_toggle_btn);
    systemd_action.add_suffix(&sysd_btn);
    if !flatpak {
        bg_group.add(&systemd_action);
    }

    let dialog_ref = Adw::PreferencesDialog::builder()
        .title(&gettext("Preferences"))
        // Do not lock the window to the content size: allow the user to
        // resize it freely (content_width/height only set the initial size).
        .follows_content_size(false)
        .content_width(520)
        .content_height(560)
        .build();
    dialog_ref.add(&page);

    // Keep a handle to the dialog for toasts.
    let dialog = dialog_ref.clone();

    // Refresh the background sub-row states: systemd install/run state, the
    // start/stop button, the standalone daemon status row and the autostart
    // switch (per current mode).
    let refresh_state = {
        let sysd_btn = sysd_btn.clone();
        let sysd_toggle_btn = sysd_toggle_btn.clone();
        let sysd_state = sysd_state.clone();
        let autostart_row = autostart_row.clone();
        let mode_row = mode_row.clone();
        let modes = modes.clone();
        let cfg = cfg.clone();
        let daemon_status_row = daemon_status_row.clone();
        let daemon_state = daemon_state.clone();
        let daemon_btn = daemon_btn.clone();
        let client = Rc::clone(&client);
        move || {
            let mode: String = modes
                .get(mode_row.selected() as usize)
                .map(|(v, _)| (*v).to_owned())
                .unwrap_or_else(|| "supervisor".to_owned());
            let installed = equinox_core::autostart::systemd_unit_installed()
                || equinox_core::autostart::systemd_enabled();
            let running = equinox_core::autostart::systemd_running();
            // Deploy button (Install/Uninstall)
            sysd_btn.set_label(&if installed {
                gettext("Uninstall")
            } else {
                gettext("Install")
            });
            sysd_btn.remove_css_class("suggested-action");
            sysd_btn.remove_css_class("destructive-action");
            if installed {
                sysd_btn.add_css_class("destructive-action");
            } else {
                sysd_btn.add_css_class("suggested-action");
            }
            // Run-state button (Start/Stop) — only meaningful once deployed.
            sysd_toggle_btn.set_sensitive(installed);
            sysd_toggle_btn.set_label(&if running {
                gettext("Stop")
            } else {
                gettext("Start")
            });
            sysd_toggle_btn.remove_css_class("suggested-action");
            sysd_toggle_btn.remove_css_class("destructive-action");
            if running {
                sysd_toggle_btn.add_css_class("destructive-action");
            } else if installed {
                sysd_toggle_btn.add_css_class("suggested-action");
            }
            sysd_state.set_text(&if installed {
                if running {
                    gettext("Installed and running")
                } else {
                    gettext("Installed, not running")
                }
            } else {
                gettext("Not installed")
            });
            // Standalone daemon status row: visible only in supervisor mode.
            if mode == "systemd" {
                daemon_status_row.set_visible(false);
                // systemd hosting: reflect systemctl is-enabled.
                autostart_row.set_active(equinox_core::autostart::systemd_boot_enabled());
            } else {
                daemon_status_row.set_visible(true);
                refresh_daemon_status(&client, &daemon_state, &daemon_btn);
                // Standalone hosting: reflect the XDG autostart config flag.
                autostart_row.set_active(cfg.autostart());
            }
        }
    };
    refresh_state();

    // Language change → toast (applies after GUI restart).
    {
        let cfg = cfg.clone();
        let dialog_toast = dialog.clone();
        lang_row.connect_selected_notify(move |row| {
            if let Some((value, _)) = languages.get(row.selected() as usize) {
                if *value != cfg.language() {
                    cfg.set_language(value);
                    dialog_toast.add_toast(Adw::Toast::new(&gettext(
                        "Restart Equinox to apply the new language",
                    )));
                }
            }
        });
    }

    // Mode change → persist run-mode and switch the live backend without
    // racing the two hosting mechanisms.
    {
        let cfg = cfg.clone();
        let refresh = refresh_state.clone();
        let dialog_err = dialog.clone();
        let modes = modes.clone();
        mode_row.connect_selected_notify(move |row| {
            let idx = row.selected() as usize;
            let Some((value, _)) = modes.get(idx) else { return };
            if *value == "systemd" && !systemd_ok {
                // Flatpak / no systemd: cannot honour the selection.
                return;
            }
            // Already effectively in this mode? (config may lag behind when
            // the user set up systemd manually.) For systemd, "effective"
            // means the CURRENT unit is installed and running — a legacy
            // equinox-daemon.service still triggers a migration via
            // install_systemd_service below.
            let sysd_current = equinox_core::autostart::systemd_unit_installed()
                && equinox_core::autostart::systemd_running();
            let sysd_any =
                equinox_core::autostart::systemd_enabled() || equinox_core::autostart::systemd_running();
            if (*value == "systemd" && sysd_current)
                || (*value == "supervisor" && !sysd_any)
            {
                cfg.set_run_mode(value);
                return;
            }
            cfg.set_run_mode(value);
            if let Err(e) = crate::app::switch_backend(value) {
                let msg = format!("{}: {e}", gettext("Failed to change the systemd service"));
                dialog_err.add_toast(crate::views::toast(&msg));
            }
            refresh();
        });
    }

    // Autostart toggle: semantics depend on the hosting mode shown in the
    // mode row — systemd mode toggles `systemctl enable/disable`, standalone
    // mode toggles the XDG autostart entry (config.autostart).
    {
        let cfg = cfg.clone();
        let mode_row = mode_row.clone();
        let modes = modes.clone();
        autostart_row.connect_active_notify(move |sw| {
            let on = sw.is_active();
            let mode = modes
                .get(mode_row.selected() as usize)
                .map(|(v, _)| *v)
                .unwrap_or("supervisor");
            if mode == "systemd" {
                // The switch already reflects is-enabled; just apply it.
                equinox_core::autostart::set_systemd_boot(on);
            } else {
                cfg.set_autostart(on);
                let _ = equinox_core::autostart::set_autostart(on);
            }
        });
    }

    // systemd install/uninstall button. Installing = switch to systemd
    // hosting (deploy + enable + start). Uninstalling = fully remove the
    // unit (stop + disable + delete the file) and switch back to the
    // standalone supervisor. This is the ONLY place the unit is deleted —
    // switching the mode row to "Standalone" merely stops+disables it.
    {
        let cfg = cfg.clone();
        let refresh = refresh_state.clone();
        let dialog_err = dialog.clone();
        let mode_row = mode_row.clone();
        let modes = modes.clone();
        sysd_btn.connect_clicked(move |_| {
            let installing = !equinox_core::autostart::systemd_unit_installed();
            let res = if installing {
                crate::app::switch_backend("systemd")
            } else {
                // Real uninstall: stop, disable, delete the unit file.
                equinox_core::autostart::uninstall_systemd_service()
                    .map_err(|e| format!("{e:#}"))
            };
            match res {
                Ok(()) => {
                    let target = if installing { "systemd" } else { "supervisor" };
                    cfg.set_run_mode(target);
                    if !installing && pgrep_quiet("equinox-supervisor") == 0 {
                        // Unit gone and no supervisor running: bring the
                        // standalone supervisor up.
                        let _ = spawn_background("equinox-supervisor");
                    }
                    // Sync the mode drop-down; its selected_notify sees the
                    // new state already effective and only persists run-mode.
                    if let Some(idx) = modes.iter().position(|(v, _)| *v == target) {
                        mode_row.set_selected(idx as u32);
                    }
                    refresh();
                }
                Err(e) => {
                    let msg = format!("{}: {e}", gettext("Failed to change the systemd service"));
                    dialog_err.add_toast(crate::views::toast(&msg));
                }
            }
        });
    }

    // Start/Stop the deployed systemd service (independent of deploy state).
    {
        let refresh = refresh_state.clone();
        let dialog_err = dialog.clone();
        sysd_toggle_btn.connect_clicked(move |_| {
            let running = equinox_core::autostart::systemd_running();
            let res = if running {
                equinox_core::autostart::systemd_stop();
                Ok(())
            } else {
                equinox_core::autostart::systemd_start()
            };
            if let Err(e) = res {
                let msg = format!("{}: {e}", gettext("Failed to change the systemd service"));
                dialog_err.add_toast(crate::views::toast(&msg));
            }
            refresh();
        });
    }

    // Standalone daemon status row: start/stop button.
    {
        let refresh = refresh_state.clone();
        daemon_btn.clone().connect_clicked(move |_| {
            if client.running() {
                // Stop: kill the standalone supervisor (its daemon child
                // follows); systemd-hosted daemons are stopped separately.
                kill_standalone();
            } else {
                start_daemon();
            }
            // Re-derive the whole dependent state (status label + Start/Stop
            // button + autostart switch), not just this row's label.
            refresh();
            // The daemon's D-Bus name appears or vanishes a moment AFTER the
            // process is spawned/killed, so the status flips slightly later.
            // Re-poll at short intervals right after the click for quick
            // feedback; the steady 1.5 s dialog poll keeps it current.
            let dialog_quick = dialog.clone();
            for delay_ms in [400, 900] {
                let r = refresh.clone();
                let dlg = dialog_quick.clone();
                glib::timeout_add_local_once(
                    std::time::Duration::from_millis(delay_ms),
                    move || {
                        if dlg.is_visible() {
                            r();
                        }
                    },
                );
            }
        });
    }

    let root = btn.root();
    dialog_ref.present(root.as_ref());

    // Poll the daemon status ONLY while the dialog is open. The source checks
    // the dialog's visibility on every tick and stops itself once closed, so
    // there is no background cost after Preferences is dismissed.
    {
        let dialog_poll = dialog_ref.clone();
        let refresh_poll = refresh_state.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(1500), move || {
            if !dialog_poll.is_visible() {
                return glib::ControlFlow::Break;
            }
            refresh_poll();
            glib::ControlFlow::Continue
        });
    }
}

/// Public helper for the OOBE welcome page: start the configured background
/// service (supervisor or systemd) without waiting for the daemon banner.
pub fn spawn_background_if_needed() {
    start_daemon();
}

/// Refresh the standalone daemon status row: running label + Start/Stop
/// button state. Uses the D-Bus client's live online state.
fn refresh_daemon_status(client: &Rc<DaemonClient>, state: &gtk4::Label, btn: &gtk4::Button) {
    let running = client.running();
    state.set_text(&if running {
        gettext("Running")
    } else {
        gettext("Not running")
    });
    btn.set_label(&if running {
        gettext("Stop")
    } else {
        gettext("Start")
    });
    btn.remove_css_class("suggested-action");
    btn.remove_css_class("destructive-action");
    if running {
        btn.add_css_class("destructive-action");
    } else {
        btn.add_css_class("suggested-action");
    }
}

/// Switch the background to a different hosting mode without racing the two
/// mechanisms:
/// - to "systemd": kill any standalone supervisor/daemon, then deploy (if
///   needed), enable and start the systemd unit;
/// - to "supervisor": stop+disable the systemd service (the unit file stays
///   deployed so switching back is instant), then spawn the standalone
///   supervisor.
/// Used by the Preferences mode row and the systemd install/uninstall button.
pub fn switch_backend(mode: &str) -> Result<(), String> {
    match mode {
        "systemd" => {
            // Stop any standalone supervisor/daemon first so it cannot hold
            // the D-Bus name when the systemd unit starts.
            kill_standalone();
            std::thread::sleep(std::time::Duration::from_millis(300));
            equinox_core::autostart::install_systemd_service().map_err(|e| format!("{e:#}"))
        }
        _ => {
            // Stop + disable the systemd service but keep the unit deployed,
            // then run the standalone supervisor. The unit stays in place so
            // switching back to systemd does not require a re-install.
            equinox_core::autostart::deactivate_systemd_service();
            std::thread::sleep(std::time::Duration::from_millis(300));
            let _ = spawn_background("equinox-supervisor");
            Ok(())
        }
    }
}

/// pgrep-style process count for a binary name (0 = not running). Uses
/// `pgrep -f` because `-x` matches the kernel-truncated comm name, which for
/// "equinox-supervisor" (17 chars) never matches.
fn pgrep_quiet(bin: &str) -> i32 {
    std::process::Command::new("pgrep")
        .args(["-f", bin])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| if s.success() { 1 } else { 0 })
        .unwrap_or(0)
}

/// Kill standalone supervisor/daemon processes (used before switching to the
/// systemd hosting so the standalone processes release the D-Bus name).
fn kill_standalone() {
    // pkill -x matches the kernel-truncated comm name (15 chars max);
    // "equinox-supervisor" (17) never matches — match the full command line.
    for bin in ["equinox-supervisor", "equinox-daemon"] {
        let _ = std::process::Command::new("pkill")
            .args(["-f", &format!("(^|/){bin}($| )")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// Start the background service according to the configured run mode:
/// - "systemd": ensure the (optional) systemd user service is installed and
///   start it;
/// - otherwise (default "supervisor"): spawn `equinox-supervisor`, which in
///   turn spawns and supervises the daemon.
/// Spawning is fast and synchronous; the banner hides via the D-Bus online
/// signal once the daemon is up.
fn start_daemon() {
    let mode = equinox_core::Config::new()
        .map(|c| c.run_mode())
        .unwrap_or_else(|_| "supervisor".into());
    if mode == "systemd" {
        // systemd mode: (re)deploy the unit with an absolute ExecStart (this
        // heals units written with a bare command — systemd user services do
        // not inherit ~/.local/bin on PATH and would fail with 203/EXEC),
        // then enable + start it. Legacy equinox-daemon.service units are
        // removed by deploy_unit.
        if let Err(e) = equinox_core::autostart::deploy_unit() {
            log::warn!("failed to deploy systemd unit: {e:#}; falling back to supervisor");
            let _ = spawn_background("equinox-supervisor");
            return;
        }
        let unit = "equinox-supervisor.service";
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "enable", unit])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let ok = std::process::Command::new("systemctl")
            .args(["--user", "start", unit])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return;
        }
        log::warn!("systemctl start failed; falling back to spawning the supervisor directly");
    }
    let _ = spawn_background("equinox-supervisor");
}

/// Spawn a background binary, honoring the packaging layout: inside a
/// flatpak, `$FLATPAK_ID` is set and the binary is reached via
/// `flatpak run --command=…`; in an AppImage it lives next to the GUI under
/// `$APPDIR/usr/bin`; dev/native installs resolve it on `$PATH`.
fn spawn_background(bin: &str) -> Option<std::process::Child> {
    use std::os::unix::process::CommandExt;
    // Own process group: terminal Ctrl-C / hangup sent to the launcher's group
    // must not take the background down with it (the supervisor additionally
    // calls setsid() in its own main).
    let spawn = |cmd: &mut std::process::Command| cmd.process_group(0).spawn().ok();
    if let Ok(app) = std::env::var("FLATPAK_ID") {
        if !app.is_empty() {
            // Inside a flatpak sandbox there is no `flatpak` CLI, so exec the
            // sibling binary directly from /app/bin (it stays inside the
            // current sandbox). NOTE: a directly-spawned child lives only as
            // long as this instance's sandbox; on flatpak the login autostart
            // entry (`flatpak run --command=…` on the host) is how the
            // background survives GUI close.
            let in_sandbox = std::path::PathBuf::from("/app/bin").join(bin);
            if in_sandbox.is_file() {
                return spawn(&mut std::process::Command::new(&in_sandbox));
            }
            // GUI launched outside the sandbox but with FLATPAK_ID (rare):
            // re-enter the sandbox through the host flatpak CLI.
            return spawn(&mut std::process::Command::new("flatpak").args([
                "run",
                &format!("--command={bin}"),
                &app,
            ]));
        }
    }
    // AppImage: the daemon is bundled next to the GUI.
    let mut candidates: Vec<std::path::PathBuf> = std::env::var("APPDIR")
        .ok()
        .map(|d| std::path::PathBuf::from(d).join("usr/bin").join(bin))
        .into_iter()
        .collect();
    // Plain PATH lookup (native install / dev).
    candidates.push(std::path::PathBuf::from(bin));
    for c in candidates {
        if let Some(child) = spawn(&mut std::process::Command::new(&c)) {
            return Some(child);
        }
    }
    None
}

/// Fill the Lorem Picsum source's width/height from the primary monitor.
///
/// Only applied while the user has not chosen a manual size (the stored value
/// is still 0, i.e. "auto"). This runs once per GUI launch so the daemon —
/// which may be headless — can rely on the screen size being recorded.
fn auto_fill_picsum_size(config: &equinox_core::Config) {
    let Some((w, h)) = crate::views::primary_monitor_size() else {
        log::debug!("auto-fill picsum size: no primary monitor geometry yet; skipped");
        return;
    };
    let settings = config.settings_for("picsum");
    if settings.get_i64("width", 0) <= 0 {
        config.set_source_setting("picsum", "width", serde_json::json!(w));
    }
    if settings.get_i64("height", 0) <= 0 {
        config.set_source_setting("picsum", "height", serde_json::json!(h));
    }
    log::info!("auto-filled Lorem Picsum size: {w}x{h}");
}
