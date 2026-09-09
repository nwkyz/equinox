//! Wallpaper page.
//!
//! - The top carousel browses the **wallpaper history** (newest first,
//!   across all sources). Swiping to a card applies that image; the daemon
//!   then also switches the rule to the image's source in manual mode.
//! - The bottom floating bar switches the **wallpaper source** (prev/next
//!   round buttons at the edges, applying each source's remembered mode);
//!   the middle capsule cycles latest/random/manual for the current source.
//! - The source-name capsule over the image stays exactly centered: end
//!   buttons hide via opacity (never `set_visible`, which would collapse
//!   them and shift the layout), and a fixed-width holder centers the
//!   capsule while its width animates.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use gio::prelude::*;

use libadwaita as Adw;
use libadwaita::prelude::*;

use gettextrs::gettext;

use equinox_core::config::Config;
use equinox_core::source::{by_id, registry};
use equinox_core::storage::ImageMeta;

use crate::daemon_client::{DaemonClient, GalleryEntry};
use crate::views::ratio_box::ScreenRatioBox;
use crate::views::{apply_carousel_tex_window, screen_ratio};

/// One carousel page: an entry of the wallpaper history.
struct HistPage {
    file: String,
    source: String,
    title: String,
}

/// Fixed-width container: measures only its own size_request and clips the
/// child, so animating size_request produces a smooth width change (a plain
/// Box would never shrink below the child's natural width — the reason the
/// capsule width previously snapped instead of animating).
mod capsule_box {
    use glib::subclass::prelude::*;
    use gtk4::prelude::WidgetExt;
    use gtk4::subclass::prelude::*;

    #[derive(Default)]
    pub struct CapsuleBox;

    #[glib::object_subclass]
    impl ObjectSubclass for CapsuleBox {
        const NAME: &'static str = "EquinoxCapsuleBox";
        type Type = super::CapsuleBox;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for CapsuleBox {}

    impl WidgetImpl for CapsuleBox {
        fn measure(&self, orientation: gtk4::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let w = self.obj().width_request().max(0);
            match orientation {
                gtk4::Orientation::Horizontal => (w, w, -1, -1),
                gtk4::Orientation::Vertical => {
                    let h = self
                        .obj()
                        .first_child()
                        .map(|c| c.measure(gtk4::Orientation::Vertical, for_size).1)
                        .unwrap_or(0);
                    (h, h, -1, -1)
                }
                _ => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            if let Some(c) = self.obj().first_child() {
                c.allocate(width, height, -1, None);
            }
        }
    }
}

glib::wrapper! {
    pub struct CapsuleBox(ObjectSubclass<capsule_box::CapsuleBox>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl CapsuleBox {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn set_child(&self, child: Option<&impl IsA<gtk4::Widget>>) {
        if let Some(c) = self.first_child() {
            c.unparent();
        }
        if let Some(c) = child {
            c.set_parent(self);
        }
    }
}

pub fn build(
    client: &Rc<DaemonClient>,
    config: &Config,
    toast: &Adw::ToastOverlay,
) -> gtk4::Widget {
    views_css();

    // The whole page (image + text) lives in a scroll area; oversized content
    // scrolls.
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    vbox.set_margin_top(24);
    // Extra bottom space so the floating capsule bar doesn't cover content.
    vbox.set_margin_bottom(88);
    vbox.set_margin_start(24);
    vbox.set_margin_end(24);
    vbox.set_hexpand(true);

    // ---- Carousel: one page per history entry (newest first) ----
    let ratio = screen_ratio();
    let carousel = Adw::Carousel::new();
    carousel.set_hexpand(true);

    // Shared UI state
    let pages: Rc<RefCell<Vec<Rc<HistPage>>>> = Rc::new(RefCell::new(Vec::new()));
    let page_widgets: Rc<RefCell<Vec<ScreenRatioBox>>> = Rc::new(RefCell::new(Vec::new()));
    // Which carousel pages currently carry a decoded thumbnail texture
    // (lazy texture window; see apply_carousel_tex_window).
    let tex_loaded: Rc<RefCell<HashSet<usize>>> = Rc::new(RefCell::new(HashSet::new()));
    // Current source's photo list (newest first) + current carousel index.
    let images: Rc<RefCell<Vec<GalleryEntry>>> = Rc::new(RefCell::new(Vec::new()));
    let cur_idx: Rc<Cell<usize>> = Rc::new(Cell::new(0));
    // Last file known to be applied — used to skip self-triggered applies.
    let applied_file: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    // True while the UI is programmatically rebuilding/repositioning the
    // carousel: page_changed signals fired during that window must NOT
    // trigger applies (otherwise apply → WallpaperChanged → rebuild →
    // page_changed → apply … ping-pongs forever).
    let syncing: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    // ---- Image-area overlay: step through THIS source's photos + mode ----
    let btn_img_prev = gtk4::Button::from_icon_name("go-previous-symbolic");
    btn_img_prev.add_css_class("wallpaper-overlay-btn");
    btn_img_prev.set_valign(gtk4::Align::Center);
    btn_img_prev.set_tooltip_text(Some(&gettext("Previous image")));
    let mode_btn = gtk4::Button::new();
    mode_btn.add_css_class("suggested-action");
    mode_btn.add_css_class("mode-capsule");
    mode_btn.set_valign(gtk4::Align::Center);
    // Exactly as tall as the neighboring round buttons.
    mode_btn.set_size_request(-1, 40);
    mode_btn.set_label(&mode_label("latest"));
    let btn_img_next = gtk4::Button::from_icon_name("go-next-symbolic");
    btn_img_next.add_css_class("wallpaper-overlay-btn");
    btn_img_next.set_valign(gtk4::Align::Center);
    btn_img_next.set_tooltip_text(Some(&gettext("Next image")));

    // Symmetric strip: end buttons are NEVER removed from the layout — they
    // hide via opacity so both sides keep identical widths.
    let strip = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    strip.set_halign(gtk4::Align::Center);
    strip.set_valign(gtk4::Align::End);
    strip.append(&btn_img_prev);
    strip.append(&mode_btn);
    strip.append(&btn_img_next);

    // Fade in/out of the whole strip
    let revealer = gtk4::Revealer::new();
    revealer.set_transition_type(gtk4::RevealerTransitionType::Crossfade);
    revealer.set_transition_duration(150);
    revealer.set_halign(gtk4::Align::Center);
    revealer.set_valign(gtk4::Align::End);
    revealer.set_margin_bottom(12);
    revealer.set_child(Some(&strip));

    let carousel_overlay = gtk4::Overlay::new();
    carousel_overlay.set_child(Some(&carousel));
    carousel_overlay.add_overlay(&revealer);

    // Show/hide: any trigger reveals + restarts the 3 s hide timer
    // (generation counter, so a stale timer can't hide after a newer show).
    let show_gen: Rc<Cell<u32>> = Rc::new(Cell::new(0));
    let show_src_controls: Rc<dyn Fn()> = Rc::new({
        let revealer = revealer.clone();
        let gen = Rc::clone(&show_gen);
        move || {
            revealer.set_reveal_child(true);
            let g = gen.get() + 1;
            gen.set(g);
            let gen2 = Rc::clone(&gen);
            let r = revealer.clone();
            glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
                if gen2.get() == g {
                    r.set_reveal_child(false);
                }
            });
        }
    });
    show_src_controls();
    let motion = gtk4::EventControllerMotion::new();
    let show_hover = Rc::clone(&show_src_controls);
    motion.connect_enter(move |_, _, _| show_hover());
    let show_motion = Rc::clone(&show_src_controls);
    motion.connect_motion(move |_, _, _| show_motion());
    carousel_overlay.add_controller(motion);

    vbox.append(&carousel_overlay);

    // ---- Headline: the wallpaper title directly ----
    let title_label = gtk4::Label::new(Some(&gettext("Loading…")));
    title_label.add_css_class("title-1");
    title_label.set_wrap(true);
    title_label.set_xalign(0.0);
    vbox.append(&title_label);

    let meta_label = gtk4::Label::new(Some(""));
    meta_label.add_css_class("dim-label");
    meta_label.set_wrap(true);
    meta_label.set_xalign(0.0);
    vbox.append(&meta_label);

    let date_label = gtk4::Label::new(Some(""));
    date_label.add_css_class("dim-label");
    date_label.set_xalign(0.0);
    vbox.append(&date_label);

    // "Update now" shown only when the current source has no wallpapers yet,
    // so the empty state is actionable instead of a dead blank.
    let update_btn = gtk4::Button::with_label(&gettext("Update now"));
    update_btn.add_css_class("suggested-action");
    update_btn.set_halign(gtk4::Align::Start);
    update_btn.set_margin_top(6);
    update_btn.set_visible(false);
    vbox.append(&update_btn);
    {
        let c = Rc::clone(client);
        let t = toast.clone();
        let cfg = config.clone();
        update_btn.connect_clicked(move |_| {
            let src = cfg.wallpaper_source();
            if src.is_empty() {
                return;
            }
            let c2 = Rc::clone(&c);
            let t2 = t.clone();
            glib::spawn_future_local(async move {
                if let Err(e) = c2.fetch_now(&src).await {
                    t2.add_toast(crate::views::toast(&format!(
                        "{}: {e}",
                        gettext("Update failed")
                    )));
                }
                // On success the daemon downloads in the background; ImageAdded
                // (subscribed below) reloads this page automatically.
            });
        });
    }

    // ---- Bottom floating bar: SOURCE switching (prev | name | next) ----
    // Round controls: single-layer elements — each circle/capsule is ONE
    // widget with a THICK window-background border (`.eq-disc-btn` /
    // `.eq-btn-core`, see mod.rs). No two-layer "concentric" disc: GtkButtons
    // get stretched by an outer Box (off-centre core, ring too big).
    let btn_src_prev = gtk4::Button::from_icon_name("go-previous-symbolic");
    btn_src_prev.add_css_class("eq-disc-btn");
    btn_src_prev.set_valign(gtk4::Align::Center);
    btn_src_prev.set_tooltip_text(Some(&gettext("Previous source")));

    let btn_src_next = gtk4::Button::from_icon_name("go-next-symbolic");
    btn_src_next.add_css_class("eq-disc-btn");
    btn_src_next.set_valign(gtk4::Align::Center);
    btn_src_next.set_tooltip_text(Some(&gettext("Next source")));
    let src_name = gtk4::Label::new(Some(&gettext("None")));
    src_name.set_valign(gtk4::Align::Center);
    src_name.set_halign(gtk4::Align::Center);

    // The source rotation starts with "None"; measure every slot (None +
    // all real sources) so the capsule fits the widest name.
    let mut max_name = 0;
    for name in std::iter::once(gettext("None"))
        .chain(registry().into_iter().map(|s| s.display_name()))
    {
        src_name.set_label(&name);
        let (_, n, _, _) = src_name.measure(gtk4::Orientation::Horizontal, -1);
        max_name = max_name.max(n.max(0));
    }
    src_name.set_label(&gettext("None"));

    // Single-layer name capsule (thick border from `.eq-btn-core`); the
    // CapsuleBox only clips and animates the width.
    let name_chip = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    name_chip.add_css_class("eq-btn-core");
    name_chip.set_valign(gtk4::Align::Center);
    name_chip.set_halign(gtk4::Align::Center);
    name_chip.append(&src_name);

    let src_capsule = CapsuleBox::new();
    src_capsule.set_overflow(gtk4::Overflow::Hidden);
    // chip padding 18×2 + border 3×2.
    src_capsule.set_size_request(max_name + 42, 46);
    src_capsule.set_valign(gtk4::Align::Center);
    src_capsule.set_child(Some(&name_chip));

    // Three capsules (round button | name pill | round button): all single
    // elements of the same height → always aligned, no concentricity.
    let bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    bar.set_halign(gtk4::Align::Center);
    bar.append(&btn_src_prev);
    bar.append(&src_capsule);
    bar.append(&btn_src_next);

    // Wide windows center content (Clamp); narrow windows scroll.
    let clamp = Adw::Clamp::new();
    clamp.set_maximum_size(550);
    clamp.set_child(Some(&vbox));
    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroll.set_child(Some(&clamp));

    // The capsule bar floats above the scrollable content, doesn't scroll
    bar.set_valign(gtk4::Align::End);
    bar.set_margin_bottom(24);
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&scroll));
    overlay.add_overlay(&bar);

    // Initial boundary state; refresh corrects once data arrives.
    set_btn_on(&btn_img_prev, false);
    set_btn_on(&btn_img_next, false);
    set_btn_on(&btn_src_prev, false);
    set_btn_on(&btn_src_next, false);

    // Switching to this page shows the source controls briefly.
    let show_map = Rc::clone(&show_src_controls);
    overlay.connect_map(move |_| show_map());

    // ---- Reload: fetch history + status, rebuild pages, sync position ----
    let reload = {
        let client = Rc::clone(client);
        let config = config.clone();
        let pages = Rc::clone(&pages);
        let widgets = Rc::clone(&page_widgets);
        let tex_loaded = Rc::clone(&tex_loaded);
        let applied = Rc::clone(&applied_file);
        let carousel = carousel.clone();
        let t = title_label.clone();
        let m = meta_label.clone();
        let d = date_label.clone();
        let name_lbl = src_name.clone();
        let mb = mode_btn.clone();
        let images_st = Rc::clone(&images);
        let cur_idx_st = Rc::clone(&cur_idx);
        let biprev = btn_img_prev.clone();
        let binext = btn_img_next.clone();
        let bprev = btn_src_prev.clone();
        let bnext = btn_src_next.clone();
        let syncing = Rc::clone(&syncing);
        let capsule_c = src_capsule.clone();
        let unbtn = update_btn.clone();
        let gen_c = Rc::new(Cell::new(0u64));
        let reload_gen = Rc::new(Cell::new(0u64));
        move || {
            let client = Rc::clone(&client);
            let config = config.clone();
            let pages = Rc::clone(&pages);
            let widgets = Rc::clone(&widgets);
            let tex_loaded = Rc::clone(&tex_loaded);
            let applied = Rc::clone(&applied);
            let carousel = carousel.clone();
            let t = t.clone();
            let m = m.clone();
            let d = d.clone();
            let name_lbl = name_lbl.clone();
            let mb = mb.clone();
            let bprev = bprev.clone();
            let bnext = bnext.clone();
            let images_st = Rc::clone(&images_st);
            let cur_idx_st = Rc::clone(&cur_idx_st);
            let biprev = biprev.clone();
            let binext = binext.clone();
            let syncing = Rc::clone(&syncing);
            let capsule_c = capsule_c.clone();
            let unbtn = unbtn.clone();
            let gen_c = gen_c.clone();
            let reload_gen = Rc::clone(&reload_gen);
            glib::spawn_future_local(async move {
                let my_gen = reload_gen.get() + 1;
                reload_gen.set(my_gen);
                syncing.set(true);
                // Current source + applied file: prefer the live daemon status, fall back
                // to the local config when the daemon is busy/offline so we
                // don't wrongly collapse to an empty page.
                let (source, current) = match client.status().await {
                    Ok((s, _, f)) => (s, f),
                    Err(_) => (config.wallpaper_source(), String::new()),
                };
                // Photo list. A transient D-Bus failure (e.g. the daemon's
                // main loop is briefly tied up by an update task) must NOT
                // flip the page to "no wallpapers": keep the current view
                // instead, a later signal will retry the reload.
                let imgs = if source.is_empty() {
                    Vec::new()
                } else {
                    match client.source_images(&source).await {
                        Ok(v) => v,
                        Err(e) => {
                            log::warn!("wallpaper refresh for {source} failed: {e:#}; keeping current view");
                            syncing.set(false);
                            return;
                        }
                    }
                };
                *images_st.borrow_mut() = imgs.clone();
                *applied.borrow_mut() = current.clone();

                // Rebuild carousel children for THIS source, ordered
                // OLDEST -> NEWEST (newest at the far right): the left arrow
                // then steps to older pages sliding in from the left, the
                // right arrow to newer ones from the right.
                while carousel.n_pages() > 0 {
                    let page = carousel.nth_page(0);
                    if let Some(f) = page.downcast_ref::<ScreenRatioBox>() {
                        f.take_child();
                    }
                    carousel.remove(&page);
                }
                // Active page (currently applied file, else newest). Known up
                // front so a single apply() right after the build lazy-loads
                // exactly the nearby thumbnails; far pages keep an empty
                // Picture until they get near (see apply_carousel_tex_window).
                // Any wait for a still-unknown index would block the UI.
                let idx0 = imgs
                    .iter()
                    .rev()
                    .position(|(f, ..)| *f == current)
                    .unwrap_or(imgs.len().saturating_sub(1));
                tex_loaded.borrow_mut().clear();
                let mut new_pages = Vec::new();
                let mut new_widgets = Vec::new();
                for (file, title, _src, _ts) in imgs.iter().rev() {
                    let frame = ScreenRatioBox::new(ratio);
                    frame.add_css_class("wallpaper-preview");
                    frame.set_overflow(gtk4::Overflow::Hidden);
                    frame.set_hexpand(true);
                    let pic = gtk4::Picture::new();
                    pic.set_content_fit(gtk4::ContentFit::Cover);
                    pic.set_can_shrink(true);
                    pic.set_hexpand(true);
                    pic.set_vexpand(true);
                    // No filename here: apply() sets the texture window.
                    frame.set_child(Some(&pic));
                    carousel.append(&frame);
                    new_widgets.push(frame);
                    new_pages.push(Rc::new(HistPage {
                        file: file.clone(),
                        source: source.clone(),
                        title: title.clone(),
                    }));
                }
                *pages.borrow_mut() = new_pages;
                *widgets.borrow_mut() = new_widgets;

                let n = pages.borrow().len();
                if n == 0 {
                    cur_idx_st.set(0);
                    set_btn_on(&biprev, false);
                    set_btn_on(&binext, false);
                    if source.is_empty() {
                        // "None" source: Equinox leaves the wallpaper alone.
                        t.set_text(&gettext("None"));
                        m.set_text(&gettext(
                            "Equinox will not change your wallpaper while no source is selected.",
                        ));
                        d.set_text("");
                        unbtn.set_visible(false);
                    } else {
                        t.set_text(&gettext("No wallpapers in this source yet"));
                        m.set_text(&gettext(
                            "Switch sources below or run an update to download the first images",
                        ));
                        d.set_text("");
                        unbtn.set_visible(true);
                    }
                    // Source capsule + arrows: "None" is slot 0; each real
                    // source sits at its registry index + 1.
                    let all: Vec<String> = std::iter::once(gettext("None"))
                        .chain(registry().into_iter().map(|s| s.display_name()))
                        .collect();
                    let pos = if source.is_empty() {
                        0
                    } else {
                        registry()
                            .iter()
                            .position(|s| s.id() == source)
                            .map(|i| i + 1)
                            .unwrap_or(1)
                    };
                    name_lbl.set_label(&all[pos]);
                    set_btn_on(&bprev, pos > 0);
                    set_btn_on(&bnext, pos + 1 < all.len());
                    return;
                }
                // A source with images never shows the update-nothing state.
                unbtn.set_visible(false);
                cur_idx_st.set(idx0);
                // Lazy texture window: only the active page ± 2 get decoded.
                {
                    let files: Vec<String> =
                        pages.borrow().iter().map(|p| p.file.clone()).collect();
                    apply_carousel_tex_window(
                        &widgets.borrow(),
                        &files,
                        idx0,
                        2,
                        &mut tex_loaded.borrow_mut(),
                    );
                }

                // Image arrows: LEFT = older (i-1), RIGHT = newer (i+1).
                set_btn_on(&biprev, idx0 > 0);
                set_btn_on(&binext, idx0 + 1 < n);
                sync_page_ui(
                    &pages, &config, idx0, &t, &m, &d, &name_lbl, &capsule_c, &gen_c, &mb, &bprev,
                    &bnext,
                );
                // Scroll only once the target page is ALLOCATED: scroll_to on
                // a just-appended page silently no-ops (idles also run before
                // the frame's allocation pass), leaving the carousel parked
                // on page 0 — the OLDEST image (pages run oldest→newest).
                // Wait indefinitely until the page is mapped AND has real
                // geometry: when an update lands while the user is on another
                // tab, the carousel is unmapped and must be positioned at the
                // moment the tab is opened. A newer reload supersedes this
                // poll. Keep swallowing page_changed until one tick past the
                // scroll, so its deferred emission can't be mistaken for a
                // user pick (that applied the oldest image and flipped the
                // rule to manual).
                let car_sync = carousel.clone();
                let widgets_sync = Rc::clone(&widgets);
                let syncing_sync = Rc::clone(&syncing);
                let poll_gen = Rc::clone(&reload_gen);
                glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
                    if poll_gen.get() != my_gen {
                        return glib::ControlFlow::Break;
                    }
                    let widgets = widgets_sync.borrow();
                    let Some(w) = widgets.get(idx0) else {
                        return glib::ControlFlow::Break;
                    };
                    if !w.is_mapped() || w.width() == 0 {
                        return glib::ControlFlow::Continue;
                    }
                    car_sync.scroll_to(w, false);
                    drop(widgets);
                    let syncing2 = Rc::clone(&syncing_sync);
                    glib::idle_add_local_once(move || syncing2.set(false));
                    glib::ControlFlow::Break
                });
            });
        }
    };

    // ---- Carousel page change: update info; debounced apply of that card ----
    let page_gen: Rc<Cell<u32>> = Rc::new(Cell::new(0));
    carousel.connect_page_changed({
        let pages = Rc::clone(&pages);
        let gen = Rc::clone(&page_gen);
        let client_pc = Rc::clone(client);
        let t = title_label.clone();
        let m = meta_label.clone();
        let d = date_label.clone();
        let show_src = Rc::clone(&show_src_controls);
        let applied = Rc::clone(&applied_file);
        let tst = toast.clone();
        let cur_idx_pc = Rc::clone(&cur_idx);
        let images_pc = Rc::clone(&images);
        let biprev_pc = btn_img_prev.clone();
        let binext_pc = btn_img_next.clone();
        let syncing_pc = Rc::clone(&syncing);
        let widgets_pc = Rc::clone(&page_widgets);
        let tex_loaded_pc = Rc::clone(&tex_loaded);
        move |_car, idx| {
            // Lazy textures: keep thumbnails only for the active page ± 2,
            // release far pages (widgets stay, so scrolling is unaffected).
            {
                let pages_b = pages.borrow();
                let files: Vec<String> = pages_b.iter().map(|p| p.file.clone()).collect();
                apply_carousel_tex_window(
                    &widgets_pc.borrow(),
                    &files,
                    idx as usize,
                    2,
                    &mut tex_loaded_pc.borrow_mut(),
                );
            }
            // Programmatic repositioning: never treat as a user pick.
            if syncing_pc.get() {
                return;
            }
            show_src();
            let page = match pages.borrow().get(idx as usize) {
                Some(p) => Rc::clone(p),
                None => return,
            };
            set_info(&t, &m, &d, &page);
            // Image step arrows follow the position inside this source.
            cur_idx_pc.set(idx as usize);
            let n_imgs = images_pc.borrow().len();
            set_btn_on(&biprev_pc, idx > 0);
            set_btn_on(&binext_pc, idx as usize + 1 < n_imgs);

            let g = gen.get() + 1;
            gen.set(g);
            let file = page.file.clone();
            let gen2 = Rc::clone(&gen);
            let c2 = Rc::clone(&client_pc);
            let applied2 = Rc::clone(&applied);
            let toast_d = tst.clone();
            let src2 = page.source.clone();
            let syncing_d = Rc::clone(&syncing_pc);
            glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                if gen2.get() != g || syncing_d.get() {
                    return;
                }
                // Already applied (programmatic scroll / signal echo)?
                if *applied2.borrow() == file {
                    return;
                }
                // ApplyFile also switches the rule to this source (manual).
                let c3 = Rc::clone(&c2);
                let f3 = file.clone();
                let src3 = src2.clone();
                glib::spawn_future_local(async move {
                    if let Err(e) = c3.apply_file(&f3).await {
                        toast_d.add_toast(crate::views::toast(&format!(
                            "{}: {e}",
                            gettext("Failed to switch")
                        )));
                    }
                    let _ = src3;
                });
            });
        }
    });

    // ---- Bottom bar actions ----
    // Prev/next SOURCE: apply that source's remembered mode immediately.
    let pages_sw = Rc::clone(&pages);
    let reload_sw = reload.clone();
    let switch_source =
        move |client: &Rc<DaemonClient>, config: &Config, toast: &Adw::ToastOverlay, delta: i32| {
            let reg = registry();
            let n_slots = reg.len() + 1; // + the "None" slot at index 0
            let cur = config.wallpaper_source();
            let pos = if cur.is_empty() {
                0 // "None"
            } else {
                reg.iter()
                    .position(|s| s.id() == cur)
                    .map(|i| i + 1)
                    .or_else(|| {
                        pages_sw
                            .borrow()
                            .first()
                            .and_then(|p| reg.iter().position(|s| s.id() == p.source))
                            .map(|i| i + 1)
                    })
                    .unwrap_or(1)
            };
            let target = pos as i32 + delta;
            if target < 0 || target as usize >= n_slots {
                return;
            }
            let c = Rc::clone(client);
            let t = toast.clone();
            let rr = reload_sw.clone();
            if target == 0 {
                // "None": stop managing the wallpaper — the daemon restores
                // the user's original wallpaper captured before the first
                // Equinox apply.
                glib::spawn_future_local(async move {
                    if let Err(e) = c.set_wallpaper_source("", "latest").await {
                        t.add_toast(crate::views::toast(&format!(
                            "{}: {e}",
                            gettext("Failed to set")
                        )));
                    } else {
                        rr();
                    }
                });
                return;
            }
            let id = reg[target as usize - 1].id();
            let mode = config.source_mode(id);
            let id = id.to_owned();
            glib::spawn_future_local(async move {
                if let Err(e) = c.set_wallpaper_source(&id, &mode).await {
                    t.add_toast(crate::views::toast(&format!(
                        "{}: {e}",
                        gettext("Failed to set")
                    )));
                } else {
                    // Reflect the new source right away — including its
                    // empty state with the Update button.
                    rr();
                }
            });
        };
    {
        let c = Rc::clone(client);
        let cfg = config.clone();
        let t = toast.clone();
        let sw = switch_source.clone();
        btn_src_prev.connect_clicked(move |_| sw(&c, &cfg, &t, -1));
    }
    {
        let c = Rc::clone(client);
        let cfg = config.clone();
        let t = toast.clone();
        btn_src_next.connect_clicked(move |_| switch_source(&c, &cfg, &t, 1));
    }

    // Image step buttons: move within THIS source's photos (the page_changed
    // debounce then applies the newly shown image).
    {
        let car = carousel.clone();
        let widgets = Rc::clone(&page_widgets);
        let idx = Rc::clone(&cur_idx);
        btn_img_prev.connect_clicked(move |_| {
            let i = idx.get();
            if i > 0 {
                if let Some(w) = widgets.borrow().get(i - 1) {
                    car.scroll_to(w, true);
                }
            }
        });
    }
    {
        let car = carousel.clone();
        let widgets = Rc::clone(&page_widgets);
        let idx = Rc::clone(&cur_idx);
        btn_img_next.connect_clicked(move |_| {
            let i = idx.get();
            if let Some(w) = widgets.borrow().get(i + 1) {
                car.scroll_to(w, true);
            }
        });
    }

    // Middle capsule: cycle the current source's mode.
    mode_btn.connect_clicked({
        let c = Rc::clone(client);
        let t = toast.clone();
        let pages_m = Rc::clone(&pages);
        let cfg = config.clone();
        move |btn| {
            let mut source = cfg.wallpaper_source();
            if source.is_empty() {
                source = pages_m
                    .borrow()
                    .first()
                    .map(|p| p.source.clone())
                    .unwrap_or_default();
            }
            if source.is_empty() {
                return;
            }
            let cur = cfg.source_mode(&source);
            let new_mode = match cur.as_str() {
                "latest" => "random",
                "random" => "manual",
                _ => "latest",
            };
            btn.set_label(&mode_label(new_mode));
            let c2 = Rc::clone(&c);
            let t2 = t.clone();
            let s2 = source.clone();
            glib::spawn_future_local(async move {
                if let Err(e) = c2.set_wallpaper_source(&s2, new_mode).await {
                    t2.add_toast(crate::views::toast(&format!(
                        "{}: {e}",
                        gettext("Failed to set")
                    )));
                }
            });
        }
    });

    // ---- Signal wiring ----
    {
        // Lightweight sync on WallpaperChanged: reposition within the
        // EXISTING pages when possible. A full rebuild here would fight the
        // user's swipes (apply → signal → rebuild → page_changed → apply …).
        let pages_w = Rc::clone(&pages);
        let widgets_w = Rc::clone(&page_widgets);
        let cur_w = Rc::clone(&cur_idx);
        let car_w = carousel.clone();
        let cfg_w = config.clone();
        let t_w = title_label.clone();
        let m_w = meta_label.clone();
        let d_w = date_label.clone();
        let name_w = src_name.clone();
        let mb_w = mode_btn.clone();
        let bsprev_w = btn_src_prev.clone();
        let bsnext_w = btn_src_next.clone();
        let biprev_w = btn_img_prev.clone();
        let binext_w = btn_img_next.clone();
        let imgs_w = Rc::clone(&images);
        let syncing = Rc::clone(&syncing);
        let capsule_w = src_capsule.clone();
        let anim_w = Rc::new(Cell::new(0u64));
        let reload_sig = reload.clone();
        client.set_on_wallpaper_changed(move |file, _source, _title| {
            *applied_file.borrow_mut() = file.to_owned();
            let idx = pages_w.borrow().iter().position(|p| p.file == file);
            match idx {
                Some(i) => {
                    cur_w.set(i);
                    syncing.set(true);
                    if let Some(w) = widgets_w.borrow().get(i) {
                        car_w.scroll_to(w, false);
                    }
                    syncing.set(false);
                    let n_imgs = imgs_w.borrow().len();
                    set_btn_on(&biprev_w, i > 0);
                    set_btn_on(&binext_w, i + 1 < n_imgs);
                    sync_page_ui(
                        &pages_w, &cfg_w, i, &t_w, &m_w, &d_w, &name_w, &capsule_w, &anim_w, &mb_w,
                        &bsprev_w, &bsnext_w,
                    );
                }
                None => {
                    // Different source or brand-new list: full refresh.
                    reload_sig();
                }
            }
        });
    }
    {
        let toast_e = toast.clone();
        client.set_on_fetch_failed(move |source, error| {
            let name = by_id(source)
                .map(|s| s.display_name())
                .unwrap_or_else(|| source.to_owned());
            let _ = name;
            toast_e.add_toast(crate::views::toast(&format!(
                "{}: {error}",
                gettext("Operation failed")
            )));
        });
    }
    {
        let reload_run = reload.clone();
        client.set_on_running_changed(move |running| {
            if running {
                reload_run();
            }
        });
    }
    {
        // New downloads must refresh this page even when no wallpaper source
        // is selected yet (first-run / OOBE) — otherwise the carousel stays
        // on "no images in this source yet" although images exist.
        let reload_img = reload.clone();
        let cfg_img = config.clone();
        client.set_on_image_added(move |added_source, _path| {
            let ws = cfg_img.wallpaper_source();
            if ws.is_empty() || added_source == ws {
                reload_img();
            }
        });
    }

    // Initial load (name watcher may not be ready yet; running-changed retries).
    reload();

    overlay.upcast()
}

/// Refresh the info labels + source capsule + mode button + arrow states for
/// the page at `idx`.
fn sync_page_ui(
    pages: &Rc<RefCell<Vec<Rc<HistPage>>>>,
    config: &Config,
    idx: usize,
    t: &gtk4::Label,
    m: &gtk4::Label,
    d: &gtk4::Label,
    name_lbl: &gtk4::Label,
    capsule: &CapsuleBox,
    anim_gen: &Rc<Cell<u64>>,
    mb: &gtk4::Button,
    bprev: &gtk4::Button,
    bnext: &gtk4::Button,
) {
    let pages = pages.borrow();
    let Some(page) = pages.get(idx) else {
        return;
    };
    set_info(t, m, d, page);
    let display = if page.source.is_empty() {
        gettext("None")
    } else {
        by_id(&page.source)
            .map(|s| s.display_name())
            .unwrap_or_else(|| page.source.clone())
    };
    name_lbl.set_label(&display);
    // Plate width follows the current name (smooth ease-out animation);
    // target = label width + chip padding (28) + plate padding (6).
    let (_, n, _, _) = name_lbl.measure(gtk4::Orientation::Horizontal, -1);
    animate_width(
        capsule,
        anim_gen,
        capsule.width_request(),
        n.max(0) + 42,
        180,
    );
    mb.set_label(&mode_label(&config.source_mode(&page.source)));

    // Source-switch arrows: "None" is slot 0, real sources follow at their
    // registry index + 1.
    let reg = registry();
    let pos = if page.source.is_empty() {
        0
    } else {
        reg.iter()
            .position(|s| s.id() == page.source)
            .map(|i| i + 1)
            .unwrap_or(1)
    };
    set_btn_on(bprev, pos > 0);
    set_btn_on(bnext, pos + 1 < reg.len() + 1);
}

/// Animate a widget's forced width (size_request) from its current value to
/// `to` over `duration_ms` with ease-out, driven by the frame clock. A
/// generation counter supersedes stale animations.
fn animate_width(
    widget: &impl IsA<gtk4::Widget>,
    gen: &Rc<Cell<u64>>,
    from: i32,
    to: i32,
    duration_ms: u64,
) {
    if from == to || to <= 0 {
        return;
    }
    let from = from as f64;
    let to = to as f64;
    let duration = duration_ms as f64;
    let start = Cell::new(None::<f64>);
    let my = gen.get() + 1;
    gen.set(my);
    let gen = Rc::clone(gen);
    let widget = widget.clone();
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
        w.set_size_request((from + (to - from) * eased).round() as i32, -1);
        if p >= 1.0 {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

/// Update the info area (title/description/date) for a history page.
fn set_info(title: &gtk4::Label, meta: &gtk4::Label, date: &gtk4::Label, page: &HistPage) {
    let (ctitle, desc, cdate) = caption(&page.file);
    title.set_text(if page.title.is_empty() {
        &ctitle
    } else {
        &page.title
    });
    meta.set_text(&desc);
    date.set_text(&cdate);
}

/// Hide a button WITHOUT removing it from the layout (opacity + insensitive):
/// set_visible would collapse it and shift sibling positions.
fn set_btn_on(btn: &gtk4::Button, on: bool) {
    btn.set_opacity(if on { 1.0 } else { 0.0 });
    btn.set_sensitive(on);
}

/// Mode display name: "latest" → "Latest", "random" → "Random",
/// "manual" → "Manual".
fn mode_label(mode: &str) -> String {
    match mode {
        "random" => gettext("Random"),
        "manual" => gettext("Manual"),
        _ => gettext("Latest"),
    }
}

/// File name (without extension) as title fallback.
fn file_stem(file: &str) -> String {
    std::path::Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| gettext("Untitled wallpaper"))
}

/// Title/introduction/date from the sidecar metadata; the title falls back to
/// the file name when missing.
fn caption(file: &str) -> (String, String, String) {
    match photo_meta(file) {
        Some(m) => {
            let desc = m
                .extra
                .get("description")
                .or_else(|| m.extra.get("summary"))
                .cloned()
                .unwrap_or_default();
            let copyright = m.copyright.unwrap_or_default();
            let meta = match (desc.is_empty(), copyright.is_empty()) {
                (false, false) => format!("{desc}\n{copyright}"),
                (false, true) => desc,
                (true, false) => copyright,
                (true, true) => String::new(),
            };
            (
                if m.title.is_empty() {
                    file_stem(file)
                } else {
                    m.title
                },
                meta,
                m.date,
            )
        }
        None => (file_stem(file), String::new(), String::new()),
    }
}

/// Read the sidecar metadata JSON next to the image (written at download).
/// Note: the sidecar name **replaces** the extension (`xxx.jpg` → `xxx.json`).
fn photo_meta(file: &str) -> Option<ImageMeta> {
    let json_path = std::path::Path::new(file).with_extension("json");
    std::fs::read_to_string(json_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

/// Page-local CSS (preview rounding, pills, capsules).
fn views_css() {
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(
        // Note: GTK4 CSS has no overflow property; clipping uses widget
        // properties (see set_overflow below).
        ".wallpaper-preview { border-radius: 12px; }
          .wallpaper-bar { background-color: @window_bg_color; border-radius: 999px; padding: 4px; }
          .round-btn { min-width: 40px; min-height: 40px; padding: 0; border-radius: 50%; }
          .mode-capsule { padding: 0 22px; border-radius: 999px; font-weight: 600; }
          .wallpaper-src-name { font-weight: 600; }
          .wallpaper-pill { background-color: @window_bg_color; border-radius: 999px; padding: 8px 14px; }
          .wallpaper-overlay-btn { min-width: 40px; min-height: 40px; padding: 0; border-radius: 50%; background-color: @window_bg_color; }
          .wallpaper-overlay-btn:hover { background-image: linear-gradient(to bottom, alpha(@window_fg_color, 0.12), alpha(@window_fg_color, 0.12)); }
          .wallpaper-overlay-btn:active { background-image: linear-gradient(to bottom, alpha(@window_fg_color, 0.2), alpha(@window_fg_color, 0.2)); }
          .wallpaper-overlay-btn > image { color: @window_fg_color; }
          /* Bottom source switcher: single-layer round/capsule controls —
             the style lives in mod.rs (`.eq-disc-btn` / `.eq-btn-core`). */",
    );
    gtk4::style_context_add_provider_for_display(
        &gtk4::gdk::Display::default().expect("no display"),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
