//! Sets the desktop wallpaper across multiple Linux desktop environments.
//!
//! `Applier` is a thin dispatcher over pluggable [`WallpaperBackend`]s. The
//! active backend is chosen automatically from the environment (GNOME → KDE →
//! XFCE, first one whose prerequisites are present), or forced via the
//! `backend` key in the config (`auto`/`gnome`/`kde`/`xfce`).
//!
//! Every apply is a short, synchronous operation (a gsettings write, an
//! in-place edit of a Plasma/XFCE config file, or a one-off `xfconf-query`
//! call), so this stays callable on the daemon's single-threaded main loop.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use gettextrs::gettext;
use gio::prelude::*;

use crate::config::Config;

/// Identifier of a wallpaper backend; `""`/unknown means auto-detect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Auto,
    Gnome,
    Kde,
    Xfce,
}

impl Backend {
    /// "auto"/""/empty → Auto; unknown values fall back to Auto.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "gnome" => Backend::Gnome,
            "kde" | "plasma" => Backend::Kde,
            "xfce" | "xubuntu" => Backend::Xfce,
            _ => Backend::Auto,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Backend::Auto => "auto",
            Backend::Gnome => "gnome",
            Backend::Kde => "kde",
            Backend::Xfce => "xfce",
        }
    }
}

/// A concrete wallpaper-setting backend. Implementations are stateless and
/// cheap; `available()` only inspects the environment, `apply()` runs
/// synchronously and returns a user-readable error on failure.
pub trait WallpaperBackend: Send + Sync {
    fn id(&self) -> &'static str;
    /// Whether the current environment provides this backend's prerequisites.
    fn available(&self) -> bool;
    /// Set the desktop wallpaper to the image at `path`.
    fn apply(&self, path: &Path) -> Result<()>;
    /// The currently set wallpaper as a **local file path** (best-effort;
    /// `None` when it cannot be read, is not a local file, or the backend
    /// does not support reading). Used to remember the user's original
    /// wallpaper before the first Equinox apply, so switching to the "None"
    /// source can restore it.
    fn current(&self) -> Option<String> {
        None
    }
}

/// Dispatcher that picks a backend per the config / environment.
///
/// Public methods keep the historical names (`available` / `apply`) but take
/// a `&Config` so a manually-configured `backend` key takes precedence over
/// auto-detection.
pub struct Applier;

impl Applier {
    /// The backend matching the desktop session named in
    /// `XDG_CURRENT_DESKTOP` (case-insensitive), or `None` when the session is
    /// unset or names an unsupported desktop. When a session explicitly says
    /// KDE/Plasma but the GNOME gsettings schema is also installed (common on
    /// KDE distros — it comes in transitively), this preference keeps the
    /// correct backend from being shadowed by the GNOME-first fallback.
    fn preferred_backend() -> Option<Backend> {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let desktop = desktop.trim().to_ascii_lowercase();
        if desktop.is_empty() {
            return None;
        }
        if desktop.contains("gnome") {
            Some(Backend::Gnome)
        } else if desktop.contains("kde") || desktop.contains("plasma") {
            Some(Backend::Kde)
        } else if desktop.contains("xfce") || desktop.contains("xubuntu") {
            Some(Backend::Xfce)
        } else {
            // An explicit but unsupported desktop (Cinnamon/Sway/Hyprland…):
            // do not force a backend; the capability fallback may still pick
            // one (e.g. Cinnamon reads the org.gnome.desktop.background keys).
            None
        }
    }

    /// Resolve the backend to use: the config-forced one if it is usable,
    /// otherwise the session's desktop if it is usable, otherwise the first
    /// auto-detected backend (GNOME → KDE → XFCE).
    fn resolve(config: &Config) -> Box<dyn WallpaperBackend> {
        let forced = config.backend();
        if forced != "auto" && !forced.is_empty() {
            let candidate: Box<dyn WallpaperBackend> = backend_for(Backend::parse(&forced));
            if candidate.available() {
                return candidate;
            }
            log::warn!(
                "configured wallpaper backend '{forced}' is unavailable, falling back to auto-detection"
            );
        }
        if let Some(pref) = Self::preferred_backend() {
            let candidate = backend_for(pref);
            if candidate.available() {
                return candidate;
            }
        }
        for b in [Backend::Gnome, Backend::Kde, Backend::Xfce] {
            let candidate = backend_for(b);
            if candidate.available() {
                return candidate;
            }
        }
        log::warn!("no usable wallpaper backend detected (GNOME/KDE/XFCE)");
        Box::new(UnavailableBackend)
    }

    /// Whether any wallpaper backend is usable in this environment.
    pub fn available() -> bool {
        match Config::new() {
            Ok(c) => Self::resolve(&c).available(),
            Err(_) => {
                // No config is writable → fall back to pure auto-detection.
                [Backend::Gnome, Backend::Kde, Backend::Xfce]
                    .into_iter()
                    .any(|b| backend_for(b).available())
            }
        }
    }

    /// The id of the backend that would be used, or `None` when none is
    /// available. Skips logging the auto-detect fallback (used for a startup
    /// log line).
    pub fn active_backend() -> Option<&'static str> {
        match Config::new() {
            Ok(c) => match Self::resolve_no_log(&c) {
                Backend::Auto => None,
                b => Some(b.id()),
            },
            Err(_) => None,
        }
    }

    /// Like [`resolve`] but returns the backend id without falling back to the
    /// "unavailable" placeholder — the caller decides how to present "none".
    fn resolve_no_log(config: &Config) -> Backend {
        let forced = config.backend();
        if forced != "auto" && !forced.is_empty() {
            let b = Backend::parse(&forced);
            if backend_for(b).available() {
                return b;
            }
        }
        if let Some(pref) = Self::preferred_backend() {
            if backend_for(pref).available() {
                return pref;
            }
        }
        for b in [Backend::Gnome, Backend::Kde, Backend::Xfce] {
            if backend_for(b).available() {
                return b;
            }
        }
        Backend::Auto
    }

    /// Set the wallpaper, honoring the config backend override if any.
    pub fn apply(path: &Path) -> Result<()> {
        let config = Config::new()?;
        Self::resolve(&config).apply(path)
    }

    /// The currently set wallpaper as a local path, via the active backend.
    /// `None` when no backend is usable or it cannot report the current
    /// wallpaper (see [`WallpaperBackend::current`]).
    pub fn current() -> Option<String> {
        let config = Config::new().ok()?;
        Self::resolve(&config).current()
    }
}

fn backend_for(b: Backend) -> Box<dyn WallpaperBackend> {
    match b {
        Backend::Gnome => Box::new(GnomeBackend),
        Backend::Kde => Box::new(KdeBackend),
        Backend::Xfce => Box::new(XfceBackend),
        Backend::Auto => Box::new(UnavailableBackend),
    }
}

/// Fallback used when no configuration is writable or nothing is usable.
/// `available()` is never true, so `resolve` only returns it after logging.
struct UnavailableBackend;
impl WallpaperBackend for UnavailableBackend {
    fn id(&self) -> &'static str {
        "none"
    }
    fn available(&self) -> bool {
        false
    }
    fn apply(&self, _path: &Path) -> Result<()> {
        Err(anyhow!(gettext(
            "no supported wallpaper backend detected (need GNOME, KDE, or XFCE)"
        )))
    }
}

// ---------------------------------------------------------------------------
// GNOME
// ---------------------------------------------------------------------------

const BG_SCHEMA: &str = "org.gnome.desktop.background";

struct GnomeBackend;

impl WallpaperBackend for GnomeBackend {
    fn id(&self) -> &'static str {
        "gnome"
    }

    fn available(&self) -> bool {
        gio::SettingsSchemaSource::default()
            .and_then(|s| s.lookup(BG_SCHEMA, true))
            .is_some()
    }

    fn apply(&self, path: &Path) -> Result<()> {
        if !self.available() {
            return Err(anyhow!(gettext(
                "this environment cannot set the GNOME wallpaper (missing org.gnome.desktop.background schema)"
            )));
        }
        let bg = gio::Settings::new(BG_SCHEMA);
        let uri = format!("file://{}", path.display());
        bg.set_string("picture-uri", &uri)
            .with_context(|| format!("failed to set picture-uri: {}", path.display()))?;
        // Dark-scheme key; ignored when missing in some environments
        let _ = bg.set_string("picture-uri-dark", &uri);
        Ok(())
    }

    fn current(&self) -> Option<String> {
        if !self.available() {
            return None;
        }
        let uri = gio::Settings::new(BG_SCHEMA)
            .string("picture-uri")
            .to_string();
        // Only a local file can be restored via apply(); a remote/or-scaling
        // uri is left alone (the original would be unrecoverable as a path).
        uri.strip_prefix("file://")
            .filter(|p| !p.is_empty())
            .map(ToOwned::to_owned)
    }
}

// ---------------------------------------------------------------------------
// KDE Plasma
// ---------------------------------------------------------------------------

/// Plasma desktop wallpaper config file (maps to `$XDG_CONFIG_HOME`).
fn plasma_appletsrc_path() -> PathBuf {
    glib::user_config_dir().join("plasma-org.kde.plasma.desktop-appletsrc")
}

/// Session-bus name of the running Plasma shell.
const PLASMA_SHELL_BUS: &str = "org.kde.plasmashell";
const PLASMA_SHELL_OBJECT: &str = "/PlasmaShell";
/// Interface exported on `/PlasmaShell` by plasmashell. NB: the name keeps
/// `org.kde.plasmashell` (all-lowercase, that is the D-Bus **bus** name) but
/// the **interface** is Qt-camelCased `org.kde.PlasmaShell` — using the
/// lowercase form yields "No such interface". Verified on Plasma 6.6.
const PLASMA_SHELL_IFACE: &str = "org.kde.PlasmaShell";

/// Build the `setWallpaper` argument tuple `(sa{sv}u)` — plugin id, a
/// `{sv}` property map (the org.kde.image plugin's General group), screen
/// number. `setWallpaper` is the dedicated scripting API of Plasma 6; the old
/// `evaluateScript` JS globals (`desktopsForActivity`/`currentActivity`) are
/// unreliable/broken on Plasma 6 (returns empty), so scripts are NOT used.
fn plasma_set_wallpaper_params(uri: &str, screen: u32) -> glib::Variant {
    // Values must be BOXED ('v') so the dict is a{sv}, not a{ss}/{su}; and the
    // outer tuple is assembled with tuple_from_iter so the dict stays a{sv}
    // (ToVariant on a glib::Variant would re-box it as 'v').
    let v_image = glib::Variant::from_variant(&glib::Variant::from(uri));
    let v_fill = glib::Variant::from_variant(&glib::Variant::from(2u32));
    let ty = glib::VariantTy::new("{sv}").expect("valid dict-entry type");
    let dict = glib::Variant::array_from_iter_with_type(
        &ty,
        [
            glib::Variant::from_dict_entry(&glib::Variant::from("Image"), &v_image),
            glib::Variant::from_dict_entry(&glib::Variant::from("FillMode"), &v_fill),
        ],
    );
    glib::Variant::tuple_from_iter([
        glib::Variant::from("org.kde.image"),
        dict,
        glib::Variant::from(screen),
    ])
}

/// Ask the running Plasma shell to set the wallpaper on every screen to `abs`.
///
/// Returns:
/// - `Some(Ok(()))` — at least one screen was set;
/// - `Some(Err(e))` — the shell is reachable but every screen rejected the call;
/// - `None` — no session bus / no plasmashell (the caller should fall back to
///   rewriting the appletsrc file).
fn plasma_shell_set_wallpaper(abs: &str) -> Option<Result<()>> {
    let conn = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).ok()?;
    // Never auto-start plasmashell if it is not running (headless / non-Plasma
    // sessions where the fallback config rewrite is the right answer).
    let proxy = gio::DBusProxy::new_sync(
        &conn,
        gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES | gio::DBusProxyFlags::DO_NOT_AUTO_START,
        None::<&gio::DBusInterfaceInfo>,
        Some(PLASMA_SHELL_BUS),
        PLASMA_SHELL_OBJECT,
        PLASMA_SHELL_IFACE,
        None::<&gio::Cancellable>,
    )
    .ok()?;
    let uri = format!("file://{abs}");
    // Only write screens that actually exist: `wallpaper(n)` returns a
    // non-empty dict for a real screen and `{}` for a non-existent one
    // (verified on a 1-screen Plasma 6.6). Covers up to 4 monitors.
    let mut set = 0u32;
    for screen in 0..4u32 {
        let probe = proxy.call_sync(
            "wallpaper",
            Some(&(screen,).to_variant()),
            gio::DBusCallFlags::NONE,
            3000,
            None::<&gio::Cancellable>,
        );
        // Reply is `(a{sv})`; an empty dict means the screen does not exist.
        let entries = probe
            .map(|v| v.child_value(0).n_children())
            .unwrap_or(0);
        if entries == 0 {
            continue;
        }
        let params = plasma_set_wallpaper_params(&uri, screen);
        match proxy.call_sync(
            "setWallpaper",
            Some(&params),
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
        ) {
            Ok(_) => set += 1,
            Err(e) => log::warn!("Plasma setWallpaper(screen {screen}) failed: {e}"),
        }
    }
    if set > 0 {
        log::info!("applied Plasma wallpaper via plasmashell: {set} screen(s)");
        Some(Ok(()))
    } else {
        Some(Err(anyhow!(
            "{}",
            gettext("Failed to set Plasma wallpaper via the running shell")
        )))
    }
}

struct KdeBackend;

/// True when the session desktop names Plasma/KDE (`XDG_CURRENT_DESKTOP`),
/// which is set by the compositor and inherited into a flatpak sandbox.
fn on_plasma_session() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .split(';')
        .any(|d| d.contains("kde") || d.contains("plasma"))
}

impl WallpaperBackend for KdeBackend {
    fn id(&self) -> &'static str {
        "kde"
    }

    fn available(&self) -> bool {
        // The appletsrc file is the native signal, but a flatpak sandbox does
        // not expose the host's ~/.config (only xdg-config/equinox is shared),
        // so the file is invisible there. That alone would report KDE
        // "unavailable" and make the dispatcher fall through to the GNOME
        // backend, which cannot set a Plasma wallpaper. The primary apply path
        // talks to the RUNNING shell over D-Bus (org.kde.plasmashell), which
        // the sandbox permits, so also treat an active Plasma session as
        // available via XDG_CURRENT_DESKTOP (which flatpak passes through).
        plasma_appletsrc_path().is_file() || on_plasma_session()
    }

    fn apply(&self, path: &Path) -> Result<()> {
        let cfg = plasma_appletsrc_path();
        let abs = fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned();

        // Preferred: the RUNNING shell sets the wallpaper itself — this is the
        // same code path Plasma's own settings dialog uses and is immune to
        // appletsrc layout differences (Plasma 6 desktops can keep their
        // containment in a form the file rewrite cannot find). Only when the
        // shell is unreachable (headless, non-Plasma) do we edit the file.
        if let Some(res) = plasma_shell_set_wallpaper(&abs) {
            match res {
                Ok(()) => return Ok(()),
                Err(e) => log::warn!("plasmashell wallpaper script failed: {e:#}; falling back to appletsrc rewrite"),
            }
        }

        // Fallback: rewrite the file directly.
        let raw = fs::read_to_string(&cfg)
            .with_context(|| format!("{}: {}", gettext("Failed to read"), cfg.display()))?;
        let updated = rewrite_plasma(&raw, &abs).ok_or_else(|| {
            // Diagnostic: dump what the file actually declares so we can tell
            // whether we simply missed a different wallpaper layout.
            let plugins: Vec<String> = raw
                .lines()
                .filter_map(|l| {
                    kconfig_value(l, "plugin")
                        .or_else(|| kconfig_value(l, "wallpaperplugin"))
                        .map(|v| v.trim().to_string())
                })
                .collect();
            let groups: Vec<String> = raw
                .lines()
                .filter(|l| l.contains("] [") || l.contains("[Wallpaper]"))
                .map(|l| l.trim().to_string())
                .collect();
            log::warn!(
                "KDE: no image wallpaper found in {};\n  plugins={plugins:?}\n  wallpaper-ish lines={groups:?}",
                cfg.display()
            );
            anyhow!(format!("{}: {}", gettext("No image-type wallpaper configured in Plasma"), cfg.display()))
        })?;
        // Atomic write (temp file + rename), mirroring the config writer.
        let tmp = cfg.with_extension("appletsrc.tmp");
        fs::write(&tmp, updated).with_context(|| {
            format!("{}: {}", gettext("Failed to write"), tmp.display())
        })?;
        fs::rename(&tmp, &cfg).with_context(|| {
            format!("{}: {}", gettext("Failed to write"), cfg.display())
        })?;
        log::info!("applied Plasma wallpaper by editing {}", cfg.display());
        Ok(())
    }

    fn current(&self) -> Option<String> {
        plasma_appletsrc_current()
    }
}

/// The current Plasma image wallpaper, from the `Image=` key inside the
/// `[Containments][N][Wallpaper][org.kde.image][General]` group of the
/// appletsrc file. `None` when unreadable (e.g. inside flatpak, where the
/// host's `~/.config` is not shared) or no image wallpaper is configured.
fn plasma_appletsrc_current() -> Option<String> {
    let raw = fs::read_to_string(plasma_appletsrc_path()).ok()?;
    plasma_current_from_raw(&raw)
}

/// Pure parser backing [`plasma_appletsrc_current`] (testable).
fn plasma_current_from_raw(raw: &str) -> Option<String> {
    let mut in_image_group = false;
    for line in raw.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            in_image_group = is_image_wallpaper_group(line);
            continue;
        }
        if in_image_group {
            if let Some(v) = kconfig_value(t, "Image").map(str::trim).filter(|s| !s.is_empty()) {
                return Some(v.strip_prefix("file://").unwrap_or(v).to_owned());
            }
        }
    }
    None
}

/// Case-insensitive `key=value` parse of a trim-red config line. KConfig keys
/// are case-insensitive, and Plasma writes `plugin=`/`wallpaperplugin=` in
/// **lowercase** in real files — matching must ignore case, or the desktop
/// containment is silently missed (the "no image wallpaper found" false
/// negative behind the user-visible *failed to set wallpaper*).
fn kconfig_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let t = line.trim();
    let eq = format!("{key}=");
    (t.len() > eq.len() && t[..eq.len()].eq_ignore_ascii_case(&eq)).then(|| t[eq.len()..].trim())
}

/// A `[Containments][N][Wallpaper][org.kde.image][General]` group header?
/// Group names are case-insensitive in KConfig; compare lowercased.
fn is_image_wallpaper_group(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('[')
        && t.ends_with(']')
        && t.to_ascii_lowercase().ends_with("[wallpaper][org.kde.image][general]")
}

/// Rewrite the Plasma desktop wallpaper to `abs`.
///
/// Handles two layouts, so it works on fresh/complex Plasma installs alike:
/// 1. An existing `[Containments][<id>][Wallpaper][org.kde.image][General]`
///    group — replace its first `Image=` value; when the group has no
///    `Image=` key yet, insert one at the group's end.
/// 2. No such group at all (e.g. a fresh desktop whose wallpaper is managed
///    elsewhere, or a slideshow default) — find the first `[Containments][N]`
///    whose `plugin=`/`wallpaperPlugin=org.kde.image` marks an image desktop
///    and CREATE the wallpaper group at the END of that containment's key
///    block (before the next section header), so containment keys that follow
///    the marker line stay inside the containment group.
///
/// Returns `None` only when the config has no image-type desktop at all
/// (guards against clobbering unknown layouts).
fn rewrite_plasma(raw: &str, abs: &str) -> Option<String> {
    let has_group = raw.lines().any(is_image_wallpaper_group);

    if has_group {
        // Pass 1: rewrite inside the existing image wallpaper group, keeping
        // every other line/group byte-for-byte.
        let mut out = String::with_capacity(raw.len() + abs.len() + 8);
        let mut in_image_group = false;
        let mut replaced = false;

        for line in raw.split('\n') {
            let t = line.trim();
            if t.is_empty() {
                out.push('\n');
                continue;
            }
            if t.starts_with('[') && t.ends_with(']') {
                // Leaving a group: if it was the first image group and carried
                // no `Image=` key, insert one before the next header so the
                // wallpaper can actually be set.
                if in_image_group && !replaced {
                    out.push_str("Image=");
                    out.push_str(abs);
                    out.push('\n');
                    replaced = true;
                }
                in_image_group = is_image_wallpaper_group(line);
                out.push_str(line);
                out.push('\n');
                continue;
            }
            if in_image_group && kconfig_value(t, "Image").is_some() && !replaced {
                out.push_str("Image=");
                out.push_str(abs);
                out.push('\n');
                replaced = true;
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        if in_image_group && !replaced {
            out.push_str("Image=");
            out.push_str(abs);
            out.push('\n');
            replaced = true;
        }
        if replaced {
            Some(out)
        } else {
            None
        }
    } else {
        // Pass 2: no image wallpaper group exists — create one in the first
        // `[Containments][N]` whose `plugin=`/`wallpaperPlugin=org.kde.image`
        // marks an image desktop.
        //
        // The group is inserted at the END of that containment's key block
        // (right before the next `[...]` header or EOF), NOT right after the
        // marker line: real Plasma configs put many containment keys
        // (`activityId=`, `lastScreen=`, `wallpaperPlugin=`, ...) after the
        // plugin marker, and inserting mid-block would swallow them into the
        // new wallpaper group and corrupt the containment (Plasma then loses
        // the wallpaper / reverts the file).
        let mut out = String::with_capacity(raw.len() + abs.len() + 64);
        let mut created = false;
        // Id of the containment we are currently inside (`[Containments][N]`).
        let mut in_containment: Option<String> = None;
        // Id of the containment whose wallpaper group is pending creation —
        // flushed at the next section header (or EOF) so its keys stay put.
        let mut pending_cid: Option<String> = None;

        for line in raw.split('\n') {
            let t = line.trim();
            let is_header = t.starts_with('[') && t.ends_with(']');
            if !created && is_header {
                // A section boundary: if a wallpaper group is pending for the
                // previous containment, it is inserted here (its key block has
                // ended). This keeps every key that followed the marker line
                // inside the containment group.
                if let Some(cid) = pending_cid.take() {
                    out.push_str(
                        &format!("\n[Containments][{cid}][Wallpaper][org.kde.image][General]\nImage={abs}\n"),
                    );
                    created = true;
                }
                // Track the containment this header opens. A nested header such
                // as `[Containments][N][General]` yields no id (nothing new to
                // match), but it still flushes any pending creation above.
                let id = t
                    .strip_prefix("[Containments]")
                    .and_then(|rest| {
                        let inner = rest.trim_matches(|c| c == '[' || c == ']');
                        (!inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()))
                            .then_some(inner.to_owned())
                    });
                in_containment = id;
            }
            if !created {
                // A desktop image containment is marked by `plugin=` OR
                // `wallpaperplugin=org.kde.image` (the latter is the standard
                // Plasma marker — `plugin` there is the folder/desktop
                // containment type). Keys are case-insensitive: real files
                // write `wallpaperplugin=` in lowercase.
                let is_image_plugin = kconfig_value(t, "plugin")
                    .or_else(|| kconfig_value(t, "wallpaperplugin"))
                    .is_some_and(|v| v.eq_ignore_ascii_case("org.kde.image"));
                if is_image_plugin {
                    if let Some(cid) = in_containment.clone() {
                        pending_cid = Some(cid);
                    }
                }
            }
            out.push_str(line);
            out.push('\n');
        }
        // EOF: flush a pending wallpaper group after the last containment block.
        if !created {
            if let Some(cid) = pending_cid.take() {
                out.push_str(
                    &format!("\n[Containments][{cid}][Wallpaper][org.kde.image][General]\nImage={abs}\n"),
                );
                created = true;
            }
        }
        if created {
            Some(out)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// XFCE
// ---------------------------------------------------------------------------

/// XFCE desktop backdrop config file (`$XDG_CONFIG_HOME`).
fn xfce_desktop_xml_path() -> PathBuf {
    glib::user_config_dir()
        .join("xfce4")
        .join("xfconf")
        .join("xfce-perchannel-xml")
        .join("xfce4-desktop.xml")
}

/// Xfconf is a session-bus D-Bus service (`org.xfce.Xfconf`); the backdrop
/// properties live in the `xfce4-desktop` channel. Talking to it over D-Bus
/// works NORMALLY and inside a flatpak sandbox — which shares the host's
/// session bus but has neither `xfconf-query` nor `xrandr` in its runtime.
const XFCONF_DBUS_NAME: &str = "org.xfce.Xfconf";
const XFCONF_DBUS_PATH: &str = "/org/xfce/Xfconf";
const XFCONF_DBUS_IFACE: &str = "org.xfce.Xfconf";
const XFCONF_CHANNEL: &str = "xfce4-desktop";

struct XfceBackend;

impl WallpaperBackend for XfceBackend {
    fn id(&self) -> &'static str {
        "xfce"
    }

    fn available(&self) -> bool {
        // Reachable via the xfconf D-Bus service (works inside flatpak, where
        // the session bus is shared but xfconf-query/xrandr are absent), the
        // xfconf-query CLI, or the stored channel XML.
        xfconf_dbus_available() || xfconf_available() || xfce_desktop_xml_path().is_file()
    }

    fn apply(&self, path: &Path) -> Result<()> {
        let abs = fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned();

        // Preferred: set every monitor's /last-image through the RUNNING
        // xfconf daemon over D-Bus. Works natively and inside flatpak; the
        // daemon creates missing properties (fresh XFCE has no /backdrop).
        if let Some(proxy) = xfconf_dbus_proxy() {
            let mut targets = xfconf_list_backdrop_properties_dbus(&proxy);
            for name in monitor_names() {
                let p = format!("/backdrop/screen0/monitor{name}/workspace0/last-image");
                if !targets.contains(&p) {
                    targets.push(p);
                }
            }
            if !targets.is_empty() {
                for p in &targets {
                    xfconf_set_property_dbus(&proxy, &p, &abs).with_context(|| {
                        format!("{}: {p}", gettext("Failed to set wallpaper"))
                    })?;
                }
                log::info!(
                    "applied XFCE wallpaper via xfconf D-Bus on {} backdrop(s)",
                    targets.len()
                );
                return Ok(());
            }
        }

        // Native fallback: the xfconf-query CLI (absent inside flatpak).
        if xfconf_available() {
            let paths = match list_backdrop_properties() {
                Ok(p) if !p.is_empty() => p,
                _ => ensure_backdrop_properties(),
            };
            if !paths.is_empty() {
                for p in &paths {
                    set_property(p, &abs).with_context(|| {
                        format!("{}: {p}", gettext("Failed to set wallpaper"))
                    })?;
                }
                log::info!(
                    "applied XFCE wallpaper via xfconf-query on {} backdrop(s)",
                    paths.len()
                );
                return Ok(());
            }
        }

        // Last resort: edit the xfce4-desktop.xml directly (no xfconf daemon).
        let xml = xfce_desktop_xml_path();
        let raw = fs::read_to_string(&xml)
            .with_context(|| format!("{}: {}", gettext("Failed to read"), xml.display()))?;
        let updated = rewrite_xfce_xml(&raw, &abs).ok_or_else(|| {
            anyhow!(format!(
                "{}: {}",
                gettext("No wallpaper property found in"),
                xml.display()
            ))
        })?;
        let tmp = xml.with_extension("xml.tmp");
        fs::write(&tmp, updated).with_context(|| {
            format!("{}: {}", gettext("Failed to write"), tmp.display())
        })?;
        fs::rename(&tmp, &xml).with_context(|| {
            format!("{}: {}", gettext("Failed to write"), xml.display())
        })?;
        log::info!("applied XFCE wallpaper by editing {}", xml.display());
        Ok(())
    }

    fn current(&self) -> Option<String> {
        let proxy = xfconf_dbus_proxy()?;
        for p in xfconf_list_backdrop_properties_dbus(&proxy) {
            if p.ends_with("/last-image") {
                if let Some(v) = xfconf_get_property_dbus(&proxy, &p) {
                    if !v.is_empty() {
                        return Some(v);
                    }
                }
            }
        }
        None
    }
}

fn xfconf_available() -> bool {
    std::process::Command::new("xfconf-query")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Connect (without auto-starting) to the host's xfconf daemon. Returns `None`
/// when the service is absent or unreachable — e.g. not an XFCE session.
fn xfconf_dbus_proxy() -> Option<gio::DBusProxy> {
    let conn = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).ok()?;
    gio::DBusProxy::new_sync(
        &conn,
        gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES | gio::DBusProxyFlags::DO_NOT_AUTO_START,
        None::<&gio::DBusInterfaceInfo>,
        Some(XFCONF_DBUS_NAME),
        XFCONF_DBUS_PATH,
        XFCONF_DBUS_IFACE,
        None::<&gio::Cancellable>,
    )
    .ok()
}

fn xfconf_dbus_available() -> bool {
    xfconf_dbus_proxy().is_some()
}

/// All `/last-image` paths under `/backdrop` in the xfce4-desktop channel
/// (one per monitor/workspace), via `GetAllProperties`. A fresh XFCE has no
/// `/backdrop` subtree and the call fails with PropertyNotFound — that is an
/// empty result, not an error.
fn xfconf_list_backdrop_properties_dbus(proxy: &gio::DBusProxy) -> Vec<String> {
    let reply = proxy.call_sync(
        "GetAllProperties",
        Some(&(XFCONF_CHANNEL.to_owned(), "/backdrop".to_owned()).to_variant()),
        gio::DBusCallFlags::NONE,
        3000,
        None::<&gio::Cancellable>,
    );
    let Ok(reply) = reply else {
        return Vec::new();
    };
    let dict = reply.child_value(0);
    let mut out = Vec::new();
    for i in 0..dict.n_children() {
        let entry = dict.child_value(i);
        if let Some(key) = entry.child_value(0).get::<String>() {
            if key.ends_with("/last-image") {
                out.push(key);
            }
        }
    }
    out
}

/// Set (creating if needed) a channel property via `SetProperty(channel,
/// property, value-v)`.
fn xfconf_set_property_dbus(proxy: &gio::DBusProxy, prop: &str, value: &str) -> Result<()> {
    // SetProperty is (channel:s, property:s, value:v). ToVariant for Variant
    // re-boxes (Variant::from_variant), so pass the RAW 's' variant and let
    // the tuple's .to_variant() wrap it exactly once into 'v' — pre-boxing
    // would yield v(v(s)) and xfconf answers InternalError.
    let value_s = glib::Variant::from(value);
    proxy
        .call_sync(
            "SetProperty",
            Some(&(XFCONF_CHANNEL.to_owned(), prop.to_owned(), value_s).to_variant()),
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
        )
        .with_context(|| format!("xfconf SetProperty failed for {prop}"))?;
    Ok(())
}

/// Read a channel property via `GetProperty(channel, property)`. Returns
/// `None` when the property is missing or not a string.
fn xfconf_get_property_dbus(proxy: &gio::DBusProxy, prop: &str) -> Option<String> {
    let reply = proxy
        .call_sync(
            "GetProperty",
            Some(&(XFCONF_CHANNEL.to_owned(), prop.to_owned()).to_variant()),
            gio::DBusCallFlags::NONE,
            3000,
            None::<&gio::Cancellable>,
        )
        .ok()?;
    // The reply is a tuple wrapping the value — the string either directly
    // (some xfconf builds) or boxed in a 'v' (verified 4.18: `(v)` with the
    // string inside). Handle both.
    let val = reply.child_value(0);
    if let Some(s) = val.get::<String>() {
        return Some(s);
    }
    val.get::<glib::Variant>()?.get::<String>()
}

/// Create a `/backdrop/screen0/monitor<NAME>/workspace0/last-image` property
/// for every monitor (needed on a fresh XFCE where the backdrop subtree does
/// not exist yet — nothing to SET until it is created). Returns the property
/// paths that were created. CLI path; the D-Bus path creates implicitly.
fn ensure_backdrop_properties() -> Vec<String> {
    let mut created = Vec::new();
    for name in monitor_names() {
        let prop = format!("/backdrop/screen0/monitor{name}/workspace0/last-image");
        let ok = std::process::Command::new("xfconf-query")
            .args([
                "-c", "xfce4-desktop",
                "-p", &prop,
                "-t", "string",
                "--create",
                "-s", "",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            created.push(prop);
        }
    }
    created
}

/// Monitor names as XFCE uses them in `/backdrop/screen0/monitor<name>/`:
/// from `xrandr --listmonitors` when available (native), else from the xfconf
/// `displays` channel over D-Bus (`/Default/<name>` keys) — the only option
/// inside a flatpak sandbox, which has neither xrandr nor xfconf-query.
fn monitor_names() -> Vec<String> {
    if let Ok(out) = std::process::Command::new("xrandr")
        .arg("--listmonitors")
        .output()
    {
        let names = parse_monitors(&String::from_utf8_lossy(&out.stdout));
        if !names.is_empty() {
            return names;
        }
    }
    xfconf_monitor_names_dbus()
}

/// Monitor names from the xfconf `displays` channel (`/Default/<name>/…` keys).
fn xfconf_monitor_names_dbus() -> Vec<String> {
    let Some(proxy) = xfconf_dbus_proxy() else {
        return Vec::new();
    };
    // property_base must NOT end with '/' (xfconf rejects it with
    // InvalidProperty); pass "" to enumerate the whole channel and filter.
    let reply = proxy.call_sync(
        "GetAllProperties",
        Some(&("displays".to_owned(), String::new()).to_variant()),
        gio::DBusCallFlags::NONE,
        3000,
        None::<&gio::Cancellable>,
    );
    let Ok(reply) = reply else {
        return Vec::new();
    };
    let dict = reply.child_value(0);
    let mut names = Vec::new();
    for i in 0..dict.n_children() {
        let entry = dict.child_value(i);
        if let Some(key) = entry.child_value(0).get::<String>() {
            let rest = key.strip_prefix("/Default/").unwrap_or(&key);
            let name = rest.split('/').next().unwrap_or("");
            if !name.is_empty() && !names.iter().any(|n| n == name) {
                names.push(name.to_owned());
            }
        }
    }
    names
}

/// Parse `xrandr --listmonitors` output into monitor names.
fn parse_monitors(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let t = l.trim();
            // A monitor line looks like " 0: +Virtual-1 2048/542x1152/305+0+0  Virtual-1".
            // Skip the "Monitors: N" header (starts with a letter).
            if !t.as_bytes().first().is_some_and(u8::is_ascii_digit) {
                return None;
            }
            let after = t.splitn(2, ':').nth(1)?;
            let first = after.split_whitespace().next()?;
            let name = first.trim_start_matches(['+', '-']);
            if name.is_empty() {
                None
            } else {
                Some(name.to_owned())
            }
        })
        .collect()
}

/// All backdrop `/last-image` property paths under the `xfce4-desktop`
/// channel (one per monitor/workspace).
fn list_backdrop_properties() -> Result<Vec<String>> {
    let out = std::process::Command::new("xfconf-query")
        .args(["-c", "xfce4-desktop", "-l"])
        .output()
        .context("failed to run xfconf-query")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let paths = text
        .lines()
        .map(str::trim)
        .filter(|l| l.ends_with("/last-image"))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if paths.is_empty() {
        Err(anyhow!(gettext(
            "no backdrop last-image property found in the xfce4-desktop channel"
        )))
    } else {
        Ok(paths)
    }
}

fn set_property(prop: &str, value: &str) -> Result<()> {
    let status = std::process::Command::new("xfconf-query")
        .args(["-c", "xfce4-desktop", "-p", prop, "-s", value])
        .status()
        .with_context(|| format!("failed to run xfconf-query {prop}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("xfconf-query returned a non-zero exit status for {prop}"))
    }
}

/// Replace the `value="..."` of every `last-image` property in the channel
/// XML. Returns `None` when nothing was rewritten (e.g. a config that stores
/// the wallpaper under a differently-named key), to avoid clobbering unknown
/// files.
fn rewrite_xfce_xml(raw: &str, abs: &str) -> Option<String> {
    let needle = r#"name="last-image""#;
    let mut out = String::with_capacity(raw.len() + 64);
    let mut found = false;
    let mut rest = raw;
    while let Some(pos) = rest.find(needle) {
        found = true;
        out.push_str(&rest[..pos]);
        out.push_str(needle);
        let tail = &rest[pos + needle.len()..];
        // After the name= attribute comes ` type="string" value="...">`; swap
        // the value, preserving everything else.
        let Some(vpos) = tail.find(r#"value=""#) else { break };
        out.push_str(&tail[..vpos + r#"value=""#.len()]);
        let value_rest = &tail[vpos + r#"value=""#.len()..];
        let Some(close) = value_rest.find('"') else {
            break;
        };
        out.push_str(abs);
        // Skip the original value up to and including its closing quote.
        out.push_str(&value_rest[close..]);
        rest = &value_rest[close..];
    }
    if !found {
        return None;
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Backend::parse
    // ------------------------------------------------------------------

    #[test]
    fn backend_parse_known_and_auto() {
        assert_eq!(Backend::parse("gnome"), Backend::Gnome);
        assert_eq!(Backend::parse("KDE"), Backend::Kde);
        assert_eq!(Backend::parse("plasma"), Backend::Kde);
        assert_eq!(Backend::parse("xfce"), Backend::Xfce);
        assert_eq!(Backend::parse("xubuntu"), Backend::Xfce);
    }

    #[test]
    fn backend_parse_empty_and_unknown_fall_back_to_auto() {
        assert_eq!(Backend::parse(""), Backend::Auto);
        assert_eq!(Backend::parse("auto"), Backend::Auto);
        assert_eq!(Backend::parse("cinnamon"), Backend::Auto);
        assert_eq!(Backend::parse("  "), Backend::Auto);
    }

    // ------------------------------------------------------------------
    // KDE Plasma rewrite
    // ------------------------------------------------------------------

    #[test]
    fn plasma_rewrites_first_image_wallpaper() {
        // Realistic Plasma layout: each containment is a [Containments][N]
        // group whose wallpaper lives in a nested
        // [Containments][N][Wallpaper][org.kde.image][General] group. The
        // first such group's `Image=` is rewritten.
        let fixture = r#"[Containments][1]
plugin=org.kde.image

[Containments][1][Wallpaper][org.kde.image][General]
Image=file:///old/wallpaper.jpg
FillMode=2

[Containments][2]
plugin=org.kde.slideshow

[Containments][2][Wallpaper][org.kde.image][General]
Image=file:///old2/wallpaper.jpg
FillMode=2
"#;
        let out = rewrite_plasma(fixture, "/new/img.jpg").unwrap();
        assert!(out.contains("Image=/new/img.jpg"));
        assert!(!out.contains("Image=file:///old/wallpaper.jpg"));
        // The second containment's wallpaper is left untouched.
        assert!(out.contains("Image=file:///old2/wallpaper.jpg"));
        // Regression: every later section header must survive — an earlier
        // bug dropped all headers after the first match, truncating the tail
        // of the Plasma config (which broke Plasma's wallpaper entirely).
        assert!(
            out.lines().any(|l| l == "[Containments][2]"),
            "standalone [Containments][2] header must survive"
        );
        assert!(out.contains("[Containments][2][Wallpaper][org.kde.image][General]"));
        assert!(out.contains("[Containments][1][Wallpaper][org.kde.image][General]"));
    }

    #[test]
    fn plasma_no_image_wallpaper_returns_none() {
        let raw = "[Containments][1][General]\nplugin=org.kde.jpeg\n";
        assert!(rewrite_plasma(raw, "/x.jpg").is_none());
    }

    #[test]
    fn plasma_current_reads_org_kde_image_group() {
        // Native Plasma: the current wallpaper is the first `Image=` inside an
        // org.kde.image wallpaper group; the file:// prefix is stripped so the
        // stored original is a plain local path (restorable via apply()).
        let raw = "\
[Containments][1]
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image

[Containments][1][Wallpaper][org.kde.image][General]
Image=file:///home/u/Pictures/original.jpg
FillMode=2

[Containments][2]
plugin=org.kde.panel
";
        assert_eq!(
            plasma_current_from_raw(raw).as_deref(),
            Some("/home/u/Pictures/original.jpg")
        );
    }

    #[test]
    fn plasma_current_none_when_no_image_group() {
        assert_eq!(plasma_current_from_raw("[Containments][1]\nplugin=org.kde.jpeg\n"), None);
        assert_eq!(plasma_current_from_raw(""), None);
    }

    #[test]
    fn plasma_empty_config_returns_none() {
        assert!(rewrite_plasma("", "/x.jpg").is_none());
    }

    #[test]
    fn plasma_group_without_image_inserts_key() {
        // A `[Wallpaper][org.kde.image][General]` group that has no `Image=`
        // key yet (fresh desktop) must get one — previously this yielded the
        // "No image-type wallpaper configured in Plasma" error.
        let raw = "[Containments][1]\nplugin=org.kde.image\n\n[Containments][1][Wallpaper][org.kde.image][General]\nFillMode=2\n";
        let out = rewrite_plasma(raw, "/new/img.jpg").unwrap();
        assert!(out.contains("Image=/new/img.jpg"));
        // The group header + other keys survive.
        assert!(out.contains("[Containments][1][Wallpaper][org.kde.image][General]"));
        assert!(out.contains("FillMode=2"));
        assert!(out.contains("plugin=org.kde.image"));
    }

    #[test]
    fn plasma_without_group_creates_one_for_image_containment() {
        // No image wallpaper group at all, but an image-type containment:
        // the group must be created (covers Plasma installs whose wallpaper
        // is managed elsewhere / a slideshow default).
        let raw = "[Containments][1]\nactivityId=abc\nplugin=org.kde.image\n\n[Containments][2]\nplugin=org.kde.panel\n";
        let out = rewrite_plasma(raw, "/new/img.jpg").unwrap();
        assert!(out.contains("[Containments][1][Wallpaper][org.kde.image][General]"));
        assert!(out.contains("Image=/new/img.jpg"));
        // The rest of the file is untouched (panel containment preserved).
        assert!(out.contains("[Containments][2]"));
        assert!(out.contains("plugin=org.kde.panel"));
    }

    #[test]
    fn plasma_starts_desktop_wallpaper_with_wallpaperplugin_marker() {
        // The STANDARD Plasma marker: the desktop containment has
        // `plugin=org.kde.desktopcontainment` + `wallpaperPlugin=org.kde.image`
        // — only the latter says "image wallpaper". Creating the group there
        // is what makes a fresh/different-layout Plasma install work.
        let raw = "[Containments][1]\nactivityId=abc\nplugin=org.kde.desktopcontainment\nwallpaperPlugin=org.kde.image\n\n[Containments][2]\nplugin=org.kde.panel\n";
        let out = rewrite_plasma(raw, "/new/img.jpg").unwrap();
        assert!(
            out.contains("[Containments][1][Wallpaper][org.kde.image][General]"),
            "group must be created under the wallpaperPlugin-marked containment"
        );
        assert!(out.contains("Image=/new/img.jpg"));
        assert!(out.contains("[Containments][2]"));
        assert!(out.contains("plugin=org.kde.panel"));
    }

    #[test]
    fn plasma_group_creation_keeps_containment_keys_in_place() {
        // Regression: the wallpaper group used to be inserted right after the
        // `wallpaperPlugin=` marker line, which swallowed every containment key
        // that followed it (`lastScreen=`, `activityId=`, ...) into the new
        // [Wallpaper] group — Plasma then lost the containment keys and the
        // change was ignored/reverted. The group must be created at the END of
        // the containment's key block instead.
        let raw = "\
[Containments][1]
activityId=abc
plugin=org.kde.desktopcontainment
wallpaperPlugin=org.kde.image
lastScreen=0
[Containments][2]
plugin=org.kde.panel
";
        let out = rewrite_plasma(raw, "/new/img.jpg").unwrap();
        let lines: Vec<&str> = out.lines().collect();
        let wpos = lines
            .iter()
            .position(|l| *l == "[Containments][1][Wallpaper][org.kde.image][General]")
            .expect("wallpaper group created");
        let ls_pos = lines
            .iter()
            .position(|l| *l == "lastScreen=0")
            .expect("lastScreen preserved");
        // The image line sits inside the wallpaper group…
        assert!(lines[wpos..].iter().any(|l| *l == "Image=/new/img.jpg"));
        // …but the containment key must stay OUTSIDE (before) the group.
        assert!(
            ls_pos < wpos,
            "containment keys after the plugin marker must remain in the containment, got lastScreen at {ls_pos} vs group at {wpos}:\n{out}"
        );
        // And the next containment is still intact.
        assert!(lines.iter().any(|l| *l == "[Containments][2]"));
    }

    #[test]
    fn plasma_group_creation_at_eof() {
        // Marker containment is the LAST section: the group is created at EOF,
        // not lost.
        let raw = "[Containments][1]\nactivityId=abc\nplugin=org.kde.desktopcontainment\nwallpaperPlugin=org.kde.image\nlastScreen=0\n";
        let out = rewrite_plasma(raw, "/new/img.jpg").unwrap();
        let lines: Vec<&str> = out.lines().collect();
        let wpos = lines
            .iter()
            .position(|l| *l == "[Containments][1][Wallpaper][org.kde.image][General]")
            .expect("wallpaper group created");
        let ls_pos = lines.iter().position(|l| *l == "lastScreen=0").unwrap();
        assert!(ls_pos < wpos);
        assert!(lines[wpos..].iter().any(|l| *l == "Image=/new/img.jpg"));
    }

    #[test]
    fn xfce_parse_monitors() {
        let out = parse_monitors("\
Monitors: 1
 0: +Virtual-1 2048/542x1152/305+0+0  Virtual-1
");
        assert_eq!(out, vec!["Virtual-1"]);
    }

    #[test]
    fn xfce_parse_monitors_multi_and_bare() {
        // Multiple monitors, a primary without the + marker, and the header
        // line must not leak in as a monitor name.
        let out = parse_monitors("\
Monitors: 2
 0: +HDMI-1 1920x1080+0+0  HDMI-1
 1: -DP-2 1280x1024+1920+0  DP-2
");
        assert_eq!(out, vec!["HDMI-1", "DP-2"]);
    }

    #[test]
    fn plasma_folder_view_with_lowercase_wallpaperplugin() {
        // The REAL Fedora/Plasma-6 desktop: a folder-view containment
        // (`plugin=org.kde.plasma.folder`) marked as image wallpaper with a
        // LOWERCASE `wallpaperplugin=` key, no `[Wallpaper]` group at all, and
        // a panel behind it. Regression: the old case-sensitive match on
        // `wallpaperPlugin` missed the marker and claimed "no image wallpaper".
        let raw = "\
[Containments][1]
ItemGeometries-1280x800=
activityId=8a6b7e14-3993-4dd5-9e24-036da3d45d51
formfactor=0
immutability=1
lastScreen=0
location=0
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image

[Containments][2]
activityId=
formfactor=2
immutability=1
LastScreen=0
location=4
plugin=org.kde.panel

[Containments][2][Applets][22]
immutability=1
plugin=org.kde.plasma.digitalclock
";
        let out = rewrite_plasma(raw, "/home/u/Equinox/x.jpg").unwrap();
        let lines: Vec<&str> = out.lines().collect();
        let wpos = lines
            .iter()
            .position(|l| *l == "[Containments][1][Wallpaper][org.kde.image][General]")
            .expect("wallpaper group must be created under containment 1");
        // The switch keys of containment 1 must stay OUTSIDE the new group.
        let act_pos = lines
            .iter()
            .position(|l| l.starts_with("activityId="))
            .expect("activityId preserved");
        assert!(act_pos < wpos, "activityId swallowed into the wallpaper group");
        // Panel + its digitalclock applet survive untouched.
        assert!(lines.iter().any(|l| *l == "[Containments][2]"));
        assert!(lines.iter().any(|l| *l == "plugin=org.kde.plasma.digitalclock"));
        assert!(lines[wpos..].iter().any(|l| *l == "Image=/home/u/Equinox/x.jpg"));
    }

    #[test]
    fn plasma_set_wallpaper_params_shape() {
        let p = plasma_set_wallpaper_params("file:///home/u/Pictures/Equinox/x.jpg", 0);
        // `(sa{sv}u)`: plugin id string, property dict (Image + FillMode), screen.
        assert_eq!(p.type_().to_string(), "(sa{sv}u)");
        // Plugin id + screen from the outer tuple.
        assert_eq!(p.child_value(0).get::<String>(), Some("org.kde.image".into()));
        assert_eq!(p.child_value(2).get::<u32>(), Some(0));
        // The dict is a{sv} with the two keys as boxed values.
        let dict_ty = p.child_value(1).type_().to_string();
        assert_eq!(dict_ty, "a{sv}");
        let dict = glib::VariantDict::new(Some(&p.child_value(1)));
        let image = dict.lookup_value("Image", None).expect("Image key");
        assert_eq!(image.get::<String>(), Some("file:///home/u/Pictures/Equinox/x.jpg".into()));
        let fill = dict.lookup_value("FillMode", None).expect("FillMode key");
        assert_eq!(fill.get::<u32>(), Some(2));
    }

    // ------------------------------------------------------------------
    // XFCE XML rewrite
    // ------------------------------------------------------------------

    const XFCE_FIXTURE: &str = r#"<channel name="xfce4-desktop" version="1.0">
  <property name="backdrop" type="empty">
    <property name="screen0" type="empty">
      <property name="monitor0" type="empty">
        <property name="workspace0" type="empty">
          <property name="last-image" type="string" value="/old/wallpaper.jpg"/>
        </property>
      </property>
    </property>
  </property>
</channel>
"#;

    #[test]
    fn xfce_rewrites_last_image_value() {
        let out = rewrite_xfce_xml(XFCE_FIXTURE, "/new/img.jpg").unwrap();
        assert!(out.contains(r#"value="/new/img.jpg""#));
        assert!(!out.contains(r#"value="/old/wallpaper.jpg""#));
    }

    #[test]
    fn xfce_rewrites_multiple_last_images() {
        let raw = XFCE_FIXTURE.replace(
            "workspace0",
            "workspace0",
        ); // single occurrence here
        let multi = raw.replace(
            r#"<property name="last-image" type="string" value="/old/wallpaper.jpg"/>"#,
            r#"<property name="last-image" type="string" value="/old/a.jpg"/>
       <property name="last-image" type="string" value="/old/b.jpg"/>"#,
        );
        let out = rewrite_xfce_xml(&multi, "/new/img.jpg").unwrap();
        assert_eq!(out.matches(r#"value="/new/img.jpg""#).count(), 2);
    }

    #[test]
    fn xfce_no_last_image_returns_none() {
        let raw = r#"<channel name="xfce4-desktop" version="1.0"><property name="x"/></channel>"#;
        assert!(rewrite_xfce_xml(raw, "/x.jpg").is_none());
    }
}