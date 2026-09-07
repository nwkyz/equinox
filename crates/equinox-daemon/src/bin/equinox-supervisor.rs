//! Equinox supervisor: keeps the wallpaper daemon running for the whole
//! login session.
//!
//! The daemon is disposable (re-reads state from disk on start), so the
//! supervisor's only job is to make sure *a* daemon is always there:
//!
//! - spawn the real daemon, then block on `waitpid`;
//! - when the daemon **exits cleanly** (exit 0, or after the supervisor
//!   forwards SIGTERM/SIGINT to it) the supervisor exits too — this is how
//!   "stop the background" works without systemd;
//! - when the daemon **crashes** (signal / non-zero exit) it is immediately
//!   restarted (systemd `Restart=on-failure` replacement).
//!
//! The supervisor itself handles SIGTERM/SIGINT by forwarding SIGTERM to the
//! daemon and waiting for it to exit. No polling: the main thread blocks on
//! the child, a signal thread only wakes on an actual signal.
//!
//! Installed as an XDG autostart entry by the GUI/installer; running it by
//! hand (or wrapping this binary in a systemd user service) works the same.

use std::io::Write;
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};

const DAEMON_BIN: &str = "equinox-daemon";
/// Backoff between restart attempts of a daemon that crashes instantly.
const RESTART_BACKOFF: std::time::Duration = std::time::Duration::from_millis(500);

fn main() {
    if let Err(e) = run() {
        eprintln!("equinox-supervisor: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    equinox_core::logging::init();

    // Detach from any controlling terminal / launcher process group, so the
    // supervisor keeps running when the GUI that started it closes, or when a
    // terminal is Ctrl-C'd / closed. Best-effort: already-detached launches
    // (systemd, XDG autostart) make setsid fail harmlessly.
    unsafe {
        libc::setsid();
        // A closed stderr pipe (e.g. the GUI that owned it quit) must not kill
        // the background with SIGPIPE on the next log line.
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    // The supervisor is a singleton per session: another instance would spawn
    // a second daemon racing for the D-Bus name. Use an advisory lock file;
    // a stale lock after a crash is released by the OS automatically.
    let _lock = LockFile::acquire()?;

    // Install handlers for our own termination: forward SIGTERM/SIGINT to the
    // daemon and let it shut down cleanly (the daemon's own handlers then
    // exit it with status 0, which we observe below).
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&stop))
        .expect("failed to install SIGTERM handler");
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&stop))
        .expect("failed to install SIGINT handler");

    // Signal thread: wakes only when a real signal arrives; polls the flag a
    // few times a second otherwise (negligible; no busy loop).
    let stop_thread = Arc::clone(&stop);
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if stop_thread.load(Ordering::Relaxed) {
            // Forward SIGTERM to the daemon; its own handler makes it exit 0,
            // which the main thread observes and then exits cleanly too.
            if let Some(pid) = current_child_pid() {
                let _ = unsafe { libc::kill(pid, libc::SIGTERM) };
            }
            break;
        }
    });

    // Supervise: spawn, wait, restart on crash, exit on clean shutdown.
    loop {
        let exit = run_daemon_once()?;
        if stop.load(Ordering::Relaxed) || exit.success() {
            // Clean shutdown (we asked it to stop, or it exited 0): done.
            break;
        }
        log::warn!("equinox-daemon exited abnormally ({exit:?}); restarting");
        std::thread::sleep(RESTART_BACKOFF);
    }
    Ok(())
}

/// The child pid currently being supervised (set by [`run_daemon_once`]).
static CURRENT_CHILD: AtomicI32 = AtomicI32::new(0);

fn current_child_pid() -> Option<i32> {
    let pid = CURRENT_CHILD.load(Ordering::Relaxed);
    (pid > 0).then_some(pid)
}

/// Spawn the daemon as a child and block until it exits, returning its status.
fn run_daemon_once() -> Result<std::process::ExitStatus> {
    let (program, args) = daemon_command();
    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn {program} {args:?}"))?;
    CURRENT_CHILD.store(child.id() as i32, Ordering::Relaxed);
    let status = child.wait()?;
    CURRENT_CHILD.store(0, Ordering::Relaxed);
    Ok(status)
}

/// Locate the daemon binary as `(program, args)`.
///
/// Prefer a sibling of the supervisor executable: native installs (same bin
/// dir) and flatpak (/app/bin) both lay the daemon next to the supervisor.
/// This must be checked BEFORE the FLATPAK_ID branch — inside a flatpak
/// sandbox there is no `flatpak` CLI to re-enter with, so a directly-resolved
/// /app/bin sibling keeps the supervisor↔daemon child relationship intact.
/// The `flatpak run` fallback only applies to host-side launches.
fn daemon_command() -> (String, Vec<String>) {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(DAEMON_BIN);
            if sibling.is_file() {
                return (sibling.to_string_lossy().into_owned(), Vec::new());
            }
        }
    }
    if let Ok(app) = std::env::var("FLATPAK_ID") {
        if !app.is_empty() {
            return (
                "flatpak".to_owned(),
                vec!["run".to_owned(), format!("--command={DAEMON_BIN}"), app],
            );
        }
    }
    (DAEMON_BIN.to_owned(), Vec::new())
}

/// Advisory lock so two supervisors don't run two daemons. The lock is held
/// by keeping `file` open (its flock is released when the file is closed).
struct LockFile {
    #[allow(dead_code)] // held open to keep the flock
    file: std::fs::File,
    path: std::path::PathBuf,
}

impl LockFile {
    fn acquire() -> Result<Self> {
        let dir = glib::user_runtime_dir().join("equinox");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
        let path = dir.join("supervisor.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            // Never truncate: the lock file's content is irrelevant, and
            // truncating while another supervisor holds the flock could
            // interfere with its view of the file.
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("failed to open lock file {}", path.display()))?;
        // Try to take an exclusive flock; fail when another instance holds it.
        let held = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 };
        if !held {
            return Err(anyhow!("another equinox-supervisor is already running"));
        }
        Ok(Self { file, path })
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = writeln!(std::io::stderr(), "equinox-supervisor stopped");
    }
}
