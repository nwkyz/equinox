//! D-Bus client wrapper: method calls, signal subscriptions, and daemon
//! online/offline monitoring.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use anyhow::{anyhow, Result};
use gettextrs::gettext;
use gio::prelude::*;
use glib::Variant;

use equinox_core::dbus::{BUS_NAME, INTERFACE, OBJECT_PATH};
use equinox_core::history::HistoryEntry;

type SignalFn = dyn Fn(&str, &str, &str);
type RunningFn = dyn Fn(bool);
type ErrorFn = dyn Fn(&str, &str);

/// Wallpaper status (source, mode, current file).
pub type WallpaperStatus = (String, String, String);

/// Gallery entry (file, title, source, downloaded timestamp).
pub type GalleryEntry = (String, String, String, i64);

/// Task list row: (id, label, is_running, done, total). `id` lets the GUI
/// cancel the task.
pub type TaskRow = (i64, String, bool, i32, i32);

/// Task-log row: (start_ts, label, kind, catchup, finished, ok, error, step_done, step_total, retries).
pub type TaskLogRow = (i64, String, String, bool, bool, bool, String, i32, i32, i32);

/// Schedule row: (source id, enabled, interval secs, last ts, next due ts).
pub type ScheduleRow = (String, bool, i64, i64, i64);

pub struct DaemonClient {
    proxy: RefCell<Option<gio::DBusProxy>>,
    /// Signal subscriptions must be held; dropping them unsubscribes
    /// (RemoveMatch) and daemon signals would stop arriving.
    subscriptions: RefCell<Vec<gio::SignalSubscription>>,
    running: Cell<bool>,
    /// Callbacks are multi-listener lists: several pages (banner/wallpaper/
    /// gallery/history) register their own; a single Option would let the
    /// last registered one overwrite the earlier ones.
    on_running: RefCell<Vec<Box<RunningFn>>>,
    on_wallpaper_changed: RefCell<Vec<Box<SignalFn>>>,
    on_fetch_failed: RefCell<Vec<Box<ErrorFn>>>,
    on_image_added: RefCell<Vec<Box<ErrorFn>>>,
    on_tasks_changed: RefCell<Vec<Box<dyn Fn()>>>,
    on_task_log_changed: RefCell<Vec<Box<dyn Fn()>>>,
}

impl DaemonClient {
    /// Create the client and start watching for the daemon appearing/vanishings.
    pub fn new() -> Rc<Self> {
        let this = Rc::new(Self {
            proxy: RefCell::new(None),
            subscriptions: RefCell::new(Vec::new()),
            running: Cell::new(false),
            on_running: RefCell::new(Vec::new()),
            on_wallpaper_changed: RefCell::new(Vec::new()),
            on_fetch_failed: RefCell::new(Vec::new()),
            on_image_added: RefCell::new(Vec::new()),
            on_tasks_changed: RefCell::new(Vec::new()),
            on_task_log_changed: RefCell::new(Vec::new()),
        });
        let weak = Rc::downgrade(&this);
        let weak_vanished = weak.clone();
        gio::bus_watch_name(
            gio::BusType::Session,
            BUS_NAME,
            gio::BusNameWatcherFlags::NONE,
            move |_conn, _name, _owner| {
                if let Some(c) = weak.upgrade() {
                    DaemonClient::on_name_appeared(&c);
                }
            },
            move |_conn, _name| {
                if let Some(c) = weak_vanished.upgrade() {
                    c.on_name_vanished();
                }
            },
        );
        this
    }

    fn on_name_appeared(this: &Rc<Self>) {
        let conn = match gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("failed to get session bus: {e}");
                return;
            }
        };
        let proxy = match gio::DBusProxy::new_sync(
            &conn,
            gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES,
            None::<&gio::DBusInterfaceInfo>,
            Some(BUS_NAME),
            OBJECT_PATH,
            INTERFACE,
            None::<&gio::Cancellable>,
        ) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("failed to create D-Bus proxy: {e}");
                return;
            }
        };
        let weak = Rc::downgrade(this);
        let sub = conn.subscribe_to_signal(
            None,
            Some(INTERFACE),
            None,
            Some(OBJECT_PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal: gio::DBusSignalRef<'_>| {
                if let Some(c) = weak.upgrade() {
                    c.on_signal(signal.signal_name, signal.parameters);
                }
            },
        );
        // The subscription must be stored; dropping it would RemoveMatch at
        // the end of the statement and signals would never arrive.
        this.subscriptions.borrow_mut().push(sub);
        *this.proxy.borrow_mut() = Some(proxy);
        this.running.set(true);
        log::info!("daemon online, D-Bus proxy and signal subscriptions established");
        this.fire_running();
    }

    fn on_name_vanished(&self) {
        log::info!("daemon offline");
        *self.proxy.borrow_mut() = None;
        self.running.set(false);
        self.fire_running();
    }

    fn on_signal(&self, member: &str, params: &Variant) {
        match member {
            "WallpaperChanged" => {
                if let Some((f, s, t)) = params.get::<(String, String, String)>() {
                    for cb in self.on_wallpaper_changed.borrow().iter() {
                        cb(&f, &s, &t);
                    }
                }
            }
            "FetchFailed" => {
                if let Some((s, e)) = params.get::<(String, String)>() {
                    for cb in self.on_fetch_failed.borrow().iter() {
                        cb(&s, &e);
                    }
                }
            }
            "ImageAdded" => {
                if let Some((s, p)) = params.get::<(String, String)>() {
                    for cb in self.on_image_added.borrow().iter() {
                        cb(&s, &p);
                    }
                }
            }
            "TasksChanged" => {
                for cb in self.on_tasks_changed.borrow().iter() {
                    cb();
                }
            }
            "TaskLogChanged" => {
                for cb in self.on_task_log_changed.borrow().iter() {
                    cb();
                }
            }
            _ => {}
        }
    }

    fn fire_running(&self) {
        let running = self.running.get();
        for cb in self.on_running.borrow().iter() {
            cb(running);
        }
    }

    pub fn running(&self) -> bool {
        self.running.get()
    }

    async fn call(&self, method: &str, params: Option<&Variant>) -> Result<Variant> {
        let proxy = match self.proxy.borrow().clone() {
            Some(p) => p,
            None => {
                log::warn!(
                    "call {method} failed: daemon not running (name watcher not ready or offline)"
                );
                return Err(anyhow!(gettext("Daemon is not running")));
            }
        };
        proxy
            .call_future(method, params, gio::DBusCallFlags::NONE, -1)
            .await
            .map_err(|e| {
                log::warn!("call {method} failed: {e}");
                anyhow!(e)
            })
    }

    /// (source, mode, current file).
    pub async fn status(&self) -> Result<WallpaperStatus> {
        let v = self.call("GetStatus", None).await?;
        Ok(v.get::<WallpaperStatus>().unwrap_or_default())
    }

    #[allow(dead_code)] // reserved for a future release
    pub async fn last_error(&self) -> Result<String> {
        let v = self.call("GetLastError", None).await?;
        Ok(v.get::<(String,)>().map(|t| t.0).unwrap_or_default())
    }

    /// Update the given source immediately (download and store).
    pub async fn fetch_now(&self, source: &str) -> Result<()> {
        self.call("FetchNow", Some(&(source,).to_variant()))
            .await
            .map(|_| ())
    }

    /// All images of a source (file, title, source, downloaded timestamp).
    ///
    /// Note: D-Bus method replies are always a tuple of the out arguments;
    /// with the single out arg `a(sssx)` the reply variant type is
    /// `(a(sssx))` — unwrap the outer tuple before taking the array.
    pub async fn source_images(&self, source: &str) -> Result<Vec<GalleryEntry>> {
        let v = self
            .call("GetSourceImages", Some(&(source,).to_variant()))
            .await?;
        Ok(v.get::<(Vec<GalleryEntry>,)>()
            .map(|t| t.0)
            .unwrap_or_default())
    }

    /// Set the wallpaper rule (source + mode); the daemon applies once
    /// immediately.
    pub async fn set_wallpaper_source(&self, source: &str, mode: &str) -> Result<()> {
        self.call("SetWallpaperSource", Some(&(source, mode).to_variant()))
            .await
            .map(|_| ())
    }

    #[allow(dead_code)] // reserved API (wallpaper page switches within a source)
    /// Apply once per the current rule.
    pub async fn apply_wallpaper(&self) -> Result<()> {
        self.call("ApplyWallpaper", None).await.map(|_| ())
    }

    #[allow(dead_code)] // reserved API (wallpaper page goes through the rule)
    pub async fn apply_latest(&self, source: &str) -> Result<()> {
        self.call("ApplyLatest", Some(&(source,).to_variant()))
            .await
            .map(|_| ())
    }

    #[allow(dead_code)] // reserved API (wallpaper page goes through the rule)
    pub async fn apply_random(&self, source: &str) -> Result<()> {
        self.call("ApplyRandom", Some(&(source,).to_variant()))
            .await
            .map(|_| ())
    }

    pub async fn apply_file(&self, path: &str) -> Result<()> {
        self.call("ApplyFile", Some(&(path,).to_variant()))
            .await
            .map(|_| ())
    }

    #[allow(dead_code)] // reserved API (wallpaper page switches within a source)
    /// Apply an earlier wallpaper from history.
    pub async fn previous(&self) -> Result<()> {
        self.call("Previous", None).await.map(|_| ())
    }

    #[allow(dead_code)] // reserved API (wallpaper page switches within a source)
    /// Apply once per the rule (same as history "next").
    pub async fn next(&self) -> Result<()> {
        self.call("Next", None).await.map(|_| ())
    }

    pub async fn history(&self) -> Result<Vec<HistoryEntry>> {
        let v = self.call("GetHistory", None).await?;
        // As above: the single out arg `a(ssssx)` comes back as `(a(ssssx))` —
        // unwrap the tuple first.
        let tuples = v
            .get::<(Vec<(String, String, String, String, i64)>,)>()
            .map(|t| t.0)
            .unwrap_or_default();
        Ok(tuples
            .into_iter()
            .map(
                |(file, source, title, copyright, applied_at)| HistoryEntry {
                    file,
                    source,
                    title,
                    copyright: (!copyright.is_empty()).then_some(copyright),
                    applied_at,
                },
            )
            .collect())
    }

    #[allow(dead_code)] // reserved for a future release
    pub async fn history_remove(&self, idx: usize) -> Result<()> {
        self.call("HistoryRemove", Some(&(idx as i64,).to_variant()))
            .await
            .map(|_| ())
    }

    pub async fn clear_history(&self) -> Result<()> {
        self.call("ClearHistory", None).await.map(|_| ())
    }

    /// The daemon's task list: running first, then queued (id, label,
    /// is_running, done, total).
    pub async fn tasks(&self) -> Result<Vec<TaskRow>> {
        let v = self.call("GetTasks", None).await?;
        Ok(v.get::<(Vec<TaskRow>,)>().map(|t| t.0).unwrap_or_default())
    }

    /// Cancel a queued/running task by its log id.
    pub async fn cancel_task(&self, id: i64) -> Result<()> {
        self.call("CancelTask", Some(&(id,).to_variant()))
            .await
            .map(|_| ())
    }

    /// Persistent task log, newest first.
    pub async fn task_log(&self) -> Result<Vec<TaskLogRow>> {
        let v = self.call("GetTaskLog", None).await?;
        Ok(v.get::<(Vec<TaskLogRow>,)>()
            .map(|t| t.0)
            .unwrap_or_default())
    }

    /// Per-source update plan (next due time etc.).
    pub async fn schedule(&self) -> Result<Vec<ScheduleRow>> {
        let v = self.call("GetSchedule", None).await?;
        Ok(v.get::<(Vec<ScheduleRow>,)>()
            .map(|t| t.0)
            .unwrap_or_default())
    }

    pub fn set_on_running_changed(&self, f: impl Fn(bool) + 'static) {
        self.on_running.borrow_mut().push(Box::new(f));
    }

    pub fn set_on_wallpaper_changed(&self, f: impl Fn(&str, &str, &str) + 'static) {
        self.on_wallpaper_changed.borrow_mut().push(Box::new(f));
    }

    pub fn set_on_fetch_failed(&self, f: impl Fn(&str, &str) + 'static) {
        self.on_fetch_failed.borrow_mut().push(Box::new(f));
    }

    /// A new image was added to a source (source, file path).
    pub fn set_on_image_added(&self, f: impl Fn(&str, &str) + 'static) {
        self.on_image_added.borrow_mut().push(Box::new(f));
    }

    /// The daemon task queue changed (task enqueued/started/finished).
    pub fn set_on_tasks_changed(&self, f: impl Fn() + 'static) {
        self.on_tasks_changed.borrow_mut().push(Box::new(f));
    }

    /// A task-log entry was created or finalized.
    pub fn set_on_task_log_changed(&self, f: impl Fn() + 'static) {
        self.on_task_log_changed.borrow_mut().push(Box::new(f));
    }
}
