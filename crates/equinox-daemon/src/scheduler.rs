//! Per-source scheduling (interval-based with catch-up), a serial task queue,
//! and wallpaper application rules (latest/random/manual of the selected source).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;

use anyhow::{anyhow, Context, Result};
use chrono::Local;
use gettextrs::gettext;
use gio::prelude::*;

use equinox_core::config::{clamp_interval, DEFAULT_UPDATE_INTERVAL};
use equinox_core::dbus::{INTERFACE, OBJECT_PATH};
use equinox_core::source::{by_id, registry, SourceSettings};
use equinox_core::storage::{ImageMeta, Storage, StoredImage};
use equinox_core::Applier;

use crate::{DaemonState, PendingTask, TaskLogEntry, TaskRun};

/// Grace period after daemon start before catch-up runs fire: at boot the
/// network is often not up yet (Wi-Fi association, DHCP), and a failed
/// catch-up fetch counts as attempted, so it would wait a full interval
/// before retrying. Waiting this out once per start is cheaper than losing
/// that interval.
const CATCHUP_DELAY_SECS: i64 = 30;

/// Start the timer: every tick, fire each enabled source whose refresh
/// interval has elapsed since its last update.
///
/// The "last updated" timestamp is persisted per source, so scheduling
/// survives restarts and **missed windows self-heal**: if the machine was
/// off/asleep past one or more intervals (glib timers freeze during suspend),
/// the comparison uses wall-clock elapsed time and the first tick after
/// wake/boot catches up — no special resume handling needed. At boot that
/// first tick races network setup, so catch-up runs additionally wait
/// [`CATCHUP_DELAY_SECS`] after daemon start; a failed fetch is stamped as
/// attempted and would otherwise not retry until a full interval passed.
pub fn start(state: &Rc<DaemonState>) {
    report_missed_windows(state);
    let state = Rc::clone(state);
    let started_at = Local::now().timestamp();
    glib::timeout_add_local(std::time::Duration::from_secs(15), move || {
        let now = Local::now().timestamp();
        // One disk read per tick for ALL sources (the config file is re-read
        // on every access; reading it once here keeps the idle cost at
        // 1 read / 15 s instead of 3 × sources).
        let settings_map = state.config.source_settings_map();
        for source in registry() {
            let id = source.id();
            let s = settings_of(&settings_map, id);
            if !s.get_bool("update-enabled", false) {
                continue;
            }
            let interval = clamp_interval(s.get_i64("update-interval", DEFAULT_UPDATE_INTERVAL));
            let last = s.get_i64("last-updated-at", 0);
            if now - last >= interval {
                // Catch-up detection: either the very first run of this
                // source (never updated) or the elapsed time is at least a
                // full interval beyond due — which means the machine was off
                // / asleep past one or more windows and this run catches up.
                let catchup = last == 0 || now - last >= interval * 2;
                if catchup && now - started_at < CATCHUP_DELAY_SECS {
                    // Just started: hold catch-up runs so the network has
                    // time to come up (a failed fetch would be stamped as
                    // attempted and not retried until a full interval).
                    continue;
                }
                if catchup {
                    log::info!("scheduled update for source {id} is a catch-up run");
                }
                log::info!("scheduled update due for source {id} (interval {interval}s)");
                spawn_update_kind(&state, id, "scheduled", catchup);
            }
        }
        glib::ControlFlow::Continue
    });
}

/// One-time scan at daemon startup: any enabled source whose interval has
/// already elapsed means at least one update window passed while the machine
/// was off/asleep. Record a gray "skipped" entry per such source so the
/// timeline shows what happened, then the normal tick catches the source up.
fn report_missed_windows(state: &Rc<DaemonState>) {
    let map = state.config.source_settings_map();
    let now = Local::now().timestamp();
    for source in registry() {
        let id = source.id();
        let s = settings_of(&map, id);
        if !s.get_bool("update-enabled", false) {
            continue;
        }
        let interval = clamp_interval(s.get_i64("update-interval", DEFAULT_UPDATE_INTERVAL));
        let last = s.get_i64("last-updated-at", 0);
        if last > 0 && now - last > interval {
            let id_gen = next_log_id(state);
            state.task_log.borrow_mut().push(TaskLogEntry {
                id: id_gen,
                start_ts: now,
                end_ts: now,
                label: format!("{} · {}", gettext("Missed update window (powered off)"), id),
                kind: "skipped".to_owned(),
                source: id.to_owned(),
                catchup: true,
                success: false,
                error: String::new(),
                done: 0,
                total: 0,
                retries: 0,
            });
            log::info!("recorded missed update window for source {id}");
        }
    }
}

/// Extract one source's settings from an already-loaded top-level map.
/// Extract one source's settings from the map returned by
/// [`equinox_core::config::Config::source_settings_map`] — that map is ALREADY
/// the inner `source-settings` object (`{ "bing": {...}, ... }`); wrapping it
/// again silently yielded empty settings everywhere (the "schedule shows off /
/// never fires" bug).
pub fn settings_of(map: &serde_json::Map<String, serde_json::Value>, id: &str) -> SourceSettings {
    SourceSettings(
        map.get(id)
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default(),
    )
}

/// Allocate a fresh task-log id (callers pass it to [`enqueue`] AND keep it
/// for progress reporting — allocating twice would orphan the progress).
pub fn next_log_id(state: &Rc<DaemonState>) -> u64 {
    let id = state.next_task_id.get();
    state.next_task_id.set(id + 1);
    id
}

/// Queue an async unit of work; runs serially (FIFO), never dropped.
/// Records the task in the persistent log and broadcasts both signals.
pub fn enqueue(state: &Rc<DaemonState>, id: u64, label: String, source: &str, kind: &str, catchup: bool, run: TaskRun) {
    state.task_log.borrow_mut().push(TaskLogEntry {
        id,
        start_ts: Local::now().timestamp(),
        end_ts: 0,
        label: label.clone(),
        kind: kind.to_owned(),
        source: source.to_owned(),
        catchup,
        success: false,
        error: String::new(),
        done: 0,
        total: 0,
        retries: 0,
    });
    state.tasks.borrow_mut().queue.push_back(PendingTask {
        label,
        source: source.to_owned(),
        log_id: id,
        run,
    });
    state.emit_tasks_changed();
    state.emit_task_log_changed();
    pump(state);
}

/// Cancel a task by its log id: a still-queued task is removed outright (its
/// log entry is finalized as cancelled), a running task is marked so its body
/// aborts at the next checkpoint in `update_source`.
pub fn cancel_task(state: &Rc<DaemonState>, log_id: u64) {
    let (queued, running) = {
        let mut t = state.tasks.borrow_mut();
        let before = t.queue.len();
        t.queue.retain(|task| task.log_id != log_id);
        (t.queue.len() < before, t.running_id == Some(log_id))
    };
    if !queued && !running {
        return; // not a live task (already finished)
    }
    if queued {
        state.task_log.borrow_mut().finish(
            log_id,
            false,
            &gettext("Update cancelled"),
            Local::now().timestamp(),
        );
    }
    if running {
        state.cancel.borrow_mut().insert(log_id);
        // Abort the running fetch immediately: kill the short-lived helper
        // child (the daemon itself holds no live network request — the
        // Soup/TLS stack only ever exists inside the fetch-once child).
        if let Some(pid) = state.fetch_child.borrow().as_ref() {
            unsafe {
                libc::kill(*pid, libc::SIGTERM);
            }
        }
    }
    log::info!("cancelled task {log_id} (queued: {queued}, running: {running})");
    state.emit_tasks_changed();
    state.emit_task_log_changed();
    if queued {
        pump(state); // a slot opened: start the next task if idle
    }
}

/// Whether a running task with `log_id` was marked for cancellation.
fn is_cancelled(state: &Rc<DaemonState>, log_id: u64) -> bool {
    state.cancel.borrow().contains(&log_id)
}

/// Start the next queued task when none is running.
fn pump(state: &Rc<DaemonState>) {
    let next = {
        let mut t = state.tasks.borrow_mut();
        if t.running_id.is_some() {
            return;
        }
        t.queue.pop_front()
    };
    let Some(task) = next else { return };
    state.tasks.borrow_mut().running_id = Some(task.log_id);
    state.emit_tasks_changed();
    let st = Rc::clone(state);
    glib::spawn_future_local(async move {
        let result = (task.run)(Rc::clone(&st)).await;
        finish_task(&st, &task.label, &task.source, task.log_id, result);
    });
}

fn finish_task(
    state: &Rc<DaemonState>,
    label: &str,
    source: &str,
    log_id: u64,
    result: Result<()>,
) {
    state.tasks.borrow_mut().running_id = None;
    state.cancel.borrow_mut().remove(&log_id);
    state.emit_tasks_changed();
    match &result {
        Ok(()) => state
            .task_log
            .borrow_mut()
            .finish(log_id, true, "", Local::now().timestamp()),
        Err(e) => state.task_log.borrow_mut().finish(
            log_id,
            false,
            &format!("{e:#}"),
            Local::now().timestamp(),
        ),
    }
    state.emit_task_log_changed();
    report_result(state, label, source, result);
    pump(state); // chain into the next queued task
}

fn report_result(state: &Rc<DaemonState>, label: &str, source: &str, result: Result<()>) {
    if let Err(e) = result {
        log::warn!("{label} failed: {e:#}");
        *state.last_error.borrow_mut() = format!("{e:#}");
        state.emit_fetch_failed(source, &format!("{e:#}"));
    }
}

/// Kick off an async "update source" task: fetch, store; if the source is the
/// current wallpaper source, apply per the rule. The attempt timestamp is
/// stamped immediately so repeated ticks cannot enqueue duplicates while the
/// request sits in the queue.
pub fn spawn_update(state: &Rc<DaemonState>, source: &str) {
    spawn_update_kind(state, source, "manual", false);
}

/// Like [`spawn_update`], with explicit log metadata: `kind` is
/// "scheduled" | "manual"; `catchup` marks runs that make up for windows
/// missed while the machine was off/asleep (or first-ever runs).
///
/// Scheduled runs retry transient failures (e.g. the network is still coming
/// up): after a failed fetch it waits [`RETRY_DELAY_SECS`] and tries again,
/// up to [`MAX_RETRIES`] times. Manual "Update now" never retries — the user
/// is waiting on the result and should see the error immediately. Retries
/// stay on the SAME task-log entry (same timeline row, same error line).
pub fn spawn_update_kind(state: &Rc<DaemonState>, source: &str, kind: &str, catchup: bool) {
    state
        .config
        .set_last_updated_at(source, Local::now().timestamp());
    let display = by_id(source)
        .map(|s| s.display_name())
        .unwrap_or_else(|| source.to_owned());
    let label = format!("{} {display}", gettext("Updating"));
    let src = source.to_owned();
    let kind = kind.to_owned();
    // Pre-generate the log id so the task body can report step progress.
    let log_id = next_log_id(state);
    let kind_for_run = kind.clone();
    enqueue(state, log_id, label, source, &kind, catchup, Box::new(move |s| {
        Box::pin(async move {
            if kind_for_run != "scheduled" {
                return update_source(&s, &src, log_id).await;
            }
            // First attempt, then up to MAX_RETRIES after RETRY_DELAY_SECS.
            let mut last_err = match update_source(&s, &src, log_id).await {
                Ok(()) => return Ok(()),
                Err(e) => e,
            };
            for _ in 0..MAX_RETRIES {
                // Cancel kills the wait immediately (the 30 s retry delay is
                // the long part of a failing scheduled update).
                if is_cancelled(&s, log_id) {
                    return Err(anyhow!(gettext("Update cancelled")));
                }
                glib::timeout_future(std::time::Duration::from_secs(RETRY_DELAY_SECS)).await;
                s.task_log.borrow_mut().retried(log_id);
                s.emit_task_log_changed();
                log::info!("retrying scheduled update for source {src}");
                match update_source(&s, &src, log_id).await {
                    Ok(()) => return Ok(()),
                    Err(e) => last_err = e,
                }
            }
            Err(last_err)
        })
    }));
}

/// Wait between scheduled-update retries.
const RETRY_DELAY_SECS: u64 = 30;
/// Maximum number of automatic retries for a failed scheduled update.
const MAX_RETRIES: u32 = 3;

/// Run a synchronous apply/wallpaper-setting operation IMMEDIATELY, outside
/// the serial FIFO queue.
///
/// Apply operations are short writes (a gsettings call, a config-file edit, a
/// `Storage::latest` lookup) that must never be blocked behind long network
/// updates: switching sources / applying wallpaper has to respond instantly
/// even when a big update is running (user request). The daemon main loop is
/// free while an async update is awaiting, so running them inline here is safe
/// and preserves single-threaded behaviour (no overlapping work). The finished
/// result is still recorded in the persistent task log (timeline).
pub fn spawn_apply(
    state: &Rc<DaemonState>,
    label: &str,
    f: impl FnOnce(&Rc<DaemonState>) -> Result<()> + 'static,
) {
    let id = next_log_id(state);
    let label = label.to_owned();
    let result = f(state);
    let now = Local::now().timestamp();
    let (success, error) = match &result {
        Ok(()) => (true, String::new()),
        Err(e) => (false, format!("{e:#}")),
    };
    state.task_log.borrow_mut().push(TaskLogEntry {
        id,
        start_ts: now,
        end_ts: now,
        label: label.clone(),
        kind: "apply".to_owned(),
        source: String::new(),
        catchup: false,
        success,
        error,
        done: 0,
        total: 0,
        retries: 0,
    });
    state.emit_task_log_changed();
    report_result(state, &label, "", result);
}

/// Fetch and store ONE day through a short-lived `equinox-daemon
/// --fetch-once` child, so the long-lived daemon never loads the
/// Soup/TLS/network-module stack (idle memory stays at the timer + D-Bus
/// floor; only the transient child grows while an update runs — it exits
/// and its pages are reclaimed).
///
/// Returns the stored image (path + reuse flag) parsed from the child's
/// `STORED\t<path>\t<reuse>` stdout line. Cancellation surface: the child is
/// SIGTERM'd by `cancel_task` and its missing result is turned into a
/// "cancelled" error by the caller's `is_cancelled` check.
async fn fetch_and_store(
    state: &Rc<DaemonState>,
    source_id: &str,
    idx: i64,
    day_date: chrono::NaiveDate,
) -> Result<StoredImage> {
    use futures_channel::oneshot;
    let me = std::env::current_exe().context("current_exe")?;
    let child = Command::new(&me)
        .arg("--fetch-once")
        .arg(source_id)
        .arg(idx.to_string())
        .arg(day_date.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn fetch helper for {source_id}"))?;
    *state.fetch_child.borrow_mut() = Some(child.id() as i32);
    // wait_with_output would block the main loop (freezing D-Bus/cancel);
    // collect the child's output on a background thread and bridge it back.
    let (tx, rx) = oneshot::channel::<std::io::Result<std::process::Output>>();
    let waiter = std::thread::spawn(move || {
        let out = child.wait_with_output();
        let _ = tx.send(out);
    });
    let output = rx.await.map_err(|_| anyhow!("fetch helper interrupted"))?;
    let _ = waiter.join();
    *state.fetch_child.borrow_mut() = None;
    let output = output.map_err(|e| anyhow!("fetch helper I/O: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(line) = stdout.lines().find(|l| l.starts_with("STORED\t")) {
        let mut p = line.splitn(4, '\t');
        p.next();
        let path = p.next().unwrap_or_default();
        let reused = p.next().unwrap_or_default();
        if !path.is_empty() {
            let pathb = PathBuf::from(path);
            return Ok(StoredImage {
                meta: ImageMeta {
                    source: source_id.to_owned(),
                    title: String::new(),
                    copyright: None,
                    url: String::new(),
                    downloaded_at: 0,
                    hash: String::new(),
                    date: String::new(),
                    width: 0,
                    height: 0,
                    extra: Default::default(),
                },
                path: pathb.clone(),
                json_path: pathb.with_extension("json"),
                reused: reused == "1",
            });
        }
    }
    // Failed: surface the child's error line; a cancelled helper (SIGTERM)
    // yields "interrupted"/empty, which the caller's is_cancelled maps to a
    // clean "Update cancelled".
    let stderr = String::from_utf8_lossy(&output.stderr);
    let msg = stderr
        .lines()
        .find(|l| l.starts_with("FETCHERR\t"))
        .map(|l| l.trim_start_matches("FETCHERR\t").to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            format!(
                "fetch helper exited with status {}",
                output
                    .status
                    .code()
                    .map_or_else(|| "signal".to_string(), |c| c.to_string())
            )
        });
    Err(anyhow!("{msg}"))
}

/// Fetch and store a new image; if the source is the current wallpaper
/// source, apply per the mode (latest/random/manual).
/// History backfill: when the source settings carry `index` (days, e.g.
/// Bing's "history offset"), fetch one image for **each** day from that day
/// to today, rather than only back to that day.
async fn update_source(state: &Rc<DaemonState>, source_id: &str, log_id: u64) -> Result<()> {
    let source = by_id(source_id)
        .ok_or_else(|| anyhow!(format!("{}: {source_id}", gettext("Unknown source"))))?;
    let settings = state.config.settings_for(source_id);
    let date = Local::now().date_naive();
    let backfill = settings.get_i64("index", 0).clamp(0, 7);
    let total = backfill + 1;
    log::info!("updating source {source_id}: history backfill index={backfill}");
    // Fetches run in a short-lived `--fetch-once` child (fetch_and_store):
    // the daemon itself never loads the Soup/TLS/network-module stack, so
    // idle memory stays at the timer + D-Bus floor and only the (transient)
    // child grows during an update.
    for i in 0..=backfill {
        // Cancellation checkpoint: a still-queued task is removed outright by
        // cancel_task; a running one reaches this check between backfill days.
        if is_cancelled(state, log_id) {
            return Err(anyhow!(gettext("Update cancelled")));
        }
        // Bandwidth guard: a day that is already on disk (filename carries
        // the image's own date) is skipped entirely — no API call, no image
        // download. Applies to backfill days AND today's re-run.
        let day_date = date - chrono::Duration::days(i);
        if source.daily() && Storage::has_dated(source_id, day_date) {
            log::info!("skipping {source_id} idx={i}: {day_date} already stored");
            state.set_task_progress(log_id, (i + 1) as i32, total as i32);
            continue;
        }
        // Fetch day by day: idx = days ago (0 = today); Bing archive covers
        // at most 7 days.
        log::info!("fetching {source_id} idx={i} (fetch helper)");
        // The child is SIGTERM'd by cancel_task; its failure is mapped to a
        // clean "cancelled" below via the cancel flag.
        let stored = fetch_and_store(state, source_id, i, day_date).await?;
        if is_cancelled(state, log_id) {
            // Aborted (mid-download or after): do NOT store/apply anything.
            return Err(anyhow!(gettext("Update cancelled")));
        }
        if stored.reused {
            log::info!(
                "fetched image already stored, nothing added: {} ({source_id})",
                stored.path.display()
            );
        } else {
            log::info!("stored image: {} ({source_id})", stored.path.display());
            state.emit_image_added(source_id, &stored.path.to_string_lossy());
        }
        state.set_task_progress(log_id, (i + 1) as i32, total as i32);
    }

    // Enforce the per-source image cap (delete oldest beyond it).
    let max_images = settings.get_i64("max-images", equinox_core::config::DEFAULT_MAX_IMAGES);
    if max_images > 0 {
        let removed = Storage::trim(source_id, max_images as usize);
        if removed > 0 {
            log::info!("trimmed {removed} old images from {source_id} (cap {max_images})");
        }
    }

    if state.config.wallpaper_source() == source_id {
        // Apply the freshly downloaded image per the source's mode. Manual
        // mode keeps the user's manual pick (no auto-apply on update).
        match state.config.source_mode(source_id).as_str() {
            "manual" => {}
            "random" => apply_random(state, source_id)?,
            _ => apply_latest(state, source_id)?,
        }
    }
    Ok(())
}

/// Apply the latest image of a source.
pub fn apply_latest(state: &Rc<DaemonState>, source: &str) -> Result<()> {
    let Some(img) = Storage::latest(source) else {
        return Err(anyhow!(format!(
            "{} ({source})",
            gettext("This source has no images yet, run \"Update now\" first")
        )));
    };
    apply_image(state, &img.path)
}

/// Apply a random image of a source.
pub fn apply_random(state: &Rc<DaemonState>, source: &str) -> Result<()> {
    let Some(img) = Storage::random(source) else {
        return Err(anyhow!(format!(
            "{} ({source})",
            gettext("This source has no images yet, run \"Update now\" first")
        )));
    };
    apply_image(state, &img.path)
}

/// Apply once per the current rule (wallpaper source + mode).
pub fn apply_rule(state: &Rc<DaemonState>) -> Result<()> {
    let source = state.config.wallpaper_source();
    if source.is_empty() {
        return Err(anyhow!(gettext(
            "No wallpaper source selected yet, choose one on the Wallpaper page"
        )));
    }
    match state.config.source_mode(&source).as_str() {
        "random" => apply_random(state, &source),
        // Manual mode: don't auto-apply; keep the current wallpaper.
        "manual" => Ok(()),
        _ => apply_latest(state, &source),
    }
}

/// Set the wallpaper source and mode (rule) and, when the source HAS images,
/// apply once immediately. Mode is remembered per source: written to the
/// source's own settings and restored when switching back.
///
/// Switching to a source **without** images performs NO fetch: the wallpaper
/// page shows a "no wallpapers yet" state with an explicit Update button.
/// Auto-fetching here made switches feel blocked whenever a source's network
/// was down, and left the page blank meanwhile.
pub fn set_wallpaper_source(state: &Rc<DaemonState>, source: &str, mode: &str) {
    if source.is_empty() {
        // "None" wallpaper source: Equinox stops managing the wallpaper and
        // restores the user's original wallpaper (the one Equinox first
        // replaced, captured in apply_image). Nothing is fetched or applied.
        state.config.set_wallpaper_source("");
        state.emit_wallpaper_changed("", "", "");
        if let Err(e) = restore_original(state) {
            log::warn!("failed to restore original wallpaper: {e:#}");
        }
        return;
    }
    state.config.set_wallpaper_source(source);
    state.config.set_source_mode(source, mode);
    // Tell the GUI to refresh the wallpaper page right away with a sentinel
    // broadcast: there is NO other signal that reliably fires on a source
    // change — switching to an empty source does not emit ImageAdded, and an
    // apply failure would otherwise leave the page stale (e.g. first-run
    // OOBE sets the source after downloads already landed).
    state.emit_wallpaper_changed("", source, "");
    if Storage::latest(source).is_none() {
        // Remember the rule only; the GUI's empty state offers the update.
        // Once images land via that button or a scheduled run, update_source
        // applies them because this source is the wallpaper source.
        log::info!("wallpaper source {source} has no images yet; showing empty state (no auto-fetch)");
        return;
    }
    let source = source.to_owned();
    let mode = mode.to_owned();
    spawn_apply(state, &gettext("Setting wallpaper"), move |s| {
        if mode == "manual" {
            match latest_history_file(s, &source) {
                Some(f) if Path::new(&f).exists() => apply_image(s, Path::new(&f)),
                _ => apply_latest(s, &source),
            }
        } else {
            apply_rule(s)
        }
    });
}

/// Apply the captured original wallpaper back — the "None" source keeps the
/// desktop as the user left it; Equinox no longer manages the wallpaper.
/// The original is applied directly (no history entry, no source switch).
fn restore_original(state: &Rc<DaemonState>) -> Result<()> {
    let orig = state.config.original_wallpaper();
    if orig.is_empty() {
        return Ok(()); // nothing was captured, nothing to restore
    }
    let orig_for_log = orig.clone();
    Applier::apply(Path::new(&orig))
        .with_context(|| format!("failed to restore original wallpaper: {orig_for_log}"))?;
    log::info!("restored original wallpaper: {orig_for_log}");
    *state.last_applied.borrow_mut() = Some(orig);
    state.emit_wallpaper_changed("", "", "");
    Ok(())
}

/// The newest history entry of a source — the manual pick after browsing.
fn latest_history_file(state: &Rc<DaemonState>, source: &str) -> Option<String> {
    state
        .history
        .borrow()
        .entries()
        .iter()
        .find(|e| e.source == source)
        .map(|e| e.file.clone())
}

/// Apply a specific image file (gallery/history click), record history.
/// History::push already dedups by path and moves the entry to the front.
///
/// Browsing to a concrete image is a **manual pick**: when the file belongs
/// to the current wallpaper source, that source's mode is switched to
/// "manual", so scheduled updates won't jump away from the picked wallpaper
/// and switching sources later restores this pick.
pub fn apply_file(state: &Rc<DaemonState>, path: &str) {
    let path = path.to_owned();
    spawn_apply(state, &gettext("Applying image"), move |s| {
        apply_image(s, Path::new(&path))?;
        // Browsing to a concrete image switches the rule to that source in
        // manual mode — the GUI wallpaper page follows via WallpaperChanged.
        let src = fs::read_to_string(Path::new(&path).with_extension("json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<ImageMeta>(&raw).ok())
            .map(|m| m.source)
            .unwrap_or_default();
        if !src.is_empty() {
            s.config.set_wallpaper_source(&src);
            s.config.set_source_mode(&src, "manual");
        }
        Ok(())
    });
}

/// "Previous": re-apply the entry before the current wallpaper in history.
pub fn apply_previous(state: &Rc<DaemonState>) {
    spawn_apply(state, &gettext("Applying previous"), |s| {
        // Take the target file out first and drop the borrow, to avoid
        // conflicting with the borrow inside apply_image.
        let file = {
            let mut h = s.history.borrow_mut();
            if h.entries().len() < 2 {
                return Err(anyhow!(gettext("No earlier wallpaper in history")));
            }
            h.move_to_front(1)?;
            h.current()
                .expect("there must be a current entry after move_to_front")
                .file
                .clone()
        };
        apply_image(s, Path::new(&file))
    });
}

/// Apply an image + record history + broadcast a signal.
fn apply_image(state: &Rc<DaemonState>, path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(anyhow!(format!(
            "{}: {}",
            gettext("File does not exist"),
            path.display()
        )));
    }
    // Loop guard: skip when the same wallpaper as the last apply (no re-write
    // of settings, no WallpaperChanged broadcast — otherwise the GUI history
    // page refresh loop would spin into endlessly re-applying). "Previous"
    // applies a different file, so it is unaffected.
    let same_as_last = state
        .last_applied
        .borrow()
        .as_deref()
        .is_some_and(|last| last == path.to_str().unwrap_or_default());
    if same_as_last {
        log::info!("wallpaper unchanged, skipping duplicate apply: {}", path.display());
        return Ok(());
    }
    // Capture the user's original wallpaper ONCE, right before Equinox first
    // overwrites it, so the "None" source can later restore it. The original
    // stays remembered forever; "None" always restores it.
    if state.config.original_wallpaper().is_empty() {
        match Applier::current() {
            Some(orig) => {
                state.config.set_original_wallpaper(&orig);
                log::info!("captured original wallpaper: {orig}");
            }
            None => log::warn!(
                "could not read the current wallpaper; the \"None\" source cannot restore it"
            ),
        }
    }
    Applier::apply(path)
        .with_context(|| format!("{}: {}", gettext("Failed to apply wallpaper"), path.display()))?;

    let meta: Option<ImageMeta> = fs::read_to_string(path.with_extension("json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok());
    let source = meta.as_ref().map(|m| m.source.clone()).unwrap_or_default();
    let title = meta
        .as_ref()
        .map(|m| m.title.clone())
        .unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    let copyright = meta.and_then(|m| m.copyright);

    let entry = equinox_core::history::HistoryEntry {
        file: path.to_string_lossy().into_owned(),
        source,
        title,
        copyright,
        applied_at: Local::now().timestamp(),
    };
    state
        .history
        .borrow_mut()
        .push(entry.clone())?;
    *state.last_applied.borrow_mut() = Some(entry.file.clone());
    state.emit_wallpaper_changed(&entry.file, &entry.source, &entry.title);
    log::info!("applied wallpaper: {}", path.display());
    Ok(())
}

impl DaemonState {
    pub fn emit_signal(&self, signal: &str, body: &glib::Variant) {
        if let Some(conn) = self.conn.borrow().as_ref() {
            if let Err(e) = conn.emit_signal(None, OBJECT_PATH, INTERFACE, signal, Some(body)) {
                log::warn!("failed to send signal {signal}: {e}");
            }
        }
    }

    pub fn emit_wallpaper_changed(&self, file: &str, source: &str, title: &str) {
        self.emit_signal("WallpaperChanged", &(file, source, title).to_variant());
    }

    pub fn emit_fetch_failed(&self, source: &str, error: &str) {
        self.emit_signal("FetchFailed", &(source, error).to_variant());
    }

    pub fn emit_image_added(&self, source: &str, path: &str) {
        self.emit_signal("ImageAdded", &(source, path).to_variant());
    }

    /// Record multi-step progress for a running task (log entry + signals).
    pub fn set_task_progress(&self, log_id: u64, done: i32, total: i32) {
        self.task_log.borrow_mut().progress(log_id, done, total);
        self.emit_tasks_changed();
        self.emit_task_log_changed();
    }

    pub fn emit_tasks_changed(&self) {
        self.emit_signal("TasksChanged", &().to_variant());
    }

    pub fn emit_task_log_changed(&self) {
        self.emit_signal("TaskLogChanged", &().to_variant());
    }
}
