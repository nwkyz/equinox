//! History page: card grid of applied wallpapers (thumbnail + title + time).
//! Clicking a thumbnail opens the fullscreen viewer (apply lives in the
//! viewer's button), refresh/clear at the top.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use libadwaita as Adw;
use libadwaita::prelude::*;

use gettextrs::gettext;

use equinox_core::history::HistoryEntry;

use crate::daemon_client::DaemonClient;
use crate::views::fullscreen_viewer;

/// History layout: same dynamic sizing as the gallery grid, uniform
/// screen-ratio tiles.
const TILE_MIN: i32 = 220;
const TILE_MAX_COLS: i32 = 6;
use crate::views::ratio_box::ScreenRatioBox;
use crate::views::{display_path, ensure_thumbnails_async, fmt_local, screen_ratio};

/// GObject wrapper of a history entry, for the GridView list model.
mod item {
    use std::cell::RefCell;

    use glib::subclass::prelude::*;
    use glib::value::ToValue;

    #[derive(Default)]
    pub struct HistoryItem {
        pub file: RefCell<String>,
        pub title: RefCell<String>,
        pub copyright: RefCell<String>,
        pub source: RefCell<String>,
        pub applied_at: RefCell<i64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HistoryItem {
        const NAME: &'static str = "EquinoxHistoryItem";
        type Type = super::HistoryItem;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for HistoryItem {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES
                .get_or_init(|| {
                    vec![
                        glib::ParamSpecString::builder("file").build(),
                        glib::ParamSpecString::builder("title").build(),
                        glib::ParamSpecString::builder("copyright").build(),
                        glib::ParamSpecString::builder("source").build(),
                        glib::ParamSpecInt64::builder("applied-at").build(),
                    ]
                })
                .as_ref()
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "file" => *self.file.borrow_mut() = value.get().unwrap_or_default(),
                "title" => *self.title.borrow_mut() = value.get().unwrap_or_default(),
                "copyright" => *self.copyright.borrow_mut() = value.get().unwrap_or_default(),
                "source" => *self.source.borrow_mut() = value.get().unwrap_or_default(),
                "applied-at" => *self.applied_at.borrow_mut() = value.get().unwrap_or_default(),
                _ => unreachable!(),
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "file" => self.file.borrow().to_value(),
                "title" => self.title.borrow().to_value(),
                "copyright" => self.copyright.borrow().to_value(),
                "source" => self.source.borrow().to_value(),
                "applied-at" => self.applied_at.borrow().to_value(),
                _ => unreachable!(),
            }
        }
    }
}

glib::wrapper! {
    pub struct HistoryItem(ObjectSubclass<item::HistoryItem>);
}

impl HistoryItem {
    fn from_entry(e: &HistoryEntry) -> Self {
        glib::Object::builder()
            .property("file", e.file.clone())
            .property("title", e.title.clone())
            .property("copyright", e.copyright.clone().unwrap_or_default())
            .property("source", e.source.clone())
            .property("applied-at", e.applied_at)
            .build()
    }

    fn file(&self) -> String {
        self.property::<String>("file")
    }

    fn title(&self) -> String {
        self.property::<String>("title")
    }
}

pub fn build(client: &Rc<DaemonClient>, toast: &Adw::ToastOverlay) -> gtk4::Widget {
    // Current dynamic tile width (px), updated on window resize.
    let tile_w: Rc<Cell<i32>> = Rc::new(Cell::new(260));
    // Currently open fullscreen viewer (only one at a time per page).
    let viewer: Rc<RefCell<Option<Adw::Window>>> = Rc::new(RefCell::new(None));
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);
    vbox.set_vexpand(true);

    // Top toolbar: page title + count on the left, actions on the right
    let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    header.set_valign(gtk4::Align::Center);
    let title_box = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    let title = gtk4::Label::new(Some(&gettext("History")));
    title.add_css_class("title-2");
    title.set_halign(gtk4::Align::Start);
    let count_label = gtk4::Label::new(None);
    count_label.add_css_class("dim-label");
    count_label.set_halign(gtk4::Align::Start);
    title_box.append(&title);
    title_box.append(&count_label);
    let refresh_btn = gtk4::Button::from_icon_name("view-refresh-symbolic");
    refresh_btn.add_css_class("flat");
    refresh_btn.set_tooltip_text(Some(&gettext("Refresh")));
    let clear_btn = gtk4::Button::from_icon_name("edit-clear-all-symbolic");
    clear_btn.add_css_class("flat");
    clear_btn.add_css_class("destructive-action");
    clear_btn.set_tooltip_text(Some(&gettext("Clear history")));
    let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    header.append(&title_box);
    header.append(&spacer);
    header.append(&refresh_btn);
    header.append(&clear_btn);
    vbox.append(&header);

    // Card grid
    let store = gio::ListStore::new::<HistoryItem>();
    let selection = gtk4::SingleSelection::new(Some(store.clone()));
    // Critical: disable autoselect. Otherwise refreshing history would make
    // autoselect pick the first item and trigger selected_notify → auto-apply
    // → WallpaperChanged → refresh again → … an infinite wallpaper-apply loop.
    // Clicking still selects normally (clicks go through the view gestures,
    // unrelated to autoselect).
    selection.set_autoselect(false);
    let factory = gtk4::SignalListItemFactory::new();
    factory.connect_setup({
        let tile_w = Rc::clone(&tile_w);
        move |_factory, item| {
            let list_item = item.downcast_ref::<gtk4::ListItem>().expect("list item");
            let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            outer.set_margin_top(3);
            outer.set_margin_bottom(3);
            outer.set_margin_start(3);
            outer.set_margin_end(3);
            outer.set_halign(gtk4::Align::Start);
            outer.set_valign(gtk4::Align::Start);
            // Card: NO own clip/radius — the frame below is the single clipping
            // layer (two stacked clips per tile tank the frame rate in software
            // rendering); the caption rounds its own bottom corners.
            let card = gtk4::Overlay::new();
            card.add_css_class("eq-zoom");
            // Uniform tile locked to the device screen's aspect ratio, in FIXED
            // pixels so GridView cannot distort it when the window resizes.
            let ratio = screen_ratio();
            let tw = tile_w.get();
            let tile_h = (tw as f64 * ratio).round() as i32;
            let frame = ScreenRatioBox::with_fixed_size(tw, tile_h);
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
            let time_lbl = gtk4::Label::new(None);
            time_lbl.add_css_class("eq-overlay-sub");
            time_lbl.set_halign(gtk4::Align::Start);
            time_lbl.set_single_line_mode(true);
            caption.append(&title_lbl);
            caption.append(&time_lbl);
            card.add_overlay(&caption);
            // Gray placeholder + ✕ for history entries whose file is gone
            // (deleted). Toggled per row in bind; sits on top of the caption
            // on purpose, so a missing tile reads as "gray + cross".
            let missing = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            missing.add_css_class("eq-missing");
            missing.set_halign(gtk4::Align::Fill);
            missing.set_valign(gtk4::Align::Fill);
            let x_icon = gtk4::Image::from_icon_name("window-close-symbolic");
            x_icon.set_halign(gtk4::Align::Center);
            x_icon.set_valign(gtk4::Align::Center);
            // vexpand so the image is centered in the full-height placeholder
            // (a vertical Box packs children at the top otherwise).
            x_icon.set_vexpand(true);
            x_icon.set_pixel_size(48);
            missing.append(&x_icon);
            missing.set_visible(false);
            card.add_overlay(&missing);
            outer.append(&card);
            list_item.set_child(Some(&outer));
        }
    });
    factory.connect_bind(|_factory, item| {
        let list_item = item.downcast_ref::<gtk4::ListItem>().expect("list item");
        let Some(obj) = list_item
            .item()
            .and_then(|o| o.downcast::<HistoryItem>().ok())
        else {
            return;
        };
        let Some(outer) = list_item
            .child()
            .and_then(|c| c.downcast::<gtk4::Box>().ok())
        else {
            return;
        };
        if let Some(card) = outer
            .first_child()
            .and_then(|c| c.downcast::<gtk4::Overlay>().ok())
        {
            let disp = display_path(&obj.file());
            let exists = std::path::Path::new(&disp).exists();
            if let Some(pic) = card
                .child()
                .and_then(|c| c.downcast::<ScreenRatioBox>().ok())
                .and_then(|f| {
                    f.first_child()
                        .and_then(|c| c.downcast::<gtk4::Picture>().ok())
                })
            {
                if exists {
                    pic.set_filename(Some(&disp));
                } else {
                    // File deleted: show the tile's gray ✕ placeholder.
                    pic.set_filename(None::<&str>);
                }
            }
            // Overlay.first_child() is the main child; caption is its first
            // sibling, the missing-placeholder box the last one.
            if let Some(caption) = card
                .child()
                .and_then(|c| c.next_sibling())
                .and_then(|c| c.downcast::<gtk4::Box>().ok())
            {
                if let Some(t) = caption
                    .first_child()
                    .and_then(|c| c.downcast::<gtk4::Label>().ok())
                {
                    t.set_text(&obj.title());
                }
                if let Some(time) = caption
                    .first_child()
                    .and_then(|c| c.next_sibling())
                    .and_then(|c| c.downcast::<gtk4::Label>().ok())
                {
                    let at = obj.property::<i64>("applied-at");
                    time.set_text(&format!("{} {}", gettext("Applied"), fmt_local(at, true)));
                }
            }
            // Toggle the gray ✕ placeholder for missing files.
            if let Some(missing) = card
                .child()
                .and_then(|c| c.next_sibling())
                .and_then(|c| c.next_sibling())
                .and_then(|c| c.downcast::<gtk4::Box>().ok())
            {
                missing.set_visible(!exists);
            }
            let tooltip = format!(
                "{}\n{} {}",
                obj.title(),
                gettext("Applied"),
                fmt_local(obj.property::<i64>("applied-at"), true)
            );
            card.set_tooltip_text(Some(&tooltip));
        }
    });

    let grid = gtk4::GridView::new(Some(selection.clone()), Some(factory.clone()));
    grid.set_max_columns(6);
    grid.set_min_columns(2);
    grid.set_halign(gtk4::Align::Fill);
    grid.set_hexpand(true);
    grid.set_valign(gtk4::Align::Start);

    // Empty state
    let empty_page = Adw::StatusPage::builder()
        .icon_name("document-open-recent-symbolic")
        .title(gettext("No wallpaper has been applied yet").as_str())
        .description(
            gettext("Wallpapers you apply will be recorded here so you can go back to them")
                .as_str(),
        )
        .build();

    let content = gtk4::Stack::new();
    content.set_hhomogeneous(false);
    content.set_vhomogeneous(false);
    content.add_named(&grid, Some("grid"));
    content.add_named(&empty_page, Some("empty"));
    content.set_visible_child_name("grid");
    let grid_scroll = gtk4::ScrolledWindow::new();
    grid_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    grid_scroll.set_vexpand(true);
    grid_scroll.set_child(Some(&content));
    vbox.append(&grid_scroll);

    // Click on a thumbnail → open the fullscreen viewer (apply lives in the
    // viewer's own button, not on the thumbnail click). Clearing the
    // selection when the viewer closes lets clicking the SAME card again
    // re-open it.
    let c = Rc::clone(client);
    let viewer_sel = Rc::clone(&viewer);
    selection.connect_selected_notify(move |sel| {
        if let Some(obj) = sel
            .selected_item()
            .and_then(|o| o.downcast::<HistoryItem>().ok())
        {
            let path = obj.file();
            let c2 = c.clone();
            let sel_close = sel.clone();
            let cb: Rc<dyn Fn()> = Rc::new(move || {
                sel_close.unselect_all();
            });
            fullscreen_viewer::open_viewer(&viewer_sel, &c2, &path, Some(cb));
        }
    });

    // Load history (fetch and fill the grid)
    let load = {
        let client_load = Rc::clone(client);
        let store_load = store.clone();
        let count_load = count_label.clone();
        let content_load = content.clone();
        move || {
            let client_load = client_load.clone();
            let store_load = store_load.clone();
            let count_load = count_load.clone();
            let content_load = content_load.clone();
            glib::spawn_future_local(async move {
                let entries = match client_load.history().await {
                    Ok(v) => v,
                    Err(e) => {
                        store_load.remove_all();
                        count_load.set_text("");
                        content_load.set_visible_child_name("empty");
                        log::warn!("failed to load history: {e}");
                        return;
                    }
                };
                store_load.remove_all();
                let n = entries.len();
                count_load.set_text(&format!("{n} {}", gettext("entries")));
                if n == 0 {
                    content_load.set_visible_child_name("empty");
                    return;
                }
                content_load.set_visible_child_name("grid");
                // Thumbnails are produced GUI-side (the daemon links no
                // image decoders, staying small at idle); backfill any
                // missing ones in the background.
                ensure_thumbnails_async(
                    entries
                        .iter()
                        .map(|e| std::path::PathBuf::from(&e.file))
                        .collect(),
                );
                for e in entries {
                    store_load.append(&HistoryItem::from_entry(&e));
                }
            });
        }
    };

    refresh_btn.connect_clicked({
        let load = load.clone();
        move |_| load()
    });

    // Dynamic tiles: recompute width from the view size, rebuild on change.
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

    // Clear
    let c = Rc::clone(client);
    let t = toast.clone();
    let load2 = load.clone();
    clear_btn.connect_clicked(move |_| {
        let c2 = c.clone();
        let t2 = t.clone();
        let load3 = load2.clone();
        glib::spawn_future_local(async move {
            if c2.clear_history().await.is_ok() {
                t2.add_toast(crate::views::toast(&gettext("History cleared")));
            } else {
                t2.add_toast(crate::views::toast(&gettext("Failed to clear history")));
            }
            load3();
        });
    });

    // Auto-refresh on wallpaper change
    client.set_on_wallpaper_changed({
        let load4 = load.clone();
        move |_, _, _| load4()
    });

    // Auto-reload when the daemon comes online (covers startup races)
    client.set_on_running_changed({
        let load5 = load.clone();
        move |running| {
            if running {
                load5();
            }
        }
    });

    // Initial load
    load();

    // Wide windows center content; narrow windows shrink gracefully
    let clamp = Adw::Clamp::new();
    clamp.set_maximum_size(1000);
    clamp.set_child(Some(&vbox));
    clamp.upcast()
}
