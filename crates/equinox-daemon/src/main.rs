//! Equinox daemon binary entry point.
//!
//! Runs the wallpaper background service (scheduler + D-Bus server). It is
//! normally spawned and supervised by `equinox-supervisor`, or wrapped in a
//! systemd user service — either way it is **disposable**: on start it re-reads
//! config/history/task-log from disk and catches up missed updates, so killing
//! or restarting it loses nothing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context as _};
use chrono::NaiveDate;
use equinox_core::config::Config;
use equinox_core::http::{HttpClient, USER_AGENT};
use equinox_core::source::by_id;
use equinox_core::storage::Storage;
use equinox_daemon::daemon_setup;

/// `equinox-daemon --fetch-once <source> <index> <date>`: fetch and store ONE
/// day in a short-lived child, print `STORED\t<path>\t<reused>` on stdout and
/// exit. The scheduler runs fetches through this child so the long-lived
/// daemon never loads the Soup/TLS/network-module stack — idle memory stays
/// at the timer + D-Bus floor, and only the (transient) child grows while an
/// update is running.
fn run_fetch_once() -> glib::ExitCode {
    let mut args = std::env::args().skip(2); // [bin, --fetch-once]
    let Some(source_id) = args.next() else {
        eprintln!("usage: equinox-daemon --fetch-once <source> <index> <yyyy-mm-dd>");
        return glib::ExitCode::FAILURE;
    };
    let Some(index) = args.next().and_then(|s| s.parse::<i64>().ok()) else {
        eprintln!("fetch-once: invalid index");
        return glib::ExitCode::FAILURE;
    };
    let Some(date) = args
        .next()
        .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
    else {
        eprintln!("fetch-once: invalid date (expected yyyy-mm-dd)");
        return glib::ExitCode::FAILURE;
    };

    let result = glib::MainContext::default().block_on(async {
        let config = Config::new().context("config")?;
        let source = by_id(&source_id)
            .ok_or_else(|| anyhow!("unknown source: {source_id}"))?;
        let mut day = config.settings_for(&source_id);
        day.set("index", serde_json::json!(index));
        let http = HttpClient::new(USER_AGENT);
        let fetched = source.fetch(&day, &http, date).await?;
        let stored = Storage::store(&http, &source_id, &fetched).await?;
        println!(
            "STORED\t{}\t{}",
            stored.path.display(),
            if stored.reused { 1 } else { 0 }
        );
        Ok::<(), anyhow::Error>(())
    });
    match result {
        Ok(()) => glib::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("FETCHERR\t{e:#}");
            glib::ExitCode::FAILURE
        }
    }
}

fn main() -> glib::ExitCode {
    equinox_core::init_i18n();
    equinox_core::logging::init();
    // A closed stderr (supervisor/terminal gone) must not SIGPIPE-kill the
    // daemon on its next log line.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    // One-shot fetch mode (spawned by the scheduler): no main loop, no D-Bus,
    // no signal handlers → default SIGTERM dies immediately on cancel.
    if std::env::args().nth(1).as_deref() == Some("--fetch-once") {
        return run_fetch_once();
    }
    if let Err(e) = daemon_setup() {
        eprintln!("equinox-daemon failed to start: {e:#}");
        return glib::ExitCode::FAILURE;
    }
    // Main loop: D-Bus callbacks and async tasks are driven here.
    // Note: never nest block_on inside the main loop — glib futures cannot be
    // polled from within another future.
    let main_loop = glib::MainLoop::new(None, false);

    // Graceful shutdown on SIGTERM/SIGINT (supervisor stop, systemd stop,
    // Ctrl-C): a signal-hook flag is polled at a low frequency on the main
    // loop, so in-flight D-Bus replies and the last config write get to
    // finish instead of the process being killed mid-write.
    let term = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term))
        .expect("failed to install SIGTERM handler");
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&term))
        .expect("failed to install SIGINT handler");
    {
        let loop_quit = main_loop.clone();
        let flag = Arc::clone(&term);
        // A tick every 200 ms is plenty for a shutdown request and costs
        // essentially nothing for an idle daemon.
        glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
            if flag.load(Ordering::Relaxed) {
                log::info!("shutdown signal received, quitting main loop");
                loop_quit.quit();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    main_loop.run();
    glib::ExitCode::SUCCESS
}
