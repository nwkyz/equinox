//! D-Bus server side (github.nwkyz.Equinox.Daemon1).

use std::rc::Rc;

use gio::prelude::*;
use glib::Variant;
use gettextrs::gettext;

use equinox_core::dbus::{BUS_NAME, INTROSPECTION_XML, OBJECT_PATH};
use equinox_core::storage::Storage;

use crate::scheduler::{self, spawn_apply, spawn_update};
use crate::DaemonState;

/// Register the session bus name and D-Bus object.
pub fn setup_bus(state: &Rc<DaemonState>) {
    let state = Rc::clone(state);
    let state_lost = state.clone();
    gio::bus_own_name(
        gio::BusType::Session,
        BUS_NAME,
        gio::BusNameOwnerFlags::NONE,
        move |conn, _name| {
            log::info!("connected to the session bus");
            *state.conn.borrow_mut() = Some(conn.clone());

            let node =
                gio::DBusNodeInfo::for_xml(INTROSPECTION_XML).expect("introspection XML must be valid");
            let iface = node.interfaces().first().expect("at least one interface").clone();
            let st = Rc::clone(&state);
            let registration = conn
                .register_object(OBJECT_PATH, &iface)
                .method_call(move |_conn, _sender, _obj, _iface, method, params, invocation| {
                    handle_method(&st, method, &params, invocation);
                })
                .build();
            match registration {
                Ok(id) => *state.registration.borrow_mut() = Some(id),
                Err(e) => log::error!("failed to register D-Bus object: {e}"),
            }
        },
        |_conn, name| log::info!("D-Bus name acquired: {name}"),
        move |conn, _name| {
            log::warn!("D-Bus name lost (connection exists: {})", conn.is_some());
            *state_lost.conn.borrow_mut() = None;
        },
    );
}

fn handle_method(
    state: &Rc<DaemonState>,
    method: &str,
    params: &Variant,
    invocation: gio::DBusMethodInvocation,
) {
    match method {
        "GetStatus" => {
            let source = state.config.wallpaper_source();
            // Mode is remembered per source; return the current source's own mode
            let mode = state.config.source_mode(&source);
            let file = state
                .history
                .borrow()
                .current()
                .map(|e| e.file.clone())
                .unwrap_or_default();
            invocation.return_value(Some(&(source, mode, file).to_variant()));
        }
        "GetLastError" => {
            let msg = state.last_error.borrow().clone();
            // GDBus requires the return args to be a tuple type
            invocation.return_value(Some(&(msg,).to_variant()));
        }
        "FetchNow" => {
            let source = params.get::<(String,)>().map(|t| t.0).unwrap_or_default();
            if equinox_core::source::by_id(&source).is_none() {
                invocation.return_dbus_error(
                    "github.nwkyz.Equinox.Daemon1.Error.UnknownSource",
                    &format!("{}: {source}", gettext("Unknown source")),
                );
                return;
            }
            spawn_update(state, &source);
            invocation.return_value(None);
        }
        "GetSourceImages" => {
            let source = params.get::<(String,)>().map(|t| t.0).unwrap_or_default();
            let images: Vec<(String, String, String, i64)> = Storage::list(&source)
                .into_iter()
                .map(|img| {
                    (
                        img.path.to_string_lossy().into_owned(),
                        img.meta.title,
                        img.meta.source,
                        img.meta.downloaded_at,
                    )
                })
                .collect();
            invocation.return_value(Some(&(images,).to_variant()));
        }
        "SetWallpaperSource" => {
            let (source, mode) = params
                .get::<(String, String)>()
                .unwrap_or_default();
            if !source.is_empty() && equinox_core::source::by_id(&source).is_none() {
                invocation.return_dbus_error(
                    "github.nwkyz.Equinox.Daemon1.Error.UnknownSource",
                    &format!("{}: {source}", gettext("Unknown source")),
                );
                return;
            }
            scheduler::set_wallpaper_source(state, &source, &mode);
            invocation.return_value(None);
        }
        "ApplyWallpaper" => {
            spawn_apply(state, "apply wallpaper", scheduler::apply_rule);
            invocation.return_value(None);
        }
        "ApplyLatest" => {
            let source = params.get::<(String,)>().map(|t| t.0).unwrap_or_default();
            spawn_apply(state, "apply latest", move |s| scheduler::apply_latest(s, &source));
            invocation.return_value(None);
        }
        "ApplyRandom" => {
            let source = params.get::<(String,)>().map(|t| t.0).unwrap_or_default();
            spawn_apply(state, "apply random", move |s| scheduler::apply_random(s, &source));
            invocation.return_value(None);
        }
        "ApplyFile" => {
            let path = params.get::<(String,)>().map(|t| t.0).unwrap_or_default();
            scheduler::apply_file(state, &path);
            invocation.return_value(None);
        }
        "Next" | "ApplyNow" => {
            spawn_apply(state, "apply wallpaper", scheduler::apply_rule);
            invocation.return_value(None);
        }
        "Previous" => {
            scheduler::apply_previous(state);
            invocation.return_value(None);
        }
        "GetHistory" => {
            let h = state.history.borrow();
            let entries: Vec<(String, String, String, String, i64)> = h
                .entries()
                .iter()
                .map(|e| {
                    (
                        e.file.clone(),
                        e.source.clone(),
                        e.title.clone(),
                        e.copyright.clone().unwrap_or_default(),
                        e.applied_at,
                    )
                })
                .collect();
            invocation.return_value(Some(&(entries,).to_variant()));
        }
        "GetTasks" => {
            // (id, label, running, done, total) — progress from the log
            // entries; the GUI uses the id for the task's cancel button.
            // NB: the id MUST be an i64 — XML declares `a(xsbii)` (`x` = i64);
            // a `u64` here serializes as `t` (u64) and GIO rejects the reply
            // ("expected a(xsbii), got a(tsbii)"), so every GetTasks call
            // times out.
            let t = state.tasks.borrow();
            let log = state.task_log.borrow();
            let row_of = |id: u64, running: bool| {
                log.entries()
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| (id as i64, e.label.clone(), running, e.done, e.total))
            };
            let mut rows: Vec<(i64, String, bool, i32, i32)> = Vec::new();
            if let Some(id) = t.running_id {
                if let Some(r) = row_of(id, true) {
                    rows.push(r);
                }
            }
            for pending in &t.queue {
                if let Some(r) = row_of(pending.log_id, false) {
                    rows.push(r);
                }
            }
            invocation.return_value(Some(&(rows,).to_variant()));
        }
        "CancelTask" => {
            let id = params.get::<(i64,)>().map(|t| t.0).unwrap_or(-1);
            if id >= 0 {
                scheduler::cancel_task(state, id as u64);
            }
            invocation.return_value(None);
        }
        "GetSchedule" => {
            // Per-source update plan: (source, enabled, interval, last, next_due)
            let map = state.config.source_settings_map();
            let now = chrono::Local::now().timestamp();
            let rows: Vec<(String, bool, i64, i64, i64)> = equinox_core::source::registry()
                .iter()
                .map(|src| {
                    let id = src.id();
                    let st = scheduler::settings_of(&map, id);
                    let enabled = st.get_bool("update-enabled", false);
                    let interval =
                        equinox_core::config::clamp_interval(st.get_i64("update-interval", equinox_core::config::DEFAULT_UPDATE_INTERVAL));
                    let last = st.get_i64("last-updated-at", 0);
                    (id.to_owned(), enabled, interval, last, last + interval)
                })
                .collect();
            let _ = now;
            invocation.return_value(Some(&(rows,).to_variant()));
        }
        "GetTaskLog" => {
            // (start_ts, label, kind, catchup, done, ok, error, step, total, retries),
            // newest first
            let log = state.task_log.borrow();
            let rows: Vec<(i64, String, String, bool, bool, bool, String, i32, i32, i32)> = log
                .entries()
                .iter()
                .map(|e| {
                    (
                        e.start_ts,
                        e.label.clone(),
                        e.kind.clone(),
                        e.catchup,
                        e.end_ts != 0,
                        e.success,
                        e.error.clone(),
                        e.done,
                        e.total,
                        e.retries,
                    )
                })
                .collect();
            invocation.return_value(Some(&(rows,).to_variant()));
        }
        "HistoryRemove" => {
            let idx = params.get::<(i64,)>().map(|t| t.0).unwrap_or(-1);
            if idx >= 0 {
                let _ = state.history.borrow_mut().remove(idx as usize);
            }
            invocation.return_value(None);
        }
        "ClearHistory" => {
            while !state.history.borrow().is_empty() {
                if state.history.borrow_mut().remove(0).is_err() {
                    break;
                }
            }
            invocation.return_value(None);
        }
        _ => invocation.return_dbus_error(
            "github.nwkyz.Equinox.Daemon1.Error.UnknownMethod",
            "unknown method",
        ),
    }
}
