pub mod fullscreen_viewer;
pub mod gallery;
pub mod history;
pub mod oobe;
pub mod ratio_box;
pub mod sources;
pub mod timeline;
pub mod wallpaper;

use std::sync::OnceLock;

use gtk4::gdk::prelude::*;

/// Toast that disappears after 2 seconds (the default 5 s is too long and
/// tends to obscure the UI). The title is parsed as Pango markup by
/// libadwaita, so dynamic text (URLs with `&`, error paths, …) must be
/// escaped — otherwise the toast fails to render ("entity not terminated").
pub fn toast(text: &str) -> libadwaita::Toast {
    libadwaita::Toast::builder()
        .title(glib::markup_escape_text(text))
        .timeout(2)
        .build()
}

/// Format a unix timestamp in the LOCAL timezone.
pub fn fmt_local(ts: i64, with_seconds: bool) -> String {
    let fmt = if with_seconds {
        "%Y-%m-%d %H:%M:%S"
    } else {
        "%Y-%m-%d %H:%M"
    };
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| t.with_timezone(&chrono::Local).format(fmt).to_string())
        .unwrap_or_default()
}

/// Aspect ratio (h/w) of the primary monitor; 9/16 fallback.
///
/// The gdk4 bindings in use expose no `primary_monitor()`, so on a
/// multi-monitor X11 setup the "primary" must be derived: it is the monitor
/// whose geometry contains the origin (0,0) (the X screen's origin), falling
/// back to the first listed monitor. Picking `monitors().item(0)` blindly is
/// wrong because the list order is unspecified. The value is logged so a
/// wrong-looking tile shape can be chased from the log.
pub fn screen_ratio() -> f64 {
    const FALLBACK: f64 = 9.0 / 16.0;
    let Some(monitor) = primary_monitor() else {
        log::warn!("screen_ratio: no primary monitor; using {FALLBACK:.3}");
        return FALLBACK;
    };
    let g = monitor.geometry();
    if g.width() > 0 && g.height() > 0 {
        let ratio = g.height() as f64 / g.width() as f64;
        log::debug!(
            "screen_ratio: {}x{} on {:?} -> {ratio:.3}",
            g.width(),
            g.height(),
            monitor.model()
        );
        ratio
    } else {
        log::warn!("screen_ratio: monitor geometry 0x0 (not yet configured); using {FALLBACK:.3}");
        FALLBACK
    }
}

/// The primary monitor, if any (see [`screen_ratio`] for how "primary" is
/// derived without a `primary_monitor()` binding).
fn primary_monitor() -> Option<gtk4::gdk::Monitor> {
    let display = gtk4::gdk::Display::default()?;
    let mons = display.monitors();
    let n = mons.n_items();
    if n <= 1 {
        return mons
            .item(0)
            .and_then(|o| o.downcast::<gtk4::gdk::Monitor>().ok());
    }
    let first = mons
        .item(0)
        .and_then(|o| o.downcast::<gtk4::gdk::Monitor>().ok());
    (0..n)
        .filter_map(|i| mons.item(i).and_then(|o| o.downcast::<gtk4::gdk::Monitor>().ok()))
        .find(|m| {
            let g = m.geometry();
            g.x() <= 0 && 0 < g.x() + g.width() && g.y() <= 0 && 0 < g.y() + g.height()
        })
        .or(first)
}

/// Width × height of the primary monitor in pixels; `None` when the display
/// is not ready or reports no usable geometry. Used to auto-fill sources that
/// default to the screen size (e.g. Lorem Picsum).
pub fn primary_monitor_size() -> Option<(i32, i32)> {
    let g = primary_monitor()?.geometry();
    if g.width() > 0 && g.height() > 0 {
        Some((g.width(), g.height()))
    } else {
        None
    }
}

/// Display path: prefer the thumbnail (fast decode, smooth scrolling), fall
/// back to the original when the thumbnail is missing. Applying/deleting
/// always uses the original path.
pub fn display_path(path: &str) -> String {
    let thumb = equinox_core::storage::Storage::thumbnail_path(std::path::Path::new(path));
    if thumb.exists() {
        thumb.to_string_lossy().into_owned()
    } else {
        path.to_owned()
    }
}

/// Generate a 512 px JPEG thumbnail next to `path` (skipped when it already
/// exists), following core's conventions (`Storage::thumbnail_path`,
/// `THUMB_MAX`/`THUMB_QUALITY`). Logs and moves on when decode/save fails;
/// the original image is never touched.
///
/// **GUI-only by design**: this links gdk-pixbuf + the platform loader
/// stack (glycin on GNOME). The daemon must never link image decoders so it
/// stays lean at idle; core reads dimensions header-only (`imagesize`).
fn ensure_thumbnail(path: &std::path::Path) {
    use equinox_core::storage::{Storage, THUMB_MAX, THUMB_QUALITY};
    let thumb = Storage::thumbnail_path(path);
    if thumb.exists() {
        return;
    }
    match gdk_pixbuf::Pixbuf::from_file_at_scale(path, THUMB_MAX, THUMB_MAX, true) {
        Ok(pixbuf) => {
            if let Err(e) = pixbuf.savev(&thumb, "jpeg", &[("quality", THUMB_QUALITY)]) {
                log::warn!("failed to save thumbnail: {}: {e}", thumb.display());
            } else {
                log::info!("generated thumbnail: {}", thumb.display());
            }
        }
        Err(e) => log::warn!(
            "failed to generate thumbnail (skipped; the UI will use the original): {}: {e}",
            path.display()
        ),
    }
}

/// Background backfill of missing thumbnails for `paths` (grids call this
/// when a view loads). Thumbnail generation is GUI-side on purpose: the
/// daemon links **no** image decoders, so it stays lean at idle.
///
/// Until a thumb exists the views fall back to the original (`display_path`);
/// after this pass, later reloads / rebinds use the thumb. Missing thumbs in
/// page-based views are handled page-by-page (see `apply_carousel_tex_window`).
pub fn ensure_thumbnails_async(paths: Vec<std::path::PathBuf>) {
    std::thread::spawn(move || {
        for p in paths {
            if !equinox_core::storage::Storage::thumbnail_path(&p).exists() {
                ensure_thumbnail(&p);
            }
        }
    });
}

/// Lazy texture window for a **non-recycling** carousel (Adw::Carousel keeps
/// every page widget alive forever): only pages within `keep` of the active
/// one keep a decoded thumbnail texture. Pages farther away keep their
/// WIDGET (layout/scrolling are untouched) but drop the texture
/// (`set_filename(None)`), so memory stays bounded no matter how far the
/// user swipes — the thumbnail re-decodes when the page gets near.
///
/// `widgets`/`paths` are parallel, in carousel page order; `loaded` tracks
/// which pages currently carry a texture (avoids re-decoding the same thumb
/// on every page change). Clear `loaded` when the pages are rebuilt. Call
/// on every `page-changed`; the build should use the same window so
/// away-pages never decode at all.
pub fn apply_carousel_tex_window(
    widgets: &[crate::views::ratio_box::ScreenRatioBox],
    paths: &[String],
    active: usize,
    keep: usize,
    loaded: &mut std::collections::HashSet<usize>,
) {
    use gtk4::prelude::WidgetExt;
    for (i, frame) in widgets.iter().enumerate() {
        let Some(pic) = frame
            .first_child()
            .and_then(|c| c.downcast::<gtk4::Picture>().ok())
        else {
            continue;
        };
        if i.abs_diff(active) <= keep {
            // Page is (or is about to become) visible: load its thumbnail.
            if loaded.insert(i) {
                pic.set_filename(Some(&display_path(&paths[i])));
            }
        } else if loaded.remove(&i) {
            // Far page: release the decoded texture, keep the widget.
            pic.set_filename(None::<&str>);
        }
    }
}

/// Install the shared application CSS once (idempotent).
pub fn ensure_ui_css() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let provider = gtk4::CssProvider::new();
        provider.load_from_string(
            // Rounded clipping frame around pictures (GTK4 CSS has no
            // overflow; widgets clip via set_overflow).
            ".eq-frame { border-radius: 12px; }
             .eq-thumb { border-radius: 10px; }
             .eq-caption { padding: 6px 10px 8px 10px; border-radius: 0 0 9px 9px; }
             /* Cheap hover highlight: opacity animates without re-rasterizing
                geometry (transform scale on clipped tiles kills the frame
                rate in software rendering). */
             .eq-zoom { transition: opacity 160ms ease-out; }
             .eq-zoom:hover { opacity: 0.82; }
             .tl-blue { color: #3584e4; }
             .tl-gray { color: alpha(@window_fg_color, 0.45); }
             .tl-dash { min-width: 12px; min-height: 3px; border-radius: 2px; background-color: alpha(@window_fg_color, 0.45); margin: 0 2px; }
             /* Kill the tinted backgrounds: the GridView base layer AND its tiles */
             gridview { background: none; }
             gridview > child, gridview > child:hover,
             gridview > child:selected { background: none; box-shadow: none; outline: none; }
             .eq-tile { background: transparent; }
             .eq-missing { background-color: alpha(@window_fg_color, 0.08); }
             .eq-missing > image { opacity: 0.45; }
             .eq-menu-title { padding: 4px 8px; }
             /* Concentric round control (gallery carousel nav + wallpaper
                source switcher): an OUTER Box disc + an INNER Button core.
                Doing both layers on one Button lets the theme's button
                min-height/padding distort the inner circle into an ellipse,
                so the outer shell is a plain Box (no theme interference)
                and the clickable core is a small round Button.
                `.eq-disc`  = outer disc/capsule (window bg)
                `.eq-disc-btn` = inner circular button (button-color core) */
             /* Circular buttons & capsules: GUARANTEED opaque fill + ONE thick
                window-background border.
                - Why mix(): the fill must be opaque no matter what theme is
                  active. `@card_bg_color` has alpha in some themes (KDE's
                  Breeze-GTK style, etc.) which made the fill show the photo
                  through while the @window_bg_color border stayed opaque.
                  mix() of the two opaque base colours is opaque by
                  construction and adapts to light/dark.
                - background-image: none kills any theme-supplied translucent
                  gradient/texture the theme would paint over our fill (real
                  GtkButtons often carry one). Hover re-enables a foreground
                  wash on top — never a transparency change. */
             .eq-disc-btn {
                        background-color: mix(@window_bg_color, @window_fg_color, 0.18);
                        background-image: none;
                        border: 3px solid @window_bg_color;
                        border-radius: 999px;
                        box-shadow: none;
                        min-width: 40px; min-height: 40px; padding: 0; }
             /* Hover = a translucent FOREGROUND tint layered ON TOP of the
                opaque background (kept opaque, works in light AND dark: the
                fg colour follows the theme). */
             .eq-disc-btn:hover { background-image: linear-gradient(to bottom, alpha(@window_fg_color, 0.12), alpha(@window_fg_color, 0.12)); }
             .eq-disc-btn:active { background-image: linear-gradient(to bottom, alpha(@window_fg_color, 0.2), alpha(@window_fg_color, 0.2)); }
             .eq-disc-btn > image { color: @window_fg_color; }
             .eq-btn-core {
                        background-color: mix(@window_bg_color, @window_fg_color, 0.18);
                        background-image: none;
                        border: 3px solid @window_bg_color;
                        border-radius: 999px;
                        box-shadow: none;
                        color: @window_fg_color;
                        min-height: 40px; padding: 0 18px; font-weight: 600; }
             .eq-gradient { background-image: linear-gradient(to bottom, alpha(black, 0) 0%, alpha(black, 0.72) 100%); }
             .eq-overlay-title { color: white; font-weight: 600; }
             .eq-overlay-sub { color: alpha(white, 0.85); font-size: 90%; }
             /* Image viewer window: dark stage behind the picture, a black
                gradient caption bar along its bottom edge, and floating
                circular window controls (top-right). */
             .viewer-stage { background-color: #141414; }
             .viewer-fade { background-image: linear-gradient(to bottom, alpha(black, 0) 0%, alpha(black, 0.72) 100%);
                            padding: 56px 44px 34px; }
             .viewer-winctrl { min-width: 36px; min-height: 36px; padding: 0;
                               border-radius: 999px; background-color: alpha(black, 0.55); }
             .viewer-winctrl:hover { background-color: alpha(black, 0.8); }
             .viewer-winctrl:active { background-color: alpha(black, 0.9); }
             /* Active/enabled = the libadwaita toggled look: accent fill + accent fg \
                icon. (The old alpha(white, 0.6) pill made the white icon \
                invisible and read wrong in dark mode.) */ \
             .viewer-winctrl-active { background-color: @accent_bg_color; }
             .viewer-winctrl-active:hover { background-color: shade(@accent_bg_color, 0.88); }
             .viewer-winctrl > image { color: white; }
             .viewer-winctrl-active > image { color: @accent_fg_color; }
             /* Viewer caption: extra-large bold title, larger intro. */
             .eq-viewer-title { color: white; font-weight: 700; font-size: 32px; }
             .eq-viewer-sub { color: alpha(white, 0.9); font-size: 17px; }",
        );
        gtk4::style_context_add_provider_for_display(
            &gtk4::gdk::Display::default().expect("no display"),
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}
