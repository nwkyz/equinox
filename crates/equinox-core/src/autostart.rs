//! Login-session autostart management.
//!
//! Equinox's background (daemon, supervised by `equinox-supervisor`) should
//! keep running after the GUI closes. Instead of depending on systemd, this
//! module manages the **XDG autostart** entry in `~/.config/autostart/` that
//! any conforming desktop session runs at login, and — as an opt-in for
//! systemd users — a `systemd --user` service unit wrapping the supervisor.
//!
//! Everything here is plain filesystem + `systemctl` invocations so it stays
//! usable from the core library (no GTK), the supervisor and the GUI alike.
//! On systems without systemd the `systemctl` calls simply fail/are skipped.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// XDG autostart entry name (also used for the systemd unit name).
pub const AUTOSTART_ID: &str = "equinox-supervisor";
/// Legacy systemd unit name (installed before the supervisor split).
/// Detection treats it as the same service so existing installs show up
/// correctly; uninstall cleans both.
const LEGACY_SERVICE: &str = "equinox-daemon.service";

/// Desktop entry installed at `~/.config/autostart/equinox-supervisor.desktop`.
fn autostart_dir() -> PathBuf {
    glib::user_config_dir().join("autostart")
}

fn autostart_file() -> PathBuf {
    autostart_dir().join(format!("{AUTOSTART_ID}.desktop"))
}

/// Systemd user unit path (`~/.config/systemd/user/equinox-supervisor.service`).
fn systemd_unit_path() -> PathBuf {
    glib::user_config_dir()
        .join("systemd")
        .join("user")
        .join(format!("{AUTOSTART_ID}.service"))
}

fn legacy_unit_path() -> PathBuf {
    glib::user_config_dir()
        .join("systemd")
        .join("user")
        .join(LEGACY_SERVICE)
}

/// Resolve the supervisor command used in `Exec=` lines — both the XDG
/// autostart entry **and** the systemd unit's `ExecStart`.
///
/// A bare `Exec=equinox-supervisor` silently fails in most sessions: GNOME
/// (and KDE via the systemd path) run autostart entries and services with the
/// systemd/display-manager environment, which does NOT include `~/.local/bin`
/// — exactly why `data/install.sh` writes the GUI's desktop entry with an
/// absolute path too. So the command must carry an absolute path: prefer a
/// sibling of the current executable (native installs put all three binaries
/// in `~/.local/bin`), fall back to a PATH lookup, and keep `flatpak run`
/// inside flatpak (the host resolves the app by id at login).
fn supervisor_exec() -> String {
    if let Ok(app) = std::env::var("FLATPAK_ID") {
        if !app.is_empty() {
            return format!("flatpak run --command=equinox-supervisor {app}");
        }
    }
    // Sibling of the running binary (GUI or supervisor): `…/bin/equinox-gui`
    // → `…/bin/equinox-supervisor`. Covers `~/.local/bin` installs and
    // `target/debug` dev runs.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("equinox-supervisor");
            if sibling.is_file() {
                return sibling.to_string_lossy().into_owned();
            }
        }
    }
    // PATH lookup for anything else (e.g. /usr/bin when packaged by a distro).
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("equinox-supervisor");
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    "equinox-supervisor".to_owned()
}

/// Whether we are running inside a flatpak sandbox. Inside the sandbox
/// `systemctl --user` is not available (no host systemd user bus), so the
/// systemd hosting option must be hidden/disabled.
pub fn in_flatpak() -> bool {
    std::env::var("FLATPAK_ID").is_ok_and(|v| !v.is_empty())
}

/// Whether the XDG autostart entry is currently installed.
pub fn autostart_enabled() -> bool {
    autostart_file().is_file()
}

/// Install (or remove) the XDG autostart entry so the supervisor starts at
/// the next login. Writing a `.desktop` is enough — the desktop environment
/// picks it up on the next session.
///
/// The `Exec=` line uses an absolute supervisor path (see [`supervisor_exec`]):
/// a bare `equinox-supervisor` would not resolve at login on GNOME/KDE, where
/// autostart entries run outside the login shell's PATH.
pub fn set_autostart(enabled: bool) -> Result<()> {
    let path = autostart_file();
    if !enabled {
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
        return Ok(());
    }
    let dir = autostart_dir();
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let exec = supervisor_exec();
    // Deliberately NO `Hidden=true`: KDE (and GNOME) treat a Hidden autostart
    // entry as "user deleted this, do not run", which silently killed
    // start-at-login. Trading the cosmetic "not in the autostart manager
    // list" for a working entry: it now shows in the manager list (honest,
    // and lets the user toggle it there too).
    let content = autostart_content(&exec);
    fs::write(&path, content)
        .with_context(|| format!("failed to write {}", path.display()))
}

/// Build the XDG autostart desktop entry body for a resolved `Exec=` command
/// (an absolute supervisor path, or the flatpak run form).
fn autostart_content(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Equinox background\n\
         Comment=Keep the Equinox wallpaper daemon running\n\
         Exec={exec}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// Whether an existing autostart file needs rewriting: its `Exec=` is stale
/// when no Exec line uses the currently resolved supervisor command, **or**
/// it carries `Hidden=true` — KDE and GNOME both treat a hidden autostart
/// entry as "do not run", so such an entry must be rewritten without the
/// flag to make login-startup work.
fn autostart_needs_rewrite(raw: &str, desired: &str) -> bool {
    let exec_ok = raw.lines().any(|l| {
        l.trim()
            .strip_prefix("Exec=")
            .is_some_and(|exec| exec.trim().contains(desired))
    });
    let hidden = raw
        .lines()
        .any(|l| l.trim().eq_ignore_ascii_case("Hidden=true"));
    !exec_ok || hidden
}

/// Heal a pre-existing autostart entry that carries a bare
/// `Exec=equinox-supervisor` (written by versions before the absolute-path
/// fix — those silently failed to start at login when `~/.local/bin` is not
/// on the session PATH). Rewrites it with the resolved command. No-op when
/// there is no entry or it already uses the resolved command.
pub fn refresh_autostart() {
    let path = autostart_file();
    if !path.is_file() {
        return;
    }
    // Heal a pre-existing autostart entry: stale `Exec=` (bare command from
    // before the absolute-path fix — silently failed when `~/.local/bin` is
    // not on the session PATH) OR a `Hidden=true` flag (KDE/GNOME do not run
    // hidden entries). Rewrites it with the resolved command. No-op when the
    // entry already looks right.
    let raw = fs::read_to_string(&path).unwrap_or_default();
    let desired = supervisor_exec();
    if autostart_needs_rewrite(&raw, &desired) {
        log::info!("rewriting the XDG autostart entry (stale Exec or hidden flag)");
        let _ = set_autostart(true);
    }
}

// ---------------------------------------------------------------------------
// systemd hosting
// ---------------------------------------------------------------------------

/// Whether a usable systemd **user** instance exists on this system (not in
/// a flatpak sandbox and a user manager is reachable). This gates the
/// "host via systemd" option in the UI.
///
/// The check is deliberately loose: `systemctl --user is-system-running`
/// reports `degraded` (non-zero) whenever any unrelated unit failed, which
/// would make a perfectly usable systemd look unavailable — so we test for
/// the user manager's private socket instead, which exists whenever a user
/// systemd instance is running. A working `systemctl --user` round-trip is
/// accepted as a fallback for environments where `XDG_RUNTIME_DIR` differs
/// between the shell and the GUI session.
pub fn systemd_available() -> bool {
    if in_flatpak() {
        return false;
    }
    let socket = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| glib::user_runtime_dir())
        .join("systemd/private");
    if socket.is_file() {
        return true;
    }
    // Fallback: ask systemd itself whether a user manager answers.
    std::process::Command::new("systemctl")
        .args(["--user", "show-environment"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Whether a systemd user service for the supervisor exists. Recognises both
/// the current unit name and the pre-supervisor legacy name so upgraded
/// installs are reported as installed.
pub fn systemd_enabled() -> bool {
    systemd_unit_path().is_file() || legacy_unit_path().is_file()
}

/// Whether the **current** (supervisor) systemd unit file exists — as opposed
/// to [`systemd_enabled`], which also accepts a legacy-name install. Callers
/// that need to start the supervisor unit must use this, because a legacy
/// unit alone would start the wrong binary.
pub fn systemd_unit_installed() -> bool {
    systemd_unit_path().is_file()
}

/// Whether the systemd user service unit is currently active (either name).
pub fn systemd_running() -> bool {
    systemctl(&["--user", "is-active", &format!("{AUTOSTART_ID}.service")])
        || systemctl(&["--user", "is-active", LEGACY_SERVICE])
}

/// Write the systemd user unit for the supervisor (does not start it).
fn write_unit() -> Result<()> {
    let path = systemd_unit_path();
    fs::create_dir_all(path.parent().expect("unit path has a parent"))
        .with_context(|| format!("failed to create {}", path.display()))?;
    let exec = supervisor_exec();
    let content = format!(
        "[Unit]\n\
         Description=Equinox wallpaper supervisor\n\
         After=graphical-session.target\n\
         \n\
         [Service]\n\
         ExecStart={exec}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    );
    fs::write(&path, content)
        .with_context(|| format!("failed to write {}", path.display()))
}

/// Deploy the systemd unit file (write + daemon-reload). Does NOT enable or
/// start it — the caller decides. Removes a legacy-name unit first so only
/// one unit exists. Idempotent: rewrites the unit with a current absolute
/// ExecStart, healing units written with a bare (PATH-dependent) command.
pub fn deploy_unit() -> Result<()> {
    // A legacy unit may already exist; stop+disable it before writing the
    // current name so only one unit runs.
    for unit in [LEGACY_SERVICE.to_owned()] {
        let _ = systemctl(&["--user", "stop", &unit]);
        let _ = systemctl(&["--user", "disable", &unit]);
    }
    let legacy = legacy_unit_path();
    if legacy.exists() {
        fs::remove_file(&legacy)?;
    }
    write_unit()?;
    let _ = systemctl(&["--user", "daemon-reload"]);
    Ok(())
}

/// Start the (already deployed) systemd user service. Errors with the unit's
/// status output when it fails to come up.
pub fn systemd_start() -> Result<()> {
    let unit = format!("{AUTOSTART_ID}.service");
    if !systemctl(&["--user", "start", &unit]) {
        let status = std::process::Command::new("systemctl")
            .args(["--user", "status", &unit])
            .output()
            .ok();
        let detail = status
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();
        return Err(anyhow::anyhow!(
            "systemd service failed to start:\n{}",
            detail.lines().take(6).collect::<Vec<_>>().join("\n")
        ));
    }
    Ok(())
}

/// Stop the systemd user service (leaves the unit deployed and enabled).
pub fn systemd_stop() {
    let _ = systemctl(&["--user", "stop", &format!("{AUTOSTART_ID}.service")]);
}

/// Stop and disable the systemd service but **keep the unit file** — used
/// when switching hosting to the standalone supervisor. The unit stays
/// deployed (so switching back to systemd is instant) but will not start at
/// login, so it cannot race the standalone supervisor on the next session.
pub fn deactivate_systemd_service() {
    let unit = format!("{AUTOSTART_ID}.service");
    let _ = systemctl(&["--user", "stop", &unit]);
    let _ = systemctl(&["--user", "disable", &unit]);
}

/// Whether the unit is enabled (starts at login via systemd).
pub fn systemd_boot_enabled() -> bool {
    systemctl(&["--user", "is-enabled", &format!("{AUTOSTART_ID}.service")])
}

/// Enable or disable start-at-login for the deployed unit.
pub fn set_systemd_boot(enabled: bool) {
    let unit = format!("{AUTOSTART_ID}.service");
    let _ = systemctl(&["--user", if enabled { "enable" } else { "disable" }, &unit]);
}

/// Install (deploy + enable + start) the systemd user service — used when
/// switching hosting to systemd. Removes the XDG autostart entry so the two
/// mechanisms never race. Returns an error when systemd is unusable.
pub fn install_systemd_service() -> Result<()> {
    if !systemd_available() {
        return Err(anyhow::anyhow!(
            "systemd user services are not available in this environment"
        ));
    }
    deploy_unit()?;
    let _ = set_autostart(false);
    let unit = format!("{AUTOSTART_ID}.service");
    let _ = systemctl(&["--user", "enable", &unit]);
    systemd_start()
}

/// Stop, disable and remove the systemd user service (switching back to the
/// standalone supervisor). Cleans up both the current and the legacy unit
/// name. Idempotent when the units do not exist.
pub fn uninstall_systemd_service() -> Result<()> {
    for unit in [format!("{AUTOSTART_ID}.service"), LEGACY_SERVICE.to_owned()] {
        let _ = systemctl(&["--user", "stop", &unit]);
        let _ = systemctl(&["--user", "disable", &unit]);
    }
    for path in [systemd_unit_path(), legacy_unit_path()] {
        if path.exists() {
            fs::remove_file(&path)?;
        }
    }
    let _ = systemctl(&["--user", "daemon-reload"]);
    Ok(())
}

/// Run `systemctl` (best-effort; returns false when systemd is unavailable).
fn systemctl(args: &[&str]) -> bool {
    std::process::Command::new("systemctl")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_id_is_supervisor() {
        // The whole autostart story is built around the supervisor binary;
        // guard against accidental renames that would desync scripts/docs.
        assert_eq!(AUTOSTART_ID, "equinox-supervisor");
    }

    #[test]
    fn legacy_service_name_is_daemon() {
        // The pre-supervisor unit name must stay recognised so upgraded
        // installs show the systemd service as installed.
        assert_eq!(LEGACY_SERVICE, "equinox-daemon.service");
    }

    #[test]
    fn autostart_rewrite_detects_bare_command_and_hidden() {
        let abs = "/home/u/.local/bin/equinox-supervisor";
        // A bare `Exec=equinox-supervisor` (the pre-fix output) is stale.
        assert!(autostart_needs_rewrite(
            "[Desktop Entry]\nExec=equinox-supervisor\n",
            abs
        ));
        // Correct Exec, no Hidden → nothing to heal.
        assert!(!autostart_needs_rewrite(
            "[Desktop Entry]\nExec=/home/u/.local/bin/equinox-supervisor\n",
            abs
        ));
        // Flatpak form counts as resolved.
        let flatpak_cmd = "flatpak run --command=equinox-supervisor github.nwkyz.Equinox";
        assert!(!autostart_needs_rewrite(
            &format!("[Desktop Entry]\nExec={flatpak_cmd}\n"),
            flatpak_cmd
        ));
        assert!(autostart_needs_rewrite("", abs));
        // Hidden=true must be healed even when the Exec is already resolved:
        // KDE/GNOME do not run hidden autostart entries.
        assert!(autostart_needs_rewrite(
            "[Desktop Entry]\nExec=/home/u/.local/bin/equinox-supervisor\nHidden=true\n",
            abs
        ));
    }

    #[test]
    fn autostart_entry_is_runnable_and_not_hidden() {
        // The written entry must reference the resolved command and must NOT
        // carry Hidden=true (that flag silently disables start-at-login on
        // KDE/GNOME — the exact bug reported on the VM).
        let content = autostart_content("/usr/bin/equinox-supervisor");
        assert!(content.contains("Exec=/usr/bin/equinox-supervisor"));
        assert!(!content.to_ascii_lowercase().contains("hidden=true"));
        assert!(!content.contains("Exec=equinox-supervisor\n"));
    }

    #[test]
    fn supervisor_exec_resolves_absolute_or_bare() {
        let exec = supervisor_exec();
        if in_flatpak() {
            // Inside a sandbox only the flatpak run form can reach the host.
            assert!(exec.starts_with("flatpak run --command=equinox-supervisor"));
        } else {
            // In the test environment (no sibling, usually no PATH hit) the
            // last-resort bare name is acceptable; real installs resolve
            // earlier. The property that matters: when it is not bare, it is
            // an absolute path to the supervisor.
            if exec != "equinox-supervisor" {
                assert!(exec.ends_with("/equinox-supervisor"));
                assert!(exec.starts_with('/'));
            }
        }
    }
}
