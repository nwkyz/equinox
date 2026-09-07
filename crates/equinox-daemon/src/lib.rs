//! Equinox daemon library: the wallpaper background service shared by the
//! `equinox-daemon` binary (standalone / systemd-wrapped) and the
//! `equinox-supervisor` binary (which spawns and supervises the daemon).

pub mod scheduler;
pub mod service;
pub mod tasklog;

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use anyhow::{Context, Result};

use equinox_core::history::History;
use equinox_core::{config::Config, Applier};

pub use tasklog::{TaskLog, TaskLogEntry};

/// Async unit of work executed by the task queue.
pub type TaskRun =
    Box<dyn FnOnce(Rc<DaemonState>) -> Pin<Box<dyn Future<Output = Result<()>> + 'static>> + 'static>;

/// A queued or running task. `label` is the user-visible (translated) name
/// shown in the GUI task list; `source` routes FetchFailed signals to the
/// right source page (empty for non-source operations). `log_id` links the
/// task to its entry in the persistent task log.
pub struct PendingTask {
    pub label: String,
    pub source: String,
    pub log_id: u64,
    pub run: TaskRun,
}

/// Serial FIFO task queue: tasks never run in parallel and are never dropped
/// with "another task is running" — they wait their turn.
#[derive(Default)]
pub struct Tasks {
    pub queue: VecDeque<PendingTask>,
    /// Log id of the currently running task, if any (label/progress live in
    /// the task log).
    pub running_id: Option<u64>,
}

/// Daemon shared state. All fields are only accessed on the main thread
/// (glib main loop), so `Rc` is safe.
pub struct DaemonState {
    pub config: Config,
    /// PID of the short-lived `--fetch-once` helper currently downloading
    /// (see `scheduler`): the main daemon never creates a Soup/TLS stack —
    /// fetches run in a child that exits, so idle memory stays at the
    /// timer + D-Bus floor and only the child grows during an update.
    /// `cancel_task` kills this child to abort a running fetch immediately.
    pub fetch_child: RefCell<Option<i32>>,
    pub history: RefCell<History>,
    /// Session bus connection (set once the name is acquired), used to broadcast signals.
    pub conn: RefCell<Option<gio::DBusConnection>>,
    /// Most recent error message (shown by the GUI via GetLastError).
    pub last_error: RefCell<String>,
    /// Last successfully applied wallpaper file (dedup: never re-apply/broadcast
    /// the same file, preventing signal loops).
    pub last_applied: RefCell<Option<String>>,
    /// Serial task queue (see [`Tasks`]).
    pub tasks: RefCell<Tasks>,
    /// Task-log ids requested for cancellation: a still-queued task is removed
    /// outright, a running one aborts at its next checkpoint in
    /// [`crate::scheduler::update_source`]. Cleared when the task finishes.
    pub cancel: RefCell<HashSet<u64>>,
    /// Persistent log of every task run (GUI "update timeline").
    pub task_log: RefCell<TaskLog>,
    /// Monotonic id generator for task-log entries.
    pub next_task_id: Cell<u64>,
    /// D-Bus object registration handle (kept alive; dropping unregisters).
    pub registration: RefCell<Option<gio::RegistrationId>>,
}

impl DaemonState {
    pub fn new() -> Result<Rc<Self>> {
        let config = Config::new()?;
        let history = History::load().context("failed to load history")?;
        Ok(Rc::new(Self {
            config,
            fetch_child: RefCell::new(None),
            history: RefCell::new(history),
            conn: RefCell::new(None),
            last_error: RefCell::new(String::new()),
            last_applied: RefCell::new(None),
            tasks: RefCell::new(Tasks::default()),
            cancel: RefCell::new(HashSet::new()),
            task_log: RefCell::new(TaskLog::load()),
            next_task_id: Cell::new(1),
            registration: RefCell::new(None),
        }))
    }
}

/// Assemble the daemon (state, wallpaper backend, D-Bus, scheduler). The
/// glib main loop is run by the caller (the `equinox-daemon` binary or the
/// supervisor's child), so `block_on` is never nested inside it.
pub fn daemon_setup() -> Result<Rc<DaemonState>> {
    let state = DaemonState::new()?;
    log::info!("Equinox daemon starting");
    log::info!(
        "active wallpaper backend: {}",
        Applier::active_backend().unwrap_or("none")
    );

    if !Applier::available() {
        log::warn!("no usable wallpaper backend detected (GNOME/KDE/XFCE); daemon still runs (manages downloads and D-Bus)");
    }

    service::setup_bus(&state);
    scheduler::start(&state);
    Ok(state)
}
