//! Timeline window: ONE unified list, icon-coded.
//!
//! - green check  — finished successfully
//! - blue clock   — planned / pending (per-source upcoming schedule, queued
//!                  or running tasks)
//! - red cross    — failed
//! - gray dash    — skipped (window missed while the machine was off) or
//!                  scheduled updates turned off for that source

use std::rc::Rc;

use libadwaita as Adw;
use libadwaita::prelude::*;

use gettextrs::gettext;

use crate::daemon_client::{DaemonClient, ScheduleRow, TaskLogRow};

/// Unix timestamp → local "YYYY-MM-DD HH:MM:SS".
fn fmt_ts(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

/// Humanized interval: "45s" / "5min" / "2h" / "1d".
fn fmt_interval(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}{}", gettext("sec"))
    } else if secs < 3600 {
        format!("{}{}", secs / 60, gettext("min"))
    } else if secs < 86400 {
        format!("{}{}", secs / 3600, gettext("h"))
    } else {
        format!("{}{}", secs / 86400, gettext("d"))
    }
}

fn kind_label(kind: &str) -> String {
    match kind {
        "scheduled" => gettext("Scheduled"),
        "apply" => gettext("Apply"),
        _ => gettext("Manual"),
    }
}

/// Build the timeline window (not yet shown). Refreshes live while open.
pub fn build_window(client: &Rc<DaemonClient>) -> Adw::Window {
    let win = Adw::Window::builder()
        .title(&gettext("Timeline"))
        .default_width(680)
        .default_height(660)
        .modal(true)
        .build();

    // Native header bar: renders the window title itself at standard size.
    let header = Adw::HeaderBar::new();

    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 10);
    vbox.set_margin_top(14);
    vbox.set_margin_bottom(18);
    vbox.set_margin_start(18);
    vbox.set_margin_end(18);

    let policy = gtk4::Label::new(Some(&gettext(
        "Each source updates on its own interval. Windows missed while the computer was off or asleep are caught up right after boot or wake-up.",
    )));
    policy.add_css_class("dim-label");
    policy.add_css_class("caption");
    policy.set_wrap(true);
    policy.set_xalign(0.0);
    vbox.append(&policy);

    let list = gtk4::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk4::SelectionMode::None);
    list.set_vexpand(true);

    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroll.set_child(Some(&list));
    scroll.set_vexpand(true);
    vbox.append(&scroll);

    let refresh = {
        let client = Rc::clone(client);
        let list = list.clone();
        move || {
            let client = Rc::clone(&client);
            let list = list.clone();
            glib::spawn_future_local(async move {
                let mut items: Vec<TlEntry> = Vec::new();

                // ---- Update PLAN: every source, upcoming run or off ----
                let schedule: Vec<ScheduleRow> = client.schedule().await.unwrap_or_default();
                for (id, enabled, interval, _last, next_due) in &schedule {
                    let name = equinox_core::source::by_id(id)
                        .map(|s| s.display_name())
                        .unwrap_or_else(|| id.clone());
                    if *enabled {
                        items.push(TlEntry {
                            icon: Icon::Clock,
                            title: format!("{} {}", gettext("Planned"), name),
                            meta: format!(
                                "{} {} · {} {}",
                                gettext("Next"),
                                fmt_ts(*next_due),
                                gettext("Every"),
                                fmt_interval(*interval)
                            ),
                            error: String::new(),
                            spinner: false,
                        });
                    } else {
                        items.push(TlEntry {
                            icon: Icon::Dash,
                            title: name,
                            meta: gettext("Scheduled updates are off"),
                            error: String::new(),
                            spinner: false,
                        });
                    }
                }

                // ---- Execution LOG: newest first ----
                let rows: Vec<TaskLogRow> = client.task_log().await.unwrap_or_default();
                for (start, label, kind, catchup, done, ok, error, step, total, retries) in rows {
                    let icon = if kind == "skipped" {
                        Icon::Dash
                    } else if !done {
                        Icon::Clock
                    } else if ok {
                        Icon::Check
                    } else {
                        Icon::Cross
                    };
                    let mut title = label;
                    if catchup && kind != "skipped" {
                        title.push_str(&format!(" {}", gettext("(catch-up)")));
                    }
                    // Automatic retries (scheduled updates): show how many
                    // retries happened before this run finished.
                    if retries > 0 && kind != "skipped" {
                        title.push_str(&format!(" ({retries})"));
                    }
                    let mut meta = format!("{} · {}", fmt_ts(start), kind_label(&kind));
                    if !done && total > 0 {
                        meta.push_str(&format!(" · {step}/{total}"));
                    }
                    if done && ok {
                        meta.push_str(&gettext(" · Success"));
                    }
                    items.push(TlEntry {
                        icon,
                        title,
                        meta,
                        error,
                        spinner: !done,
                    });
                }

                clear(&list);
                if items.is_empty() {
                    let empty = gtk4::Label::new(Some(&gettext("No tasks recorded yet")));
                    empty.add_css_class("dim-label");
                    empty.set_margin_top(24);
                    empty.set_margin_bottom(24);
                    list.append(&row_of(&empty));
                    return;
                }
                for e in items {
                    list.append(&row_of(&build_row(&e)));
                }
            });
        }
    };

    // Live refresh while the window is open + initial load on open
    let refresh_sig = refresh.clone();
    let win_sig = win.clone();
    client.set_on_task_log_changed(move || {
        if win_sig.is_visible() {
            refresh_sig();
        }
    });
    win.connect_map({
        let refresh_map = refresh.clone();
        move |_| refresh_map()
    });
    refresh();

    let content = Adw::ToolbarView::new();
    content.add_top_bar(&header);
    content.set_content(Some(&vbox));
    win.set_content(Some(&content));
    win
}

struct TlEntry {
    icon: Icon,
    title: String,
    meta: String,
    error: String,
    spinner: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Icon {
    Check,
    Clock,
    Cross,
    Dash,
}

impl Icon {
    fn name(self) -> &'static str {
        match self {
            Icon::Check => "object-select-symbolic",
            Icon::Clock => "preferences-system-time-symbolic",
            Icon::Cross => "window-close-symbolic",
            Icon::Dash => "",
        }
    }

    fn css(self) -> &'static str {
        match self {
            Icon::Check => "success",
            Icon::Clock => "tl-blue",
            Icon::Cross => "error",
            Icon::Dash => "tl-gray",
        }
    }
}

/// One row: [status icon] [title / meta / error].
fn build_row(e: &TlEntry) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    row.set_margin_top(8);
    row.set_margin_bottom(8);
    row.set_margin_start(14);
    row.set_margin_end(14);
    row.set_valign(gtk4::Align::Center);

    // Status icon (gray dash is drawn as a small bar, not an icon)
    if e.icon == Icon::Dash {
        let dash = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        dash.add_css_class("tl-dash");
        dash.set_valign(gtk4::Align::Center);
        row.append(&dash);
    } else if e.spinner {
        let sp = gtk4::Spinner::new();
        sp.set_spinning(true);
        row.append(&sp);
    } else {
        let img = gtk4::Image::from_icon_name(e.icon.name());
        img.add_css_class(e.icon.css());
        row.append(&img);
    }

    let text = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    text.set_valign(gtk4::Align::Center);
    text.set_hexpand(true);

    let title = gtk4::Label::new(Some(&e.title));
    title.set_halign(gtk4::Align::Start);
    title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    title.set_single_line_mode(true);
    text.append(&title);

    let meta_lbl = gtk4::Label::new(Some(&e.meta));
    meta_lbl.add_css_class("dim-label");
    meta_lbl.add_css_class("caption");
    meta_lbl.set_halign(gtk4::Align::Start);
    meta_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    meta_lbl.set_single_line_mode(true);
    text.append(&meta_lbl);

    if !e.error.is_empty() {
        let err = gtk4::Label::new(Some(&e.error));
        err.add_css_class("error");
        err.add_css_class("caption");
        err.set_halign(gtk4::Align::Start);
        err.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        err.set_single_line_mode(true);
        text.append(&err);
    }
    row.append(&text);
    row
}

fn clear(list: &gtk4::ListBox) {
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
}

fn row_of(child: &impl AsRef<gtk4::Widget>) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.set_child(Some(child.as_ref()));
    row
}
