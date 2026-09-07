//! Fullscreen-ish image viewer window, shared by the gallery and history
//! pages: a modal `Adw::Window` with a near-black content area showing the
//! full-resolution image, its caption (title + intro/copyright from the
//! sidecar JSON) and an "Apply wallpaper" action. Clicking a thumbnail no
//! longer applies directly; it opens this viewer, and applying happens from
//! the dedicated button (or, in the gallery carousel, the standalone Apply
//! button in the toolbar).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use libadwaita as Adw;
use libadwaita::prelude::*;

use gettextrs::gettext;

use crate::daemon_client::DaemonClient;
use crate::views::toast;

/// Open the fullscreen viewer for `path`, keeping at most one viewer open per
/// page (a previous one stored in `slot` is closed first). `on_close` runs
/// when the new viewer closes (e.g. to clear the grid selection so the same
/// card can be clicked again).
pub fn open_viewer(
    slot: &Rc<RefCell<Option<Adw::Window>>>,
    client: &Rc<DaemonClient>,
    path: &str,
    on_close: Option<Rc<dyn Fn()>>,
) {
    // Close any previous viewer first (its own close_request handler, if any,
    // clears selections).
    if let Some(old) = slot.borrow_mut().take() {
        old.close();
    }
    let win = build_window(client, path);
    if let Some(cb) = on_close {
        win.connect_close_request(move |_| {
            cb();
            glib::Propagation::Proceed
        });
    }
    *slot.borrow_mut() = Some(win.clone());
    win.present();
}

/// Read title + caption for `file` from its sidecar metadata. The caption
/// follows the gallery grid rule: description/summary, falling back to the
/// copyright (whichever exists; empty when neither does).
pub fn caption_for(file: &str) -> (String, String) {
    match photo_meta(file) {
        Some(m) => {
            let title = if m.title.is_empty() {
                file_stem(file)
            } else {
                m.title
            };
            let desc = m
                .extra
                .get("description")
                .or_else(|| m.extra.get("summary"))
                .cloned()
                .unwrap_or_default();
            let copyright = m.copyright.unwrap_or_default();
            let caption = if !desc.is_empty() {
                desc
            } else {
                copyright
            };
            (title, caption)
        }
        None => (file_stem(file), String::new()),
    }
}

/// Title from the file name (without extension).
fn file_stem(file: &str) -> String {
    std::path::Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| gettext("Untitled wallpaper"))
}

/// Read the sidecar metadata JSON next to the image (written at download).
fn photo_meta(file: &str) -> Option<equinox_core::storage::ImageMeta> {
    let json_path = std::path::Path::new(file).with_extension("json");
    std::fs::read_to_string(json_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

/// Build (but do not yet show) the viewer for one image.
///
/// Borderless window (no decorations, no title bar): the picture fills the
/// window edge to edge. The whole surface is a [`gtk4::WindowHandle`], so
/// dragging any blank area moves the window while the floating controls
/// still receive clicks. Window controls (fit/fill, maximize, close) float
/// at the top-right; the caption + Apply sit on a black gradient at the
/// bottom.
pub fn build_window(client: &Rc<DaemonClient>, file: &str) -> Adw::Window {
    let (title, caption) = caption_for(file);
    let win = Adw::Window::builder()
        .title(title.as_str())
        .modal(true)
        .default_width(1080)
        .default_height(720)
        .build();
    win.set_decorated(false);

    // Full-bleed stage: picture fills the window, letterboxed on the dark
    // background.
    let stage = gtk4::Overlay::new();
    stage.set_hexpand(true);
    stage.set_vexpand(true);
    stage.add_css_class("viewer-stage");

    let pic = gtk4::Picture::new();
    pic.set_content_fit(gtk4::ContentFit::Contain);
    pic.set_can_shrink(true);
    pic.set_hexpand(true);
    pic.set_vexpand(true);
    // The full-resolution original (not the grid thumbnail).
    pic.set_filename(Some(file));
    stage.set_child(Some(&pic));

    // ---- Bottom caption on a black gradient (like the wallpaper page's
    // text over its preview): big title + intro on the left, Apply on the
    // right. The gradient container spans the full window width.
    let cap_lbl = gtk4::Label::new(Some(&caption));
    cap_lbl.add_css_class("eq-viewer-sub");
    cap_lbl.set_wrap(true);
    cap_lbl.set_xalign(0.0);
    cap_lbl.set_visible(!caption.is_empty());
    let title_lbl = gtk4::Label::new(Some(&title));
    // Bigger, bolder than the wallpaper-page headline so it reads as the
    // viewer's own caption over the gradient.
    title_lbl.add_css_class("eq-viewer-title");
    title_lbl.set_wrap(true);
    title_lbl.set_xalign(0.0);
    let info = gtk4::Box::new(gtk4::Orientation::Vertical, 10);
    info.set_hexpand(true);
    info.set_valign(gtk4::Align::Center);
    info.append(&title_lbl);
    info.append(&cap_lbl);

    let apply_btn = gtk4::Button::with_label(&gettext("Apply wallpaper"));
    apply_btn.add_css_class("suggested-action");
    apply_btn.set_valign(gtk4::Align::Center);
    apply_btn.set_tooltip_text(Some(&gettext("Set as wallpaper")));

    let bottom = gtk4::Box::new(gtk4::Orientation::Horizontal, 20);
    bottom.add_css_class("viewer-fade");
    bottom.set_halign(gtk4::Align::Fill);
    bottom.set_valign(gtk4::Align::End);
    bottom.set_hexpand(true);
    bottom.append(&info);
    bottom.append(&apply_btn);
    stage.add_overlay(&bottom);

    // ---- Floating window controls (top-right): fit/fill toggle, fullscreen
    // and close — close sits at the far right, GNOME style. "Fullscreen"
    // uses a REAL fullscreen (covers the taskbar/panel, browser-F11 style);
    // plain maximize would leave the panel visible.
    let btn_fit = gtk4::Button::from_icon_name("zoom-fit-best-symbolic");
    btn_fit.add_css_class("viewer-winctrl");
    btn_fit.set_tooltip_text(Some(&gettext("Fill mode")));
    let btn_max = gtk4::Button::from_icon_name("window-maximize-symbolic");
    btn_max.add_css_class("viewer-winctrl");
    btn_max.set_tooltip_text(Some(&gettext("Fullscreen")));
    let btn_close = gtk4::Button::from_icon_name("window-close-symbolic");
    btn_close.add_css_class("viewer-winctrl");
    btn_close.set_tooltip_text(Some(&gettext("Close")));
    let controls = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    controls.set_halign(gtk4::Align::End);
    controls.set_valign(gtk4::Align::Start);
    controls.set_margin_top(10);
    controls.set_margin_end(10);
    controls.append(&btn_fit);
    controls.append(&btn_max);
    controls.append(&btn_close);
    stage.add_overlay(&controls);

    // Fill mode: toggle the picture between "fit" (whole image, letterboxed)
    // and "fill" (crop to the window, wallpaper-style). The icon and tooltip
    // describe the mode the button switches TO. The choice is remembered.
    let config = equinox_core::Config::new().ok();
    let fill_mode = Rc::new(Cell::new(
        config.as_ref().map(|c| c.viewer_fill()).unwrap_or(false),
    ));
    if fill_mode.get() {
        pic.set_content_fit(gtk4::ContentFit::Cover);
        btn_fit.add_css_class("viewer-winctrl-active");
        btn_fit.set_tooltip_text(Some(&gettext("Fit mode")));
    } else {
        btn_fit.set_tooltip_text(Some(&gettext("Fill mode")));
    }
    {
        let pic_f = pic.clone();
        let fit = Rc::clone(&fill_mode);
        let btn = btn_fit.clone();
        let cfg = config.clone();
        btn_fit.connect_clicked(move |_| {
            let fill = !fit.get();
            fit.set(fill);
            pic_f.set_content_fit(if fill {
                gtk4::ContentFit::Cover
            } else {
                gtk4::ContentFit::Contain
            });
            if let Some(c) = &cfg {
                c.set_viewer_fill(fill);
            }
            if fill {
                btn.add_css_class("viewer-winctrl-active");
            } else {
                btn.remove_css_class("viewer-winctrl-active");
            }
            let tip = if fill {
                gettext("Fit mode")
            } else {
                gettext("Fill mode")
            };
            btn.set_tooltip_text(Some(&tip));
        });
    }
    {
        let w = win.clone();
        btn_close.connect_clicked(move |_| w.close());
    }
    {
        let w = win.clone();
        let max_icon = btn_max.clone();
        btn_max.connect_clicked(move |_| {
            if w.is_fullscreen() {
                w.unfullscreen();
                max_icon.set_icon_name("window-maximize-symbolic");
                max_icon.remove_css_class("viewer-winctrl-active");
            } else {
                w.fullscreen();
                max_icon.set_icon_name("window-restore-symbolic");
                max_icon.add_css_class("viewer-winctrl-active");
            }
        });
    }

    // Toast overlay inside the window so apply feedback is visible without a
    // page-level toast.
    let toasts = Adw::ToastOverlay::new();
    toasts.set_child(Some(&stage));

    // Whole surface doubles as the window handle: dragging any blank area
    // (picture, gradient padding, …) moves the window; interactive children
    // (buttons) are still clickable.
    let handle = gtk4::WindowHandle::new();
    handle.set_child(Some(&toasts));
    win.set_content(Some(&handle));

    // ---- Auto-hidden UI: with no pointer activity the controls + caption
    // fade out leaving only the picture; any movement fades them back in.
    // Fade-in is intentionally slower than fade-out. Hidden layers are
    // unmapped so they never swallow events, and the pointer is hidden too.
    let ui_layers: Rc<Vec<gtk4::Widget>> =
        Rc::new(vec![bottom.clone().upcast(), controls.clone().upcast()]);
    let ui_gen: Rc<Cell<u64>> = Rc::new(Cell::new(0));
    // (fade-in ms, fade-out ms) — fade-in stays a bit longer than fade-out.
    let fade_ms: (u64, u64) = (400, 300);

    let hide_cursor = gtk4::gdk::Cursor::from_name("none", None);
    let show_cursor = gtk4::gdk::Cursor::from_name("default", None);

    let show_ui: Rc<dyn Fn()> = {
        let layers = Rc::clone(&ui_layers);
        let gen = Rc::clone(&ui_gen);
        let stage_c = stage.clone();
        let cursor = show_cursor.clone();
        Rc::new(move || {
            let g = gen.get() + 1;
            gen.set(g);
            stage_c.set_cursor(cursor.as_ref());
            for w in layers.iter() {
                w.set_visible(true);
                w.set_sensitive(true);
                fade_to(w.clone(), 1.0, fade_ms.0, Rc::clone(&gen), g);
            }
        })
    };
    let hide_ui: Rc<dyn Fn()> = {
        let layers = Rc::clone(&ui_layers);
        let gen = Rc::clone(&ui_gen);
        let stage_c = stage.clone();
        let cursor = hide_cursor.clone();
        Rc::new(move || {
            let g = gen.get() + 1;
            gen.set(g);
            stage_c.set_cursor(cursor.as_ref());
            for w in layers.iter() {
                w.set_sensitive(false);
                fade_to(w.clone(), 0.0, fade_ms.1, Rc::clone(&gen), g);
            }
        })
    };

    // Poke: reveal the UI and restart the inactivity timer (each poke is
    // generation-tagged so a stale timer can never hide after a newer poke).
    // Leaving the window only restarts the timer (the UI keeps its state).
    let restart_timer: Rc<dyn Fn()> = {
        let hide = Rc::clone(&hide_ui);
        let poke_gen: Rc<Cell<u64>> = Rc::new(Cell::new(0));
        Rc::new(move || {
            let g = poke_gen.get() + 1;
            poke_gen.set(g);
            let pg = Rc::clone(&poke_gen);
            let h = Rc::clone(&hide);
            glib::timeout_add_local_once(std::time::Duration::from_millis(3000), move || {
                if pg.get() == g {
                    h();
                }
            });
        })
    };
    let poke: Rc<dyn Fn()> = {
        let show = Rc::clone(&show_ui);
        let timer = Rc::clone(&restart_timer);
        Rc::new(move || {
            show();
            timer();
        })
    };

    // Pointer movement reveals + restarts the timer. Only MOTION pokes: a
    // spurious `enter` is synthesized when a hidden layer unmaps under a
    // stationary pointer, which would immediately re-show the UI (flicker
    // loop). Real pointer entry always comes with movement, so motion alone
    // is sufficient. The controller sits on the topmost full-window widget
    // (the handle) AND on the stage (the WindowHandle consumes pointer events
    // for dragging; poke is idempotent so double firing is harmless).
    let motion = gtk4::EventControllerMotion::new();
    let poke_motion = Rc::clone(&poke);
    motion.connect_motion(move |_, _, _| poke_motion());
    let leave_timer = Rc::clone(&restart_timer);
    motion.connect_leave(move |_| leave_timer());
    handle.add_controller(motion);
    let motion2 = gtk4::EventControllerMotion::new();
    let poke_motion2 = Rc::clone(&poke);
    motion2.connect_motion(move |_, _, _| poke_motion2());
    let leave_timer2 = Rc::clone(&restart_timer);
    motion2.connect_leave(move |_| leave_timer2());
    stage.add_controller(motion2);
    // Initial reveal + schedule the first auto-hide once mapped.
    win.connect_map({
        let poke_map = Rc::clone(&poke);
        move |_| poke_map()
    });

    // Remember the fullscreen state on close (fill mode is saved on toggle).
    {
        let cfg = config.clone();
        win.connect_close_request(move |w| {
            if let Some(c) = &cfg {
                c.set_viewer_fullscreen(w.is_fullscreen());
            }
            glib::Propagation::Proceed
        });
    }
    // Restore the last fullscreen state (icon + active state reflect it).
    if config.as_ref().map(|c| c.viewer_fullscreen()).unwrap_or(false) {
        win.fullscreen();
        btn_max.set_icon_name("window-restore-symbolic");
        btn_max.add_css_class("viewer-winctrl-active");
    }

    // ApplyFile also switches the rule to the image's source in manual mode
    // (same semantics as the wallpaper page browse).
    {
        let c = Rc::clone(client);
        let toasts_c = toasts.clone();
        let f = file.to_owned();
        apply_btn.connect_clicked(move |btn| {
            let btn_c = btn.clone();
            btn_c.set_sensitive(false);
            let c2 = c.clone();
            let t2 = toasts_c.clone();
            let f2 = f.clone();
            glib::spawn_future_local(async move {
                let r = c2.apply_file(&f2).await;
                let msg = if r.is_ok() {
                    gettext("Wallpaper applied")
                } else {
                    gettext("Failed to apply wallpaper")
                };
                t2.add_toast(toast(&msg));
                if r.is_err() {
                    let btn3 = btn_c.clone();
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(900),
                        move || btn3.set_sensitive(true),
                    );
                }
            });
        });
    }

    win
}

/// Animate a widget's opacity towards `to` over `duration_ms`, driven by the
/// frame clock. `gen`/`my` are a generation counter: a newer animation bumps
/// the counter so stale ticks stop. When hidden the widget is unmapped at the
/// end (no invisible interactive area, no swallowed events).
fn fade_to(widget: gtk4::Widget, to: f64, duration_ms: u64, gen: Rc<Cell<u64>>, my: u64) {
    let from = widget.opacity();
    if (from - to).abs() < 0.001 {
        widget.set_visible(to > 0.5);
        widget.set_sensitive(to > 0.5);
        return;
    }
    // frame_time() is in microseconds.
    let duration = duration_ms as f64 * 1000.0;
    let start = Cell::new(None::<f64>);
    widget.add_tick_callback(move |w, clock| {
        if gen.get() != my {
            return glib::ControlFlow::Break;
        }
        let now = clock.frame_time() as f64;
        let t0 = match start.get() {
            Some(t) => t,
            None => {
                start.set(Some(now));
                now
            }
        };
        let p = ((now - t0) / duration).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - p) * (1.0 - p);
        w.set_opacity(from + (to - from) * eased);
        if p >= 1.0 {
            w.set_sensitive(to > 0.5);
            w.set_visible(to > 0.5);
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}
