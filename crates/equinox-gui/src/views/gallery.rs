//! Gallery page: browse downloaded images per source, delete, apply through
//! the dedicated toolbar button. Dual view: a centered fixed-ratio carousel
//! (default, with per-page title/caption overlay + position counter) and a
//! card grid, switched with a segmented control at the top. Clicking a
//! picture (carousel page or grid card) never applies directly: it opens the
//! fullscreen viewer.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;

use libadwaita as Adw;
use libadwaita::prelude::*;

use gettextrs::gettext;

use equinox_core::source::registry;

use crate::daemon_client::{DaemonClient, GalleryEntry};
use crate::views::{apply_carousel_tex_window, display_path, ensure_thumbnails_async};
use crate::views::fullscreen_viewer;
use crate::views::ratio_box::ScreenRatioBox;

/// GObject wrapper of a gallery entry.
mod item {
    use std::cell::RefCell;

    use glib::subclass::prelude::*;
    use glib::value::ToValue;

    #[derive(Default)]
    pub struct GalleryItem {
        pub path: RefCell<String>,
        pub title: RefCell<String>,
        pub source: RefCell<String>,
        pub downloaded_at: RefCell<i64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for GalleryItem {
        const NAME: &'static str = "EquinoxGalleryItem";
        type Type = super::GalleryItem;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for GalleryItem {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES
                .get_or_init(|| {
                    vec![
                        glib::ParamSpecString::builder("path").build(),
                        glib::ParamSpecString::builder("title").build(),
                        glib::ParamSpecString::builder("source").build(),
                        glib::ParamSpecInt64::builder("downloaded-at").build(),
                    ]
                })
                .as_ref()
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "path" => *self.path.borrow_mut() = value.get().unwrap_or_default(),
                "title" => *self.title.borrow_mut() = value.get().unwrap_or_default(),
                "source" => *self.source.borrow_mut() = value.get().unwrap_or_default(),
                "downloaded-at" => {
                    *self.downloaded_at.borrow_mut() = value.get().unwrap_or_default()
                }
                _ => unreachable!(),
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "path" => self.path.borrow().to_value(),
                "title" => self.title.borrow().to_value(),
                "source" => self.source.borrow().to_value(),
                "downloaded-at" => self.downloaded_at.borrow().to_value(),
                _ => unreachable!(),
            }
        }
    }
}

glib::wrapper! {
    pub struct GalleryItem(ObjectSubclass<item::GalleryItem>);
}

impl GalleryItem {
    fn from_tuple((path, title, source, downloaded_at): GalleryEntry) -> Self {
        glib::Object::builder()
            .property("path", path)
            .property("title", title)
            .property("source", source)
            .property("downloaded-at", downloaded_at)
            .build()
    }

    fn path(&self) -> String {
        self.property::<String>("path")
    }

    fn title(&self) -> String {
        self.property::<String>("title")
    }
}

/// Carousel pages run newest→oldest left-to-right and start at the newest,
/// so a page index maps 1:1 onto the (newest-first) entries list.
fn carousel_to_entry(entries: &[GalleryEntry], page: usize) -> Option<usize> {
    (page < entries.len()).then_some(page)
}

/// Remove every carousel page, unparenting each page's picture frame first
/// (GTK warns "finalized but still has children" otherwise). Pages are a Box
/// (picture + caption); the picture frame is the Box's first child.
fn clear_carousel(carousel: &Adw::Carousel) {
    while carousel.n_pages() > 0 {
        let page = carousel.nth_page(0);
        if let Some(b) = page.downcast_ref::<gtk4::Box>() {
            if let Some(f) = b
                .first_child()
                .and_then(|c| c.downcast::<ScreenRatioBox>().ok())
            {
                f.take_child();
            }
        } else if let Some(f) = page.downcast_ref::<ScreenRatioBox>() {
            f.take_child();
        }
        carousel.remove(&page);
    }
}

/// Enable/disable the floating carousel navigation buttons at the boundaries.
fn set_nav_state(n: usize, idx: usize, prev: &gtk4::Button, next: &gtk4::Button) {
    prev.set_sensitive(idx > 0);
    next.set_sensitive(idx + 1 < n);
}

/// Update the description area (title + meta line): carousel current page /
/// grid selection.
/// Grid layout: target column width drives a dynamic tile size.
const TILE_MIN: i32 = 220;
const TILE_MAX_COLS: i32 = 6;

pub fn build(client: &Rc<DaemonClient>, toast: &Adw::ToastOverlay) -> gtk4::Widget {
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);
    vbox.set_vexpand(true);
    // Always fill the clamp width: otherwise the vbox hugs the carousel page
    // (≈600 px) and the whole top toolbar row narrows, visibly shifting
    // between the carousel and grid modes.
    vbox.set_hexpand(true);

    // ---- Top toolbar: source picker + view toggle + actions ----
    let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    header.set_valign(gtk4::Align::Center);
    let names: Vec<String> = registry().iter().map(|s| s.display_name()).collect();
    let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let source_model = gtk4::StringList::new(&name_refs);
    // The source picker in the toolbar uses GtkDropDown: Adw::ComboRow is for
    // PreferencesGroup (lists); in a plain horizontal Box its popup
    // interaction doesn't work.
    let source_row = gtk4::DropDown::new(Some(source_model.clone()), None::<&gtk4::Expression>);
    source_row.set_valign(gtk4::Align::Center);
    source_row.set_selected(0);

    // Carousel / grid view toggle
    let carousel_toggle = Adw::Toggle::new();
    carousel_toggle.set_label(Some(&gettext("Carousel")));
    let grid_toggle = Adw::Toggle::new();
    grid_toggle.set_label(Some(&gettext("Grid")));
    let mode_group = Adw::ToggleGroup::new();
    mode_group.add(carousel_toggle);
    mode_group.add(grid_toggle);
    mode_group.set_active(0);
    // Never let the segmented control stretch with the toolbar row.
    mode_group.set_valign(gtk4::Align::Center);
    mode_group.add_css_class("eq-seg");

    let refresh_btn = gtk4::Button::from_icon_name("view-refresh-symbolic");
    refresh_btn.set_valign(gtk4::Align::Center);
    refresh_btn.set_tooltip_text(Some(&gettext("Refresh")));
    let delete_btn = gtk4::Button::from_icon_name("user-trash-symbolic");
    delete_btn.add_css_class("destructive-action");
    delete_btn.set_valign(gtk4::Align::Center);
    delete_btn.set_tooltip_text(Some(&gettext("Delete selected")));
    // Apply sits with the other actions on the right, left of the refresh
    // button (clicking a picture never applies directly any more).
    let apply_btn = gtk4::Button::with_label(&gettext("Apply"));
    apply_btn.add_css_class("suggested-action");
    apply_btn.set_valign(gtk4::Align::Center);
    apply_btn.set_tooltip_text(Some(&gettext("Apply the selected image as wallpaper")));
    // 源切换 · 视图切换 ········· 应用 · 刷新 · 删除(右侧)
    header.append(&source_row);
    header.append(&mode_group);
    let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    header.append(&spacer);
    header.append(&apply_btn);
    header.append(&refresh_btn);
    header.append(&delete_btn);
    vbox.append(&header);

    // Current source + entry cache + image-date cache (parallel to entries) +
    // current carousel page index + per-image aspect-ratio cache (h/w)
    let current_source: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let entries: Rc<RefCell<Vec<GalleryEntry>>> = Rc::new(RefCell::new(Vec::new()));
    let dates: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let current_idx: Rc<Cell<usize>> = Rc::new(Cell::new(0));
    let ratios: Rc<RefCell<std::collections::HashMap<String, f64>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    // Current dynamic tile width (px), updated on window resize.
    let tile_w: Rc<Cell<i32>> = Rc::new(Cell::new(260));
    // Per-entry description preview (sidecar description/summary/copyright).
    let descs: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    // Currently open fullscreen viewer (only one at a time per page).
    let viewer: Rc<RefCell<Option<Adw::Window>>> = Rc::new(RefCell::new(None));

    // ---- Grid view (cards) ----
    let store = gio::ListStore::new::<GalleryItem>();
    let selection = gtk4::SingleSelection::new(Some(store.clone()));
    // Disable autoselect: otherwise refreshing would make autoselect pick the
    // first item and trigger selected_notify (which applies the wallpaper in
    // grid mode, and combined with the history page's refresh loop could spin
    // into endless wallpaper applying).
    selection.set_autoselect(false);
    let factory = gtk4::SignalListItemFactory::new();
    factory.connect_setup({
        let tile_w = Rc::clone(&tile_w);
        move |_factory, item| {
            let list_item = item.downcast_ref::<gtk4::ListItem>().expect("list item");
            let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            outer.set_margin_bottom(3);
            outer.set_margin_start(3);
            outer.set_margin_end(3);
            outer.set_margin_top(3);
            // Never stretch inside the GridView cell (rows size to the tallest
            // tile; variable-ratio tiles must keep their own shape).
            outer.set_halign(gtk4::Align::Start);
            outer.set_valign(gtk4::Align::Start);
            // Card: rounded clipping frame at the IMAGE's own aspect ratio;
            // title/description sit on a bottom gradient overlay. Hover zooms
            // the whole card slightly (springy CSS transition).
            // Card: NO own clip/radius — the frame below is the single clipping
            // layer (two stacked clips per tile tank the frame rate in software
            // rendering); the caption rounds its own bottom corners.
            let card = gtk4::Overlay::new();
            card.add_css_class("eq-zoom");
            // Rounded clipping lives ON the frame (the direct parent of the
            // picture): radius on a background-less widget alone is invisible,
            // and overflow on a distant ancestor does not round the corners.
            let frame = crate::views::ratio_box::ScreenRatioBox::with_fixed_size(
                tile_w.get(),
                tile_w.get() * 9 / 16,
            );
            frame.add_css_class("eq-frame");
            frame.set_overflow(gtk4::Overflow::Hidden);
            let picture = gtk4::Picture::new();
            picture.set_content_fit(gtk4::ContentFit::Cover);
            picture.set_can_shrink(true);
            picture.set_hexpand(true);
            picture.set_vexpand(true);
            frame.set_child(Some(&picture));
            card.set_child(Some(&frame));
            let caption = gtk4::Box::new(gtk4::Orientation::Vertical, 1);
            caption.add_css_class("eq-gradient");
            caption.add_css_class("eq-caption");
            caption.set_halign(gtk4::Align::Fill);
            caption.set_valign(gtk4::Align::End);
            let title_lbl = gtk4::Label::new(None);
            title_lbl.add_css_class("eq-overlay-title");
            title_lbl.set_halign(gtk4::Align::Start);
            title_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            title_lbl.set_max_width_chars(28);
            title_lbl.set_single_line_mode(true);
            let desc_lbl = gtk4::Label::new(None);
            desc_lbl.add_css_class("eq-overlay-sub");
            desc_lbl.set_halign(gtk4::Align::Start);
            desc_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            desc_lbl.set_single_line_mode(true);
            caption.append(&title_lbl);
            caption.append(&desc_lbl);
            card.add_overlay(&caption);
            outer.append(&card);
            list_item.set_child(Some(&outer));
        }
    });
    factory.connect_bind({
        let ratios = Rc::clone(&ratios);
        let descs = Rc::clone(&descs);
        let tile_w_cell = Rc::clone(&tile_w);
        move |_factory, item| {
            let list_item = item.downcast_ref::<gtk4::ListItem>().expect("list item");
            let Some(obj) = list_item
                .item()
                .and_then(|o| o.downcast::<GalleryItem>().ok())
            else {
                return;
            };
            let pos = list_item.position() as usize;
            let Some(outer) = list_item
                .child()
                .and_then(|c| c.downcast::<gtk4::Box>().ok())
            else {
                return;
            };
            let Some(card) = outer
                .first_child()
                .and_then(|c| c.downcast::<gtk4::Overlay>().ok())
            else {
                return;
            };
            let Some(frame) = card
                .child()
                .and_then(|c| c.downcast::<crate::views::ratio_box::ScreenRatioBox>().ok())
            else {
                return;
            };
            let disp = display_path(&obj.path());

            // Ratio comes from the sidecar JSON (written at download time) —
            // zero decoding on this thread. Pixels load via filename (GTK-async).
            let pic = frame
                .first_child()
                .and_then(|c| c.downcast::<gtk4::Picture>().ok());
            let pin = |r: f64| {
                let w = tile_w_cell.get();
                let h = (w as f64 * r).round() as i32;
                frame.set_fixed_size(w, h);
            };
            match ratios.borrow().get(&disp).copied() {
                Some(r) => pin(r),
                None => pin(9.0 / 16.0),
            }
            if let Some(pic) = &pic {
                pic.set_filename(Some(&disp));
            }

            // Gradient caption: line 1 title, line 2 description preview.
            // NOTE: Overlay.first_child() is the MAIN child (the frame); the
            // caption overlay sits at next_sibling.
            let caption = card
                .child()
                .and_then(|c| c.next_sibling())
                .and_then(|c| c.downcast::<gtk4::Box>().ok());
            if let Some(caption) = caption {
                if let Some(t) = caption
                    .first_child()
                    .and_then(|c| c.downcast::<gtk4::Label>().ok())
                {
                    t.set_text(&obj.title());
                }
                if let Some(d) = caption
                    .first_child()
                    .and_then(|c| c.next_sibling())
                    .and_then(|c| c.downcast::<gtk4::Label>().ok())
                {
                    let meta = descs.borrow().get(pos).cloned().unwrap_or_default();
                    d.set_text(&meta);
                    d.set_visible(!meta.is_empty());
                }
                card.set_tooltip_text(Some(&obj.title()));
            }
        }
    });

    let grid = gtk4::GridView::new(Some(selection.clone()), Some(factory.clone()));
    grid.set_max_columns(6);
    grid.set_min_columns(2);
    grid.set_halign(gtk4::Align::Fill);
    grid.set_hexpand(true);
    grid.set_valign(gtk4::Align::Start);

    // ---- Carousel view (default): centered fixed-size frame ----
    let carousel = Adw::Carousel::new();
    carousel.set_allow_mouse_drag(true);
    carousel.set_hexpand(true);
    // Carousel pages in carousel order (parallel to `entries`). Used for
    // lazy texture management: far pages keep their widget but no texture.
    let car_widgets_load: std::rc::Rc<std::cell::RefCell<Vec<ScreenRatioBox>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    // Which carousel pages currently carry a decoded thumbnail texture.
    let tex_loaded: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<usize>>> =
        std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashSet::new()));
    // Clones handed to the load closure / page-changed handler (the load
    // closure is `move`, so the originals stay alive for the handler).
    let car_widgets_cl = car_widgets_load.clone();
    let tex_loaded_cl = tex_loaded.clone();

    // Bottom page navigation (floating at the window bottom, not over the
    // picture): [previous] [n / total] [next]. Single-layer elements: circles
    // and the count pill each carry a thick window-bg border (mod.rs
    // `.eq-disc-btn`/`.eq-btn-core`), so they always match in height.
    let nav_prev = gtk4::Button::from_icon_name("go-previous-symbolic");
    nav_prev.add_css_class("eq-disc-btn");
    nav_prev.set_valign(gtk4::Align::Center);
    nav_prev.set_tooltip_text(Some(&gettext("Previous image")));

    let nav_next = gtk4::Button::from_icon_name("go-next-symbolic");
    nav_next.add_css_class("eq-disc-btn");
    nav_next.set_valign(gtk4::Align::Center);
    nav_next.set_tooltip_text(Some(&gettext("Next image")));

    let counter = gtk4::Label::new(Some("1 / 0"));
    counter.add_css_class("eq-btn-core");
    counter.set_valign(gtk4::Align::Center);

    let nav = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    nav.set_halign(gtk4::Align::Center);
    nav.set_valign(gtk4::Align::Center);
    nav.append(&nav_prev);
    nav.append(&counter);
    nav.append(&nav_next);

    // Caption BELOW the carousel, following the current page (title + intro
    // in the blank space under the picture, wallpaper-page style: big title,
    // dim meta). Kept outside the carousel so each page is just the picture
    // and the caption never covers it. Some breathing room above the title.
    let car_title = gtk4::Label::new(None);
    car_title.add_css_class("title-1");
    car_title.set_wrap(true);
    car_title.set_xalign(0.0);
    let car_desc = gtk4::Label::new(None);
    car_desc.add_css_class("dim-label");
    car_desc.set_wrap(true);
    car_desc.set_xalign(0.0);
    car_desc.set_visible(false);
    let caption_area = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    caption_area.set_margin_top(18);
    caption_area.append(&car_title);
    caption_area.append(&car_desc);

    let carousel_column = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    carousel_column.append(&carousel);
    carousel_column.append(&caption_area);

    // Bounded natural height: pages are fixed-size (see the load loop), so
    // the whole carousel keeps a compact height and no outer scrolling is
    // needed here. The clamp width equals the page width, so the carousel
    // never stretches pages beyond their fixed measure (which would distort
    // the Cover-cropped picture).
    let carousel_clamp = Adw::Clamp::new();
    carousel_clamp.set_maximum_size(600);
    carousel_clamp.set_child(Some(&carousel_column));

    // Fill the below-carousel caption for the page at `idx` (title falls
    // back to the sidecar/file name; intro follows the grid's descs rule).
    let set_caption: Rc<dyn Fn(usize)> = {
        let entries_cap = Rc::clone(&entries);
        let descs_cap = Rc::clone(&descs);
        let t_cap = car_title.clone();
        let d_cap = car_desc.clone();
        Rc::new(move |idx: usize| {
            let e = entries_cap.borrow();
            let Some((path, title, _, _)) = e.get(idx) else {
                return;
            };
            let d = descs_cap.borrow();
            let title_txt = if title.is_empty() {
                fullscreen_viewer::caption_for(path).0
            } else {
                title.clone()
            };
            let meta = d.get(idx).cloned().unwrap_or_default();
            t_cap.set_text(&title_txt);
            d_cap.set_text(&meta);
            d_cap.set_visible(!meta.is_empty());
        })
    };

    // Content area: carousel page / grid page / empty-state page (wrapped in a
    // scroll area so short windows or many images still scroll).
    let empty_page = Adw::StatusPage::builder()
        .icon_name("image-x-generic-symbolic")
        .title(gettext("No images yet").as_str())
        .description(
            gettext("Go to the Sources page and click \"Update now\" to download some wallpapers")
                .as_str(),
        )
        .build();
    let content = gtk4::Stack::new();
    content.set_hhomogeneous(false);
    content.set_vhomogeneous(false);
    content.add_named(&carousel_clamp, Some("carousel"));
    // The grid scrolls internally (its natural height is unbounded).
    let grid_scroll = gtk4::ScrolledWindow::new();
    grid_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    grid_scroll.set_vexpand(true);
    grid_scroll.set_child(Some(&grid));
    content.add_named(&grid_scroll, Some("grid"));
    content.add_named(&empty_page, Some("empty"));
    content.set_visible_child_name("carousel");
    vbox.append(&content);

    // Load: fills both the grid ListStore and the carousel children
    let load = {
        let client_load = Rc::clone(client);
        let store_load = store.clone();
        let carousel_load = carousel.clone();
        let entries_load = Rc::clone(&entries);
        let dates_load = Rc::clone(&dates);
        let ratios_load = Rc::clone(&ratios);
        let descs_load = Rc::clone(&descs);
        let idx_load = Rc::clone(&current_idx);
        let content_load = content.clone();
        let mode_load = mode_group.clone();
        let counter_load = counter.clone();
        let src = Rc::clone(&current_source);
        let viewer_load = Rc::clone(&viewer);
        let prev_load = nav_prev.clone();
        let next_load = nav_next.clone();
        let caption_load = Rc::clone(&set_caption);
        move || {
            let client_load = client_load.clone();
            let store_load = store_load.clone();
            let carousel_load = carousel_load.clone();
            let entries_load = entries_load.clone();
            let dates_load = dates_load.clone();
            let ratios_load = ratios_load.clone();
            let descs_load = descs_load.clone();
            let idx_load = idx_load.clone();
            let content_load = content_load.clone();
            let mode_load = mode_load.clone();
            let counter_load = counter_load.clone();
            let src = src.clone();
            let viewer_load = viewer_load.clone();
            let prev_load = prev_load.clone();
            let next_load = next_load.clone();
            let caption_load = caption_load.clone();
            let car_widgets_b = car_widgets_cl.clone();
            let tex_loaded_in = tex_loaded_cl.clone();
            glib::spawn_future_local(async move {
                // Clone the id out before awaiting: holding the RefCell
                // borrow across .await would panic if the user switches
                // sources while this request is in flight.
                let src_id = src.borrow().clone();
                let images = match client_load.source_images(&src_id).await {
                    Ok(v) => v,
                    Err(_e) => {
                        store_load.remove_all();
                        clear_carousel(&carousel_load);
                        *entries_load.borrow_mut() = Vec::new();
                        *dates_load.borrow_mut() = Vec::new();
                        return;
                    }
                };
                // Thumbnails are produced GUI-side (the daemon links no
                // image decoders, staying small at idle); backfill any
                // missing ones in the background.
                ensure_thumbnails_async(
                    images
                        .iter()
                        .map(|(path, ..)| std::path::PathBuf::from(path))
                        .collect(),
                );
                // Each image's original date (read from the description file;
                // old images may not have one)
                *dates_load.borrow_mut() = images
                    .iter()
                    .map(|(path, ..)| {
                        equinox_core::storage::Storage::meta_for(std::path::Path::new(path))
                            .map(|m| m.date)
                            .unwrap_or_default()
                    })
                    .collect();
                *descs_load.borrow_mut() = images
                    .iter()
                    .map(|(path, ..)| {
                        equinox_core::storage::Storage::meta_for(std::path::Path::new(path))
                            .map(|m| {
                                m.extra
                                    .get("description")
                                    .or_else(|| m.extra.get("summary"))
                                    .cloned()
                                    .or(m.copyright)
                                    .unwrap_or_default()
                            })
                            .unwrap_or_default()
                    })
                    .collect();
                // Ratios from the sidecar (width/height written at download
                // time); legacy files without dims keep a 16:9 fallback.
                {
                    let mut rmap = ratios_load.borrow_mut();
                    for (path, ..) in &images {
                        if let Some(m) =
                            equinox_core::storage::Storage::meta_for(std::path::Path::new(path))
                        {
                            if m.width > 0 && m.height > 0 {
                                let key = display_path(path);
                                rmap.insert(key, m.height as f64 / m.width as f64);
                            }
                        }
                    }
                }
                store_load.remove_all();
                for entry in &images {
                    store_load.append(&GalleryItem::from_tuple(entry.clone()));
                }
                // Rebuild the carousel (full rebuild; textures lazy).
                // Pages run newest-first (page 0 = latest image).
                clear_carousel(&carousel_load);
                tex_loaded_in.borrow_mut().clear();
                let mut car_pages: Vec<ScreenRatioBox> = Vec::new();
                for (path, _title, _src, _ts) in images.iter() {
                    // One carousel page is JUST the picture, fixed-size so the
                    // whole carousel stays compact; the photo fills the tile
                    // (Cover) so neighbouring pages run seamlessly edge to
                    // edge with no gaps. The caption lives BELOW the carousel
                    // and follows the current page (wallpaper-page style).
                    let frame = ScreenRatioBox::with_fixed_size(600, 300);
                    frame.add_css_class("eq-frame");
                    frame.set_overflow(gtk4::Overflow::Hidden);
                    let pic = gtk4::Picture::new();
                    pic.set_content_fit(gtk4::ContentFit::Cover);
                    pic.set_can_shrink(true);
                    pic.set_hexpand(true);
                    pic.set_vexpand(true);
                    // No filename here: apply_carousel_tex_window below sets
                    // the lazy texture window (active page ± 2).
                    frame.set_child(Some(&pic));

                    // Click (not drag!) → open the fullscreen viewer: only a
                    // short tap without movement opens it; swiping between
                    // pages must not.
                    let c = Rc::clone(&client_load);
                    let p = path.clone();
                    let press_pos: Rc<(Cell<f64>, Cell<f64>, Cell<u32>)> =
                        Rc::new((Cell::new(0.0), Cell::new(0.0), Cell::new(0)));
                    let gesture = gtk4::GestureClick::new();
                    {
                        let pos = Rc::clone(&press_pos);
                        gesture.connect_pressed(move |g, _n, x, y| {
                            pos.0.set(x);
                            pos.1.set(y);
                            pos.2.set(g.current_event_time());
                        });
                    }
                    {
                        let pos = Rc::clone(&press_pos);
                        let c = c.clone();
                        let p = p.clone();
                        let viewer_tap = viewer_load.clone();
                        gesture.connect_released(move |g, _n, x, y| {
                            let dx = (x - pos.0.get()).abs();
                            let dy = (y - pos.1.get()).abs();
                            let dt = g.current_event_time().saturating_sub(pos.2.get());
                            // Tap (< 6 px movement, < 600 ms), not a swipe
                            if dx < 6.0 && dy < 6.0 && dt < 600 {
                                fullscreen_viewer::open_viewer(&viewer_tap, &c, &p, None);
                            }
                        });
                    }
                    frame.add_controller(gesture);
                    carousel_load.append(&frame);
                    car_pages.push(frame);
                }
                *car_widgets_b.borrow_mut() = car_pages;
                *entries_load.borrow_mut() = images;
                // Lazy texture window for the initial page (0) ± 2.
                {
                    let w = car_widgets_b.borrow();
                    let e = entries_load.borrow();
                    let paths: Vec<String> = e.iter().map(|(p, ..)| p.clone()).collect();
                    apply_carousel_tex_window(&w, &paths, 0, 2, &mut tex_loaded_in.borrow_mut());
                }
                idx_load.set(0);
                let n = entries_load.borrow().len();
                counter_load.set_text(&format!("1 / {n}"));
                set_nav_state(n, 0, &prev_load, &next_load);
                caption_load(0);
                if entries_load.borrow().is_empty() {
                    // Empty source: show a hint page instead of a blank area
                    // (so it isn't mistaken for an unavailable picker).
                    content_load.set_visible_child_name("empty");
                    return;
                }
                content_load.set_visible_child_name(if mode_load.active() == 0 {
                    "carousel"
                } else {
                    "grid"
                });
                // Start at the far left = oldest; description matches its entry
            });
        }
    };

    // Carousel page change → update counter, below-carousel caption and
    // delete target; the floating navigation buttons follow the page
    // boundaries.
    let idx_sig = Rc::clone(&current_idx);
    let entries_sig = Rc::clone(&entries);
    let counter_sig = counter.clone();
    let prev_sig = nav_prev.clone();
    let next_sig = nav_next.clone();
    let caption_sig = Rc::clone(&set_caption);
    let car_widgets_sig = car_widgets_load.clone();
    let tex_loaded_sig = tex_loaded.clone();
    carousel.connect_page_changed(move |_c, page| {
        // Lazy textures: only active ± 2 pages hold thumbnails; far pages
        // keep their widget but no texture (re-decoded when approaching).
        {
            let w = car_widgets_sig.borrow();
            let e = entries_sig.borrow();
            let paths: Vec<String> = e.iter().map(|(p, ..)| p.clone()).collect();
            apply_carousel_tex_window(
                &w,
                &paths,
                page as usize,
                2,
                &mut tex_loaded_sig.borrow_mut(),
            );
        }
        idx_sig.set(page as usize);
        let n = entries_sig.borrow().len();
        counter_sig.set_text(&format!("{} / {}", page as usize + 1, n));
        set_nav_state(n, page as usize, &prev_sig, &next_sig);
        caption_sig(page as usize);
    });

    // Floating navigation: previous / next page (newest first, so "next"
    // walks towards older images).
    {
        let car = carousel.clone();
        let idx = Rc::clone(&current_idx);
        nav_prev.connect_clicked(move |_| {
            let i = idx.get();
            if i > 0 {
                car.scroll_to(&car.nth_page(i as u32 - 1), true);
            }
        });
    }
    {
        let car = carousel.clone();
        let idx = Rc::clone(&current_idx);
        let entries_btn = Rc::clone(&entries);
        nav_next.connect_clicked(move |_| {
            let i = idx.get();
            if i + 1 < entries_btn.borrow().len() {
                car.scroll_to(&car.nth_page(i as u32 + 1), true);
            }
        });
    }

    // View toggle
    let content_sw = content.clone();
    let entries_sw = Rc::clone(&entries);
    mode_group.connect_active_notify(move |group| {
        // Keep the hint page for an empty source, don't switch to a blank view
        if entries_sw.borrow().is_empty() {
            content_sw.set_visible_child_name("empty");
            return;
        }
        if group.active() == 0 {
            content_sw.set_visible_child_name("carousel");
        } else {
            content_sw.set_visible_child_name("grid");
        }
    });

    // Source switch → reload
    let load_on_change = load.clone();
    let src = Rc::clone(&current_source);
    source_row.connect_selected_notify(move |row| {
        let idx = row.selected() as usize;
        if let Some(s) = registry().get(idx) {
            *src.borrow_mut() = s.id().to_owned();
            load_on_change();
        }
    });

    // Grid: click → open the fullscreen viewer (only while the grid view is
    // visible, so a carousel-mode reload can't silently pop a viewer via
    // autoselect picking the first item). Clearing the selection when the
    // viewer closes lets clicking the SAME card again re-open it.
    let c_click = Rc::clone(client);
    let content_sel = content.clone();
    let viewer_sel = Rc::clone(&viewer);
    selection.connect_selected_notify(move |sel| {
        if let Some(obj) = sel
            .selected_item()
            .and_then(|o| o.downcast::<GalleryItem>().ok())
        {
            if content_sel.visible_child_name().as_deref() != Some("grid") {
                return;
            }
            let path = obj.path();
            let c2 = c_click.clone();
            let sel_close = sel.clone();
            let cb: Rc<dyn Fn()> = Rc::new(move || {
                sel_close.unselect_all();
            });
            fullscreen_viewer::open_viewer(&viewer_sel, &c2, &path, Some(cb));
        }
    });

    // Delete selected: carousel mode deletes the current page, grid mode the
    // selected item
    let sel_del = selection.clone();
    let content_del = content.clone();
    let entries_del = Rc::clone(&entries);
    let idx_del = Rc::clone(&current_idx);
    let load_after_del = load.clone();
    let toast_del = toast.clone();
    delete_btn.connect_clicked(move |_| {
        let path = if content_del.visible_child_name().as_deref() == Some("carousel") {
            // Carousel page → entries index (newest on the right)
            let e = entries_del.borrow();
            carousel_to_entry(&e, idx_del.get())
                .and_then(|i| e.get(i))
                .map(|e| e.0.clone())
        } else {
            sel_del
                .selected_item()
                .and_then(|o| o.downcast::<GalleryItem>().ok())
                .map(|o| o.path())
        };
        let Some(path) = path else { return };
        let load2 = load_after_del.clone();
        let t2 = toast_del.clone();
        glib::spawn_future_local(async move {
            if equinox_core::storage::Storage::delete(std::path::Path::new(&path)).is_ok() {
                t2.add_toast(crate::views::toast(&gettext("Image deleted")));
                load2();
            } else {
                t2.add_toast(crate::views::toast(&gettext("Failed to delete")));
            }
        });
    });

    // Apply button: applies the current carousel page / grid selection.
    // ApplyFile also switches the rule to that source in manual mode (same
    // semantics as the wallpaper page / the previous click-to-apply).
    let sel_apply = selection.clone();
    let content_apply = content.clone();
    let entries_apply = Rc::clone(&entries);
    let idx_apply = Rc::clone(&current_idx);
    let client_apply = Rc::clone(client);
    let toast_apply = toast.clone();
    apply_btn.connect_clicked(move |_| {
        let path = if content_apply.visible_child_name().as_deref() == Some("carousel") {
            let e = entries_apply.borrow();
            carousel_to_entry(&e, idx_apply.get())
                .and_then(|i| e.get(i))
                .map(|e| e.0.clone())
        } else {
            sel_apply
                .selected_item()
                .and_then(|o| o.downcast::<GalleryItem>().ok())
                .map(|o| o.path())
        };
        let Some(path) = path else { return };
        let c2 = Rc::clone(&client_apply);
        let t2 = toast_apply.clone();
        glib::spawn_future_local(async move {
            match c2.apply_file(&path).await {
                Ok(()) => t2.add_toast(crate::views::toast(&gettext("Wallpaper applied"))),
                Err(e) => t2.add_toast(crate::views::toast(&format!(
                    "{}: {e}",
                    gettext("Failed to apply wallpaper")
                ))),
            }
        });
    });

    // Refresh button
    refresh_btn.connect_clicked({
        let load = load.clone();
        move |_| load()
    });

    // Dynamic tiles: derive column count + tile width from the view width;
    // rebuild (decode cache makes this cheap) when the width bucket changes.
    {
        let tile = Rc::clone(&tile_w);
        let reload_r = load.clone();
        let grid_w = grid.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            let g = &grid_w;
            let w = g.width().max(1);
            let cols = ((w - 24) / TILE_MIN).clamp(2, TILE_MAX_COLS);
            let tw = ((w - 24 - cols * 6) / cols.max(1)).max(TILE_MIN);
            if (tw - tile.get()).abs() >= 8 {
                tile.set(tw);
                reload_r();
            }
            glib::ControlFlow::Continue
        });
    }

    // New image arrived → refresh
    let load_sig = load.clone();
    let src_sig = Rc::clone(&current_source);
    client.set_on_image_added(move |added_source, _path| {
        if added_source == src_sig.borrow().as_str() {
            load_sig();
        }
    });

    // Auto-reload when the daemon comes online (covers startup races)
    let load_on_online = load.clone();
    let src_online = Rc::clone(&current_source);
    client.set_on_running_changed(move |running| {
        if running && !src_online.borrow().is_empty() {
            load_on_online();
        }
    });

    // Initial load
    *current_source.borrow_mut() = registry()[0].id().to_owned();
    load();

    // Wide windows center content; narrow windows shrink gracefully
    let clamp = Adw::Clamp::new();
    clamp.set_maximum_size(1000);
    clamp.set_child(Some(&vbox));

    // The carousel nav bar floats at the window's bottom edge (never over
    // the picture); it is only meaningful in carousel mode.
    let nav_holder = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    nav_holder.set_halign(gtk4::Align::Center);
    nav_holder.set_valign(gtk4::Align::End);
    nav_holder.set_margin_bottom(12);
    nav_holder.append(&nav);
    let page = gtk4::Overlay::new();
    page.set_child(Some(&clamp));
    page.add_overlay(&nav_holder);

    // Keep the floating bar out of the grid view.
    let nav_vis = nav_holder.clone();
    mode_group.connect_active_notify(move |group| {
        nav_vis.set_visible(group.active() == 0);
    });
    nav_holder.set_visible(mode_group.active() == 0);

    page.upcast()
}
